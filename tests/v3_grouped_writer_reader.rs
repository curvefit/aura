use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use aura_codec::{
    AuraV3Column, AuraV3ColumnValues as Values, AuraV3EventBatch, FieldRole, FieldType,
    RelationshipPermissions, SchemaBuilder, V3GroupedAura0Reader, V3GroupedAura0Writer,
    V3GroupedReaderState, V3GroupedWriterOptions, V3GroupedWriterState,
};

fn permissions() -> RelationshipPermissions {
    RelationshipPermissions::none()
        .with_split()
        .with_within_domain()
}

fn schema() -> aura_codec::SchemaDescriptor {
    let schema = SchemaBuilder::new("grouped_writer_reader")
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .field("seq", FieldType::U64, FieldRole::Sequence)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("price", FieldType::I64, FieldRole::Price)
        .dual_domain_repeated_group(1, vec![2, 3], 2, permissions())
        .finish()
        .unwrap();
    let groups = schema.groups.clone();
    schema.with_v3_groups(groups).unwrap()
}

fn batch(
    schema_id: u32,
    timestamps: &[i64],
    sequences: &[u64],
    offsets: &[u32],
    sides: &[u8],
    prices: &[i64],
) -> AuraV3EventBatch {
    AuraV3EventBatch {
        schema_id,
        event_count: timestamps.len() as u32,
        child_offsets: offsets.to_vec(),
        event_columns: vec![
            AuraV3Column {
                slot: 0,
                validity: None,
                values: Values::TimestampMs(timestamps.to_vec()),
            },
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::U64(sequences.to_vec()),
            },
        ],
        repeated_columns: vec![
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::U8(sides.to_vec()),
            },
            AuraV3Column {
                slot: 3,
                validity: None,
                values: Values::I64(prices.to_vec()),
            },
        ],
    }
}

fn batches(schema_id: u32) -> Vec<AuraV3EventBatch> {
    vec![
        batch(schema_id, &[10], &[1], &[0, 0], &[], &[]),
        batch(
            schema_id,
            &[20, 30],
            &[2, 3],
            &[0, 1, 3],
            &[0, 1, 0],
            &[100, 101, 102],
        ),
    ]
}

fn file() -> Vec<u8> {
    let schema = schema();
    let mut writer = V3GroupedAura0Writer::try_new(
        Cursor::new(Vec::new()),
        schema.clone(),
        V3GroupedWriterOptions::default(),
    )
    .unwrap();
    for batch in batches(schema.schema_id) {
        writer.write_batch(&batch).unwrap();
    }
    writer.finish().unwrap().0.into_inner()
}

#[test]
fn seekable_reader_locates_and_verifies_chunks_without_overclaiming() {
    let bytes = file();
    let mut reader = V3GroupedAura0Reader::open(Cursor::new(bytes)).unwrap();
    assert_eq!(reader.state(), V3GroupedReaderState::Opened);
    assert!(!reader.is_fully_verified());
    assert_eq!(reader.chunk_for_event(0).unwrap().chunk_id, 0);
    assert_eq!(reader.chunk_for_event(1).unwrap().chunk_id, 1);
    assert_eq!(reader.chunk_for_event(2).unwrap().chunk_id, 1);
    assert!(reader.chunk_for_event(3).is_none());
    assert_eq!(reader.chunk_for_child(0).unwrap().chunk_id, 1);
    assert_eq!(reader.chunk_for_child(2).unwrap().chunk_id, 1);
    assert!(reader.chunk_for_child(3).is_none());
    let chunk = reader.read_chunk(1).unwrap();
    assert_eq!(chunk.batch.event_count, 2);
    assert_eq!(chunk.batch.child_count(), 3);
    assert_eq!(reader.state(), V3GroupedReaderState::Opened);
    let summary = reader.verify_all().unwrap();
    assert_eq!(summary.event_count, 3);
    assert_eq!(summary.child_count, 3);
    assert_eq!(summary.chunk_count, 2);
    assert_eq!(reader.state(), V3GroupedReaderState::Verified);
}

#[derive(Clone)]
struct SharedCursor(Arc<Mutex<Cursor<Vec<u8>>>>);

impl Read for SharedCursor {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.lock().unwrap().read(bytes)
    }
}

impl Seek for SharedCursor {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.0.lock().unwrap().seek(position)
    }
}

#[test]
fn reader_detects_body_footer_and_length_mutation_after_open() {
    let bytes = file();
    let shared = Arc::new(Mutex::new(Cursor::new(bytes.clone())));
    let mut reader = V3GroupedAura0Reader::open(SharedCursor(shared.clone())).unwrap();
    let header_len = reader.header_len() as usize;
    shared.lock().unwrap().get_mut()[header_len + 80] ^= 1;
    assert!(reader.verify_all().is_err());
    assert!(!reader.is_fully_verified());

    let shared = Arc::new(Mutex::new(Cursor::new(bytes.clone())));
    let mut reader = V3GroupedAura0Reader::open(SharedCursor(shared.clone())).unwrap();
    shared.lock().unwrap().get_mut().push(0);
    assert!(reader.verify_all().is_err());

    let shared = Arc::new(Mutex::new(Cursor::new(bytes.clone())));
    let mut reader = V3GroupedAura0Reader::open(SharedCursor(shared.clone())).unwrap();
    let last_footer_byte = bytes.len() - 13;
    shared.lock().unwrap().get_mut()[last_footer_byte] ^= 1;
    assert!(reader.verify_all().is_err());
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
fn partial_body_write_poisons_writer_and_never_seals() {
    let schema = schema();
    let valid = batches(schema.schema_id).remove(1);
    let header_len = aura_codec::AuraHeader::encoded_len(&file()).unwrap();
    let (stream, shared) = FailingStream::new(header_len + 5);
    let mut writer =
        V3GroupedAura0Writer::try_new(stream, schema, V3GroupedWriterOptions::default()).unwrap();
    assert!(writer.write_batch(&valid).is_err());
    assert_eq!(writer.state(), V3GroupedWriterState::Poisoned);
    assert!(writer.write_batch(&valid).is_err());
    assert!(!shared.lock().unwrap().get_ref().ends_with(b"sealed:)"));
}

#[test]
fn every_partial_write_boundary_fails_without_a_complete_seal() {
    let expected = file();
    let schema = schema();
    let batches = batches(schema.schema_id);
    for fail_after in 0..expected.len() {
        let (stream, shared) = FailingStream::new(fail_after);
        let result = V3GroupedAura0Writer::try_new(
            stream,
            schema.clone(),
            V3GroupedWriterOptions::default(),
        );
        let succeeded = match result {
            Err(_) => false,
            Ok(mut writer) => {
                let mut okay = true;
                for batch in &batches {
                    if writer.write_batch(batch).is_err() {
                        okay = false;
                        break;
                    }
                }
                okay && writer.finish().is_ok()
            }
        };
        assert!(!succeeded, "write boundary {fail_after}");
        assert!(
            !shared.lock().unwrap().get_ref().ends_with(b"sealed:)"),
            "write boundary {fail_after}"
        );
    }
}

#[test]
fn multiple_signed_sequence_capabilities_do_not_block_exact_container() {
    let mut builder = SchemaBuilder::new("grouped_signed_sequence_capabilities").field(
        "ts",
        FieldType::TimestampMs,
        FieldRole::Timestamp,
    );
    for name in ["seq_a", "seq_b", "seq_c", "seq_okx"] {
        builder = builder.nullable_field(name, FieldType::I64, FieldRole::Sequence);
    }
    let schema = builder
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("price", FieldType::I64, FieldRole::Price)
        .dual_domain_repeated_group(1, vec![5, 6], 5, permissions())
        .finish()
        .unwrap();
    let groups = schema.groups.clone();
    let schema = schema.with_v3_groups(groups).unwrap();
    let mut event_columns = vec![AuraV3Column {
        slot: 0,
        validity: None,
        values: Values::TimestampMs(vec![1]),
    }];
    for (slot, value) in [1i64, 2, 3, i64::MIN].into_iter().enumerate() {
        event_columns.push(AuraV3Column {
            slot: slot as u16 + 1,
            validity: Some(vec![1]),
            values: Values::I64(vec![value]),
        });
    }
    let batch = AuraV3EventBatch {
        schema_id: schema.schema_id,
        event_count: 1,
        child_offsets: vec![0, 1],
        event_columns,
        repeated_columns: vec![
            AuraV3Column {
                slot: 5,
                validity: None,
                values: Values::U8(vec![0]),
            },
            AuraV3Column {
                slot: 6,
                validity: None,
                values: Values::I64(vec![100]),
            },
        ],
    };
    let mut writer = V3GroupedAura0Writer::try_new(
        Cursor::new(Vec::new()),
        schema,
        V3GroupedWriterOptions::default(),
    )
    .unwrap();
    writer.write_batch(&batch).unwrap();
    let bytes = writer.finish().unwrap().0.into_inner();
    let decoded = aura_codec::decode_v3_grouped_aura0(&bytes).unwrap();
    assert_eq!(decoded.footer.primary_sequence_slot, None);
    assert!(!decoded.footer.chunks[0].has_sequence_bounds());
    assert_eq!(decoded.batches[0], batch);
}
