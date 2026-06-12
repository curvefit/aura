use crate::bytes::{put_i64_le, put_u16_le, put_u32_le, put_u64_le, put_u8, ByteReader};
use crate::types::{BookEvent, BookId, LevelChange};
use crate::{AuraError, Result};

pub const MAGIC: &[u8; 4] = crate::format::WARM_MAGIC;
pub const VERSION: u16 = crate::format::FORMAT_VERSION;
pub const LEVEL_SIZE: usize = 24;

/// Tier 1 warm profile: resolved fixed-width values with exact level counts.
pub fn encode_events(events: &[BookEvent]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    put_u16_le(&mut out, VERSION);
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
        encode_levels(&event.bids, &mut out);
        encode_levels(&event.asks, &mut out);
    }
    Ok(out)
}

fn encode_levels(levels: &[LevelChange], out: &mut Vec<u8>) {
    for level in levels {
        put_i64_le(out, level.price);
        put_i64_le(out, level.qty_a);
        put_i64_le(out, level.qty_b);
    }
}

pub fn decode_events(bytes: &[u8]) -> Result<Vec<BookEvent>> {
    let mut reader = ByteReader::new(bytes);
    if reader.read_exact(4)? != MAGIC {
        return Err(AuraError::InvalidMagic { expected: "AUR1" });
    }
    let version = reader.read_u16_le()?;
    if version != VERSION {
        return Err(AuraError::UnsupportedVersion(version));
    }
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
        let bids = decode_levels(&mut reader, bid_count)?;
        let asks = decode_levels(&mut reader, ask_count)?;
        events.push(BookEvent::new(ts_event, sequence, book, bids, asks));
    }
    reader.finish()?;
    Ok(events)
}

fn decode_levels(reader: &mut ByteReader<'_>, count: usize) -> Result<Vec<LevelChange>> {
    let mut levels = Vec::with_capacity(count);
    for _ in 0..count {
        levels.push(LevelChange::new(
            reader.read_i64_le()?,
            reader.read_i64_le()?,
            reader.read_i64_le()?,
        ));
    }
    Ok(levels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_profile_round_trips_events() {
        let events = vec![
            BookEvent::new(
                100,
                10,
                BookId::BookA,
                vec![LevelChange::new(10, 1, 2)],
                vec![LevelChange::new(20, 3, 4)],
            ),
            BookEvent::new(200, 11, BookId::BookB, vec![], vec![LevelChange::delete(30)]),
        ];

        let encoded = encode_events(&events).unwrap();
        let decoded = decode_events(&encoded).unwrap();

        assert_eq!(events, decoded);
        assert_eq!(MAGIC, &encoded[..4]);
    }
}
