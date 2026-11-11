//! Jupiter AMM adapter for Loadout's bonding curves.
//!
//! Every Loadout token trades on its own bonding curve (`loadout-core`'s `BondingCurve`
//! account) until the curve reaches its graduation target; the token then moves to a Meteora
//! DAMM v2 pool, which Jupiter already routes. Each curve is one market here: from the curve
//! account's project id the adapter derives every other account. Jupiter discovery and
//! execution integration are separate requirements before new launches become routable.
//!
//! Markets quote SOL -> token (`buy`) and token -> SOL (`sell`), exact-in, with the program's
//! own math (`loadout-math`). Both instructions move native SOL, not wrapped SOL: `buy` takes
//! lamports from the trader's wallet and `sell` pays lamports to it.

pub mod quote;
pub mod state;

use std::sync::{OnceLock, atomic::Ordering};

use jupiter_amm_interface::{
    AccountProvider, Amm, AmmContext, AmmError, AmmLabel, ClockRef, KeyedAccount, Quote,
    QuoteParams, SingleProgramAmm, Swap, SwapAndAccountMetas, SwapMode, SwapParams,
    single_program_amm,
};
use rust_decimal::Decimal;
use solana_account::ReadableAccount;
use solana_instruction::AccountMeta;
use solana_pubkey::{Pubkey, pubkey};

use crate::quote::Market;
use crate::state::{BondingCurve, FeeConfig, PROJECT_STATE_LIVE, Project};

pub const LOADOUT_PROGRAM_ID: Pubkey = pubkey!("8nPyghXuy35JwxZ8ANAzxMXKruibh6B3js4QMYBRFadx");
pub const NATIVE_MINT: Pubkey = pubkey!("So11111111111111111111111111111111111111112");
pub const TOKEN_PROGRAM_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const SYSTEM_PROGRAM_ID: Pubkey = pubkey!("11111111111111111111111111111111");
pub const LABEL: AmmLabel = "Loadout";

/// `sha256("global:buy")[..8]`.
pub const BUY_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
/// `sha256("global:sell")[..8]`.
pub const SELL_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

/// `Swap::Placeholder` data for a buy and a sell, until Jupiter adds Loadout's own variants.
pub const PLACEHOLDER_BUY: u8 = 0;
pub const PLACEHOLDER_SELL: u8 = 1;

/// Why a quote fails. The program-side variants carry the error `loadout-core` returns for the
/// same trade (`code()`), so a quote fails exactly when the instruction would.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuoteError {
    ProtocolPaused,
    MarketNotActive,
    ProjectNotLive,
    MathOverflow,
    ZeroAmount,
    InvariantViolation,
    FeeCalculationFailed,
    TradeFeeTooHigh,
    ProjectPaused,
    CurveComplete,
    InvalidAccount(String),
    Unsupported(String),
}

impl QuoteError {
    /// `loadout-core`'s custom error code for the same failure, if the program has one.
    pub fn code(&self) -> Option<u32> {
        Some(match self {
            Self::ProtocolPaused => 12001,
            Self::MarketNotActive => 12011,
            Self::ProjectNotLive => 12013,
            Self::MathOverflow => 12020,
            Self::ZeroAmount => 12021,
            Self::InvariantViolation => 12023,
            Self::FeeCalculationFailed => 12025,
            Self::TradeFeeTooHigh => 12220,
            Self::ProjectPaused => 12120,
            Self::CurveComplete => 12140,
            Self::InvalidAccount(_) | Self::Unsupported(_) => return None,
        })
    }
}

impl std::fmt::Display for QuoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAccount(why) => write!(f, "invalid account: {why}"),
            Self::Unsupported(why) => write!(f, "unsupported: {why}"),
            other => write!(f, "{other:?}"),
        }
    }
}

impl From<QuoteError> for AmmError {
    fn from(error: QuoteError) -> Self {
        AmmError::Custom(error.to_string())
    }
}

/// The `loadout-core` accounts of one project, all derived from its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectAccounts {
    pub fee_config: Pubkey,
    pub project: Pubkey,
    pub bonding_curve: Pubkey,
    pub token_mint: Pubkey,
    pub token_vault: Pubkey,
    pub vault_authority: Pubkey,
    pub project_treasury: Pubkey,
}

impl ProjectAccounts {
    pub fn derive(project_id: u64) -> Self {
        let id = project_id.to_le_bytes();
        let pda = |seed: &[u8]| Pubkey::find_program_address(&[seed, &id], &LOADOUT_PROGRAM_ID).0;
        Self {
            fee_config: Pubkey::find_program_address(&[b"fee_config"], &LOADOUT_PROGRAM_ID).0,
            project: pda(b"project"),
            bonding_curve: pda(b"curve"),
            token_mint: pda(b"token_mint"),
            token_vault: pda(b"token_vault"),
            vault_authority: pda(b"vault_authority"),
            project_treasury: pda(b"treasury"),
        }
    }
}

/// loadout-core's protocol fee shard count.
pub const PROTOCOL_FEE_SHARDS: u8 = 16;

/// The protocol fee shard a trader's `buy`/`sell` pays its protocol fee into: loadout-core's
/// `["protocol_fee_shard", shard]` PDA, where `shard` is the trader key's first byte modulo
/// `PROTOCOL_FEE_SHARDS` (the program checks it by seeds).
pub fn protocol_fee_shard(trader: &Pubkey) -> Pubkey {
    static SHARDS: OnceLock<[Pubkey; PROTOCOL_FEE_SHARDS as usize]> = OnceLock::new();
    let shards = SHARDS.get_or_init(|| {
        std::array::from_fn(|index| {
            Pubkey::find_program_address(
                &[b"protocol_fee_shard", &[index as u8]],
                &LOADOUT_PROGRAM_ID,
            )
            .0
        })
    });
    shards[usize::from(trader.to_bytes()[0] % PROTOCOL_FEE_SHARDS)]
}

/// The two per-trader accounts initialized on the first successful buy or sell.
/// The signing trader authority funds their rent, including on unreferred trades.
pub fn trader_accounts(trader: &Pubkey) -> (Pubkey, Pubkey) {
    let rebate = Pubkey::find_program_address(&[b"rebate", trader.as_ref()], &LOADOUT_PROGRAM_ID).0;
    let binding =
        Pubkey::find_program_address(&[b"referral_binding", trader.as_ref()], &LOADOUT_PROGRAM_ID)
            .0;
    (rebate, binding)
}

/// One Loadout bonding curve as a Jupiter market.
#[derive(Clone)]
pub struct LoadoutCurve {
    accounts: ProjectAccounts,
    project_id: u64,
    clock: ClockRef,
    curve: BondingCurve,
    project: Option<Project>,
    fee_config: Option<FeeConfig>,
}

single_program_amm!(LoadoutCurve, LOADOUT_PROGRAM_ID, LABEL);

impl LoadoutCurve {
    /// The state a quote needs, once `update` has loaded it.
    pub fn market(&self) -> Result<Market, QuoteError> {
        match (self.project, self.fee_config) {
            (Some(project), Some(fee_config)) => Ok(Market {
                curve: self.curve,
                project,
                fee_config,
            }),
            _ => Err(QuoteError::InvalidAccount("not updated yet".to_string())),
        }
    }

    pub fn accounts(&self) -> &ProjectAccounts {
        &self.accounts
    }

    pub fn project_id(&self) -> u64 {
        self.project_id
    }

    fn now(&self) -> i64 {
        self.clock.unix_timestamp.load(Ordering::Relaxed)
    }

    fn side(&self, input_mint: &Pubkey, output_mint: &Pubkey) -> Result<u8, QuoteError> {
        let token = self.accounts.token_mint;
        if *input_mint == NATIVE_MINT && *output_mint == token {
            Ok(PLACEHOLDER_BUY)
        } else if *input_mint == token && *output_mint == NATIVE_MINT {
            Ok(PLACEHOLDER_SELL)
        } else {
            Err(QuoteError::Unsupported(
                "mints aren't this curve's".to_string(),
            ))
        }
    }

    fn read<T>(
        provider: &impl AccountProvider,
        key: &Pubkey,
        decode: fn(&[u8]) -> Result<T, QuoteError>,
    ) -> Result<T, AmmError> {
        let account = provider.try_get(key)?;
        if *account.owner() != LOADOUT_PROGRAM_ID {
            return Err(QuoteError::InvalidAccount(format!("{key} isn't loadout-core's")).into());
        }
        Ok(decode(account.data())?)
    }
}

impl Amm for LoadoutCurve {
    fn from_keyed_account(
        keyed_account: &KeyedAccount,
        amm_context: &AmmContext,
    ) -> Result<Self, AmmError> {
        if keyed_account.account.owner != LOADOUT_PROGRAM_ID {
            return Err(QuoteError::InvalidAccount("not owned by loadout-core".to_string()).into());
        }
        let curve = BondingCurve::decode(&keyed_account.account.data)?;
        let accounts = ProjectAccounts::derive(curve.project_id);
        if accounts.bonding_curve != keyed_account.key {
            return Err(QuoteError::InvalidAccount("not the project's curve".to_string()).into());
        }
        Ok(Self {
            accounts,
            project_id: curve.project_id,
            clock: amm_context.clock_ref.clone(),
            curve,
            project: None,
            fee_config: None,
        })
    }

    fn label(&self) -> AmmLabel {
        LABEL
    }

    fn program_id(&self) -> Pubkey {
        LOADOUT_PROGRAM_ID
    }

    fn key(&self) -> Pubkey {
        self.accounts.bonding_curve
    }

    fn get_reserve_mints(&self) -> Vec<Pubkey> {
        vec![NATIVE_MINT, self.accounts.token_mint]
    }

    fn get_accounts_to_update(&self) -> Vec<Pubkey> {
        vec![
            self.accounts.bonding_curve,
            self.accounts.project,
            self.accounts.fee_config,
        ]
    }

    fn update(&mut self, account_provider: impl AccountProvider) -> Result<(), AmmError> {
        let curve = Self::read(
            &account_provider,
            &self.accounts.bonding_curve,
            BondingCurve::decode,
        )?;
        let project = Self::read(&account_provider, &self.accounts.project, Project::decode)?;
        let fee_config = Self::read(
            &account_provider,
            &self.accounts.fee_config,
            FeeConfig::decode,
        )?;
        if curve.project_id != self.project_id
            || project.project_id != self.project_id
            || project.token_mint != self.accounts.token_mint
        {
            return Err(
                QuoteError::InvalidAccount("accounts of another project".to_string()).into(),
            );
        }
        self.curve = curve;
        self.project = Some(project);
        self.fee_config = Some(fee_config);
        Ok(())
    }

    fn quote(&self, quote_params: &QuoteParams) -> Result<Quote, AmmError> {
        if quote_params.swap_mode != SwapMode::ExactIn {
            return Err(QuoteError::Unsupported("exact-out".to_string()).into());
        }
        let market = self.market()?;
        let side = self.side(&quote_params.input_mint, &quote_params.output_mint)?;
        let fill = if side == PLACEHOLDER_BUY {
            quote::buy(&market, quote_params.amount, self.now())?
        } else {
            quote::sell(&market, quote_params.amount, self.now())?
        };
        // Both legs' fees are taken in SOL: from the input on a buy, from the output on a sell.
        let base = if side == PLACEHOLDER_BUY {
            fill.in_amount
        } else {
            fill.out_amount + fill.fee()
        };
        let fee_pct = if base == 0 {
            Decimal::ZERO
        } else {
            Decimal::from(fill.fee()) / Decimal::from(base)
        };
        Ok(Quote {
            in_amount: fill.in_amount,
            out_amount: fill.out_amount,
            fee_amount: fill.fee(),
            fee_mint: NATIVE_MINT,
            fee_pct,
        })
    }

    /// The program's own `buy`/`sell` accounts, without a referrer (the optional referrer
    /// slots get the program id, Anchor's "none"). The trader is `token_transfer_authority`:
    /// it signs, owns the token account, and pays or receives the SOL. The protocol fee goes to
    /// the trader's protocol fee shard.
    fn get_swap_and_account_metas(
        &self,
        swap_params: &SwapParams,
    ) -> Result<SwapAndAccountMetas, AmmError> {
        let side = self.side(&swap_params.source_mint, &swap_params.destination_mint)?;
        let trader = swap_params.token_transfer_authority;
        let token_account = if side == PLACEHOLDER_BUY {
            swap_params.destination_token_account
        } else {
            swap_params.source_token_account
        };
        let (rebate, binding) = trader_accounts(&trader);
        let a = &self.accounts;
        let account_metas = vec![
            AccountMeta::new_readonly(a.fee_config, false),
            AccountMeta::new(a.project, false),
            AccountMeta::new(a.bonding_curve, false),
            AccountMeta::new(a.token_vault, false),
            AccountMeta::new_readonly(a.vault_authority, false),
            AccountMeta::new(a.project_treasury, false),
            AccountMeta::new(protocol_fee_shard(&trader), false),
            AccountMeta::new_readonly(LOADOUT_PROGRAM_ID, false), // referrer_account: none
            AccountMeta::new_readonly(LOADOUT_PROGRAM_ID, false), // l2_referrer_account: none
            AccountMeta::new(rebate, false),
            AccountMeta::new(binding, false),
            AccountMeta::new(token_account, false),
            AccountMeta::new(trader, true),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ];
        Ok(SwapAndAccountMetas {
            swap: Swap::Placeholder {
                data: Some(vec![side]),
            },
            account_metas,
        })
    }

    fn get_accounts_len(&self) -> usize {
        15
    }

    /// Tradable while the protocol and project are unpaused, the project is Live (not yet
    /// graduated), and the curve takes buys (below its target) or sells (its escape delay has
    /// passed).
    fn is_active(&self) -> bool {
        let Ok(market) = self.market() else {
            return false;
        };
        !market.fee_config.paused
            && market.project.state == PROJECT_STATE_LIVE
            && !market.project.paused
            && market.curve.is_active
            && (market.curve.reserve_sol < market.project.treasury_target
                || quote::sells_open(&market, self.now()).unwrap_or(false))
    }
}

/// Instruction data for the `Swap::Placeholder` this adapter returns: `buy { sol_amount,
/// min_tokens_out }` or `sell { token_amount, min_sol_out }`.
pub fn encode_instruction(swap: &Swap, in_amount: u64, min_out: u64) -> Option<Vec<u8>> {
    let Swap::Placeholder { data: Some(data) } = swap else {
        return None;
    };
    let discriminator = match *data.first()? {
        PLACEHOLDER_BUY => BUY_DISCRIMINATOR,
        PLACEHOLDER_SELL => SELL_DISCRIMINATOR,
        _ => return None,
    };
    let mut out = Vec::with_capacity(24);
    out.extend_from_slice(&discriminator);
    out.extend_from_slice(&in_amount.to_le_bytes());
    out.extend_from_slice(&min_out.to_le_bytes());
    Some(out)
}
