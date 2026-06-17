use crate::legacy::BookEvent;
use crate::stats::round_up;

/// Legacy fixed-event summary used by quarantined replay prototypes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventStats {
    pub event_count: u64,
    pub bid_level_count: u64,
    pub ask_level_count: u64,
    pub max_bid_count: u32,
    pub max_ask_count: u32,
    pub first_ts_event: Option<u64>,
    pub last_ts_event: Option<u64>,
    pub first_sequence: Option<u64>,
    pub last_sequence: Option<u64>,
}

impl EventStats {
    pub fn observe(&mut self, event: &BookEvent) {
        let bid_count = event.bids.len() as u32;
        let ask_count = event.asks.len() as u32;
        self.event_count += 1;
        self.bid_level_count += u64::from(bid_count);
        self.ask_level_count += u64::from(ask_count);
        self.max_bid_count = self.max_bid_count.max(bid_count);
        self.max_ask_count = self.max_ask_count.max(ask_count);
        self.first_ts_event.get_or_insert(event.ts_event);
        self.last_ts_event = Some(event.ts_event);
        self.first_sequence.get_or_insert(event.sequence);
        self.last_sequence = Some(event.sequence);
    }

    pub fn from_events(events: &[BookEvent]) -> Self {
        let mut stats = Self::default();
        for event in events {
            stats.observe(event);
        }
        stats
    }

    pub fn level_count(&self) -> u64 {
        self.bid_level_count + self.ask_level_count
    }

    pub fn padded_slots(&self, block_size: u16) -> u64 {
        if block_size == 0 {
            return self.level_count();
        }
        let block = u64::from(block_size);
        round_up(self.bid_level_count, block) + round_up(self.ask_level_count, block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::{BookId, LevelChange};

    #[test]
    fn stats_track_counts_and_edges() {
        let events = vec![
            BookEvent::new(
                100,
                7,
                BookId::BookA,
                vec![LevelChange::new(10, 1, 0)],
                vec![],
            ),
            BookEvent::new(
                200,
                8,
                BookId::BookA,
                vec![LevelChange::new(11, 2, 0), LevelChange::new(12, 3, 0)],
                vec![LevelChange::new(13, 4, 0)],
            ),
        ];

        let stats = EventStats::from_events(&events);

        assert_eq!(2, stats.event_count);
        assert_eq!(3, stats.bid_level_count);
        assert_eq!(1, stats.ask_level_count);
        assert_eq!(2, stats.max_bid_count);
        assert_eq!(Some(100), stats.first_ts_event);
        assert_eq!(Some(200), stats.last_ts_event);
    }
}
