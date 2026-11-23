# Integration interface v1

One adapter supports historical executables and three quote behavior profiles through
one stable schema-1 `IntegrationConfig` account. Jupiter supplies the descriptor and
canonical ProgramData through its account cache alongside each market's accounts.
The adapter performs no RPC calls, and swap account order and encoding are unchanged.

## Executable admission

Pinned historical payloads remain usable before a descriptor exists and after rollback.
A complete pinned historical payload followed only by zero allocation padding is also
accepted: the adapter verifies every byte of the original payload and the padding.
The low-level `Runtime::decode` API still checks strict full-payload fingerprints.

Other executables require a descriptor with the canonical Core owner, discriminator,
84-byte length, schema 1, canonical bump, supported `quote_version`, zero reserved bytes,
matching loader deployment slot and full executable-payload SHA-256, including padding.
The current loader upgrade authority publishes the attestation; the SDK independently
compares it with the executable. This establishes identity, not semantic correctness.

A failed refresh invalidates the previous market. Unsupported versions, wrong owners,
stale slots and altered payloads disable quoting. Pinned historical code cannot be
relabelled by metadata. Explicit deployment parameters remain a legacy compatibility
helper; the descriptor path needs no integrator-specific executable registration.

## Behavior profiles

`quote_version` selects implemented behavior, independently of descriptor schema or
SDK package version. One schema-1 SDK implements all three profiles:

- **0 (`Runtime::Develop`):** global fees, fillable remainder guard and deferred trader accounts.
- **1 (`Runtime::Nominated`):** captured curve fees and eager trader-account initialization.
- **2 (`Runtime::Preview`):** captured fees, deferred trader accounts and optional inline buybacks.

These are supported semantics, not a required deployment order. An upgrade of unrelated
programs that leaves Core unchanged needs no Core upgrade for this adapter. Introducing
a publisher into an existing Core must preserve that Core's quote profile. A profile-0
fixture does not establish that replacing a profile-1 deployment with it is safe.

## Compatibility rules for future changes

Keep the profile for refactors, logging or unrelated instructions only when all
SDK-visible behavior stays compatible. Parameter changes through fields already read
by the SDK do not inherently require a new profile; verify affected boundaries.

Compatibility boundaries include quote math, rounding, fee application, post-swap
reserves/buybacks, trade eligibility, completion rules, decoded layouts, instruction
encoding and account order. Changes that make this adapter incorrect require a new
profile and its decoding, quote and swap implementation before activation. Metadata
cannot supply an unimplemented algorithm. Preserve supported historical paths and do
not add fingerprints to bypass missing semantic support.

`schema_version` describes the descriptor's wire format. This reader accepts only
schema 1, exactly 84 bytes and zero reserved bytes. Format changes or use of reserved
bytes require a compatible reader before activation; additive fields are not
implicitly accepted.

Before an upgrade, run binary-backed buy/sell parity across fees, rounding boundaries,
post-swap state and failures. For a compatible executable, also prove that the unchanged
SDK admits it through publication, rejects stale metadata and recovers after rollback.
A new profile requires old/new path validation and integrator adoption before activation.

## Upgrade sequence and host behavior

Publish the actual full payload hash and loader deployment slot after upgraded code
becomes visible. Real loader tests show that code cannot execute in its deployment
slot (`UnsupportedProgramId`), so upgrade and publication cannot be atomic. Unknown
code stays unavailable for quoting until matching metadata reaches the host cache.
Compatible builds need fresh publication and validation, without an SDK source edit.

Extending a descriptor-dependent ProgramData allocation changes the full-payload
hash even if the executable is unchanged. After an extension or rollback to a
descriptor-dependent build, publish the actual payload hash and loader slot, then
verify that the host has resumed quotes. Built-in historical fallbacks can recover
without publication when their executable identity is still recognized.

Hosts must refresh both ProgramData and the descriptor and tolerate an absent optional
PDA. Out-of-order account updates fail closed. The preview publisher fixture requires
more capacity than the captured mainnet allocation; capacity extension, upgrade and
publication require a separately reviewed release plan. These tests do not establish
production host cache behavior, discovery, wrapping or direct/multihop/split routing.

The publisher's five accounts are the typed self-program, its ProgramData, current
upgrade authority (writable signer and rent payer), canonical descriptor PDA and System
Program. No descriptor account is forwarded into swaps. No public-chain transaction
is performed by the default checks.

## Offline verification

If an intermediate release keeps Core byte-identical, validate the direct Nominated
to Combined path with the frozen SDK checkout. In release verification, record the
Combined ELF checksum and supply the actual artifact:

```sh
LOADOUT_COMBINED_CORE_ELF=/absolute/path/to/loadout_core.so \
  cargo test --locked --test integration_runtime \
  one_sdk_survives_nominated_to_combined_and_rollback -- --exact
```

The default CI invocation uses the checked-in fixture. The release invocation
checks the supplied Core artifact against the captured Nominated curve, including
trade parity, descriptor publication and rollback. Verify planned ProgramData
capacity and host refresh separately. If an intermediate release changes Core,
include its actual Core binary and selected profile in the compatibility rehearsal.

Historical tests remain. `integration_runtime.rs` executes publication, loader upgrades,
capacity extension, authority rotation, rollback, prefunding, pause, replay and failed
publication rollback in LiteSVM. Cross-profile sequences preserve curve bytes and check
exact buy/sell output, fees, reserves, wallet deltas and rent. Buffers and initial loader
authority are bootstrapped test accounts; upgrades use real loader instructions.
Synthetic parser mutations test identity validation, not arbitrary-bytecode compatibility.

Fixture hashes and origins are listed in `tests/fixtures/releases.json`. Publisher
fixtures are local test builds, not deployment or release assertions; their fingerprints
are deliberately absent from the built-in table. Two different profile-2 builds
(default and `no-log-ix-name`) prove stale-cache rejection and recovery through real
publication without adding either fingerprint to SDK source. Verify final release
artifacts independently before activation.

A governance PDA may sign through CPI. If its program-owned account cannot fund a
System transfer, a separate voluntary payer can prefund the descriptor to full rent;
publication still requires the current upgrade authority's signature. Native-CPI tests
assert unchanged governance data and lamports. They do not verify a production
governance proposal/execution flow.
