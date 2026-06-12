use crate::{BookEvent, BookId, LevelChange};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntheticConfig {
    pub event_count: usize,
    pub base_ts_event: u64,
    pub ts_step: u64,
    pub repeated_ts_run: usize,
    pub max_levels_per_side: usize,
}

impl Default for SyntheticConfig {
    fn default() -> Self {
        Self {
            event_count: 1024,
            base_ts_event: 1_700_000_000_000,
            ts_step: 100,
            repeated_ts_run: 1,
            max_levels_per_side: 8,
        }
    }
}

pub fn generate_events(config: SyntheticConfig) -> Vec<BookEvent> {
    let mut rng = Lcg::new(0xAURA_C0DE);
    let repeated = config.repeated_ts_run.max(1);
    let max_levels = config.max_levels_per_side.max(1);
    let mut events = Vec::with_capacity(config.event_count);
    for idx in 0..config.event_count {
        let ts_event = config.base_ts_event + ((idx / repeated) as u64 * config.ts_step);
        let sequence = idx as u64 + 1;
        let book = if idx % 2 == 0 { BookId::BookA } else { BookId::BookB };
        let bid_count = 1 + (rng.next_usize() % max_levels);
        let ask_count = 1 + (rng.next_usize() % max_levels);
        events.push(BookEvent::new(
            ts_event,
            sequence,
            book,
            generate_side(&mut rng, 10_000, bid_count),
            generate_side(&mut rng, 10_100, ask_count),
        ));
    }
    events
}

fn generate_side(rng: &mut Lcg, base_price: i64, count: usize) -> Vec<LevelChange> {
    let mut levels = Vec::with_capacity(count);
    let mut price = base_price + (rng.next_usize() % 50) as i64;
    for _ in 0..count {
        price += (rng.next_usize() % 3) as i64;
        let qty_a = (rng.next_usize() % 10_000) as i64;
        let qty_b = (rng.next_usize() % 1_000) as i64;
        levels.push(LevelChange::new(price, qty_a, qty_b));
    }
    levels
}

#[derive(Debug, Clone, Copy)]
struct Lcg(u64);

impl Lcg {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_usize(&mut self) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.0 >> 32) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_is_deterministic() {
        let config = SyntheticConfig { event_count: 4, ..SyntheticConfig::default() };

        assert_eq!(generate_events(config), generate_events(config));
    }

    #[test]
    fn generator_can_repeat_timestamps() {
        let events = generate_events(SyntheticConfig {
            event_count: 4,
            repeated_ts_run: 2,
            ..SyntheticConfig::default()
        });

        assert_eq!(events[0].ts_event, events[1].ts_event);
        assert_ne!(events[1].ts_event, events[2].ts_event);
    }
}
