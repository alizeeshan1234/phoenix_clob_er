use num_enum::TryFromPrimitive;
use shank::ShankInstruction;

#[repr(u8)]
#[derive(TryFromPrimitive, Debug, Copy, Clone, ShankInstruction, PartialEq, Eq)]
#[rustfmt::skip]
pub enum PhoenixInstruction {
    // Market instructions
    /// Send a swap (no limit orders allowed) order
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    #[account(4, writable, name = "base_account", desc = "Trader base token account")]
    #[account(5, writable, name = "quote_account", desc = "Trader quote token account")]
    #[account(6, writable, name = "base_vault", desc = "Base vault PDA, seeds are [b'vault', market_address, base_mint_address]")]
    #[account(7, writable, name = "quote_vault", desc = "Quote vault PDA, seeds are [b'vault', market_address, quote_mint_address]")]
    #[account(8, name = "token_program", desc = "Token program")]
    Swap = 0,

    /// Send a swap (no limit orders allowed) order using only deposited funds
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    #[account(4, name = "seat")]
    SwapWithFreeFunds = 1,

    /// Place a limit order on the book. The order can cross if the supplied order type is Limit
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    #[account(4, name = "seat")]
    #[account(5, writable, name = "base_account", desc = "Trader base token account")]
    #[account(6, writable, name = "quote_account", desc = "Trader quote token account")]
    #[account(7, writable, name = "base_vault", desc = "Base vault PDA, seeds are [b'vault', market_address, base_mint_address]")]
    #[account(8, writable, name = "quote_vault", desc = "Quote vault PDA, seeds are [b'vault', market_address, quote_mint_address]")]
    #[account(9, name = "token_program", desc = "Token program")]
    PlaceLimitOrder = 2,

    /// Place a limit order on the book using only deposited funds.
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    #[account(4, name = "seat")]
    PlaceLimitOrderWithFreeFunds = 3,

    /// Reduce the size of an existing order on the book 
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    #[account(4, writable, name = "base_account", desc = "Trader base token account")]
    #[account(5, writable, name = "quote_account", desc = "Trader quote token account")]
    #[account(6, writable, name = "base_vault", desc = "Base vault PDA, seeds are [b'vault', market_address, base_mint_address]")]
    #[account(7, writable, name = "quote_vault", desc = "Quote vault PDA, seeds are [b'vault', market_address, quote_mint_address]")]
    #[account(8, name = "token_program", desc = "Token program")]
    ReduceOrder = 4,

    /// Reduce the size of an existing order on the book 
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, writable, signer, name = "trader")]
    ReduceOrderWithFreeFunds = 5,


    /// Cancel all orders 
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    #[account(4, writable, name = "base_account", desc = "Trader base token account")]
    #[account(5, writable, name = "quote_account", desc = "Trader quote token account")]
    #[account(6, writable, name = "base_vault", desc = "Base vault PDA, seeds are [b'vault', market_address, base_mint_address]")]
    #[account(7, writable, name = "quote_vault", desc = "Quote vault PDA, seeds are [b'vault', market_address, quote_mint_address]")]
    #[account(8, name = "token_program", desc = "Token program")]
    CancelAllOrders = 6,

    /// Cancel all orders (no token transfers) 
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    CancelAllOrdersWithFreeFunds = 7,

    /// Cancel all orders more aggressive than a specified price
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    #[account(4, writable, name = "base_account", desc = "Trader base token account")]
    #[account(5, writable, name = "quote_account", desc = "Trader quote token account")]
    #[account(6, writable, name = "base_vault", desc = "Base vault PDA, seeds are [b'vault', market_address, base_mint_address]")]
    #[account(7, writable, name = "quote_vault", desc = "Quote vault PDA, seeds are [b'vault', market_address, quote_mint_address]")]
    #[account(8, name = "token_program", desc = "Token program")]
    CancelUpTo = 8,


    /// Cancel all orders more aggressive than a specified price (no token transfers) 
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    CancelUpToWithFreeFunds = 9,

    /// Cancel multiple orders by ID 
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    #[account(4, writable, name = "base_account", desc = "Trader base token account")]
    #[account(5, writable, name = "quote_account", desc = "Trader quote token account")]
    #[account(6, writable, name = "base_vault", desc = "Base vault PDA, seeds are [b'vault', market_address, base_mint_address]")]
    #[account(7, writable, name = "quote_vault", desc = "Quote vault PDA, seeds are [b'vault', market_address, quote_mint_address]")]
    #[account(8, name = "token_program", desc = "Token program")]
    CancelMultipleOrdersById = 10,

    /// Cancel multiple orders by ID (no token transfers) 
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    CancelMultipleOrdersByIdWithFreeFunds = 11,

    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    #[account(4, writable, name = "base_account", desc = "Trader base token account")]
    #[account(5, writable, name = "quote_account", desc = "Trader quote token account")]
    #[account(6, writable, name = "base_vault", desc = "Base vault PDA, seeds are [b'vault', market_address, base_mint_address]")]
    #[account(7, writable, name = "quote_vault", desc = "Quote vault PDA, seeds are [b'vault', market_address, quote_mint_address]")]
    #[account(8, name = "token_program", desc = "Token program")]
    WithdrawFunds = 12,

    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    #[account(4, name = "seat")]
    #[account(5, writable, name = "base_account", desc = "Trader base token account")]
    #[account(6, writable, name = "quote_account", desc = "Trader quote token account")]
    #[account(7, writable, name = "base_vault", desc = "Base vault PDA, seeds are [b'vault', market_address, base_mint_address]")]
    #[account(8, writable, name = "quote_vault", desc = "Quote vault PDA, seeds are [b'vault', market_address, quote_mint_address]")]
    #[account(9, name = "token_program", desc = "Token program")]
    DepositFunds = 13,

    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, writable, signer, name = "payer")]
    #[account(4, writable, name = "seat")]
    #[account(5, name = "system_program", desc = "System program")]
    RequestSeat = 14,

    #[account(0, signer, name = "log_authority", desc = "Log authority")]
    Log = 15,

    /// Place multiple post only orders on the book.
    /// Similar to single post only orders, these can either be set to be rejected or amended to top of book if they cross.
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    #[account(4, name = "seat")]
    #[account(5, writable, name = "base_account", desc = "Trader base token account")]
    #[account(6, writable, name = "quote_account", desc = "Trader quote token account")]
    #[account(7, writable, name = "base_vault", desc = "Base vault PDA, seeds are [b'vault', market_address, base_mint_address]")]
    #[account(8, writable, name = "quote_vault", desc = "Quote vault PDA, seeds are [b'vault', market_address, quote_mint_address]")]
    #[account(9, name = "token_program", desc = "Token program")]
    PlaceMultiplePostOnlyOrders = 16,
        
    /// Place multiple post only orders on the book using only deposited funds.
    /// Similar to single post only orders, these can either be set to be rejected or amended to top of book if they cross.
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "trader")]
    #[account(4, name = "seat")]
    PlaceMultiplePostOnlyOrdersWithFreeFunds = 17,


    // Admin instructions
    /// Create a market 
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, writable, signer, name = "market_creator", desc = "The market_creator account must sign for the creation of new vaults")]
    #[account(4, name = "base_mint", desc = "Base mint account")]
    #[account(5, name = "quote_mint", desc = "Quote mint account")]
    #[account(6, writable, name = "base_vault", desc = "Base vault PDA, seeds are [b'vault', market_address, base_mint_address]")]
    #[account(7, writable, name = "quote_vault", desc = "Quote vault PDA, seeds are [b'vault', market_address, quote_mint_address]")]
    #[account(8, name = "system_program", desc = "System program")]
    #[account(9, name = "token_program", desc = "Token program")]
    InitializeMarket = 100,

    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "successor", desc = "The successor account must sign to claim authority")]
    ClaimAuthority = 101,

    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "market_authority", desc = "The market_authority account must sign to name successor")]
    NameSuccessor = 102,

    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "market_authority", desc = "The market_authority account must sign to change market status")]
    ChangeMarketStatus = 103,

    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "market_authority", desc = "The market_authority account must sign to change seat status")]
    #[account(4, writable, name = "seat")]
    ChangeSeatStatus = 104,

    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "market_authority", desc = "The market_authority account must sign to request a seat on behalf of a trader")]
    #[account(4, writable, signer, name = "payer")]
    #[account(5, name = "trader")]
    #[account(6, writable, name = "seat")]
    #[account(7, name = "system_program", desc = "System program")]
    RequestSeatAuthorized = 105,

    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "market_authority", desc = "The market_authority account must sign to evict a seat")]
    #[account(4, name = "trader")]
    #[account(5, name = "seat", desc = "The trader's PDA seat account, seeds are [b'seat', market_address, trader_address]")]
    #[account(6, writable, name = "base_account")]
    #[account(7, writable, name = "quote_account")]
    #[account(8, writable, name = "base_vault")]
    #[account(9, writable, name = "quote_vault")]
    #[account(10, name = "token_program", desc = "Token program")]
    EvictSeat = 106,

    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "market_authority", desc = "The market_authority account must sign to claim authority")]
    #[account(4, name = "trader")]
    #[account(5, name = "seat", desc = "The trader's PDA seat account, seeds are [b'seat', market_address, trader_address]")]
    #[account(6, writable, name = "base_account", desc = "Trader base token account")]
    #[account(7, writable, name = "quote_account", desc = "Trader quote token account")]
    #[account(8, writable, name = "base_vault", desc = "Base vault PDA, seeds are [b'vault', market_address, base_mint_address]")]
    #[account(9, writable, name = "quote_vault", desc = "Quote vault PDA, seeds are [b'vault', market_address, quote_mint_address]")]
    #[account(10, name = "token_program", desc = "Token program")]
    ForceCancelOrders = 107,

    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "sweeper", desc = "Signer of collect fees instruction")]
    #[account(4, writable, name = "fee_recipient", desc = "Fee collector quote token account")]
    #[account(5, writable, name = "quote_vault", desc = "Quote vault PDA, seeds are [b'vault', market_address, quote_mint_address]")]
    #[account(6, name = "token_program", desc = "Token program")]
    CollectFees = 108,

    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "This account holds the market state")]
    #[account(3, signer, name = "market_authority", desc = "The market_authority account must sign to change the free recipient")]
    #[account(4, name = "new_fee_recipient", desc = "New fee recipient")]
    ChangeFeeRecipient = 109,

    // MagicBlock Ephemeral Rollup integration. These ixs do NOT follow
    // Phoenix's standard "first 4 accounts are [program, log_authority,
    // market, signer]" convention; they're dispatched separately in
    // `process_instruction`.

    /// Delegate the market PDA to the MagicBlock delegation program so it
    /// can be mutated on an Ephemeral Rollup. Base-layer-only.
    #[account(0, writable, signer, name = "authority", desc = "Market authority, payer for delegation buffer")]
    #[account(1, name = "system_program", desc = "System program")]
    #[account(2, writable, name = "market", desc = "Market PDA to delegate")]
    #[account(3, name = "owner_program", desc = "Phoenix program (= current owner of the market)")]
    #[account(4, writable, name = "delegation_buffer", desc = "Buffer PDA used during delegation, seeds [b'buffer', market]")]
    #[account(5, writable, name = "delegation_record", desc = "Delegation record account (owned by delegation program)")]
    #[account(6, writable, name = "delegation_metadata", desc = "Delegation metadata account (owned by delegation program)")]
    #[account(7, name = "delegation_program", desc = "MagicBlock delegation program")]
    DelegateMarket = 200,

    /// Snapshot the delegated market's state back to the base layer; the
    /// market stays delegated. ER-only.
    #[account(0, writable, signer, name = "payer", desc = "Pays for the commit")]
    #[account(1, writable, name = "market", desc = "Delegated market PDA")]
    #[account(2, name = "magic_program", desc = "MagicBlock program (Magic111...)")]
    #[account(3, writable, name = "magic_context", desc = "MagicBlock context (MagicContext1...)")]
    CommitMarket = 201,

    /// Snapshot final state AND queue undelegation. After this lands on the
    /// ER, the delegation program will call back into Phoenix on the base
    /// layer with the `EXTERNAL_UNDELEGATE_DISCRIMINATOR` to finalize
    /// ownership transfer. ER-only.
    #[account(0, writable, signer, name = "payer", desc = "Pays for the commit")]
    #[account(1, writable, name = "market", desc = "Delegated market PDA")]
    #[account(2, name = "magic_program", desc = "MagicBlock program (Magic111...)")]
    #[account(3, writable, name = "magic_context", desc = "MagicBlock context (MagicContext1...)")]
    CommitAndUndelegateMarket = 202,

    /// Delegate a Seat PDA to the MagicBlock delegation program so trading
    /// instructions that require seat validation (PlaceLimitOrder*,
    /// SwapWithFreeFunds, PlaceMultiplePostOnly*) can run on the ER.
    /// Base-layer-only.
    #[account(0, writable, signer, name = "authority", desc = "Market authority, payer for delegation buffer")]
    #[account(1, name = "system_program", desc = "System program")]
    #[account(2, name = "market", desc = "Market this seat belongs to (read-only here)")]
    #[account(3, writable, name = "seat", desc = "Seat PDA to delegate, seeds [b'seat', market, trader]")]
    #[account(4, name = "trader", desc = "Trader pubkey (read-only) used to derive the seat PDA")]
    #[account(5, name = "owner_program", desc = "Phoenix program (= current owner of the seat)")]
    #[account(6, writable, name = "delegation_buffer", desc = "Buffer PDA used during delegation, seeds [b'buffer', seat]")]
    #[account(7, writable, name = "delegation_record", desc = "Delegation record account (owned by delegation program)")]
    #[account(8, writable, name = "delegation_metadata", desc = "Delegation metadata account (owned by delegation program)")]
    #[account(9, name = "delegation_program", desc = "MagicBlock delegation program")]
    DelegateSeat = 203,

    /// Receipt-PDA deposit, step 1 of 3 (base layer, user-signed).
    /// SPL transfer wallet -> vault, create DepositReceipt PDA, delegate
    /// receipt to ER. Used when the market is already delegated.
    #[account(0, writable, signer, name = "trader", desc = "Trader (signer, payer)")]
    #[account(1, name = "system_program", desc = "System program")]
    #[account(2, writable, name = "market", desc = "Delegated market (read-only data, used to validate vault/mint)")]
    #[account(3, writable, name = "base_account", desc = "Trader base token account (SPL source)")]
    #[account(4, writable, name = "quote_account", desc = "Trader quote token account (SPL source)")]
    #[account(5, writable, name = "base_vault", desc = "Market base vault (SPL destination)")]
    #[account(6, writable, name = "quote_vault", desc = "Market quote vault (SPL destination)")]
    #[account(7, name = "token_program", desc = "SPL Token program")]
    #[account(8, writable, name = "receipt", desc = "DepositReceipt PDA (empty, will be created)")]
    #[account(9, name = "owner_program", desc = "Phoenix program (= future owner of the receipt)")]
    #[account(10, writable, name = "delegation_buffer", desc = "Buffer PDA for delegation")]
    #[account(11, writable, name = "delegation_record", desc = "Delegation record account")]
    #[account(12, writable, name = "delegation_metadata", desc = "Delegation metadata account")]
    #[account(13, name = "delegation_program", desc = "MagicBlock delegation program")]
    RequestDeposit = 204,

    /// Receipt-PDA deposit, step 2 of 3 (ER, validator- or keeper-signed).
    /// Reads the delegated receipt, credits the trader's TraderState inside
    /// the delegated market, marks receipt processed, and CPIs
    /// commit_and_undelegate so the receipt returns to base layer.
    #[account(0, writable, signer, name = "payer", desc = "Pays for the commit; not necessarily the trader")]
    #[account(1, writable, name = "market", desc = "Delegated market")]
    #[account(2, writable, name = "receipt", desc = "Delegated DepositReceipt PDA")]
    #[account(3, name = "magic_program", desc = "MagicBlock program (Magic111...)")]
    #[account(4, writable, name = "magic_context", desc = "MagicBlock context")]
    ProcessDepositEr = 205,

    /// Receipt-PDA deposit, step 3 of 3 (base layer, auto-fired post-undelegate).
    /// Closes a processed deposit receipt and refunds rent to the trader.
    /// Trader is not a signer; pubkey is verified against receipt.trader.
    #[account(0, writable, name = "trader", desc = "Trader (lamport destination)")]
    #[account(1, writable, name = "receipt", desc = "Processed DepositReceipt PDA")]
    CloseDepositReceipt = 206,

    /// Receipt-PDA withdrawal, step 1 of 3 (base layer, user-signed).
    /// Creates the WithdrawalReceipt PDA, delegates it with a
    /// post-delegation action that auto-fires `ProcessWithdrawalEr` on
    /// the ER. The vault/user-token accounts are forwarded into both
    /// the post-delegation action and the eventual post-undelegate
    /// `ExecuteWithdrawalBaseChain` settlement so the SPL transfer at
    /// step 3 has everything it needs.
    #[account(0, writable, signer, name = "trader", desc = "Trader (signer, payer)")]
    #[account(1, name = "system_program", desc = "System program")]
    #[account(2, writable, name = "receipt", desc = "WithdrawalReceipt PDA (empty, will be created)")]
    #[account(3, name = "market", desc = "Market (read-only; identifies the receipt)")]
    #[account(4, writable, name = "base_account", desc = "Trader base token account (forwarded to step 3)")]
    #[account(5, writable, name = "quote_account", desc = "Trader quote token account (forwarded to step 3)")]
    #[account(6, writable, name = "base_vault", desc = "Market base vault (forwarded to step 3)")]
    #[account(7, writable, name = "quote_vault", desc = "Market quote vault (forwarded to step 3)")]
    #[account(8, name = "token_program", desc = "SPL Token program (forwarded to step 3)")]
    #[account(9, name = "owner_program", desc = "Phoenix program")]
    #[account(10, writable, name = "delegation_buffer", desc = "Buffer PDA for delegation")]
    #[account(11, writable, name = "delegation_record", desc = "Delegation record account")]
    #[account(12, writable, name = "delegation_metadata", desc = "Delegation metadata account")]
    #[account(13, name = "delegation_program", desc = "MagicBlock delegation program")]
    #[account(14, name = "magic_program", desc = "MagicBlock program (Magic111...)")]
    #[account(15, writable, name = "magic_context", desc = "MagicBlock context (forwarded into action)")]
    RequestWithdrawal = 207,

    /// Receipt-PDA withdrawal, step 2 of 3 (ER, auto-fired by
    /// post-delegation action).
    #[account(0, writable, signer, name = "trader", desc = "Trader (signer via escrow chain)")]
    #[account(1, writable, name = "market", desc = "Delegated market")]
    #[account(2, writable, name = "receipt", desc = "Delegated WithdrawalReceipt PDA")]
    #[account(3, name = "magic_program", desc = "MagicBlock program (Magic111...)")]
    #[account(4, writable, name = "magic_context", desc = "MagicBlock context")]
    #[account(5, writable, name = "base_account", desc = "Trader base token account (forwarded to step 3)")]
    #[account(6, writable, name = "quote_account", desc = "Trader quote token account (forwarded to step 3)")]
    #[account(7, writable, name = "base_vault", desc = "Market base vault (forwarded to step 3)")]
    #[account(8, writable, name = "quote_vault", desc = "Market quote vault (forwarded to step 3)")]
    #[account(9, name = "token_program", desc = "SPL Token program (forwarded to step 3)")]
    ProcessWithdrawalEr = 208,

    /// Receipt-PDA withdrawal, step 3 of 3 (base layer, auto-fired post-undelegate).
    /// Verifies receipt is processed, SPL transfers vault → user_ata for
    /// the debited amounts, closes the receipt. Trader is NOT a signer.
    #[account(0, writable, name = "trader", desc = "Trader (lamport destination)")]
    #[account(1, name = "market", desc = "Market (read-only; provides mint + vault keys)")]
    #[account(2, writable, name = "base_account", desc = "Trader base token account (SPL destination)")]
    #[account(3, writable, name = "quote_account", desc = "Trader quote token account (SPL destination)")]
    #[account(4, writable, name = "base_vault", desc = "Market base vault (SPL source)")]
    #[account(5, writable, name = "quote_vault", desc = "Market quote vault (SPL source)")]
    #[account(6, name = "token_program", desc = "SPL Token program")]
    #[account(7, writable, name = "receipt", desc = "Processed WithdrawalReceipt PDA")]
    ExecuteWithdrawalBaseChain = 209,

    /// Create a `SessionToken` PDA so an ephemeral keypair can sign
    /// trading ixs on behalf of the owner. Base layer, owner-signed.
    #[account(0, writable, signer, name = "owner", desc = "Real trader; pays rent and signs")]
    #[account(1, writable, name = "session_token", desc = "SessionToken PDA, seeds [b'session', owner, session_signer]")]
    #[account(2, name = "system_program", desc = "System program")]
    CreateSessionToken = 210,

    /// Close a `SessionToken` PDA and refund rent to the owner. Base layer.
    #[account(0, writable, signer, name = "owner", desc = "Real trader; lamport destination")]
    #[account(1, writable, name = "session_token", desc = "SessionToken PDA")]
    RevokeSessionToken = 211,

    /// Place a limit order using a session token. The `session_signer`
    /// signs; the order is attributed to `owner` (from session_token).
    /// Runs on the ER like PlaceLimitOrderWithFreeFunds.
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "Delegated market")]
    #[account(3, signer, name = "session_signer", desc = "Ephemeral session keypair")]
    #[account(4, name = "owner", desc = "Real trader (from session_token)")]
    #[account(5, name = "session_token", desc = "SessionToken PDA proving session_signer can sign for owner")]
    #[account(6, name = "seat", desc = "Owner's seat PDA")]
    PlaceLimitOrderViaSession = 212,

    /// Swap (IOC) using a session token.
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "Delegated market")]
    #[account(3, signer, name = "session_signer", desc = "Ephemeral session keypair")]
    #[account(4, name = "owner", desc = "Real trader (from session_token)")]
    #[account(5, name = "session_token", desc = "SessionToken PDA")]
    #[account(6, name = "seat", desc = "Owner's seat PDA")]
    SwapViaSession = 213,

    /// Cancel all orders using a session token.
    #[account(0, name = "phoenix_program", desc = "Phoenix program")]
    #[account(1, name = "log_authority", desc = "Phoenix log authority")]
    #[account(2, writable, name = "market", desc = "Delegated market")]
    #[account(3, signer, name = "session_signer", desc = "Ephemeral session keypair")]
    #[account(4, name = "owner", desc = "Real trader (from session_token)")]
    #[account(5, name = "session_token", desc = "SessionToken PDA")]
    CancelAllOrdersViaSession = 214,
}

impl PhoenixInstruction {
    pub fn to_vec(&self) -> Vec<u8> {
        vec![*self as u8]
    }
}

#[test]
fn test_instruction_serialization() {
    for i in 0..=108 {
        let instruction = match PhoenixInstruction::try_from(i) {
            Ok(j) => j,
            Err(_) => {
                assert!(i < 100);
                // This needs to be changed if new instructions are added
                assert!(i > 17);
                continue;
            }
        };
        assert_eq!(instruction as u8, i);
    }
}
