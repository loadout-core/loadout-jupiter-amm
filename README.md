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

On the deployed program, a new trader's first successful buy or sell initializes a
57-byte rebate account and a 113-byte referral-binding account, even without a
referrer. Their combined rent is 2,964,960 lamports under the tested rent schedule.
It is additional to the quoted swap amount, transaction fees and any token-account
setup. A separate transaction fee payer does not implicitly fund this trader rent.
Subsequent swaps reuse those accounts.

## Verification

Tests execute the dumped mainnet program in LiteSVM. `tests/fixtures/mainnet-program.json`
records the program address, mainnet genesis, observation/deployment slots and binary
SHA-256. The official test kit and both supplemental harnesses load the same
`tests/fixtures/loadout.so`. No program deployment or public-chain transaction is
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

Use Rust 1.95.0 with rustfmt and Clippy:

```sh
cargo +1.95.0 fetch --locked
shasum -a 256 -c tests/fixtures/loadout.so.sha256
cargo +1.95.0 fmt --all --check
cargo +1.95.0 clippy --workspace --all-targets --frozen -- -D warnings
cargo +1.95.0 test --workspace --frozen
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
