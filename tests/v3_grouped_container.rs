use std::io::Cursor;

use aura_codec::{
    canonical_v3_event_batch_sha256, decode_v3_grouped_aura0, decode_v3_grouped_aura0_with_limits,
    decode_v3_grouped_footer, encode_v3_grouped_footer, AnyCompiledFooter, AuraV3Column,
    AuraV3ColumnValues as Values, AuraV3EventBatch, CanonicalV3EventHasher, FieldRole, FieldType,
    RelationshipPermissions, SchemaBuilder, V3EventLimits, V3GroupedAura0Writer, V3GroupedLimits,
    V3GroupedWriterOptions, V3_GROUPED_BODY_ENCODING_EXACT_EVENTS, V3_GROUPED_BODY_LAYOUT_VERSION,
    V3_GROUPED_CHUNK_DESCRIPTOR_BYTES, V3_GROUPED_EVENT_BLOCK_VERSION,
    V3_GROUPED_FOOTER_LAYOUT_VERSION, V3_GROUPED_FOOTER_PREFIX_BYTES,
};
use sha2::{Digest, Sha256};

const FOOTER_DOMAIN: &[u8] = b"aura-v3-grouped-aura0-footer-v1\0";

fn permissions() -> RelationshipPermissions {
    RelationshipPermissions::none()
        .with_split()
        .with_within_domain()
}

fn schema() -> aura_codec::SchemaDescriptor {
    let schema = SchemaBuilder::new("grouped_container_exact")
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

fn chunks(schema_id: u32) -> Vec<AuraV3EventBatch> {
    vec![
        batch(schema_id, &[10], &[1], &[0, 0], &[], &[]),
        batch(
            schema_id,
            &[20, 30],
            &[2, 3],
            &[0, 2, 4],
            &[0, 1, 1, 0],
            &[100, 101, 102, 103],
        ),
    ]
}

fn whole(schema_id: u32) -> AuraV3EventBatch {
    batch(
        schema_id,
        &[10, 20, 30],
        &[1, 2, 3],
        &[0, 0, 2, 4],
        &[0, 1, 1, 0],
        &[100, 101, 102, 103],
    )
}

fn write_file(batches: &[AuraV3EventBatch]) -> Vec<u8> {
    let mut writer = V3GroupedAura0Writer::try_new(
        Cursor::new(Vec::new()),
        schema(),
        V3GroupedWriterOptions::default(),
    )
    .unwrap();
    for batch in batches {
        writer.write_batch(batch).unwrap();
    }
    writer.finish().unwrap().0.into_inner()
}

fn footer_range(file: &[u8]) -> std::ops::Range<usize> {
    let footer_len_offset = file.len() - 12;
    let footer_len = u32::from_le_bytes(
        file[footer_len_offset..footer_len_offset + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    footer_len_offset - footer_len..footer_len_offset
}

fn resign_footer(file: &mut [u8]) {
    let range = footer_range(file);
    let hash_start = range.end - 32;
    let mut hasher = Sha256::new();
    hasher.update(FOOTER_DOMAIN);
    hasher.update(((hash_start - range.start) as u64).to_le_bytes());
    hasher.update(&file[range.start..hash_start]);
    let hash: [u8; 32] = hasher.finalize().into();
    file[hash_start..range.end].copy_from_slice(&hash);
}

#[test]
fn grouped_complete_file_roundtrips_empty_zero_child_and_multichunk() {
    let schema = schema();
    let empty = write_file(&[]);
    let decoded_empty = decode_v3_grouped_aura0(&empty).unwrap();
    assert_eq!(decoded_empty.footer.event_count, 0);
    assert_eq!(decoded_empty.footer.child_count, 0);
    assert!(decoded_empty.footer.chunks.is_empty());
    assert!(decoded_empty.batches.is_empty());

    let batches = chunks(schema.schema_id);
    let file = write_file(&batches);
    let decoded = decode_v3_grouped_aura0(&file).unwrap();
    assert_eq!(decoded.batches, batches);
    assert_eq!(decoded.footer.event_count, 3);
    assert_eq!(decoded.footer.child_count, 4);
    assert_eq!(decoded.footer.chunks.len(), 2);
    assert_eq!(decoded.footer.chunks[0].child_count, 0);
    assert_eq!(decoded.footer.chunks[1].first_global_child, 0);
    assert_eq!(decoded.footer.stats[0].present_count, 3);
    assert_eq!(decoded.footer.stats[2].present_count, 4);
    let footer_bytes = &file[footer_range(&file)];
    assert_eq!(
        footer_bytes[6..8],
        V3_GROUPED_FOOTER_LAYOUT_VERSION.to_le_bytes()
    );
    assert_eq!(footer_bytes[8], V3_GROUPED_BODY_ENCODING_EXACT_EVENTS);
    assert_eq!(
        footer_bytes[52..54],
        V3_GROUPED_BODY_LAYOUT_VERSION.to_le_bytes()
    );
    assert_eq!(
        decode_v3_grouped_footer(footer_bytes).unwrap(),
        decoded.footer
    );
    assert_eq!(
        encode_v3_grouped_footer(&decoded.footer).unwrap(),
        footer_bytes
    );
    assert!(matches!(
        AnyCompiledFooter::decode(footer_bytes).unwrap(),
        AnyCompiledFooter::V3Grouped(_)
    ));
}

#[test]
fn canonical_grouped_hash_is_rechunking_invariant_and_single_batch_compatible() {
    let schema = schema();
    let batches = chunks(schema.schema_id);
    let whole = whole(schema.schema_id);
    let expected = canonical_v3_event_batch_sha256(&schema, &whole, V3EventLimits::HARD).unwrap();
    let mut incremental = CanonicalV3EventHasher::new(&schema, 3, 4, V3EventLimits::HARD).unwrap();
    for batch in &batches {
        incremental.update_batch(&schema, batch).unwrap();
    }
    assert_eq!(incremental.finalize().unwrap(), expected);
    let chunked = decode_v3_grouped_aura0(&write_file(&batches)).unwrap();
    let single = decode_v3_grouped_aura0(&write_file(&[whole])).unwrap();
    assert_eq!(
        chunked.footer.global_logical_sha256,
        single.footer.global_logical_sha256
    );
    assert_ne!(chunked.footer.body_sha256, single.footer.body_sha256);
    let mut incomplete = CanonicalV3EventHasher::new(&schema, 4, 4, V3EventLimits::HARD).unwrap();
    incomplete.update_batch(&schema, &batches[0]).unwrap();
    assert!(incomplete.finalize().is_err());
}

#[test]
fn exact_limits_accept_boundaries_and_reject_one_lower() {
    let schema = schema();
    let file = write_file(&chunks(schema.schema_id));
    let decoded = decode_v3_grouped_aura0(&file).unwrap();
    let footer_bytes = footer_range(&file).len();
    let max_block = decoded
        .footer
        .chunks
        .iter()
        .map(|chunk| chunk.stored_len as usize)
        .max()
        .unwrap();
    let max_chunk_events = decoded
        .footer
        .chunks
        .iter()
        .map(|chunk| chunk.event_count as usize)
        .max()
        .unwrap();
    let max_chunk_children = decoded
        .footer
        .chunks
        .iter()
        .map(|chunk| chunk.child_count as usize)
        .max()
        .unwrap();
    let max_chunk_values = decoded
        .footer
        .chunks
        .iter()
        .map(|chunk| chunk.event_count as usize * 2 + chunk.child_count as usize * 2)
        .max()
        .unwrap();
    let exact = V3GroupedLimits {
        max_footer_bytes: footer_bytes,
        max_body_bytes: decoded.footer.body_len,
        max_chunks: decoded.footer.chunks.len(),
        max_events: decoded.footer.event_count,
        max_children: decoded.footer.child_count,
        event_limits: V3EventLimits {
            max_block_bytes: max_block,
            max_events: max_chunk_events,
            max_children: max_chunk_children,
            max_values: max_chunk_values,
            ..V3EventLimits::HARD
        },
    };
    decode_v3_grouped_aura0_with_limits(&file, exact).unwrap();
    for lower in [
        V3GroupedLimits {
            max_footer_bytes: footer_bytes - 1,
            ..exact
        },
        V3GroupedLimits {
            max_body_bytes: decoded.footer.body_len - 1,
            ..exact
        },
        V3GroupedLimits {
            max_chunks: decoded.footer.chunks.len() - 1,
            ..exact
        },
        V3GroupedLimits {
            max_events: decoded.footer.event_count - 1,
            ..exact
        },
        V3GroupedLimits {
            max_children: decoded.footer.child_count - 1,
            ..exact
        },
        V3GroupedLimits {
            event_limits: V3EventLimits {
                max_block_bytes: max_block - 1,
                ..V3EventLimits::HARD
            },
            ..exact
        },
        V3GroupedLimits {
            event_limits: V3EventLimits {
                max_events: max_chunk_events - 1,
                ..exact.event_limits
            },
            ..exact
        },
        V3GroupedLimits {
            event_limits: V3EventLimits {
                max_children: max_chunk_children - 1,
                ..exact.event_limits
            },
            ..exact
        },
        V3GroupedLimits {
            event_limits: V3EventLimits {
                max_values: max_chunk_values - 1,
                ..exact.event_limits
            },
            ..exact
        },
    ] {
        assert!(decode_v3_grouped_aura0_with_limits(&file, lower).is_err());
    }
}

#[test]
fn truncation_mutation_and_resigned_semantic_corruption_fail_closed() {
    let schema = schema();
    let file = write_file(&chunks(schema.schema_id));
    for end in 0..file.len() {
        assert!(
            decode_v3_grouped_aura0(&file[..end]).is_err(),
            "prefix {end}"
        );
    }
    for offset in 0..file.len() {
        let mut bad = file.clone();
        bad[offset] ^= 1;
        assert!(decode_v3_grouped_aura0(&bad).is_err(), "byte {offset}");
    }

    let footer = footer_range(&file);
    let schema_len = 4 + u32::from_le_bytes(
        file[footer.start + V3_GROUPED_FOOTER_PREFIX_BYTES..][..4]
            .try_into()
            .unwrap(),
    ) as usize;
    let chunks_start = footer.start
        + V3_GROUPED_FOOTER_PREFIX_BYTES
        + schema_len
        + 8
        + schema.fields.len() * 36
        + 8;
    assert_eq!(
        footer.end - 32 - chunks_start,
        2 * V3_GROUPED_CHUNK_DESCRIPTOR_BYTES
    );
    for absolute in [
        footer.start + 9,
        footer.start + 52,
        chunks_start + 24,
        chunks_start + 40,
        chunks_start + 48,
        chunks_start + V3_GROUPED_CHUNK_DESCRIPTOR_BYTES + 8,
    ] {
        let mut bad = file.clone();
        bad[absolute] ^= 1;
        resign_footer(&mut bad);
        assert!(decode_v3_grouped_aura0(&bad).is_err(), "offset {absolute}");
    }
}

#[test]
fn flat_and_v2_compiled_footer_bytes_stay_dispatch_compatible() {
    for file in [
        include_bytes!("fixtures/v3-container/three-row-two-chunk.aura0").as_slice(),
        include_bytes!("fixtures/v2/plain-compact.aura0").as_slice(),
    ] {
        let range = footer_range(file);
        let decoded = AnyCompiledFooter::decode(&file[range.clone()]).unwrap();
        assert_eq!(decoded.encode().unwrap(), &file[range]);
    }
}

#[test]
fn checked_grouped_fixture_is_byte_exact_and_decodable() {
    let schema = schema();
    let file = write_file(&chunks(schema.schema_id));
    let source_hex = include_str!("fixtures/v3-grouped-container/three-event-two-chunk.aura0.hex");
    let compact = source_hex
        .chars()
        .filter(|value| !value.is_whitespace())
        .collect::<String>();
    let expected = (0..compact.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&compact[index..index + 2], 16).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(file, expected);
    let decoded = decode_v3_grouped_aura0(&expected).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/v3-grouped-container/manifest.json")).unwrap();
    let fixture = &manifest["fixture"];
    assert_eq!(manifest["manifest_version"], 1);
    assert_eq!(
        manifest["compatibility_promise"],
        "grouped Aura0 V3 exact-event v1 fixture bytes, routing tuple, schema identity, counts, and logical hash are frozen"
    );
    assert_eq!(fixture["file"], "three-event-two-chunk.aura0.hex");
    assert_eq!(fixture["container_version"], 3);
    assert_eq!(
        fixture["footer_layout_version"],
        V3_GROUPED_FOOTER_LAYOUT_VERSION
    );
    assert_eq!(
        fixture["body_encoding"],
        V3_GROUPED_BODY_ENCODING_EXACT_EVENTS
    );
    assert_eq!(
        fixture["body_layout_version"],
        V3_GROUPED_BODY_LAYOUT_VERSION
    );
    assert_eq!(
        fixture["event_block_version"],
        V3_GROUPED_EVENT_BLOCK_VERSION
    );
    assert_eq!(fixture["artifact_sha256"], hex(&Sha256::digest(&expected)));
    assert_eq!(fixture["schema_id"], decoded.footer.schema.schema_id);
    assert_eq!(
        fixture["schema_fingerprint_sha256"],
        hex(&decoded.footer.schema_fingerprint)
    );
    assert_eq!(
        fixture["global_logical_sha256"],
        hex(&decoded.footer.global_logical_sha256)
    );
    assert_eq!(fixture["event_count"], decoded.footer.event_count);
    assert_eq!(fixture["child_count"], decoded.footer.child_count);
    assert_eq!(fixture["chunk_count"], decoded.footer.chunks.len());
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
