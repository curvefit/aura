use aura_codec::experimental::{AuraV3Column, AuraV3ColumnValues as Values, AuraV3EventBatch};
use aura_codec::{FieldRole, FieldType, RelationshipPermissions, SchemaBuilder};

pub fn schema(name: &str) -> aura_codec::SchemaDescriptor {
    let mut schema = SchemaBuilder::new(name)
        .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
        .repeated_field("side", FieldType::U8, FieldRole::Side)
        .repeated_field("value", FieldType::I64, FieldRole::Value)
        .repeated_field("optional", FieldType::I64, FieldRole::Quantity)
        .repeated_field("count", FieldType::U32, FieldRole::Count)
        .dual_domain_repeated_group(
            7,
            vec![1, 2, 3, 4],
            1,
            RelationshipPermissions::none()
                .with_split()
                .with_within_domain()
                .with_across_domain_same_field(),
        )
        .finish()
        .unwrap();
    schema.fields[3].nullable = true;
    schema.fields[4].nullable = true;
    let groups = schema.groups.clone();
    schema.with_v3_groups(groups).unwrap()
}

pub fn cross_only_schema(name: &str) -> aura_codec::SchemaDescriptor {
    let mut schema = schema(name);
    schema.groups[0].relationships =
        RelationshipPermissions::none().with_across_domain_same_field();
    let groups = schema.groups.clone();
    schema.with_v3_groups(groups).unwrap()
}

pub fn paired_batch(schema_id: u32, domain0_is_cheaper: bool) -> AuraV3EventBatch {
    let mut sides = Vec::new();
    let mut values = Vec::new();
    for ordinal in 0..192i64 {
        let shift = ((ordinal % 6) * 7 + 6) as u32;
        let cheap = (1i64 << shift).saturating_sub(1);
        let expensive = cheap + 1;
        sides.extend_from_slice(&[0, 1]);
        if domain0_is_cheaper {
            values.extend_from_slice(&[cheap, expensive]);
        } else {
            values.extend_from_slice(&[expensive, cheap]);
        }
    }
    let children = sides.len();
    AuraV3EventBatch {
        schema_id,
        event_count: 1,
        child_offsets: vec![0, children as u32],
        event_columns: vec![AuraV3Column {
            slot: 0,
            validity: None,
            values: Values::TimestampMs(vec![1]),
        }],
        repeated_columns: vec![
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::U8(sides),
            },
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::I64(values),
            },
            AuraV3Column {
                slot: 3,
                validity: Some(vec![0; children.div_ceil(8)]),
                values: Values::I64(vec![0; children]),
            },
            AuraV3Column {
                slot: 4,
                validity: Some(vec![0; children.div_ceil(8)]),
                values: Values::U32(vec![0; children]),
            },
        ],
    }
}

pub fn batch(schema_id: u32, first_event: usize, events: usize) -> AuraV3EventBatch {
    let mut offsets = vec![0u32];
    let mut sides = Vec::new();
    let mut values = Vec::new();
    let mut optional = Vec::new();
    let mut counts = Vec::new();
    for event in first_event..first_event + events {
        if event % 3 != 0 {
            for ordinal in 0..48i64 {
                let base = if ordinal % 2 == 0 {
                    4_000_000_000 + ordinal * 1_000_003 + event as i64
                } else {
                    -4_000_000_000 - ordinal * 999_983 - event as i64
                };
                sides.extend_from_slice(&[0, 1]);
                values.extend_from_slice(&[base, base + 1]);
                optional.extend_from_slice(&[0, 0]);
                counts.extend_from_slice(&[0, 0]);
            }
            if event % 2 == 0 {
                sides.push(0);
                values.push(-77);
                optional.push(0);
                counts.push(0);
            }
        }
        offsets.push(sides.len() as u32);
    }
    let validity_len = sides.len().div_ceil(8);
    AuraV3EventBatch {
        schema_id,
        event_count: events as u32,
        child_offsets: offsets,
        event_columns: vec![AuraV3Column {
            slot: 0,
            validity: None,
            values: Values::TimestampMs(
                (first_event..first_event + events)
                    .map(|value| value as i64)
                    .collect(),
            ),
        }],
        repeated_columns: vec![
            AuraV3Column {
                slot: 1,
                validity: None,
                values: Values::U8(sides),
            },
            AuraV3Column {
                slot: 2,
                validity: None,
                values: Values::I64(values),
            },
            AuraV3Column {
                slot: 3,
                validity: Some(vec![0; validity_len]),
                values: Values::I64(optional),
            },
            AuraV3Column {
                slot: 4,
                validity: Some(vec![0; validity_len]),
                values: Values::U32(counts),
            },
        ],
    }
}
