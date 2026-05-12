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
