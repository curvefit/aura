use crate::stats::padded_event_slots;
use crate::{AuraError, Result};

pub const MAGIC: &[u8; 4] = b"AUR3";
pub const VERSION: u16 = 1;
pub const EVENT_HEADER_SIZE: usize = 32;
pub const LEVEL_SIZE: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UltraLayout {
    pub block_size: u16,
}

impl UltraLayout {
    pub const fn new_unchecked(block_size: u16) -> Self {
        Self { block_size }
    }

    pub fn new(block_size: u16) -> Result<Self> {
        match block_size {
            1 | 2 | 4 | 8 | 10 | 16 | 20 | 32 => Ok(Self { block_size }),
            other => Err(AuraError::InvalidBlockSize(other)),
        }
    }

    pub fn padded_count(self, count: usize) -> usize {
        padded_event_slots(count, self.block_size)
    }

    pub fn event_bytes(self, bid_count: usize, ask_count: usize) -> usize {
        EVENT_HEADER_SIZE
            + self.padded_count(bid_count) * LEVEL_SIZE
            + self.padded_count(ask_count) * LEVEL_SIZE
    }
}

pub fn choose_largest_block_under_padding(
    bid_counts: &[usize],
    ask_counts: &[usize],
    candidates: &[u16],
    max_padding_ratio: f64,
) -> Result<UltraLayout> {
    let mut best = UltraLayout::new(1)?;
    for candidate in candidates {
        let layout = UltraLayout::new(*candidate)?;
        let real: usize = bid_counts.iter().sum::<usize>() + ask_counts.iter().sum::<usize>();
        let padded: usize = bid_counts
            .iter()
            .map(|count| layout.padded_count(*count))
            .sum::<usize>()
            + ask_counts
                .iter()
                .map(|count| layout.padded_count(*count))
                .sum::<usize>();
        let ratio = if real == 0 {
            1.0
        } else {
            padded as f64 / real as f64
        };
        if ratio <= max_padding_ratio && layout.block_size >= best.block_size {
            best = layout;
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ultra_layout_pads_levels_per_side() {
        let layout = UltraLayout::new(8).unwrap();

        assert_eq!(0, layout.padded_count(0));
        assert_eq!(8, layout.padded_count(1));
        assert_eq!(8, layout.padded_count(8));
        assert_eq!(16, layout.padded_count(9));
    }

    #[test]
    fn event_size_includes_padded_bid_and_ask_sections() {
        let layout = UltraLayout::new(8).unwrap();

        assert_eq!(EVENT_HEADER_SIZE + 24 * LEVEL_SIZE, layout.event_bytes(3, 11));
    }

    #[test]
    fn block_choice_respects_padding_budget() {
        let bids = [1, 2, 3, 4];
        let asks = [1, 2, 3, 4];

        let layout = choose_largest_block_under_padding(&bids, &asks, &[4, 8, 16], 2.0).unwrap();

        assert_eq!(4, layout.block_size);
    }
}
