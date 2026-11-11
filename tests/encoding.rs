use jupiter_amm_interface::Swap;
use loadout_jupiter_amm::{
    BUY_DISCRIMINATOR, LOADOUT_PROGRAM_ID, SELL_DISCRIMINATOR, encode_instruction,
    protocol_fee_shard,
};
use solana_pubkey::Pubkey;

#[test]
fn cached_fee_shards_match_canonical_derivation_for_every_trader_prefix() {
    for byte in 0..=255u8 {
        let trader = Pubkey::new_from_array([byte; 32]);
        let expected = Pubkey::find_program_address(
            &[b"protocol_fee_shard", &[byte % 16]],
            &LOADOUT_PROGRAM_ID,
        )
        .0;
        assert_eq!(protocol_fee_shard(&trader), expected);
    }
}

#[test]
fn encoding_preserves_direction_amount_and_slippage_without_growing_capacity() {
    for (tag, discriminator) in [(0, BUY_DISCRIMINATOR), (1, SELL_DISCRIMINATOR)] {
        let data = encode_instruction(
            &Swap::Placeholder {
                data: Some(vec![tag]),
            },
            u64::MAX,
            123,
        )
        .unwrap();
        assert_eq!(data.len(), 24);
        assert_eq!(data.capacity(), 24);
        assert_eq!(&data[..8], &discriminator);
        assert_eq!(&data[8..16], &u64::MAX.to_le_bytes());
        assert_eq!(&data[16..], &123u64.to_le_bytes());
    }
}
