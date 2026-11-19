# Loadout Jupiter AMM adapter

A Rust adapter implementing Jupiter's `Amm` interface for Loadout's pre-graduation
SOL/token bonding curves. Each `BondingCurve` account is a market. After graduation,
trading moves to Meteora DAMM v2.

## Quoting

The adapter reads the curve, project and fee configuration from account data supplied
by Jupiter. It makes no network calls. It supports ExactIn buys and sells, including
protected-launch fees, capped input near the graduation target and trading-state checks.
ExactOut is unsupported. Interface and test kit use matching pinned revisions of
Jupiter interface 0.7.0.

The detailed [technical specification](docs/jupiter-integration.md) describes account
order, native SOL handling and execution constraints. Production routing and market
discovery require Jupiter integration; this crate alone does not enable either.

## Native SOL and account funding

`buy` takes native SOL from the signing trader authority; `sell` pays native SOL to
that authority. Source/destination token accounts carry the project token.

The signing trader authority pays or receives native SOL; a separate transaction fee
payer does not implicitly fund it. Legacy and nominated executables create rebate and
referral-binding accounts on a first trade (2,964,960 lamports under the fixture rent
schedule). The global-fee and preview executables create them only when needed: an unreferred trade
creates neither. Rent, transaction fees and token-account setup are outside the quote.

## Executable compatibility

The adapter also requests the program's canonical ProgramData account through Jupiter's
account cache. `update` validates its owner and hashes its executable payload to select
an explicitly supported behavior. Account size and launch type cannot identify a program
upgrade. An unknown executable needs a matching supported-version on-chain descriptor. A failed
refresh disables quoting until a validated deployment is supplied. No network calls occur in the adapter.

This single adapter supports schema 1 across historical, global-fee, Nominated and
preview behavior. New supported binaries are admitted through the on-chain descriptor,
without a new SDK release for every compatible program upgrade. The preview is not a mainnet release. See
[compatibility](docs/compatibility.md) and [fixture provenance](tests/fixtures/releases.json).

## Verification

Tests execute pinned legacy, nominated and preview programs in LiteSVM. `tests/fixtures/mainnet-program.json`
records the program address, mainnet genesis, observation/deployment slots and binary
SHA-256. Each harness loads the executable for its declared scenario; legacy tests retain
`tests/fixtures/loadout.so` and the new cross-release suite selects the other two fixtures. No program deployment or public-chain transaction is
performed by the default checks.

Account inputs comprise five local-validator snapshots and one public devnet capture.
They are historical state fixtures, not a claim that these are live mainnet markets.
Time and boundary cases change copies in memory. The vendored `loadout-math` supplies
the curve and fee arithmetic; update quotes and fixtures together when deployed
behavior changes.

The suite checks amounts and fees to the unit, refusal codes, first/subsequent-trade
rent, slippage rollback, small-supply boundaries and core/Solana CPI composition.
Buys also run through Jupiter's test kit. Native-SOL sells use the supplemental
harness because the kit measures output-token-account deltas. The local CPI relay
is a test probe, not Jupiter's production router.

Use Rust 1.95.0 or newer with rustfmt and Clippy:

```sh
cargo +stable fetch --locked
shasum -a 256 -c tests/fixtures/releases.sha256
cargo +stable fmt --all --check
cargo +stable clippy --workspace --all-targets --frozen -- -D warnings
cargo +stable test --workspace --frozen
```

Default builds disable fixture RPC access. `tests/snapshot.rs` is separately opt-in
through `fixture-rpc`; do not regenerate fixtures during an ordinary test run.

## References

- [Jupiter AMM integration](https://developers.jup.ag/docs/swap/routing/amm/integration)
- [Jupiter interface and test kit](https://github.com/jup-ag/jupiter-amm-interface)
- [Technical specification](docs/jupiter-integration.md)

## License

Copyright 2026 LDO Labs Inc. Licensed under the [Apache License, Version 2.0](LICENSE).
The license grants no rights to the Loadout name or logo (section 6).
