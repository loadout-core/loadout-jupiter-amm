use sha2::{Digest, Sha256};
use solana_pubkey::{Pubkey, pubkey};

use crate::QuoteError;

// The canonical upgradeable-loader ProgramData for Loadout Core.
pub const ADDRESS: Pubkey = pubkey!("13vr9ps8P1RMsFZvjQAPgaL3VVgSWwGxRvnkHR3SDnMb");
const LOADER: Pubkey = pubkey!("BPFLoaderUpgradeab1e11111111111111111111111");
const HEADER_LEN: usize = 45;
const NOMINATED_PAYLOAD_LEN: usize = 1_280_224;
const NOMINATED_SHA256: [u8; 32] = [
    0x2f, 0x22, 0x22, 0x7f, 0x7c, 0x1a, 0xad, 0x7a, 0xb1, 0xca, 0x60, 0x69, 0xed, 0x61, 0xcf, 0x93,
    0xb8, 0xe3, 0xa5, 0x57, 0xba, 0x43, 0xe1, 0xca, 0x75, 0xcf, 0xd7, 0x99, 0x2a, 0xdd, 0xdc, 0xcb,
];

pub fn validate(owner: &Pubkey, data: &[u8]) -> Result<(), QuoteError> {
    if *owner != LOADER
        || data.len() < HEADER_LEN
        || data[..4] != 3u32.to_le_bytes()
        || data[12] > 1
    {
        return Err(QuoteError::InvalidAccount(
            "invalid Core ProgramData".into(),
        ));
    }
    let payload = &data[HEADER_LEN..];
    let Some((nominated, padding)) = payload.split_at_checked(NOMINATED_PAYLOAD_LEN) else {
        return Err(QuoteError::Unsupported(
            "unrecognized Core executable".into(),
        ));
    };
    // A loader capacity extension may append only zeroes to the deployed payload.
    let hash: [u8; 32] = Sha256::digest(nominated).into();
    if hash != NOMINATED_SHA256 || padding.iter().any(|&byte| byte != 0) {
        return Err(QuoteError::Unsupported(
            "unrecognized Core executable".into(),
        ));
    }
    Ok(())
}
