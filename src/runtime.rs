//! Match behavior to the executable, not to account size or a market's launch type.
//! ProgramData is supplied through Jupiter's account cache; no RPC occurs in the adapter.
use sha2::{Digest, Sha256};
use solana_pubkey::{Pubkey, pubkey};

use crate::QuoteError;

const PADDED_PREVIEW_SHA256: [u8; 32] = [
    0xda, 0x8a, 0x8e, 0x27, 0xfa, 0x44, 0x66, 0x89, 0x29, 0xa2, 0xdf, 0x00, 0x0e, 0xce, 0x1e, 0x0d,
    0x18, 0x65, 0x57, 0x3f, 0x20, 0x9a, 0x59, 0xe9, 0x86, 0x7e, 0x35, 0xbc, 0x09, 0xdc, 0xc8, 0xbb,
];
const LEGACY_SHA256: [u8; 32] = [
    0x50, 0x2c, 0x4e, 0x1b, 0xc9, 0x28, 0xf5, 0xfd, 0x21, 0xc5, 0xeb, 0x3e, 0x25, 0xa3, 0x46, 0x6c,
    0x43, 0x60, 0x1b, 0x18, 0x71, 0xa8, 0xb8, 0x52, 0x83, 0x63, 0x3d, 0x9b, 0xed, 0x3a, 0xca, 0x5f,
];
const NOMINATED_SHA256: [u8; 32] = [
    0x2f, 0x22, 0x22, 0x7f, 0x7c, 0x1a, 0xad, 0x7a, 0xb1, 0xca, 0x60, 0x69, 0xed, 0x61, 0xcf, 0x93,
    0xb8, 0xe3, 0xa5, 0x57, 0xba, 0x43, 0xe1, 0xca, 0x75, 0xcf, 0xd7, 0x99, 0x2a, 0xdd, 0xdc, 0xcb,
];
const PREVIEW_SHA256: [u8; 32] = [
    0xec, 0x84, 0x13, 0x5, 0x83, 0xe9, 0x99, 0x4d, 0x41, 0xce, 0x65, 0x14, 0x32, 0x25, 0x33, 0xfe,
    0xc9, 0x2, 0x95, 0x5a, 0xd3, 0x89, 0x86, 0xcd, 0x38, 0x2b, 0x6c, 0xf0, 0x2b, 0x5e, 0x5a, 0x27,
];

pub fn program_data_address() -> Pubkey {
    pubkey!("13vr9ps8P1RMsFZvjQAPgaL3VVgSWwGxRvnkHR3SDnMb")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runtime {
    Legacy,
    /// Global fees, fillable remainder guard, deferred trader accounts.
    Develop,
    Nominated,
    Preview,
}

impl Runtime {
    pub fn decode(data: &[u8], owner: &Pubkey) -> Result<Self, QuoteError> {
        Self::decode_with_deployment(data, owner, None)
    }

    pub fn decode_with_deployment(
        data: &[u8],
        owner: &Pubkey,
        deployment: Option<Deployment>,
    ) -> Result<Self, QuoteError> {
        Self::resolve(data, owner, None, deployment)
    }

    /// Historical executables are pinned fallbacks. New executables must have a matching
    /// canonical on-chain descriptor, or an explicitly supplied legacy deployment registration.
    pub fn resolve(
        data: &[u8],
        owner: &Pubkey,
        descriptor: Option<(&[u8], &Pubkey)>,
        deployment: Option<Deployment>,
    ) -> Result<Self, QuoteError> {
        if *owner != pubkey!("BPFLoaderUpgradeab1e11111111111111111111111")
            || data.len() <= 45
            || data[..4] != 3u32.to_le_bytes()
            || data[12] > 1
        {
            return Err(QuoteError::InvalidAccount("invalid ProgramData".into()));
        }
        let digest: [u8; 32] = Sha256::digest(&data[45..]).into();
        let known = match digest {
            LEGACY_SHA256 => Ok(Self::Legacy),
            NOMINATED_SHA256 => Ok(Self::Nominated),
            PREVIEW_SHA256 | PADDED_PREVIEW_SHA256 => Ok(Self::Preview),
            _ => Err(QuoteError::Unsupported(
                "unrecognized program executable".into(),
            )),
        };
        // A pinned historical executable is sufficient evidence even after an operator rollback
        // leaves a newer descriptor behind. It cannot be relabelled by metadata.
        if known.is_err()
            && let Some((bytes, descriptor_owner)) = descriptor
        {
            return IntegrationDescriptor::decode(bytes, descriptor_owner)?.runtime(
                u64::from_le_bytes(
                    data[4..12].try_into().map_err(|_| {
                        QuoteError::InvalidAccount("invalid deployment slot".into())
                    })?,
                ),
                &digest,
            );
        }
        if let Some(deployment) = deployment
            && digest == deployment.sha256
        {
            if known
                .as_ref()
                .is_ok_and(|profile| *profile != deployment.runtime)
            {
                return Err(QuoteError::Unsupported(
                    "deployment profile conflicts with known executable".into(),
                ));
            }
            return Ok(deployment.runtime);
        }
        known
    }
}

/// Trusted integrator configuration for another binary with an already-supported behavior.
/// This is not an on-chain assertion of compatibility: validate parity before registering it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deployment {
    pub sha256: [u8; 32],
    pub runtime: Runtime,
}

impl Deployment {
    pub fn from_params(params: Option<&serde_json::Value>) -> Result<Option<Self>, QuoteError> {
        let Some(value) = params.and_then(|v| v.get("loadout_deployment")) else {
            return Ok(None);
        };
        let invalid = || QuoteError::Unsupported("invalid loadout_deployment parameters".into());
        let runtime = match value.get("profile").and_then(|v| v.as_str()) {
            Some("legacy") => Runtime::Legacy,
            Some("nominated") => Runtime::Nominated,
            Some("preview") => Runtime::Preview,
            _ => return Err(invalid()),
        };
        let hex = value
            .get("sha256")
            .and_then(|v| v.as_str())
            .ok_or_else(invalid)?;
        if hex.len() != 64 || !hex.is_ascii() {
            return Err(invalid());
        }
        let mut sha256 = [0; 32];
        for (out, pair) in sha256.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
            let digit = |c: u8| match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                _ => None,
            };
            *out = digit(pair[0])
                .zip(digit(pair[1]))
                .map(|(a, b)| a * 16 + b)
                .ok_or_else(invalid)?;
        }
        Ok(Some(Self { sha256, runtime }))
    }
}

pub fn integration_config_address() -> Pubkey {
    Pubkey::find_program_address(&[b"integration_config"], &crate::LOADOUT_PROGRAM_ID).0
}

/// Fixed-width account declaration published by the executable's upgrade authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntegrationDescriptor {
    pub quote_version: u16,
    pub deployment_slot: u64,
    pub executable_sha256: [u8; 32],
}
impl IntegrationDescriptor {
    pub fn decode(data: &[u8], owner: &Pubkey) -> Result<Self, QuoteError> {
        let invalid = || QuoteError::InvalidAccount("invalid integration descriptor".into());
        let discriminator = &Sha256::digest(b"account:IntegrationConfig")[..8];
        let (_, bump) =
            Pubkey::find_program_address(&[b"integration_config"], &crate::LOADOUT_PROGRAM_ID);
        if *owner != crate::LOADOUT_PROGRAM_ID
            || data.len() != 84
            || data[..8] != *discriminator
            || data[8] != 1
            || data[51] != bump
            || data[52..].iter().any(|b| *b != 0)
        {
            return Err(invalid());
        }
        Ok(Self {
            quote_version: u16::from_le_bytes(data[9..11].try_into().map_err(|_| invalid())?),
            deployment_slot: u64::from_le_bytes(data[11..19].try_into().map_err(|_| invalid())?),
            executable_sha256: data[19..51].try_into().map_err(|_| invalid())?,
        })
    }
    fn runtime(&self, slot: u64, digest: &[u8; 32]) -> Result<Runtime, QuoteError> {
        if self.deployment_slot != slot || self.executable_sha256 != *digest {
            return Err(QuoteError::Unsupported(
                "integration descriptor does not match the deployed executable".into(),
            ));
        }
        // Schema 1 supports multiple behavior profiles in one SDK release.
        // Profiles select implemented semantics, not arbitrary executable compatibility.
        // Same-version code upgrades need fresh slot/hash publication, not a new fingerprint.
        // Changed quote/state/ABI behavior needs a new version and implementation here and
        // in its consumers; preserve old branches for existing deployments and rollback.
        // See docs/compatibility.md, "Compatibility rules for future changes".
        match self.quote_version {
            0 => Ok(Runtime::Develop),
            1 => Ok(Runtime::Nominated),
            2 => Ok(Runtime::Preview),
            _ => Err(QuoteError::Unsupported(
                "unsupported quote interface version".into(),
            )),
        }
    }
}

/// A loader extension can append zero allocation padding to a complete, pinned historical
/// payload. This is byte-for-byte code identity, not an inference from layout or launch type.
/// The strict `decode` API still fingerprints the entire supplied payload.
pub(crate) fn historical_zero_extension(data: &[u8], owner: &Pubkey) -> Option<Runtime> {
    const HISTORICAL_PAYLOAD_LEN: usize = 1_280_224;
    if *owner != pubkey!("BPFLoaderUpgradeab1e11111111111111111111111")
        || data.get(..4)? != 3u32.to_le_bytes()
        || *data.get(12)? > 1
    {
        return None;
    }
    let payload = data.get(45..)?;
    if payload.len() <= HISTORICAL_PAYLOAD_LEN
        || payload
            .get(HISTORICAL_PAYLOAD_LEN..)?
            .iter()
            .any(|b| *b != 0)
    {
        return None;
    }
    let digest: [u8; 32] = Sha256::digest(payload.get(..HISTORICAL_PAYLOAD_LEN)?).into();
    match digest {
        LEGACY_SHA256 => Some(Runtime::Legacy),
        NOMINATED_SHA256 => Some(Runtime::Nominated),
        _ => None,
    }
}
