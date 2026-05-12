use crate::{
    program::{
        dispatch_market::{get_market_size, load_with_dispatch_init},
        error::{assert_with_msg, PhoenixError},
        loaders::{get_vault_address, InitializeMarketContext},
        system_utils::create_account,
        MarketHeader, MarketSizeParams, PhoenixMarketContext, TokenParams,
    },
    quantities::{
        BaseAtomsPerBaseUnit, BaseLotsPerBaseUnit, QuoteAtomsPerQuoteUnit,
        QuoteLotsPerBaseUnitPerTick, QuoteLotsPerQuoteUnit, WrapperU64,
    },
};
use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program::{invoke, invoke_signed},
    program_error::ProgramError, program_pack::Pack, pubkey::Pubkey, rent::Rent,
    system_instruction, system_program, sysvar::Sysvar,
};
use std::{mem::size_of, ops::DerefMut};

/// PDA seed prefix for the market account. The full seed list is:
/// `[MARKET_SEED_PREFIX, base_mint, quote_mint, market_creator]`.
///
/// Markets allocated as PDAs with these seeds can be delegated to the
/// MagicBlock Ephemeral Rollup. Legacy keypair-allocated markets remain
/// supported (they cannot be delegated).
pub const MARKET_SEED_PREFIX: &[u8] = b"market";

/// Derives the canonical market PDA for a given (base_mint, quote_mint,
/// market_creator) triple.
pub fn find_market_address(
    base_mint: &Pubkey,
    quote_mint: &Pubkey,
    market_creator: &Pubkey,
    program_id: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            MARKET_SEED_PREFIX,
            base_mint.as_ref(),
            quote_mint.as_ref(),
            market_creator.as_ref(),
        ],
        program_id,
    )
}

/// Allocates the market account as a PDA when the supplied account is still
/// system-owned (uninit). No-op when the caller pre-allocated the account
/// themselves (legacy keypair flow).
///
/// Must be called in the dispatch path *before* any code reads the market
/// header (e.g. `PhoenixMarketContext::load_init`, `EventRecorder::new`),
/// because those expect the account to be Phoenix-owned with zero-initialized
/// data.
///
/// Accounts (program_accounts[2..], accounts[..]):
///   program_accounts[2]: market   (system-owned uninit OR Phoenix-owned)
///   program_accounts[3]: market_creator (signer, payer)
///   accounts[0]: base_mint
///   accounts[1]: quote_mint
///   accounts[2]: base_vault    (unused here)
///   accounts[3]: quote_vault   (unused here)
///   accounts[4]: system_program
///   accounts[5]: token_program (unused here)
pub(crate) fn maybe_allocate_market_pda<'a, 'info>(
    program_id: &Pubkey,
    market_info: &'a AccountInfo<'info>,
    market_creator: &'a AccountInfo<'info>,
    accounts: &'a [AccountInfo<'info>],
    data: &[u8],
) -> ProgramResult {
    // Already pre-allocated and owned by Phoenix (legacy keypair flow); no-op.
    if market_info.owner == &crate::ID {
        return Ok(());
    }

    // Anything other than system-owned + empty is an error here.
    assert_with_msg(
        market_info.owner == &system_program::ID && market_info.data_is_empty(),
        ProgramError::IllegalOwner,
        "Market account must be system-owned + uninit for PDA allocation, or pre-allocated by Phoenix",
    )?;
    assert_with_msg(
        market_creator.is_signer,
        ProgramError::MissingRequiredSignature,
        "market_creator must sign for market PDA allocation",
    )?;

    let ctx = InitializeMarketContext::load(accounts)?;
    let params = InitializeParams::try_from_slice(data)?;

    let (expected_market, bump) = find_market_address(
        ctx.base_mint.as_ref().key,
        ctx.quote_mint.as_ref().key,
        market_creator.key,
        program_id,
    );
    assert_with_msg(
        market_info.key == &expected_market,
        ProgramError::InvalidSeeds,
        "Market account key does not match expected PDA",
    )?;

    let space = size_of::<MarketHeader>() + get_market_size(&params.market_size_params)?;
    let rent = Rent::get()?;
    let lamports = rent.minimum_balance(space);

    let bump_slice: &[u8] = &[bump];
    let base_mint_key = ctx.base_mint.as_ref().key;
    let quote_mint_key = ctx.quote_mint.as_ref().key;
    let signer_seeds: &[&[u8]] = &[
        MARKET_SEED_PREFIX,
        base_mint_key.as_ref(),
        quote_mint_key.as_ref(),
        market_creator.key.as_ref(),
        bump_slice,
    ];

    // SystemProgram::create_account via CPI caps allocation at 10240 bytes
    // (MAX_PERMITTED_DATA_INCREASE) per top-level instruction. The
    // smallest supported Phoenix market config (8×8×4) is well under
    // that; the standard config (512×512×128) is ~80 KB and requires
    // the legacy keypair-allocated flow.
    const MAX_PERMITTED_DATA_INCREASE: usize = 10_240;
    assert_with_msg(
        space <= MAX_PERMITTED_DATA_INCREASE,
        ProgramError::InvalidArgument,
        "Market size > 10240 bytes can't be PDA-allocated in one ix; use the legacy keypair flow",
    )?;
    invoke_signed(
        &system_instruction::create_account(
            market_creator.key,
            market_info.key,
            lamports,
            space as u64,
            program_id,
        ),
        &[
            market_creator.clone(),
            market_info.clone(),
            ctx.system_program.as_ref().clone(),
        ],
        &[signer_seeds],
    )?;

    Ok(())
}

#[derive(BorshDeserialize, BorshSerialize)]
pub struct InitializeParams {
    /// These parameters define the number of orders on each side of the market as well as the maximum
    /// number of supported traders. They are used to deserialize the market state (see `dispatch_market.rs`).
    pub market_size_params: MarketSizeParams,

    /// Number of quote lots to make up a full quote unit. Quote lots are the smallest measurement for
    /// the quote currency that can be processed by the market. 1 "unit" is the standard measure of a currency
    /// (e.g. 1 US Dollar, 1 Euro, or 1 BTC).
    ///
    /// Assume the quote mint is USDC.
    /// If num_quote_lots_per_quote_unit is equal to 10000, this means that the smallest unit that the exchange
    /// can process is $0.0001. Because USDC has 6 decimals, this means the equivalent quote_lot_size (quote atoms per quote lot)
    /// is equal to 1e6 / 10000 = 100.
    pub num_quote_lots_per_quote_unit: u64,

    /// Tick size, in quote lots per base units. A tick is the smallest price increment for a market.
    ///
    /// Assume the quote mint is USDC and num_quote_lots_per_quote_unit is equal to 10000 (quote_lot_size = 100).
    /// If tick size is equal to $0.01 (10000 atoms), this means that tick_size_in_quote_lots_per_base_unit is equal to
    /// tick_size / quote_lot_size = 10000 / 100 = 100.
    pub tick_size_in_quote_lots_per_base_unit: u64,

    /// Number of base lots to make up a full base unit. Base lots are the smallest measurement for
    /// the base currency that can be processed by the market.
    ///
    /// Assume the base mint is SOL.
    /// If num_base_lots_per_base_unit is equal to 1000, this means that the smallest unit that the exchange
    /// can process is 0.0001 SOL. Because SOL has 9 decimals, this means the equivalent base_lot_size is equal
    /// to 1e9 / 1000 = 1e6.
    pub num_base_lots_per_base_unit: u64,

    /// Market fee charged to takers, in basis points (0.01%). This fee is charged on the quote currency.
    pub taker_fee_bps: u16,

    /// The Pubkey of the account that will receive fees for this market.
    pub fee_collector: Pubkey,

    /// 1 raw base unit is defined as 10^base_mint_decimals atoms.
    /// By default, raw_base_units_per_base_unit is set to 1 (if the Option is passed in as `None`).
    /// It is highly recommended to be a power of 10.
    ///
    /// If this parameter is supplied, the market will treat the number of base atoms in a base unit as
    /// `(10^base_mint_decimals) * raw_base_units_per_base_unit`.
    pub raw_base_units_per_base_unit: Option<u32>,
}

pub(crate) fn process_initialize_market<'a, 'info>(
    _program_id: &Pubkey,
    market_context: &PhoenixMarketContext<'a, 'info>,
    accounts: &'a [AccountInfo<'info>],
    data: &[u8],
) -> ProgramResult {
    let PhoenixMarketContext {
        market_info,
        signer: market_creator,
    } = market_context;
    let InitializeMarketContext {
        base_mint,
        quote_mint,
        base_vault,
        quote_vault,
        system_program,
        token_program,
        ..
    } = InitializeMarketContext::load(accounts)?;

    let InitializeParams {
        market_size_params,
        tick_size_in_quote_lots_per_base_unit,
        num_quote_lots_per_quote_unit,
        num_base_lots_per_base_unit,
        taker_fee_bps,
        fee_collector,
        raw_base_units_per_base_unit,
    } = InitializeParams::try_from_slice(data)?;

    let tick_size_in_quote_lots_per_base_unit =
        QuoteLotsPerBaseUnitPerTick::new(tick_size_in_quote_lots_per_base_unit);
    let num_quote_lots_per_quote_unit = QuoteLotsPerQuoteUnit::new(num_quote_lots_per_quote_unit);
    let num_base_lots_per_base_unit = BaseLotsPerBaseUnit::new(num_base_lots_per_base_unit);
    assert_with_msg(
        taker_fee_bps <= 10000,
        ProgramError::InvalidInstructionData,
        "Taker fee must be less than or equal to 10000 basis points (100%)",
    )?;

    let base_atoms_per_base_unit = BaseAtomsPerBaseUnit::new(
        10u64.pow(base_mint.decimals as u32) * raw_base_units_per_base_unit.unwrap_or(1) as u64,
    );
    let quote_atoms_per_quote_unit =
        QuoteAtomsPerQuoteUnit::new(10u64.pow(quote_mint.decimals as u32));

    assert_with_msg(
        base_atoms_per_base_unit % num_base_lots_per_base_unit == 0,
        PhoenixError::InvalidLotSize,
        &format!(
            "Base lots per base unit ({}) must be a factor of base atoms per base unit ({})",
            num_base_lots_per_base_unit, base_atoms_per_base_unit
        ),
    )?;
    assert_with_msg(
        quote_atoms_per_quote_unit % num_quote_lots_per_quote_unit == 0,
        PhoenixError::InvalidLotSize,
        &format!(
            "Quote lots per quote unit ({}) must be a factor of quote atoms per quote unit ({})",
            num_quote_lots_per_quote_unit, quote_atoms_per_quote_unit
        ),
    )?;

    let quote_lot_size = quote_atoms_per_quote_unit / num_quote_lots_per_quote_unit;
    let tick_size_in_quote_atoms_per_base_unit =
        quote_lot_size * tick_size_in_quote_lots_per_base_unit;

    phoenix_log!(
        "Market parameters:
        num_quote_lots_per_quote_unit: {}, 
        tick_size_in_quote_lots_per_base_unit: {}, 
        num_base_lots_per_base_unit: {},
        tick_size_in_quote_atoms_per_base_unit: {},",
        num_quote_lots_per_quote_unit,
        tick_size_in_quote_lots_per_base_unit,
        num_base_lots_per_base_unit,
        tick_size_in_quote_atoms_per_base_unit,
    );
    // A trade of 1 base lot at the minimum tick price of 1 must result in an integer number of quote lots
    // Suppose there are T quote lots per tick and there are B base lots per base unit.
    // At a price of 1 tick per base unit, for a trade of size 1 base lot, the resulting quote lots N must be an integer
    // T (quote lots/tick) * 1 (tick/base unit) * 1/B (base units/base lots) * 1 (base lots) = N (quote lots)
    // T/B  = N => B | T (B divides T)
    assert_with_msg(
        tick_size_in_quote_lots_per_base_unit % num_base_lots_per_base_unit == 0,
        ProgramError::InvalidInstructionData,
        "The number of quote lots per tick be a multiple of the number of base lots per base unit",
    )?;

    // Create the base and quote vaults of this market
    let rent = Rent::get()?;
    let mut bumps = vec![];
    for (token_account, mint) in [
        (base_vault.as_ref(), base_mint.as_ref()),
        (quote_vault.as_ref(), quote_mint.as_ref()),
    ] {
        let (vault_key, bump) = get_vault_address(market_info.key, mint.key);
        assert_with_msg(
            vault_key == *token_account.key,
            PhoenixError::InvalidMarketSigner,
            &format!(
                "Supplied vault ({}) does not match computed key ({})",
                token_account.key, vault_key
            ),
        )?;
        let space = spl_token::state::Account::LEN;
        let seeds = vec![
            b"vault".to_vec(),
            market_info.key.as_ref().to_vec(),
            mint.key.as_ref().to_vec(),
            vec![bump],
        ];
        create_account(
            market_creator.as_ref(),
            token_account,
            system_program.as_ref(),
            &spl_token::id(),
            &rent,
            space as u64,
            seeds,
        )?;
        invoke(
            &spl_token::instruction::initialize_account3(
                &spl_token::id(),
                token_account.key,
                mint.key,
                token_account.key,
            )?,
            &[
                market_creator.as_ref().clone(),
                token_account.clone(),
                mint.clone(),
                token_program.as_ref().clone(),
            ],
        )?;
        bumps.push(bump);
    }

    // Setup the initial market state
    {
        let market_bytes = &mut market_info.try_borrow_mut_data()?[size_of::<MarketHeader>()..];
        let market = load_with_dispatch_init(&market_size_params, market_bytes)?.inner;
        assert_with_msg(
            market.get_sequence_number() == 0,
            PhoenixError::MarketAlreadyInitialized,
            "Market must have a sequence number of 0",
        )?;

        market.initialize_with_params(
            tick_size_in_quote_lots_per_base_unit,
            num_base_lots_per_base_unit,
        );
        market.set_fee(taker_fee_bps as u64);
    }

    // Derive the canonical market PDA for the (base, quote, creator) triple.
    // If the market account matches that PDA, the market was allocated by
    // `maybe_allocate_market_pda` and is delegatable; persist its bump in
    // the header so MagicBlock delegation CPIs can sign with the correct
    // seeds. If the market is keypair-allocated (legacy), market_bump stays
    // 0 and the delegation entrypoint will refuse to sign for it.
    let (expected_market_pda, derived_bump) = find_market_address(
        base_mint.as_ref().key,
        quote_mint.as_ref().key,
        market_creator.key,
        _program_id,
    );
    let market_bump = if market_info.key == &expected_market_pda {
        derived_bump
    } else {
        0
    };

    // Populate the header data
    let mut header = market_info.get_header_mut()?;
    // All markets are initialized with a status of `PostOnly`
    *header.deref_mut() = MarketHeader::new(
        market_size_params,
        TokenParams {
            vault_bump: bumps[0] as u32,
            decimals: base_mint.decimals as u32,
            mint_key: *base_mint.as_ref().key,
            vault_key: *base_vault.key,
        },
        base_atoms_per_base_unit / num_base_lots_per_base_unit,
        TokenParams {
            vault_bump: bumps[1] as u32,
            decimals: quote_mint.decimals as u32,
            mint_key: *quote_mint.as_ref().key,
            vault_key: *quote_vault.key,
        },
        quote_lot_size,
        tick_size_in_quote_atoms_per_base_unit,
        *market_creator.key,
        *market_creator.key,
        fee_collector,
        raw_base_units_per_base_unit.unwrap_or(1),
        market_bump,
    );

    drop(header);
    Ok(())
}
