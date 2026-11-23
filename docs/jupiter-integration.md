# Loadout AMM technical specification

## Market identity and state

Program: `8nPyghXuy35JwxZ8ANAzxMXKruibh6B3js4QMYBRFadx`.
Each market key is its 154-byte `BondingCurve` account. The reserve mints are the
native SOL mint and the project's SPL token mint. The adapter validates account
owners, canonical curve derivation, project IDs and token mint consistency.

`get_accounts_to_update` returns the curve, project, fee configuration and Core
ProgramData. `update` checks the loader owner and pinned Nominated payload hash.
Zero-filled capacity extensions are supported. Missing or unrecognized ProgramData
invalidates the market. `quote` uses the refreshed state and Jupiter's clock without
network calls.

## Quote behavior

ExactIn buys and sells reproduce the deployed program's constant-product virtual
reserve math, rounding, treasury/protocol fees and protected-launch buy fee window.
The adapter uses captured curve fees when present and global fees for older curves
without a snapshot. The protocol can receive a share of the excess protected fee.
Fees are denominated in SOL. A capped buy can consume less input than offered.
A completed curve refuses buys; sells follow its configured escape delay while the
project remains Live. Paused, inactive or graduated markets are refused as applicable.
Quotes do not include transaction fees, token-account setup or first-trade trader rent.
Nominated refuses buys that would leave an unfillable final gap to graduation.

All interface `FlowType` values currently use the same on-chain amounts and fees.
ExactOut and referrer-specific swap routing are not implemented.

## Native instruction accounts

`get_swap_and_account_metas` returns these accounts in native instruction order:

1. Fee configuration, readonly.
2. Project, writable.
3. Bonding curve, writable.
4. Token vault, writable.
5. Vault authority, readonly.
6. Project treasury, writable.
7. Authority-selected protocol fee shard, writable.
8. Optional referrer slot, readonly program ID for none.
9. Optional second-level referrer slot, readonly program ID for none.
10. Trader rebate account, writable.
11. Referral binding, writable.
12. Project-token source or destination account, writable.
13. Trader authority, writable signer.
14. SPL Token Program, readonly.
15. System Program, readonly.

The trader authority is `SwapParams.token_transfer_authority`. Buy output goes to
`destination_token_account`; sell input comes from `source_token_account`.
The token account must have the project mint. Sells require owner/delegate authority.
Native SOL is debited from or paid to the signing trader authority, which can differ
from the end user's token-account owner or the transaction fee payer.

The first successful trade initializes the trader's 57-byte rebate account and
113-byte binding account. Combined rent is 2,964,960 lamports under the tested rent
schedule, paid by the trader authority. Prefunded system accounts are allocated and
topped up to the required rent. Existing trader accounts are reused.

## Swap encoding and composition

Until a dedicated Jupiter variant exists, the adapter returns `Swap::Placeholder`
with a buy/sell direction byte. The native encoder produces an eight-byte instruction
discriminator followed by little-endian input amount and minimum output, both u64.

Execution must satisfy native-SOL funding, signer/writable privileges and minimum
output. Native SOL paid into a WSOL account requires `SyncNative` before a subsequent
SPL-token transfer. Temporary-account cleanup requires the correct authority.
Capped input can leave surplus; a composed route must account for that surplus,
pre-existing WSOL balances and rent. These constraints are tested with a local CPI
probe; the probe does not implement Jupiter's production wrapper or router.

## Program fixture and test scope

The canonical program fixture is a read-only dump of finalized mainnet ProgramData.
See [program provenance](../tests/fixtures/mainnet-program.json) and
[binary checksum](../tests/fixtures/loadout.so.sha256). All execution harnesses use it.
The default suite runs offline in LiteSVM, without signing public-chain transactions.

Historical local-validator and public devnet account fixtures exercise active,
protected, near-target, completed and graduated states. Boundary mutations are made
only in memory. Fixtures do not establish live mainnet market discovery.

Official-kit buys compare received token deltas with quotes. Supplemental tests
measure native-SOL sells, rent, payer costs, fees, rollback, delegated/PDA authorities,
WSOL synchronization, cleanup, capped input and changing state. Real direct,
multihop and split routes through Jupiter require integration-level validation.
