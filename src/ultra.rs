use crate::bytes::{put_i64_le, put_u16_le, put_u32_le, put_u64_le, put_u8, ByteReader};
use crate::stats::padded_event_slots;
use crate::types::{BookEvent, BookId, LevelChange};
use crate::{AuraError, Result};

pub const MAGIC: &[u8; 4] = crate::format::AURA1_MAGIC;
pub const VERSION: u16 = crate::format::FORMAT_VERSION;
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

/// Aura1 prototype: one fixed event header plus padded fixed-width levels.
pub fn encode_events(events: &[BookEvent], layout: UltraLayout) -> Result<Vec<u8>> {
    UltraLayout::new(layout.block_size)?;
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    put_u16_le(&mut out, VERSION);
    put_u16_le(&mut out, layout.block_size);
    put_u64_le(&mut out, events.len() as u64);
    for event in events {
        event.validate_counts()?;
        put_u8(&mut out, event.book as u8);
        put_u8(&mut out, 0);
        put_u16_le(&mut out, 0);
        put_u32_le(&mut out, event.bids.len() as u32);
        put_u32_le(&mut out, event.asks.len() as u32);
        put_u64_le(&mut out, event.ts_event);
        put_u64_le(&mut out, event.sequence);
        put_u32_le(&mut out, 0);
        encode_padded_levels(&event.bids, layout, &mut out);
        encode_padded_levels(&event.asks, layout, &mut out);
    }
    Ok(out)
}

fn encode_padded_levels(levels: &[LevelChange], layout: UltraLayout, out: &mut Vec<u8>) {
    for level in levels {
        encode_level(*level, out);
    }
    for _ in levels.len()..layout.padded_count(levels.len()) {
        encode_level(LevelChange::new(0, 0, 0), out);
    }
}

fn encode_level(level: LevelChange, out: &mut Vec<u8>) {
    put_i64_le(out, level.price);
    put_i64_le(out, level.qty_a);
    put_i64_le(out, level.qty_b);
}

pub fn decode_events(bytes: &[u8]) -> Result<(UltraLayout, Vec<BookEvent>)> {
    let mut reader = ByteReader::new(bytes);
    if reader.read_exact(4)? != MAGIC {
        return Err(AuraError::InvalidMagic { expected: "AUR1" });
    }
    let version = reader.read_u16_le()?;
    if version != VERSION {
        return Err(AuraError::UnsupportedVersion(version));
    }
    let layout = UltraLayout::new(reader.read_u16_le()?)?;
    let event_count = reader.read_u64_le()?;
    let mut events = Vec::with_capacity(event_count as usize);
    for _ in 0..event_count {
        let book = BookId::from_byte(reader.read_u8()?)?;
        let _flags = reader.read_u8()?;
        let _reserved = reader.read_u16_le()?;
        let bid_count = reader.read_u32_le()? as usize;
        let ask_count = reader.read_u32_le()? as usize;
        let ts_event = reader.read_u64_le()?;
        let sequence = reader.read_u64_le()?;
        let _header_padding = reader.read_u32_le()?;
        let bids = decode_padded_levels(&mut reader, bid_count, layout)?;
        let asks = decode_padded_levels(&mut reader, ask_count, layout)?;
        events.push(BookEvent::new(ts_event, sequence, book, bids, asks));
    }
    reader.finish()?;
    Ok((layout, events))
}

fn decode_padded_levels(
    reader: &mut ByteReader<'_>,
    real_count: usize,
    layout: UltraLayout,
) -> Result<Vec<LevelChange>> {
    let padded = layout.padded_count(real_count);
    let mut levels = Vec::with_capacity(real_count);
    for idx in 0..padded {
        let level = LevelChange::new(
            reader.read_i64_le()?,
            reader.read_i64_le()?,
            reader.read_i64_le()?,
        );
        if idx < real_count {
            levels.push(level);
        }
    }
    Ok(levels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ultra_profile_round_trips_padded_events() {
        let layout = UltraLayout::new(4).unwrap();
        let events = vec![BookEvent::new(
            100,
            7,
            BookId::BookA,
            vec![LevelChange::new(10, 1, 0)],
            vec![LevelChange::new(20, 2, 0), LevelChange::new(21, 3, 0)],
        )];

        let encoded = encode_events(&events, layout).unwrap();
        let (decoded_layout, decoded) = decode_events(&encoded).unwrap();

        assert_eq!(layout, decoded_layout);
        assert_eq!(events, decoded);
        assert_eq!(
            4 + 2 + 2 + 8 + EVENT_HEADER_SIZE + 8 * LEVEL_SIZE,
            encoded.len()
        );
    }

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

        assert_eq!(
            EVENT_HEADER_SIZE + 24 * LEVEL_SIZE,
            layout.event_bytes(3, 11)
        );
    }

    #[test]
    fn block_choice_respects_padding_budget() {
        let bids = [1, 2, 3, 4];
        let asks = [1, 2, 3, 4];

        let layout = choose_largest_block_under_padding(&bids, &asks, &[4, 8, 16], 2.0).unwrap();

        assert_eq!(4, layout.block_size);
    }
}
