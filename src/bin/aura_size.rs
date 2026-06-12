use aura_codec::synthetic::{generate_events, SyntheticConfig};
use aura_codec::ultra::UltraLayout;
use aura_codec::{cold, grouped, ultra, warm};

fn main() -> aura_codec::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let event_count = args.get(1).and_then(|value| value.parse().ok()).unwrap_or(10_000);
    let repeated_ts_run = args.get(2).and_then(|value| value.parse().ok()).unwrap_or(1);
    let block_size = args.get(3).and_then(|value| value.parse().ok()).unwrap_or(8);
    let layout = UltraLayout::new(block_size)?;
    let events = generate_events(SyntheticConfig {
        event_count,
        repeated_ts_run,
        ..SyntheticConfig::default()
    });
    let cold = cold::encode_events(&events)?;
    let warm = warm::encode_events(&events)?;
    let ultra = ultra::encode_events(&events, layout)?;
    let groups = grouped::plan_groups(&events, grouped::GroupPolicy::default());

    println!("events={}", events.len());
    println!("repeated_ts_run={repeated_ts_run}");
    println!("ultra_block_size={block_size}");
    println!("group_count={}", groups.len());
    println!("cold_bytes={}", cold.len());
    println!("warm_bytes={}", warm.len());
    println!("ultra_bytes={}", ultra.len());
    println!("warm_vs_cold={:.3}", warm.len() as f64 / cold.len() as f64);
    println!("ultra_vs_cold={:.3}", ultra.len() as f64 / cold.len() as f64);
    Ok(())
}
