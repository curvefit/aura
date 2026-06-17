use crate::chunk::ChunkDescriptor;
use crate::legacy::stats::EventStats;
use crate::legacy::BookEvent;

impl ChunkDescriptor {
    pub fn from_legacy_events(
        chunk_id: u32,
        first_event_index: u64,
        compressed_offset: u64,
        compressed_len: u64,
        uncompressed_len: u64,
        checksum: u32,
        events: &[BookEvent],
    ) -> Option<Self> {
        let stats = EventStats::from_events(events);
        Some(Self {
            chunk_id,
            first_event_index,
            event_count: u32::try_from(stats.event_count).ok()?,
            compressed_offset,
            compressed_len,
            uncompressed_len,
            first_ts_event: stats.first_ts_event?,
            last_ts_event: stats.last_ts_event?,
            first_sequence: stats.first_sequence?,
            last_sequence: stats.last_sequence?,
            checksum,
        })
    }
}

pub fn partition_events(events: &[BookEvent], target_events_per_chunk: usize) -> Vec<&[BookEvent]> {
    if events.is_empty() {
        return Vec::new();
    }
    let target = target_events_per_chunk.max(1);
    events.chunks(target).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::{BookId, LevelChange};

    #[test]
    fn partitions_events_by_target_count() {
        let events: Vec<_> = (0..5)
            .map(|idx| {
                BookEvent::new(
                    idx,
                    idx,
                    BookId::BookA,
                    vec![LevelChange::new(idx as i64, 1, 0)],
                    vec![],
                )
            })
            .collect();

        let chunks = partition_events(&events, 2);

        assert_eq!(3, chunks.len());
        assert_eq!(2, chunks[0].len());
        assert_eq!(1, chunks[2].len());
    }

    #[test]
    fn descriptor_captures_event_range() {
        let events = vec![
            BookEvent::new(10, 100, BookId::BookA, vec![], vec![]),
            BookEvent::new(20, 101, BookId::BookA, vec![], vec![]),
        ];

        let descriptor =
            ChunkDescriptor::from_legacy_events(3, 20, 1000, 50, 90, 7, &events).unwrap();

        assert_eq!(3, descriptor.chunk_id);
        assert_eq!(20, descriptor.first_event_index);
        assert_eq!(2, descriptor.event_count);
        assert_eq!(10, descriptor.first_ts_event);
        assert_eq!(101, descriptor.last_sequence);
    }
}
