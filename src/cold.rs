use crate::bytes::ByteReader;
use crate::types::{BookEvent, BookId, LevelChange};
use crate::varint;
use crate::{AuraError, Result};

pub const MAGIC: &[u8; 4] = crate::format::COLD_MAGIC;
pub const VERSION: u16 = crate::format::FORMAT_VERSION;

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


/// One independently encoded cold chunk. Compression is intentionally external so
/// callers can choose zstd level and frame policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdChunk {
    pub first_event_index: u64,
    pub events: Vec<BookEvent>,
    pub encoded_payload: Vec<u8>,
}

impl ColdChunk {
    pub fn encode(first_event_index: u64, events: Vec<BookEvent>) -> Result<Self> {
        let encoded_payload = encode_events(&events)?;
        Ok(Self {
            first_event_index,
            events,
            encoded_payload,
        })
    }

    pub fn decode(first_event_index: u64, encoded_payload: Vec<u8>) -> Result<Self> {
        let events = decode_events(&encoded_payload)?;
        Ok(Self {
            first_event_index,
            events,
            encoded_payload,
        })
    }
}

pub fn encode_chunks(events: &[BookEvent], target_events_per_chunk: usize) -> Result<Vec<ColdChunk>> {
    let target = target_events_per_chunk.max(1);
    let mut chunks = Vec::new();
    for (idx, events) in events.chunks(target).enumerate() {
        let first_event_index = (idx * target) as u64;
        chunks.push(ColdChunk::encode(first_event_index, events.to_vec())?);
    }
    Ok(chunks)
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
    fn cold_chunks_round_trip_independently() {
        let events = vec![
            BookEvent::new(1, 1, BookId::BookA, vec![LevelChange::new(10, 1, 0)], vec![]),
            BookEvent::new(2, 2, BookId::BookA, vec![LevelChange::new(11, 1, 0)], vec![]),
            BookEvent::new(3, 3, BookId::BookB, vec![], vec![LevelChange::new(12, 1, 0)]),
        ];

        let chunks = encode_chunks(&events, 2).unwrap();

        assert_eq!(2, chunks.len());
        assert_eq!(0, chunks[0].first_event_index);
        assert_eq!(2, chunks[1].first_event_index);
        assert_eq!(events[..2], chunks[0].events);
        assert_eq!(events[2..], chunks[1].events);
    }

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
