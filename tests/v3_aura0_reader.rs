use std::io::{Cursor, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use aura_codec::{V3FlatAura0Reader, V3FlatLimits, V3FlatReaderState, V3ValueLimits};

const FILE: &[u8] = include_bytes!("fixtures/v3-container/three-row-two-chunk.aura0");

#[test]
fn open_checks_metadata_but_only_verify_makes_complete_claim() {
    let mut reader = V3FlatAura0Reader::open(Cursor::new(FILE)).unwrap();
    assert_eq!(V3FlatReaderState::Opened, reader.state());
    assert!(!reader.is_fully_verified());
    assert_eq!(3, reader.embedded_footer().record_count);
    let summary = reader.verify_all().unwrap();
    assert_eq!(3, summary.record_count);
    assert_eq!(2, summary.chunk_count);
    assert_eq!(V3FlatReaderState::Verified, reader.state());
    assert!(reader.is_fully_verified());
}

#[test]
fn read_all_returns_exact_batches_after_full_verification() {
    let mut reader = V3FlatAura0Reader::open(Cursor::new(FILE)).unwrap();
    let decoded = reader.read_all().unwrap();
    assert_eq!(2, decoded.batches.len());
    assert_eq!(2, decoded.batches[0].row_count);
    assert_eq!(1, decoded.batches[1].row_count);
    assert_eq!(V3FlatReaderState::Verified, reader.state());
}

#[test]
fn body_mutation_opens_but_fails_streaming_verification() {
    let header_len = aura_codec::AuraHeader::encoded_len(FILE).unwrap();
    let mut bytes = FILE.to_vec();
    bytes[header_len + 70] ^= 1;
    let mut reader = V3FlatAura0Reader::open(Cursor::new(bytes.clone())).unwrap();
    assert_eq!(V3FlatReaderState::Opened, reader.state());
    assert!(reader.verify_all().is_err());
    assert!(!reader.is_fully_verified());

    let mut callbacks = 0;
    let mut reader = V3FlatAura0Reader::open(Cursor::new(bytes)).unwrap();
    assert!(reader
        .verify_with(|_| {
            callbacks += 1;
            Ok(())
        })
        .is_err());
    assert_eq!(0, callbacks);
}

#[test]
fn trailer_header_footer_truncation_and_limits_fail_at_open() {
    for end in 0..FILE.len() {
        assert!(V3FlatAura0Reader::open(Cursor::new(&FILE[..end])).is_err());
    }
    let mut header = FILE.to_vec();
    header[39] ^= 1;
    assert!(V3FlatAura0Reader::open(Cursor::new(header)).is_err());
    let mut footer = FILE.to_vec();
    footer[FILE.len() - 13] ^= 1;
    assert!(V3FlatAura0Reader::open(Cursor::new(footer)).is_err());

    let opened = V3FlatAura0Reader::open(Cursor::new(FILE)).unwrap();
    let footer_len = FILE.len() as u64 - 12 - opened.body_range().1;
    let body_len = opened.embedded_footer().body_len;
    drop(opened);
    for limits in [
        V3FlatLimits {
            max_footer_bytes: footer_len as usize - 1,
            ..V3FlatLimits::default()
        },
        V3FlatLimits {
            max_body_bytes: body_len - 1,
            ..V3FlatLimits::default()
        },
        V3FlatLimits {
            max_chunks: 1,
            ..V3FlatLimits::default()
        },
        V3FlatLimits {
            max_rows: 2,
            ..V3FlatLimits::default()
        },
        V3FlatLimits {
            value_limits: V3ValueLimits {
                max_block_bytes: 243,
                ..V3ValueLimits::HARD
            },
            ..V3FlatLimits::default()
        },
    ] {
        assert!(V3FlatAura0Reader::open_with_limits(Cursor::new(FILE), limits).is_err());
    }
}

#[derive(Clone)]
struct SharedCursor(Arc<Mutex<Cursor<Vec<u8>>>>);

impl Read for SharedCursor {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().read(bytes)
    }
}

struct EarlierChunkMutator {
    cursor: Cursor<Vec<u8>>,
    armed: Arc<AtomicBool>,
    mutated: bool,
    second_chunk_start: u64,
    first_chunk_byte: usize,
}

impl Read for EarlierChunkMutator {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.cursor.read(bytes)
    }
}

impl Seek for EarlierChunkMutator {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        if self.armed.load(Ordering::SeqCst)
            && !self.mutated
            && position == SeekFrom::Start(self.second_chunk_start)
        {
            self.cursor.get_mut()[self.first_chunk_byte] ^= 1;
            self.mutated = true;
        }
        self.cursor.seek(position)
    }
}

impl Seek for SharedCursor {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.0.lock().unwrap().seek(position)
    }
}

#[test]
fn held_stream_in_place_mutation_and_length_change_are_detected() {
    let shared = Arc::new(Mutex::new(Cursor::new(FILE.to_vec())));
    let mut reader = V3FlatAura0Reader::open(SharedCursor(shared.clone())).unwrap();
    let header_len = reader.header_len() as usize;
    shared.lock().unwrap().get_mut()[header_len + 70] ^= 1;
    assert!(reader.verify_all().is_err());

    let shared = Arc::new(Mutex::new(Cursor::new(FILE.to_vec())));
    let mut reader = V3FlatAura0Reader::open(SharedCursor(shared.clone())).unwrap();
    shared.lock().unwrap().get_mut().push(0);
    assert!(reader.verify_all().is_err());
    assert!(!reader.is_fully_verified());

    let shared = Arc::new(Mutex::new(Cursor::new(FILE.to_vec())));
    let mut reader = V3FlatAura0Reader::open(SharedCursor(shared.clone())).unwrap();
    shared.lock().unwrap().get_mut()[FILE.len() - 13] ^= 1;
    assert!(reader.verify_all().is_err());
    assert!(!reader.is_fully_verified());
}

#[test]
fn full_body_rehash_detects_earlier_chunk_mutated_after_it_was_read() {
    let decoded = aura_codec::decode_v3_flat_aura0(FILE).unwrap();
    let header_len = aura_codec::AuraHeader::encoded_len(FILE).unwrap();
    let second_chunk_start = header_len as u64 + decoded.footer.chunks[1].body_relative_offset;
    let armed = Arc::new(AtomicBool::new(false));
    let stream = EarlierChunkMutator {
        cursor: Cursor::new(FILE.to_vec()),
        armed: armed.clone(),
        mutated: false,
        second_chunk_start,
        first_chunk_byte: header_len + 70,
    };
    let mut reader = V3FlatAura0Reader::open(stream).unwrap();
    armed.store(true, Ordering::SeqCst);
    assert!(reader.verify_all().is_err());
    assert!(!reader.is_fully_verified());
}
