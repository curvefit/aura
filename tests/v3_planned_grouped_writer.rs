use aura_codec::experimental::GroupedSearch;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aura_codec::experimental::{
    decode_v3_planned_grouped, AuraV3ColumnValues as Values, AuraV3EventBatch, V3GroupedLimits,
    V3PlannedGroupedCreateOnceWriter, V3PlannedGroupedIngestWriter,
    V3PlannedGroupedPublicationOutcome, V3PlannedGroupedWriterOptions, V3PlannedGroupedWriterState,
    AURA_PLAN_V2_DOMAIN0_FROM_DOMAIN1_OP, AURA_PLAN_V2_DOMAIN1_FROM_DOMAIN0_OP,
};

#[path = "support/grouped_fixture.rs"]
mod fixture;
use fixture::{batch, cross_only_schema, paired_batch, schema};

#[test]
fn incremental_writer_matches_cross_domain_reference_complete_bytes() {
    let schema = schema("incremental_reference");
    let batches = vec![batch(schema.schema_id, 0, 2), batch(schema.schema_id, 2, 2)];
    let reference = GroupedSearch::CrossDomain
        .compile(&schema, &batches, Default::default())
        .unwrap();
    let mut writer = V3PlannedGroupedIngestWriter::try_new(
        Cursor::new(Vec::new()),
        schema,
        V3PlannedGroupedWriterOptions::default(),
    )
    .unwrap();
    for batch in &batches {
        writer.write_batch(batch).unwrap();
    }
    assert_eq!(writer.state(), V3PlannedGroupedWriterState::Open);
    assert_eq!(writer.chunk_count(), 2);
    let (_, artifact) = writer.finish().unwrap();
    assert_eq!(artifact.bytes, reference.bytes);
    assert_eq!(artifact.summary, reference.summary);
    assert_eq!(artifact.inspection.plan, reference.inspection.plan);
}

#[test]
fn writer_rechunking_has_stable_plan_and_logical_identity() {
    let schema = schema("incremental_rechunk");
    let whole = batch(schema.schema_id, 0, 4);
    let split = vec![batch(schema.schema_id, 0, 2), batch(schema.schema_id, 2, 2)];
    let compile = |batches: &[AuraV3EventBatch]| {
        let mut writer = V3PlannedGroupedIngestWriter::try_new(
            Cursor::new(Vec::new()),
            schema.clone(),
            Default::default(),
        )
        .unwrap();
        for batch in batches {
            writer.write_batch(batch).unwrap();
        }
        writer.finish().unwrap().1
    };
    let one = compile(&[whole]);
    let two = compile(&split);
    assert_eq!(one.summary.plan_sha256, two.summary.plan_sha256);
    assert_eq!(
        one.summary.global_logical_sha256,
        two.summary.global_logical_sha256
    );
    assert_eq!(one.inspection.plan.selected, two.inspection.plan.selected);
}

#[test]
fn writer_selects_both_cross_orientations_and_direct_overflow_fallback() {
    for (domain0_is_cheaper, expected_op) in [
        (true, AURA_PLAN_V2_DOMAIN1_FROM_DOMAIN0_OP),
        (false, AURA_PLAN_V2_DOMAIN0_FROM_DOMAIN1_OP),
    ] {
        let schema = cross_only_schema(if domain0_is_cheaper {
            "writer_cross_one"
        } else {
            "writer_cross_zero"
        });
        let batch = paired_batch(schema.schema_id, domain0_is_cheaper);
        let mut writer = V3PlannedGroupedIngestWriter::try_new(
            Cursor::new(Vec::new()),
            schema,
            Default::default(),
        )
        .unwrap();
        writer.write_batch(&batch).unwrap();
        let artifact = writer.finish().unwrap().1;
        let decoded = decode_v3_planned_grouped(&artifact.bytes, V3GroupedLimits::HARD).unwrap();
        assert_eq!(decoded.footer.plan.streams[2].op, expected_op);
        assert_eq!(decoded.batches, vec![batch]);
    }

    let schema = cross_only_schema("writer_cross_overflow");
    let mut overflow = paired_batch(schema.schema_id, true);
    overflow.child_offsets = vec![0, 2];
    overflow.repeated_columns[0].values = Values::U8(vec![0, 1]);
    overflow.repeated_columns[1].values = Values::I64(vec![i64::MIN, i64::MAX]);
    overflow.repeated_columns[2].validity = Some(vec![0]);
    overflow.repeated_columns[2].values = Values::I64(vec![0, 0]);
    overflow.repeated_columns[3].validity = Some(vec![0]);
    overflow.repeated_columns[3].values = Values::U32(vec![0, 0]);
    let mut writer =
        V3PlannedGroupedIngestWriter::try_new(Cursor::new(Vec::new()), schema, Default::default())
            .unwrap();
    writer.write_batch(&overflow).unwrap();
    let artifact = writer.finish().unwrap().1;
    let decoded = decode_v3_planned_grouped(&artifact.bytes, V3GroupedLimits::HARD).unwrap();
    assert_eq!(decoded.footer.plan.streams[2].op, 0);
    assert_eq!(decoded.batches, vec![overflow]);
}

#[test]
fn unsealed_truncated_and_mutated_scratch_never_decodes_as_final() {
    let schema = schema("incremental_incomplete");
    let batch = batch(schema.schema_id, 0, 2);
    let mut writer =
        V3PlannedGroupedIngestWriter::try_new(Cursor::new(Vec::new()), schema, Default::default())
            .unwrap();
    writer.write_batch(&batch).unwrap();
    let (scratch, _) = writer.finish().unwrap();
    let scratch = scratch.into_inner();
    assert!(decode_v3_planned_grouped(&scratch, V3GroupedLimits::HARD).is_err());
    for length in 0..scratch.len() {
        assert!(decode_v3_planned_grouped(&scratch[..length], V3GroupedLimits::HARD).is_err());
    }
    let mut mutated = scratch;
    let last = mutated.len() - 1;
    mutated[last] ^= 1;
    assert!(decode_v3_planned_grouped(&mutated, V3GroupedLimits::HARD).is_err());
}

#[derive(Default)]
struct Faults {
    fail_read: bool,
    fail_seek: bool,
    fail_flush: bool,
    fail_write: bool,
}

#[derive(Clone)]
struct FaultStream {
    cursor: Arc<Mutex<Cursor<Vec<u8>>>>,
    faults: Arc<Mutex<Faults>>,
}

impl FaultStream {
    fn new() -> Self {
        Self {
            cursor: Arc::new(Mutex::new(Cursor::new(Vec::new()))),
            faults: Arc::new(Mutex::new(Faults::default())),
        }
    }
}

impl Read for FaultStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.faults.lock().unwrap().fail_read {
            return Err(io::Error::other("injected read"));
        }
        self.cursor.lock().unwrap().read(bytes)
    }
}

impl Seek for FaultStream {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if self.faults.lock().unwrap().fail_seek {
            return Err(io::Error::other("injected seek"));
        }
        self.cursor.lock().unwrap().seek(position)
    }
}

impl Write for FaultStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.faults.lock().unwrap().fail_write {
            return Err(io::Error::other("injected write"));
        }
        self.cursor.lock().unwrap().write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.faults.lock().unwrap().fail_flush {
            return Err(io::Error::other("injected flush"));
        }
        Ok(())
    }
}

#[test]
fn write_flush_seek_and_reread_failures_never_seal_scratch() {
    let schema = schema("incremental_faults");
    let batch = batch(schema.schema_id, 0, 2);
    for mode in ["write", "flush", "seek", "read"] {
        let stream = FaultStream::new();
        let held = stream.cursor.clone();
        let faults = stream.faults.clone();
        let mut writer =
            V3PlannedGroupedIngestWriter::try_new(stream, schema.clone(), Default::default())
                .unwrap();
        if mode == "write" {
            faults.lock().unwrap().fail_write = true;
            assert!(writer.write_batch(&batch).is_err());
            assert_eq!(writer.state(), V3PlannedGroupedWriterState::Poisoned);
        } else {
            writer.write_batch(&batch).unwrap();
            match mode {
                "flush" => faults.lock().unwrap().fail_flush = true,
                "seek" => faults.lock().unwrap().fail_seek = true,
                "read" => faults.lock().unwrap().fail_read = true,
                _ => unreachable!(),
            }
            assert!(writer.finish().is_err());
        }
        assert!(!held.lock().unwrap().get_ref().ends_with(b"sealed:)"));
    }
}

#[test]
fn caller_scratch_output_chunk_event_and_child_limits_fail_closed() {
    let schema = schema("incremental_limits");
    let batch = batch(schema.schema_id, 0, 2);
    let tiny = V3PlannedGroupedWriterOptions {
        max_scratch_bytes: 1,
        ..Default::default()
    };
    assert!(
        V3PlannedGroupedIngestWriter::try_new(Cursor::new(Vec::new()), schema.clone(), tiny)
            .is_err()
    );

    let mut limits = V3GroupedLimits::DEFAULT_IN_MEMORY;
    limits.max_chunks = 1;
    limits.max_events = 2;
    limits.max_children = batch.child_count() as u64;
    let options = V3PlannedGroupedWriterOptions {
        limits,
        ..Default::default()
    };
    let mut writer =
        V3PlannedGroupedIngestWriter::try_new(Cursor::new(Vec::new()), schema.clone(), options)
            .unwrap();
    writer.write_batch(&batch).unwrap();
    assert!(writer.write_batch(&batch).is_err());

    let options = V3PlannedGroupedWriterOptions {
        max_output_bytes: 1,
        ..Default::default()
    };
    let mut writer =
        V3PlannedGroupedIngestWriter::try_new(Cursor::new(Vec::new()), schema, options).unwrap();
    writer.write_batch(&batch).unwrap();
    assert!(writer.finish().is_err());
}

fn unique_path(name: &str) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::os::unix::fs::PermissionsExt;
    let mut hasher = DefaultHasher::new();
    std::thread::current()
        .name()
        .unwrap_or("test")
        .hash(&mut hasher);
    let directory = std::env::temp_dir().join(format!(
        "apw-{}-{:016x}",
        std::process::id(),
        hasher.finish(),
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    directory.join(name)
}

#[test]
fn held_file_sync_and_atomic_create_once_publish_exact_reference() {
    let output = unique_path("publish.aura0");
    let _ = fs::remove_file(&output);
    let schema = schema("incremental_publish");
    let batches = vec![batch(schema.schema_id, 0, 2), batch(schema.schema_id, 2, 2)];
    let reference = GroupedSearch::CrossDomain
        .compile(&schema, &batches, Default::default())
        .unwrap();
    let mut publisher =
        V3PlannedGroupedCreateOnceWriter::create(&output, schema.clone(), Default::default())
            .unwrap();
    for batch in &batches {
        publisher.write_batch(batch).unwrap();
    }
    let temp = publisher.temp_path().to_path_buf();
    let incomplete = fs::read(&temp).unwrap();
    assert!(decode_v3_planned_grouped(&incomplete, V3GroupedLimits::HARD).is_err());
    let (receipt, outcome) = publisher.finish_and_publish().unwrap();
    assert_eq!(
        outcome,
        V3PlannedGroupedPublicationOutcome::Committed {
            stale_temp_cleanup_required: false
        }
    );
    assert_eq!(fs::read(&output).unwrap(), reference.bytes);
    assert_eq!(receipt.summary, reference.summary);
    assert!(!receipt.stale_temp_cleanup_required);
    assert!(!temp.exists());

    assert_eq!(
        V3PlannedGroupedCreateOnceWriter::recover_exact(&output, &receipt).unwrap(),
        V3PlannedGroupedPublicationOutcome::AdoptedExact
    );
    let mut restart =
        V3PlannedGroupedCreateOnceWriter::create(&output, schema.clone(), Default::default())
            .unwrap();
    for batch in &batches {
        restart.write_batch(batch).unwrap();
    }
    assert_eq!(
        restart.finish_and_publish().unwrap().1,
        V3PlannedGroupedPublicationOutcome::AdoptedExact
    );
    fs::write(&output, b"conflicting-final").unwrap();
    let before = fs::read(&output).unwrap();
    let mut conflict =
        V3PlannedGroupedCreateOnceWriter::create(&output, schema, Default::default()).unwrap();
    for batch in &batches {
        conflict.write_batch(batch).unwrap();
    }
    assert!(conflict.finish_and_publish().is_err());
    assert_eq!(fs::read(&output).unwrap(), before);
    fs::remove_file(&output).unwrap();
    fs::remove_dir(output.parent().unwrap()).unwrap();
}

#[test]
fn file_finish_rewrites_and_truncates_held_scratch_to_exact_artifact() {
    let path = unique_path("held.aura0");
    let _ = fs::remove_file(&path);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let schema = schema("incremental_held");
    let batch = batch(schema.schema_id, 0, 2);
    let reference = GroupedSearch::CrossDomain
        .compile(&schema, std::slice::from_ref(&batch), Default::default())
        .unwrap();
    let mut writer =
        V3PlannedGroupedIngestWriter::try_new(file, schema, Default::default()).unwrap();
    writer.write_batch(&batch).unwrap();
    let (_, receipt) = writer.finish_and_sync().unwrap();
    assert_eq!(fs::read(&path).unwrap(), reference.bytes);
    assert_eq!(receipt.summary, reference.summary);
    File::open(&path).unwrap().sync_all().unwrap();
    fs::remove_file(&path).unwrap();
    fs::remove_dir(path.parent().unwrap()).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn publication_rejects_untrusted_parent_and_symlink_components() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::os::unix::net::UnixListener;
    let root = unique_path("security-root");
    let directory = root.parent().unwrap().to_path_buf();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(V3PlannedGroupedCreateOnceWriter::create(
        directory.join("bad.aura0"),
        schema("bad_parent"),
        Default::default()
    )
    .is_err());
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let real = directory.join("real");
    fs::create_dir(&real).unwrap();
    let link = directory.join("link");
    symlink(&real, &link).unwrap();
    assert!(V3PlannedGroupedCreateOnceWriter::create(
        link.join("bad.aura0"),
        schema("symlink_parent"),
        Default::default()
    )
    .is_err());
    let broken = directory.join("broken.aura0");
    symlink(directory.join("missing"), &broken).unwrap();
    assert!(V3PlannedGroupedCreateOnceWriter::create(
        &broken,
        schema("broken_output"),
        Default::default()
    )
    .is_err());
    fs::remove_file(broken).unwrap();
    let socket = directory.join("socket.aura0");
    let listener = UnixListener::bind(&socket).unwrap();
    assert!(V3PlannedGroupedCreateOnceWriter::create(
        &socket,
        schema("socket_output"),
        Default::default()
    )
    .is_err());
    drop(listener);
    fs::remove_file(socket).unwrap();
    let wrong_mode = directory.join("mode.aura0");
    fs::write(&wrong_mode, b"existing").unwrap();
    fs::set_permissions(&wrong_mode, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(V3PlannedGroupedCreateOnceWriter::create(
        &wrong_mode,
        schema("wrong_mode_output"),
        Default::default()
    )
    .is_err());
    fs::remove_file(wrong_mode).unwrap();
    fs::remove_file(link).unwrap();
    fs::remove_dir(real).unwrap();
    fs::remove_dir(directory).unwrap();
}

#[test]
fn selected_bytes_match_pre_cleanup_writer() {
    use sha2::{Digest, Sha256};
    // Frozen from dfd4c048, before selector/analysis consolidation. These check
    // complete selection and bytes independently of the new encoder/decoder pair.
    for (mode, expected) in [
        (
            "mixed",
            "55c478c6dc19524644033d0a8876b901d775c49c047b3c831e6613c43eb58a38",
        ),
        (
            "cross0",
            "cf8d117699157ad7c0b8d67a41ce3ca842f706a447ecebb07dc1b11f1a7b912d",
        ),
        (
            "cross1",
            "cf9cf0790b520240d4d5e004027f1d494ce1b5a719d765683d2d7694e2c57e8f",
        ),
        (
            "empty",
            "b5c764d48fc2ed63cfe38477e0e5909ecef2169ef165d93b4b2e18c47da733f9",
        ),
    ] {
        let schema = if mode.starts_with("cross") {
            cross_only_schema("frozen-selection")
        } else {
            schema("frozen-selection")
        };
        let batches = match mode {
            "mixed" => vec![
                batch(schema.schema_id, 0, 64),
                batch(schema.schema_id, 64, 64),
            ],
            "cross0" => vec![paired_batch(schema.schema_id, true)],
            "cross1" => vec![paired_batch(schema.schema_id, false)],
            _ => vec![],
        };
        let artifact = GroupedSearch::CrossDomain
            .compile(&schema, &batches, Default::default())
            .unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&artifact.bytes)),
            expected,
            "{mode}"
        );
    }
}
