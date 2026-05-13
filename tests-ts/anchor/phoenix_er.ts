// Phoenix + MagicBlock Ephemeral Rollup — devnet E2E.
//
// Run with:
//   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com \
//   ANCHOR_WALLET=~/.config/solana/id.json \
//   PHOENIX_PROGRAM_ID=<deployed_program_id> \
//   yarn test
//
// Every step prints its confirmed transaction signature so you can
// audit the run on https://explorer.solana.com/?cluster=devnet.

import * as anchor from "@coral-xyz/anchor";
import { expect } from "chai";
import BN from "bn.js";
import { PublicKey } from "@solana/web3.js";
import { GetCommitmentSignature } from "@magicblock-labs/ephemeral-rollups-sdk";
import {
  PHOENIX_PROGRAM_ID,
  DELEGATION_PROGRAM_ID,
  TestClient,
} from "./test_client";

describe("phoenix-er", () => {
  const tc = new TestClient();
  tc.printErrors = true;

  let preWithdrawBase: bigint;
  let preWithdrawQuote: bigint;

  before(() => {
    console.log("Program ID:    ", PHOENIX_PROGRAM_ID.toBase58());
    console.log("Admin:         ", tc.admin.publicKey.toBase58());
    console.log("Base RPC:      ", tc.connection.rpcEndpoint);
    console.log("Router RPC:    ", tc.routerConnection.rpcEndpoint);
  });

  it("setup — create mints + fund trader ATAs", async () => {
    const { txSig: mintsSig } = await tc.createMints();
    console.log("  create mints tx:    ", mintsSig);
    console.log("    baseMint:          ", tc.baseMint.toBase58());
    console.log("    quoteMint:         ", tc.quoteMint.toBase58());

    const fundSig = await tc.fundTraderAtas(
      BigInt("1000000000000"), // 1000 base units
      BigInt("100000000000"),  // 100k quote units
    );
    console.log("  fund ATAs tx:       ", fundSig);
    console.log("    traderBaseAta:    ", tc.traderBaseAta.toBase58());
    console.log("    traderQuoteAta:   ", tc.traderQuoteAta.toBase58());
  }).timeout(120_000);

  it("initializes the market as a PDA", async () => {
    const sig = await tc.initializeMarket({
      // Tiny config so the PDA fits in MAX_PERMITTED_DATA_INCREASE (10240).
      bidsSize: new BN(8),
      asksSize: new BN(8),
      numSeats: new BN(4),
      numQuoteLotsPerQuoteUnit: new BN(1000),
      tickSizeInQuoteLotsPerBaseUnit: new BN(1000),
      numBaseLotsPerBaseUnit: new BN(1000),
      takerFeeBps: 5,
      rawBaseUnitsPerBaseUnit: null,
    });
    console.log("  initialize tx:      ", sig);
    console.log("    market PDA:        ", tc.market.toBase58());

    const info = await tc.connection.getAccountInfo(tc.market);
    expect(info, "market exists").to.not.be.null;
    expect(info!.owner.equals(PHOENIX_PROGRAM_ID)).to.be.true;
  }).timeout(60_000);

  it("requests a seat", async () => {
    const sig = await tc.requestSeat();
    console.log("  request seat tx:    ", sig);
  }).timeout(60_000);

  it("approves the seat (market authority)", async () => {
    const sig = await tc.changeSeatStatus(tc.admin.publicKey, 1 /* Approved */);
    console.log("  approve seat tx:    ", sig);
  }).timeout(60_000);

  it("deposits funds on base layer (pre-delegation)", async () => {
    const sig = await tc.depositFunds(new BN(100), new BN(1000));
    console.log("  deposit funds tx:   ", sig);
  }).timeout(60_000);

  it("activates the market (status: PostOnly → Active) so IOC/Swap is allowed", async () => {
    // MarketStatus::Active = 1. Required for SwapWithFreeFunds / IOC.
    const sig = await tc.changeMarketStatus(1);
    console.log("  activate market tx: ", sig);
  }).timeout(60_000);

  it("delegates the market to the ER", async () => {
    const sig = await tc.delegateMarket(null);
    console.log("  delegate market tx: ", sig);

    const info = await tc.connection.getAccountInfo(tc.market);
    expect(info!.owner.equals(DELEGATION_PROGRAM_ID), "market owner = delegation program")
      .to.be.true;
  }).timeout(120_000);

  it("delegates the trader seat to the ER", async () => {
    const sig = await tc.delegateSeat(tc.admin.publicKey, null);
    console.log("  delegate seat tx:   ", sig);

    const [seat] = tc.findSeatAddress(tc.market, tc.admin.publicKey);
    const info = await tc.connection.getAccountInfo(seat);
    expect(info!.owner.equals(DELEGATION_PROGRAM_ID), "seat owner = delegation program")
      .to.be.true;
  }).timeout(120_000);

  it("ER trading smoke test — CancelAllOrdersWithFreeFunds on delegated market", async () => {
    const sig = await tc.cancelAllOrdersOnEr();
    console.log("  ER cancelAll tx:    ", sig);
  }).timeout(60_000);

  // =====================================================================
  // ER trading: place / match / cancel on the delegated market
  // =====================================================================

  it("places a resting ASK on the ER (PostOnly @ 100 ticks × 5 base lots)", async () => {
    const pre = await tc.readTraderStateFromMarket(tc.routerConnection);
    console.log("  pre-place trader state on ER:", pre);

    const sig = await tc.placeLimitOrderOnEr({
      type: "post_only",
      side: "ask",
      priceInTicks: 100n,
      numBaseLots: 5n,
      clientOrderId: 1n,
    });
    console.log("  place ASK tx:                ", sig);

    const post = await tc.readTraderStateFromMarket(tc.routerConnection);
    console.log("  post-place trader state on ER:", post);
    expect(post!.baseLocked > (pre?.baseLocked ?? 0n), "5 base lots got locked")
      .to.be.true;
  }).timeout(60_000);

  it("places a crossing BID on the ER → in-engine matching fires", async () => {
    const pre = await tc.readTraderStateFromMarket(tc.routerConnection);
    console.log("  pre-cross trader state on ER:", pre);

    const sig = await tc.placeLimitOrderOnEr({
      type: "limit",
      side: "bid",
      priceInTicks: 100n,
      numBaseLots: 3n,
      selfTradeBehavior: "decrement_take",
      clientOrderId: 2n,
    });
    console.log("  place crossing BID tx:       ", sig);

    const post = await tc.readTraderStateFromMarket(tc.routerConnection);
    console.log("  post-cross trader state on ER:", post);
    // With DecrementTake self-trade, the resting ask is reduced by 3 base
    // lots; the locked count on the ask side drops accordingly.
    expect(post!.baseLocked < pre!.baseLocked, "ask locked decremented by match")
      .to.be.true;
  }).timeout(60_000);

  it("swaps on the ER — SwapWithFreeFunds aggressive IOC", async () => {
    // Place a fresh ask on the other side first so the swap has something
    // to cross against. Then swap an IOC bid against it.
    const sigA = await tc.placeLimitOrderOnEr({
      type: "post_only",
      side: "ask",
      priceInTicks: 200n,
      numBaseLots: 2n,
      clientOrderId: 10n,
    });
    console.log("  resting ask @ 200 tx:        ", sigA);

    const sigSwap = await tc.swapOnEr({
      type: "ioc",
      side: "bid",
      priceInTicks: 200n,
      numBaseLots: 2n,
      numQuoteLots: 0n,
      selfTradeBehavior: "decrement_take",
      clientOrderId: 11n,
    });
    console.log("  swap IOC bid tx:             ", sigSwap);
  }).timeout(60_000);

  it("cancels all remaining orders on the ER", async () => {
    const sig = await tc.cancelAllOrdersOnEr();
    console.log("  cancel-all tx:               ", sig);
    const post = await tc.readTraderStateFromMarket(tc.routerConnection);
    console.log("  post-cancel trader state ER:", post);
    expect(post!.baseLocked, "no base lots locked").to.equal(0n);
    expect(post!.quoteLocked, "no quote lots locked").to.equal(0n);
  }).timeout(60_000);

  // =====================================================================
  // Session keys — ephemeral signer authorized by the owner wallet
  // =====================================================================

  let sessionSigner: anchor.web3.Keypair;

  it("creates a SessionToken authorizing an ephemeral keypair", async () => {
    sessionSigner = anchor.web3.Keypair.generate();
    console.log("  session_signer:              ", sessionSigner.publicKey.toBase58());
    const expiresAt = BigInt(Math.floor(Date.now() / 1000) + 3600); // 1 hour
    const sig = await tc.createSessionToken(sessionSigner.publicKey, expiresAt);
    console.log("  CreateSessionToken tx:       ", sig);

    const [token] = tc.findSessionTokenAddress(
      tc.admin.publicKey,
      sessionSigner.publicKey,
    );
    const info = await tc.connection.getAccountInfo(token);
    expect(info, "session token PDA exists").to.not.be.null;
    expect(info!.owner.equals(PHOENIX_PROGRAM_ID), "owned by Phoenix").to.be.true;
  }).timeout(60_000);

  it("session-signed: places an ASK via session key (owner never signs)", async () => {
    // Fund the session signer so it can pay tx fees on the ER.
    const transferIx = anchor.web3.SystemProgram.transfer({
      fromPubkey: tc.admin.publicKey,
      toPubkey: sessionSigner.publicKey,
      lamports: 50_000_000,
    });
    await tc.provider.sendAndConfirm(new anchor.web3.Transaction().add(transferIx));

    const sig = await tc.placeLimitOrderViaSession(sessionSigner, {
      type: "post_only",
      side: "ask",
      priceInTicks: 150n,
      numBaseLots: 5n,
      clientOrderId: 100n,
    });
    console.log("  session place ASK tx:        ", sig);

    const post = await tc.readTraderStateFromMarket(tc.routerConnection);
    console.log("  post-place trader state ER:  ", post);
    expect(post!.baseLocked >= 5n, "ask got locked under owner's TraderState").to.be.true;
  }).timeout(60_000);

  it("session-signed: cancels orders via session key", async () => {
    const sig = await tc.cancelAllOrdersViaSession(sessionSigner);
    console.log("  session cancel-all tx:       ", sig);
    const post = await tc.readTraderStateFromMarket(tc.routerConnection);
    console.log("  post-cancel trader state ER: ", post);
    expect(post!.baseLocked, "all locks released").to.equal(0n);
  }).timeout(60_000);

  it("revokes the SessionToken (owner signs)", async () => {
    const sig = await tc.revokeSessionToken(sessionSigner.publicKey);
    console.log("  RevokeSessionToken tx:       ", sig);
    const [token] = tc.findSessionTokenAddress(
      tc.admin.publicKey,
      sessionSigner.publicKey,
    );
    const info = await tc.connection.getAccountInfo(token);
    expect(info, "session token closed").to.be.null;
  }).timeout(60_000);

  it("revoked session can no longer place orders", async () => {
    let failed = false;
    try {
      await tc.placeLimitOrderViaSession(sessionSigner, {
        type: "post_only",
        side: "ask",
        priceInTicks: 200n,
        numBaseLots: 1n,
        clientOrderId: 999n,
      });
    } catch (e: any) {
      failed = true;
      console.log("  ✓ revoked session rejected:", String(e.message).slice(0, 80) + "…");
    }
    expect(failed, "post-revoke session ix must fail").to.be.true;
  }).timeout(60_000);

  it("Magic Action deposit — single sig, full chain auto-fires", async () => {
    const baseSig = await tc.requestDeposit(new BN(50), new BN(500));
    console.log("  request deposit tx:        ", baseSig);

    const [receipt] = tc.findDepositReceiptAddress(tc.market, tc.admin.publicKey);
    console.log("  waiting for full chain auto-fire (ProcessDepositEr + CloseDepositReceipt)…");
    await tc.waitForReceiptClosed(receipt);
    console.log("  ✓ receipt closed by post-undelegate auto-fire");
  }).timeout(240_000);

  it("Magic Action withdraw — single sig, full chain auto-fires", async () => {
    const pre = await tc.getAtaBalances();
    preWithdrawBase = pre.base;
    preWithdrawQuote = pre.quote;
    console.log("  pre-withdraw base ATA:     ", preWithdrawBase.toString());
    console.log("  pre-withdraw quote ATA:    ", preWithdrawQuote.toString());

    const baseSig = await tc.requestWithdrawal(new BN(25), new BN(250));
    console.log("  request withdraw tx:       ", baseSig);

    const [receipt] = tc.findWithdrawalReceiptAddress(tc.market, tc.admin.publicKey);
    console.log("  waiting for full chain auto-fire (ProcessWithdrawalEr + ExecuteWithdrawalBaseChain)…");
    await tc.waitForReceiptClosed(receipt);
    console.log("  ✓ receipt closed by post-undelegate auto-fire");

    const post = await tc.getAtaBalances();
    console.log("  post-withdraw base ATA:    ", post.base.toString());
    console.log("  post-withdraw quote ATA:   ", post.quote.toString());
    expect(post.base > preWithdrawBase, "base ATA grew (vault paid out via auto-fired action)")
      .to.be.true;
    expect(post.quote > preWithdrawQuote, "quote ATA grew (vault paid out via auto-fired action)")
      .to.be.true;
  }).timeout(240_000);
});
