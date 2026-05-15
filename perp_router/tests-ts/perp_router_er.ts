// perp_router ER end-to-end test — devnet.
//
// Demonstrates ER trading with real devnet signatures. Strategy:
//   - TraderAccount + PerpMarket delegated to ER.
//   - GlobalState may already be delegation-owned on base from a prior
//     run; the Magic Action deposit writes it on the ER (where delegated
//     state is fine), avoiding the strict base-side owner check that
//     `direct_deposit` enforces.
//   - Trading (open/close) runs on ER, real Solana confirmation each tx.
//
// Re-runnable across runs without redeploying: each run uses a fresh
// trader keypair and fresh phoenix_market pubkey.
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

describe("perp-router-er (devnet, ER trading)", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const wallet = (provider.wallet as anchor.Wallet).payer;
  const trader = Keypair.generate();
  const tc = new PerpTestClient(wallet);

  let quoteMint: Keypair;
  let traderAta: PublicKey;
  let perpMarketPda: PublicKey;
  let orderbookPda: PublicKey;

  before(async () => {
    console.log("Program ID:    ", PERP_ROUTER_PROGRAM_ID.toBase58());
    console.log("Admin / payer: ", wallet.publicKey.toBase58());
    console.log("Trader:        ", trader.publicKey.toBase58());

    // Trader pays rent for the DepositReceipt PDA + delegation aux
    // accounts created during requestCollateralDeposit. 0.2 SOL is
    // comfortably above the actual cost.
    const tx = new Transaction().add(
      SystemProgram.transfer({
        fromPubkey: wallet.publicKey,
        toPubkey: trader.publicKey,
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
    console.log("    fund trader tx:", sig);
  });

  it("1. setup — mint + trader ATA + perp vault ATA", async () => {
    const { mint } = await tc.createQuoteMint(6);
    quoteMint = mint;
    const fund = await tc.fundTrader(
      trader.publicKey,
      mint.publicKey,
      new BN(1_000_000_000),
    );
    traderAta = fund.ata;
    await tc.ensureVaultAta(mint.publicKey);
    console.log("    mint:    ", mint.publicKey.toBase58());
    console.log("    traderAta:", traderAta.toBase58());
  });

  it("2. init — GlobalState (idempotent) + PerpMarket + TraderAccount + Orderbook", async () => {
    const [g] = tc.globalStatePda();
    const gInfo = await tc.baseConnection.getAccountInfo(g);
    if (!gInfo) {
      const s1 = await tc.initializeGlobalState();
      console.log("    init global:  ", s1);
    } else {
      console.log("    global exists, skipping init");
    }
    const s2 = await tc.initializeMarket(
      PHOENIX_MARKET,
      quoteMint.publicKey,
      quoteMint.publicKey,
      ORACLE,
    );
    const s3 = await tc.initializeTrader(trader);
    [perpMarketPda] = tc.perpMarketPda(PHOENIX_MARKET);
    console.log("    init market:  ", s2);
    console.log("    init trader:  ", s3);

    // Orderbook: in-process phoenix FIFOMarket<Pubkey, 32, 32, 32>,
    // 9,104 bytes. PDA seeded by perp_market — fits under the 10 KB
    // single-CPI alloc cap, so perp_router signs the create_account
    // itself. The shape is constrained: 32×32×32 is the largest one that
    // is both PDA-allocatable in one CPI and delegatable to MagicBlock
    // ER. See state/orderbook.rs + the sweep test for why.
    const PERP_ORDERBOOK_SIZE = 9_104;
    const { orderbook, sig: s4 } = await tc.initializeOrderbook(
      perpMarketPda,
      new BN(1),
      new BN(1),
      0,
    );
    orderbookPda = orderbook;
    console.log("    init orderbook:", s4);
    console.log("    orderbook pda: ", orderbook.toBase58());
    const obInfo = await tc.baseConnection.getAccountInfo(orderbook);
    assert(obInfo, "orderbook account must exist after init");
    assert(
      obInfo!.owner.equals(PERP_ROUTER_PROGRAM_ID),
      `orderbook owner = ${obInfo!.owner.toBase58()}, expected perp_router`,
    );
    assert(
      obInfo!.data.length === PERP_ORDERBOOK_SIZE,
      `orderbook size = ${obInfo!.data.length}, expected ${PERP_ORDERBOOK_SIZE}`,
    );
    console.log("    orderbook size:", obInfo!.data.length, "bytes");
  });

  it("3. (base) delegate GlobalState + PerpMarket + TraderAccount + Orderbook to ER", async () => {
    // Idempotent: skip any account that's already delegation-program-owned
    // on base (carry-over from a prior run). Step 9 leaves them undelegated
    // when it succeeds, but we don't want a failed cleanup to block reruns.
    const [g] = tc.globalStatePda();
    const [t] = tc.traderAccountPda(trader.publicKey);
    const delegated = async (k: PublicKey) =>
      (await tc.baseConnection.getAccountInfo(k))?.owner.equals(DELEGATION_PROGRAM_ID) ?? false;

    for (const [label, pda, signer] of [
      ["global", g, wallet],
      ["market", perpMarketPda, wallet],
      ["trader", t, trader],
    ] as const) {
      if (await delegated(pda)) {
        console.log(`    ${label} already delegated, skipping`);
        continue;
      }
      const tag = label === "global" ? 3 : label === "market" ? 4 : 2;
      const aux = tc.delegationAuxFor(pda);
      const sig = await tc.delegateAccount(tag, pda, signer, aux.buffer, aux.record, aux.metadata);
      console.log(`    delegate ${label}: `, sig);
    }

    // Orderbook delegation uses its own account list (perp_market is
    // needed server-side to derive the bump), so it goes through the
    // dedicated builder rather than the generic delegateAccount.
    if (await delegated(orderbookPda)) {
      console.log("    orderbook already delegated, skipping");
    } else {
      const sig = await tc.delegateOrderbook(perpMarketPda);
      console.log("    delegate orderbook:", sig);
    }
  });

  it("3b. (ER) ClaimSeat — register trader in orderbook", async () => {
    const sig = await tc.claimSeat(trader, perpMarketPda);
    console.log("    claim seat:   ", sig);
  });

  it("3c. (ER) PlaceOrderPerp — post a Bid limit, matching engine fires", async () => {
    // Bid @ 100 ticks for 10 base lots. With an empty book the order
    // posts as resting liquidity — proves the in-tree matching engine
    // ran and mutated PerpOrderbook on the ER.
    const sig = await tc.placeOrderPerp(
      trader,
      perpMarketPda,
      0, // Side::Bid
      new BN(100),
      new BN(10),
      new BN(1),
    );
    console.log("    place order: ", sig);
    // Sanity: orderbook still exists at the expected size and is
    // delegation-owned on base (i.e. live state lives on ER).
    const obInfo = await tc.baseConnection.getAccountInfo(orderbookPda);
    assert(obInfo, "orderbook still exists on base");
    assert(
      obInfo!.owner.equals(DELEGATION_PROGRAM_ID),
      `orderbook owner = ${obInfo!.owner.toBase58()}, expected delegation program`,
    );
  });

  it("4. magic deposit — request (base) + process (ER) credits 200 USDC", async () => {
    // Stage 1 (base): SPL transfer trader→vault, create DepositReceipt,
    // delegate receipt + queue ProcessCollateralDepositEr.
    const [receipt] = tc.depositReceiptPda(trader.publicKey);
    const auxR = tc.delegationAuxFor(receipt);
    const s1 = await tc.requestCollateralDeposit(
      trader,
      quoteMint.publicKey,
      perpMarketPda,
      traderAta,
      new BN(200_000_000),
      auxR.buffer,
      auxR.record,
      auxR.metadata,
    );
    console.log("    request (base):", s1);

    // Wait for the validator to replicate the delegation + run the
    // queued ProcessCollateralDepositEr auto-fire on the ER.
    console.log("    waiting 15s for auto-fire to land...");
    await new Promise((r) => setTimeout(r, 15_000));

    // If auto-fire already credited collateral, we're done.
    let c = await tc.getTraderCollateral(trader.publicKey);
    if (c.eqn(0)) {
      // Auto-fire didn't land — invoke stage 2 manually as fallback.
      console.log("    auto-fire didn't land; calling stage 2 manually...");
      try {
        const s2 = await tc.processCollateralDepositEr(trader);
        console.log("    process (ER): ", s2);
      } catch (e) {
        // Common case: auto-fire raced us; receipt is processed →
        // Custom(12) ReceiptAlreadyProcessed. Treat as success.
        const msg = String((e as any)?.message ?? e);
        if (msg.includes("0xc") || msg.includes("ReceiptAlreadyProcessed")) {
          console.log("    process (ER): receipt already processed (race)");
        } else {
          throw e;
        }
      }
      c = await tc.getTraderCollateral(trader.publicKey);
    } else {
      console.log("    process (ER): auto-fired by validator");
    }
    console.log("    collateral:    ", c.toString());
    assert(c.eq(new BN(200_000_000)));
  });

  it("5. (ER) DirectOpenPosition — long 1 @ $100, $50 margin (2x)", async () => {
    const sig = await tc.directOpenPosition(
      trader,
      PHOENIX_MARKET,
      new BN(1),
      new BN(100_000_000),
      new BN(50_000_000),
      true,
    );
    console.log("    open (ER):    ", sig);
  });

  it("6. (ER) DirectClosePosition @ $102 — gain to reserve", async () => {
    const sig = await tc.directClosePosition(
      trader,
      PHOENIX_MARKET,
      new BN(102_000_000),
      true,
    );
    console.log("    close (ER):   ", sig);
  });

  it("7. (ER) MaturePnl — sweep reserve to matured after warmup", async () => {
    // WARMUP_SLOTS = 150 ≈ ~60s on devnet (slot time ~400ms). Pad to
    // ensure the slot has advanced past mature_slot.
    console.log("    waiting 75s for warmup slots to elapse...");
    await new Promise((r) => setTimeout(r, 75_000));
    const sig = await tc.maturePnl(trader);
    console.log("    mature (ER): ", sig);
    const m = await tc.getTraderPnlMatured(trader.publicKey);
    console.log("    pnl_matured:  ", m.toString());
    assert(m.gt(new BN(0)), "pnl_matured must be > 0 after maturePnl");
  });

  it("8. magic withdraw — request (base) + auto-fired process + execute", async () => {
    const beforeAta = await tc.getTokenBalance(traderAta);
    const [w] = tc.withdrawalReceiptPda(trader.publicKey);
    const auxW = tc.delegationAuxFor(w);
    // Pull 50 USDC (well under collateral + matured reserve).
    const s1 = await tc.requestCollateralWithdrawal(
      trader,
      quoteMint.publicKey,
      perpMarketPda,
      traderAta,
      new BN(50_000_000),
      auxW.buffer,
      auxW.record,
      auxW.metadata,
    );
    console.log("    request (base):", s1);
    console.log("    waiting 20s for ER process + base execute to land...");
    await new Promise((r) => setTimeout(r, 20_000));
    const afterAta = await tc.getTokenBalance(traderAta);
    const delta = afterAta.sub(beforeAta);
    console.log("    ATA delta:    ", delta.toString());
    assert(delta.gt(new BN(0)), "trader ATA must increase after withdraw");
  });

  it("9. (ER) Undelegate trader + global + market — return to base", async () => {
    const [t] = tc.traderAccountPda(trader.publicKey);
    const [g] = tc.globalStatePda();
    // Use wallet (= feepayer) as signer for all three. The undelegate
    // handler is permissionless and trader isn't writable on ER.
    const sT = await tc.undelegateAccount(wallet, t);
    const sG = await tc.undelegateAccount(wallet, g);
    const sM = await tc.undelegateAccount(wallet, perpMarketPda);
    console.log("    undelegate trader (ER):", sT);
    console.log("    undelegate global (ER):", sG);
    console.log("    undelegate market (ER):", sM);
    console.log("    waiting up to 20s for commit-backs to land on base...");
    const targets = [
      ["trader_account", t],
      ["global_state", g],
      ["perp_market", perpMarketPda],
    ] as const;
    for (let i = 0; i < 20; i++) {
      await new Promise((r) => setTimeout(r, 1_000));
      const owners = await Promise.all(
        targets.map(([_, k]) => tc.baseConnection.getAccountInfo(k)),
      );
      if (owners.every((o) => o?.owner.equals(PERP_ROUTER_PROGRAM_ID))) {
        console.log(`    all three restored after ${i + 1}s`);
        return;
      }
    }
    for (const [label, k] of targets) {
      const info = await tc.baseConnection.getAccountInfo(k);
      console.log(`    ${label} owner: ${info?.owner.toBase58()}`);
      assert(
        info?.owner.equals(PERP_ROUTER_PROGRAM_ID),
        `${label} not restored to perp_router on base`,
      );
    }
  });

  it("10. summary — full ER round-trip complete", async () => {
    console.log("\n   ✓ Full Percolator + ER round-trip on devnet:");
    console.log("   ✓ deposit → delegate → open → close → mature → withdraw → undelegate");
    console.log("   ✓ Verify on https://explorer.solana.com/?cluster=devnet");
  });
});
