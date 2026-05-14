# perp_router devnet E2E test

Mirrors `phoenix-v1/tests-ts/`. A single `describe` block with sequential
`it()` steps; each prints its confirmed devnet transaction signature.

## Layout

```
perp_router/tests-ts/
├── test_client.ts        — PerpTestClient wraps base + ER connections,
│                            one method per perp_router instruction
└── perp_router_er.ts     — sequential E2E flow (mocha)
```

## Steps

| # | Step | Layer | Tests |
|---|---|---|---|
| 1 | `setup — quote mint + funded trader ATA`             | base       | SPL infrastructure |
| 2 | `initializes GlobalState`                            | base       | `InitializeGlobalState` |
| 3 | `initializes PerpMarket`                             | base       | `InitializeMarket` (binds Phoenix market + Pyth oracle) |
| 4 | `initializes TraderAccount`                          | base       | `InitializeTrader` |
| 5 | `Magic Action deposit`                               | base+ER+base | `RequestCollateralDeposit` → auto `ProcessCollateralDepositEr` → auto `CloseCollateralDepositReceipt` |
| 6 | `opens a long position at envelope-clamped mark`     | ER         | `OpenPosition` (recovery gate + envelope + leverage) |
| 7 | `closes the position; PnL → reserve`                 | ER         | `ClosePosition` (lazy A/K/F/B + warmup push) |
| 8 | `cranks MaturePnl — sweep reserve → matured`         | ER         | `MaturePnl` (warmup ageing) |
| 9 | `Magic Action withdraw`                              | base+ER+base | `RequestCollateralWithdrawal` → auto `ProcessCollateralWithdrawalEr` (haircut!) → auto `ExecuteCollateralWithdrawalBaseChain` |
| 10 | `undelegates trader account`                         | ER         | `UndelegateTraderAccount` |

Steps 5 and 9 use `GetCommitmentSignature` from the MagicBlock SDK to block
until the post-undelegate action lands on the base layer.

## Pre-requisites

```bash
# 0. Make sure the Solana CLI is recent (older toolchains can't parse
#    edition2024 in transitive deps):
solana-install update
solana --version   # expect >= 1.18, ideally 2.x

# 1. Deploy perp_router to devnet
cargo build-sbf -p perp-router
solana program deploy target/deploy/perp_router.so
# → note the deployed program ID

# 2. Confirm a Phoenix market already exists on devnet (initialise via
#    phoenix-v1/tests-ts if needed)

# 3. Wallet >= 2 SOL
solana airdrop 2
```

## Run

```bash
export ANCHOR_PROVIDER_URL=https://api.devnet.solana.com
export ANCHOR_WALLET=~/.config/solana/id.json
export PERP_ROUTER_PROGRAM_ID=<deployed_program_id>
export PHOENIX_MARKET=<existing devnet phoenix market>
export ORACLE=<devnet Pyth feed, e.g. SOL/USD>

# Optional MagicBlock router overrides:
export ROUTER_ENDPOINT=https://devnet-router.magicblock.app
export ROUTER_WS_ENDPOINT=wss://devnet-router.magicblock.app

cd perp_router/tests-ts
yarn install
yarn test
```

## Known v1 gaps

- **`mark_price` param** in steps 6–7 / `crank_funding`: v1 accepts a
  client-supplied price (envelope-clamped against `last_oracle_price`). v1.1
  will parse a Pyth Pull `PriceUpdateV2` account directly on-chain.

- **Phoenix CPI matching**: v1 open/close uses oracle-priced synthetic
  settlement. v1.1 will swap into Phoenix via `cpi/phoenix.rs`.

- **Replace placeholder program id**: after `solana program deploy`, update
  `declare_id!()` in `perp_router/src/lib.rs` to match the deployed key, or
  set `PERP_ROUTER_PROGRAM_ID` env var to point at the deployed program.
