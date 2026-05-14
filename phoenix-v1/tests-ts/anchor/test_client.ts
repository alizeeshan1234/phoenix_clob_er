// TestClient — wraps Anchor provider, base + ER connections, and exposes
// every Phoenix-ER instruction as an async method that returns the
// confirmed transaction signature. Mirrors the magic-trade test pattern.
//
// Phoenix is a native (non-Anchor) program, so we use anchor.AnchorProvider
// only for environment-driven wallet + RPC loading. Instructions are
// hand-rolled `TransactionInstruction`s; we sign and send via the base
// or ER connection depending on whether the target accounts are delegated.
//
// Env vars (Anchor convention):
//   ANCHOR_PROVIDER_URL   — base layer RPC (default: devnet)
//   ANCHOR_WALLET         — keypair file path (default: ~/.config/solana/id.json)
//   ROUTER_ENDPOINT       — Magic Router RPC (default: devnet router)
//   ROUTER_WS_ENDPOINT    — Magic Router WS  (default: devnet router)
//   PHOENIX_PROGRAM_ID    — deployed Phoenix-ER program (required)

import * as anchor from "@coral-xyz/anchor";
import {
  AccountMeta,
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import {
  createAssociatedTokenAccountIdempotentInstruction,
  createInitializeMint2Instruction,
  createMintToInstruction,
  getAccount,
  getAssociatedTokenAddressSync,
  getMinimumBalanceForRentExemptMint,
  MINT_SIZE,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import BN from "bn.js";

export const PHOENIX_PROGRAM_ID = new PublicKey(
  process.env.PHOENIX_PROGRAM_ID || "PhoeNiXZ8ByJGLkxNfZRnkUfjvmuYqLR89jjFHGqdXY",
);
export const DELEGATION_PROGRAM_ID = new PublicKey(
  "DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh",
);
export const MAGIC_PROGRAM_ID = new PublicKey(
  "Magic11111111111111111111111111111111111111",
);
export const MAGIC_CONTEXT_ID = new PublicKey(
  "MagicContext1111111111111111111111111111111",
);

// PhoenixInstruction tags (matches src/program/instruction.rs).
export const TAG = {
  Swap: 0,
  SwapWithFreeFunds: 1,
  PlaceLimitOrder: 2,
  PlaceLimitOrderWithFreeFunds: 3,
  ReduceOrder: 4,
  ReduceOrderWithFreeFunds: 5,
  CancelAllOrders: 6,
  CancelAllOrdersWithFreeFunds: 7,
  CancelUpTo: 8,
  CancelUpToWithFreeFunds: 9,
  CancelMultipleOrdersById: 10,
  CancelMultipleOrdersByIdWithFreeFunds: 11,
  WithdrawFunds: 12,
  DepositFunds: 13,
  RequestSeat: 14,
  PlaceMultiplePostOnlyOrders: 16,
  PlaceMultiplePostOnlyOrdersWithFreeFunds: 17,
  InitializeMarket: 100,
  ChangeMarketStatus: 103,
  ChangeSeatStatus: 104,
  DelegateMarket: 200,
  CommitMarket: 201,
  CommitAndUndelegateMarket: 202,
  DelegateSeat: 203,
  RequestDeposit: 204,
  ProcessDepositEr: 205,
  CloseDepositReceipt: 206,
  RequestWithdrawal: 207,
  ProcessWithdrawalEr: 208,
  ExecuteWithdrawalBaseChain: 209,
  CreateSessionToken: 210,
  RevokeSessionToken: 211,
  PlaceLimitOrderViaSession: 212,
  SwapViaSession: 213,
  CancelAllOrdersViaSession: 214,
} as const;

const m = (
  pubkey: PublicKey,
  isWritable = false,
  isSigner = false,
): AccountMeta => ({ pubkey, isSigner, isWritable });

// =====================================================================
// OrderPacket — borsh encoding for Phoenix's matching engine.
// Mirrors src/state/order_schema/order_packet.rs.
//   Side:               Bid=0, Ask=1
//   SelfTradeBehavior:  Abort=0, CancelProvide=1, DecrementTake=2
// =====================================================================

export type Side = "bid" | "ask";
export type SelfTradeBehavior = "abort" | "cancel_provide" | "decrement_take";

export type OrderPacket =
  | {
      type: "post_only";
      side: Side;
      priceInTicks: bigint;
      numBaseLots: bigint;
      clientOrderId?: bigint;
      rejectPostOnly?: boolean;
      lastValidSlot?: bigint;
      lastValidUnixTimestampInSeconds?: bigint;
      failSilentlyOnInsufficientFunds?: boolean;
    }
  | {
      type: "limit";
      side: Side;
      priceInTicks: bigint;
      numBaseLots: bigint;
      selfTradeBehavior?: SelfTradeBehavior;
      matchLimit?: bigint;
      clientOrderId?: bigint;
      lastValidSlot?: bigint;
      lastValidUnixTimestampInSeconds?: bigint;
      failSilentlyOnInsufficientFunds?: boolean;
    }
  | {
      type: "ioc";
      side: Side;
      priceInTicks?: bigint;
      numBaseLots: bigint;
      numQuoteLots: bigint;
      minBaseLotsToFill?: bigint;
      minQuoteLotsToFill?: bigint;
      selfTradeBehavior?: SelfTradeBehavior;
      matchLimit?: bigint;
      clientOrderId?: bigint;
      lastValidSlot?: bigint;
      lastValidUnixTimestampInSeconds?: bigint;
    };

const u8 = (n: number) => {
  const b = Buffer.alloc(1);
  b.writeUInt8(n, 0);
  return b;
};
const u64 = (n: bigint) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(n, 0);
  return b;
};
const u128 = (n: bigint) => {
  const b = Buffer.alloc(16);
  b.writeBigUInt64LE(n & 0xffffffffffffffffn, 0);
  b.writeBigUInt64LE((n >> 64n) & 0xffffffffffffffffn, 8);
  return b;
};
const bool = (v: boolean) => u8(v ? 1 : 0);
const optU64 = (v?: bigint) =>
  v === undefined ? u8(0) : Buffer.concat([u8(1), u64(v)]);

const sideTag = (s: Side) => u8(s === "bid" ? 0 : 1);
const stbTag = (b?: SelfTradeBehavior) => {
  const map: Record<SelfTradeBehavior, number> = {
    abort: 0,
    cancel_provide: 1,
    decrement_take: 2,
  };
  return u8(map[b ?? "decrement_take"]);
};

export function encodeOrderPacket(p: OrderPacket): Buffer {
  const chunks: Buffer[] = [];
  switch (p.type) {
    case "post_only":
      chunks.push(u8(0));
      chunks.push(sideTag(p.side));
      chunks.push(u64(p.priceInTicks));
      chunks.push(u64(p.numBaseLots));
      chunks.push(u128(p.clientOrderId ?? 0n));
      chunks.push(bool(p.rejectPostOnly ?? true));
      chunks.push(bool(true)); // use_only_deposited_funds
      chunks.push(optU64(p.lastValidSlot));
      chunks.push(optU64(p.lastValidUnixTimestampInSeconds));
      chunks.push(bool(p.failSilentlyOnInsufficientFunds ?? false));
      break;
    case "limit":
      chunks.push(u8(1));
      chunks.push(sideTag(p.side));
      chunks.push(u64(p.priceInTicks));
      chunks.push(u64(p.numBaseLots));
      chunks.push(stbTag(p.selfTradeBehavior));
      chunks.push(
        p.matchLimit === undefined
          ? u8(0)
          : Buffer.concat([u8(1), u64(p.matchLimit)]),
      );
      chunks.push(u128(p.clientOrderId ?? 0n));
      chunks.push(bool(true));
      chunks.push(optU64(p.lastValidSlot));
      chunks.push(optU64(p.lastValidUnixTimestampInSeconds));
      chunks.push(bool(p.failSilentlyOnInsufficientFunds ?? false));
      break;
    case "ioc":
      chunks.push(u8(2));
      chunks.push(sideTag(p.side));
      chunks.push(optU64(p.priceInTicks));
      chunks.push(u64(p.numBaseLots));
      chunks.push(u64(p.numQuoteLots));
      chunks.push(u64(p.minBaseLotsToFill ?? 0n));
      chunks.push(u64(p.minQuoteLotsToFill ?? 0n));
      chunks.push(stbTag(p.selfTradeBehavior));
      chunks.push(
        p.matchLimit === undefined
          ? u8(0)
          : Buffer.concat([u8(1), u64(p.matchLimit)]),
      );
      chunks.push(u128(p.clientOrderId ?? 0n));
      chunks.push(bool(true));
      chunks.push(optU64(p.lastValidSlot));
      chunks.push(optU64(p.lastValidUnixTimestampInSeconds));
      break;
  }
  return Buffer.concat(chunks);
}

// CancelOrderParams (borsh: side u8 + price_in_ticks u64 + order_sequence_number u64 = 17 bytes)
export interface CancelOrderParam {
  side: Side;
  priceInTicks: bigint;
  orderSequenceNumber: bigint;
}

export function encodeCancelMultipleOrdersByIdParams(
  orders: CancelOrderParam[],
): Buffer {
  // Vec<T> = u32 length + items
  const head = Buffer.alloc(4);
  head.writeUInt32LE(orders.length, 0);
  const items = orders.map((o) =>
    Buffer.concat([sideTag(o.side), u64(o.priceInTicks), u64(o.orderSequenceNumber)]),
  );
  return Buffer.concat([head, ...items]);
}

export class TestClient {
  provider: anchor.AnchorProvider;
  connection: Connection;
  routerConnection: Connection;
  admin: Keypair;
  printErrors: boolean;

  // Persisted across `it()` blocks
  baseMint!: PublicKey;
  quoteMint!: PublicKey;
  market!: PublicKey;
  traderBaseAta!: PublicKey;
  traderQuoteAta!: PublicKey;
  isDelegated: boolean = false;

  constructor() {
    this.provider = anchor.AnchorProvider.env();
    anchor.setProvider(this.provider);
    this.connection = this.provider.connection;
    this.routerConnection = new Connection(
      process.env.ROUTER_ENDPOINT || "https://devnet-router.magicblock.app",
      {
        wsEndpoint:
          process.env.ROUTER_WS_ENDPOINT || "wss://devnet-router.magicblock.app",
        commitment: "confirmed",
      },
    );
    this.admin = (this.provider.wallet as anchor.Wallet).payer;
    this.printErrors = true;
  }

  // ===================================================================
  // PDA derivations
  // ===================================================================

  findMarketAddress(
    baseMint: PublicKey,
    quoteMint: PublicKey,
    creator: PublicKey,
  ): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [
        Buffer.from("market"),
        baseMint.toBuffer(),
        quoteMint.toBuffer(),
        creator.toBuffer(),
      ],
      PHOENIX_PROGRAM_ID,
    );
  }

  findVaultAddress(market: PublicKey, mint: PublicKey): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), market.toBuffer(), mint.toBuffer()],
      PHOENIX_PROGRAM_ID,
    );
  }

  findSeatAddress(market: PublicKey, trader: PublicKey): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("seat"), market.toBuffer(), trader.toBuffer()],
      PHOENIX_PROGRAM_ID,
    );
  }

  findDepositReceiptAddress(
    market: PublicKey,
    trader: PublicKey,
  ): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [
        Buffer.from("deposit_receipt"),
        market.toBuffer(),
        trader.toBuffer(),
      ],
      PHOENIX_PROGRAM_ID,
    );
  }

  findWithdrawalReceiptAddress(
    market: PublicKey,
    trader: PublicKey,
  ): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [
        Buffer.from("withdrawal_receipt"),
        market.toBuffer(),
        trader.toBuffer(),
      ],
      PHOENIX_PROGRAM_ID,
    );
  }

  findSessionTokenAddress(
    owner: PublicKey,
    sessionSigner: PublicKey,
  ): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("session"), owner.toBuffer(), sessionSigner.toBuffer()],
      PHOENIX_PROGRAM_ID,
    );
  }

  findLogAuthority(): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("log")],
      PHOENIX_PROGRAM_ID,
    );
  }

  findDelegationBuffer(pda: PublicKey): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("buffer"), pda.toBuffer()],
      PHOENIX_PROGRAM_ID,
    );
  }

  findDelegationRecord(pda: PublicKey): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("delegation"), pda.toBuffer()],
      DELEGATION_PROGRAM_ID,
    );
  }

  findDelegationMetadata(pda: PublicKey): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("delegation-metadata"), pda.toBuffer()],
      DELEGATION_PROGRAM_ID,
    );
  }

  // ===================================================================
  // Setup: mints + ATAs
  // ===================================================================

  /** Create base (9-dec) + quote (6-dec) SPL mints in one tx. */
  async createMints(): Promise<{
    baseMint: Keypair;
    quoteMint: Keypair;
    txSig: string;
  }> {
    const baseMint = Keypair.generate();
    const quoteMint = Keypair.generate();
    const rent = await getMinimumBalanceForRentExemptMint(this.connection);
    const tx = new Transaction()
      .add(
        SystemProgram.createAccount({
          fromPubkey: this.admin.publicKey,
          newAccountPubkey: baseMint.publicKey,
          space: MINT_SIZE,
          lamports: rent,
          programId: TOKEN_PROGRAM_ID,
        }),
      )
      .add(
        createInitializeMint2Instruction(
          baseMint.publicKey,
          9,
          this.admin.publicKey,
          null,
        ),
      )
      .add(
        SystemProgram.createAccount({
          fromPubkey: this.admin.publicKey,
          newAccountPubkey: quoteMint.publicKey,
          space: MINT_SIZE,
          lamports: rent,
          programId: TOKEN_PROGRAM_ID,
        }),
      )
      .add(
        createInitializeMint2Instruction(
          quoteMint.publicKey,
          6,
          this.admin.publicKey,
          null,
        ),
      );
    const txSig = await this.provider.sendAndConfirm(tx, [baseMint, quoteMint]);
    this.baseMint = baseMint.publicKey;
    this.quoteMint = quoteMint.publicKey;
    return { baseMint, quoteMint, txSig };
  }

  /** Create trader ATAs and mint initial supply. */
  async fundTraderAtas(
    baseAmount: bigint,
    quoteAmount: bigint,
  ): Promise<string> {
    this.traderBaseAta = getAssociatedTokenAddressSync(
      this.baseMint,
      this.admin.publicKey,
    );
    this.traderQuoteAta = getAssociatedTokenAddressSync(
      this.quoteMint,
      this.admin.publicKey,
    );
    const tx = new Transaction()
      .add(
        createAssociatedTokenAccountIdempotentInstruction(
          this.admin.publicKey,
          this.traderBaseAta,
          this.admin.publicKey,
          this.baseMint,
        ),
      )
      .add(
        createAssociatedTokenAccountIdempotentInstruction(
          this.admin.publicKey,
          this.traderQuoteAta,
          this.admin.publicKey,
          this.quoteMint,
        ),
      )
      .add(
        createMintToInstruction(
          this.baseMint,
          this.traderBaseAta,
          this.admin.publicKey,
          baseAmount,
        ),
      )
      .add(
        createMintToInstruction(
          this.quoteMint,
          this.traderQuoteAta,
          this.admin.publicKey,
          quoteAmount,
        ),
      );
    return await this.provider.sendAndConfirm(tx);
  }

  async getAtaBalances(): Promise<{ base: bigint; quote: bigint }> {
    const baseAcct = await getAccount(this.connection, this.traderBaseAta);
    const quoteAcct = await getAccount(this.connection, this.traderQuoteAta);
    return { base: baseAcct.amount, quote: quoteAcct.amount };
  }

  // ===================================================================
  // Phoenix base-layer instructions
  // ===================================================================

  /** InitializeMarket (PDA flow). Program self-allocates the market. */
  async initializeMarket(params: {
    bidsSize: BN;
    asksSize: BN;
    numSeats: BN;
    numQuoteLotsPerQuoteUnit: BN;
    tickSizeInQuoteLotsPerBaseUnit: BN;
    numBaseLotsPerBaseUnit: BN;
    takerFeeBps: number;
    rawBaseUnitsPerBaseUnit?: number | null;
  }): Promise<string> {
    const [market] = this.findMarketAddress(
      this.baseMint,
      this.quoteMint,
      this.admin.publicKey,
    );
    this.market = market;
    const [logAuth] = this.findLogAuthority();
    const [baseVault] = this.findVaultAddress(market, this.baseMint);
    const [quoteVault] = this.findVaultAddress(market, this.quoteMint);

    const rawBaseUnits =
      params.rawBaseUnitsPerBaseUnit === undefined
        ? null
        : params.rawBaseUnitsPerBaseUnit;
    const dataLen = 8 * 3 + 8 + 8 + 8 + 2 + 32 + (rawBaseUnits !== null ? 5 : 1);
    const data = Buffer.alloc(1 + dataLen);
    let o = 0;
    data.writeUInt8(TAG.InitializeMarket, o); o += 1;
    data.writeBigUInt64LE(BigInt(params.bidsSize.toString()), o); o += 8;
    data.writeBigUInt64LE(BigInt(params.asksSize.toString()), o); o += 8;
    data.writeBigUInt64LE(BigInt(params.numSeats.toString()), o); o += 8;
    data.writeBigUInt64LE(BigInt(params.numQuoteLotsPerQuoteUnit.toString()), o); o += 8;
    data.writeBigUInt64LE(BigInt(params.tickSizeInQuoteLotsPerBaseUnit.toString()), o); o += 8;
    data.writeBigUInt64LE(BigInt(params.numBaseLotsPerBaseUnit.toString()), o); o += 8;
    data.writeUInt16LE(params.takerFeeBps, o); o += 2;
    this.admin.publicKey.toBuffer().copy(data, o); o += 32;
    if (rawBaseUnits !== null) {
      data.writeUInt8(1, o); o += 1;
      data.writeUInt32LE(rawBaseUnits, o);
    } else {
      data.writeUInt8(0, o);
    }

    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(PHOENIX_PROGRAM_ID),
        m(logAuth),
        m(market, true),
        m(this.admin.publicKey, true, true),
        m(this.baseMint),
        m(this.quoteMint),
        m(baseVault, true),
        m(quoteVault, true),
        m(SystemProgram.programId),
        m(TOKEN_PROGRAM_ID),
      ],
      data,
    });
    return await this.provider.sendAndConfirm(new Transaction().add(ix));
  }

  async requestSeat(): Promise<string> {
    const [logAuth] = this.findLogAuthority();
    const [seat] = this.findSeatAddress(this.market, this.admin.publicKey);
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(PHOENIX_PROGRAM_ID),
        m(logAuth),
        m(this.market, true),
        m(this.admin.publicKey, true, true),
        m(seat, true),
        m(SystemProgram.programId),
      ],
      data: Buffer.from([TAG.RequestSeat]),
    });
    return await this.provider.sendAndConfirm(new Transaction().add(ix));
  }

  /** ChangeMarketStatus — admin ix. MarketStatus enum:
   *  0=Uninitialized, 1=Active, 2=PostOnly, 3=Paused, 4=Closed, 5=Tombstoned.
   *  Cross/IOC (Swap) requires Active; PostOnly only allows posts+reduces. */
  async changeMarketStatus(status: number): Promise<string> {
    const [logAuth] = this.findLogAuthority();
    const data = Buffer.from([TAG.ChangeMarketStatus, status]);
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(PHOENIX_PROGRAM_ID),
        m(logAuth),
        m(this.market, true),
        m(this.admin.publicKey, false, true),
      ],
      data,
    });
    return await this.provider.sendAndConfirm(new Transaction().add(ix));
  }

  async changeSeatStatus(
    trader: PublicKey,
    status: number, // 0=NotApproved, 1=Approved, 2=Retired
  ): Promise<string> {
    const [logAuth] = this.findLogAuthority();
    const [seat] = this.findSeatAddress(this.market, trader);
    // BorshSerialize on a unit-variant enum produces a single u8 tag,
    // regardless of #[repr(u64)] on the Rust side.
    const data = Buffer.from([TAG.ChangeSeatStatus, status]);
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(PHOENIX_PROGRAM_ID),
        m(logAuth),
        m(this.market, true),
        m(this.admin.publicKey, false, true),
        m(seat, true),
      ],
      data,
    });
    return await this.provider.sendAndConfirm(new Transaction().add(ix));
  }

  async depositFunds(baseLots: BN, quoteLots: BN): Promise<string> {
    const [logAuth] = this.findLogAuthority();
    const [seat] = this.findSeatAddress(this.market, this.admin.publicKey);
    const [baseVault] = this.findVaultAddress(this.market, this.baseMint);
    const [quoteVault] = this.findVaultAddress(this.market, this.quoteMint);
    const data = Buffer.alloc(1 + 16);
    data.writeUInt8(TAG.DepositFunds, 0);
    data.writeBigUInt64LE(BigInt(quoteLots.toString()), 1);
    data.writeBigUInt64LE(BigInt(baseLots.toString()), 9);
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(PHOENIX_PROGRAM_ID),
        m(logAuth),
        m(this.market, true),
        m(this.admin.publicKey, false, true),
        m(seat),
        m(this.traderBaseAta, true),
        m(this.traderQuoteAta, true),
        m(baseVault, true),
        m(quoteVault, true),
        m(TOKEN_PROGRAM_ID),
      ],
      data,
    });
    return await this.provider.sendAndConfirm(new Transaction().add(ix));
  }

  // ===================================================================
  // MagicBlock delegation instructions
  // ===================================================================

  private optionPubkey(pk: PublicKey | null): Buffer {
    return pk === null ? Buffer.from([0]) : Buffer.concat([Buffer.from([1]), pk.toBuffer()]);
  }

  async delegateMarket(validator: PublicKey | null = null): Promise<string> {
    const [buffer] = this.findDelegationBuffer(this.market);
    const [record] = this.findDelegationRecord(this.market);
    const [metadata] = this.findDelegationMetadata(this.market);
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(this.admin.publicKey, true, true),
        m(SystemProgram.programId),
        m(this.market, true),
        m(PHOENIX_PROGRAM_ID),
        m(buffer, true),
        m(record, true),
        m(metadata, true),
        m(DELEGATION_PROGRAM_ID),
      ],
      data: Buffer.concat([Buffer.from([TAG.DelegateMarket]), this.optionPubkey(validator)]),
    });
    const sig = await this.provider.sendAndConfirm(new Transaction().add(ix));
    this.isDelegated = true;
    return sig;
  }

  async delegateSeat(
    trader: PublicKey,
    validator: PublicKey | null = null,
  ): Promise<string> {
    const [seat] = this.findSeatAddress(this.market, trader);
    const [buffer] = this.findDelegationBuffer(seat);
    const [record] = this.findDelegationRecord(seat);
    const [metadata] = this.findDelegationMetadata(seat);
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(this.admin.publicKey, true, true),
        m(SystemProgram.programId),
        m(this.market),
        m(seat, true),
        m(trader),
        m(PHOENIX_PROGRAM_ID),
        m(buffer, true),
        m(record, true),
        m(metadata, true),
        m(DELEGATION_PROGRAM_ID),
      ],
      data: Buffer.concat([Buffer.from([TAG.DelegateSeat]), this.optionPubkey(validator)]),
    });
    return await this.provider.sendAndConfirm(new Transaction().add(ix));
  }

  // ===================================================================
  // Magic Action flows (single user signature; rest auto-fires)
  // ===================================================================

  async requestDeposit(baseLots: BN, quoteLots: BN): Promise<string> {
    const [baseVault] = this.findVaultAddress(this.market, this.baseMint);
    const [quoteVault] = this.findVaultAddress(this.market, this.quoteMint);
    const [receipt] = this.findDepositReceiptAddress(this.market, this.admin.publicKey);
    const [buffer] = this.findDelegationBuffer(receipt);
    const [record] = this.findDelegationRecord(receipt);
    const [metadata] = this.findDelegationMetadata(receipt);
    const data = Buffer.alloc(1 + 16);
    data.writeUInt8(TAG.RequestDeposit, 0);
    data.writeBigUInt64LE(BigInt(quoteLots.toString()), 1);
    data.writeBigUInt64LE(BigInt(baseLots.toString()), 9);
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(this.admin.publicKey, true, true),
        m(SystemProgram.programId),
        m(this.market, true),
        m(this.traderBaseAta, true),
        m(this.traderQuoteAta, true),
        m(baseVault, true),
        m(quoteVault, true),
        m(TOKEN_PROGRAM_ID),
        m(receipt, true),
        m(PHOENIX_PROGRAM_ID),
        m(buffer, true),
        m(record, true),
        m(metadata, true),
        m(DELEGATION_PROGRAM_ID),
        m(MAGIC_PROGRAM_ID),
        m(MAGIC_CONTEXT_ID, true),
      ],
      data,
    });
    const tx = new Transaction().add(ix);
    const { blockhash } = await this.connection.getLatestBlockhash();
    tx.recentBlockhash = blockhash;
    tx.feePayer = this.admin.publicKey;
    tx.sign(this.admin);
    const sig = await this.connection.sendRawTransaction(tx.serialize(), {
      skipPreflight: true,
    });
    await this.connection.confirmTransaction(sig, "confirmed");
    return sig;
  }

  async requestWithdrawal(baseLots: BN, quoteLots: BN): Promise<string> {
    const [baseVault] = this.findVaultAddress(this.market, this.baseMint);
    const [quoteVault] = this.findVaultAddress(this.market, this.quoteMint);
    const [receipt] = this.findWithdrawalReceiptAddress(this.market, this.admin.publicKey);
    const [buffer] = this.findDelegationBuffer(receipt);
    const [record] = this.findDelegationRecord(receipt);
    const [metadata] = this.findDelegationMetadata(receipt);
    const data = Buffer.alloc(1 + 16);
    data.writeUInt8(TAG.RequestWithdrawal, 0);
    data.writeBigUInt64LE(BigInt(baseLots.toString()), 1);
    data.writeBigUInt64LE(BigInt(quoteLots.toString()), 9);
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(this.admin.publicKey, true, true),
        m(SystemProgram.programId),
        m(receipt, true),
        m(this.market),
        m(this.traderBaseAta, true),
        m(this.traderQuoteAta, true),
        m(baseVault, true),
        m(quoteVault, true),
        m(TOKEN_PROGRAM_ID),
        m(PHOENIX_PROGRAM_ID),
        m(buffer, true),
        m(record, true),
        m(metadata, true),
        m(DELEGATION_PROGRAM_ID),
        m(MAGIC_PROGRAM_ID),
        m(MAGIC_CONTEXT_ID, true),
      ],
      data,
    });
    const tx = new Transaction().add(ix);
    const { blockhash } = await this.connection.getLatestBlockhash();
    tx.recentBlockhash = blockhash;
    tx.feePayer = this.admin.publicKey;
    tx.sign(this.admin);
    const sig = await this.connection.sendRawTransaction(tx.serialize(), {
      skipPreflight: true,
    });
    await this.connection.confirmTransaction(sig, "confirmed");
    return sig;
  }

  // ===================================================================
  // ER-direct send helper (for ixs that hit delegated state directly,
  // bypassing the Magic Router auto-routing)
  // ===================================================================

  async sendOnRouter(ix: TransactionInstruction): Promise<string> {
    const tx = new Transaction().add(ix);
    const { blockhash } = await this.routerConnection.getLatestBlockhash();
    tx.recentBlockhash = blockhash;
    tx.feePayer = this.admin.publicKey;
    tx.sign(this.admin);
    const sig = await this.routerConnection.sendRawTransaction(tx.serialize(), {
      skipPreflight: true,
    });
    await this.routerConnection.confirmTransaction(sig, "confirmed");

    // confirmTransaction returns when the tx is confirmed at block level,
    // NOT when the program succeeded. We must explicitly fetch the tx and
    // verify meta.err is null — otherwise silent on-chain failures slip
    // through as test passes.
    for (let i = 0; i < 10; i++) {
      const tx = await this.routerConnection.getTransaction(sig, {
        commitment: "confirmed",
        maxSupportedTransactionVersion: 0,
      });
      if (tx) {
        if (tx.meta?.err) {
          const logs = (tx.meta.logMessages ?? []).join("\n  ");
          throw new Error(
            `ER tx ${sig} failed on-chain: ${JSON.stringify(tx.meta.err)}\n  ${logs}`,
          );
        }
        return sig;
      }
      await new Promise((r) => setTimeout(r, 1000));
    }
    throw new Error(`ER tx ${sig} not retrievable after confirmation`);
  }

  // ===================================================================
  // Manual settlement fallback — used when the post-undelegate Magic
  // Action doesn't auto-fire on devnet.
  // ===================================================================

  /** Waits until the receipt PDA is closed (auto-fired post-undelegate
   *  action). Returns when getAccountInfo returns null. */
  async waitForReceiptClosed(
    receipt: PublicKey,
    timeoutMs: number = 120_000,
  ): Promise<void> {
    const start = Date.now();
    while (Date.now() - start < timeoutMs) {
      const info = await this.connection.getAccountInfo(receipt);
      if (info === null) return;
      await new Promise((r) => setTimeout(r, 2000));
    }
    throw new Error(`Receipt ${receipt.toBase58()} not closed within ${timeoutMs}ms`);
  }

  async closeDepositReceipt(): Promise<string> {
    const [receipt] = this.findDepositReceiptAddress(this.market, this.admin.publicKey);
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(this.admin.publicKey, true, false), // trader (writable, not signer)
        m(receipt, true),                      // receipt
      ],
      data: Buffer.from([TAG.CloseDepositReceipt]),
    });
    const tx = new Transaction().add(ix);
    const { blockhash } = await this.connection.getLatestBlockhash();
    tx.recentBlockhash = blockhash;
    tx.feePayer = this.admin.publicKey;
    tx.sign(this.admin);
    const sig = await this.connection.sendRawTransaction(tx.serialize(), {
      skipPreflight: true,
    });
    await this.connection.confirmTransaction(sig, "confirmed");
    return sig;
  }

  async executeWithdrawalBaseChain(): Promise<string> {
    const [receipt] = this.findWithdrawalReceiptAddress(this.market, this.admin.publicKey);
    const [baseVault] = this.findVaultAddress(this.market, this.baseMint);
    const [quoteVault] = this.findVaultAddress(this.market, this.quoteMint);
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(this.admin.publicKey, true, false), // trader (writable, not signer)
        m(this.market),
        m(this.traderBaseAta, true),
        m(this.traderQuoteAta, true),
        m(baseVault, true),
        m(quoteVault, true),
        m(TOKEN_PROGRAM_ID),
        m(receipt, true),
      ],
      data: Buffer.from([TAG.ExecuteWithdrawalBaseChain]),
    });
    const tx = new Transaction().add(ix);
    const { blockhash } = await this.connection.getLatestBlockhash();
    tx.recentBlockhash = blockhash;
    tx.feePayer = this.admin.publicKey;
    tx.sign(this.admin);
    const sig = await this.connection.sendRawTransaction(tx.serialize(), {
      skipPreflight: true,
    });
    await this.connection.confirmTransaction(sig, "confirmed");
    return sig;
  }

  // ===================================================================
  // ER trading — place / match / cancel via Magic Router
  // ===================================================================

  /** PlaceLimitOrderWithFreeFunds on the delegated market via the ER.
   *  Matching against the opposite book (FIFO) happens inside this ix. */
  async placeLimitOrderOnEr(packet: OrderPacket): Promise<string> {
    const [logAuth] = this.findLogAuthority();
    const [seat] = this.findSeatAddress(this.market, this.admin.publicKey);
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(PHOENIX_PROGRAM_ID),
        m(logAuth),
        m(this.market, true),
        m(this.admin.publicKey, false, true),
        m(seat),
      ],
      data: Buffer.concat([
        Buffer.from([TAG.PlaceLimitOrderWithFreeFunds]),
        encodeOrderPacket(packet),
      ]),
    });
    return await this.sendOnRouter(ix);
  }

  /** SwapWithFreeFunds — aggressive IOC cross. Same accounts as place. */
  async swapOnEr(packet: OrderPacket): Promise<string> {
    const [logAuth] = this.findLogAuthority();
    const [seat] = this.findSeatAddress(this.market, this.admin.publicKey);
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(PHOENIX_PROGRAM_ID),
        m(logAuth),
        m(this.market, true),
        m(this.admin.publicKey, false, true),
        m(seat),
      ],
      data: Buffer.concat([
        Buffer.from([TAG.SwapWithFreeFunds]),
        encodeOrderPacket(packet),
      ]),
    });
    return await this.sendOnRouter(ix);
  }

  /** CancelMultipleOrdersByIdWithFreeFunds — surgical cancel by (side, price, sequence). */
  async cancelMultipleOrdersByIdOnEr(
    orders: CancelOrderParam[],
  ): Promise<string> {
    const [logAuth] = this.findLogAuthority();
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(PHOENIX_PROGRAM_ID),
        m(logAuth),
        m(this.market, true),
        m(this.admin.publicKey, false, true),
      ],
      data: Buffer.concat([
        Buffer.from([TAG.CancelMultipleOrdersByIdWithFreeFunds]),
        encodeCancelMultipleOrdersByIdParams(orders),
      ]),
    });
    return await this.sendOnRouter(ix);
  }

  /** Read the trader's TraderState from the (possibly delegated) market
   *  account fetched from a specific connection (base or ER). */
  async readTraderStateFromMarket(
    conn: Connection,
  ): Promise<{ baseFree: bigint; quoteFree: bigint; baseLocked: bigint; quoteLocked: bigint } | null> {
    const info = await conn.getAccountInfo(this.market);
    if (!info) return null;
    const traderBytes = this.admin.publicKey.toBytes();
    for (let i = 576; i < info.data.length - 32; i++) {
      let match = true;
      for (let j = 0; j < 32; j++) {
        if (info.data[i + j] !== traderBytes[j]) {
          match = false;
          break;
        }
      }
      if (match) {
        const ts = i + 32;
        const dv = new DataView(
          info.data.buffer,
          info.data.byteOffset + ts,
          96,
        );
        return {
          quoteLocked: dv.getBigUint64(0, true),
          quoteFree: dv.getBigUint64(8, true),
          baseLocked: dv.getBigUint64(16, true),
          baseFree: dv.getBigUint64(24, true),
        };
      }
    }
    return null;
  }

  // ===================================================================
  // Session keys
  // ===================================================================

  /** Create a SessionToken authorizing `sessionSigner` to act for the
   *  admin (owner) until `expiresAt` (unix timestamp, 0 = never). */
  async createSessionToken(
    sessionSigner: PublicKey,
    expiresAt: bigint,
  ): Promise<string> {
    const [token] = this.findSessionTokenAddress(this.admin.publicKey, sessionSigner);
    const data = Buffer.alloc(1 + 32 + 8);
    data.writeUInt8(TAG.CreateSessionToken, 0);
    sessionSigner.toBuffer().copy(data, 1);
    data.writeBigInt64LE(expiresAt, 33);
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(this.admin.publicKey, true, true),
        m(token, true),
        m(SystemProgram.programId),
      ],
      data,
    });
    return await this.provider.sendAndConfirm(new Transaction().add(ix));
  }

  async revokeSessionToken(sessionSigner: PublicKey): Promise<string> {
    const [token] = this.findSessionTokenAddress(this.admin.publicKey, sessionSigner);
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(this.admin.publicKey, true, true),
        m(token, true),
      ],
      data: Buffer.from([TAG.RevokeSessionToken]),
    });
    return await this.provider.sendAndConfirm(new Transaction().add(ix));
  }

  /** Send a session-signed ix to the ER via the Magic Router. The
   *  `sessionSigner` keypair signs the tx (not the admin/owner wallet). */
  private async sendSessionOnRouter(
    sessionSigner: Keypair,
    ix: TransactionInstruction,
  ): Promise<string> {
    const tx = new Transaction().add(ix);
    const { blockhash } = await this.routerConnection.getLatestBlockhash();
    tx.recentBlockhash = blockhash;
    tx.feePayer = sessionSigner.publicKey;
    tx.sign(sessionSigner);
    const sig = await this.routerConnection.sendRawTransaction(tx.serialize(), {
      skipPreflight: true,
    });
    await this.routerConnection.confirmTransaction(sig, "confirmed");
    for (let i = 0; i < 10; i++) {
      const tx2 = await this.routerConnection.getTransaction(sig, {
        commitment: "confirmed",
        maxSupportedTransactionVersion: 0,
      });
      if (tx2) {
        if (tx2.meta?.err) {
          const logs = (tx2.meta.logMessages ?? []).join("\n  ");
          throw new Error(
            `Session tx ${sig} failed on-chain: ${JSON.stringify(tx2.meta.err)}\n  ${logs}`,
          );
        }
        return sig;
      }
      await new Promise((r) => setTimeout(r, 1000));
    }
    throw new Error(`Session tx ${sig} not retrievable after confirmation`);
  }

  /** Place a limit order on the ER signed by a session key (not the
   *  owner wallet). The order is attributed to `this.admin` (owner). */
  async placeLimitOrderViaSession(
    sessionSigner: Keypair,
    packet: OrderPacket,
  ): Promise<string> {
    const [logAuth] = this.findLogAuthority();
    const [seat] = this.findSeatAddress(this.market, this.admin.publicKey);
    const [token] = this.findSessionTokenAddress(
      this.admin.publicKey,
      sessionSigner.publicKey,
    );
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(PHOENIX_PROGRAM_ID),
        m(logAuth),
        m(this.market, true),
        m(sessionSigner.publicKey, true, true),
        m(this.admin.publicKey),
        m(token),
        m(seat),
      ],
      data: Buffer.concat([
        Buffer.from([TAG.PlaceLimitOrderViaSession]),
        encodeOrderPacket(packet),
      ]),
    });
    return await this.sendSessionOnRouter(sessionSigner, ix);
  }

  async swapViaSession(
    sessionSigner: Keypair,
    packet: OrderPacket,
  ): Promise<string> {
    const [logAuth] = this.findLogAuthority();
    const [seat] = this.findSeatAddress(this.market, this.admin.publicKey);
    const [token] = this.findSessionTokenAddress(
      this.admin.publicKey,
      sessionSigner.publicKey,
    );
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(PHOENIX_PROGRAM_ID),
        m(logAuth),
        m(this.market, true),
        m(sessionSigner.publicKey, true, true),
        m(this.admin.publicKey),
        m(token),
        m(seat),
      ],
      data: Buffer.concat([
        Buffer.from([TAG.SwapViaSession]),
        encodeOrderPacket(packet),
      ]),
    });
    return await this.sendSessionOnRouter(sessionSigner, ix);
  }

  async cancelAllOrdersViaSession(sessionSigner: Keypair): Promise<string> {
    const [logAuth] = this.findLogAuthority();
    const [token] = this.findSessionTokenAddress(
      this.admin.publicKey,
      sessionSigner.publicKey,
    );
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(PHOENIX_PROGRAM_ID),
        m(logAuth),
        m(this.market, true),
        m(sessionSigner.publicKey, true, true),
        m(this.admin.publicKey),
        m(token),
      ],
      data: Buffer.from([TAG.CancelAllOrdersViaSession]),
    });
    return await this.sendSessionOnRouter(sessionSigner, ix);
  }

  /** Smoke-test: CancelAllOrdersWithFreeFunds on the ER. */
  async cancelAllOrdersOnEr(): Promise<string> {
    const [logAuth] = this.findLogAuthority();
    const ix = new TransactionInstruction({
      programId: PHOENIX_PROGRAM_ID,
      keys: [
        m(PHOENIX_PROGRAM_ID),
        m(logAuth),
        m(this.market, false, false),
        m(this.admin.publicKey, true, true),
      ],
      data: Buffer.from([TAG.CancelAllOrdersWithFreeFunds]),
    });
    // Note: market is writable on the ER; the AccountMeta above is for
    // building the wire. We mark it writable here.
    ix.keys[2].isWritable = true;
    return await this.sendOnRouter(ix);
  }
}
