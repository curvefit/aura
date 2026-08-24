//! Reference planned-grouped Aura0 V3 container (layout 2, body encoding 3).
//!
//! Attempt 1 stores exact direct `AURAV3EB` chunks. Attempt 2 adds a distinct
//! plan-bound compact-stream block and scores registry-1 direct, compact
//! registry-2 direct, and schema-authorized compact split-domain direct by
//! actual complete-file bytes. Nothing here claims compression, Parquet
//! comparison, holdout evidence, or production readiness.

use sha2::{Digest, Sha256};

use crate::bytes::ByteReader;
use crate::format::{AuraContainerVersion, SEAL_MAGIC};
use crate::schema::{
    decode_schema_descriptor, encode_schema_descriptor, FieldScope, SchemaDescriptor,
};
use crate::v3_events::{
    canonical_v3_event_batch_sha256, decode_v3_event_block, encode_v3_event_block,
    validate_v3_event_batch, AuraV3EventBatch, CanonicalV3EventHasher,
};
use crate::v3_grouped_container::V3GroupedLimits;
use crate::v3_plan_v2::{AuraPlanV2, PlanV2Inspection, PlanV2Selection, MAX_AURA_PLAN_V2_BYTES};
use crate::v3_values::{
    canonical_v3_schema_fingerprint, decode_column, encode_column, AuraV3Column,
    AuraV3ColumnValues, AuraV3VariableColumn, V3ValueLimits,
};
use crate::{AuraError, AuraHeader, Profile, Result};

pub const V3_PLANNED_GROUPED_FOOTER_LAYOUT_VERSION: u16 = 2;
pub const V3_PLANNED_GROUPED_BODY_ENCODING: u8 = 3;
pub const V3_PLANNED_GROUPED_BODY_LAYOUT_VERSION: u16 = 1;
pub const V3_PLANNED_GROUPED_DIRECT_BLOCK_VERSION: u16 = crate::V3_EVENT_BLOCK_VERSION;
pub const V3_PLANNED_GROUPED_COMPACT_BODY_LAYOUT_VERSION: u16 = 2;
pub const V3_PLANNED_GROUPED_COMPACT_BLOCK_VERSION: u16 = 2;
pub const V3_PLANNED_GROUPED_FOOTER_PREFIX_BYTES: usize = 216;
pub const V3_PLANNED_GROUPED_CHUNK_DESCRIPTOR_BYTES: usize = 120;
pub const MAX_V3_PLANNED_GROUPED_FOOTER_BYTES: usize = 64 * 1024 * 1024;

const FOOTER_MAGIC: &[u8; 4] = b"AURP";
const FOOTER_HASH_DOMAIN: &[u8] = b"aura-v3-planned-grouped-footer-v1\0";
const HEADER_HASH_DOMAIN: &[u8] = b"aura-v3-planned-grouped-header-v1\0";
const BODY_HASH_DOMAIN: &[u8] = b"aura-v3-planned-grouped-body-v1\0";
const CHUNK_TABLE_VERSION: u16 = 1;
const TRAILER_BYTES: usize = 12;
const ATTEMPT2_BLOCK_MAGIC: &[u8; 8] = b"AUPGDB02";
const ATTEMPT2_BLOCK_HEADER_BYTES: usize = 72;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedGroupedChunkDescriptor {
    pub chunk_id: u32,
    pub first_global_event: u64,
    pub event_count: u32,
    pub first_global_child: u64,
    pub child_count: u32,
    pub body_relative_offset: u64,
    pub stored_len: u64,
    pub stored_sha256: [u8; 32],
    pub chunk_logical_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedGroupedFooter {
    pub event_count: u64,
    pub child_count: u64,
    pub body_len: u64,
    pub body_layout_version: u16,
    pub block_version: u16,
    pub schema: SchemaDescriptor,
    pub plan: AuraPlanV2,
    pub schema_fingerprint: [u8; 32],
    pub plan_sha256: [u8; 32],
    pub header_sha256: [u8; 32],
    pub body_sha256: [u8; 32],
    pub global_logical_sha256: [u8; 32],
    pub chunks: Vec<V3PlannedGroupedChunkDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedGroupedSummary {
    pub event_count: u64,
    pub child_count: u64,
    pub chunk_count: u32,
    pub header_bytes: u64,
    pub body_bytes: u64,
    pub footer_bytes: u32,
    pub file_bytes: u64,
    pub accounted_file_bytes: u64,
    pub schema_fingerprint: [u8; 32],
    pub plan_sha256: [u8; 32],
    pub global_logical_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedGroupedInspection {
    pub body_encoding: u8,
    pub body_layout_version: u16,
    pub direct_block_version: u16,
    pub compression: &'static str,
    pub plan: PlanV2Inspection,
    pub schema_relationships_authorized_only: bool,
    pub candidates: Vec<V3PlannedGroupedCandidateInspection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedGroupedCandidateInspection {
    pub candidate_id: String,
    pub selection: PlanV2Selection,
    pub authorized: bool,
    pub applicable: bool,
    pub rejection: Option<String>,
    pub complete_bytes: Option<u64>,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedGroupedArtifact {
    pub bytes: Vec<u8>,
    pub summary: V3PlannedGroupedSummary,
    pub inspection: V3PlannedGroupedInspection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedV3PlannedGrouped {
    pub header: AuraHeader,
    pub footer: V3PlannedGroupedFooter,
    pub batches: Vec<AuraV3EventBatch>,
    pub summary: V3PlannedGroupedSummary,
    pub inspection: V3PlannedGroupedInspection,
}

pub fn compile_v3_planned_grouped(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    limits: V3GroupedLimits,
) -> Result<V3PlannedGroupedArtifact> {
    let limits = limits.effective();
    crate::v3_events::validate_v3_grouped_exact_subset(schema)?;
    if batches.len() > limits.max_chunks {
        return Err(AuraError::InvalidValue("v3 planned grouped chunk count"));
    }
    let mut event_count = 0u64;
    let mut child_count = 0u64;
    for batch in batches {
        validate_v3_event_batch(schema, batch, limits.event_limits)?;
        if batch.event_count == 0 {
            return Err(AuraError::InvalidValue("v3 planned grouped empty chunk"));
        }
        event_count = event_count
            .checked_add(u64::from(batch.event_count))
            .filter(|value| *value <= limits.max_events)
            .ok_or(AuraError::InvalidValue("v3 planned grouped event count"))?;
        child_count = child_count
            .checked_add(u64::from(batch.child_count()))
            .filter(|value| *value <= limits.max_children)
            .ok_or(AuraError::InvalidValue("v3 planned grouped child count"))?;
    }
    let total_events = u32::try_from(event_count)
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped event count"))?;
    let total_children = u32::try_from(child_count)
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped child count"))?;
    let plan = AuraPlanV2::direct_for_schema(schema)?;
    let plan_sha256 = plan.hash(schema)?;
    let header = canonical_header(schema)?;
    let header_bytes = header.encode()?;
    let mut body = Vec::new();
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(batches.len())
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    let mut next_event = 0u64;
    let mut next_child = 0u64;
    let mut global =
        CanonicalV3EventHasher::new(schema, total_events, total_children, limits.event_limits)?;
    for (index, batch) in batches.iter().enumerate() {
        let block = encode_v3_event_block(schema, batch, limits.event_limits)?;
        let next_body = body
            .len()
            .checked_add(block.len())
            .filter(|value| *value as u64 <= limits.max_body_bytes)
            .ok_or(AuraError::InvalidValue("v3 planned grouped body length"))?;
        body.try_reserve_exact(block.len())
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
        let stored_sha256 = Sha256::digest(&block).into();
        let chunk_logical_sha256 =
            canonical_v3_event_batch_sha256(schema, batch, limits.event_limits)?;
        chunks.push(V3PlannedGroupedChunkDescriptor {
            chunk_id: u32::try_from(index)
                .map_err(|_| AuraError::InvalidValue("v3 planned grouped chunk count"))?,
            first_global_event: next_event,
            event_count: batch.event_count,
            first_global_child: next_child,
            child_count: batch.child_count(),
            body_relative_offset: body.len() as u64,
            stored_len: block.len() as u64,
            stored_sha256,
            chunk_logical_sha256,
        });
        body.extend_from_slice(&block);
        debug_assert_eq!(body.len(), next_body);
        next_event = next_event
            .checked_add(u64::from(batch.event_count))
            .ok_or(AuraError::InvalidValue("v3 planned grouped event count"))?;
        next_child = next_child
            .checked_add(u64::from(batch.child_count()))
            .ok_or(AuraError::InvalidValue("v3 planned grouped child count"))?;
        global.update_batch(schema, batch)?;
    }
    let schema_fingerprint = canonical_v3_schema_fingerprint(schema)?;
    let footer = V3PlannedGroupedFooter {
        event_count,
        child_count,
        body_len: body.len() as u64,
        body_layout_version: V3_PLANNED_GROUPED_BODY_LAYOUT_VERSION,
        block_version: V3_PLANNED_GROUPED_DIRECT_BLOCK_VERSION,
        schema: schema.clone(),
        plan,
        schema_fingerprint,
        plan_sha256,
        header_sha256: domain_hash(
            HEADER_HASH_DOMAIN,
            &header_bytes,
            "v3 planned grouped header length",
        )?,
        body_sha256: domain_hash(BODY_HASH_DOMAIN, &body, "v3 planned grouped body length")?,
        global_logical_sha256: global.finalize()?,
        chunks,
    };
    let footer_bytes = encode_v3_planned_grouped_footer(&footer, limits)?;
    let footer_len = u32::try_from(footer_bytes.len())
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped footer length"))?;
    let capacity = header_bytes
        .len()
        .checked_add(body.len())
        .and_then(|value| value.checked_add(footer_bytes.len()))
        .and_then(|value| value.checked_add(TRAILER_BYTES))
        .ok_or(AuraError::InvalidValue("v3 planned grouped file length"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    bytes.extend_from_slice(&header_bytes);
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(&footer_bytes);
    bytes.extend_from_slice(&footer_len.to_le_bytes());
    bytes.extend_from_slice(SEAL_MAGIC);
    let summary = summary(&footer, header_bytes.len(), footer_bytes.len(), bytes.len())?;
    Ok(V3PlannedGroupedArtifact {
        bytes,
        inspection: inspection(&footer),
        summary,
    })
}

/// Score complete registry-1 direct, compact registry-2 direct, and compact
/// registry-2 split-domain-direct files. Ties select the earliest candidate.
pub fn compile_v3_planned_grouped_attempt2(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    limits: V3GroupedLimits,
) -> Result<V3PlannedGroupedArtifact> {
    let registry1 = compile_v3_planned_grouped(schema, batches, limits)?;
    let direct = compile_attempt2_candidate(schema, batches, limits, PlanV2Selection::Direct)?;
    let authorized = schema
        .groups
        .first()
        .is_some_and(|group| group.relationships.allows_split());
    let split = if authorized {
        compile_attempt2_candidate(schema, batches, limits, PlanV2Selection::SplitDomainDirect)
            .map_err(|error| error.to_string())
    } else {
        Err("schema does not authorize split-domain direct".to_owned())
    };
    let registry1_bytes = registry1.summary.file_bytes;
    let direct_bytes = direct.summary.file_bytes;
    let split_bytes = split.as_ref().ok().map(|value| value.summary.file_bytes);
    let best_direct = registry1_bytes.min(direct_bytes);
    let select_split = split_bytes.is_some_and(|bytes| bytes < best_direct);
    let select_compact_direct = !select_split && direct_bytes < registry1_bytes;
    let split_rejection = split
        .as_ref()
        .err()
        .cloned()
        .or_else(|| (!select_split).then(|| "complete cost did not beat direct".to_owned()));
    let mut selected = if select_split {
        split.unwrap()
    } else if select_compact_direct {
        direct
    } else {
        registry1
    };
    selected.inspection.candidates = vec![
        V3PlannedGroupedCandidateInspection {
            candidate_id: "registry1-direct".to_owned(),
            selection: PlanV2Selection::Direct,
            authorized: true,
            applicable: true,
            rejection: (select_split || select_compact_direct)
                .then(|| "complete cost did not beat selected candidate".to_owned()),
            complete_bytes: Some(registry1_bytes),
            selected: !select_split && !select_compact_direct,
        },
        V3PlannedGroupedCandidateInspection {
            candidate_id: "registry2-compact-direct".to_owned(),
            selection: PlanV2Selection::Direct,
            authorized: true,
            applicable: true,
            rejection: (!select_compact_direct)
                .then(|| "complete cost did not beat earlier direct".to_owned()),
            complete_bytes: Some(direct_bytes),
            selected: select_compact_direct,
        },
        V3PlannedGroupedCandidateInspection {
            candidate_id: "registry2-compact-split-domain-direct".to_owned(),
            selection: PlanV2Selection::SplitDomainDirect,
            authorized,
            applicable: split_bytes.is_some(),
            rejection: split_rejection,
            complete_bytes: split_bytes,
            selected: select_split,
        },
    ];
    Ok(selected)
}

/// Encode one attempt-2 candidate for development inverse/cost auditing.
/// Selection callers should use [`compile_v3_planned_grouped_attempt2`].
pub fn compile_v3_planned_grouped_attempt2_candidate(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    limits: V3GroupedLimits,
    selection: PlanV2Selection,
) -> Result<V3PlannedGroupedArtifact> {
    compile_attempt2_candidate(schema, batches, limits, selection)
}

fn compile_attempt2_candidate(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    limits: V3GroupedLimits,
    selection: PlanV2Selection,
) -> Result<V3PlannedGroupedArtifact> {
    let limits = limits.effective();
    crate::v3_events::validate_v3_grouped_exact_subset(schema)?;
    if batches.len() > limits.max_chunks {
        return Err(AuraError::InvalidValue("v3 planned grouped chunk count"));
    }
    let mut event_count = 0u64;
    let mut child_count = 0u64;
    for batch in batches {
        validate_v3_event_batch(schema, batch, limits.event_limits)?;
        if batch.event_count == 0 {
            return Err(AuraError::InvalidValue("v3 planned grouped empty chunk"));
        }
        event_count = event_count
            .checked_add(u64::from(batch.event_count))
            .filter(|value| *value <= limits.max_events)
            .ok_or(AuraError::InvalidValue("v3 planned grouped event count"))?;
        child_count = child_count
            .checked_add(u64::from(batch.child_count()))
            .filter(|value| *value <= limits.max_children)
            .ok_or(AuraError::InvalidValue("v3 planned grouped child count"))?;
    }
    let plan = AuraPlanV2::candidate_for_schema(schema, selection)?;
    let plan_sha256 = plan.hash(schema)?;
    let header_bytes = canonical_header(schema)?.encode()?;
    let mut body = Vec::new();
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(batches.len())
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    let mut next_event = 0u64;
    let mut next_child = 0u64;
    let mut global = CanonicalV3EventHasher::new(
        schema,
        u32::try_from(event_count)
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped event count"))?,
        u32::try_from(child_count)
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped child count"))?,
        limits.event_limits,
    )?;
    for (index, batch) in batches.iter().enumerate() {
        let block = encode_attempt2_block(schema, batch, &plan, limits.event_limits)?;
        let next_body = body
            .len()
            .checked_add(block.len())
            .filter(|value| *value as u64 <= limits.max_body_bytes)
            .ok_or(AuraError::InvalidValue("v3 planned grouped body length"))?;
        body.try_reserve_exact(block.len())
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
        chunks.push(V3PlannedGroupedChunkDescriptor {
            chunk_id: u32::try_from(index)
                .map_err(|_| AuraError::InvalidValue("v3 planned grouped chunk count"))?,
            first_global_event: next_event,
            event_count: batch.event_count,
            first_global_child: next_child,
            child_count: batch.child_count(),
            body_relative_offset: body.len() as u64,
            stored_len: block.len() as u64,
            stored_sha256: Sha256::digest(&block).into(),
            chunk_logical_sha256: canonical_v3_event_batch_sha256(
                schema,
                batch,
                limits.event_limits,
            )?,
        });
        body.extend_from_slice(&block);
        debug_assert_eq!(body.len(), next_body);
        next_event = next_event
            .checked_add(u64::from(batch.event_count))
            .ok_or(AuraError::InvalidValue("v3 planned grouped event count"))?;
        next_child = next_child
            .checked_add(u64::from(batch.child_count()))
            .ok_or(AuraError::InvalidValue("v3 planned grouped child count"))?;
        global.update_batch(schema, batch)?;
    }
    let schema_fingerprint = canonical_v3_schema_fingerprint(schema)?;
    let footer = V3PlannedGroupedFooter {
        event_count,
        child_count,
        body_len: body.len() as u64,
        body_layout_version: V3_PLANNED_GROUPED_COMPACT_BODY_LAYOUT_VERSION,
        block_version: V3_PLANNED_GROUPED_COMPACT_BLOCK_VERSION,
        schema: schema.clone(),
        plan,
        schema_fingerprint,
        plan_sha256,
        header_sha256: domain_hash(
            HEADER_HASH_DOMAIN,
            &header_bytes,
            "v3 planned grouped header length",
        )?,
        body_sha256: domain_hash(BODY_HASH_DOMAIN, &body, "v3 planned grouped body length")?,
        global_logical_sha256: global.finalize()?,
        chunks,
    };
    seal_artifact(header_bytes, body, footer, limits)
}

fn seal_artifact(
    header_bytes: Vec<u8>,
    body: Vec<u8>,
    footer: V3PlannedGroupedFooter,
    limits: V3GroupedLimits,
) -> Result<V3PlannedGroupedArtifact> {
    let footer_bytes = encode_v3_planned_grouped_footer(&footer, limits)?;
    let capacity = header_bytes
        .len()
        .checked_add(body.len())
        .and_then(|value| value.checked_add(footer_bytes.len()))
        .and_then(|value| value.checked_add(TRAILER_BYTES))
        .ok_or(AuraError::InvalidValue("v3 planned grouped file length"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    bytes.extend_from_slice(&header_bytes);
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(&footer_bytes);
    bytes.extend_from_slice(
        &u32::try_from(footer_bytes.len())
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped footer length"))?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(SEAL_MAGIC);
    Ok(V3PlannedGroupedArtifact {
        summary: summary(&footer, header_bytes.len(), footer_bytes.len(), bytes.len())?,
        inspection: inspection(&footer),
        bytes,
    })
}

fn encode_attempt2_block(
    schema: &SchemaDescriptor,
    batch: &AuraV3EventBatch,
    plan: &AuraPlanV2,
    limits: crate::V3EventLimits,
) -> Result<Vec<u8>> {
    validate_v3_event_batch(schema, batch, limits)?;
    let repeated_fields = schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Repeated)
        .collect::<Vec<_>>();
    let discriminator_position = repeated_fields
        .iter()
        .position(|field| field.index == plan.discriminator_slot)
        .ok_or(AuraError::InvalidValue("v3 planned split discriminator"))?;
    let selector = match &batch.repeated_columns[discriminator_position].values {
        AuraV3ColumnValues::U8(values) if values.iter().all(|value| *value <= 1) => values,
        _ => return Err(AuraError::InvalidValue("v3 planned split selector")),
    };
    let mut domain = [Vec::new(), Vec::new()];
    for (index, side) in selector.iter().copied().enumerate() {
        domain[usize::from(side)].push(index);
    }
    let physical_count = plan.streams.iter().try_fold(1usize, |total, stream| {
        total
            .checked_add(stream.physical_stream_ids.len())
            .ok_or(AuraError::InvalidValue("v3 planned physical stream count"))
    })?;
    let mut out = Vec::new();
    out.try_reserve_exact(ATTEMPT2_BLOCK_HEADER_BYTES)
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    out.extend_from_slice(ATTEMPT2_BLOCK_MAGIC);
    put_u16(&mut out, V3_PLANNED_GROUPED_COMPACT_BLOCK_VERSION);
    out.push(plan.selection as u8);
    out.push(0);
    put_u32(&mut out, schema.schema_id);
    out.extend_from_slice(&canonical_v3_schema_fingerprint(schema)?);
    put_u32(&mut out, batch.event_count);
    put_u32(&mut out, batch.child_count());
    put_u16(
        &mut out,
        u16::try_from(batch.event_columns.len())
            .map_err(|_| AuraError::InvalidValue("v3 planned event column count"))?,
    );
    put_u16(
        &mut out,
        u16::try_from(batch.repeated_columns.len())
            .map_err(|_| AuraError::InvalidValue("v3 planned repeated column count"))?,
    );
    put_u16(
        &mut out,
        u16::try_from(physical_count)
            .map_err(|_| AuraError::InvalidValue("v3 planned physical stream count"))?,
    );
    put_u16(&mut out, 0);
    put_u64(&mut out, 0);
    debug_assert_eq!(out.len(), ATTEMPT2_BLOCK_HEADER_BYTES);
    for offset in &batch.child_offsets {
        put_u32(&mut out, *offset);
    }
    for column in &batch.event_columns {
        encode_compact_lane(column, batch.event_count as usize, &mut out)?;
    }
    out.extend_from_slice(selector);
    for (field, column) in repeated_fields.iter().zip(&batch.repeated_columns) {
        if field.index == plan.discriminator_slot {
            continue;
        }
        if plan.selection == PlanV2Selection::Direct {
            encode_compact_lane(column, batch.child_count() as usize, &mut out)?;
        } else {
            let zero = select_column(column, &domain[0])?;
            let one = select_column(column, &domain[1])?;
            encode_compact_lane(&zero, domain[0].len(), &mut out)?;
            encode_compact_lane(&one, domain[1].len(), &mut out)?;
        }
    }
    let total =
        u64::try_from(out.len()).map_err(|_| AuraError::InvalidValue("v3 planned block length"))?;
    out[64..72].copy_from_slice(&total.to_le_bytes());
    if out.len() > limits.max_block_bytes {
        return Err(AuraError::InvalidValue("v3 planned block length"));
    }
    let decoded = decode_attempt2_block(schema, &out, plan, limits)?;
    if decoded != *batch {
        return Err(AuraError::InvalidValue("v3 planned split inverse"));
    }
    Ok(out)
}

fn decode_attempt2_block(
    schema: &SchemaDescriptor,
    bytes: &[u8],
    plan: &AuraPlanV2,
    limits: crate::V3EventLimits,
) -> Result<AuraV3EventBatch> {
    if bytes.len() > limits.max_block_bytes || bytes.len() < ATTEMPT2_BLOCK_HEADER_BYTES {
        return Err(AuraError::InvalidValue("v3 planned block length"));
    }
    let mut reader = ByteReader::new(bytes);
    if reader.read_exact(8)? != ATTEMPT2_BLOCK_MAGIC
        || reader.read_u16_le()? != V3_PLANNED_GROUPED_COMPACT_BLOCK_VERSION
        || reader.read_u8()? != plan.selection as u8
        || reader.read_u8()? != 0
        || reader.read_u32_le()? != schema.schema_id
        || reader.read_exact(32)? != canonical_v3_schema_fingerprint(schema)?
    {
        return Err(AuraError::InvalidValue("v3 planned block header"));
    }
    let event_count = reader.read_u32_le()?;
    let child_count = reader.read_u32_le()?;
    let event_fields = schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Event)
        .collect::<Vec<_>>();
    let repeated_fields = schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Repeated)
        .collect::<Vec<_>>();
    let physical_count = plan
        .streams
        .iter()
        .map(|stream| stream.physical_stream_ids.len())
        .sum::<usize>()
        .checked_add(1)
        .ok_or(AuraError::InvalidValue("v3 planned physical stream count"))?;
    if reader.read_u16_le()? as usize != event_fields.len()
        || reader.read_u16_le()? as usize != repeated_fields.len()
        || reader.read_u16_le()? as usize != physical_count
        || reader.read_u16_le()? != 0
        || reader.read_u64_le()? as usize != bytes.len()
        || event_count as usize > limits.max_events
        || child_count as usize > limits.max_children
    {
        return Err(AuraError::InvalidValue("v3 planned block counts"));
    }
    let mut child_offsets = Vec::new();
    child_offsets
        .try_reserve_exact(event_count as usize + 1)
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    for _ in 0..=event_count {
        child_offsets.push(reader.read_u32_le()?);
    }
    if child_offsets.first() != Some(&0)
        || child_offsets.last() != Some(&child_count)
        || child_offsets.windows(2).any(|pair| pair[0] > pair[1])
    {
        return Err(AuraError::InvalidValue("v3 planned child offsets"));
    }
    let value_limits = V3ValueLimits {
        max_block_bytes: limits.max_block_bytes,
        max_variable_value_bytes: limits.max_variable_value_bytes,
        max_rows: limits.max_events.max(limits.max_children),
    };
    let mut event_columns = Vec::new();
    event_columns
        .try_reserve_exact(event_fields.len())
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    for field in event_fields {
        event_columns.push(decode_compact_lane(
            field.index,
            field.field_type,
            field.nullable,
            event_count as usize,
            &mut reader,
            value_limits,
        )?);
    }
    let selector = reader.read_exact(child_count as usize)?.to_vec();
    if selector.iter().any(|side| *side > 1) {
        return Err(AuraError::InvalidValue("v3 planned split selector"));
    }
    let domain_count = [
        selector.iter().filter(|side| **side == 0).count(),
        selector.iter().filter(|side| **side == 1).count(),
    ];
    let mut repeated_columns = Vec::new();
    repeated_columns
        .try_reserve_exact(repeated_fields.len())
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    for field in repeated_fields {
        if field.index == plan.discriminator_slot {
            repeated_columns.push(AuraV3Column {
                slot: field.index,
                validity: None,
                values: AuraV3ColumnValues::U8(selector.clone()),
            });
        } else if plan.selection == PlanV2Selection::Direct {
            let column = decode_compact_lane(
                field.index,
                field.field_type,
                field.nullable,
                child_count as usize,
                &mut reader,
                value_limits,
            )?;
            repeated_columns.push(column);
        } else {
            let zero = decode_compact_lane(
                field.index,
                field.field_type,
                field.nullable,
                domain_count[0],
                &mut reader,
                value_limits,
            )?;
            let one = decode_compact_lane(
                field.index,
                field.field_type,
                field.nullable,
                domain_count[1],
                &mut reader,
                value_limits,
            )?;
            repeated_columns.push(merge_columns(&zero, &one, &selector)?);
        }
    }
    reader.finish()?;
    let batch = AuraV3EventBatch {
        schema_id: schema.schema_id,
        event_count,
        child_offsets,
        event_columns,
        repeated_columns,
    };
    validate_v3_event_batch(schema, &batch, limits)?;
    Ok(batch)
}

fn encode_compact_lane(column: &AuraV3Column, rows: usize, out: &mut Vec<u8>) -> Result<()> {
    let mut encoded = Vec::new();
    encode_column(column, rows, &mut encoded)?;
    let payload = encoded
        .get(20..)
        .ok_or(AuraError::InvalidValue("v3 planned compact lane"))?;
    out.try_reserve_exact(payload.len())
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    out.extend_from_slice(payload);
    Ok(())
}

fn decode_compact_lane(
    slot: u16,
    field_type: crate::FieldType,
    nullable: bool,
    rows: usize,
    reader: &mut ByteReader<'_>,
    limits: V3ValueLimits,
) -> Result<AuraV3Column> {
    let validity_len = if nullable { rows.div_ceil(8) } else { 0 };
    let variable = matches!(
        field_type,
        crate::FieldType::Utf8 | crate::FieldType::DecimalText
    );
    let (fixed_len, offsets_len, data_len) = if variable {
        let offsets_len = rows
            .checked_add(1)
            .and_then(|value| value.checked_mul(4))
            .ok_or(AuraError::InvalidValue("v3 planned compact lane"))?;
        let prefix = reader.read_exact(validity_len + offsets_len)?;
        let final_offset = u32::from_le_bytes(
            prefix[prefix.len() - 4..]
                .try_into()
                .map_err(|_| AuraError::UnexpectedEof)?,
        ) as usize;
        let data = reader.read_exact(final_offset)?;
        let mut synthetic = compact_column_header(
            slot,
            field_type,
            nullable,
            true,
            validity_len,
            0,
            offsets_len,
            final_offset,
        )?;
        synthetic.extend_from_slice(prefix);
        synthetic.extend_from_slice(data);
        let mut synthetic_reader = ByteReader::new(&synthetic);
        return decode_column(
            slot,
            field_type,
            nullable,
            rows,
            &mut synthetic_reader,
            limits,
        );
    } else {
        let width =
            fixed_width(field_type).ok_or(AuraError::InvalidValue("v3 planned compact lane"))?;
        (
            rows.checked_mul(width)
                .ok_or(AuraError::InvalidValue("v3 planned compact lane"))?,
            0,
            0,
        )
    };
    let payload = reader.read_exact(validity_len + fixed_len)?;
    let mut synthetic = compact_column_header(
        slot,
        field_type,
        nullable,
        false,
        validity_len,
        fixed_len,
        offsets_len,
        data_len,
    )?;
    synthetic.extend_from_slice(payload);
    let mut synthetic_reader = ByteReader::new(&synthetic);
    decode_column(
        slot,
        field_type,
        nullable,
        rows,
        &mut synthetic_reader,
        limits,
    )
}

#[allow(clippy::too_many_arguments)]
fn compact_column_header(
    slot: u16,
    field_type: crate::FieldType,
    nullable: bool,
    variable: bool,
    validity_len: usize,
    fixed_len: usize,
    offsets_len: usize,
    data_len: usize,
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(20);
    put_u16(&mut out, slot);
    out.push(field_type as u8);
    out.push(u8::from(nullable) | (u8::from(variable) << 1));
    for length in [validity_len, fixed_len, offsets_len, data_len] {
        put_u32(
            &mut out,
            u32::try_from(length)
                .map_err(|_| AuraError::InvalidValue("v3 planned compact lane"))?,
        );
    }
    Ok(out)
}

const fn fixed_width(field_type: crate::FieldType) -> Option<usize> {
    match field_type {
        crate::FieldType::I8 | crate::FieldType::U8 => Some(1),
        crate::FieldType::I16 | crate::FieldType::U16 => Some(2),
        crate::FieldType::I32 | crate::FieldType::U32 => Some(4),
        crate::FieldType::I64
        | crate::FieldType::U64
        | crate::FieldType::TimestampNs
        | crate::FieldType::TimestampMs => Some(8),
        crate::FieldType::I128 | crate::FieldType::Opaque16 => Some(16),
        crate::FieldType::Utf8 | crate::FieldType::DecimalText => None,
    }
}

fn select_column(column: &AuraV3Column, indexes: &[usize]) -> Result<AuraV3Column> {
    let values = select_values(&column.values, indexes)?;
    let validity = column
        .validity
        .as_deref()
        .map(|bitmap| select_validity(bitmap, indexes));
    Ok(AuraV3Column {
        slot: column.slot,
        validity,
        values,
    })
}

fn select_values(values: &AuraV3ColumnValues, indexes: &[usize]) -> Result<AuraV3ColumnValues> {
    macro_rules! select {
        ($variant:ident, $values:expr) => {
            AuraV3ColumnValues::$variant(indexes.iter().map(|index| $values[*index]).collect())
        };
    }
    Ok(match values {
        AuraV3ColumnValues::I8(values) => select!(I8, values),
        AuraV3ColumnValues::U8(values) => select!(U8, values),
        AuraV3ColumnValues::I16(values) => select!(I16, values),
        AuraV3ColumnValues::U16(values) => select!(U16, values),
        AuraV3ColumnValues::I32(values) => select!(I32, values),
        AuraV3ColumnValues::U32(values) => select!(U32, values),
        AuraV3ColumnValues::I64(values) => select!(I64, values),
        AuraV3ColumnValues::U64(values) => select!(U64, values),
        AuraV3ColumnValues::TimestampNs(values) => select!(TimestampNs, values),
        AuraV3ColumnValues::I128(values) => select!(I128, values),
        AuraV3ColumnValues::Opaque16(values) => select!(Opaque16, values),
        AuraV3ColumnValues::TimestampMs(values) => select!(TimestampMs, values),
        AuraV3ColumnValues::Utf8(values) => {
            AuraV3ColumnValues::Utf8(select_variable(values, indexes)?)
        }
        AuraV3ColumnValues::DecimalText(values) => {
            AuraV3ColumnValues::DecimalText(select_variable(values, indexes)?)
        }
    })
}

fn select_variable(
    values: &AuraV3VariableColumn,
    indexes: &[usize],
) -> Result<AuraV3VariableColumn> {
    let mut offsets = Vec::new();
    offsets
        .try_reserve_exact(indexes.len() + 1)
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    offsets.push(0);
    let mut data = Vec::new();
    for index in indexes {
        let start = values.offsets[*index] as usize;
        let end = values.offsets[*index + 1] as usize;
        let part = values
            .data
            .get(start..end)
            .ok_or(AuraError::InvalidValue("v3 planned variable range"))?;
        data.try_reserve_exact(part.len())
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
        data.extend_from_slice(part);
        offsets.push(
            u32::try_from(data.len())
                .map_err(|_| AuraError::InvalidValue("v3 planned variable length"))?,
        );
    }
    Ok(AuraV3VariableColumn { offsets, data })
}

fn select_validity(bitmap: &[u8], indexes: &[usize]) -> Vec<u8> {
    let mut selected = vec![0u8; indexes.len().div_ceil(8)];
    for (target, source) in indexes.iter().copied().enumerate() {
        if bitmap[source / 8] & (1 << (source % 8)) != 0 {
            selected[target / 8] |= 1 << (target % 8);
        }
    }
    selected
}

fn merge_columns(zero: &AuraV3Column, one: &AuraV3Column, selector: &[u8]) -> Result<AuraV3Column> {
    if zero.slot != one.slot || zero.values.field_type() != one.values.field_type() {
        return Err(AuraError::InvalidValue("v3 planned split lane identity"));
    }
    let mut zero_index = 0usize;
    let mut one_index = 0usize;
    let mut source = Vec::new();
    source
        .try_reserve_exact(selector.len())
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    for side in selector {
        if *side == 0 {
            source.push((0u8, zero_index));
            zero_index += 1;
        } else {
            source.push((1u8, one_index));
            one_index += 1;
        }
    }
    let values = merge_values(&zero.values, &one.values, &source)?;
    let validity = match (zero.validity.as_deref(), one.validity.as_deref()) {
        (None, None) => None,
        (Some(zero_bits), Some(one_bits)) => {
            let mut merged = vec![0u8; selector.len().div_ceil(8)];
            for (target, (side, index)) in source.iter().copied().enumerate() {
                let bits = if side == 0 { zero_bits } else { one_bits };
                if bits[index / 8] & (1 << (index % 8)) != 0 {
                    merged[target / 8] |= 1 << (target % 8);
                }
            }
            Some(merged)
        }
        _ => return Err(AuraError::InvalidValue("v3 planned split presence")),
    };
    Ok(AuraV3Column {
        slot: zero.slot,
        validity,
        values,
    })
}

fn merge_values(
    zero: &AuraV3ColumnValues,
    one: &AuraV3ColumnValues,
    source: &[(u8, usize)],
) -> Result<AuraV3ColumnValues> {
    macro_rules! merge {
        ($variant:ident, $zero:expr, $one:expr) => {
            AuraV3ColumnValues::$variant(
                source
                    .iter()
                    .map(|(side, index)| {
                        if *side == 0 {
                            $zero[*index]
                        } else {
                            $one[*index]
                        }
                    })
                    .collect(),
            )
        };
    }
    Ok(match (zero, one) {
        (AuraV3ColumnValues::I8(z), AuraV3ColumnValues::I8(o)) => merge!(I8, z, o),
        (AuraV3ColumnValues::U8(z), AuraV3ColumnValues::U8(o)) => merge!(U8, z, o),
        (AuraV3ColumnValues::I16(z), AuraV3ColumnValues::I16(o)) => merge!(I16, z, o),
        (AuraV3ColumnValues::U16(z), AuraV3ColumnValues::U16(o)) => merge!(U16, z, o),
        (AuraV3ColumnValues::I32(z), AuraV3ColumnValues::I32(o)) => merge!(I32, z, o),
        (AuraV3ColumnValues::U32(z), AuraV3ColumnValues::U32(o)) => merge!(U32, z, o),
        (AuraV3ColumnValues::I64(z), AuraV3ColumnValues::I64(o)) => merge!(I64, z, o),
        (AuraV3ColumnValues::U64(z), AuraV3ColumnValues::U64(o)) => merge!(U64, z, o),
        (AuraV3ColumnValues::TimestampNs(z), AuraV3ColumnValues::TimestampNs(o)) => {
            merge!(TimestampNs, z, o)
        }
        (AuraV3ColumnValues::I128(z), AuraV3ColumnValues::I128(o)) => merge!(I128, z, o),
        (AuraV3ColumnValues::Opaque16(z), AuraV3ColumnValues::Opaque16(o)) => {
            merge!(Opaque16, z, o)
        }
        (AuraV3ColumnValues::TimestampMs(z), AuraV3ColumnValues::TimestampMs(o)) => {
            merge!(TimestampMs, z, o)
        }
        (AuraV3ColumnValues::Utf8(z), AuraV3ColumnValues::Utf8(o)) => {
            AuraV3ColumnValues::Utf8(merge_variable(z, o, source)?)
        }
        (AuraV3ColumnValues::DecimalText(z), AuraV3ColumnValues::DecimalText(o)) => {
            AuraV3ColumnValues::DecimalText(merge_variable(z, o, source)?)
        }
        _ => return Err(AuraError::InvalidValue("v3 planned split lane type")),
    })
}

fn merge_variable(
    zero: &AuraV3VariableColumn,
    one: &AuraV3VariableColumn,
    source: &[(u8, usize)],
) -> Result<AuraV3VariableColumn> {
    let mut offsets = Vec::with_capacity(source.len() + 1);
    let mut data = Vec::new();
    offsets.push(0);
    for (side, index) in source {
        let lane = if *side == 0 { zero } else { one };
        let part = lane
            .data
            .get(lane.offsets[*index] as usize..lane.offsets[*index + 1] as usize)
            .ok_or(AuraError::InvalidValue("v3 planned variable range"))?;
        data.try_reserve_exact(part.len())
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
        data.extend_from_slice(part);
        offsets.push(
            u32::try_from(data.len())
                .map_err(|_| AuraError::InvalidValue("v3 planned variable length"))?,
        );
    }
    Ok(AuraV3VariableColumn { offsets, data })
}

pub fn decode_v3_planned_grouped(
    bytes: &[u8],
    limits: V3GroupedLimits,
) -> Result<DecodedV3PlannedGrouped> {
    let limits = limits.effective();
    if bytes.len() < crate::V3_HEADER_PREFIX_SIZE + TRAILER_BYTES {
        return Err(AuraError::UnexpectedEof);
    }
    let seal_start = bytes.len() - SEAL_MAGIC.len();
    if &bytes[seal_start..] != SEAL_MAGIC {
        return Err(AuraError::InvalidMagic {
            expected: "sealed:)",
        });
    }
    let footer_len_offset = seal_start.checked_sub(4).ok_or(AuraError::UnexpectedEof)?;
    let footer_len = u32::from_le_bytes(
        bytes[footer_len_offset..seal_start]
            .try_into()
            .map_err(|_| AuraError::UnexpectedEof)?,
    ) as usize;
    if footer_len
        > limits
            .max_footer_bytes
            .min(MAX_V3_PLANNED_GROUPED_FOOTER_BYTES)
    {
        return Err(AuraError::InvalidValue("v3 planned grouped footer length"));
    }
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::InvalidValue("v3 planned grouped footer length"))?;
    let header_len = AuraHeader::encoded_len(bytes)?;
    if header_len > footer_start {
        return Err(AuraError::InvalidValue("v3 planned grouped file ranges"));
    }
    let header_bytes = &bytes[..header_len];
    let body = &bytes[header_len..footer_start];
    if body.len() as u64 > limits.max_body_bytes {
        return Err(AuraError::InvalidValue("v3 planned grouped body length"));
    }
    let header = AuraHeader::decode(header_bytes)?;
    let footer = decode_v3_planned_grouped_footer(&bytes[footer_start..footer_len_offset], limits)?;
    validate_header(&header, &footer, header_bytes)?;
    if footer.body_len != body.len() as u64
        || footer.header_sha256
            != domain_hash(
                HEADER_HASH_DOMAIN,
                header_bytes,
                "v3 planned grouped header length",
            )?
        || footer.body_sha256
            != domain_hash(BODY_HASH_DOMAIN, body, "v3 planned grouped body length")?
    {
        return Err(AuraError::InvalidValue("v3 planned grouped envelope hash"));
    }
    let total_events = u32::try_from(footer.event_count)
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped event count"))?;
    let total_children = u32::try_from(footer.child_count)
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped child count"))?;
    let mut global = CanonicalV3EventHasher::new(
        &footer.schema,
        total_events,
        total_children,
        limits.event_limits,
    )?;
    let mut batches = Vec::new();
    batches
        .try_reserve_exact(footer.chunks.len())
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    for chunk in &footer.chunks {
        let start = usize::try_from(chunk.body_relative_offset)
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped chunk range"))?;
        let end = start
            .checked_add(
                usize::try_from(chunk.stored_len)
                    .map_err(|_| AuraError::InvalidValue("v3 planned grouped chunk range"))?,
            )
            .ok_or(AuraError::InvalidValue("v3 planned grouped chunk range"))?;
        let block = body
            .get(start..end)
            .ok_or(AuraError::InvalidValue("v3 planned grouped chunk range"))?;
        if Sha256::digest(block).as_slice() != chunk.stored_sha256 {
            return Err(AuraError::InvalidValue("v3 planned grouped stored hash"));
        }
        let batch = if footer.block_version == V3_PLANNED_GROUPED_DIRECT_BLOCK_VERSION {
            decode_v3_event_block(&footer.schema, block, limits.event_limits)?
        } else {
            decode_attempt2_block(&footer.schema, block, &footer.plan, limits.event_limits)?
        };
        if batch.event_count != chunk.event_count
            || batch.child_count() != chunk.child_count
            || canonical_v3_event_batch_sha256(&footer.schema, &batch, limits.event_limits)?
                != chunk.chunk_logical_sha256
        {
            return Err(AuraError::InvalidValue("v3 planned grouped chunk identity"));
        }
        validate_direct_batch_against_plan(&footer.plan, &batch)?;
        global.update_batch(&footer.schema, &batch)?;
        batches.push(batch);
    }
    if global.finalize()? != footer.global_logical_sha256 {
        return Err(AuraError::InvalidValue("v3 planned grouped logical hash"));
    }
    let summary = summary(&footer, header_len, footer_len, bytes.len())?;
    Ok(DecodedV3PlannedGrouped {
        inspection: inspection(&footer),
        header,
        footer,
        batches,
        summary,
    })
}

pub fn encode_v3_planned_grouped_footer(
    footer: &V3PlannedGroupedFooter,
    limits: V3GroupedLimits,
) -> Result<Vec<u8>> {
    let limits = limits.effective();
    validate_footer(footer, limits)?;
    let schema_bytes = encode_schema_descriptor(&footer.schema)?;
    let plan_bytes = footer.plan.encode(&footer.schema)?;
    let length = V3_PLANNED_GROUPED_FOOTER_PREFIX_BYTES
        .checked_add(schema_bytes.len())
        .and_then(|value| value.checked_add(plan_bytes.len()))
        .and_then(|value| value.checked_add(8))
        .and_then(|value| {
            value.checked_add(
                footer
                    .chunks
                    .len()
                    .checked_mul(V3_PLANNED_GROUPED_CHUNK_DESCRIPTOR_BYTES)?,
            )
        })
        .and_then(|value| value.checked_add(32))
        .filter(|value| {
            *value
                <= limits
                    .max_footer_bytes
                    .min(MAX_V3_PLANNED_GROUPED_FOOTER_BYTES)
        })
        .ok_or(AuraError::InvalidValue("v3 planned grouped footer length"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    bytes.extend_from_slice(FOOTER_MAGIC);
    put_u16(&mut bytes, 3);
    put_u16(&mut bytes, V3_PLANNED_GROUPED_FOOTER_LAYOUT_VERSION);
    bytes.extend_from_slice(&[V3_PLANNED_GROUPED_BODY_ENCODING, 0, 0, 0]);
    put_u64(&mut bytes, footer.event_count);
    put_u64(&mut bytes, footer.child_count);
    put_u64(&mut bytes, footer.body_len);
    put_u32_len(
        &mut bytes,
        footer.chunks.len(),
        "v3 planned grouped chunk count",
    )?;
    put_u32(&mut bytes, footer.schema.schema_id);
    put_u16(&mut bytes, footer.body_layout_version);
    put_u16(&mut bytes, footer.block_version);
    put_u32_len(
        &mut bytes,
        schema_bytes.len(),
        "v3 planned grouped schema length",
    )?;
    put_u32_len(
        &mut bytes,
        plan_bytes.len(),
        "v3 planned grouped plan length",
    )?;
    bytes.extend_from_slice(&footer.schema_fingerprint);
    bytes.extend_from_slice(&footer.plan_sha256);
    bytes.extend_from_slice(&footer.header_sha256);
    bytes.extend_from_slice(&footer.body_sha256);
    bytes.extend_from_slice(&footer.global_logical_sha256);
    debug_assert_eq!(bytes.len(), V3_PLANNED_GROUPED_FOOTER_PREFIX_BYTES);
    bytes.extend_from_slice(&schema_bytes);
    bytes.extend_from_slice(&plan_bytes);
    put_u16(&mut bytes, CHUNK_TABLE_VERSION);
    put_u16(&mut bytes, V3_PLANNED_GROUPED_CHUNK_DESCRIPTOR_BYTES as u16);
    put_u32_len(
        &mut bytes,
        footer.chunks.len(),
        "v3 planned grouped chunk count",
    )?;
    for chunk in &footer.chunks {
        put_u32(&mut bytes, chunk.chunk_id);
        put_u32(&mut bytes, 0);
        put_u64(&mut bytes, chunk.first_global_event);
        put_u32(&mut bytes, chunk.event_count);
        put_u16(&mut bytes, footer.block_version);
        put_u16(&mut bytes, 0);
        put_u64(&mut bytes, chunk.first_global_child);
        put_u32(&mut bytes, chunk.child_count);
        put_u32(&mut bytes, 0);
        put_u64(&mut bytes, chunk.body_relative_offset);
        put_u64(&mut bytes, chunk.stored_len);
        bytes.extend_from_slice(&chunk.stored_sha256);
        bytes.extend_from_slice(&chunk.chunk_logical_sha256);
    }
    let hash = domain_hash(
        FOOTER_HASH_DOMAIN,
        &bytes,
        "v3 planned grouped footer length",
    )?;
    bytes.extend_from_slice(&hash);
    debug_assert_eq!(bytes.len(), length);
    Ok(bytes)
}

pub fn decode_v3_planned_grouped_footer(
    bytes: &[u8],
    limits: V3GroupedLimits,
) -> Result<V3PlannedGroupedFooter> {
    let limits = limits.effective();
    if bytes.len()
        > limits
            .max_footer_bytes
            .min(MAX_V3_PLANNED_GROUPED_FOOTER_BYTES)
    {
        return Err(AuraError::InvalidValue("v3 planned grouped footer length"));
    }
    if bytes.len() < V3_PLANNED_GROUPED_FOOTER_PREFIX_BYTES + 8 + 32 {
        return Err(AuraError::UnexpectedEof);
    }
    let hash_start = bytes.len() - 32;
    if domain_hash(
        FOOTER_HASH_DOMAIN,
        &bytes[..hash_start],
        "v3 planned grouped footer length",
    )? != bytes[hash_start..]
    {
        return Err(AuraError::InvalidValue("v3 planned grouped footer hash"));
    }
    let mut reader = Reader::new(&bytes[..hash_start]);
    if reader.take(4)? != FOOTER_MAGIC
        || reader.u16()? != 3
        || reader.u16()? != V3_PLANNED_GROUPED_FOOTER_LAYOUT_VERSION
        || reader.u8()? != V3_PLANNED_GROUPED_BODY_ENCODING
        || reader.u8()? != 0
        || reader.u8()? != 0
        || reader.u8()? != 0
    {
        return Err(AuraError::InvalidValue("v3 planned grouped footer tuple"));
    }
    let event_count = reader.u64()?;
    let child_count = reader.u64()?;
    let body_len = reader.u64()?;
    let chunk_count = reader.u32()? as usize;
    let schema_id = reader.u32()?;
    let body_layout_version = reader.u16()?;
    let block_version = reader.u16()?;
    if !matches!(
        (body_layout_version, block_version),
        (
            V3_PLANNED_GROUPED_BODY_LAYOUT_VERSION,
            V3_PLANNED_GROUPED_DIRECT_BLOCK_VERSION
        ) | (
            V3_PLANNED_GROUPED_COMPACT_BODY_LAYOUT_VERSION,
            V3_PLANNED_GROUPED_COMPACT_BLOCK_VERSION
        )
    ) {
        return Err(AuraError::InvalidValue("v3 planned grouped body layout"));
    }
    let schema_len = reader.u32()? as usize;
    let plan_len = reader.u32()? as usize;
    if schema_len > crate::MAX_SCHEMA_JSON_BYTES
        || plan_len > MAX_AURA_PLAN_V2_BYTES
        || chunk_count > limits.max_chunks
    {
        return Err(AuraError::InvalidValue("v3 planned grouped footer counts"));
    }
    let schema_fingerprint = reader.array32()?;
    let plan_sha256 = reader.array32()?;
    let header_sha256 = reader.array32()?;
    let body_sha256 = reader.array32()?;
    let global_logical_sha256 = reader.array32()?;
    let schema = decode_schema_descriptor(reader.take(schema_len)?)?;
    if schema.schema_id != schema_id {
        return Err(AuraError::InvalidValue("v3 planned grouped schema id"));
    }
    let plan_bytes = reader.take(plan_len)?;
    let plan = AuraPlanV2::decode(&schema, plan_bytes)?;
    if reader.u16()? != CHUNK_TABLE_VERSION
        || reader.u16()? as usize != V3_PLANNED_GROUPED_CHUNK_DESCRIPTOR_BYTES
        || reader.u32()? as usize != chunk_count
        || chunk_count > reader.remaining() / V3_PLANNED_GROUPED_CHUNK_DESCRIPTOR_BYTES
    {
        return Err(AuraError::InvalidValue("v3 planned grouped chunk table"));
    }
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(chunk_count)
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    for _ in 0..chunk_count {
        let chunk_id = reader.u32()?;
        if reader.u32()? != 0 {
            return Err(AuraError::InvalidValue("v3 planned grouped chunk flags"));
        }
        let first_global_event = reader.u64()?;
        let event_count = reader.u32()?;
        if reader.u16()? != block_version || reader.u16()? != 0 {
            return Err(AuraError::InvalidValue("v3 planned grouped block version"));
        }
        let first_global_child = reader.u64()?;
        let child_count = reader.u32()?;
        if reader.u32()? != 0 {
            return Err(AuraError::InvalidValue("v3 planned grouped chunk reserved"));
        }
        chunks.push(V3PlannedGroupedChunkDescriptor {
            chunk_id,
            first_global_event,
            event_count,
            first_global_child,
            child_count,
            body_relative_offset: reader.u64()?,
            stored_len: reader.u64()?,
            stored_sha256: reader.array32()?,
            chunk_logical_sha256: reader.array32()?,
        });
    }
    if reader.remaining() != 0 {
        return Err(AuraError::TrailingBytes(reader.remaining()));
    }
    let footer = V3PlannedGroupedFooter {
        event_count,
        child_count,
        body_len,
        body_layout_version,
        block_version,
        schema,
        plan,
        schema_fingerprint,
        plan_sha256,
        header_sha256,
        body_sha256,
        global_logical_sha256,
        chunks,
    };
    validate_footer(&footer, limits)?;
    if encode_v3_planned_grouped_footer(&footer, limits)? != bytes {
        return Err(AuraError::InvalidValue(
            "v3 planned grouped noncanonical footer",
        ));
    }
    Ok(footer)
}

fn validate_footer(footer: &V3PlannedGroupedFooter, limits: V3GroupedLimits) -> Result<()> {
    let limits = limits.effective();
    crate::v3_events::validate_v3_grouped_exact_subset(&footer.schema)?;
    footer.plan.validate(&footer.schema)?;
    let version_contract = matches!(
        (
            footer.body_layout_version,
            footer.block_version,
            footer.plan.registry_version,
        ),
        (
            V3_PLANNED_GROUPED_BODY_LAYOUT_VERSION,
            V3_PLANNED_GROUPED_DIRECT_BLOCK_VERSION,
            crate::AURA_PLAN_V2_REGISTRY_VERSION,
        ) | (
            V3_PLANNED_GROUPED_COMPACT_BODY_LAYOUT_VERSION,
            V3_PLANNED_GROUPED_COMPACT_BLOCK_VERSION,
            crate::AURA_PLAN_V2_SPLIT_REGISTRY_VERSION,
        )
    );
    if !version_contract
        || footer.event_count > limits.max_events
        || footer.child_count > limits.max_children
        || footer.body_len > limits.max_body_bytes
        || footer.chunks.len() > limits.max_chunks
        || footer.schema_fingerprint != canonical_v3_schema_fingerprint(&footer.schema)?
        || footer.plan_sha256 != footer.plan.hash(&footer.schema)?
    {
        return Err(AuraError::InvalidValue(
            "v3 planned grouped footer metadata",
        ));
    }
    let mut event = 0u64;
    let mut child = 0u64;
    let mut offset = 0u64;
    for (index, chunk) in footer.chunks.iter().enumerate() {
        let event_count = usize::try_from(chunk.event_count)
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped event count"))?;
        let child_count = usize::try_from(chunk.child_count)
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped child count"))?;
        let event_fields = footer
            .schema
            .fields
            .iter()
            .filter(|field| field.scope == crate::FieldScope::Event)
            .count();
        let repeated_fields = footer.schema.fields.len().saturating_sub(event_fields);
        let logical_values = event_count
            .checked_mul(event_fields)
            .and_then(|value| {
                child_count
                    .checked_mul(repeated_fields)
                    .and_then(|children| value.checked_add(children))
            })
            .ok_or(AuraError::InvalidValue(
                "v3 planned grouped logical value count",
            ))?;
        if chunk.chunk_id as usize != index
            || chunk.event_count == 0
            || event_count > limits.event_limits.max_events
            || child_count > limits.event_limits.max_children
            || logical_values > limits.event_limits.max_values
            || chunk.stored_len == 0
            || chunk.stored_len > limits.event_limits.max_block_bytes as u64
            || chunk.first_global_event != event
            || chunk.first_global_child != child
            || chunk.body_relative_offset != offset
        {
            return Err(AuraError::InvalidValue("v3 planned grouped chunk ranges"));
        }
        event = event
            .checked_add(u64::from(chunk.event_count))
            .ok_or(AuraError::InvalidValue("v3 planned grouped event count"))?;
        child = child
            .checked_add(u64::from(chunk.child_count))
            .ok_or(AuraError::InvalidValue("v3 planned grouped child count"))?;
        offset = offset
            .checked_add(chunk.stored_len)
            .ok_or(AuraError::InvalidValue("v3 planned grouped body length"))?;
    }
    if event != footer.event_count || child != footer.child_count || offset != footer.body_len {
        return Err(AuraError::InvalidValue("v3 planned grouped totals"));
    }
    if footer.event_count == 0
        && (footer.child_count != 0 || footer.body_len != 0 || !footer.chunks.is_empty())
    {
        return Err(AuraError::InvalidValue("v3 planned grouped empty file"));
    }
    Ok(())
}

fn validate_header(
    header: &AuraHeader,
    footer: &V3PlannedGroupedFooter,
    encoded: &[u8],
) -> Result<()> {
    if header.container_version != AuraContainerVersion::V3
        || header.profile != Profile::Aura0
        || header.stream_id != 0
        || header.dictionary_id != 0
        || header.base_time_ns != 0
        || header.schema_mapping
            != footer
                .schema
                .compact_schema_map
                .as_deref()
                .unwrap_or_default()
        || header.groups != footer.schema.groups
        || !header.derived_expressions.is_empty()
        || !header.comment.is_empty()
        || canonical_header(&footer.schema)?.encode()? != encoded
    {
        return Err(AuraError::InvalidValue(
            "v3 planned grouped header/footer agreement",
        ));
    }
    Ok(())
}

fn canonical_header(schema: &SchemaDescriptor) -> Result<AuraHeader> {
    AuraHeader::new(Profile::Aura0)
        .with_container_version(AuraContainerVersion::V3)
        .with_schema_mapping(
            schema
                .compact_schema_map
                .clone()
                .ok_or(AuraError::InvalidValue("v3 planned grouped schema map"))?,
        )?
        .with_groups(schema.groups.clone())
}

fn validate_direct_batch_against_plan(plan: &AuraPlanV2, batch: &AuraV3EventBatch) -> Result<()> {
    let mut event = batch.event_columns.iter();
    let mut repeated = batch.repeated_columns.iter();
    for slot in &plan.decode_order {
        let descriptor = plan
            .streams
            .get(usize::from(*slot))
            .filter(|descriptor| descriptor.slot == *slot)
            .ok_or(AuraError::InvalidValue("v3 planned grouped decode stream"))?;
        let column = match descriptor.scope {
            crate::FieldScope::Event => event.next(),
            crate::FieldScope::Repeated => repeated.next(),
        }
        .filter(|column| {
            column.slot == descriptor.slot && column.values.field_type() == descriptor.field_type
        })
        .ok_or(AuraError::InvalidValue("v3 planned grouped decode stream"))?;
        if column.validity.is_some() != descriptor.nullable {
            return Err(AuraError::InvalidValue(
                "v3 planned grouped decode presence",
            ));
        }
    }
    if event.next().is_some() || repeated.next().is_some() {
        return Err(AuraError::InvalidValue("v3 planned grouped decode order"));
    }
    Ok(())
}

fn summary(
    footer: &V3PlannedGroupedFooter,
    header_bytes: usize,
    footer_bytes: usize,
    file_bytes: usize,
) -> Result<V3PlannedGroupedSummary> {
    let accounted = (header_bytes as u64)
        .checked_add(footer.body_len)
        .and_then(|value| value.checked_add(footer_bytes as u64))
        .and_then(|value| value.checked_add(TRAILER_BYTES as u64))
        .ok_or(AuraError::InvalidValue("v3 planned grouped file length"))?;
    if accounted != file_bytes as u64 {
        return Err(AuraError::InvalidValue(
            "v3 planned grouped byte accounting",
        ));
    }
    Ok(V3PlannedGroupedSummary {
        event_count: footer.event_count,
        child_count: footer.child_count,
        chunk_count: u32::try_from(footer.chunks.len())
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped chunk count"))?,
        header_bytes: header_bytes as u64,
        body_bytes: footer.body_len,
        footer_bytes: u32::try_from(footer_bytes)
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped footer length"))?,
        file_bytes: file_bytes as u64,
        accounted_file_bytes: accounted,
        schema_fingerprint: footer.schema_fingerprint,
        plan_sha256: footer.plan_sha256,
        global_logical_sha256: footer.global_logical_sha256,
    })
}

fn inspection(footer: &V3PlannedGroupedFooter) -> V3PlannedGroupedInspection {
    V3PlannedGroupedInspection {
        body_encoding: V3_PLANNED_GROUPED_BODY_ENCODING,
        body_layout_version: footer.body_layout_version,
        direct_block_version: footer.block_version,
        compression: "none",
        plan: footer.plan.inspection(),
        schema_relationships_authorized_only: !footer.plan.inspection().relationships_attempted,
        candidates: Vec::new(),
    }
}

fn domain_hash(domain: &[u8], bytes: &[u8], name: &'static str) -> Result<[u8; 32]> {
    let len = u64::try_from(bytes.len()).map_err(|_| AuraError::InvalidValue(name))?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(len.to_le_bytes());
    hasher.update(bytes);
    Ok(hasher.finalize().into())
}

fn put_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u32_len(bytes: &mut Vec<u8>, value: usize, name: &'static str) -> Result<()> {
    put_u32(
        bytes,
        u32::try_from(value).map_err(|_| AuraError::InvalidValue(name))?,
    );
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    const fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(AuraError::UnexpectedEof)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(AuraError::UnexpectedEof)?;
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn array32(&mut self) -> Result<[u8; 32]> {
        Ok(self.take(32)?.try_into().unwrap())
    }
}
