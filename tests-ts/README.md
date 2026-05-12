# Phoenix + MagicBlock devnet E2E test

Mirrors the `magic-trade` test style: a single `describe` block with sequential `it()` steps, each printing its confirmed devnet transaction signature.

## Layout

```
tests-ts/anchor/
├── test_client.ts   — TestClient class: provider, connections, PDAs, ix methods
└── phoenix_er.ts    — the actual test file (mocha)
```

`TestClient` exposes one async method per Phoenix-ER instruction and returns the confirmed transaction signature for every one of them.

## Prerequisites

```bash
# 1. Deploy the upgraded Phoenix to devnet
~/.local/share/solana/install/releases/3.1.6/solana-release/bin/cargo-build-sbf
solana config set --url devnet
solana program deploy target/deploy/phoenix.so
# → note the deployed program ID

# 2. Make sure your wallet has >= 2 SOL on devnet
solana airdrop 2

# 3. Install deps
yarn install   # (or npm install)
```

## Run

```bash
export ANCHOR_PROVIDER_URL=https://api.devnet.solana.com
export ANCHOR_WALLET=~/.config/solana/id.json
export PHOENIX_PROGRAM_ID=<your_deployed_program_id>

yarn test
```

Optional overrides:
```bash
export ROUTER_ENDPOINT=https://devnet-router.magicblock.app
export ROUTER_WS_ENDPOINT=wss://devnet-router.magicblock.app
```

## What runs

10 sequential `it()` steps, each printing its tx signature:

| # | Step | Layer | What it tests |
|---|---|---|---|
| 1 | `setup — create mints + fund trader ATAs` | base | SPL infrastructure |
| 2 | `initializes the market as a PDA` | base | `InitializeMarket` PDA flow (program self-allocates) |
| 3 | `requests a seat` | base | `RequestSeat` |
| 4 | `approves the seat` | base | `ChangeSeatStatus` |
| 5 | `deposits funds on base layer` | base | `DepositFunds` (legacy / pre-delegation) |
| 6 | `delegates the market to the ER` | base | `DelegateMarket` + verifies owner flip to delegation program |
| 7 | `delegates the trader seat to the ER` | base | `DelegateSeat` + verifies seat owner flip |
| 8 | `ER trading smoke test` | ER (via Router) | `CancelAllOrdersWithFreeFunds` on delegated market |
| 9 | `Magic Action deposit` | base + ER + base (auto) | `RequestDeposit` (user sig) → auto `ProcessDepositEr` → auto `CloseDepositReceipt` |
| 10 | `Magic Action withdraw` | base + ER + base (auto) | `RequestWithdrawal` (user sig) → auto `ProcessWithdrawalEr` → auto `ExecuteWithdrawalBaseChain` (SPL transfer back to user) |

Steps 9 and 10 use `GetCommitmentSignature` from `@magicblock-labs/ephemeral-rollups-sdk` to block until the post-undelegate action lands on the base layer, and assert that the receipt PDA has been closed and (for withdraw) the user's ATA balance grew.

## Sample output

```
  phoenix-er
Program ID:     <your_program_id>
Admin:          <your_wallet>
Base RPC:       https://api.devnet.solana.com
Router RPC:     https://devnet-router.magicblock.app
    ✔ setup — create mints + fund trader ATAs
  create mints tx:     5Lq…ABC
    baseMint:           4Hx…
    quoteMint:          7Ky…
  fund ATAs tx:        2Pm…XYZ
    ✔ initializes the market as a PDA
  initialize tx:       3Vn…QRS
    market PDA:          B7w…
    ✔ Magic Action deposit
  request deposit tx:  9Tr…JKL
  settlement sig:      6Wu…MNO
    ✔ Magic Action withdraw
  request withdraw tx: 4Xs…PQR
  settlement sig:      8Ye…STU
  pre-withdraw base ATA:  999900000000
  post-withdraw base ATA: 1000000000000  ← vault paid out via Magic Action
```

## Troubleshooting

- **`IllegalOwner`** on init: you're hitting the upstream Phoenix at `PhoeNiXZ8…`. Set `PHOENIX_PROGRAM_ID` to your deployed copy.
- **`InsufficientFunds`** on withdraw: deposit more in steps 5 or 9.
- **RPC rate-limited**: use a higher-tier endpoint via `ANCHOR_PROVIDER_URL`.
