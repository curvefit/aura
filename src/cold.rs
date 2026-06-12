use crate::bytes::ByteReader;
use crate::types::{BookEvent, BookId, LevelChange};
use crate::varint;
use crate::{AuraError, Result};

pub const MAGIC: &[u8; 4] = b"AUR0";
pub const VERSION: u16 = 1;

/// Tier 0 cold profile: compact delta stream before outer chunk compression.
pub fn encode_events(events: &[BookEvent]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    varint::encode_u64(events.len() as u64, &mut out);

    let mut prev_ts = 0i64;
    let mut prev_seq = 0i64;
    for event in events {
        event.validate_counts()?;
        out.push(event.book as u8);
        let ts = event.ts_event as i64;
        let seq = event.sequence as i64;
        varint::encode_i64(ts - prev_ts, &mut out);
        varint::encode_i64(seq - prev_seq, &mut out);
        prev_ts = ts;
        prev_seq = seq;
        varint::encode_u64(event.bids.len() as u64, &mut out);
        varint::encode_u64(event.asks.len() as u64, &mut out);
        encode_levels(&event.bids, &mut out);
        encode_levels(&event.asks, &mut out);
    }

    Ok(out)
}

fn encode_levels(levels: &[LevelChange], out: &mut Vec<u8>) {
    let mut prev_price = 0i64;
    for level in levels {
        varint::encode_i64(level.price - prev_price, out);
        varint::encode_i64(level.qty_a, out);
        varint::encode_i64(level.qty_b, out);
        prev_price = level.price;
    }
}

pub fn decode_events(bytes: &[u8]) -> Result<Vec<BookEvent>> {
    let mut reader = ByteReader::new(bytes);
    if reader.read_exact(4)? != MAGIC {
        return Err(AuraError::InvalidMagic { expected: "AUR0" });
    }
    let version = reader.read_u16_le()?;
    if version != VERSION {
        return Err(AuraError::UnsupportedVersion(version));
    }
    let event_count = varint::decode_u64(&mut reader)?;
    let mut events = Vec::with_capacity(event_count as usize);
    let mut prev_ts = 0i64;
    let mut prev_seq = 0i64;

    for _ in 0..event_count {
        let book = BookId::from_byte(reader.read_u8()?)?;
        let ts = prev_ts + varint::decode_i64(&mut reader)?;
        let seq = prev_seq + varint::decode_i64(&mut reader)?;
        if ts < 0 || seq < 0 {
            return Err(AuraError::InvalidValue("negative timestamp or sequence"));
        }
        prev_ts = ts;
        prev_seq = seq;
        let bid_count = usize::try_from(varint::decode_u64(&mut reader)?)
            .map_err(|_| AuraError::InvalidValue("bid count"))?;
        let ask_count = usize::try_from(varint::decode_u64(&mut reader)?)
            .map_err(|_| AuraError::InvalidValue("ask count"))?;
        let bids = decode_levels(&mut reader, bid_count)?;
        let asks = decode_levels(&mut reader, ask_count)?;
        events.push(BookEvent::new(ts as u64, seq as u64, book, bids, asks));
    }
    reader.finish()?;
    Ok(events)
}

fn decode_levels(reader: &mut ByteReader<'_>, count: usize) -> Result<Vec<LevelChange>> {
    let mut levels = Vec::with_capacity(count);
    let mut prev_price = 0i64;
    for _ in 0..count {
        let price = prev_price + varint::decode_i64(reader)?;
        let qty_a = varint::decode_i64(reader)?;
        let qty_b = varint::decode_i64(reader)?;
        levels.push(LevelChange::new(price, qty_a, qty_b));
        prev_price = price;
    }
    Ok(levels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_profile_round_trips_events() {
        let events = vec![
            BookEvent::new(
                1000,
                10,
                BookId::BookA,
                vec![LevelChange::new(100, 5, 1), LevelChange::new(101, 6, 0)],
                vec![LevelChange::delete(110)],
            ),
            BookEvent::new(
                1100,
                11,
                BookId::BookB,
                vec![LevelChange::new(99, 7, 2)],
                vec![],
            ),
        ];

        let encoded = encode_events(&events).unwrap();
        let decoded = decode_events(&encoded).unwrap();

        assert_eq!(events, decoded);
        assert!(encoded.len() < 120);
    }
}
