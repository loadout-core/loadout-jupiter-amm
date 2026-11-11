//! Offline microbenchmarks; fixture loading and market initialization are outside timings.
#[path = "../tests/common/mod.rs"]
mod common;

use jupiter_amm_interface::{Amm, QuoteParams, SwapMode};
use loadout_jupiter_amm::{NATIVE_MINT, encode_instruction, protocol_fee_shard};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use solana_pubkey::Pubkey;

struct CountingAllocator;
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, size) }
    }
}

fn measure<T>(name: &str, mut operation: impl FnMut() -> T) {
    const N: u32 = 100_000;
    for _ in 0..1_000 {
        black_box(operation());
    }
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..N {
        black_box(operation());
    }
    let elapsed = start.elapsed();
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations;
    println!(
        "{name}: {:.1} ns/op, {:.2} alloc/realloc calls/op",
        elapsed.as_nanos() as f64 / f64::from(N),
        allocations as f64 / f64::from(N)
    );
}

fn main() {
    for (name, key) in [
        ("active", "8gMCp3MpUN6nJ9N2nwjuefrqK6rp6Mrvpnqn1pjUvmwn"),
        ("protected", "5o56bndGq6YFZFwCTuCmZv42g89bi6gdhC2TEeRYaFbo"),
        ("capped", "9pN9gVHZXHYoPdW44i7RCS14kBcQdsFGqh3NL1F6tTvX"),
    ] {
        let snapshot = common::Snapshot::load(key);
        let (amm, _) = common::amm_at(&snapshot, None);
        let token = amm.accounts().token_mint;
        for (side, input_mint, output_mint, amount) in [
            ("buy", NATIVE_MINT, token, 1_000_000_000),
            ("sell", token, NATIVE_MINT, 1_000_000_000),
            ("rejected", NATIVE_MINT, token, 0),
        ] {
            let params = QuoteParams {
                amount,
                input_mint,
                output_mint,
                swap_mode: SwapMode::ExactIn,
                flow_type: Default::default(),
            };
            measure(&format!("{name}/{side}"), || amm.quote(black_box(&params)));
        }
    }
    let swap = jupiter_amm_interface::Swap::Placeholder {
        data: Some(vec![0]),
    };
    measure("encode", || {
        encode_instruction(black_box(&swap), black_box(1_000_000), 1)
    });
    let trader = Pubkey::new_from_array([17; 32]);
    measure("fee-shard", || protocol_fee_shard(black_box(&trader)));
}
