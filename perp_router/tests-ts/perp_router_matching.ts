// Two-trader cross-trade test for the in-tree matching engine on ER.
//
// Stage 3b verification: trader A posts a resting ASK; wallet B places
// a crossing BID; on-chain the fill is captured, A's locked_margin is
// released into a short position, B's collateral is debited into a long
// position. No PnL / withdraw flow — this spec is matching-focused.
//
// Re-runnable: each run uses a fresh trader keypair for A. Wallet B is
// reused across runs (its TraderAccount is initialized idempotently).
//
// Env vars: ANCHOR_PROVIDER_URL, ANCHOR_WALLET, PERP_ROUTER_PROGRAM_ID

import * as anchor from "@coral-xyz/anchor";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
} from "@solana/web3.js";
import BN from "bn.js";
import { strict as assert } from "assert";

import {
  DELEGATION_PROGRAM_ID,
  PERP_ROUTER_PROGRAM_ID,
  PerpTestClient,
} from "./test_client";

const PHOENIX_MARKET = Keypair.generate().publicKey;
const ORACLE = Keypair.generate().publicKey;

describe("perp-router-matching (devnet, cross-trade fills)", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const wallet = (provider.wallet as anchor.Wallet).payer;
  const traderA = Keypair.generate();
  const tc = new PerpTestClient(wallet);

  let quoteMint: Keypair;
  let traderAAta: PublicKey;
  let walletAta: PublicKey;
  let perpMarketPda: PublicKey;
  let traderAAccountPda: PublicKey;
  let walletAccountPda: PublicKey;

  before(async () => {
    console.log("Program ID:    ", PERP_ROUTER_PROGRAM_ID.toBase58());
    console.log("Wallet (B):    ", wallet.publicKey.toBase58());
    console.log("Trader A:      ", traderA.publicKey.toBase58());

    const tx = new Transaction().add(
      SystemProgram.transfer({
        fromPubkey: wallet.publicKey,
        toPubkey: traderA.publicKey,
        lamports: 200_000_000,
      }),
    );
    tx.feePayer = wallet.publicKey;
    tx.recentBlockhash = (await tc.baseConnection.getLatestBlockhash()).blockhash;
    tx.sign(wallet);
    const sig = await tc.baseConnection.sendRawTransaction(tx.serialize(), {
      skipPreflight: true,
    });
    await tc.baseConnection.confirmTransaction(sig, "confirmed");

    [perpMarketPda] = tc.perpMarketPda(PHOENIX_MARKET);
    [traderAAccountPda] = tc.traderAccountPda(traderA.publicKey);
    [walletAccountPda] = tc.traderAccountPda(wallet.publicKey);
  });

  it("1. mint + ATAs for A and wallet", async () => {
    const { mint } = await tc.createQuoteMint(6);
    quoteMint = mint;
    traderAAta = (await tc.fundTrader(traderA.publicKey, mint.publicKey, new BN(500_000_000))).ata;
    walletAta = (await tc.fundTrader(wallet.publicKey, mint.publicKey, new BN(500_000_000))).ata;
    await tc.ensureVaultAta(mint.publicKey);
    console.log("    mint: ", mint.publicKey.toBase58());
  });

  it("2. init — GlobalState + PerpMarket + both TraderAccounts + Orderbook", async () => {
    const [g] = tc.globalStatePda();
    if (!(await tc.baseConnection.getAccountInfo(g))) {
      await tc.initializeGlobalState();
    }
    await tc.initializeMarket(PHOENIX_MARKET, quoteMint.publicKey, quoteMint.publicKey, ORACLE);
    await tc.initializeTrader(traderA);
    // Wallet's TraderAccount may already exist from a prior run.
    if (!(await tc.baseConnection.getAccountInfo(walletAccountPda))) {
      await tc.initializeTrader(wallet);
    }
    const { orderbook, sig } = await tc.initializeOrderbook(
      perpMarketPda,
      new BN(1),
      new BN(1),
      0,
    );
    console.log("    init orderbook:", sig);
    console.log("    orderbook pda: ", orderbook.toBase58());
  });

  it("3. direct deposits — A: 200, wallet: top-up to ≥ 200", async () => {
    // Trader A is a fresh keypair every run; expect exactly 200M.
    await tc.directDeposit(traderA, quoteMint.publicKey, traderAAta, new BN(200_000_000));

    // Wallet's TraderAccount carries collateral across runs. Top up
    // only what's needed to get to >= 200M.
    const bBefore = await tc.getTraderCollateral(wallet.publicKey);
    const target = new BN(200_000_000);
    if (bBefore.lt(target)) {
      const needed = target.sub(bBefore);
      await tc.directDeposit(wallet, quoteMint.publicKey, walletAta, needed);
    }

    const a = await tc.getTraderCollateral(traderA.publicKey);
    const b = await tc.getTraderCollateral(wallet.publicKey);
    console.log("    A collateral:", a.toString(), " wallet collateral:", b.toString());
    // BN.eqn truncates its operand to 26 bits, so values > ~67M must
    // go through `.eq(new BN(...))`.
    assert(a.eq(new BN(200_000_000)), `A collateral = ${a.toString()}`);
    assert(b.gte(new BN(200_000_000)), `wallet collateral = ${b.toString()}`);
  });

  it("4. delegate global + perp_market + both TraderAccounts + orderbook", async () => {
    const [g] = tc.globalStatePda();
    const delegated = async (k: PublicKey) =>
      (await tc.baseConnection.getAccountInfo(k))?.owner.equals(DELEGATION_PROGRAM_ID) ?? false;

    for (const [label, pda, signer, tag] of [
      ["global", g, wallet, 3],
      ["market", perpMarketPda, wallet, 4],
      ["traderA", traderAAccountPda, traderA, 2],
      ["wallet", walletAccountPda, wallet, 2],
    ] as const) {
      if (await delegated(pda)) {
        console.log(`    ${label} already delegated, skipping`);
        continue;
      }
      const aux = tc.delegationAuxFor(pda);
      await tc.delegateAccount(tag, pda, signer, aux.buffer, aux.record, aux.metadata);
      console.log(`    delegate ${label} ok`);
    }

    const [orderbook] = tc.orderbookPda(perpMarketPda);
    if (!(await delegated(orderbook))) {
      await tc.delegateOrderbook(perpMarketPda);
      console.log("    delegate orderbook ok");
    }
  });

  it("5. claim seats — A + wallet", async () => {
    await tc.claimSeat(traderA, perpMarketPda);
    await tc.claimSeat(wallet, perpMarketPda);
    console.log("    both seats claimed");
  });

  it("6. (ER) trader A posts resting ASK at price=100, size=5 → locks 50 margin", async () => {
    const sig = await tc.placeOrderPerp(
      traderA,
      perpMarketPda,
      1, // Ask
      new BN(100),
      new BN(5),
      new BN(10),
    );
    console.log("    A place ask:", sig);
    const lm = await tc.getTraderLockedMargin(traderA.publicKey);
    assert(lm.eqn(50), `A.locked_margin = ${lm.toString()}, expected 50`);
  });

  it("7. (ER) wallet B places crossing BID — fill fires, both sides settle", async () => {
    const aColBefore = await tc.getTraderCollateral(traderA.publicKey);
    const bColBefore = await tc.getTraderCollateral(wallet.publicKey);
    const aLockedBefore = await tc.getTraderLockedMargin(traderA.publicKey);
    const bLockedBefore = await tc.getTraderLockedMargin(wallet.publicKey);

    const sig = await tc.placeOrderPerp(
      wallet,
      perpMarketPda,
      0, // Bid (crosses A's ask)
      new BN(100),
      new BN(5),
      new BN(11),
      [traderAAccountPda],
    );
    console.log("    B place bid (taker):", sig);

    const aColAfter = await tc.getTraderCollateral(traderA.publicKey);
    const bColAfter = await tc.getTraderCollateral(wallet.publicKey);
    const aLockedAfter = await tc.getTraderLockedMargin(traderA.publicKey);
    const bLockedAfter = await tc.getTraderLockedMargin(wallet.publicKey);
    const aPos = await tc.getTraderPositionForMarket(traderA.publicKey, perpMarketPda);
    const bPos = await tc.getTraderPositionForMarket(wallet.publicKey, perpMarketPda);

    console.log("    Δ A.collateral:   ", aColBefore.sub(aColAfter).toString());
    console.log("    Δ A.locked_margin:", aLockedBefore.sub(aLockedAfter).toString());
    console.log("    Δ B.collateral:   ", bColBefore.sub(bColAfter).toString());
    console.log("    Δ B.locked_margin:", bLockedBefore.sub(bLockedAfter).toString());
    console.log("    A.position:", aPos);
    console.log("    B.position:", bPos);

    // Maker A: locked_margin released by 50, collateral untouched, now short 5.
    assert(aLockedBefore.sub(aLockedAfter).eqn(50), "A locked_margin should drop by 50");
    assert(aColBefore.eq(aColAfter), "A collateral should be unchanged");
    assert(aPos !== null, "A should have a position");
    assert(aPos!.size_stored.eqn(-5), `A.size_stored = ${aPos!.size_stored.toString()}, expected -5`);
    assert(aPos!.entry_price.eqn(100), `A.entry_price = ${aPos!.entry_price.toString()}, expected 100`);
    assert(aPos!.margin_locked.eqn(50), `A.margin_locked = ${aPos!.margin_locked.toString()}, expected 50`);

    // Taker B: collateral debited 50, locked_margin untouched, now long 5.
    assert(bColBefore.sub(bColAfter).eqn(50), "B collateral should drop by 50");
    assert(bLockedBefore.eq(bLockedAfter), "B locked_margin should be unchanged");
    assert(bPos !== null, "B should have a position");
    assert(bPos!.size_stored.eqn(5), `B.size_stored = ${bPos!.size_stored.toString()}, expected 5`);
    assert(bPos!.entry_price.eqn(100), `B.entry_price = ${bPos!.entry_price.toString()}, expected 100`);
    assert(bPos!.margin_locked.eqn(50), `B.margin_locked = ${bPos!.margin_locked.toString()}, expected 50`);
  });

  it("8. (ER) wallet B posts resting ASK at price=110, size=5 → locks 55 margin", async () => {
    const lmBefore = await tc.getTraderLockedMargin(wallet.publicKey);
    const sig = await tc.placeOrderPerp(
      wallet,
      perpMarketPda,
      1, // Ask
      new BN(110),
      new BN(5),
      new BN(20),
    );
    console.log("    B place ask:", sig);
    const lmAfter = await tc.getTraderLockedMargin(wallet.publicKey);
    assert(
      lmAfter.sub(lmBefore).eqn(55),
      `B locked_margin delta = ${lmAfter.sub(lmBefore).toString()}, expected 55`,
    );
  });

  it("9. (ER) trader A places crossing BID @ 110 — closes BOTH sides, realizes PnL", async () => {
    // After step 7: A is short 5 @ 100, B is long 5 @ 100.
    // After step 8: B has a resting ask 5 @ 110.
    // Now A bids at 110 — crosses B's ask. Both positions close.
    //   A (short→buy): PnL = (entry 100 - fill 110) * 5 = -50 (loss)
    //   B (long→sell): PnL = (fill 110 - entry 100) * 5 = +50 (profit, → reserve)
    const aColBefore = await tc.getTraderCollateral(traderA.publicKey);
    const bColBefore = await tc.getTraderCollateral(wallet.publicKey);
    const bReserveBefore = await tc.getTraderPnlReserveTotal(wallet.publicKey);
    const aLenBefore = await tc.getTraderPositionsLen(traderA.publicKey);
    const bLenBefore = await tc.getTraderPositionsLen(wallet.publicKey);

    const sig = await tc.placeOrderPerp(
      traderA,
      perpMarketPda,
      0, // Bid (crosses B's ask)
      new BN(110),
      new BN(5),
      new BN(21),
      [walletAccountPda],
    );
    console.log("    A close bid (taker):", sig);

    const aColAfter = await tc.getTraderCollateral(traderA.publicKey);
    const bColAfter = await tc.getTraderCollateral(wallet.publicKey);
    const aLockedAfter = await tc.getTraderLockedMargin(traderA.publicKey);
    const bLockedAfter = await tc.getTraderLockedMargin(wallet.publicKey);
    const aReserveAfter = await tc.getTraderPnlReserveTotal(traderA.publicKey);
    const bReserveAfter = await tc.getTraderPnlReserveTotal(wallet.publicKey);
    const aPos = await tc.getTraderPositionForMarket(traderA.publicKey, perpMarketPda);
    const bPos = await tc.getTraderPositionForMarket(wallet.publicKey, perpMarketPda);

    console.log("    Δ A.collateral:        ", aColAfter.sub(aColBefore).toString(), "(released 50 - loss 50 = 0)");
    console.log("    Δ B.collateral:        ", bColAfter.sub(bColBefore).toString(), "(released 50)");
    console.log("    A.locked_margin:       ", aLockedAfter.toString());
    console.log("    B.locked_margin:       ", bLockedAfter.toString());
    console.log("    A.pnl_reserve_total:   ", aReserveAfter.toString());
    console.log("    Δ B.pnl_reserve_total: ", bReserveAfter.sub(bReserveBefore).toString());
    console.log("    A.position:", aPos);
    console.log("    B.position:", bPos);

    // A: short closed at higher price → loss 50. position margin 50 released
    //    back to collateral, then 50 debited as loss → net Δ = 0.
    assert(aColAfter.eq(aColBefore), "A collateral net Δ should be 0");
    assert(aLockedAfter.eqn(0), "A locked_margin should be 0");
    assert(aReserveAfter.eqn(0), "A should have no reserve entry (loss)");
    // Slot compaction: after a full close the position slot is dropped
    // (swap-with-last + len--), not just zeroed in place. So we expect
    // getTraderPositionForMarket to return null and positions_len == 0
    // for the only-market case.
    assert(aPos === null, "A position slot should be compacted (null) after full close");

    // B: long closed at higher price → profit 50 to reserve. position margin
    //    50 released back to collateral. B's resting-ask locked_margin (55)
    //    is released as part of the fill (consumed by the close).
    assert(bColAfter.sub(bColBefore).eqn(50), "B collateral should increase by 50 (released margin)");
    assert(bLockedAfter.eqn(0), "B locked_margin should be 0");
    assert(bReserveAfter.sub(bReserveBefore).eqn(50), "B pnl_reserve should grow by 50 (profit)");
    assert(bPos === null, "B position slot should be compacted (null) after full close");

    // Compaction check: each side had one slot for this market entering
    // step 9; after the full close that slot is swapped-with-last and
    // positions_len decremented. We assert -1 deltas rather than absolute
    // 0 because wallet's TraderAccount carries zombie slots from earlier
    // runs (predating the compaction fix).
    const aLenAfter = await tc.getTraderPositionsLen(traderA.publicKey);
    const bLenAfter = await tc.getTraderPositionsLen(wallet.publicKey);
    console.log(
      "    A.positions_len:", aLenBefore, "→", aLenAfter,
      "   B.positions_len:", bLenBefore, "→", bLenAfter,
    );
    assert.equal(aLenBefore - aLenAfter, 1, "A.positions_len should decrement by 1");
    assert.equal(bLenBefore - bLenAfter, 1, "B.positions_len should decrement by 1");
  });

  it("10. (ER) A posts small resting BID — locks margin to test withdraw gate", async () => {
    // After step 9 both traders have flat positions and zero locked_margin.
    // Re-lock a small amount on A so we can prove direct_withdraw refuses
    // to drain it after undelegation. Notional = 100 × 5 = 500 quote lots;
    // at 10x leverage that's 50 atoms locked.
    const lmBefore = await tc.getTraderLockedMargin(traderA.publicKey);
    await tc.placeOrderPerp(
      traderA,
      perpMarketPda,
      0, // Bid
      new BN(100),
      new BN(5),
      new BN(99),
    );
    const lmAfter = await tc.getTraderLockedMargin(traderA.publicKey);
    assert(
      lmAfter.sub(lmBefore).eqn(50),
      `A locked_margin delta = ${lmAfter.sub(lmBefore).toString()}, expected 50`,
    );
  });

  it("11. (ER) undelegate everything back to base — commits locked_margin", async () => {
    const [g] = tc.globalStatePda();
    for (const [label, k] of [
      ["traderA", traderAAccountPda],
      ["wallet", walletAccountPda],
      ["global", g],
      ["market", perpMarketPda],
    ] as const) {
      try {
        await tc.undelegateAccount(wallet, k);
        console.log(`    undelegate ${label} ok`);
      } catch (e) {
        console.log(`    undelegate ${label} skipped:`, String((e as any)?.message ?? e).slice(0, 80));
      }
    }
    // Wait for commit-backs to land on base before the next step inspects
    // base-side state.
    for (let i = 0; i < 20; i++) {
      await new Promise((r) => setTimeout(r, 1_000));
      const info = await tc.baseConnection.getAccountInfo(traderAAccountPda);
      if (info?.owner.equals(PERP_ROUTER_PROGRAM_ID)) {
        console.log(`    trader A restored to base after ${i + 1}s`);
        break;
      }
    }
  });

  it("12. (base) direct_withdraw must respect locked_margin gate", async () => {
    // Verify the soundness fix: with A.collateral = 200M and locked_margin
    // = 50, asking to withdraw the full 200M should pay only the free
    // portion (200M - 50). Pre-fix would have drained the locked margin.
    const lockedBefore = await tc.getTraderLockedMargin(traderA.publicKey);
    const ataBefore = await tc.getTokenBalance(traderAAta);
    const colBefore = await tc.getTraderCollateral(traderA.publicKey);

    const sig = await tc.directWithdraw(
      traderA,
      quoteMint.publicKey,
      traderAAta,
      new BN(200_000_000),
    );
    console.log("    direct_withdraw:", sig);

    const ataAfter = await tc.getTokenBalance(traderAAta);
    const colAfter = await tc.getTraderCollateral(traderA.publicKey);
    const lockedAfter = await tc.getTraderLockedMargin(traderA.publicKey);
    const ataDelta = ataAfter.sub(ataBefore);
    const expectedFree = colBefore.sub(lockedBefore);

    console.log("    A.collateral before:", colBefore.toString());
    console.log("    A.locked_margin:    ", lockedBefore.toString());
    console.log("    free (expected pay):", expectedFree.toString());
    console.log("    ATA delta:          ", ataDelta.toString());
    console.log("    A.collateral after: ", colAfter.toString());

    // No haircut when v_total_pool_value >= c_total_collateral + matured
    // (pool is healthy). Net payout equals coll_req exactly.
    assert(
      ataDelta.eq(expectedFree),
      `ATA delta = ${ataDelta.toString()}, expected ${expectedFree.toString()}`,
    );
    // locked_margin remains untouched — that's the point.
    assert(
      lockedAfter.eq(lockedBefore),
      `locked_margin should not change, got ${lockedAfter.toString()}`,
    );
    // Trader.collateral reduced by exactly what was paid out.
    assert(
      colBefore.sub(colAfter).eq(expectedFree),
      `collateral Δ = ${colBefore.sub(colAfter).toString()}, expected ${expectedFree.toString()}`,
    );
  });
});
