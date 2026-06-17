use aura_codec::legacy::synthetic::{generate_events, SyntheticConfig};
use aura_codec::legacy::ultra::UltraLayout;
use aura_codec::legacy::{cold, convert, ultra, warm};

#[test]
fn all_profiles_preserve_logical_events() {
    let events = generate_events(SyntheticConfig {
        event_count: 128,
        repeated_ts_run: 4,
        max_levels_per_side: 12,
        ..SyntheticConfig::default()
    });

    let cold_bytes = cold::encode_events(&events).unwrap();
    let warm_bytes = convert::cold_to_warm(&cold_bytes).unwrap();
    let ultra_bytes = convert::cold_to_ultra(&cold_bytes, UltraLayout::new(8).unwrap()).unwrap();

    assert_eq!(events, cold::decode_events(&cold_bytes).unwrap());
    assert_eq!(events, warm::decode_events(&warm_bytes).unwrap());
    assert_eq!(events, ultra::decode_events(&ultra_bytes).unwrap().1);
}
