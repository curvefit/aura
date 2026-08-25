//! Reference planned-grouped Aura0 V3 container (layout 2, body encoding 3).
//!
//! Attempt 1 stores exact direct `AURAV3EB` chunks. Attempt 2 adds a distinct
//! plan-bound compact-stream block and scores registry-1 direct, compact
//! registry-2 direct, and schema-authorized compact split-domain direct by
//! actual complete-file bytes. Attempt 3 adds a distinct Direct-only integer
//! codec block and scores registry-1 Direct, registry-2 compact Direct,
//! registry-3 all-fixed, and registry-3 per-stream fixed/absolute-varint files.
//! Attempt 4 separately scores schema-authorized previous-within-domain math,
//! with event/domain resets and checked inverse, against all accepted absolute
//! fallbacks. Attempt 5 adds both schema-authorized cross-domain same-slot
//! orientations, pairing ordinal occurrences per event and retaining unmatched
//! tails as absolute values. Field roles such as price or quantity carry no
//! economic meaning. Cross-event state remains excluded. Nothing here
//! claims compression, Parquet comparison, holdout evidence, production
//! readiness, or the campaign size goal.

use sha2::{Digest, Sha256};

use crate::bytes::ByteReader;
use crate::format::{AuraContainerVersion, SEAL_MAGIC};
use crate::schema::{
    decode_schema_descriptor, encode_schema_descriptor, FieldScope, SchemaDescriptor,
};
use crate::v3_codecs::{
    decode_canonical_uleb128, decode_canonical_zigzag, fixed_width, integer_varint_codec,
    PlanV2PhysicalCodec,
};
use crate::v3_events::{
    canonical_v3_event_batch_sha256, decode_v3_event_block, encode_v3_event_block,
    validate_v3_event_batch, AuraV3EventBatch, CanonicalV3EventHasher, V3EventLimits,
    MAX_V3_EVENT_BLOCK_BYTES,
};
use crate::v3_grouped_container::V3GroupedLimits;
use crate::v3_plan_v2::{AuraPlanV2, PlanV2Inspection, PlanV2Selection, MAX_AURA_PLAN_V2_BYTES};
use crate::v3_plan_v2::{
    AURA_PLAN_V2_CROSS_DOMAIN_REGISTRY_VERSION, AURA_PLAN_V2_DOMAIN0_FROM_DOMAIN1_OP,
    AURA_PLAN_V2_DOMAIN1_FROM_DOMAIN0_OP, AURA_PLAN_V2_INTEGER_CODEC_REGISTRY_VERSION,
    AURA_PLAN_V2_PREVIOUS_WITHIN_DOMAIN_OP, AURA_PLAN_V2_WITHIN_DOMAIN_REGISTRY_VERSION,
};
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
pub const V3_PLANNED_GROUPED_INTEGER_CODEC_BODY_LAYOUT_VERSION: u16 = 3;
pub const V3_PLANNED_GROUPED_INTEGER_CODEC_BLOCK_VERSION: u16 = 3;
pub const V3_PLANNED_GROUPED_WITHIN_DOMAIN_BODY_LAYOUT_VERSION: u16 = 4;
pub const V3_PLANNED_GROUPED_WITHIN_DOMAIN_BLOCK_VERSION: u16 = 4;
pub const V3_PLANNED_GROUPED_CROSS_DOMAIN_BODY_LAYOUT_VERSION: u16 = 5;
pub const V3_PLANNED_GROUPED_CROSS_DOMAIN_BLOCK_VERSION: u16 = 5;
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
    pub codecs: Vec<V3PlannedGroupedCodecInspection>,
    pub within_domain: Vec<V3PlannedGroupedWithinInspection>,
    pub cross_domain: Vec<V3PlannedGroupedCrossInspection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedGroupedCrossInspection {
    pub logical_slot: u16,
    pub field_type: crate::FieldType,
    pub authorized: bool,
    pub eligible: bool,
    pub applicable: bool,
    pub rejection: Option<String>,
    pub absolute_bytes: u64,
    pub domain0_from_domain1_fixed_bytes: Option<u64>,
    pub domain0_from_domain1_varint_bytes: Option<u64>,
    pub domain1_from_domain0_fixed_bytes: Option<u64>,
    pub domain1_from_domain0_varint_bytes: Option<u64>,
    pub selected_op: u8,
    pub selected_codec: PlanV2PhysicalCodec,
    pub candidate_selected: bool,
    pub selected: bool,
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
pub struct V3PlannedGroupedCodecInspection {
    pub physical_stream_id: u16,
    pub logical_slot: u16,
    pub field_type: crate::FieldType,
    pub eligible: bool,
    pub fixed_bytes: u64,
    pub varint_bytes: Option<u64>,
    pub selected: PlanV2PhysicalCodec,
    pub rejection: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedGroupedWithinInspection {
    pub logical_slot: u16,
    pub field_type: crate::FieldType,
    pub authorized: bool,
    pub eligible: bool,
    pub applicable: bool,
    pub rejection: Option<String>,
    pub absolute_bytes: u64,
    pub transformed_fixed_bytes: Option<u64>,
    pub transformed_varint_bytes: Option<u64>,
    pub candidate_plan_bytes: u64,
    pub selected_plan_bytes: u64,
    pub selected_op: u8,
    pub selected_codec: PlanV2PhysicalCodec,
    pub candidate_selected: bool,
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

/// Size-attempt 3: select exact absolute integer codecs without relationship math.
/// Compact SplitDomainDirect is deliberately not a candidate: for absolute
/// fixed/varint lanes it preserves the same selector and value bytes, cannot
/// reduce summed validity bytes, adds one variable offset at a domain split,
/// and stamps extra dependency/physical-stream metadata. Its contribution is
/// therefore structurally non-negative relative to compact Direct and belongs
/// to the separately accounted relationship attempt.
pub fn compile_v3_planned_grouped_attempt3(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    limits: V3GroupedLimits,
) -> Result<V3PlannedGroupedArtifact> {
    validate_attempt3_inputs(schema, batches, limits)?;
    let fixed_plan = AuraPlanV2::integer_codec_direct_for_schema(schema);
    let mixed_plan_and_rows = fixed_plan
        .clone()
        .and_then(|plan| select_integer_codecs(schema, batches, plan));
    let mixed_rows = mixed_plan_and_rows
        .as_ref()
        .ok()
        .map(|(_, rows)| rows.clone());
    let mut fixed_rows = mixed_rows.clone().unwrap_or_default();
    for row in &mut fixed_rows {
        row.selected = PlanV2PhysicalCodec::FixedWidth;
        row.rejection = Some("registry 3 all-fixed candidate".to_owned());
    }
    let candidates = vec![
        compile_v3_planned_grouped(schema, batches, limits),
        compile_attempt2_candidate(schema, batches, limits, PlanV2Selection::Direct),
        fixed_plan
            .and_then(|plan| compile_compact_candidate_with_plan(schema, batches, limits, plan)),
        mixed_plan_and_rows.and_then(|(plan, _)| {
            compile_compact_candidate_with_plan(schema, batches, limits, plan)
        }),
    ];
    if let Some(error) = candidates
        .iter()
        .filter_map(|candidate| candidate.as_ref().err())
        .find(|error| !is_expected_candidate_limit(error))
    {
        return Err(error.clone());
    }
    let sizes = candidates
        .iter()
        .map(|candidate| {
            candidate
                .as_ref()
                .ok()
                .map(|value| value.summary.file_bytes)
        })
        .collect::<Vec<_>>();
    let selected_index = sizes
        .iter()
        .enumerate()
        .filter_map(|(index, bytes)| bytes.map(|bytes| (index, bytes)))
        .min_by_key(|(index, bytes)| (*bytes, *index))
        .map(|(index, _)| index)
        .ok_or(AuraError::InvalidValue(
            "v3 planned no applicable candidate",
        ))?;
    let ids = [
        "registry1-direct",
        "registry2-compact-direct",
        "registry3-compact-fixed",
        "registry3-compact-integer-codecs",
    ];
    let candidate_rows = ids
        .iter()
        .enumerate()
        .map(|(index, id)| V3PlannedGroupedCandidateInspection {
            candidate_id: (*id).to_owned(),
            selection: PlanV2Selection::Direct,
            authorized: true,
            applicable: candidates[index].is_ok(),
            rejection: match &candidates[index] {
                Err(error) => Some(sanitize_candidate_error(error)),
                Ok(_) if index != selected_index => {
                    Some("complete cost did not beat selected candidate".to_owned())
                }
                Ok(_) => None,
            },
            complete_bytes: sizes[index],
            selected: index == selected_index,
        })
        .collect::<Vec<_>>();
    let mut selected = candidates
        .into_iter()
        .nth(selected_index)
        .ok_or(AuraError::InvalidValue("v3 planned candidate count"))??;
    selected.inspection.candidates = candidate_rows;
    selected.inspection.codecs = match selected_index {
        2 => fixed_rows,
        3 => mixed_rows.unwrap_or_default(),
        _ => Vec::new(),
    };
    Ok(selected)
}

/// Size-attempt 4: score previous-within-domain-per-event relationship math
/// separately from the accepted absolute physical codecs.
/// This remains an all-memory reference compiler: complete candidate artifacts
/// are retained together for exact scoring, so peak memory is approximately
/// the sum of six candidate files plus transform scratch. Streaming/reread
/// planning is intentionally deferred rather than hidden by this attempt.
pub fn compile_v3_planned_grouped_attempt4(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    limits: V3GroupedLimits,
) -> Result<V3PlannedGroupedArtifact> {
    validate_attempt3_inputs(schema, batches, limits)?;
    let r3_fixed_plan = AuraPlanV2::integer_codec_direct_for_schema(schema);
    let r3_mixed = r3_fixed_plan
        .clone()
        .and_then(|plan| select_integer_codecs(schema, batches, plan));
    let r4_base = AuraPlanV2::within_domain_direct_for_schema(schema);
    let r4_absolute = r4_base.and_then(|plan| select_integer_codecs(schema, batches, plan));
    let r4_codec_rows = r4_absolute.as_ref().ok().map(|(_, rows)| rows.clone());
    let within_authorized = schema
        .groups
        .first()
        .is_some_and(|group| group.relationships.allows_within_domain());
    let r4_within = if within_authorized {
        r4_absolute
            .clone()
            .and_then(|(plan, _)| select_previous_within_domain(schema, batches, plan))
    } else {
        Err(AuraError::InvalidValue("v3 planned within unauthorized"))
    };
    let within_rows = r4_within.as_ref().ok().map(|(_, rows)| rows.clone());
    let candidates = vec![
        compile_v3_planned_grouped(schema, batches, limits),
        compile_attempt2_candidate(schema, batches, limits, PlanV2Selection::Direct),
        r3_fixed_plan
            .and_then(|plan| compile_compact_candidate_with_plan(schema, batches, limits, plan)),
        r3_mixed.and_then(|(plan, _)| {
            compile_compact_candidate_with_plan(schema, batches, limits, plan)
        }),
        r4_absolute.clone().and_then(|(plan, _)| {
            compile_compact_candidate_with_plan(schema, batches, limits, plan)
        }),
        r4_within.clone().and_then(|(plan, _)| {
            compile_compact_candidate_with_plan(schema, batches, limits, plan)
        }),
    ];
    if let Some(error) = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            (index != 5 || within_authorized)
                .then(|| candidate.as_ref().err())
                .flatten()
        })
        .find(|error| !is_expected_candidate_limit(error))
    {
        return Err(error.clone());
    }
    let sizes = candidates
        .iter()
        .map(|candidate| {
            candidate
                .as_ref()
                .ok()
                .map(|value| value.summary.file_bytes)
        })
        .collect::<Vec<_>>();
    let selected_index = sizes
        .iter()
        .enumerate()
        .filter_map(|(index, bytes)| bytes.map(|bytes| (index, bytes)))
        .min_by_key(|(index, bytes)| (*bytes, *index))
        .map(|(index, _)| index)
        .ok_or(AuraError::InvalidValue(
            "v3 planned no applicable candidate",
        ))?;
    let ids = [
        "registry1-direct",
        "registry2-compact-direct",
        "registry3-compact-fixed",
        "registry3-compact-integer-codecs",
        "registry4-absolute-integer-codecs",
        "registry4-previous-within-domain",
    ];
    let candidate_rows = ids
        .iter()
        .enumerate()
        .map(|(index, id)| V3PlannedGroupedCandidateInspection {
            candidate_id: (*id).to_owned(),
            selection: if index == 5 {
                PlanV2Selection::PreviousWithinDomainMixed
            } else {
                PlanV2Selection::Direct
            },
            authorized: index != 5 || within_authorized,
            applicable: candidates[index].is_ok() && (index != 5 || within_authorized),
            rejection: match &candidates[index] {
                _ if index == 5 && !within_authorized => {
                    Some("schema does not authorize within-domain".to_owned())
                }
                Err(error) => Some(sanitize_candidate_error(error)),
                Ok(_) if index != selected_index => {
                    Some("complete cost did not beat selected candidate".to_owned())
                }
                Ok(_) => None,
            },
            complete_bytes: if index == 5 && !within_authorized {
                None
            } else {
                sizes[index]
            },
            selected: index == selected_index,
        })
        .collect::<Vec<_>>();
    let mut selected = candidates.into_iter().nth(selected_index).unwrap()?;
    selected.inspection.candidates = candidate_rows;
    selected.inspection.codecs = if selected_index == 4 {
        r4_codec_rows.unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut relationship_rows = within_rows.unwrap_or_default();
    if selected_index == 5 {
        for row in &mut relationship_rows {
            row.selected = row.candidate_selected;
            row.selected_plan_bytes = u64::from(row.selected) * 2;
        }
    }
    selected.inspection.within_domain = relationship_rows;
    Ok(selected)
}

/// Encode an explicit registry-4 candidate for bounded inverse/wire auditing.
pub fn compile_v3_planned_grouped_attempt4_candidate(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    limits: V3GroupedLimits,
    slots: &[u16],
    codec: PlanV2PhysicalCodec,
) -> Result<V3PlannedGroupedArtifact> {
    let mut plan = AuraPlanV2::within_domain_direct_for_schema(schema)?;
    plan.select_previous_within_domain(slots);
    for slot in slots {
        let physical = plan.streams[usize::from(*slot)].physical_stream_ids[0];
        plan.physical_stream_codecs[usize::from(physical)] = codec;
    }
    plan.validate(schema)?;
    compile_compact_candidate_with_plan(schema, batches, limits, plan)
}

/// Size-attempt 5: score both reversible cross-domain same-slot orientations.
/// Pairing restarts at each event and uses ordinal occurrence within each
/// discriminator domain. The source domain and every unmatched tail value are
/// absolute, so asymmetric and empty domains require no side metadata.
pub fn compile_v3_planned_grouped_attempt5(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    limits: V3GroupedLimits,
) -> Result<V3PlannedGroupedArtifact> {
    validate_attempt3_inputs(schema, batches, limits)?;
    let r3_fixed_plan = AuraPlanV2::integer_codec_direct_for_schema(schema);
    let r3_mixed = r3_fixed_plan
        .clone()
        .and_then(|plan| select_integer_codecs(schema, batches, plan));
    let r4_absolute = AuraPlanV2::within_domain_direct_for_schema(schema)
        .and_then(|plan| select_integer_codecs(schema, batches, plan));
    let within_authorized = schema
        .groups
        .first()
        .is_some_and(|group| group.relationships.allows_within_domain());
    let r4_within = if within_authorized {
        r4_absolute
            .clone()
            .and_then(|(plan, _)| select_previous_within_domain(schema, batches, plan))
    } else {
        Err(AuraError::InvalidValue("v3 planned within unauthorized"))
    };
    let cross_authorized = schema
        .groups
        .first()
        .is_some_and(|group| group.relationships.allows_across_domain_same_field());
    let r5_cross = if cross_authorized {
        AuraPlanV2::cross_domain_direct_for_schema(schema)
            .and_then(|plan| select_integer_codecs(schema, batches, plan))
            .and_then(|(plan, _)| select_cross_domain_same_field(schema, batches, plan))
    } else {
        Err(AuraError::InvalidValue("v3 planned cross unauthorized"))
    };
    let cross_rows = r5_cross.as_ref().ok().map(|(_, rows)| rows.clone());
    // Compile and score one complete artifact at a time. The retained winner
    // is the only whole candidate kept in memory; ties remain with the earlier
    // Direct candidate by replacing only on a strict complete-byte win.
    let mut outcomes = Vec::new();
    outcomes
        .try_reserve_exact(7)
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    let mut winner = None;
    consider_attempt5_candidate(
        0,
        true,
        compile_v3_planned_grouped(schema, batches, limits),
        &mut outcomes,
        &mut winner,
    )?;
    consider_attempt5_candidate(
        1,
        true,
        compile_attempt2_candidate(schema, batches, limits, PlanV2Selection::Direct),
        &mut outcomes,
        &mut winner,
    )?;
    consider_attempt5_candidate(
        2,
        true,
        r3_fixed_plan
            .and_then(|plan| compile_compact_candidate_with_plan(schema, batches, limits, plan)),
        &mut outcomes,
        &mut winner,
    )?;
    consider_attempt5_candidate(
        3,
        true,
        r3_mixed.and_then(|(plan, _)| {
            compile_compact_candidate_with_plan(schema, batches, limits, plan)
        }),
        &mut outcomes,
        &mut winner,
    )?;
    consider_attempt5_candidate(
        4,
        true,
        r4_absolute.clone().and_then(|(plan, _)| {
            compile_compact_candidate_with_plan(schema, batches, limits, plan)
        }),
        &mut outcomes,
        &mut winner,
    )?;
    consider_attempt5_candidate(
        5,
        within_authorized,
        r4_within.and_then(|(plan, _)| {
            compile_compact_candidate_with_plan(schema, batches, limits, plan)
        }),
        &mut outcomes,
        &mut winner,
    )?;
    consider_attempt5_candidate(
        6,
        cross_authorized,
        r5_cross.clone().and_then(|(plan, _)| {
            compile_compact_candidate_with_plan(schema, batches, limits, plan)
        }),
        &mut outcomes,
        &mut winner,
    )?;
    let (selected_index, mut selected) = winner.ok_or(AuraError::InvalidValue(
        "v3 planned no applicable candidate",
    ))?;
    let ids = [
        "registry1-direct",
        "registry2-compact-direct",
        "registry3-compact-fixed",
        "registry3-compact-integer-codecs",
        "registry4-absolute-integer-codecs",
        "registry4-previous-within-domain",
        "registry5-cross-domain-same-field",
    ];
    let candidate_rows = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let (selection, authorized, unauthorized_reason) = match index {
                5 => (
                    PlanV2Selection::PreviousWithinDomainMixed,
                    within_authorized,
                    "schema does not authorize within-domain",
                ),
                6 => (
                    PlanV2Selection::CrossDomainSameFieldMixed,
                    cross_authorized,
                    "schema does not authorize cross-domain same-field",
                ),
                _ => (PlanV2Selection::Direct, true, ""),
            };
            V3PlannedGroupedCandidateInspection {
                candidate_id: (*id).to_owned(),
                selection,
                authorized,
                applicable: authorized && outcomes[index].is_ok(),
                rejection: if !authorized {
                    Some(unauthorized_reason.to_owned())
                } else {
                    match &outcomes[index] {
                        Err(error) => Some(sanitize_candidate_error(error)),
                        Ok(_) if index != selected_index => {
                            Some("complete cost did not beat selected candidate".to_owned())
                        }
                        Ok(_) => None,
                    }
                },
                complete_bytes: authorized
                    .then(|| outcomes[index].as_ref().ok().copied())
                    .flatten(),
                selected: index == selected_index,
            }
        })
        .collect::<Vec<_>>();
    selected.inspection.candidates = candidate_rows;
    let mut rows = cross_rows.unwrap_or_default();
    if selected_index == 6 {
        for row in &mut rows {
            row.selected = row.candidate_selected;
        }
    }
    selected.inspection.cross_domain = rows;
    Ok(selected)
}

/// Encode an explicit registry-5 candidate for inverse and corruption tests.
pub fn compile_v3_planned_grouped_attempt5_candidate(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    limits: V3GroupedLimits,
    ops: &[(u16, u8)],
    codec: PlanV2PhysicalCodec,
) -> Result<V3PlannedGroupedArtifact> {
    let mut plan = AuraPlanV2::cross_domain_direct_for_schema(schema)?;
    plan.select_cross_domain_same_field(ops);
    for (slot, _) in ops {
        let physical = plan
            .streams
            .get(usize::from(*slot))
            .and_then(|stream| stream.physical_stream_ids.first())
            .copied()
            .ok_or(AuraError::InvalidValue("v3 planned cross slot"))?;
        plan.physical_stream_codecs[usize::from(physical)] = codec;
    }
    plan.validate(schema)?;
    compile_compact_candidate_with_plan(schema, batches, limits, plan)
}

fn select_cross_domain_same_field(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    mut plan: AuraPlanV2,
) -> Result<(AuraPlanV2, Vec<V3PlannedGroupedCrossInspection>)> {
    let authorized = schema
        .groups
        .first()
        .is_some_and(|group| group.relationships.allows_across_domain_same_field());
    let mut selected_ops = Vec::new();
    let mut rows = Vec::new();
    for field in &schema.fields {
        if field.scope != FieldScope::Repeated || field.index == plan.discriminator_slot {
            continue;
        }
        let physical = plan.streams[usize::from(field.index)].physical_stream_ids[0];
        let absolute_codec = plan.physical_stream_codecs[usize::from(physical)];
        let absolute_bytes = encoded_codec_bytes(schema, batches, field.index, absolute_codec)?;
        let eligible = authorized && !field.nullable && is_within_field_type(field.field_type);
        let orientation = |op| {
            if eligible {
                encoded_cross_domain_bytes(schema, batches, field.index, op)
            } else {
                Err(AuraError::InvalidValue("v3 planned cross ineligible"))
            }
        };
        let zero = orientation(AURA_PLAN_V2_DOMAIN0_FROM_DOMAIN1_OP);
        let one = orientation(AURA_PLAN_V2_DOMAIN1_FROM_DOMAIN0_OP);
        for result in [&zero, &one] {
            if let Err(error) = result {
                let expected = !eligible
                    || matches!(error, AuraError::InvalidValue("v3 planned cross overflow"));
                if !expected {
                    return Err(error.clone());
                }
            }
        }
        let score = |columns: &Vec<(AuraV3Column, usize)>,
                     op|
         -> Result<(usize, usize, usize, u8, PlanV2PhysicalCodec)> {
            let fixed = encoded_columns_bytes(columns, PlanV2PhysicalCodec::FixedWidth)?;
            let varint = encoded_columns_bytes(columns, PlanV2PhysicalCodec::SignedZigZagUleb128)?;
            let (bytes, codec) = if varint < fixed {
                (varint, PlanV2PhysicalCodec::SignedZigZagUleb128)
            } else {
                (fixed, PlanV2PhysicalCodec::FixedWidth)
            };
            Ok((bytes, fixed, varint, op, codec))
        };
        let zero_score = zero
            .as_ref()
            .ok()
            .map(|columns| score(columns, AURA_PLAN_V2_DOMAIN0_FROM_DOMAIN1_OP))
            .transpose()?;
        let one_score = one
            .as_ref()
            .ok()
            .map(|columns| score(columns, AURA_PLAN_V2_DOMAIN1_FROM_DOMAIN0_OP))
            .transpose()?;
        let best = [zero_score, one_score]
            .into_iter()
            .flatten()
            .min_by_key(|(bytes, _, _, op, _)| (*bytes, *op));
        let select = best.is_some_and(|(bytes, _, _, _, _)| {
            bytes
                .checked_add(2)
                .is_some_and(|cost| cost < absolute_bytes)
        });
        let (selected_op, selected_codec) = if select {
            let (_, _, _, op, codec) = best.unwrap();
            selected_ops.push((field.index, op));
            plan.physical_stream_codecs[usize::from(physical)] = codec;
            (op, codec)
        } else {
            (crate::AURA_PLAN_V2_DIRECT_OP, absolute_codec)
        };
        let overflow = matches!(
            zero,
            Err(AuraError::InvalidValue("v3 planned cross overflow"))
        ) || matches!(
            one,
            Err(AuraError::InvalidValue("v3 planned cross overflow"))
        );
        let rejection = if select {
            None
        } else if !authorized {
            Some("schema does not authorize cross-domain same-field".to_owned())
        } else if field.nullable {
            Some("nullable fields remain absolute".to_owned())
        } else if !is_within_field_type(field.field_type) {
            Some("logical type is ineligible".to_owned())
        } else if overflow {
            Some("signed residual overflow".to_owned())
        } else {
            Some("relationship did not beat absolute codec".to_owned())
        };
        rows.push(V3PlannedGroupedCrossInspection {
            logical_slot: field.index,
            field_type: field.field_type,
            authorized,
            eligible,
            applicable: best.is_some(),
            rejection,
            absolute_bytes: absolute_bytes as u64,
            domain0_from_domain1_fixed_bytes: zero_score.map(|(_, fixed, _, _, _)| fixed as u64),
            domain0_from_domain1_varint_bytes: zero_score.map(|(_, _, varint, _, _)| varint as u64),
            domain1_from_domain0_fixed_bytes: one_score.map(|(_, fixed, _, _, _)| fixed as u64),
            domain1_from_domain0_varint_bytes: one_score.map(|(_, _, varint, _, _)| varint as u64),
            selected_op,
            selected_codec,
            candidate_selected: select,
            selected: false,
        });
    }
    plan.select_cross_domain_same_field(&selected_ops);
    plan.validate(schema)?;
    Ok((plan, rows))
}

fn select_previous_within_domain(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    mut plan: AuraPlanV2,
) -> Result<(AuraPlanV2, Vec<V3PlannedGroupedWithinInspection>)> {
    let authorized = schema
        .groups
        .first()
        .is_some_and(|group| group.relationships.allows_within_domain());
    let mut selected_slots = Vec::new();
    let mut rows = Vec::new();
    for field in &schema.fields {
        if field.scope != FieldScope::Repeated || field.index == plan.discriminator_slot {
            continue;
        }
        let physical = plan.streams[usize::from(field.index)].physical_stream_ids[0];
        let absolute_codec = plan.physical_stream_codecs[usize::from(physical)];
        let absolute_bytes = encoded_codec_bytes(schema, batches, field.index, absolute_codec)?;
        let eligible = authorized && !field.nullable && is_within_field_type(field.field_type);
        let transformed = if eligible {
            encoded_previous_within_bytes(schema, batches, field.index)
        } else {
            Err(AuraError::InvalidValue("v3 planned within ineligible"))
        };
        let (fixed_bytes, varint_bytes, selected_codec, rejection, select) = match transformed {
            Ok(columns) => {
                let fixed = encoded_columns_bytes(&columns, PlanV2PhysicalCodec::FixedWidth)?;
                let varint =
                    encoded_columns_bytes(&columns, PlanV2PhysicalCodec::SignedZigZagUleb128)?;
                let (codec, body) = if varint < fixed {
                    (PlanV2PhysicalCodec::SignedZigZagUleb128, varint)
                } else {
                    (PlanV2PhysicalCodec::FixedWidth, fixed)
                };
                let select = body
                    .checked_add(2)
                    .is_some_and(|cost| cost < absolute_bytes);
                (
                    Some(fixed as u64),
                    Some(varint as u64),
                    if select { codec } else { absolute_codec },
                    (!select).then(|| "relationship did not beat absolute codec".to_owned()),
                    select,
                )
            }
            Err(AuraError::InvalidValue("v3 planned within overflow")) => (
                None,
                None,
                absolute_codec,
                Some("signed residual overflow".to_owned()),
                false,
            ),
            Err(_) if !eligible => (
                None,
                None,
                absolute_codec,
                Some(if !authorized {
                    "schema does not authorize within-domain".to_owned()
                } else if field.nullable {
                    "nullable fields remain absolute".to_owned()
                } else {
                    "logical type is ineligible".to_owned()
                }),
                false,
            ),
            Err(error) => return Err(error),
        };
        if select {
            selected_slots.push(field.index);
            plan.physical_stream_codecs[usize::from(physical)] = selected_codec;
        }
        rows.push(V3PlannedGroupedWithinInspection {
            logical_slot: field.index,
            field_type: field.field_type,
            authorized,
            eligible,
            applicable: fixed_bytes.is_some(),
            rejection,
            absolute_bytes: absolute_bytes as u64,
            transformed_fixed_bytes: fixed_bytes,
            transformed_varint_bytes: varint_bytes,
            candidate_plan_bytes: u64::from(eligible && fixed_bytes.is_some()) * 2,
            selected_plan_bytes: 0,
            selected_op: if select {
                AURA_PLAN_V2_PREVIOUS_WITHIN_DOMAIN_OP
            } else {
                crate::AURA_PLAN_V2_DIRECT_OP
            },
            selected_codec,
            candidate_selected: select,
            selected: false,
        });
    }
    plan.select_previous_within_domain(&selected_slots);
    plan.validate(schema)?;
    Ok((plan, rows))
}

fn validate_attempt3_inputs(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    limits: V3GroupedLimits,
) -> Result<()> {
    let limits = limits.effective();
    crate::v3_events::validate_v3_grouped_exact_subset(schema)?;
    if batches.len() > limits.max_chunks {
        return Err(AuraError::InvalidValue("v3 planned grouped chunk count"));
    }
    let mut events = 0u64;
    let mut children = 0u64;
    for batch in batches {
        validate_v3_event_batch(
            schema,
            batch,
            planned_structural_limits(limits.event_limits),
        )?;
        if batch.event_count == 0 {
            return Err(AuraError::InvalidValue("v3 planned grouped empty chunk"));
        }
        events = events
            .checked_add(u64::from(batch.event_count))
            .filter(|value| *value <= limits.max_events)
            .ok_or(AuraError::InvalidValue("v3 planned grouped event count"))?;
        children = children
            .checked_add(u64::from(batch.child_count()))
            .filter(|value| *value <= limits.max_children)
            .ok_or(AuraError::InvalidValue("v3 planned grouped child count"))?;
    }
    Ok(())
}

/// Planned compact candidates use the caller's logical/value ceilings, while
/// `max_block_bytes` applies to the actual candidate block rather than the
/// larger canonical `AURAV3EB` reference representation used for structural
/// validation and canonical hashing.
const fn planned_structural_limits(limits: V3EventLimits) -> V3EventLimits {
    V3EventLimits {
        max_block_bytes: MAX_V3_EVENT_BLOCK_BYTES,
        max_variable_value_bytes: limits.max_variable_value_bytes,
        max_events: limits.max_events,
        max_children: limits.max_children,
        max_values: limits.max_values,
    }
}

fn sanitize_candidate_error(error: &AuraError) -> String {
    error
        .to_string()
        .chars()
        .take(160)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn is_expected_candidate_limit(error: &AuraError) -> bool {
    matches!(
        error,
        AuraError::InvalidValue(
            "v3 event block length"
                | "v3 planned block length"
                | "v3 planned grouped body length"
                | "v3 planned grouped footer length"
        )
    )
}

fn consider_attempt5_candidate(
    index: usize,
    authorized: bool,
    candidate: Result<V3PlannedGroupedArtifact>,
    outcomes: &mut Vec<Result<u64>>,
    winner: &mut Option<(usize, V3PlannedGroupedArtifact)>,
) -> Result<()> {
    if !authorized {
        outcomes.push(Err(AuraError::InvalidValue(
            "v3 planned candidate unauthorized",
        )));
        return Ok(());
    }
    match candidate {
        Ok(artifact) => {
            let bytes = artifact.summary.file_bytes;
            let replace = winner
                .as_ref()
                .is_none_or(|(_, current)| bytes < current.summary.file_bytes);
            if replace {
                *winner = Some((index, artifact));
            }
            outcomes.push(Ok(bytes));
            Ok(())
        }
        Err(error) if is_expected_candidate_limit(&error) => {
            outcomes.push(Err(error));
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn select_integer_codecs(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    mut plan: AuraPlanV2,
) -> Result<(AuraPlanV2, Vec<V3PlannedGroupedCodecInspection>)> {
    let mut rows = Vec::new();
    for field in &schema.fields {
        if field.index == plan.discriminator_slot {
            continue;
        }
        let physical = plan.streams[usize::from(field.index)].physical_stream_ids[0];
        let fixed_bytes = encoded_codec_bytes(
            schema,
            batches,
            field.index,
            PlanV2PhysicalCodec::FixedWidth,
        )?;
        let varint_codec = integer_varint_codec(field.field_type);
        let varint_bytes = varint_codec
            .map(|codec| encoded_codec_bytes(schema, batches, field.index, codec))
            .transpose()?;
        let selected_codec = match (varint_codec, varint_bytes) {
            (Some(codec), Some(bytes)) if bytes < fixed_bytes => codec,
            _ => PlanV2PhysicalCodec::FixedWidth,
        };
        plan.physical_stream_codecs[usize::from(physical)] = selected_codec;
        rows.push(V3PlannedGroupedCodecInspection {
            physical_stream_id: physical,
            logical_slot: field.index,
            field_type: field.field_type,
            eligible: varint_codec.is_some(),
            fixed_bytes: fixed_bytes as u64,
            varint_bytes: varint_bytes.map(|value| value as u64),
            selected: selected_codec,
            rejection: match (varint_codec, varint_bytes) {
                (None, _) => Some("type is fixed-only in registry 3".to_owned()),
                (Some(_), Some(bytes)) if bytes >= fixed_bytes => {
                    Some("varint did not beat fixed width".to_owned())
                }
                _ => None,
            },
        });
    }
    plan.validate(schema)?;
    Ok((plan, rows))
}

fn encoded_codec_bytes(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    slot: u16,
    codec: PlanV2PhysicalCodec,
) -> Result<usize> {
    let field = schema
        .fields
        .get(usize::from(slot))
        .filter(|field| field.index == slot)
        .ok_or(AuraError::InvalidValue("v3 planned codec slot"))?;
    let scoped_position = schema
        .fields
        .iter()
        .filter(|candidate| candidate.scope == field.scope)
        .position(|candidate| candidate.index == slot)
        .ok_or(AuraError::InvalidValue("v3 planned codec slot"))?;
    let mut total = 0usize;
    for batch in batches {
        let (column, rows) = match field.scope {
            FieldScope::Event => (
                &batch.event_columns[scoped_position],
                batch.event_count as usize,
            ),
            FieldScope::Repeated => (
                &batch.repeated_columns[scoped_position],
                batch.child_count() as usize,
            ),
        };
        let mut encoded = Vec::new();
        encode_codec_lane(column, rows, codec, &mut encoded)?;
        total = total
            .checked_add(encoded.len())
            .ok_or(AuraError::InvalidValue("v3 planned codec length"))?;
    }
    Ok(total)
}

fn encoded_previous_within_bytes(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    slot: u16,
) -> Result<Vec<(AuraV3Column, usize)>> {
    let repeated = schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Repeated)
        .collect::<Vec<_>>();
    let position = repeated
        .iter()
        .position(|field| field.index == slot)
        .ok_or(AuraError::InvalidValue("v3 planned within slot"))?;
    let discriminator = schema.groups[0].dual_domain.unwrap().discriminator_slot;
    let discriminator_position = repeated
        .iter()
        .position(|field| field.index == discriminator)
        .ok_or(AuraError::InvalidValue("v3 planned within discriminator"))?;
    let mut columns = Vec::new();
    columns
        .try_reserve_exact(batches.len())
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    for batch in batches {
        let selector = match &batch.repeated_columns[discriminator_position].values {
            AuraV3ColumnValues::U8(values) => values,
            _ => return Err(AuraError::InvalidValue("v3 planned within discriminator")),
        };
        columns.push((
            previous_within_domain_values(batch, &batch.repeated_columns[position], selector)?,
            batch.child_count() as usize,
        ));
    }
    Ok(columns)
}

fn encoded_cross_domain_bytes(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    slot: u16,
    op: u8,
) -> Result<Vec<(AuraV3Column, usize)>> {
    let repeated = schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Repeated)
        .collect::<Vec<_>>();
    let position = repeated
        .iter()
        .position(|field| field.index == slot)
        .ok_or(AuraError::InvalidValue("v3 planned cross slot"))?;
    let discriminator = schema.groups[0].dual_domain.unwrap().discriminator_slot;
    let discriminator_position = repeated
        .iter()
        .position(|field| field.index == discriminator)
        .ok_or(AuraError::InvalidValue("v3 planned cross discriminator"))?;
    let mut columns = Vec::new();
    columns
        .try_reserve_exact(batches.len())
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    for batch in batches {
        let selector = match &batch.repeated_columns[discriminator_position].values {
            AuraV3ColumnValues::U8(values) => values,
            _ => return Err(AuraError::InvalidValue("v3 planned cross discriminator")),
        };
        columns.push((
            cross_domain_same_field_values(batch, &batch.repeated_columns[position], selector, op)?,
            batch.child_count() as usize,
        ));
    }
    Ok(columns)
}

fn encoded_columns_bytes(
    columns: &[(AuraV3Column, usize)],
    codec: PlanV2PhysicalCodec,
) -> Result<usize> {
    let mut total = 0usize;
    for (column, rows) in columns {
        let mut encoded = Vec::new();
        encode_codec_lane(column, *rows, codec, &mut encoded)?;
        total = total
            .checked_add(encoded.len())
            .ok_or(AuraError::InvalidValue("v3 planned within length"))?;
    }
    Ok(total)
}

const fn is_within_field_type(field_type: crate::FieldType) -> bool {
    matches!(
        field_type,
        crate::FieldType::I8
            | crate::FieldType::I16
            | crate::FieldType::I32
            | crate::FieldType::I64
            | crate::FieldType::TimestampNs
    )
}

fn compile_attempt2_candidate(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    limits: V3GroupedLimits,
    selection: PlanV2Selection,
) -> Result<V3PlannedGroupedArtifact> {
    let plan = AuraPlanV2::candidate_for_schema(schema, selection)?;
    compile_compact_candidate_with_plan(schema, batches, limits, plan)
}

fn compile_compact_candidate_with_plan(
    schema: &SchemaDescriptor,
    batches: &[AuraV3EventBatch],
    limits: V3GroupedLimits,
    plan: AuraPlanV2,
) -> Result<V3PlannedGroupedArtifact> {
    let limits = limits.effective();
    let structural_limits = planned_structural_limits(limits.event_limits);
    crate::v3_events::validate_v3_grouped_exact_subset(schema)?;
    if batches.len() > limits.max_chunks {
        return Err(AuraError::InvalidValue("v3 planned grouped chunk count"));
    }
    let mut event_count = 0u64;
    let mut child_count = 0u64;
    for batch in batches {
        validate_v3_event_batch(schema, batch, structural_limits)?;
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
        structural_limits,
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
                structural_limits,
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
        body_layout_version: body_versions(&plan).0,
        block_version: body_versions(&plan).1,
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

const fn body_versions(plan: &AuraPlanV2) -> (u16, u16) {
    match plan.registry_version {
        AURA_PLAN_V2_CROSS_DOMAIN_REGISTRY_VERSION => (
            V3_PLANNED_GROUPED_CROSS_DOMAIN_BODY_LAYOUT_VERSION,
            V3_PLANNED_GROUPED_CROSS_DOMAIN_BLOCK_VERSION,
        ),
        AURA_PLAN_V2_WITHIN_DOMAIN_REGISTRY_VERSION => (
            V3_PLANNED_GROUPED_WITHIN_DOMAIN_BODY_LAYOUT_VERSION,
            V3_PLANNED_GROUPED_WITHIN_DOMAIN_BLOCK_VERSION,
        ),
        AURA_PLAN_V2_INTEGER_CODEC_REGISTRY_VERSION => (
            V3_PLANNED_GROUPED_INTEGER_CODEC_BODY_LAYOUT_VERSION,
            V3_PLANNED_GROUPED_INTEGER_CODEC_BLOCK_VERSION,
        ),
        _ => (
            V3_PLANNED_GROUPED_COMPACT_BODY_LAYOUT_VERSION,
            V3_PLANNED_GROUPED_COMPACT_BLOCK_VERSION,
        ),
    }
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
    validate_v3_event_batch(schema, batch, planned_structural_limits(limits))?;
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
    let block_version = body_versions(plan).1;
    put_u16(&mut out, block_version);
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
        encode_codec_lane(
            column,
            batch.event_count as usize,
            codec_for_column(plan, column.slot)?,
            &mut out,
        )?;
    }
    out.extend_from_slice(selector);
    for (field, column) in repeated_fields.iter().zip(&batch.repeated_columns) {
        if field.index == plan.discriminator_slot {
            continue;
        }
        let descriptor = &plan.streams[usize::from(field.index)];
        if descriptor.op == AURA_PLAN_V2_PREVIOUS_WITHIN_DOMAIN_OP {
            let transformed = previous_within_domain_values(batch, column, selector)?;
            encode_codec_lane(
                &transformed,
                batch.child_count() as usize,
                codec_for_column(plan, column.slot)?,
                &mut out,
            )?;
        } else if matches!(
            descriptor.op,
            AURA_PLAN_V2_DOMAIN0_FROM_DOMAIN1_OP | AURA_PLAN_V2_DOMAIN1_FROM_DOMAIN0_OP
        ) {
            let transformed =
                cross_domain_same_field_values(batch, column, selector, descriptor.op)?;
            encode_codec_lane(
                &transformed,
                batch.child_count() as usize,
                codec_for_column(plan, column.slot)?,
                &mut out,
            )?;
        } else if plan.selection != PlanV2Selection::SplitDomainDirect {
            encode_codec_lane(
                column,
                batch.child_count() as usize,
                codec_for_column(plan, column.slot)?,
                &mut out,
            )?;
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
        || reader.read_u16_le()? != body_versions(plan).1
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
        || reader.read_u64_le()?
            != u64::try_from(bytes.len())
                .map_err(|_| AuraError::InvalidValue("v3 planned block length"))?
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
        event_columns.push(decode_codec_lane(
            field.index,
            field.field_type,
            field.nullable,
            event_count as usize,
            codec_for_column(plan, field.index)?,
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
        } else if plan.streams[usize::from(field.index)].op
            == AURA_PLAN_V2_PREVIOUS_WITHIN_DOMAIN_OP
        {
            let physical = decode_codec_lane(
                field.index,
                crate::FieldType::I64,
                false,
                child_count as usize,
                codec_for_column(plan, field.index)?,
                &mut reader,
                value_limits,
            )?;
            repeated_columns.push(inverse_previous_within_domain(
                field,
                &physical,
                &child_offsets,
                &selector,
            )?);
        } else if matches!(
            plan.streams[usize::from(field.index)].op,
            AURA_PLAN_V2_DOMAIN0_FROM_DOMAIN1_OP | AURA_PLAN_V2_DOMAIN1_FROM_DOMAIN0_OP
        ) {
            let op = plan.streams[usize::from(field.index)].op;
            let physical = decode_codec_lane(
                field.index,
                crate::FieldType::I64,
                false,
                child_count as usize,
                codec_for_column(plan, field.index)?,
                &mut reader,
                value_limits,
            )?;
            repeated_columns.push(inverse_cross_domain_same_field(
                field,
                &physical,
                &child_offsets,
                &selector,
                op,
            )?);
        } else if plan.selection != PlanV2Selection::SplitDomainDirect {
            let column = decode_codec_lane(
                field.index,
                field.field_type,
                field.nullable,
                child_count as usize,
                codec_for_column(plan, field.index)?,
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
    validate_v3_event_batch(schema, &batch, planned_structural_limits(limits))?;
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

fn previous_within_domain_values(
    batch: &AuraV3EventBatch,
    column: &AuraV3Column,
    selector: &[u8],
) -> Result<AuraV3Column> {
    let logical = signed_values_as_i64(&column.values)?;
    let mut transformed = Vec::new();
    transformed
        .try_reserve_exact(logical.len())
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    for event in 0..batch.event_count as usize {
        let mut previous = [None, None];
        let start = batch.child_offsets[event] as usize;
        let end = batch.child_offsets[event + 1] as usize;
        for child in start..end {
            let domain = usize::from(selector[child]);
            let value = logical[child];
            let stored = if let Some(base) = previous[domain] {
                i64::try_from(i128::from(value) - i128::from(base))
                    .map_err(|_| AuraError::InvalidValue("v3 planned within overflow"))?
            } else {
                value
            };
            transformed.push(stored);
            previous[domain] = Some(value);
        }
    }
    Ok(AuraV3Column {
        slot: column.slot,
        validity: None,
        values: AuraV3ColumnValues::I64(transformed),
    })
}

fn cross_domain_same_field_values(
    batch: &AuraV3EventBatch,
    column: &AuraV3Column,
    selector: &[u8],
    op: u8,
) -> Result<AuraV3Column> {
    let target_domain = cross_target_domain(op)?;
    let source_domain = 1usize - target_domain;
    let logical = signed_values_as_i64(&column.values)?;
    let mut transformed = logical.clone();
    for offsets in batch.child_offsets.windows(2) {
        let start = offsets[0] as usize;
        let end = offsets[1] as usize;
        let mut source_positions = Vec::new();
        source_positions
            .try_reserve_exact(end - start)
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
        for (relative, side) in selector[start..end].iter().copied().enumerate() {
            let child = start + relative;
            if usize::from(side) == source_domain {
                source_positions.push(child);
            }
        }
        let mut target_ordinal = 0usize;
        for (relative, side) in selector[start..end].iter().copied().enumerate() {
            let target = start + relative;
            if usize::from(side) == target_domain {
                if let Some(source) = source_positions.get(target_ordinal).copied() {
                    transformed[target] =
                        i64::try_from(i128::from(logical[target]) - i128::from(logical[source]))
                            .map_err(|_| AuraError::InvalidValue("v3 planned cross overflow"))?;
                }
                target_ordinal += 1;
            }
        }
    }
    Ok(AuraV3Column {
        slot: column.slot,
        validity: None,
        values: AuraV3ColumnValues::I64(transformed),
    })
}

fn inverse_cross_domain_same_field(
    field: &crate::FieldDescriptor,
    physical: &AuraV3Column,
    child_offsets: &[u32],
    selector: &[u8],
    op: u8,
) -> Result<AuraV3Column> {
    let AuraV3ColumnValues::I64(stored) = &physical.values else {
        return Err(AuraError::InvalidValue("v3 planned cross physical type"));
    };
    let target_domain = cross_target_domain(op)?;
    let source_domain = 1usize - target_domain;
    let mut logical = stored.clone();
    for offsets in child_offsets.windows(2) {
        let start = offsets[0] as usize;
        let end = offsets[1] as usize;
        let mut source_positions = Vec::new();
        source_positions
            .try_reserve_exact(end - start)
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
        for (relative, side) in selector[start..end].iter().copied().enumerate() {
            let child = start + relative;
            if usize::from(side) == source_domain {
                source_positions.push(child);
            }
        }
        let mut target_ordinal = 0usize;
        for (relative, side) in selector[start..end].iter().copied().enumerate() {
            let target = start + relative;
            if usize::from(side) == target_domain {
                if let Some(source) = source_positions.get(target_ordinal).copied() {
                    logical[target] =
                        i64::try_from(i128::from(stored[source]) + i128::from(stored[target]))
                            .map_err(|_| AuraError::InvalidValue("v3 planned cross inverse"))?;
                }
                target_ordinal += 1;
            }
        }
    }
    Ok(AuraV3Column {
        slot: field.index,
        validity: None,
        values: signed_i64_to_values(field.field_type, logical)?,
    })
}

fn cross_target_domain(op: u8) -> Result<usize> {
    match op {
        AURA_PLAN_V2_DOMAIN0_FROM_DOMAIN1_OP => Ok(0),
        AURA_PLAN_V2_DOMAIN1_FROM_DOMAIN0_OP => Ok(1),
        _ => Err(AuraError::InvalidValue("v3 planned cross op")),
    }
}

fn inverse_previous_within_domain(
    field: &crate::FieldDescriptor,
    physical: &AuraV3Column,
    child_offsets: &[u32],
    selector: &[u8],
) -> Result<AuraV3Column> {
    let AuraV3ColumnValues::I64(stored) = &physical.values else {
        return Err(AuraError::InvalidValue("v3 planned within physical type"));
    };
    let mut logical = Vec::new();
    logical
        .try_reserve_exact(stored.len())
        .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
    for offsets in child_offsets.windows(2) {
        let mut previous = [None, None];
        for child in offsets[0] as usize..offsets[1] as usize {
            let domain = usize::from(selector[child]);
            let value = if let Some(base) = previous[domain] {
                i64::try_from(i128::from(base) + i128::from(stored[child]))
                    .map_err(|_| AuraError::InvalidValue("v3 planned within inverse"))?
            } else {
                stored[child]
            };
            logical.push(value);
            previous[domain] = Some(value);
        }
    }
    Ok(AuraV3Column {
        slot: field.index,
        validity: None,
        values: signed_i64_to_values(field.field_type, logical)?,
    })
}

fn signed_values_as_i64(values: &AuraV3ColumnValues) -> Result<Vec<i64>> {
    Ok(match values {
        AuraV3ColumnValues::I8(values) => values.iter().map(|value| i64::from(*value)).collect(),
        AuraV3ColumnValues::I16(values) => values.iter().map(|value| i64::from(*value)).collect(),
        AuraV3ColumnValues::I32(values) => values.iter().map(|value| i64::from(*value)).collect(),
        AuraV3ColumnValues::I64(values)
        | AuraV3ColumnValues::TimestampNs(values)
        | AuraV3ColumnValues::TimestampMs(values) => values.clone(),
        _ => return Err(AuraError::InvalidValue("v3 planned within logical type")),
    })
}

fn signed_i64_to_values(
    field_type: crate::FieldType,
    values: Vec<i64>,
) -> Result<AuraV3ColumnValues> {
    macro_rules! narrow {
        ($variant:ident, $type:ty) => {
            AuraV3ColumnValues::$variant(
                values
                    .into_iter()
                    .map(|value| {
                        <$type>::try_from(value)
                            .map_err(|_| AuraError::InvalidValue("v3 planned within inverse"))
                    })
                    .collect::<Result<Vec<_>>>()?,
            )
        };
    }
    Ok(match field_type {
        crate::FieldType::I8 => narrow!(I8, i8),
        crate::FieldType::I16 => narrow!(I16, i16),
        crate::FieldType::I32 => narrow!(I32, i32),
        crate::FieldType::I64 => AuraV3ColumnValues::I64(values),
        crate::FieldType::TimestampNs => AuraV3ColumnValues::TimestampNs(values),
        crate::FieldType::TimestampMs => AuraV3ColumnValues::TimestampMs(values),
        _ => return Err(AuraError::InvalidValue("v3 planned within logical type")),
    })
}

fn codec_for_column(plan: &AuraPlanV2, slot: u16) -> Result<PlanV2PhysicalCodec> {
    if !matches!(
        plan.registry_version,
        AURA_PLAN_V2_INTEGER_CODEC_REGISTRY_VERSION
            | AURA_PLAN_V2_WITHIN_DOMAIN_REGISTRY_VERSION
            | AURA_PLAN_V2_CROSS_DOMAIN_REGISTRY_VERSION
    ) {
        return Ok(PlanV2PhysicalCodec::FixedWidth);
    }
    let physical = plan
        .streams
        .get(usize::from(slot))
        .filter(|stream| stream.slot == slot)
        .and_then(|stream| stream.physical_stream_ids.first())
        .copied()
        .ok_or(AuraError::InvalidValue("v3 planned codec stream"))?;
    plan.physical_stream_codecs
        .get(usize::from(physical))
        .copied()
        .ok_or(AuraError::InvalidValue("v3 planned codec stream"))
}

fn encode_codec_lane(
    column: &AuraV3Column,
    rows: usize,
    codec: PlanV2PhysicalCodec,
    out: &mut Vec<u8>,
) -> Result<()> {
    if codec == PlanV2PhysicalCodec::FixedWidth {
        return encode_compact_lane(column, rows, out);
    }
    if let Some(validity) = &column.validity {
        out.try_reserve_exact(validity.len())
            .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
        out.extend_from_slice(validity);
    }
    match (&column.values, codec) {
        (AuraV3ColumnValues::U8(values), PlanV2PhysicalCodec::UnsignedUleb128) => {
            for value in values {
                crate::varint::encode_u64(u64::from(*value), out);
            }
        }
        (AuraV3ColumnValues::U16(values), PlanV2PhysicalCodec::UnsignedUleb128) => {
            for value in values {
                crate::varint::encode_u64(u64::from(*value), out);
            }
        }
        (AuraV3ColumnValues::U32(values), PlanV2PhysicalCodec::UnsignedUleb128) => {
            for value in values {
                crate::varint::encode_u64(u64::from(*value), out);
            }
        }
        (AuraV3ColumnValues::U64(values), PlanV2PhysicalCodec::UnsignedUleb128) => {
            for value in values {
                crate::varint::encode_u64(*value, out);
            }
        }
        (AuraV3ColumnValues::I8(values), PlanV2PhysicalCodec::SignedZigZagUleb128) => {
            for value in values {
                crate::varint::encode_i64(i64::from(*value), out);
            }
        }
        (AuraV3ColumnValues::I16(values), PlanV2PhysicalCodec::SignedZigZagUleb128) => {
            for value in values {
                crate::varint::encode_i64(i64::from(*value), out);
            }
        }
        (AuraV3ColumnValues::I32(values), PlanV2PhysicalCodec::SignedZigZagUleb128) => {
            for value in values {
                crate::varint::encode_i64(i64::from(*value), out);
            }
        }
        (
            AuraV3ColumnValues::I64(values)
            | AuraV3ColumnValues::TimestampNs(values)
            | AuraV3ColumnValues::TimestampMs(values),
            PlanV2PhysicalCodec::SignedZigZagUleb128,
        ) => {
            for value in values {
                crate::varint::encode_i64(*value, out);
            }
        }
        _ => return Err(AuraError::InvalidValue("v3 planned codec type")),
    }
    Ok(())
}

fn decode_codec_lane(
    slot: u16,
    field_type: crate::FieldType,
    nullable: bool,
    rows: usize,
    codec: PlanV2PhysicalCodec,
    reader: &mut ByteReader<'_>,
    limits: V3ValueLimits,
) -> Result<AuraV3Column> {
    if codec == PlanV2PhysicalCodec::FixedWidth {
        return decode_compact_lane(slot, field_type, nullable, rows, reader, limits);
    }
    let validity = if nullable {
        Some(reader.read_exact(rows.div_ceil(8))?.to_vec())
    } else {
        None
    };
    macro_rules! decode_unsigned {
        ($variant:ident, $type:ty) => {{
            let mut values = Vec::new();
            values
                .try_reserve_exact(rows)
                .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
            for _ in 0..rows {
                values.push(
                    <$type>::try_from(decode_canonical_uleb128(reader)?)
                        .map_err(|_| AuraError::InvalidValue("v3 planned unsigned varint range"))?,
                );
            }
            AuraV3ColumnValues::$variant(values)
        }};
    }
    macro_rules! decode_signed {
        ($variant:ident, $type:ty) => {{
            let mut values = Vec::new();
            values
                .try_reserve_exact(rows)
                .map_err(|_| AuraError::InvalidValue("v3 planned grouped allocation"))?;
            for _ in 0..rows {
                values.push(
                    <$type>::try_from(decode_canonical_zigzag(reader)?)
                        .map_err(|_| AuraError::InvalidValue("v3 planned signed varint range"))?,
                );
            }
            AuraV3ColumnValues::$variant(values)
        }};
    }
    let values = match (field_type, codec) {
        (crate::FieldType::U8, PlanV2PhysicalCodec::UnsignedUleb128) => {
            decode_unsigned!(U8, u8)
        }
        (crate::FieldType::U16, PlanV2PhysicalCodec::UnsignedUleb128) => {
            decode_unsigned!(U16, u16)
        }
        (crate::FieldType::U32, PlanV2PhysicalCodec::UnsignedUleb128) => {
            decode_unsigned!(U32, u32)
        }
        (crate::FieldType::U64, PlanV2PhysicalCodec::UnsignedUleb128) => {
            decode_unsigned!(U64, u64)
        }
        (crate::FieldType::I8, PlanV2PhysicalCodec::SignedZigZagUleb128) => {
            decode_signed!(I8, i8)
        }
        (crate::FieldType::I16, PlanV2PhysicalCodec::SignedZigZagUleb128) => {
            decode_signed!(I16, i16)
        }
        (crate::FieldType::I32, PlanV2PhysicalCodec::SignedZigZagUleb128) => {
            decode_signed!(I32, i32)
        }
        (crate::FieldType::I64, PlanV2PhysicalCodec::SignedZigZagUleb128) => {
            decode_signed!(I64, i64)
        }
        (crate::FieldType::TimestampNs, PlanV2PhysicalCodec::SignedZigZagUleb128) => {
            decode_signed!(TimestampNs, i64)
        }
        (crate::FieldType::TimestampMs, PlanV2PhysicalCodec::SignedZigZagUleb128) => {
            decode_signed!(TimestampMs, i64)
        }
        _ => return Err(AuraError::InvalidValue("v3 planned codec type")),
    };
    Ok(AuraV3Column {
        slot,
        validity,
        values,
    })
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
    let logical_limits = if footer.block_version == V3_PLANNED_GROUPED_DIRECT_BLOCK_VERSION {
        limits.event_limits
    } else {
        planned_structural_limits(limits.event_limits)
    };
    let mut global =
        CanonicalV3EventHasher::new(&footer.schema, total_events, total_children, logical_limits)?;
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
            || canonical_v3_event_batch_sha256(&footer.schema, &batch, logical_limits)?
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
        ) | (
            V3_PLANNED_GROUPED_INTEGER_CODEC_BODY_LAYOUT_VERSION,
            V3_PLANNED_GROUPED_INTEGER_CODEC_BLOCK_VERSION
        ) | (
            V3_PLANNED_GROUPED_WITHIN_DOMAIN_BODY_LAYOUT_VERSION,
            V3_PLANNED_GROUPED_WITHIN_DOMAIN_BLOCK_VERSION
        ) | (
            V3_PLANNED_GROUPED_CROSS_DOMAIN_BODY_LAYOUT_VERSION,
            V3_PLANNED_GROUPED_CROSS_DOMAIN_BLOCK_VERSION
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
        ) | (
            V3_PLANNED_GROUPED_INTEGER_CODEC_BODY_LAYOUT_VERSION,
            V3_PLANNED_GROUPED_INTEGER_CODEC_BLOCK_VERSION,
            crate::AURA_PLAN_V2_INTEGER_CODEC_REGISTRY_VERSION,
        ) | (
            V3_PLANNED_GROUPED_WITHIN_DOMAIN_BODY_LAYOUT_VERSION,
            V3_PLANNED_GROUPED_WITHIN_DOMAIN_BLOCK_VERSION,
            crate::AURA_PLAN_V2_WITHIN_DOMAIN_REGISTRY_VERSION,
        ) | (
            V3_PLANNED_GROUPED_CROSS_DOMAIN_BODY_LAYOUT_VERSION,
            V3_PLANNED_GROUPED_CROSS_DOMAIN_BLOCK_VERSION,
            crate::AURA_PLAN_V2_CROSS_DOMAIN_REGISTRY_VERSION,
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
        codecs: Vec::new(),
        within_domain: Vec::new(),
        cross_domain: Vec::new(),
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

#[cfg(test)]
mod candidate_limit_tests {
    use super::*;

    #[test]
    fn only_physical_length_limits_are_candidate_inapplicability() {
        for name in [
            "v3 event block length",
            "v3 planned block length",
            "v3 planned grouped body length",
            "v3 planned grouped footer length",
        ] {
            assert!(is_expected_candidate_limit(&AuraError::InvalidValue(name)));
        }
        for error in [
            AuraError::UnexpectedEof,
            AuraError::InvalidValue("v3 planned grouped allocation"),
            AuraError::InvalidValue("v3 planned split inverse"),
            AuraError::InvalidValue("aura plan v2 codec type"),
        ] {
            assert!(!is_expected_candidate_limit(&error));
        }
    }
}
