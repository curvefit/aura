use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use aura_codec::experimental::{
    decode_v3_flat_aura0, V3FlatAura0Writer, V3FlatWriterOptions, V3FlatWriterState,
};
use aura_codec::parse_schema_json;

fn schema() -> aura_codec::SchemaDescriptor {
    parse_schema_json(include_str!("fixtures/v3-container/anonymous.schema.json")).unwrap()
}

#[test]
fn writer_matches_checked_empty_and_two_chunk_files_exactly() {
    let schema = schema();
    let (cursor, empty_summary) = V3FlatAura0Writer::try_new(
        Cursor::new(Vec::new()),
        schema.clone(),
        V3FlatWriterOptions::default(),
    )
    .unwrap()
    .finish()
    .unwrap();
    assert_eq!(0, empty_summary.record_count);
    assert_eq!(
        include_bytes!("fixtures/v3-container/empty-flat.aura0").as_slice(),
        cursor.into_inner()
    );

    let expected = include_bytes!("fixtures/v3-container/three-row-two-chunk.aura0");
    let decoded = decode_v3_flat_aura0(expected).unwrap();
    let mut writer = V3FlatAura0Writer::try_new(
        Cursor::new(Vec::new()),
        schema,
        V3FlatWriterOptions::default(),
    )
    .unwrap();
    assert_eq!(V3FlatWriterState::Open, writer.state());
    for batch in &decoded.batches {
        writer.write_batch(batch).unwrap();
    }
    assert_eq!(3, writer.record_count());
    assert_eq!(2, writer.chunk_count());
    let (cursor, summary) = writer.finish().unwrap();
    assert_eq!(3, summary.record_count);
    assert_eq!(2, summary.chunk_count);
    assert_eq!(expected.as_slice(), cursor.into_inner());
}

#[test]
fn zero_row_batches_and_wrong_schema_are_rejected_before_append() {
    let schema = schema();
    let decoded = decode_v3_flat_aura0(include_bytes!(
        "fixtures/v3-container/three-row-two-chunk.aura0"
    ))
    .unwrap();
    let mut writer = V3FlatAura0Writer::try_new(
        Cursor::new(Vec::new()),
        schema,
        V3FlatWriterOptions::default(),
    )
    .unwrap();
    let mut empty = decoded.batches[0].clone();
    empty.row_count = 0;
    assert!(writer.write_batch(&empty).is_err());
    let mut wrong = decoded.batches[0].clone();
    wrong.schema_id ^= 1;
    assert!(writer.write_batch(&wrong).is_err());
    assert_eq!(0, writer.record_count());
    writer.write_batch(&decoded.batches[0]).unwrap();
}

#[derive(Clone)]
struct FailingStream {
    shared: Arc<Mutex<Cursor<Vec<u8>>>>,
    fail_after: usize,
    written: usize,
}

impl FailingStream {
    fn new(fail_after: usize) -> (Self, Arc<Mutex<Cursor<Vec<u8>>>>) {
        let shared = Arc::new(Mutex::new(Cursor::new(Vec::new())));
        (
            Self {
                shared: shared.clone(),
                fail_after,
                written: 0,
            },
            shared,
        )
    }
}

impl Read for FailingStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.shared.lock().unwrap().read(bytes)
    }
}

impl Seek for FailingStream {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.shared.lock().unwrap().seek(position)
    }
}

impl Write for FailingStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.written >= self.fail_after {
            return Err(io::Error::other("injected write"));
        }
        let accepted = bytes.len().min(self.fail_after - self.written);
        let count = self.shared.lock().unwrap().write(&bytes[..accepted])?;
        self.written += count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn every_partial_write_boundary_fails_and_never_seals() {
    let expected = include_bytes!("fixtures/v3-container/three-row-two-chunk.aura0");
    let decoded = decode_v3_flat_aura0(expected).unwrap();
    for fail_after in 0..expected.len() {
        let (stream, shared) = FailingStream::new(fail_after);
        let result = V3FlatAura0Writer::try_new(stream, schema(), V3FlatWriterOptions::default());
        let succeeded = match result {
            Err(_) => false,
            Ok(mut writer) => {
                let mut okay = true;
                for batch in &decoded.batches {
                    if writer.write_batch(batch).is_err() {
                        okay = false;
                        break;
                    }
                }
                okay && writer.finish().is_ok()
            }
        };
        assert!(!succeeded, "write boundary {fail_after}");
        let bytes = shared.lock().unwrap().get_ref().clone();
        assert!(!bytes.ends_with(b"sealed:)"), "write boundary {fail_after}");
    }
}

#[test]
fn partial_body_write_poisons_writer_and_refuses_reuse() {
    let expected = include_bytes!("fixtures/v3-container/three-row-two-chunk.aura0");
    let decoded = decode_v3_flat_aura0(expected).unwrap();
    let header_len = aura_codec::AuraHeader::encoded_len(expected).unwrap();
    let (stream, _) = FailingStream::new(header_len + 5);
    let mut writer =
        V3FlatAura0Writer::try_new(stream, schema(), V3FlatWriterOptions::default()).unwrap();
    assert!(writer.write_batch(&decoded.batches[0]).is_err());
    assert_eq!(V3FlatWriterState::Poisoned, writer.state());
    assert!(writer.write_batch(&decoded.batches[0]).is_err());
}
