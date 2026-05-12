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
} as const;

const m = (
  pubkey: PublicKey,
  isWritable = false,
  isSigner = false,
): AccountMeta => ({ pubkey, isSigner, isWritable });

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
    return sig;
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
