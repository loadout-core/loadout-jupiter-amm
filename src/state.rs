//! Raw readers for the three `loadout-core` accounts a quote depends on.
//!
//! Each reader checks the account's size and its Anchor discriminator, then reads fields at
//! fixed offsets (8-byte discriminator, then Borsh in declaration order; the structs have no
//! variable-length fields). The offsets are those of `programs/loadout-core/src/state/*.rs`;
//! `tests/state.rs` checks them against real accounts, and the parity tests against the program.

use solana_pubkey::Pubkey;

use crate::QuoteError;

/// `sha256("account:BondingCurve")[..8]`.
pub const BONDING_CURVE_DISCRIMINATOR: [u8; 8] = [23, 183, 248, 55, 96, 216, 172, 96];
/// `sha256("account:Project")[..8]`.
pub const PROJECT_DISCRIMINATOR: [u8; 8] = [205, 168, 189, 202, 181, 247, 142, 19];
/// `sha256("account:FeeConfig")[..8]`.
pub const FEE_CONFIG_DISCRIMINATOR: [u8; 8] = [143, 52, 146, 187, 219, 123, 76, 155];

pub const BONDING_CURVE_LEN: usize = 154;
pub const PROJECT_LEN: usize = 810;
pub const FEE_CONFIG_LEN: usize = 260;

/// `ProjectState::Live`.
pub const PROJECT_STATE_LIVE: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BondingCurve {
    pub project_id: u64,
    pub reserve_sol: u64,
    pub reserve_token: u64,
    pub virtual_sol: u64,
    pub virtual_token: u64,
    pub invariant_k: u128,
    pub is_active: bool,
    /// Unix time of the first buy that reached the target; 0 if never.
    pub completed_at: i64,
    pub fee_snapshot: u8,
    pub treasury_fee_bps: u16,
    pub protocol_fee_bps: u16,
    pub sniper_share_bps: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Project {
    pub project_id: u64,
    pub token_mint: Pubkey,
    /// Real SOL reserve at which the curve completes.
    pub treasury_target: u64,
    pub state: u8,
    pub launched_at_ts: i64,
    pub protected_launch_duration: u32,
    pub protected_launch_fee_bps: u16,
    pub paused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeeConfig {
    pub protocol_fee_recipient: Pubkey,
    pub protocol_fee_bps: u16,
    pub treasury_fee_bps: u16,
    pub paused: bool,
    /// 0 = the program's default (24 hours).
    pub curve_escape_delay_seconds: u32,
    pub sniper_share_bps: u16,
}

impl BondingCurve {
    pub fn decode(data: &[u8]) -> Result<Self, QuoteError> {
        let d = checked(
            data,
            &BONDING_CURVE_DISCRIMINATOR,
            BONDING_CURVE_LEN,
            "BondingCurve",
        )?;
        Ok(Self {
            project_id: u64_at(d, 8),
            reserve_sol: u64_at(d, 16),
            reserve_token: u64_at(d, 24),
            virtual_sol: u64_at(d, 32),
            virtual_token: u64_at(d, 40),
            invariant_k: u128::from_le_bytes(bytes_at(d, 48)),
            // cumulative_volume_sol 64, total_buys 72, total_sells 80
            is_active: bool_at(d, 88)?,
            // bump 89
            completed_at: i64::from_le_bytes(bytes_at(d, 90)),
            fee_snapshot: d[114],
            treasury_fee_bps: u16::from_le_bytes(bytes_at(d, 115)),
            protocol_fee_bps: u16::from_le_bytes(bytes_at(d, 117)),
            sniper_share_bps: u16::from_le_bytes(bytes_at(d, 119)),
        })
    }
}

impl Project {
    pub fn decode(data: &[u8]) -> Result<Self, QuoteError> {
        let d = checked(data, &PROJECT_DISCRIMINATOR, PROJECT_LEN, "Project")?;
        Ok(Self {
            project_id: u64_at(d, 8),
            // admin 16
            token_mint: Pubkey::new_from_array(bytes_at(d, 48)),
            // total_supply 80
            treasury_target: u64_at(d, 88),
            // treasury_received 96
            state: d[104],
            // launch_type 105 .. cliff_ts 746
            launched_at_ts: i64::from_le_bytes(bytes_at(d, 722)),
            // pending_admin 754
            protected_launch_duration: u32::from_le_bytes(bytes_at(d, 786)),
            protected_launch_fee_bps: u16::from_le_bytes(bytes_at(d, 790)),
            paused: bool_at(d, 792)?,
        })
    }
}

impl FeeConfig {
    pub fn decode(data: &[u8]) -> Result<Self, QuoteError> {
        let d = checked(data, &FEE_CONFIG_DISCRIMINATOR, FEE_CONFIG_LEN, "FeeConfig")?;
        Ok(Self {
            // admin 8, pending_admin 40
            protocol_fee_recipient: Pubkey::new_from_array(bytes_at(d, 72)),
            protocol_fee_bps: u16::from_le_bytes(bytes_at(d, 104)),
            treasury_fee_bps: u16::from_le_bytes(bytes_at(d, 106)),
            paused: bool_at(d, 108)?,
            // next_project_id 109 .. router_fees_disabled 229
            curve_escape_delay_seconds: u32::from_le_bytes(bytes_at(d, 230)),
            sniper_share_bps: u16::from_le_bytes(bytes_at(d, 254)),
        })
    }
}

fn checked<'a>(
    data: &'a [u8],
    discriminator: &[u8; 8],
    len: usize,
    name: &str,
) -> Result<&'a [u8], QuoteError> {
    if data.len() != len || data[..8] != discriminator[..] {
        return Err(QuoteError::InvalidAccount(format!("not a {name} account")));
    }
    Ok(data)
}

/// `N` bytes at `offset`. Callers only use offsets inside the length `checked` verified.
fn bytes_at<const N: usize>(data: &[u8], offset: usize) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(&data[offset..offset + N]);
    out
}

fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes_at(data, offset))
}

/// Borsh `bool`: anything but 0 or 1 is a decode error on-chain too.
fn bool_at(data: &[u8], offset: usize) -> Result<bool, QuoteError> {
    match data[offset] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(QuoteError::InvalidAccount("invalid bool".to_string())),
    }
}
