// PerpTestClient — wraps Anchor provider, base + ER connections, and exposes
// every perp_router instruction as an async method returning the confirmed
// transaction signature. Mirrors the style of phoenix-v1/tests-ts/anchor/
// test_client.ts.
//
// Env vars:
//   ANCHOR_PROVIDER_URL    — base layer RPC (default: devnet)
//   ANCHOR_WALLET          — keypair path (default: ~/.config/solana/id.json)
//   ROUTER_ENDPOINT        — Magic Router RPC (default: devnet router)
//   ROUTER_WS_ENDPOINT     — Magic Router WS  (default: devnet router)
//   PERP_ROUTER_PROGRAM_ID — deployed perp_router program (required)

import * as anchor from "@coral-xyz/anchor";
import {
  AccountMeta,
  Connection,
  Keypair,
  PublicKey,
  Signer,
  SystemProgram,
  SYSVAR_CLOCK_PUBKEY,
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

export const PERP_ROUTER_PROGRAM_ID = new PublicKey(
  process.env.PERP_ROUTER_PROGRAM_ID ||
    "A1eqsa75nTvgBpN6NKxQceXop1gjwi8fZc12SgBgLFDz",
);
export const PHOENIX_PROGRAM_ID = new PublicKey(
  process.env.PHOENIX_PROGRAM_ID ||
    "PhoeNiXZ8ByJGLkxNfZRnkUfjvmuYqLR89jjFHGqdXY",
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

// Instruction discriminants (matches perp_router/src/lib.rs).
export const TAG = {
  InitializeMarket: 0,
  InitializeTrader: 1,
  DelegateTraderAccount: 2,
  DelegateGlobalState: 3,
  DelegatePerpMarket: 4,
  UndelegateTraderAccount: 5,
  RequestCollateralDeposit: 6,
  ProcessCollateralDepositEr: 7,
  CloseCollateralDepositReceipt: 8,
  RequestCollateralWithdrawal: 9,
  ProcessCollateralWithdrawalEr: 10,
  ExecuteCollateralWithdrawalBaseChain: 11,
  OpenPosition: 12,
  ClosePosition: 13,
  Liquidate: 14,
  MaturePnl: 15,
  CrankFunding: 16,
  RecoveryCheck: 17,
  ScheduleCranks: 18,
  InitializeGlobalState: 19,
  DirectDeposit: 20,
  DirectWithdraw: 21,
  DirectOpenPosition: 22,
  DirectClosePosition: 23,
  InitializeOrderbook: 24,
  DelegateOrderbook: 25,
  ClaimSeat: 26,
  PlaceOrderPerp: 27,
} as const;

// PDA seed prefixes (matches perp_router/src/constants.rs).
const GLOBAL_STATE_SEED = Buffer.from("global_state");
const PERP_MARKET_SEED = Buffer.from("perp_market");
const TRADER_ACCOUNT_SEED = Buffer.from("trader_account");
const PERP_AUTHORITY_SEED = Buffer.from("perp_authority");
const DEPOSIT_RECEIPT_SEED = Buffer.from("perp_deposit_receipt");
const WITHDRAWAL_RECEIPT_SEED = Buffer.from("perp_withdrawal_receipt");
const ORDERBOOK_SEED = Buffer.from("orderbook");

const u64 = (n: number | bigint | BN) => {
  const b = BN.isBN(n) ? n : new BN(n.toString());
  return b.toArrayLike(Buffer, "le", 8);
};
const u128 = (n: number | bigint | BN) => {
  const b = BN.isBN(n) ? n : new BN(n.toString());
  return b.toArrayLike(Buffer, "le", 16);
};
const i64 = (n: number | bigint | BN) => {
  let b = BN.isBN(n) ? n : new BN(n.toString());
  if (b.isNeg()) {
    b = b.toTwos(64);
  }
  return b.toArrayLike(Buffer, "le", 8);
};
const u32 = (n: number) => {
  const buf = Buffer.alloc(4);
  buf.writeUInt32LE(n, 0);
  return buf;
};
const u16 = (n: number) => {
  const buf = Buffer.alloc(2);
  buf.writeUInt16LE(n, 0);
  return buf;
};
const u8 = (n: number) => Buffer.from([n & 0xff]);
const optionPubkey = (k: PublicKey | null) =>
  k ? Buffer.concat([u8(1), k.toBuffer()]) : u8(0);

export class PerpTestClient {
  baseConnection: Connection;
  erConnection: Connection;
  payer: Keypair;
  admin: Keypair;

  constructor(payer: Keypair) {
    const baseUrl =
      process.env.ANCHOR_PROVIDER_URL || "https://api.devnet.solana.com";
    const routerUrl =
      process.env.ROUTER_ENDPOINT || "https://devnet-router.magicblock.app";
    const routerWs =
      process.env.ROUTER_WS_ENDPOINT || "wss://devnet-router.magicblock.app";
    this.baseConnection = new Connection(baseUrl, "confirmed");
    this.erConnection = new Connection(routerUrl, {
      commitment: "confirmed",
      wsEndpoint: routerWs,
    });
    this.payer = payer;
    this.admin = payer; // single-keypair test for v1
  }

  // ─── PDA derivations ──────────────────────────────────────────────────

  globalStatePda(): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [GLOBAL_STATE_SEED],
      PERP_ROUTER_PROGRAM_ID,
    );
  }
  perpMarketPda(phoenixMarket: PublicKey): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [PERP_MARKET_SEED, phoenixMarket.toBuffer()],
      PERP_ROUTER_PROGRAM_ID,
    );
  }
  traderAccountPda(owner: PublicKey): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [TRADER_ACCOUNT_SEED, owner.toBuffer()],
      PERP_ROUTER_PROGRAM_ID,
    );
  }
  perpAuthorityPda(): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [PERP_AUTHORITY_SEED],
      PERP_ROUTER_PROGRAM_ID,
    );
  }

  /**
   * Collateral and per-market base vaults are ATAs of perp_authority for
   * the relevant mint (USDC for collateral, base mint for trading
   * inventory). Use this for both — same derivation.
   */
  vaultAta(mint: PublicKey): PublicKey {
    const [authority] = this.perpAuthorityPda();
    return getAssociatedTokenAddressSync(mint, authority, true);
  }

  /** Phoenix's static log authority — `[b"log"]` PDA of the Phoenix program. */
  phoenixLogAuthority(): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("log")],
      PHOENIX_PROGRAM_ID,
    );
  }

  /** Phoenix's per-market token vault — `[b"vault", market, mint]`. */
  phoenixVault(market: PublicKey, mint: PublicKey): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), market.toBuffer(), mint.toBuffer()],
      PHOENIX_PROGRAM_ID,
    );
  }
  depositReceiptPda(trader: PublicKey): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [DEPOSIT_RECEIPT_SEED, trader.toBuffer()],
      PERP_ROUTER_PROGRAM_ID,
    );
  }
  withdrawalReceiptPda(trader: PublicKey): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [WITHDRAWAL_RECEIPT_SEED, trader.toBuffer()],
      PERP_ROUTER_PROGRAM_ID,
    );
  }
  /** Per-market orderbook PDA — `[b"orderbook", perp_market]`. */
  orderbookPda(perpMarket: PublicKey): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [ORDERBOOK_SEED, perpMarket.toBuffer()],
      PERP_ROUTER_PROGRAM_ID,
    );
  }

  // ─── Delegation auxiliary PDAs ─────────────────────────────────────────
  // Buffer is owned by the original program (perp_router). Record and
  // metadata are owned by the delegation program. Seeds match the
  // MagicBlock conventions used by phoenix-v1's existing tests.

  findDelegationBuffer(pda: PublicKey): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("buffer"), pda.toBuffer()],
      PERP_ROUTER_PROGRAM_ID,
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

  /**
   * Convenience: one call returns all three delegation aux PDAs for a
   * given target.
   */
  delegationAuxFor(pda: PublicKey): {
    buffer: PublicKey;
    record: PublicKey;
    metadata: PublicKey;
  } {
    const [buffer] = this.findDelegationBuffer(pda);
    const [record] = this.findDelegationRecord(pda);
    const [metadata] = this.findDelegationMetadata(pda);
    return { buffer, record, metadata };
  }

  // ─── Helpers ──────────────────────────────────────────────────────────

  private async assertTxOk(
    conn: Connection,
    sig: string,
    label: string,
  ): Promise<void> {
    // Wait until landed.
    await conn.confirmTransaction(sig, "confirmed");
    // Fetch and verify meta.err is null (confirmation alone doesn't
    // distinguish success from runtime failure).
    const tx = await conn.getTransaction(sig, {
      commitment: "confirmed",
      maxSupportedTransactionVersion: 0,
    });
    if (!tx) {
      throw new Error(`${label}: tx not retrievable (sig: ${sig})`);
    }
    if (tx.meta?.err) {
      const logs = (tx.meta.logMessages || []).join("\n  ");
      throw new Error(
        `${label} FAILED on-chain (sig: ${sig})\n  err: ${JSON.stringify(
          tx.meta.err,
        )}\n  logs:\n  ${logs}`,
      );
    }
  }

  private async sendBase(
    ixs: TransactionInstruction[],
    signers: Signer[] = [],
    label = "base tx",
  ): Promise<string> {
    const tx = new Transaction().add(...ixs);
    tx.feePayer = this.payer.publicKey;
    tx.recentBlockhash = (
      await this.baseConnection.getLatestBlockhash("confirmed")
    ).blockhash;
    tx.sign(this.payer, ...signers);
    const sig = await this.baseConnection.sendRawTransaction(tx.serialize(), {
      skipPreflight: true,
    });
    await this.assertTxOk(this.baseConnection, sig, label);
    return sig;
  }

  private async sendEr(
    ixs: TransactionInstruction[],
    signers: Signer[] = [],
    label = "ER tx",
  ): Promise<string> {
    const tx = new Transaction().add(...ixs);
    tx.feePayer = this.payer.publicKey;
    tx.recentBlockhash = (
      await this.erConnection.getLatestBlockhash("confirmed")
    ).blockhash;
    tx.sign(this.payer, ...signers);
    const sig = await this.erConnection.sendRawTransaction(tx.serialize(), {
      skipPreflight: true,
    });
    await this.assertTxOk(this.erConnection, sig, label);
    return sig;
  }

  // ─── Instructions ─────────────────────────────────────────────────────

  async initializeGlobalState(): Promise<string> {
    const [globalState] = this.globalStatePda();
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: this.admin.publicKey, isSigner: true, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
        { pubkey: globalState, isSigner: false, isWritable: true },
      ],
      data: Buffer.from([TAG.InitializeGlobalState]),
    });
    return this.sendBase([ix]);
  }

  async initializeMarket(
    phoenixMarket: PublicKey,
    baseMint: PublicKey,
    quoteMint: PublicKey,
    oracle: PublicKey,
    maxBpsPerSlot = 50,
    maxLeverageBps = 100_000, // 10x
  ): Promise<string> {
    const [perpMarket] = this.perpMarketPda(phoenixMarket);
    const [phoenixBaseVault] = this.phoenixVault(phoenixMarket, baseMint);
    const [phoenixQuoteVault] = this.phoenixVault(phoenixMarket, quoteMint);
    const data = Buffer.concat([
      u8(TAG.InitializeMarket),
      u32(maxBpsPerSlot),
      u32(maxLeverageBps),
    ]);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: this.admin.publicKey, isSigner: true, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
        { pubkey: perpMarket, isSigner: false, isWritable: true },
        { pubkey: phoenixMarket, isSigner: false, isWritable: false },
        { pubkey: baseMint, isSigner: false, isWritable: false },
        { pubkey: quoteMint, isSigner: false, isWritable: false },
        { pubkey: phoenixBaseVault, isSigner: false, isWritable: false },
        { pubkey: phoenixQuoteVault, isSigner: false, isWritable: false },
        { pubkey: oracle, isSigner: false, isWritable: false },
      ],
      data,
    });
    return this.sendBase([ix]);
  }

  /**
   * Allocates the orderbook PDA (`[b"orderbook", perp_market]`) and runs
   * `FIFOMarket::initialize_with_params` + `set_fee` on it. The PDA layout
   * (9,104 bytes for the 32×32×32 FIFOMarket shape) fits under the 10 KB
   * single-CPI alloc cap, so this is one ix — the program itself signs the
   * `create_account` with the orderbook seeds. Returns the derived PDA.
   */
  async initializeOrderbook(
    perpMarket: PublicKey,
    tickSizeInQuoteLotsPerBaseUnit: BN,
    baseLotsPerBaseUnit: BN,
    takerFeeBps: number,
  ): Promise<{ orderbook: PublicKey; sig: string }> {
    const [orderbook] = this.orderbookPda(perpMarket);
    const data = Buffer.concat([
      u8(TAG.InitializeOrderbook),
      u64(tickSizeInQuoteLotsPerBaseUnit),
      u64(baseLotsPerBaseUnit),
      u16(takerFeeBps),
    ]);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: this.admin.publicKey, isSigner: true, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
        { pubkey: perpMarket, isSigner: false, isWritable: true },
        { pubkey: orderbook, isSigner: false, isWritable: true },
      ],
      data,
    });
    const sig = await this.sendBase([ix], [], "initializeOrderbook");
    return { orderbook, sig };
  }

  /**
   * Register the trader into the orderbook's inline seat RBTree. Runs on
   * ER (orderbook is delegated). Idempotent — `get_or_register_trader`
   * is a no-op for existing seats.
   */
  async claimSeat(
    trader: Keypair,
    perpMarket: PublicKey,
  ): Promise<string> {
    const [orderbook] = this.orderbookPda(perpMarket);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: trader.publicKey, isSigner: true, isWritable: false },
        { pubkey: perpMarket, isSigner: false, isWritable: false },
        { pubkey: orderbook, isSigner: false, isWritable: true },
      ],
      data: Buffer.from([TAG.ClaimSeat]),
    });
    return this.sendEr([ix], trader === this.payer ? [] : [trader], "claimSeat");
  }

  /**
   * Push a limit order into the in-tree Phoenix matching engine on the
   * delegated orderbook. `side`: 0=Bid, 1=Ask. Runs on ER.
   */
  async placeOrderPerp(
    trader: Keypair,
    perpMarket: PublicKey,
    side: 0 | 1,
    priceInTicks: BN,
    numBaseLots: BN,
    clientOrderId: BN = new BN(0),
  ): Promise<string> {
    const [orderbook] = this.orderbookPda(perpMarket);
    const data = Buffer.concat([
      u8(TAG.PlaceOrderPerp),
      u8(side),
      u64(priceInTicks),
      u64(numBaseLots),
      u128(clientOrderId),
    ]);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: trader.publicKey, isSigner: true, isWritable: false },
        { pubkey: perpMarket, isSigner: false, isWritable: false },
        { pubkey: orderbook, isSigner: false, isWritable: true },
      ],
      data,
    });
    return this.sendEr([ix], trader === this.payer ? [] : [trader], "placeOrderPerp");
  }

  /**
   * Hand the orderbook PDA to the MagicBlock delegation program so the
   * in-tree matching engine can mutate it on the Ephemeral Rollup.
   * Mirrors `delegateAccount` but with the orderbook-specific account
   * list (perp_market is needed to derive the bump server-side).
   */
  async delegateOrderbook(
    perpMarket: PublicKey,
    validator: PublicKey | null = null,
  ): Promise<string> {
    const [orderbook] = this.orderbookPda(perpMarket);
    const aux = this.delegationAuxFor(orderbook);
    const data = Buffer.concat([
      u8(TAG.DelegateOrderbook),
      optionPubkey(validator),
    ]);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: this.admin.publicKey, isSigner: true, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
        { pubkey: perpMarket, isSigner: false, isWritable: false },
        { pubkey: orderbook, isSigner: false, isWritable: true },
        { pubkey: PERP_ROUTER_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: aux.buffer, isSigner: false, isWritable: true },
        { pubkey: aux.record, isSigner: false, isWritable: true },
        { pubkey: aux.metadata, isSigner: false, isWritable: true },
        { pubkey: DELEGATION_PROGRAM_ID, isSigner: false, isWritable: false },
      ],
      data,
    });
    return this.sendBase([ix], [], "delegateOrderbook");
  }

  async initializeTrader(owner: Keypair): Promise<string> {
    const [traderAccount] = this.traderAccountPda(owner.publicKey);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: owner.publicKey, isSigner: true, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
        { pubkey: traderAccount, isSigner: false, isWritable: true },
      ],
      data: Buffer.from([TAG.InitializeTrader]),
    });
    return this.sendBase([ix], owner === this.payer ? [] : [owner]);
  }

  /**
   * Generic delegation builder. Caller supplies the variant tag and the
   * delegation auxiliary accounts (buffer/record/metadata). Returns sig.
   */
  async delegateAccount(
    tag: number,
    pda: PublicKey,
    signer: Keypair,
    delegationBuffer: PublicKey,
    delegationRecord: PublicKey,
    delegationMetadata: PublicKey,
    validator: PublicKey | null = null,
  ): Promise<string> {
    const data = Buffer.concat([u8(tag), optionPubkey(validator)]);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: signer.publicKey, isSigner: true, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
        { pubkey: pda, isSigner: false, isWritable: true },
        { pubkey: PERP_ROUTER_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: delegationBuffer, isSigner: false, isWritable: true },
        { pubkey: delegationRecord, isSigner: false, isWritable: true },
        { pubkey: delegationMetadata, isSigner: false, isWritable: true },
        { pubkey: DELEGATION_PROGRAM_ID, isSigner: false, isWritable: false },
      ],
      data,
    });
    return this.sendBase([ix], signer === this.payer ? [] : [signer]);
  }

  async requestCollateralDeposit(
    trader: Keypair,
    quoteMint: PublicKey,
    perpMarket: PublicKey,
    traderTokenAccount: PublicKey,
    amount: BN,
    delegationBuffer: PublicKey,
    delegationRecord: PublicKey,
    delegationMetadata: PublicKey,
  ): Promise<string> {
    const vault = this.vaultAta(quoteMint);
    const [receipt] = this.depositReceiptPda(trader.publicKey);
    const [traderAccount] = this.traderAccountPda(trader.publicKey);
    const [globalState] = this.globalStatePda();
    const data = Buffer.concat([u8(TAG.RequestCollateralDeposit), u64(amount)]);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: trader.publicKey, isSigner: true, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
        { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: quoteMint, isSigner: false, isWritable: false },
        { pubkey: perpMarket, isSigner: false, isWritable: false },
        { pubkey: traderTokenAccount, isSigner: false, isWritable: true },
        { pubkey: vault, isSigner: false, isWritable: true },
        { pubkey: receipt, isSigner: false, isWritable: true },
        { pubkey: PERP_ROUTER_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: delegationBuffer, isSigner: false, isWritable: true },
        { pubkey: delegationRecord, isSigner: false, isWritable: true },
        { pubkey: delegationMetadata, isSigner: false, isWritable: true },
        { pubkey: DELEGATION_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: MAGIC_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: MAGIC_CONTEXT_ID, isSigner: false, isWritable: true },
        // forwarded to stage 2 (ProcessCollateralDepositEr):
        { pubkey: traderAccount, isSigner: false, isWritable: true },
        { pubkey: globalState, isSigner: false, isWritable: true },
      ],
      data,
    });
    return this.sendBase([ix], trader === this.payer ? [] : [trader], "requestCollateralDeposit");
  }

  /** Open a long position via Phoenix Swap (Bid). v1.1: longs only. */
  async openPosition(
    trader: Keypair,
    phoenixMarket: PublicKey,
    baseMint: PublicKey,
    quoteMint: PublicKey,
    margin: BN,
    numQuoteLotsToSpend: BN,
    minBaseLotsToReceive: BN,
    markPrice: BN,
    clientOrderId: BN = new BN(0),
  ): Promise<string> {
    const [traderAccount] = this.traderAccountPda(trader.publicKey);
    const [globalState] = this.globalStatePda();
    const [perpMarket] = this.perpMarketPda(phoenixMarket);
    const [authority] = this.perpAuthorityPda();
    const collateralVault = this.vaultAta(quoteMint);
    const perpBaseVault = this.vaultAta(baseMint);
    const [logAuthority] = this.phoenixLogAuthority();
    const [phoenixBaseVault] = this.phoenixVault(phoenixMarket, baseMint);
    const [phoenixQuoteVault] = this.phoenixVault(phoenixMarket, quoteMint);
    const data = Buffer.concat([
      u8(TAG.OpenPosition),
      u64(margin),
      u64(numQuoteLotsToSpend),
      u64(minBaseLotsToReceive),
      u64(markPrice),
      u128(clientOrderId),
    ]);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: trader.publicKey, isSigner: true, isWritable: true },
        { pubkey: traderAccount, isSigner: false, isWritable: true },
        { pubkey: globalState, isSigner: false, isWritable: true },
        { pubkey: perpMarket, isSigner: false, isWritable: true },
        { pubkey: authority, isSigner: false, isWritable: false },
        { pubkey: collateralVault, isSigner: false, isWritable: true },
        { pubkey: perpBaseVault, isSigner: false, isWritable: true },
        { pubkey: PHOENIX_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: logAuthority, isSigner: false, isWritable: false },
        { pubkey: phoenixMarket, isSigner: false, isWritable: true },
        { pubkey: phoenixBaseVault, isSigner: false, isWritable: true },
        { pubkey: phoenixQuoteVault, isSigner: false, isWritable: true },
        { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
      ],
      data,
    });
    return this.sendEr([ix], trader === this.payer ? [] : [trader]);
  }

  /** Close a long position via Phoenix Swap (Ask). v1.1: longs only. */
  async closePosition(
    trader: Keypair,
    phoenixMarket: PublicKey,
    baseMint: PublicKey,
    quoteMint: PublicKey,
    minQuoteLotsToReceive: BN,
    markPrice: BN,
    clientOrderId: BN = new BN(0),
  ): Promise<string> {
    const [traderAccount] = this.traderAccountPda(trader.publicKey);
    const [globalState] = this.globalStatePda();
    const [perpMarket] = this.perpMarketPda(phoenixMarket);
    const [authority] = this.perpAuthorityPda();
    const collateralVault = this.vaultAta(quoteMint);
    const perpBaseVault = this.vaultAta(baseMint);
    const [logAuthority] = this.phoenixLogAuthority();
    const [phoenixBaseVault] = this.phoenixVault(phoenixMarket, baseMint);
    const [phoenixQuoteVault] = this.phoenixVault(phoenixMarket, quoteMint);
    const data = Buffer.concat([
      u8(TAG.ClosePosition),
      u64(minQuoteLotsToReceive),
      u64(markPrice),
      u128(clientOrderId),
    ]);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: trader.publicKey, isSigner: true, isWritable: true },
        { pubkey: traderAccount, isSigner: false, isWritable: true },
        { pubkey: globalState, isSigner: false, isWritable: true },
        { pubkey: perpMarket, isSigner: false, isWritable: true },
        { pubkey: authority, isSigner: false, isWritable: false },
        { pubkey: collateralVault, isSigner: false, isWritable: true },
        { pubkey: perpBaseVault, isSigner: false, isWritable: true },
        { pubkey: PHOENIX_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: logAuthority, isSigner: false, isWritable: false },
        { pubkey: phoenixMarket, isSigner: false, isWritable: true },
        { pubkey: phoenixBaseVault, isSigner: false, isWritable: true },
        { pubkey: phoenixQuoteVault, isSigner: false, isWritable: true },
        { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
      ],
      data,
    });
    return this.sendEr([ix], trader === this.payer ? [] : [trader]);
  }

  async maturePnl(trader: Keypair): Promise<string> {
    const [traderAccount] = this.traderAccountPda(trader.publicKey);
    const [globalState] = this.globalStatePda();
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        // Caller signs permissionlessly; never mutated.
        { pubkey: trader.publicKey, isSigner: true, isWritable: false },
        { pubkey: traderAccount, isSigner: false, isWritable: true },
        { pubkey: globalState, isSigner: false, isWritable: true },
      ],
      data: Buffer.from([TAG.MaturePnl]),
    });
    return this.sendEr([ix], trader === this.payer ? [] : [trader], "maturePnl");
  }

  async requestCollateralWithdrawal(
    trader: Keypair,
    quoteMint: PublicKey,
    perpMarket: PublicKey,
    traderTokenAccount: PublicKey,
    amount: BN,
    delegationBuffer: PublicKey,
    delegationRecord: PublicKey,
    delegationMetadata: PublicKey,
  ): Promise<string> {
    const vault = this.vaultAta(quoteMint);
    const [receipt] = this.withdrawalReceiptPda(trader.publicKey);
    const [traderAccount] = this.traderAccountPda(trader.publicKey);
    const [globalState] = this.globalStatePda();
    const data = Buffer.concat([
      u8(TAG.RequestCollateralWithdrawal),
      u64(amount),
    ]);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: trader.publicKey, isSigner: true, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
        { pubkey: receipt, isSigner: false, isWritable: true },
        { pubkey: quoteMint, isSigner: false, isWritable: false },
        { pubkey: perpMarket, isSigner: false, isWritable: false },
        { pubkey: traderTokenAccount, isSigner: false, isWritable: true },
        { pubkey: vault, isSigner: false, isWritable: true },
        { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: PERP_ROUTER_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: delegationBuffer, isSigner: false, isWritable: true },
        { pubkey: delegationRecord, isSigner: false, isWritable: true },
        { pubkey: delegationMetadata, isSigner: false, isWritable: true },
        { pubkey: DELEGATION_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: MAGIC_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: MAGIC_CONTEXT_ID, isSigner: false, isWritable: true },
        // forwarded to stage 2:
        { pubkey: traderAccount, isSigner: false, isWritable: true },
        { pubkey: globalState, isSigner: false, isWritable: true },
      ],
      data,
    });
    return this.sendBase([ix], trader === this.payer ? [] : [trader], "requestCollateralWithdrawal");
  }

  /**
   * Manually invoke ProcessCollateralDepositEr on the ER. Use when the
   * auto-fire post-delegation chain isn't dispatched (e.g. program not
   * registered with MagicBlock validator). Account list mirrors the
   * scheduled action.
   */
  async processCollateralDepositEr(trader: Keypair): Promise<string> {
    const [receipt] = this.depositReceiptPda(trader.publicKey);
    const [traderAccount] = this.traderAccountPda(trader.publicKey);
    const [globalState] = this.globalStatePda();
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        // Trader signs as escrow authority; never mutated. Marking
        // writable triggers InvalidWritableAccount on ER (wallet isn't
        // delegated).
        { pubkey: trader.publicKey, isSigner: true, isWritable: false },
        { pubkey: receipt, isSigner: false, isWritable: true },
        { pubkey: traderAccount, isSigner: false, isWritable: true },
        { pubkey: globalState, isSigner: false, isWritable: true },
        { pubkey: MAGIC_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: MAGIC_CONTEXT_ID, isSigner: false, isWritable: true },
      ],
      data: Buffer.from([TAG.ProcessCollateralDepositEr]),
    });
    return this.sendEr([ix], trader === this.payer ? [] : [trader], "processCollateralDepositEr");
  }

  /**
   * Manual stage 3 close on base layer (auto-fired in normal flow).
   */
  async closeCollateralDepositReceipt(trader: Keypair): Promise<string> {
    const [receipt] = this.depositReceiptPda(trader.publicKey);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: trader.publicKey, isSigner: false, isWritable: true },
        { pubkey: receipt, isSigner: false, isWritable: true },
      ],
      data: Buffer.from([TAG.CloseCollateralDepositReceipt]),
    });
    // Trader does NOT sign — the post-undelegate ix expects no user signer
    // (validator signs in production). For manual mode we just send as payer.
    return this.sendBase([ix], [], "closeCollateralDepositReceipt");
  }

  async processCollateralWithdrawalEr(
    trader: Keypair,
    quoteMint: PublicKey,
    traderTokenAccount: PublicKey,
  ): Promise<string> {
    const [receipt] = this.withdrawalReceiptPda(trader.publicKey);
    const [traderAccount] = this.traderAccountPda(trader.publicKey);
    const [globalState] = this.globalStatePda();
    const vault = this.vaultAta(quoteMint);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: trader.publicKey, isSigner: true, isWritable: true },
        { pubkey: receipt, isSigner: false, isWritable: true },
        { pubkey: MAGIC_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: MAGIC_CONTEXT_ID, isSigner: false, isWritable: true },
        { pubkey: traderTokenAccount, isSigner: false, isWritable: true },
        { pubkey: vault, isSigner: false, isWritable: true },
        { pubkey: quoteMint, isSigner: false, isWritable: false },
        { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: traderAccount, isSigner: false, isWritable: true },
        { pubkey: globalState, isSigner: false, isWritable: true },
      ],
      data: Buffer.from([TAG.ProcessCollateralWithdrawalEr]),
    });
    return this.sendEr([ix], trader === this.payer ? [] : [trader], "processCollateralWithdrawalEr");
  }

  async executeCollateralWithdrawalBaseChain(
    trader: PublicKey,
    quoteMint: PublicKey,
    traderTokenAccount: PublicKey,
  ): Promise<string> {
    const [receipt] = this.withdrawalReceiptPda(trader);
    const vault = this.vaultAta(quoteMint);
    const [authority] = this.perpAuthorityPda();
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: trader, isSigner: false, isWritable: true },
        { pubkey: receipt, isSigner: false, isWritable: true },
        { pubkey: traderTokenAccount, isSigner: false, isWritable: true },
        { pubkey: vault, isSigner: false, isWritable: true },
        { pubkey: quoteMint, isSigner: false, isWritable: false },
        { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: authority, isSigner: false, isWritable: false },
      ],
      data: Buffer.from([TAG.ExecuteCollateralWithdrawalBaseChain]),
    });
    return this.sendBase([ix], [], "executeCollateralWithdrawalBaseChain");
  }

  /**
   * Single-tx deposit (no delegation, no Magic Action chain).
   * Used for local testing + as a fallback when MagicBlock isn't
   * dispatching auto-fires.
   */
  async directDeposit(
    trader: Keypair,
    quoteMint: PublicKey,
    traderTokenAccount: PublicKey,
    amount: BN,
  ): Promise<string> {
    const vault = this.vaultAta(quoteMint);
    const [traderAccount] = this.traderAccountPda(trader.publicKey);
    const [globalState] = this.globalStatePda();
    const data = Buffer.concat([u8(TAG.DirectDeposit), u64(amount)]);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: trader.publicKey, isSigner: true, isWritable: true },
        { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: quoteMint, isSigner: false, isWritable: false },
        { pubkey: traderTokenAccount, isSigner: false, isWritable: true },
        { pubkey: vault, isSigner: false, isWritable: true },
        { pubkey: traderAccount, isSigner: false, isWritable: true },
        { pubkey: globalState, isSigner: false, isWritable: true },
      ],
      data,
    });
    return this.sendBase([ix], trader === this.payer ? [] : [trader], "directDeposit");
  }

  /** Single-tx withdraw, haircut applied internally. */
  async directWithdraw(
    trader: Keypair,
    quoteMint: PublicKey,
    traderTokenAccount: PublicKey,
    amount: BN,
  ): Promise<string> {
    const vault = this.vaultAta(quoteMint);
    const [traderAccount] = this.traderAccountPda(trader.publicKey);
    const [globalState] = this.globalStatePda();
    const [authority] = this.perpAuthorityPda();
    const data = Buffer.concat([u8(TAG.DirectWithdraw), u64(amount)]);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: trader.publicKey, isSigner: true, isWritable: true },
        { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: quoteMint, isSigner: false, isWritable: false },
        { pubkey: traderTokenAccount, isSigner: false, isWritable: true },
        { pubkey: vault, isSigner: false, isWritable: true },
        { pubkey: traderAccount, isSigner: false, isWritable: true },
        { pubkey: globalState, isSigner: false, isWritable: true },
        { pubkey: authority, isSigner: false, isWritable: false },
      ],
      data,
    });
    return this.sendBase([ix], trader === this.payer ? [] : [trader], "directWithdraw");
  }

  async undelegateTraderAccount(trader: Keypair): Promise<string> {
    const [traderAccount] = this.traderAccountPda(trader.publicKey);
    return this.undelegateAccount(trader, traderAccount);
  }

  /**
   * Generic undelegate — UndelegateTraderAccount's processor just does
   * `commit_and_undelegate_accounts(payer, [target], ...)` and isn't
   * actually scoped to a TraderAccount, so we reuse it for GlobalState /
   * PerpMarket / receipts.
   */
  async undelegateAccount(signer: Keypair, target: PublicKey): Promise<string> {
    // Signer (= the commit payer) must be writable — commit_and_undelegate
    // creates state buffer accounts and pays rent from this payer.
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: signer.publicKey, isSigner: true, isWritable: true },
        { pubkey: target, isSigner: false, isWritable: true },
        { pubkey: MAGIC_PROGRAM_ID, isSigner: false, isWritable: false },
        { pubkey: MAGIC_CONTEXT_ID, isSigner: false, isWritable: true },
      ],
      data: Buffer.from([TAG.UndelegateTraderAccount]),
    });
    return this.sendEr([ix], signer === this.payer ? [] : [signer], "undelegate");
  }

  /** DirectOpenPosition — runs on ER when trader_account + perp_market are
   * delegated. GlobalState is READ-ONLY in this ix, so it can stay on base
   * (ER replicates it as a readonly clone) — that means we never need to
   * delegate GlobalState, which keeps the test re-runnable. */
  async directOpenPosition(
    trader: Keypair,
    phoenixMarket: PublicKey,
    size: BN,
    markPrice: BN,
    margin: BN,
    onEr: boolean,
  ): Promise<string> {
    const [traderAccount] = this.traderAccountPda(trader.publicKey);
    const [globalState] = this.globalStatePda();
    const [perpMarket] = this.perpMarketPda(phoenixMarket);
    const data = Buffer.concat([
      u8(TAG.DirectOpenPosition),
      i64(size),
      u64(markPrice),
      u64(margin),
    ]);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        // Trader signs but is never mutated by the program. On ER the
        // wallet isn't delegated, so writable=true is rejected as
        // InvalidWritableAccount.
        { pubkey: trader.publicKey, isSigner: true, isWritable: false },
        { pubkey: traderAccount, isSigner: false, isWritable: true },
        // GlobalState is read-only in this ix (recovery_state + a-coeff).
        // Keeping it non-writable lets it stay un-delegated on base while
        // the ER uses a replicated readonly clone.
        { pubkey: globalState, isSigner: false, isWritable: false },
        { pubkey: perpMarket, isSigner: false, isWritable: true },
      ],
      data,
    });
    const signers = trader === this.payer ? [] : [trader];
    return onEr
      ? this.sendEr([ix], signers, "directOpenPosition")
      : this.sendBase([ix], signers, "directOpenPosition");
  }

  async directClosePosition(
    trader: Keypair,
    phoenixMarket: PublicKey,
    markPrice: BN,
    onEr: boolean,
  ): Promise<string> {
    const [traderAccount] = this.traderAccountPda(trader.publicKey);
    const [globalState] = this.globalStatePda();
    const [perpMarket] = this.perpMarketPda(phoenixMarket);
    const data = Buffer.concat([u8(TAG.DirectClosePosition), u64(markPrice)]);
    const ix = new TransactionInstruction({
      programId: PERP_ROUTER_PROGRAM_ID,
      keys: [
        { pubkey: trader.publicKey, isSigner: true, isWritable: false },
        { pubkey: traderAccount, isSigner: false, isWritable: true },
        { pubkey: globalState, isSigner: false, isWritable: false },
        { pubkey: perpMarket, isSigner: false, isWritable: true },
      ],
      data,
    });
    const signers = trader === this.payer ? [] : [trader];
    return onEr
      ? this.sendEr([ix], signers, "directClosePosition")
      : this.sendBase([ix], signers, "directClosePosition");
  }

  // ─── Util: spin up an SPL mint + ATA for a trader ─────────────────────

  async createQuoteMint(decimals = 6): Promise<{ mint: Keypair; sig: string }> {
    const mint = Keypair.generate();
    const lamports = await getMinimumBalanceForRentExemptMint(
      this.baseConnection,
    );
    const ixs = [
      SystemProgram.createAccount({
        fromPubkey: this.payer.publicKey,
        newAccountPubkey: mint.publicKey,
        space: MINT_SIZE,
        lamports,
        programId: TOKEN_PROGRAM_ID,
      }),
      createInitializeMint2Instruction(
        mint.publicKey,
        decimals,
        this.payer.publicKey,
        null,
      ),
    ];
    const sig = await this.sendBase(ixs, [mint]);
    return { mint, sig };
  }

  async fundTrader(
    trader: PublicKey,
    mint: PublicKey,
    amount: BN,
  ): Promise<{ ata: PublicKey; sig: string }> {
    const ata = getAssociatedTokenAddressSync(mint, trader);
    const ixs = [
      createAssociatedTokenAccountIdempotentInstruction(
        this.payer.publicKey,
        ata,
        trader,
        mint,
      ),
      createMintToInstruction(
        mint,
        ata,
        this.payer.publicKey,
        BigInt(amount.toString()),
      ),
    ];
    const sig = await this.sendBase(ixs);
    return { ata, sig };
  }

  async getTokenBalance(account: PublicKey): Promise<BN> {
    const acc = await getAccount(this.baseConnection, account);
    return new BN(acc.amount.toString());
  }

  /**
   * Create the perp_authority-owned ATA for `mint` if it doesn't exist.
   * Required before deposit — the vault is the SPL destination.
   */
  async ensureVaultAta(mint: PublicKey): Promise<{ ata: PublicKey; sig?: string }> {
    const ata = this.vaultAta(mint);
    const existing = await this.baseConnection.getAccountInfo(ata);
    if (existing) return { ata };
    const [authority] = this.perpAuthorityPda();
    const ix = createAssociatedTokenAccountIdempotentInstruction(
      this.payer.publicKey,
      ata,
      authority,
      mint,
    );
    const sig = await this.sendBase([ix], [], "ensureVaultAta");
    return { ata, sig };
  }

  // ─── State readers (use base OR er depending on delegation) ─────────

  /**
   * Read a delegated or base account from whichever layer holds the live
   * copy. Tries ER first (delegated state), falls back to base.
   */
  private async getLiveAccount(
    pda: PublicKey,
  ): Promise<{ data: Buffer; owner: PublicKey } | null> {
    const er = await this.erConnection.getAccountInfo(pda).catch(() => null);
    if (er && er.data.length > 0) return { data: er.data, owner: er.owner };
    const base = await this.baseConnection.getAccountInfo(pda);
    if (base) return { data: base.data, owner: base.owner };
    return null;
  }

  /** Parse TraderAccount.collateral (u64 LE) at field offset 32 (after owner). */
  async getTraderCollateral(owner: PublicKey): Promise<BN> {
    const [pda] = this.traderAccountPda(owner);
    const a = await this.getLiveAccount(pda);
    if (!a) return new BN(0);
    // Pubkey(32) → collateral u64
    return new BN(a.data.subarray(32, 40), "le");
  }

  /** Parse TraderAccount.pnl_matured (u64 LE) at field offset 40. */
  async getTraderPnlMatured(owner: PublicKey): Promise<BN> {
    const [pda] = this.traderAccountPda(owner);
    const a = await this.getLiveAccount(pda);
    if (!a) return new BN(0);
    // Pubkey(32) + collateral(8) → pnl_matured u64
    return new BN(a.data.subarray(40, 48), "le");
  }

  /**
   * GlobalState layout (matches perp_router/src/state/global.rs):
   *   offset  0  A  [u8;16]
   *          16  K  [u8;16]
   *          32  F  [u8;16]
   *          48  B  [u8;16]
   *          64  epoch          u64
   *          72  recovery_state u8
   *          73  _pad0          [u8;7]
   *          80  v_total_pool_value u64
   *          88  c_total_collateral u64
   *          96  i_insurance_reserve u64
   *         104  total_matured_pnl u64
   */
  async getRecoveryState(): Promise<number> {
    const [pda] = this.globalStatePda();
    const a = await this.getLiveAccount(pda);
    if (!a) return 0;
    return a.data[72];
  }

  async getEpoch(): Promise<BN> {
    const [pda] = this.globalStatePda();
    const a = await this.getLiveAccount(pda);
    if (!a) return new BN(0);
    return new BN(a.data.subarray(64, 72), "le");
  }

  async getTotalCollateral(): Promise<BN> {
    const [pda] = this.globalStatePda();
    const a = await this.getLiveAccount(pda);
    if (!a) return new BN(0);
    return new BN(a.data.subarray(88, 96), "le");
  }

  async getTotalMaturedPnl(): Promise<BN> {
    const [pda] = this.globalStatePda();
    const a = await this.getLiveAccount(pda);
    if (!a) return new BN(0);
    return new BN(a.data.subarray(104, 112), "le");
  }
}
