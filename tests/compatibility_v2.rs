use std::fs;
use std::path::{Path, PathBuf};

use aura_codec::records::{self, Aura0ByteLaneCodec, Aura0FileProfile};
use aura_codec::{
    generic_i64_parent_schema, Aura0ByteLaneUse, AuraContainerVersion, AuraError, AuraHeader,
    AuraI64EventReader, AuraI64EventWriter, AuraI64Reader, AuraI64Writer, AuraReader, I64Event,
    Profile,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v2");
const FORMAT_VERSION: u16 = 2;

fn plain_schema() -> aura_codec::Result<aura_codec::SchemaDescriptor> {
    generic_i64_parent_schema("compat-v2-plain", &[100, 0, 2, 0])
}

fn plain_rows() -> Vec<Vec<i64>> {
    vec![
        vec![1_000, 10_000, 1, 7],
        vec![1_001, 10_010, 2, 8],
        vec![1_002, 10_020, 3, 9],
        vec![1_003, 10_030, 4, 10],
    ]
}

fn explicit_schema() -> aura_codec::Result<aura_codec::SchemaDescriptor> {
    generic_i64_parent_schema("compat-v2-explicit-events", &[100, 0, 200, 205, 0, 0, 5, 0])
}

fn explicit_events() -> Vec<I64Event> {
    vec![
        I64Event {
            event_values: vec![1_000, 0],
            children: vec![vec![0, 100, 10, 8, 1], vec![1, 101, 11, 9, 2]],
        },
        I64Event {
            event_values: vec![1_001, 1],
            children: Vec::new(),
        },
        I64Event {
            event_values: vec![1_002, 0],
            children: vec![vec![0, 102, 12, 10, 3]],
        },
        I64Event {
            event_values: vec![1_002, 0],
            children: vec![vec![1, 103, 13, 11, 4]],
        },
    ]
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn canonical_rows_hash(rows: &[Vec<i64>]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aura-v2-canonical-rows\0");
    hasher.update((rows.len() as u64).to_le_bytes());
    for row in rows {
        hasher.update((row.len() as u64).to_le_bytes());
        for value in row {
            hasher.update(value.to_le_bytes());
        }
    }
    sha256_hex(&hasher.finalize())
}

fn canonical_events_hash(events: &[I64Event]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aura-v2-canonical-events\0");
    hasher.update((events.len() as u64).to_le_bytes());
    for event in events {
        hasher.update((event.event_values.len() as u64).to_le_bytes());
        for value in &event.event_values {
            hasher.update(value.to_le_bytes());
        }
        hasher.update((event.children.len() as u64).to_le_bytes());
        for child in &event.children {
            hasher.update((child.len() as u64).to_le_bytes());
            for value in child {
                hasher.update(value.to_le_bytes());
            }
        }
    }
    sha256_hex(&hasher.finalize())
}

fn profile_name(profile: Profile) -> &'static str {
    match profile {
        Profile::Ingest => "ingest",
        Profile::Aura0 => "aura0",
        Profile::Aura1 => "aura1",
    }
}

fn error_kind(error: &AuraError) -> &'static str {
    match error {
        AuraError::UnexpectedEof => "UnexpectedEof",
        AuraError::InvalidMagic { .. } => "InvalidMagic",
        AuraError::UnsupportedVersion(_) => "UnsupportedVersion",
        AuraError::InvalidProfile(_) => "InvalidProfile",
        AuraError::InvalidBlockSize(_) => "InvalidBlockSize",
        AuraError::InvalidValue(_) => "InvalidValue",
        AuraError::Diagnostic(_) => "Diagnostic",
        AuraError::TrailingBytes(_) => "TrailingBytes",
    }
}

fn schema_json(schema: &aura_codec::SchemaDescriptor) -> Value {
    json!({
        "name": schema.name,
        "schema_id": schema.schema_id,
        "compact_schema_map": schema.compact_schema_map,
        "field_count": schema.fields.len(),
    })
}

struct ArtifactFacts<'a> {
    filename: &'a str,
    bytes: &'a [u8],
    profile: Profile,
    schema: &'a aura_codec::SchemaDescriptor,
    row_count: usize,
    event_count: Option<usize>,
    child_count: Option<usize>,
    canonical_hash: String,
    storage_profile: Option<&'a str>,
    has_byte_lane: bool,
}

fn artifact_json(facts: ArtifactFacts<'_>) -> Value {
    json!({
        "filename": facts.filename,
        "artifact_kind": if facts.event_count.is_some() { "explicit-events" } else { "rows" },
        "profile": profile_name(facts.profile),
        "container_magic": "AURA",
        "format_version": FORMAT_VERSION,
        "sha256": sha256_hex(facts.bytes),
        "bytes": facts.bytes.len(),
        "schema": schema_json(facts.schema),
        "row_count": facts.row_count,
        "event_count": facts.event_count,
        "child_count": facts.child_count,
        "canonical_hash": facts.canonical_hash,
        "aura0_storage_profile": facts.storage_profile,
        "has_byte_lane": facts.has_byte_lane,
    })
}

fn write_fixture(path: &Path, bytes: &[u8]) -> aura_codec::Result<()> {
    fs::write(path, bytes).map_err(|_| AuraError::InvalidValue("fixture write"))
}

fn regenerate_valid_fixtures_in_memory() -> aura_codec::Result<Vec<(&'static str, Vec<u8>)>> {
    let rows = plain_rows();
    let schema = plain_schema()?;
    let mut writer = AuraI64Writer::new(schema)
        .with_stream(17, 23)
        .with_header_comment("compatibility-v2 plain rows");
    writer.extend_rows(rows)?;
    let ingest = writer.finish()?;
    let compact = AuraI64Writer::compile_profile(&ingest, Profile::Aura0)?;
    let replay = AuraI64Writer::compile_profile(&ingest, Profile::Aura1)?;
    let hybrid = records::compile_i64_file_with_aura0_profile(
        &ingest,
        Aura0FileProfile::Hybrid,
        Aura0ByteLaneCodec::Lz4,
    )?;

    let event_schema = explicit_schema()?;
    let mut event_writer = AuraI64EventWriter::new(event_schema)
        .with_stream(29, 31)
        .with_header_comment("compatibility-v2 explicit events");
    for event in explicit_events() {
        event_writer.push_event(event)?;
    }
    let explicit_compact = event_writer.finish_profile(Profile::Aura0)?;

    Ok(vec![
        ("plain-ingest.aura", ingest),
        ("plain-compact.aura0", compact),
        ("plain-replay.aura1", replay),
        ("plain-hybrid.aura0", hybrid),
        ("explicit-events.aura0", explicit_compact),
    ])
}

fn generate_fixtures() -> aura_codec::Result<()> {
    let dir = PathBuf::from(FIXTURE_DIR);
    fs::create_dir_all(&dir).map_err(|_| AuraError::InvalidValue("fixture directory"))?;

    let rows = plain_rows();
    let schema = plain_schema()?;
    let mut writer = AuraI64Writer::new(schema.clone())
        .with_stream(17, 23)
        .with_header_comment("compatibility-v2 plain rows");
    writer.extend_rows(rows.clone())?;
    let ingest = writer.finish()?;
    let compact = AuraI64Writer::compile_profile(&ingest, Profile::Aura0)?;
    let replay = AuraI64Writer::compile_profile(&ingest, Profile::Aura1)?;
    let hybrid = records::compile_i64_file_with_aura0_profile(
        &ingest,
        Aura0FileProfile::Hybrid,
        Aura0ByteLaneCodec::Lz4,
    )?;

    let events = explicit_events();
    let event_schema = explicit_schema()?;
    let mut event_writer = AuraI64EventWriter::new(event_schema.clone())
        .with_stream(29, 31)
        .with_header_comment("compatibility-v2 explicit events");
    for event in events.clone() {
        event_writer.push_event(event)?;
    }
    let explicit_compact = event_writer.finish_profile(Profile::Aura0)?;

    let invalid = compact
        .get(..compact.len().saturating_sub(1))
        .ok_or(AuraError::InvalidValue("invalid fixture source"))?
        .to_vec();
    let invalid_error =
        records::decode_i64_file(&invalid).expect_err("truncated fixture must reject");

    let artifacts = vec![
        ("plain-ingest.aura", &ingest, Profile::Ingest, None, false),
        ("plain-compact.aura0", &compact, Profile::Aura0, None, false),
        ("plain-replay.aura1", &replay, Profile::Aura1, None, false),
        (
            "plain-hybrid.aura0",
            &hybrid,
            Profile::Aura0,
            Some("hybrid"),
            true,
        ),
        (
            "explicit-events.aura0",
            &explicit_compact,
            Profile::Aura0,
            None,
            false,
        ),
    ];
    for (filename, bytes, _, _, _) in &artifacts {
        write_fixture(&dir.join(filename), bytes)?;
    }
    write_fixture(&dir.join("invalid-truncated.aura0"), &invalid)?;

    let mut manifest_artifacts = Vec::new();
    for (filename, bytes, profile, storage_profile, has_byte_lane) in artifacts {
        let is_explicit = filename == "explicit-events.aura0";
        manifest_artifacts.push(if is_explicit {
            artifact_json(ArtifactFacts {
                filename,
                bytes,
                profile,
                schema: &event_schema,
                row_count: events.iter().map(|event| event.children.len()).sum(),
                event_count: Some(events.len()),
                child_count: Some(events.iter().map(|event| event.children.len()).sum()),
                canonical_hash: canonical_events_hash(&events),
                storage_profile,
                has_byte_lane,
            })
        } else {
            artifact_json(ArtifactFacts {
                filename,
                bytes,
                profile,
                schema: &schema,
                row_count: rows.len(),
                event_count: None,
                child_count: None,
                canonical_hash: canonical_rows_hash(&rows),
                storage_profile,
                has_byte_lane,
            })
        });
    }

    let manifest = json!({
        "manifest_version": 1,
        "format": "Aura v2 compatibility fixtures",
        "format_version": FORMAT_VERSION,
        "generation_contract": "tiny deterministic artifacts generated through public Aura writer/compile APIs",
        "fixtures": manifest_artifacts,
        "invalid": [{
            "filename": "invalid-truncated.aura0",
            "source_fixture": "plain-compact.aura0",
            "profile": "aura0",
            "container_magic": "AURA",
            "format_version": FORMAT_VERSION,
            "sha256": sha256_hex(&invalid),
            "bytes": invalid.len(),
            "expected_behavior": "reject with a typed AuraError before logical row decode",
            "expected_error_kind": error_kind(&invalid_error),
            "schema": schema_json(&schema),
            "row_count": rows.len(),
            "event_count": Value::Null,
            "child_count": Value::Null,
            "canonical_hash": canonical_rows_hash(&rows),
        }],
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|_| AuraError::InvalidValue("fixture manifest"))?;
    fs::write(dir.join("manifest.json"), manifest_bytes)
        .map_err(|_| AuraError::InvalidValue("fixture manifest write"))?;
    Ok(())
}

fn read_manifest() -> Value {
    let path = Path::new(FIXTURE_DIR).join("manifest.json");
    let bytes = fs::read(path).expect("committed Aura v2 fixture manifest");
    serde_json::from_slice(&bytes).expect("valid Aura v2 fixture manifest JSON")
}

fn check_schema_identity(entry: &Value, schema: &aura_codec::SchemaDescriptor) {
    let declared = &entry["schema"];
    assert_eq!(declared["name"].as_str(), Some(schema.name.as_str()));
    assert_eq!(
        declared["schema_id"].as_u64(),
        Some(u64::from(schema.schema_id))
    );
    assert_eq!(
        declared["field_count"].as_u64(),
        Some(schema.fields.len() as u64)
    );
    let map = schema
        .compact_schema_map
        .as_ref()
        .expect("compact schema map");
    assert_eq!(declared["compact_schema_map"], json!(map));
}

fn check_container(bytes: &[u8], entry: &Value, expected_profile: Profile) {
    assert!(bytes.len() >= 6, "fixture has a complete header prefix");
    assert_eq!(&bytes[..4], b"AURA");
    assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), FORMAT_VERSION);
    assert_eq!(entry["container_magic"].as_str(), Some("AURA"));
    assert_eq!(
        entry["format_version"].as_u64(),
        Some(u64::from(FORMAT_VERSION))
    );
    assert_eq!(
        entry["profile"].as_str(),
        Some(profile_name(expected_profile))
    );
}

fn compiled_footer_start(bytes: &[u8]) -> usize {
    let seal_offset = bytes.len() - 8;
    assert_eq!(&bytes[seal_offset..], b"sealed:)");
    let footer_len_offset = seal_offset - 4;
    let footer_len =
        u32::from_le_bytes(bytes[footer_len_offset..seal_offset].try_into().unwrap()) as usize;
    footer_len_offset - footer_len
}

fn mutate_compiled_footer_version(bytes: &[u8], version: AuraContainerVersion) -> Vec<u8> {
    let mut mutated = bytes.to_vec();
    let footer_start = compiled_footer_start(&mutated);
    assert_eq!(&mutated[footer_start..footer_start + 4], b"AURP");
    mutated[footer_start + 4..footer_start + 6]
        .copy_from_slice(&version.wire_value().to_le_bytes());
    mutated
}

fn mutate_raw_embedded_footer_version(bytes: &[u8], version: AuraContainerVersion) -> Vec<u8> {
    let mut mutated = bytes.to_vec();
    let outer_header_len = AuraHeader::encoded_len(&mutated).unwrap();
    let outer_footer_start = compiled_footer_start(&mutated);
    let embedded = &mutated[outer_header_len..outer_footer_start];
    let embedded_footer_start = compiled_footer_start(embedded);
    assert_eq!(
        &embedded[embedded_footer_start..embedded_footer_start + 4],
        b"AURP"
    );
    let absolute_version_offset = outer_header_len + embedded_footer_start + 4;
    mutated[absolute_version_offset..absolute_version_offset + 2]
        .copy_from_slice(&version.wire_value().to_le_bytes());
    mutated
}

#[test]
#[ignore = "fixture generation is an explicit maintenance action"]
fn generate_v2_fixtures() {
    assert_eq!(
        std::env::var("AURA_V2_FIXTURE_GENERATE").ok().as_deref(),
        Some("1"),
        "set AURA_V2_FIXTURE_GENERATE=1 to regenerate committed fixtures"
    );
    generate_fixtures().expect("generate Aura v2 fixtures");
}

#[test]
fn compatibility_v2_manifest_hashes_and_decode_contracts() {
    let manifest = read_manifest();
    assert_eq!(manifest["manifest_version"].as_u64(), Some(1));
    assert_eq!(
        manifest["format_version"].as_u64(),
        Some(u64::from(FORMAT_VERSION))
    );

    let rows = plain_rows();
    let plain_schema = plain_schema().unwrap();
    let events = explicit_events();
    let event_schema = explicit_schema().unwrap();
    let fixtures = manifest["fixtures"].as_array().expect("fixture entries");
    assert_eq!(fixtures.len(), 5);
    let regenerated = regenerate_valid_fixtures_in_memory().unwrap();
    assert_eq!(regenerated.len(), fixtures.len());

    for entry in fixtures {
        let filename = entry["filename"].as_str().expect("fixture filename");
        let bytes = fs::read(Path::new(FIXTURE_DIR).join(filename)).expect("fixture bytes");
        let regenerated_bytes = regenerated
            .iter()
            .find_map(|(generated_name, generated_bytes)| {
                (*generated_name == filename).then_some(generated_bytes)
            })
            .expect("every committed valid fixture is regenerated in ordinary CI");
        assert_eq!(
            regenerated_bytes, &bytes,
            "regenerated V2 bytes changed for {filename}"
        );
        assert_eq!(entry["bytes"].as_u64(), Some(bytes.len() as u64));
        assert_eq!(entry["sha256"].as_str(), Some(sha256_hex(&bytes).as_str()));
        assert_eq!(
            entry["sha256"].as_str(),
            Some(sha256_hex(regenerated_bytes).as_str())
        );

        if entry["artifact_kind"] == "explicit-events" {
            let reader = AuraI64EventReader::open(&bytes).expect("explicit event fixture decodes");
            check_container(&bytes, entry, Profile::Aura0);
            check_schema_identity(entry, &event_schema);
            assert_eq!(reader.header().profile, Profile::Aura0);
            assert_eq!(reader.header().container_version, AuraContainerVersion::V2);
            assert_eq!(reader.schema(), &event_schema);
            assert_eq!(reader.events(), events.as_slice());
            assert_eq!(entry["row_count"].as_u64(), Some(4));
            assert_eq!(entry["event_count"].as_u64(), Some(4));
            assert_eq!(entry["child_count"].as_u64(), Some(4));
            assert_eq!(
                entry["canonical_hash"].as_str(),
                Some(canonical_events_hash(&events).as_str())
            );
            let decoded = records::decode_i64_events_file(&bytes).unwrap();
            assert_eq!(decoded.header.container_version, AuraContainerVersion::V2);
            assert_eq!(
                decoded.compiled_footer.unwrap().container_version,
                AuraContainerVersion::V2
            );
        } else {
            let reader = AuraI64Reader::open(&bytes).expect("row fixture decodes");
            let expected_profile = match entry["profile"].as_str().unwrap() {
                "ingest" => Profile::Ingest,
                "aura0" => Profile::Aura0,
                "aura1" => Profile::Aura1,
                other => panic!("unexpected profile {other}"),
            };
            check_container(&bytes, entry, expected_profile);
            check_schema_identity(entry, &plain_schema);
            assert_eq!(reader.profile(), expected_profile);
            assert_eq!(reader.header().container_version, AuraContainerVersion::V2);
            assert_eq!(reader.schema(), &plain_schema);
            assert_eq!(reader.rows(), rows.as_slice());
            assert_eq!(entry["row_count"].as_u64(), Some(rows.len() as u64));
            assert_eq!(entry["event_count"], Value::Null);
            assert_eq!(entry["child_count"], Value::Null);
            assert_eq!(
                entry["canonical_hash"].as_str(),
                Some(canonical_rows_hash(&rows).as_str())
            );
            match expected_profile {
                Profile::Ingest => {
                    assert_eq!(
                        reader.ingest_footer().unwrap().container_version,
                        AuraContainerVersion::V2
                    );
                    let converted = AuraI64Writer::compile_profile(&bytes, Profile::Aura0)
                        .expect("committed V2 ingest fixture converts");
                    let decoded_converted = records::decode_i64_file(&converted).unwrap();
                    assert_eq!(
                        decoded_converted.header.container_version,
                        AuraContainerVersion::V2
                    );
                    assert_eq!(
                        decoded_converted.compiled_footer.unwrap().container_version,
                        AuraContainerVersion::V2
                    );
                }
                Profile::Aura0 | Profile::Aura1 => assert_eq!(
                    reader.compiled_footer().unwrap().container_version,
                    AuraContainerVersion::V2
                ),
            }
            if filename == "plain-hybrid.aura0" {
                assert_eq!(entry["aura0_storage_profile"].as_str(), Some("hybrid"));
                assert_eq!(entry["has_byte_lane"].as_bool(), Some(true));
                let footer = reader.compiled_footer().expect("hybrid compiled footer");
                assert!(!footer.aura1_byte_lanes.is_empty());
                let expanded = records::compile_aura0_to_aura1_bytes_with_lane(
                    &bytes,
                    Aura0ByteLaneUse::Always,
                    true,
                )
                .expect("hybrid byte lane expands");
                assert_eq!(records::decode_i64_file(&expanded).unwrap().rows, rows);
            }
        }
    }

    let invalid_entry = manifest["invalid"]
        .as_array()
        .and_then(|entries| entries.first())
        .expect("invalid fixture entry");
    let invalid_path = Path::new(FIXTURE_DIR).join(
        invalid_entry["filename"]
            .as_str()
            .expect("invalid fixture filename"),
    );
    let invalid = fs::read(invalid_path).expect("invalid fixture bytes");
    assert_eq!(invalid_entry["bytes"].as_u64(), Some(invalid.len() as u64));
    assert_eq!(
        invalid_entry["sha256"].as_str(),
        Some(sha256_hex(&invalid).as_str())
    );
    check_container(&invalid, invalid_entry, Profile::Aura0);
    check_schema_identity(invalid_entry, &plain_schema);
    assert_eq!(invalid_entry["row_count"].as_u64(), Some(rows.len() as u64));
    assert_eq!(invalid_entry["event_count"], Value::Null);
    assert_eq!(invalid_entry["child_count"], Value::Null);
    assert_eq!(
        invalid_entry["canonical_hash"].as_str(),
        Some(canonical_rows_hash(&rows).as_str())
    );
    let error = records::decode_i64_file(&invalid).expect_err("truncated fixture must reject");
    assert_eq!(error_kind(&error), invalid_entry["expected_error_kind"]);
}

#[test]
fn hybrid_byte_lane_shortcuts_reject_footer_only_v3_mismatch() {
    let bytes = fs::read(Path::new(FIXTURE_DIR).join("plain-hybrid.aura0"))
        .expect("committed hybrid fixture");
    let mutated = mutate_compiled_footer_version(&bytes, AuraContainerVersion::V3);
    let expected = AuraError::UnsupportedVersion(3);

    assert_eq!(
        records::compile_aura0_to_aura1_bytes_with_lane(&mutated, Aura0ByteLaneUse::Always, true,),
        Err(expected.clone()),
        "public byte-lane extraction must validate the outer AURP version"
    );
    assert_eq!(
        records::compile_i64_file(&mutated, Profile::Aura1),
        Err(expected.clone()),
        "fast hybrid conversion must not bypass the outer footer version"
    );
    assert_eq!(
        records::try_compile_i64_file_with_fused_output_guard(&mutated, Profile::Aura1),
        Err(expected.clone()),
        "guarded fast conversion must not bypass the outer footer version"
    );
    assert_eq!(
        records::try_compile_i64_file_profiled(
            &mutated,
            Profile::Aura1,
            records::OutputGuardMode::NoGuard,
            records::TranscodePath::Auto,
            records::Aura0EncoderPath::Materialized,
            records::Aura0DecodePath::Materialized,
        ),
        Err(expected),
        "profiled fast conversion must not bypass the outer footer version"
    );
}

#[test]
fn raw_byte_lane_shortcuts_validate_embedded_aura1_footer_version() {
    let ingest = fs::read(Path::new(FIXTURE_DIR).join("plain-ingest.aura"))
        .expect("committed ingest fixture");
    let fast = records::compile_i64_file_with_aura0_profile(
        &ingest,
        Aura0FileProfile::Fast,
        Aura0ByteLaneCodec::Raw,
    )
    .unwrap();
    let mutated = mutate_raw_embedded_footer_version(&fast, AuraContainerVersion::V3);

    assert_eq!(
        records::compile_aura0_to_aura1_bytes_with_lane(&mutated, Aura0ByteLaneUse::Always, false,),
        Err(AuraError::UnsupportedVersion(3))
    );
    assert_eq!(
        records::compile_i64_file(&mutated, Profile::Aura1),
        Err(AuraError::UnsupportedVersion(3))
    );
    assert_eq!(
        records::try_compile_i64_file_profiled(
            &mutated,
            Profile::Aura1,
            records::OutputGuardMode::NoGuard,
            records::TranscodePath::Auto,
            records::Aura0EncoderPath::Materialized,
            records::Aura0DecodePath::Materialized,
        ),
        Err(AuraError::UnsupportedVersion(3))
    );
}

#[test]
fn file_backed_reader_rejects_footer_only_v3_mismatch() {
    let bytes = fs::read(Path::new(FIXTURE_DIR).join("plain-hybrid.aura0"))
        .expect("committed hybrid fixture");
    let mutated = mutate_compiled_footer_version(&bytes, AuraContainerVersion::V3);
    let path = std::env::temp_dir().join(format!(
        "aura-v3-mismatch-{}-{}.aura0",
        std::process::id(),
        std::thread::current().name().unwrap_or("compat")
    ));
    fs::write(&path, mutated).unwrap();
    let result = AuraReader::open_path(&path);
    let _removed = fs::remove_file(&path);

    assert!(matches!(result, Err(AuraError::UnsupportedVersion(3))));
}
