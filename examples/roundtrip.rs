use aura_codec::legacy::synthetic::{generate_events, SyntheticConfig};
use aura_codec::legacy::ultra::UltraLayout;
use aura_codec::legacy::{cold, convert, ultra, warm};

fn main() -> aura_codec::Result<()> {
    let events = generate_events(SyntheticConfig {
        event_count: 16,
        repeated_ts_run: 2,
        ..SyntheticConfig::default()
    });

    let cold_bytes = cold::encode_events(&events)?;
    let warm_bytes = convert::cold_to_warm(&cold_bytes)?;
    let ultra_bytes = convert::cold_to_ultra(&cold_bytes, UltraLayout::new(8)?)?;

    assert_eq!(events, cold::decode_events(&cold_bytes)?);
    assert_eq!(events, warm::decode_events(&warm_bytes)?);
    assert_eq!(events, ultra::decode_events(&ultra_bytes)?.1);

    println!("cold_bytes={}", cold_bytes.len());
    println!("warm_bytes={}", warm_bytes.len());
    println!("ultra_bytes={}", ultra_bytes.len());
    Ok(())
}
