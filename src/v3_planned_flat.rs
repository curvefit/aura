//! Bounded all-memory reference planned-flat Aura0 V3 container.
//!
//! Exact, all-fixed, mixed-integer, variable-dictionary, the existing wrapped
//! plan, one primary-timestamp temporal wrapped artifact, and one exact-byte
//! previous-common-prefix/suffix wrapped artifact coexist during scoring. Peak
//! memory is therefore roughly their summed bytes plus lane/dictionary/
//! compression scratch. This is an honest replayable reference API, not a
//! streaming writer claim. Registry 2 dictionaries retain exact Utf8 and
//! DecimalText bytes. Registry 3 may use only schema-authorized temporal lanes.
//! Registry 4 additively permits per-chunk prefix/suffix byte lanes for Utf8 and
//! DecimalText. No candidate uses provider identity or inferred economics.

use std::io::{self, Cursor, Read, Write};
use std::{cell::Cell, rc::Rc};

use sha2::{Digest, Sha256};

use crate::bitpack::{
    bitpacked_byte_len, pack_unsigned_values, unpack_unsigned_values, unsigned_bitpack_width,
};
use crate::bytes::ByteReader;
use crate::format::{AuraContainerVersion, SEAL_MAGIC};
use crate::v3_codecs::{
    decode_canonical_uleb128, decode_canonical_zigzag, fixed_width, integer_varint_codec,
    PlanV2PhysicalCodec,
};
use crate::v3_flat_plan_v2::{
    temporal_field_authorized, FlatAuraPlanV2, FLAT_PLAN_V2_DICTIONARY_REGISTRY_VERSION,
    FLAT_PLAN_V2_PREFIX_SUFFIX_REGISTRY_VERSION, FLAT_PLAN_V2_REGISTRY_VERSION,
    FLAT_PLAN_V2_TEMPORAL_REGISTRY_VERSION,
};
use crate::v3_values::{
    decode_column, encode_column, is_present, validate_bitmap_padding, AuraV3VariableColumn,
};
use crate::{
    canonical_v3_batch_sha256, canonical_v3_schema_fingerprint, decode_schema_descriptor,
    decode_v3_flat_aura0_with_limits, encode_schema_descriptor, validate_decimal_text_v1,
    validate_v3_batch, AuraError, AuraHeader, AuraV3Batch, AuraV3Column, AuraV3ColumnValues,
    CanonicalV3RowHasher, DecodedV3FlatAura0, Profile, Result, SchemaDescriptor, V3FlatAura0Writer,
    V3FlatLimits, V3FlatWriterOptions, V3ValueLimits, MAX_V3_VALUE_BLOCK_BYTES,
    V3_FLAT_BODY_ENCODING_EXACT_BLOCKS, V3_FLAT_FOOTER_LAYOUT_VERSION,
};

pub const V3_PLANNED_FLAT_FOOTER_LAYOUT_VERSION: u16 = 3;
pub const V3_PLANNED_FLAT_BODY_ENCODING: u8 = 4;
pub const V3_PLANNED_FLAT_BODY_LAYOUT_VERSION: u16 = 1;
pub const V3_PLANNED_FLAT_BLOCK_VERSION: u16 = 1;
pub const V3_PLANNED_FLAT_DICTIONARY_BODY_LAYOUT_VERSION: u16 = 2;
pub const V3_PLANNED_FLAT_DICTIONARY_BLOCK_VERSION: u16 = 2;
pub const V3_PLANNED_FLAT_ZSTD_BODY_LAYOUT_VERSION: u16 = 3;
pub const V3_PLANNED_FLAT_ZSTD_BLOCK_VERSION: u16 = 3;
pub const V3_PLANNED_FLAT_ZSTD_WRAPPER_VERSION: u16 = 1;
pub const V3_PLANNED_FLAT_ZSTD_LEVEL: i32 = 19;
pub const V3_PLANNED_FLAT_ZSTD_WINDOW_LOG: u8 = 23;
pub const V3_PLANNED_FLAT_TEMPORAL_BODY_LAYOUT_VERSION: u16 = 4;
pub const V3_PLANNED_FLAT_TEMPORAL_BLOCK_VERSION: u16 = 4;
pub const V3_PLANNED_FLAT_TEMPORAL_ZSTD_BODY_LAYOUT_VERSION: u16 = 5;
pub const V3_PLANNED_FLAT_TEMPORAL_ZSTD_BLOCK_VERSION: u16 = 5;
pub const V3_PLANNED_FLAT_TEMPORAL_ZSTD_WRAPPER_VERSION: u16 = 2;
pub const V3_PLANNED_FLAT_PREFIX_SUFFIX_BODY_LAYOUT_VERSION: u16 = 6;
pub const V3_PLANNED_FLAT_PREFIX_SUFFIX_BLOCK_VERSION: u16 = 6;
pub const V3_PLANNED_FLAT_PREFIX_SUFFIX_ZSTD_BODY_LAYOUT_VERSION: u16 = 7;
pub const V3_PLANNED_FLAT_PREFIX_SUFFIX_ZSTD_BLOCK_VERSION: u16 = 7;
pub const V3_PLANNED_FLAT_PREFIX_SUFFIX_ZSTD_WRAPPER_VERSION: u16 = 3;
pub const V3_PLANNED_FLAT_FOOTER_PREFIX_BYTES: usize = 208;
pub const V3_PLANNED_FLAT_CHUNK_DESCRIPTOR_BYTES: usize = 104;
pub const MAX_V3_PLANNED_FLAT_FOOTER_BYTES: usize = 64 * 1024 * 1024;

const BLOCK_MAGIC_V1: &[u8; 8] = b"AUFPVB01";
const BLOCK_MAGIC_V2: &[u8; 8] = b"AUFPVB02";
const BLOCK_MAGIC_V3: &[u8; 8] = b"AUFPVB03";
const BLOCK_MAGIC_V4: &[u8; 8] = b"AUFPVB04";
const BLOCK_HEADER_BYTES: usize = 64;
const DICTIONARY_LANE_PREFIX_BYTES: usize = 12;
const PREFIX_SUFFIX_LANE_HEADER_BYTES: usize = 16;
const PREFIX_SUFFIX_LANE_VERSION: u16 = 1;
const ZSTD_WRAPPER_MAGIC: &[u8; 8] = b"AUFPZB01";
const TEMPORAL_ZSTD_WRAPPER_MAGIC: &[u8; 8] = b"AUFPZB02";
const PREFIX_SUFFIX_ZSTD_WRAPPER_MAGIC: &[u8; 8] = b"AUFPZB03";
const ZSTD_WRAPPER_HEADER_BYTES: usize = 68;
const ZSTD_CODEC_ID: u8 = 1;
const ZSTD_WRAPPER_FLAGS: u8 = 0b0000_0011;
const FOOTER_HASH_DOMAIN: &[u8] = b"aura-v3-planned-flat-footer-v1\0";
const HEADER_HASH_DOMAIN: &[u8] = b"aura-v3-planned-flat-header-v1\0";
const BODY_HASH_DOMAIN: &[u8] = b"aura-v3-planned-flat-body-v1\0";
const PREFIX_SUFFIX_DERIVED_RAW_PLAN_ID: &str = "planned-flat-prefix-suffix-raw-registry4";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedFlatChunkDescriptor {
    pub chunk_id: u32,
    pub first_global_row: u64,
    pub row_count: u32,
    pub body_relative_offset: u64,
    pub stored_len: u64,
    pub stored_sha256: [u8; 32],
    pub chunk_logical_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedFlatFooter {
    pub record_count: u64,
    pub body_len: u64,
    pub body_layout_version: u16,
    pub block_version: u16,
    pub schema: SchemaDescriptor,
    pub plan: FlatAuraPlanV2,
    pub schema_fingerprint: [u8; 32],
    pub plan_sha256: [u8; 32],
    pub header_sha256: [u8; 32],
    pub body_sha256: [u8; 32],
    pub global_logical_sha256: [u8; 32],
    pub chunks: Vec<V3PlannedFlatChunkDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedFlatSummary {
    pub row_count: u64,
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
pub struct V3PlannedFlatCandidateInspection {
    pub candidate_id: String,
    pub applicable: bool,
    pub rejection: Option<String>,
    pub complete_bytes: Option<u64>,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedFlatCodecInspection {
    pub slot: u16,
    pub field_type: crate::FieldType,
    pub fixed_bytes: u64,
    pub varint_bytes: Option<u64>,
    pub dictionary_bytes: Option<u64>,
    pub present_count: Option<u64>,
    pub null_count: Option<u64>,
    pub present_empty_count: Option<u64>,
    pub dictionary_entries: Option<u64>,
    pub max_chunk_dictionary_entries: Option<u32>,
    pub unique_data_bytes: Option<u64>,
    pub dictionary_index_bytes: Option<u64>,
    pub dictionary_selected: bool,
    pub dictionary_rejection: Option<String>,
    pub temporal_direct_bytes: Option<u64>,
    pub previous_delta_bytes: Option<u64>,
    pub delta_of_delta_bytes: Option<u64>,
    pub previous_delta_authorized: bool,
    pub delta_of_delta_authorized: bool,
    pub temporal_selected: bool,
    pub temporal_rejection: Option<String>,
    pub prefix_suffix_bytes: Option<u64>,
    pub prefix_suffix_frame_bytes: Option<u64>,
    pub prefix_suffix_control_bytes: Option<u64>,
    pub prefix_suffix_literal_bytes: Option<u64>,
    pub prefix_suffix_validity_bytes: Option<u64>,
    pub prefix_suffix_baseline_codec: Option<PlanV2PhysicalCodec>,
    pub prefix_suffix_baseline_bytes: Option<u64>,
    pub prefix_suffix_selected: bool,
    pub prefix_suffix_rejection: Option<String>,
    pub selected: PlanV2PhysicalCodec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedFlatInspection {
    pub candidates: Vec<V3PlannedFlatCandidateInspection>,
    pub codecs: Vec<V3PlannedFlatCodecInspection>,
    pub dictionary_candidate_codecs: Vec<V3PlannedFlatCodecInspection>,
    pub zstd_candidate: Option<V3PlannedFlatZstdInspection>,
    pub temporal_zstd_candidate: Option<V3PlannedFlatZstdInspection>,
    pub temporal_candidate_codecs: Vec<V3PlannedFlatCodecInspection>,
    pub prefix_suffix_zstd_candidate: Option<V3PlannedFlatPrefixSuffixZstdInspection>,
    pub prefix_suffix_candidate_codecs: Vec<V3PlannedFlatCodecInspection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedFlatZstdInspection {
    pub base_candidate_id: String,
    pub base_registry_version: u16,
    pub inner_body_layout_version: u16,
    pub inner_block_version: u16,
    pub inner_body_bytes: u64,
    pub compressed_payload_bytes: u64,
    pub wrapper_overhead_bytes: u64,
    pub stored_body_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedFlatPrefixSuffixZstdInspection {
    pub inherited_complete_candidate_id: String,
    pub inherited_complete_registry_version: u16,
    pub derived_raw_plan_id: String,
    pub derived_raw_plan_registry_version: u16,
    pub inner_body_layout_version: u16,
    pub inner_block_version: u16,
    pub inner_body_bytes: u64,
    pub compressed_payload_bytes: u64,
    pub wrapper_overhead_bytes: u64,
    pub stored_body_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedFlatArtifact {
    pub bytes: Vec<u8>,
    pub summary: V3PlannedFlatSummary,
    pub inspection: V3PlannedFlatInspection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedV3PlannedFlat {
    pub header: AuraHeader,
    pub footer: V3PlannedFlatFooter,
    pub batches: Vec<AuraV3Batch>,
    pub summary: V3PlannedFlatSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedV3SelectedFlat {
    Exact(Box<DecodedV3FlatAura0>),
    Planned(Box<DecodedV3PlannedFlat>),
}

pub fn compile_v3_planned_flat(
    schema: &SchemaDescriptor,
    batches: &[AuraV3Batch],
    limits: V3FlatLimits,
) -> Result<V3PlannedFlatArtifact> {
    let limits = limits.effective();
    validate_inputs(schema, batches, limits)?;
    let exact = compile_exact(schema, batches, limits);
    let fixed_plan = FlatAuraPlanV2::all_fixed(schema);
    let fixed = fixed_plan
        .clone()
        .and_then(|plan| compile_plan(schema, batches, limits, plan));
    let mixed_plan_rows = fixed_plan.and_then(|plan| select_codecs(schema, batches, plan));
    let mixed_rows = mixed_plan_rows
        .as_ref()
        .ok()
        .map(|(_, rows)| rows.clone())
        .unwrap_or_default();
    let mixed = match &mixed_plan_rows {
        Ok((plan, _)) => compile_plan(schema, batches, limits, plan.clone()),
        Err(error) => Err(error.clone()),
    };
    let dictionary_plan_rows = match &mixed_plan_rows {
        Ok((plan, rows)) => select_dictionary_codecs(schema, batches, plan.clone(), rows.clone()),
        Err(error) => Err(error.clone()),
    };
    let dictionary_rows = dictionary_plan_rows
        .as_ref()
        .ok()
        .map(|(_, rows)| rows.clone())
        .unwrap_or_default();
    let dictionary = match &dictionary_plan_rows {
        Ok((Some(plan), _)) => compile_plan(schema, batches, limits, plan.clone()),
        Ok((None, _)) => Err(AuraError::InvalidValue(
            "planned flat dictionary no lane win",
        )),
        Err(error) => Err(error.clone()),
    };
    let base_plan_rows = match (&dictionary, &dictionary_plan_rows) {
        (Ok(_), Ok((Some(plan), _))) => Ok((
            plan.clone(),
            dictionary_rows.clone(),
            "planned-flat-variable-dictionary",
        )),
        _ => match &mixed_plan_rows {
            Ok((plan, _)) => Ok((
                plan.clone(),
                mixed_rows.clone(),
                "planned-flat-integer-codecs",
            )),
            Err(error) => Err(error.clone()),
        },
    };
    let (zstd, zstd_rows) = match &base_plan_rows {
        Ok((plan, rows, base_candidate_id)) => (
            compile_zstd_plan(schema, batches, limits, plan.clone(), base_candidate_id),
            rows.clone(),
        ),
        Err(error) => (Err(error.clone()), Vec::new()),
    };
    let zstd_inspection = zstd
        .as_ref()
        .ok()
        .and_then(|artifact| artifact.inspection.zstd_candidate.clone());
    let temporal_plan_rows = match &base_plan_rows {
        Ok((plan, rows, _)) => select_temporal_codecs(schema, batches, plan.clone(), rows.clone()),
        Err(error) => Err(error.clone()),
    };
    let temporal_rows = temporal_plan_rows
        .as_ref()
        .ok()
        .map(|(_, rows)| rows.clone())
        .unwrap_or_default();
    let temporal = match (&temporal_plan_rows, &base_plan_rows) {
        (Ok((Some(plan), _)), Ok((base_plan, _, base_candidate_id))) => compile_temporal_zstd_plan(
            schema,
            batches,
            limits,
            plan.clone(),
            base_candidate_id,
            base_plan.registry_version,
        ),
        (Ok((None, _)), _) => Err(AuraError::InvalidValue("planned flat temporal no lane win")),
        (Err(error), _) => Err(error.clone()),
        (_, Err(error)) => Err(error.clone()),
    };
    let temporal_zstd_inspection = temporal
        .as_ref()
        .ok()
        .and_then(|artifact| artifact.inspection.temporal_zstd_candidate.clone());
    let prefix_suffix_base = match &temporal_plan_rows {
        Ok((Some(plan), _)) => Ok((
            plan.clone(),
            temporal_rows.clone(),
            "planned-flat-temporal-zstd19-wrapper",
            FLAT_PLAN_V2_TEMPORAL_REGISTRY_VERSION,
        )),
        Ok((None, _)) => match &base_plan_rows {
            Ok((plan, rows, base_candidate_id)) => Ok((
                plan.clone(),
                rows.clone(),
                *base_candidate_id,
                plan.registry_version,
            )),
            Err(error) => Err(error.clone()),
        },
        Err(error) => Err(error.clone()),
    };
    let prefix_suffix_plan_rows = match &prefix_suffix_base {
        Ok((plan, rows, _, _)) => select_prefix_suffix_codecs(
            schema,
            batches,
            plan.clone(),
            rows.clone(),
            limits.value_limits.effective(),
        ),
        Err(error) => Err(error.clone()),
    };
    let prefix_suffix_rows = prefix_suffix_plan_rows
        .as_ref()
        .ok()
        .map(|(_, rows)| rows.clone())
        .unwrap_or_default();
    let prefix_suffix = match (&prefix_suffix_plan_rows, &prefix_suffix_base) {
        (Ok((Some(plan), _)), Ok((_, _, base_candidate_id, base_registry_version))) => {
            compile_prefix_suffix_zstd_plan(
                schema,
                batches,
                limits,
                plan.clone(),
                base_candidate_id,
                *base_registry_version,
            )
        }
        (Ok((None, _)), _) => Err(AuraError::InvalidValue(
            "planned flat prefix suffix no lane win",
        )),
        (Err(error), _) => Err(error.clone()),
        (_, Err(error)) => Err(error.clone()),
    };
    let prefix_suffix_zstd_inspection = prefix_suffix
        .as_ref()
        .ok()
        .and_then(|artifact| artifact.inspection.prefix_suffix_zstd_candidate.clone());
    let candidates = vec![
        exact,
        fixed,
        mixed,
        dictionary,
        zstd,
        temporal,
        prefix_suffix,
    ];
    if let Some(error) = candidates
        .iter()
        .filter_map(|candidate| candidate.as_ref().err())
        .find(|error| !candidate_inapplicable(error))
    {
        return Err(error.clone());
    }
    let sizes = candidates
        .iter()
        .map(|candidate| {
            candidate
                .as_ref()
                .ok()
                .map(|artifact| artifact.summary.file_bytes)
        })
        .collect::<Vec<_>>();
    let selected_index = select_complete_candidate(&sizes)?;
    let ids = [
        "exact-flat",
        "planned-flat-fixed",
        "planned-flat-integer-codecs",
        "planned-flat-variable-dictionary",
        "planned-flat-zstd19-wrapper",
        "planned-flat-temporal-zstd19-wrapper",
        "planned-flat-prefix-suffix-zstd19-wrapper",
    ];
    let candidate_rows = ids
        .iter()
        .enumerate()
        .map(|(index, id)| V3PlannedFlatCandidateInspection {
            candidate_id: (*id).to_owned(),
            applicable: candidates[index].is_ok(),
            rejection: match &candidates[index] {
                Err(error) => Some(error.to_string()),
                Ok(_) if index != selected_index => Some("complete cost did not win".to_owned()),
                Ok(_) => None,
            },
            complete_bytes: sizes[index],
            selected: index == selected_index,
        })
        .collect();
    let mut selected = candidates.into_iter().nth(selected_index).unwrap()?;
    selected.inspection.candidates = candidate_rows;
    selected.inspection.codecs = match selected_index {
        2 => mixed_rows,
        3 => dictionary_rows.clone(),
        4 => zstd_rows,
        5 => temporal_rows.clone(),
        6 => prefix_suffix_rows.clone(),
        _ => Vec::new(),
    };
    selected.inspection.dictionary_candidate_codecs = dictionary_rows;
    selected.inspection.zstd_candidate = zstd_inspection;
    selected.inspection.temporal_candidate_codecs = temporal_rows;
    selected.inspection.temporal_zstd_candidate = temporal_zstd_inspection;
    selected.inspection.prefix_suffix_candidate_codecs = prefix_suffix_rows;
    selected.inspection.prefix_suffix_zstd_candidate = prefix_suffix_zstd_inspection;
    Ok(selected)
}

fn select_complete_candidate(sizes: &[Option<u64>]) -> Result<usize> {
    sizes
        .iter()
        .enumerate()
        .filter_map(|(index, bytes)| bytes.map(|bytes| (index, bytes)))
        .min_by_key(|(index, bytes)| (*bytes, *index))
        .map(|(index, _)| index)
        .ok_or(AuraError::InvalidValue(
            "planned flat no applicable candidate",
        ))
}

fn validate_inputs(
    schema: &SchemaDescriptor,
    batches: &[AuraV3Batch],
    limits: V3FlatLimits,
) -> Result<()> {
    let limits = limits.effective();
    crate::v3_container::validate_flat_schema(schema)?;
    if batches.len() > limits.max_chunks {
        return Err(AuraError::InvalidValue("planned flat chunk count"));
    }
    let structural = V3ValueLimits {
        max_block_bytes: MAX_V3_VALUE_BLOCK_BYTES,
        ..limits.value_limits
    };
    let mut rows = 0u64;
    for batch in batches {
        validate_v3_batch(schema, batch, structural)?;
        if batch.row_count == 0 {
            return Err(AuraError::InvalidValue("planned flat empty chunk"));
        }
        rows = rows
            .checked_add(u64::from(batch.row_count))
            .filter(|rows| *rows <= limits.max_rows)
            .ok_or(AuraError::InvalidValue("planned flat row count"))?;
    }
    Ok(())
}

fn compile_exact(
    schema: &SchemaDescriptor,
    batches: &[AuraV3Batch],
    limits: V3FlatLimits,
) -> Result<V3PlannedFlatArtifact> {
    let mut writer = V3FlatAura0Writer::try_new(
        Cursor::new(Vec::new()),
        schema.clone(),
        V3FlatWriterOptions {
            limits,
            comment: String::new(),
        },
    )?;
    for batch in batches {
        writer.write_batch(batch)?;
    }
    let (cursor, result) = writer.finish()?;
    Ok(V3PlannedFlatArtifact {
        bytes: cursor.into_inner(),
        summary: V3PlannedFlatSummary {
            row_count: result.record_count,
            chunk_count: result.chunk_count,
            header_bytes: result.header_bytes,
            body_bytes: result.body_bytes,
            footer_bytes: result.footer_bytes,
            file_bytes: result.file_bytes,
            accounted_file_bytes: result.file_bytes,
            schema_fingerprint: result.schema_fingerprint,
            plan_sha256: [0; 32],
            global_logical_sha256: result.global_logical_sha256,
        },
        inspection: V3PlannedFlatInspection {
            candidates: Vec::new(),
            codecs: Vec::new(),
            dictionary_candidate_codecs: Vec::new(),
            zstd_candidate: None,
            temporal_zstd_candidate: None,
            temporal_candidate_codecs: Vec::new(),
            prefix_suffix_zstd_candidate: None,
            prefix_suffix_candidate_codecs: Vec::new(),
        },
    })
}

fn select_codecs(
    schema: &SchemaDescriptor,
    batches: &[AuraV3Batch],
    mut plan: FlatAuraPlanV2,
) -> Result<(FlatAuraPlanV2, Vec<V3PlannedFlatCodecInspection>)> {
    let mut rows = Vec::new();
    for field in &schema.fields {
        let fixed = lane_total(
            batches,
            field.index as usize,
            PlanV2PhysicalCodec::FixedWidth,
        )?;
        let varint_codec = integer_varint_codec(field.field_type);
        let varint = varint_codec
            .map(|codec| lane_total(batches, field.index as usize, codec))
            .transpose()?;
        let selected = match (varint_codec, varint) {
            (Some(codec), Some(bytes)) if bytes < fixed => codec,
            _ => PlanV2PhysicalCodec::FixedWidth,
        };
        plan.codecs[field.index as usize] = selected;
        rows.push(V3PlannedFlatCodecInspection {
            slot: field.index,
            field_type: field.field_type,
            fixed_bytes: fixed as u64,
            varint_bytes: varint.map(|v| v as u64),
            dictionary_bytes: None,
            present_count: None,
            null_count: None,
            present_empty_count: None,
            dictionary_entries: None,
            max_chunk_dictionary_entries: None,
            unique_data_bytes: None,
            dictionary_index_bytes: None,
            dictionary_selected: false,
            dictionary_rejection: None,
            temporal_direct_bytes: None,
            previous_delta_bytes: None,
            delta_of_delta_bytes: None,
            previous_delta_authorized: false,
            delta_of_delta_authorized: false,
            temporal_selected: false,
            temporal_rejection: None,
            prefix_suffix_bytes: None,
            prefix_suffix_frame_bytes: None,
            prefix_suffix_control_bytes: None,
            prefix_suffix_literal_bytes: None,
            prefix_suffix_validity_bytes: None,
            prefix_suffix_baseline_codec: None,
            prefix_suffix_baseline_bytes: None,
            prefix_suffix_selected: false,
            prefix_suffix_rejection: None,
            selected,
        });
    }
    plan.validate(schema)?;
    Ok((plan, rows))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DictionaryLaneTotals {
    encoded_bytes: u64,
    present_count: u64,
    null_count: u64,
    present_empty_count: u64,
    dictionary_entries: u64,
    max_chunk_dictionary_entries: u32,
    unique_data_bytes: u64,
    index_bytes: u64,
}

fn select_dictionary_codecs(
    schema: &SchemaDescriptor,
    batches: &[AuraV3Batch],
    mut plan: FlatAuraPlanV2,
    mut rows: Vec<V3PlannedFlatCodecInspection>,
) -> Result<(Option<FlatAuraPlanV2>, Vec<V3PlannedFlatCodecInspection>)> {
    if rows.len() != schema.fields.len() || plan.registry_version != FLAT_PLAN_V2_REGISTRY_VERSION {
        return Err(AuraError::InvalidValue("planned flat dictionary analysis"));
    }
    plan.registry_version = FLAT_PLAN_V2_DICTIONARY_REGISTRY_VERSION;
    let mut selected_any = false;
    for (field, row) in schema.fields.iter().zip(&mut rows) {
        if !matches!(
            field.field_type,
            crate::FieldType::Utf8 | crate::FieldType::DecimalText
        ) {
            row.dictionary_rejection = Some("logical type is not dictionary-eligible".to_owned());
            continue;
        }
        let totals = dictionary_lane_totals(batches, usize::from(field.index))?;
        row.dictionary_bytes = Some(totals.encoded_bytes);
        row.present_count = Some(totals.present_count);
        row.null_count = Some(totals.null_count);
        row.present_empty_count = Some(totals.present_empty_count);
        row.dictionary_entries = Some(totals.dictionary_entries);
        row.max_chunk_dictionary_entries = Some(totals.max_chunk_dictionary_entries);
        row.unique_data_bytes = Some(totals.unique_data_bytes);
        row.dictionary_index_bytes = Some(totals.index_bytes);
        if totals.encoded_bytes < row.fixed_bytes {
            row.dictionary_selected = true;
            row.dictionary_rejection = None;
            row.selected = PlanV2PhysicalCodec::VariableByteDictionaryBitpacked;
            plan.codecs[usize::from(field.index)] = row.selected;
            selected_any = true;
        } else {
            row.dictionary_rejection = Some("dictionary lane bytes did not win".to_owned());
        }
    }
    if selected_any {
        plan.validate(schema)?;
        Ok((Some(plan), rows))
    } else {
        Ok((None, rows))
    }
}

fn select_temporal_codecs(
    schema: &SchemaDescriptor,
    batches: &[AuraV3Batch],
    mut plan: FlatAuraPlanV2,
    mut rows: Vec<V3PlannedFlatCodecInspection>,
) -> Result<(Option<FlatAuraPlanV2>, Vec<V3PlannedFlatCodecInspection>)> {
    if rows.len() != schema.fields.len()
        || !matches!(
            plan.registry_version,
            FLAT_PLAN_V2_REGISTRY_VERSION | FLAT_PLAN_V2_DICTIONARY_REGISTRY_VERSION
        )
    {
        return Err(AuraError::InvalidValue("planned flat temporal analysis"));
    }
    plan.registry_version = FLAT_PLAN_V2_TEMPORAL_REGISTRY_VERSION;
    let mut selected_any = false;
    for (field, row) in schema.fields.iter().zip(&mut rows) {
        let previous_authorized =
            temporal_field_authorized(schema, field, crate::FieldTransform::DeltaPrevious);
        let delta2_authorized =
            temporal_field_authorized(schema, field, crate::FieldTransform::Delta2);
        row.previous_delta_authorized = previous_authorized;
        row.delta_of_delta_authorized = delta2_authorized;
        if !previous_authorized && !delta2_authorized {
            row.temporal_rejection = Some(
                "primary timestamp header and explicit transform do not authorize temporal codec"
                    .to_owned(),
            );
            continue;
        }
        let direct = u64::try_from(lane_total(batches, usize::from(field.index), row.selected)?)
            .map_err(|_| AuraError::InvalidValue("planned flat temporal lane length"))?;
        row.temporal_direct_bytes = Some(direct);
        let previous = if previous_authorized {
            temporal_lane_total(
                batches,
                usize::from(field.index),
                PlanV2PhysicalCodec::TimestampPreviousDeltaZigZagUleb128,
            )?
        } else {
            None
        };
        let delta2 = if delta2_authorized {
            temporal_lane_total(
                batches,
                usize::from(field.index),
                PlanV2PhysicalCodec::TimestampDeltaOfDeltaZigZagUleb128,
            )?
        } else {
            None
        };
        row.previous_delta_bytes = previous;
        row.delta_of_delta_bytes = delta2;
        let mut selected_codec = row.selected;
        let mut selected_bytes = direct;
        if previous.is_some_and(|bytes| bytes < selected_bytes) {
            selected_codec = PlanV2PhysicalCodec::TimestampPreviousDeltaZigZagUleb128;
            selected_bytes = previous.unwrap();
        }
        if delta2.is_some_and(|bytes| bytes < selected_bytes) {
            selected_codec = PlanV2PhysicalCodec::TimestampDeltaOfDeltaZigZagUleb128;
        }
        if selected_codec == row.selected {
            row.temporal_rejection = Some("temporal lane bytes did not win".to_owned());
        } else {
            row.selected = selected_codec;
            row.temporal_selected = true;
            row.temporal_rejection = None;
            plan.codecs[usize::from(field.index)] = selected_codec;
            selected_any = true;
        }
    }
    if selected_any {
        plan.validate(schema)?;
        Ok((Some(plan), rows))
    } else {
        Ok((None, rows))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PrefixSuffixLaneTotals {
    encoded_bytes: u64,
    frame_bytes: u64,
    control_bytes: u64,
    literal_bytes: u64,
    validity_bytes: u64,
    present_count: u64,
    null_count: u64,
    present_empty_count: u64,
}

fn select_prefix_suffix_codecs(
    schema: &SchemaDescriptor,
    batches: &[AuraV3Batch],
    mut plan: FlatAuraPlanV2,
    mut rows: Vec<V3PlannedFlatCodecInspection>,
    limits: V3ValueLimits,
) -> Result<(Option<FlatAuraPlanV2>, Vec<V3PlannedFlatCodecInspection>)> {
    if rows.len() != schema.fields.len()
        || !matches!(
            plan.registry_version,
            FLAT_PLAN_V2_REGISTRY_VERSION
                | FLAT_PLAN_V2_DICTIONARY_REGISTRY_VERSION
                | FLAT_PLAN_V2_TEMPORAL_REGISTRY_VERSION
        )
    {
        return Err(AuraError::InvalidValue(
            "planned flat prefix suffix analysis",
        ));
    }
    plan.registry_version = FLAT_PLAN_V2_PREFIX_SUFFIX_REGISTRY_VERSION;
    let mut selected_any = false;
    for (field, row) in schema.fields.iter().zip(&mut rows) {
        if !matches!(
            field.field_type,
            crate::FieldType::Utf8 | crate::FieldType::DecimalText
        ) {
            row.prefix_suffix_rejection =
                Some("logical type is not prefix-suffix-eligible".to_owned());
            continue;
        }
        let direct = u64::try_from(lane_total(batches, usize::from(field.index), row.selected)?)
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix lane length"))?;
        let totals = prefix_suffix_lane_totals(batches, usize::from(field.index), limits)?;
        row.prefix_suffix_bytes = Some(totals.encoded_bytes);
        row.prefix_suffix_frame_bytes = Some(totals.frame_bytes);
        row.prefix_suffix_control_bytes = Some(totals.control_bytes);
        row.prefix_suffix_literal_bytes = Some(totals.literal_bytes);
        row.prefix_suffix_validity_bytes = Some(totals.validity_bytes);
        row.prefix_suffix_baseline_codec = Some(row.selected);
        row.prefix_suffix_baseline_bytes = Some(direct);
        row.present_count = Some(totals.present_count);
        row.null_count = Some(totals.null_count);
        row.present_empty_count = Some(totals.present_empty_count);
        if totals.encoded_bytes < direct {
            row.prefix_suffix_selected = true;
            row.prefix_suffix_rejection = None;
            row.selected = PlanV2PhysicalCodec::PreviousCommonPrefixSuffixBytes;
            plan.codecs[usize::from(field.index)] = row.selected;
            selected_any = true;
        } else {
            row.prefix_suffix_rejection = Some("prefix suffix lane bytes did not win".to_owned());
        }
    }
    if selected_any {
        plan.validate(schema)?;
        Ok((Some(plan), rows))
    } else {
        Ok((None, rows))
    }
}

fn prefix_suffix_lane_totals(
    batches: &[AuraV3Batch],
    slot: usize,
    limits: V3ValueLimits,
) -> Result<PrefixSuffixLaneTotals> {
    let mut totals = PrefixSuffixLaneTotals::default();
    for batch in batches {
        let encoded = encode_prefix_suffix_lane(
            batch
                .columns
                .get(slot)
                .ok_or(AuraError::InvalidValue("planned flat prefix suffix slot"))?,
            usize::try_from(batch.row_count)
                .map_err(|_| AuraError::InvalidValue("planned flat row count"))?,
            limits,
        )?;
        totals.encoded_bytes =
            totals
                .encoded_bytes
                .checked_add(u64::try_from(encoded.bytes.len()).map_err(|_| {
                    AuraError::InvalidValue("planned flat prefix suffix lane length")
                })?)
                .ok_or(AuraError::InvalidValue(
                    "planned flat prefix suffix lane length",
                ))?;
        totals.frame_bytes = totals
            .frame_bytes
            .checked_add(PREFIX_SUFFIX_LANE_HEADER_BYTES as u64)
            .ok_or(AuraError::InvalidValue(
                "planned flat prefix suffix lane length",
            ))?;
        totals.control_bytes = totals
            .control_bytes
            .checked_add(encoded.control_bytes)
            .ok_or(AuraError::InvalidValue(
                "planned flat prefix suffix lane length",
            ))?;
        totals.literal_bytes = totals
            .literal_bytes
            .checked_add(encoded.literal_bytes)
            .ok_or(AuraError::InvalidValue(
                "planned flat prefix suffix lane length",
            ))?;
        totals.validity_bytes = totals
            .validity_bytes
            .checked_add(encoded.validity_bytes)
            .ok_or(AuraError::InvalidValue(
                "planned flat prefix suffix lane length",
            ))?;
        totals.present_count = totals
            .present_count
            .checked_add(encoded.present_count)
            .ok_or(AuraError::InvalidValue("planned flat prefix suffix count"))?;
        totals.null_count = totals
            .null_count
            .checked_add(encoded.null_count)
            .ok_or(AuraError::InvalidValue("planned flat prefix suffix count"))?;
        totals.present_empty_count = totals
            .present_empty_count
            .checked_add(encoded.present_empty_count)
            .ok_or(AuraError::InvalidValue("planned flat prefix suffix count"))?;
    }
    Ok(totals)
}

fn temporal_lane_total(
    batches: &[AuraV3Batch],
    slot: usize,
    codec: PlanV2PhysicalCodec,
) -> Result<Option<u64>> {
    let mut total = 0u64;
    for batch in batches {
        let column = batch
            .columns
            .get(slot)
            .ok_or(AuraError::InvalidValue("planned flat temporal slot"))?;
        let mut encoded = Vec::new();
        match encode_temporal_lane(column, codec, &mut encoded) {
            Ok(()) => {}
            Err(AuraError::InvalidValue("planned flat temporal overflow")) => return Ok(None),
            Err(error) => return Err(error),
        }
        total = total
            .checked_add(
                u64::try_from(encoded.len())
                    .map_err(|_| AuraError::InvalidValue("planned flat temporal lane length"))?,
            )
            .ok_or(AuraError::InvalidValue("planned flat temporal lane length"))?;
    }
    Ok(Some(total))
}

fn dictionary_lane_totals(batches: &[AuraV3Batch], slot: usize) -> Result<DictionaryLaneTotals> {
    let mut totals = DictionaryLaneTotals::default();
    for batch in batches {
        let encoded = encode_dictionary_lane(
            batch
                .columns
                .get(slot)
                .ok_or(AuraError::InvalidValue("planned flat dictionary slot"))?,
            usize::try_from(batch.row_count)
                .map_err(|_| AuraError::InvalidValue("planned flat row count"))?,
        )?;
        totals.encoded_bytes = totals
            .encoded_bytes
            .checked_add(
                u64::try_from(encoded.bytes.len())
                    .map_err(|_| AuraError::InvalidValue("planned flat dictionary lane length"))?,
            )
            .ok_or(AuraError::InvalidValue(
                "planned flat dictionary lane length",
            ))?;
        totals.present_count = totals
            .present_count
            .checked_add(encoded.present_count)
            .ok_or(AuraError::InvalidValue("planned flat dictionary count"))?;
        totals.null_count = totals
            .null_count
            .checked_add(encoded.null_count)
            .ok_or(AuraError::InvalidValue("planned flat dictionary count"))?;
        totals.present_empty_count = totals
            .present_empty_count
            .checked_add(encoded.present_empty_count)
            .ok_or(AuraError::InvalidValue("planned flat dictionary count"))?;
        totals.dictionary_entries = totals
            .dictionary_entries
            .checked_add(u64::from(encoded.entry_count))
            .ok_or(AuraError::InvalidValue("planned flat dictionary count"))?;
        totals.max_chunk_dictionary_entries =
            totals.max_chunk_dictionary_entries.max(encoded.entry_count);
        totals.unique_data_bytes = totals
            .unique_data_bytes
            .checked_add(encoded.unique_data_bytes)
            .ok_or(AuraError::InvalidValue(
                "planned flat dictionary data length",
            ))?;
        totals.index_bytes =
            totals
                .index_bytes
                .checked_add(encoded.index_bytes)
                .ok_or(AuraError::InvalidValue(
                    "planned flat dictionary index length",
                ))?;
    }
    Ok(totals)
}

fn lane_total(batches: &[AuraV3Batch], slot: usize, codec: PlanV2PhysicalCodec) -> Result<usize> {
    let mut total = 0usize;
    for batch in batches {
        let mut out = Vec::new();
        encode_lane(
            &batch.columns[slot],
            batch.row_count as usize,
            codec,
            V3ValueLimits::HARD,
            &mut out,
        )?;
        total = total
            .checked_add(out.len())
            .ok_or(AuraError::InvalidValue("planned flat lane length"))?;
    }
    Ok(total)
}

fn compile_plan(
    schema: &SchemaDescriptor,
    batches: &[AuraV3Batch],
    limits: V3FlatLimits,
    plan: FlatAuraPlanV2,
) -> Result<V3PlannedFlatArtifact> {
    compile_plan_storage(schema, batches, limits, plan, PlannedStorage::Direct)
}

fn compile_zstd_plan(
    schema: &SchemaDescriptor,
    batches: &[AuraV3Batch],
    limits: V3FlatLimits,
    plan: FlatAuraPlanV2,
    base_candidate_id: &'static str,
) -> Result<V3PlannedFlatArtifact> {
    compile_plan_storage(
        schema,
        batches,
        limits,
        plan,
        PlannedStorage::Zstd19 { base_candidate_id },
    )
}

fn compile_temporal_zstd_plan(
    schema: &SchemaDescriptor,
    batches: &[AuraV3Batch],
    limits: V3FlatLimits,
    plan: FlatAuraPlanV2,
    base_candidate_id: &'static str,
    base_registry_version: u16,
) -> Result<V3PlannedFlatArtifact> {
    compile_plan_storage(
        schema,
        batches,
        limits,
        plan,
        PlannedStorage::TemporalZstd19 {
            base_candidate_id,
            base_registry_version,
        },
    )
}

fn compile_prefix_suffix_zstd_plan(
    schema: &SchemaDescriptor,
    batches: &[AuraV3Batch],
    limits: V3FlatLimits,
    plan: FlatAuraPlanV2,
    inherited_complete_candidate_id: &'static str,
    inherited_complete_registry_version: u16,
) -> Result<V3PlannedFlatArtifact> {
    compile_plan_storage(
        schema,
        batches,
        limits,
        plan,
        PlannedStorage::PrefixSuffixZstd19 {
            inherited_complete_candidate_id,
            inherited_complete_registry_version,
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlannedStorage {
    Direct,
    Zstd19 {
        base_candidate_id: &'static str,
    },
    TemporalZstd19 {
        base_candidate_id: &'static str,
        base_registry_version: u16,
    },
    PrefixSuffixZstd19 {
        inherited_complete_candidate_id: &'static str,
        inherited_complete_registry_version: u16,
    },
}

fn compile_plan_storage(
    schema: &SchemaDescriptor,
    batches: &[AuraV3Batch],
    limits: V3FlatLimits,
    plan: FlatAuraPlanV2,
    storage: PlannedStorage,
) -> Result<V3PlannedFlatArtifact> {
    let limits = limits.effective();
    if storage == PlannedStorage::Direct
        && plan.registry_version == FLAT_PLAN_V2_PREFIX_SUFFIX_REGISTRY_VERSION
    {
        return Err(AuraError::InvalidValue(
            "planned flat prefix suffix raw storage",
        ));
    }
    let structural = V3ValueLimits {
        max_block_bytes: MAX_V3_VALUE_BLOCK_BYTES,
        ..limits.value_limits
    };
    let header = canonical_header(schema)?;
    let header_bytes = header.encode()?;
    let mut body = Vec::new();
    let mut chunks = Vec::new();
    let mut first_row = 0u64;
    let (inner_body_layout_version, inner_block_version, _) = planned_body_versions(&plan)?;
    let (body_layout_version, block_version) = match storage {
        PlannedStorage::Direct => (inner_body_layout_version, inner_block_version),
        PlannedStorage::Zstd19 { .. } => (
            V3_PLANNED_FLAT_ZSTD_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_ZSTD_BLOCK_VERSION,
        ),
        PlannedStorage::TemporalZstd19 { .. } => (
            V3_PLANNED_FLAT_TEMPORAL_ZSTD_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_TEMPORAL_ZSTD_BLOCK_VERSION,
        ),
        PlannedStorage::PrefixSuffixZstd19 { .. } => (
            V3_PLANNED_FLAT_PREFIX_SUFFIX_ZSTD_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_PREFIX_SUFFIX_ZSTD_BLOCK_VERSION,
        ),
    };
    let mut inner_body_bytes = 0u64;
    let mut compressed_payload_bytes = 0u64;
    let total_rows = batches
        .iter()
        .map(|batch| u64::from(batch.row_count))
        .sum::<u64>();
    let mut global = CanonicalV3RowHasher::new(schema, total_rows as u32, structural)?;
    for (index, batch) in batches.iter().enumerate() {
        let inner_block = encode_block(schema, batch, &plan, limits.value_limits)?;
        inner_body_bytes = inner_body_bytes
            .checked_add(
                u64::try_from(inner_block.len())
                    .map_err(|_| AuraError::InvalidValue("planned flat zstd inner body length"))?,
            )
            .ok_or(AuraError::InvalidValue(
                "planned flat zstd inner body length",
            ))?;
        let block = match storage {
            PlannedStorage::Direct => inner_block,
            PlannedStorage::Zstd19 { .. }
            | PlannedStorage::TemporalZstd19 { .. }
            | PlannedStorage::PrefixSuffixZstd19 { .. } => {
                let wrapper = match storage {
                    PlannedStorage::TemporalZstd19 { .. } => encode_temporal_zstd_wrapper(
                        &inner_block,
                        inner_body_layout_version,
                        inner_block_version,
                        limits.value_limits,
                    )?,
                    PlannedStorage::PrefixSuffixZstd19 { .. } => encode_prefix_suffix_zstd_wrapper(
                        &inner_block,
                        inner_body_layout_version,
                        inner_block_version,
                        limits.value_limits,
                    )?,
                    PlannedStorage::Zstd19 { .. } => encode_zstd_wrapper(
                        &inner_block,
                        inner_body_layout_version,
                        inner_block_version,
                        limits.value_limits,
                    )?,
                    PlannedStorage::Direct => unreachable!(),
                };
                compressed_payload_bytes = compressed_payload_bytes
                    .checked_add(
                        u64::try_from(wrapper.len() - ZSTD_WRAPPER_HEADER_BYTES).map_err(|_| {
                            AuraError::InvalidValue("planned flat zstd compressed length")
                        })?,
                    )
                    .ok_or(AuraError::InvalidValue(
                        "planned flat zstd compressed length",
                    ))?;
                wrapper
            }
        };
        let next = body
            .len()
            .checked_add(block.len())
            .filter(|len| *len as u64 <= limits.max_body_bytes)
            .ok_or(AuraError::InvalidValue("planned flat body length"))?;
        chunks.push(V3PlannedFlatChunkDescriptor {
            chunk_id: index as u32,
            first_global_row: first_row,
            row_count: batch.row_count,
            body_relative_offset: body.len() as u64,
            stored_len: block.len() as u64,
            stored_sha256: Sha256::digest(&block).into(),
            chunk_logical_sha256: canonical_v3_batch_sha256(schema, batch, structural)?,
        });
        body.extend_from_slice(&block);
        debug_assert_eq!(body.len(), next);
        first_row += u64::from(batch.row_count);
        global.update_batch(schema, batch)?;
    }
    let footer = V3PlannedFlatFooter {
        record_count: total_rows,
        body_len: body.len() as u64,
        body_layout_version,
        block_version,
        schema: schema.clone(),
        plan: plan.clone(),
        schema_fingerprint: canonical_v3_schema_fingerprint(schema)?,
        plan_sha256: plan.hash(schema)?,
        header_sha256: domain_hash(HEADER_HASH_DOMAIN, &header_bytes)?,
        body_sha256: domain_hash(BODY_HASH_DOMAIN, &body)?,
        global_logical_sha256: global.finalize()?,
        chunks,
    };
    let mut artifact = seal(header_bytes, body, footer, limits)?;
    if storage != PlannedStorage::Direct {
        let wrapper_overhead_bytes = u64::try_from(batches.len())
            .ok()
            .and_then(|chunks| chunks.checked_mul(ZSTD_WRAPPER_HEADER_BYTES as u64))
            .ok_or(AuraError::InvalidValue("planned flat zstd wrapper length"))?;
        match storage {
            PlannedStorage::Zstd19 { base_candidate_id } => {
                artifact.inspection.zstd_candidate = Some(V3PlannedFlatZstdInspection {
                    base_candidate_id: base_candidate_id.to_owned(),
                    base_registry_version: plan.registry_version,
                    inner_body_layout_version,
                    inner_block_version,
                    inner_body_bytes,
                    compressed_payload_bytes,
                    wrapper_overhead_bytes,
                    stored_body_bytes: artifact.summary.body_bytes,
                });
            }
            PlannedStorage::TemporalZstd19 {
                base_candidate_id,
                base_registry_version,
            } => {
                artifact.inspection.temporal_zstd_candidate = Some(V3PlannedFlatZstdInspection {
                    base_candidate_id: base_candidate_id.to_owned(),
                    base_registry_version,
                    inner_body_layout_version,
                    inner_block_version,
                    inner_body_bytes,
                    compressed_payload_bytes,
                    wrapper_overhead_bytes,
                    stored_body_bytes: artifact.summary.body_bytes,
                });
            }
            PlannedStorage::PrefixSuffixZstd19 {
                inherited_complete_candidate_id,
                inherited_complete_registry_version,
            } => {
                artifact.inspection.prefix_suffix_zstd_candidate =
                    Some(V3PlannedFlatPrefixSuffixZstdInspection {
                        inherited_complete_candidate_id: inherited_complete_candidate_id.to_owned(),
                        inherited_complete_registry_version,
                        derived_raw_plan_id: PREFIX_SUFFIX_DERIVED_RAW_PLAN_ID.to_owned(),
                        derived_raw_plan_registry_version: plan.registry_version,
                        inner_body_layout_version,
                        inner_block_version,
                        inner_body_bytes,
                        compressed_payload_bytes,
                        wrapper_overhead_bytes,
                        stored_body_bytes: artifact.summary.body_bytes,
                    });
            }
            PlannedStorage::Direct => unreachable!(),
        }
    }
    Ok(artifact)
}

#[derive(Debug)]
struct BoundedZstdWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: Rc<Cell<bool>>,
}

impl BoundedZstdWriter {
    fn new(limit: usize, reserve: usize, exceeded: Rc<Cell<bool>>) -> Result<Self> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(reserve.min(limit))
            .map_err(|_| AuraError::InvalidValue("planned flat zstd allocation"))?;
        Ok(Self {
            bytes,
            limit,
            exceeded,
        })
    }
}

impl Write for BoundedZstdWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(buffer.len())
            .is_none_or(|length| length > self.limit)
        {
            self.exceeded.set(true);
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "planned flat zstd output bound",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_zstd_wrapper(
    inner_block: &[u8],
    inner_body_layout_version: u16,
    inner_block_version: u16,
    limits: V3ValueLimits,
) -> Result<Vec<u8>> {
    encode_zstd_wrapper_profile(
        inner_block,
        inner_body_layout_version,
        inner_block_version,
        limits,
        ZstdWrapperKind::V1,
    )
}

fn encode_temporal_zstd_wrapper(
    inner_block: &[u8],
    inner_body_layout_version: u16,
    inner_block_version: u16,
    limits: V3ValueLimits,
) -> Result<Vec<u8>> {
    encode_zstd_wrapper_profile(
        inner_block,
        inner_body_layout_version,
        inner_block_version,
        limits,
        ZstdWrapperKind::V2,
    )
}

fn encode_prefix_suffix_zstd_wrapper(
    inner_block: &[u8],
    inner_body_layout_version: u16,
    inner_block_version: u16,
    limits: V3ValueLimits,
) -> Result<Vec<u8>> {
    encode_zstd_wrapper_profile(
        inner_block,
        inner_body_layout_version,
        inner_block_version,
        limits,
        ZstdWrapperKind::V3,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZstdWrapperKind {
    V1,
    V2,
    V3,
}

impl ZstdWrapperKind {
    const fn magic(self) -> &'static [u8; 8] {
        match self {
            Self::V1 => ZSTD_WRAPPER_MAGIC,
            Self::V2 => TEMPORAL_ZSTD_WRAPPER_MAGIC,
            Self::V3 => PREFIX_SUFFIX_ZSTD_WRAPPER_MAGIC,
        }
    }

    const fn version(self) -> u16 {
        match self {
            Self::V1 => V3_PLANNED_FLAT_ZSTD_WRAPPER_VERSION,
            Self::V2 => V3_PLANNED_FLAT_TEMPORAL_ZSTD_WRAPPER_VERSION,
            Self::V3 => V3_PLANNED_FLAT_PREFIX_SUFFIX_ZSTD_WRAPPER_VERSION,
        }
    }
}

fn encode_zstd_wrapper_profile(
    inner_block: &[u8],
    inner_body_layout_version: u16,
    inner_block_version: u16,
    limits: V3ValueLimits,
    wrapper_kind: ZstdWrapperKind,
) -> Result<Vec<u8>> {
    let inner_len = u64::try_from(inner_block.len())
        .map_err(|_| AuraError::InvalidValue("planned flat zstd inner length"))?;
    if inner_block.len() > limits.max_block_bytes {
        return Err(AuraError::InvalidValue("planned flat zstd inner length"));
    }
    let payload_limit = limits
        .max_block_bytes
        .checked_sub(ZSTD_WRAPPER_HEADER_BYTES)
        .ok_or(AuraError::InvalidValue("planned flat zstd wrapper length"))?;
    let compress_bound = zstd::zstd_safe::compress_bound(inner_block.len());
    let exceeded = Rc::new(Cell::new(false));
    let writer = BoundedZstdWriter::new(payload_limit, compress_bound, Rc::clone(&exceeded))?;
    let mut encoder = zstd::stream::write::Encoder::new(writer, V3_PLANNED_FLAT_ZSTD_LEVEL)
        .map_err(|_| AuraError::InvalidValue("planned flat zstd encoder"))?;
    encoder
        .set_pledged_src_size(Some(inner_len))
        .and_then(|_| encoder.include_contentsize(true))
        .and_then(|_| encoder.include_checksum(true))
        .and_then(|_| encoder.include_dictid(false))
        .and_then(|_| encoder.long_distance_matching(false))
        .and_then(|_| encoder.window_log(u32::from(V3_PLANNED_FLAT_ZSTD_WINDOW_LOG)))
        .and_then(|_| encoder.set_parameter(zstd::zstd_safe::CParameter::NbWorkers(0)))
        .and_then(|_| encoder.set_target_cblock_size(None))
        .map_err(|_| AuraError::InvalidValue("planned flat zstd encoder profile"))?;
    encoder.write_all(inner_block).map_err(|_| {
        if exceeded.get() {
            AuraError::InvalidValue("planned flat zstd block length")
        } else {
            AuraError::InvalidValue("planned flat zstd frame")
        }
    })?;
    let compressed = encoder
        .finish()
        .map_err(|_| {
            if exceeded.get() {
                AuraError::InvalidValue("planned flat zstd block length")
            } else {
                AuraError::InvalidValue("planned flat zstd frame")
            }
        })?
        .bytes;
    let compressed_len = u64::try_from(compressed.len())
        .map_err(|_| AuraError::InvalidValue("planned flat zstd compressed length"))?;
    let wrapper_len = ZSTD_WRAPPER_HEADER_BYTES
        .checked_add(compressed.len())
        .filter(|length| *length <= limits.max_block_bytes)
        .ok_or(AuraError::InvalidValue("planned flat zstd wrapper length"))?;
    let mut wrapper = Vec::new();
    wrapper
        .try_reserve_exact(wrapper_len)
        .map_err(|_| AuraError::InvalidValue("planned flat zstd allocation"))?;
    wrapper.extend_from_slice(wrapper_kind.magic());
    wrapper.extend_from_slice(&wrapper_kind.version().to_le_bytes());
    wrapper.push(ZSTD_CODEC_ID);
    wrapper.push(V3_PLANNED_FLAT_ZSTD_LEVEL as u8);
    wrapper.push(V3_PLANNED_FLAT_ZSTD_WINDOW_LOG);
    wrapper.push(ZSTD_WRAPPER_FLAGS);
    wrapper.extend_from_slice(&0u16.to_le_bytes());
    wrapper.extend_from_slice(&inner_body_layout_version.to_le_bytes());
    wrapper.extend_from_slice(&inner_block_version.to_le_bytes());
    wrapper.extend_from_slice(&inner_len.to_le_bytes());
    wrapper.extend_from_slice(&compressed_len.to_le_bytes());
    let inner_sha256: [u8; 32] = Sha256::digest(inner_block).into();
    wrapper.extend_from_slice(&inner_sha256);
    debug_assert_eq!(wrapper.len(), ZSTD_WRAPPER_HEADER_BYTES);
    wrapper.extend_from_slice(&compressed);
    Ok(wrapper)
}

fn decode_zstd_wrapper(
    wrapper: &[u8],
    plan: &FlatAuraPlanV2,
    limits: V3ValueLimits,
) -> Result<Vec<u8>> {
    decode_zstd_wrapper_profile(wrapper, plan, limits, ZstdWrapperKind::V1)
}

fn decode_temporal_zstd_wrapper(
    wrapper: &[u8],
    plan: &FlatAuraPlanV2,
    limits: V3ValueLimits,
) -> Result<Vec<u8>> {
    decode_zstd_wrapper_profile(wrapper, plan, limits, ZstdWrapperKind::V2)
}

fn decode_prefix_suffix_zstd_wrapper(
    wrapper: &[u8],
    plan: &FlatAuraPlanV2,
    limits: V3ValueLimits,
) -> Result<Vec<u8>> {
    decode_zstd_wrapper_profile(wrapper, plan, limits, ZstdWrapperKind::V3)
}

fn decode_zstd_wrapper_profile(
    wrapper: &[u8],
    plan: &FlatAuraPlanV2,
    limits: V3ValueLimits,
    wrapper_kind: ZstdWrapperKind,
) -> Result<Vec<u8>> {
    if wrapper.len() < ZSTD_WRAPPER_HEADER_BYTES || wrapper.len() > limits.max_block_bytes {
        return Err(AuraError::InvalidValue("planned flat zstd wrapper length"));
    }
    let mut reader = ByteReader::new(wrapper);
    if reader.read_exact(8)? != wrapper_kind.magic()
        || reader.read_u16_le()? != wrapper_kind.version()
        || reader.read_u8()? != ZSTD_CODEC_ID
        || reader.read_u8()? != V3_PLANNED_FLAT_ZSTD_LEVEL as u8
        || reader.read_u8()? != V3_PLANNED_FLAT_ZSTD_WINDOW_LOG
        || reader.read_u8()? != ZSTD_WRAPPER_FLAGS
        || reader.read_u16_le()? != 0
    {
        return Err(AuraError::InvalidValue("planned flat zstd wrapper header"));
    }
    let inner_body_layout_version = reader.read_u16_le()?;
    let inner_block_version = reader.read_u16_le()?;
    let (expected_inner_layout, expected_inner_block, _) = planned_body_versions(plan)?;
    if inner_body_layout_version != expected_inner_layout
        || inner_block_version != expected_inner_block
    {
        return Err(AuraError::InvalidValue("planned flat zstd inner versions"));
    }
    let inner_len_u64 = reader.read_u64_le()?;
    let compressed_len_u64 = reader.read_u64_le()?;
    let inner_sha256: [u8; 32] = reader.read_exact(32)?.try_into().unwrap();
    let inner_len = usize::try_from(inner_len_u64)
        .map_err(|_| AuraError::InvalidValue("planned flat zstd inner length"))?;
    let compressed_len = usize::try_from(compressed_len_u64)
        .map_err(|_| AuraError::InvalidValue("planned flat zstd compressed length"))?;
    if inner_len > limits.max_block_bytes
        || ZSTD_WRAPPER_HEADER_BYTES
            .checked_add(compressed_len)
            .is_none_or(|length| length != wrapper.len())
    {
        return Err(AuraError::InvalidValue("planned flat zstd wrapper length"));
    }
    let compressed = reader.read_exact(compressed_len)?;
    reader.finish()?;
    if !compressed.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        return Err(AuraError::InvalidValue("planned flat zstd frame magic"));
    }
    let frame_len = zstd::zstd_safe::find_frame_compressed_size(compressed)
        .map_err(|_| AuraError::InvalidValue("planned flat zstd frame"))?;
    if frame_len != compressed.len() {
        return Err(AuraError::InvalidValue("planned flat zstd frame length"));
    }
    match zstd::zstd_safe::get_frame_content_size(compressed) {
        Ok(Some(content_size)) if content_size == inner_len_u64 => {}
        _ => {
            return Err(AuraError::InvalidValue(
                "planned flat zstd frame content size",
            ))
        }
    }
    let mut inner = Vec::new();
    inner
        .try_reserve_exact(
            inner_len
                .checked_add(1)
                .ok_or(AuraError::InvalidValue("planned flat zstd inner length"))?,
        )
        .map_err(|_| AuraError::InvalidValue("planned flat zstd allocation"))?;
    let mut decoder = zstd::stream::read::Decoder::new(Cursor::new(compressed))
        .map_err(|_| AuraError::InvalidValue("planned flat zstd frame"))?;
    decoder
        .window_log_max(u32::from(V3_PLANNED_FLAT_ZSTD_WINDOW_LOG))
        .map_err(|_| AuraError::InvalidValue("planned flat zstd window"))?;
    decoder
        .take(inner_len_u64.saturating_add(1))
        .read_to_end(&mut inner)
        .map_err(|_| AuraError::InvalidValue("planned flat zstd frame"))?;
    if inner.len() != inner_len {
        return Err(AuraError::InvalidValue("planned flat zstd output length"));
    }
    if Sha256::digest(&inner).as_slice() != inner_sha256 {
        return Err(AuraError::InvalidValue("planned flat zstd inner hash"));
    }
    if encode_zstd_wrapper_profile(
        &inner,
        inner_body_layout_version,
        inner_block_version,
        limits,
        wrapper_kind,
    )? != wrapper
    {
        return Err(AuraError::InvalidValue("planned flat zstd noncanonical"));
    }
    Ok(inner)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PrefixSuffixLaneEncoding {
    bytes: Vec<u8>,
    control_bytes: u64,
    literal_bytes: u64,
    validity_bytes: u64,
    present_count: u64,
    null_count: u64,
    present_empty_count: u64,
}

fn common_prefix_suffix_lengths(previous: &[u8], value: &[u8]) -> (usize, usize) {
    let prefix = previous
        .iter()
        .zip(value)
        .take_while(|(left, right)| left == right)
        .count();
    let max_suffix = previous
        .len()
        .saturating_sub(prefix)
        .min(value.len().saturating_sub(prefix));
    let suffix = (0..max_suffix)
        .take_while(|offset| {
            previous[previous.len() - 1 - offset] == value[value.len() - 1 - offset]
        })
        .count();
    (prefix, suffix)
}

const fn prefix_suffix_uleb_len(mut value: usize) -> usize {
    let mut length = 1usize;
    while value >= 0x80 {
        value >>= 7;
        length += 1;
    }
    length
}

fn encode_prefix_suffix_lane(
    column: &AuraV3Column,
    rows: usize,
    limits: V3ValueLimits,
) -> Result<PrefixSuffixLaneEncoding> {
    let limits = limits.effective();
    let variable = match &column.values {
        AuraV3ColumnValues::Utf8(variable) | AuraV3ColumnValues::DecimalText(variable) => variable,
        _ => return Err(AuraError::InvalidValue("planned flat prefix suffix type")),
    };
    if variable.offsets.len()
        != rows.checked_add(1).ok_or(AuraError::InvalidValue(
            "planned flat prefix suffix offsets",
        ))?
        || variable.offsets.first() != Some(&0)
        || usize::try_from(*variable.offsets.last().unwrap_or(&0))
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix offsets"))?
            != variable.data.len()
    {
        return Err(AuraError::InvalidValue(
            "planned flat prefix suffix offsets",
        ));
    }
    let validity = column.validity.as_deref();
    if let Some(validity) = validity {
        if validity.len() != checked_validity_len(rows)? {
            return Err(AuraError::InvalidValue(
                "planned flat prefix suffix validity length",
            ));
        }
        validate_bitmap_padding(validity, rows)?;
    }
    let validity_len = validity.map_or(0, <[u8]>::len);
    let logical_output_len = validity_len
        .checked_add(
            rows.checked_add(1)
                .and_then(|count| count.checked_mul(4))
                .ok_or(AuraError::InvalidValue(
                    "planned flat prefix suffix lane length",
                ))?,
        )
        .and_then(|length| length.checked_add(variable.data.len()))
        .filter(|length| *length <= limits.max_block_bytes)
        .ok_or(AuraError::InvalidValue(
            "planned flat prefix suffix block length",
        ))?;
    let _ = logical_output_len;
    let mut previous = None::<&[u8]>;
    let mut present_count = 0u64;
    let mut present_empty_count = 0u64;
    let mut control_bytes = 0usize;
    let mut literal_bytes = 0usize;
    let mut records_len = 0usize;
    for row in 0..rows {
        if !is_present(validity, row) {
            continue;
        }
        let start = usize::try_from(variable.offsets[row])
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix offsets"))?;
        let end = usize::try_from(variable.offsets[row + 1])
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix offsets"))?;
        let value = variable
            .data
            .get(start..end)
            .ok_or(AuraError::InvalidValue(
                "planned flat prefix suffix offsets",
            ))?;
        if value.len() > limits.max_variable_value_bytes {
            return Err(AuraError::InvalidValue(
                "planned flat prefix suffix value length",
            ));
        }
        let previous_value = previous.unwrap_or(&[]);
        let (prefix, suffix) = common_prefix_suffix_lengths(previous_value, value);
        let middle_len = value
            .len()
            .checked_sub(prefix)
            .and_then(|length| length.checked_sub(suffix))
            .ok_or(AuraError::InvalidValue(
                "planned flat prefix suffix lengths",
            ))?;
        let record_control_bytes = prefix_suffix_uleb_len(prefix)
            .checked_add(prefix_suffix_uleb_len(suffix))
            .and_then(|length| length.checked_add(prefix_suffix_uleb_len(middle_len)))
            .ok_or(AuraError::InvalidValue(
                "planned flat prefix suffix lane length",
            ))?;
        control_bytes =
            control_bytes
                .checked_add(record_control_bytes)
                .ok_or(AuraError::InvalidValue(
                    "planned flat prefix suffix lane length",
                ))?;
        literal_bytes = literal_bytes
            .checked_add(middle_len)
            .ok_or(AuraError::InvalidValue(
                "planned flat prefix suffix lane length",
            ))?;
        records_len = records_len
            .checked_add(record_control_bytes)
            .and_then(|length| length.checked_add(middle_len))
            .ok_or(AuraError::InvalidValue(
                "planned flat prefix suffix lane length",
            ))?;
        present_count = present_count
            .checked_add(1)
            .ok_or(AuraError::InvalidValue("planned flat prefix suffix count"))?;
        if value.is_empty() {
            present_empty_count = present_empty_count
                .checked_add(1)
                .ok_or(AuraError::InvalidValue("planned flat prefix suffix count"))?;
        }
        previous = Some(value);
    }
    let total_len = validity_len
        .checked_add(PREFIX_SUFFIX_LANE_HEADER_BYTES)
        .and_then(|length| length.checked_add(records_len))
        .filter(|length| *length <= limits.max_block_bytes && records_len <= u32::MAX as usize)
        .ok_or(AuraError::InvalidValue(
            "planned flat prefix suffix block length",
        ))?;
    let mut records = Vec::<u8>::new();
    records
        .try_reserve_exact(records_len)
        .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix allocation"))?;
    previous = None;
    for row in 0..rows {
        if !is_present(validity, row) {
            continue;
        }
        let start = usize::try_from(variable.offsets[row])
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix offsets"))?;
        let end = usize::try_from(variable.offsets[row + 1])
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix offsets"))?;
        let value = &variable.data[start..end];
        let previous_value = previous.unwrap_or(&[]);
        let (prefix, suffix) = common_prefix_suffix_lengths(previous_value, value);
        let middle_len = value.len() - prefix - suffix;
        crate::varint::encode_u64(
            u64::try_from(prefix)
                .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix length"))?,
            &mut records,
        );
        crate::varint::encode_u64(
            u64::try_from(suffix)
                .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix length"))?,
            &mut records,
        );
        crate::varint::encode_u64(
            u64::try_from(middle_len)
                .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix length"))?,
            &mut records,
        );
        records.extend_from_slice(&value[prefix..prefix + middle_len]);
        previous = Some(value);
    }
    debug_assert_eq!(records.len(), records_len);
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(total_len)
        .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix allocation"))?;
    if let Some(validity) = validity {
        bytes.extend_from_slice(validity);
    }
    bytes.extend_from_slice(&PREFIX_SUFFIX_LANE_VERSION.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(
        &u32::try_from(rows)
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix row count"))?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(present_count)
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix count"))?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(records_len)
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix block length"))?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&records);
    debug_assert_eq!(bytes.len(), total_len);
    let row_count = u64::try_from(rows)
        .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix row count"))?;
    Ok(PrefixSuffixLaneEncoding {
        bytes,
        control_bytes: u64::try_from(control_bytes)
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix lane length"))?,
        literal_bytes: u64::try_from(literal_bytes)
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix lane length"))?,
        validity_bytes: u64::try_from(validity_len)
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix lane length"))?,
        present_count,
        null_count: row_count
            .checked_sub(present_count)
            .ok_or(AuraError::InvalidValue("planned flat prefix suffix count"))?,
        present_empty_count,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DictionaryLaneEncoding {
    bytes: Vec<u8>,
    present_count: u64,
    null_count: u64,
    present_empty_count: u64,
    entry_count: u32,
    unique_data_bytes: u64,
    index_bytes: u64,
}

fn encode_dictionary_lane(column: &AuraV3Column, rows: usize) -> Result<DictionaryLaneEncoding> {
    let variable = match &column.values {
        AuraV3ColumnValues::Utf8(variable) | AuraV3ColumnValues::DecimalText(variable) => variable,
        _ => return Err(AuraError::InvalidValue("planned flat dictionary type")),
    };
    let validity = column.validity.as_deref();
    let mut ranges = Vec::<(usize, usize)>::new();
    ranges
        .try_reserve_exact(rows)
        .map_err(|_| AuraError::InvalidValue("planned flat dictionary allocation"))?;
    let mut present_count = 0u64;
    let mut present_empty_count = 0u64;
    for row in 0..rows {
        if is_present(validity, row) {
            let start = usize::try_from(variable.offsets[row])
                .map_err(|_| AuraError::InvalidValue("planned flat dictionary offsets"))?;
            let end = usize::try_from(variable.offsets[row + 1])
                .map_err(|_| AuraError::InvalidValue("planned flat dictionary offsets"))?;
            let _ = variable
                .data
                .get(start..end)
                .ok_or(AuraError::InvalidValue("planned flat dictionary offsets"))?;
            present_count = present_count
                .checked_add(1)
                .ok_or(AuraError::InvalidValue("planned flat dictionary count"))?;
            if start == end {
                present_empty_count = present_empty_count
                    .checked_add(1)
                    .ok_or(AuraError::InvalidValue("planned flat dictionary count"))?;
            }
            ranges.push((start, end));
        }
    }
    ranges.sort_unstable_by(|left, right| {
        variable.data[left.0..left.1].cmp(&variable.data[right.0..right.1])
    });
    ranges.dedup_by(|left, right| variable.data[left.0..left.1] == variable.data[right.0..right.1]);
    let entry_count = u32::try_from(ranges.len())
        .map_err(|_| AuraError::InvalidValue("planned flat dictionary count"))?;
    if (present_count == 0) != (entry_count == 0) {
        return Err(AuraError::InvalidValue("planned flat dictionary count"));
    }
    let mut unique_data_len = 0usize;
    for (start, end) in &ranges {
        unique_data_len =
            unique_data_len
                .checked_add(end - start)
                .ok_or(AuraError::InvalidValue(
                    "planned flat dictionary data length",
                ))?;
    }
    let unique_data_len_u32 = u32::try_from(unique_data_len)
        .map_err(|_| AuraError::InvalidValue("planned flat dictionary data length"))?;
    let max_index = u64::from(entry_count.saturating_sub(1));
    let index_bit_width = unsigned_bitpack_width(max_index);
    let present_count_usize = usize::try_from(present_count)
        .map_err(|_| AuraError::InvalidValue("planned flat dictionary count"))?;
    let mut indexes = Vec::<u64>::new();
    indexes
        .try_reserve_exact(present_count_usize)
        .map_err(|_| AuraError::InvalidValue("planned flat dictionary allocation"))?;
    for row in 0..rows {
        if !is_present(validity, row) {
            continue;
        }
        let start = usize::try_from(variable.offsets[row])
            .map_err(|_| AuraError::InvalidValue("planned flat dictionary offsets"))?;
        let end = usize::try_from(variable.offsets[row + 1])
            .map_err(|_| AuraError::InvalidValue("planned flat dictionary offsets"))?;
        let value = &variable.data[start..end];
        let index = ranges
            .binary_search_by(|range| variable.data[range.0..range.1].cmp(value))
            .map_err(|_| AuraError::InvalidValue("planned flat dictionary inverse"))?;
        indexes.push(
            u64::try_from(index)
                .map_err(|_| AuraError::InvalidValue("planned flat dictionary index"))?,
        );
    }
    let packed_indexes = pack_unsigned_values(&indexes, index_bit_width)?;
    let offsets_len = ranges
        .len()
        .checked_add(1)
        .and_then(|count| count.checked_mul(4))
        .ok_or(AuraError::InvalidValue(
            "planned flat dictionary offsets length",
        ))?;
    let validity_len = column.validity.as_ref().map_or(0, Vec::len);
    let total_len = validity_len
        .checked_add(DICTIONARY_LANE_PREFIX_BYTES)
        .and_then(|length| length.checked_add(offsets_len))
        .and_then(|length| length.checked_add(unique_data_len))
        .and_then(|length| length.checked_add(packed_indexes.len()))
        .ok_or(AuraError::InvalidValue(
            "planned flat dictionary lane length",
        ))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(total_len)
        .map_err(|_| AuraError::InvalidValue("planned flat dictionary allocation"))?;
    if let Some(validity) = validity {
        bytes.extend_from_slice(validity);
    }
    bytes.extend_from_slice(&entry_count.to_le_bytes());
    bytes.extend_from_slice(&unique_data_len_u32.to_le_bytes());
    bytes.push(index_bit_width);
    bytes.extend_from_slice(&[0, 0, 0]);
    let mut offset = 0u32;
    bytes.extend_from_slice(&offset.to_le_bytes());
    for (start, end) in &ranges {
        offset = offset
            .checked_add(
                u32::try_from(end - start)
                    .map_err(|_| AuraError::InvalidValue("planned flat dictionary data length"))?,
            )
            .ok_or(AuraError::InvalidValue(
                "planned flat dictionary data length",
            ))?;
        bytes.extend_from_slice(&offset.to_le_bytes());
    }
    if offset != unique_data_len_u32 {
        return Err(AuraError::InvalidValue(
            "planned flat dictionary data length",
        ));
    }
    for (start, end) in &ranges {
        bytes.extend_from_slice(&variable.data[*start..*end]);
    }
    bytes.extend_from_slice(&packed_indexes);
    debug_assert_eq!(bytes.len(), total_len);
    let row_count = u64::try_from(rows)
        .map_err(|_| AuraError::InvalidValue("planned flat dictionary count"))?;
    Ok(DictionaryLaneEncoding {
        bytes,
        present_count,
        null_count: row_count
            .checked_sub(present_count)
            .ok_or(AuraError::InvalidValue("planned flat dictionary count"))?,
        present_empty_count,
        entry_count,
        unique_data_bytes: u64::from(unique_data_len_u32),
        index_bytes: u64::try_from(packed_indexes.len())
            .map_err(|_| AuraError::InvalidValue("planned flat dictionary index length"))?,
    })
}

fn encode_block(
    schema: &SchemaDescriptor,
    batch: &AuraV3Batch,
    plan: &FlatAuraPlanV2,
    limits: V3ValueLimits,
) -> Result<Vec<u8>> {
    validate_v3_batch(
        schema,
        batch,
        V3ValueLimits {
            max_block_bytes: MAX_V3_VALUE_BLOCK_BYTES,
            ..limits
        },
    )?;
    if plan.registry_version != FLAT_PLAN_V2_REGISTRY_VERSION {
        let mut logical_output_bytes = BLOCK_HEADER_BYTES;
        for column in &batch.columns {
            logical_output_bytes = logical_output_bytes
                .checked_add(decoded_lane_logical_bytes(
                    column,
                    usize::try_from(batch.row_count)
                        .map_err(|_| AuraError::InvalidValue("planned flat row count"))?,
                )?)
                .filter(|length| *length <= limits.max_block_bytes)
                .ok_or(AuraError::InvalidValue(
                    "planned flat logical output length",
                ))?;
        }
    }
    let (body_layout_version, block_version, block_magic) = planned_body_versions(plan)?;
    let _ = body_layout_version;
    let mut out = Vec::new();
    out.extend_from_slice(block_magic);
    out.extend_from_slice(&block_version.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&schema.schema_id.to_le_bytes());
    out.extend_from_slice(&canonical_v3_schema_fingerprint(schema)?);
    out.extend_from_slice(&batch.row_count.to_le_bytes());
    out.extend_from_slice(&(schema.fields.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    for column in &batch.columns {
        encode_lane(
            column,
            batch.row_count as usize,
            plan.codecs[column.slot as usize],
            limits,
            &mut out,
        )?;
    }
    let len = out.len() as u64;
    out[56..64].copy_from_slice(&len.to_le_bytes());
    if out.len() > limits.max_block_bytes {
        return Err(AuraError::InvalidValue("planned flat block length"));
    }
    Ok(out)
}

fn encode_lane(
    column: &AuraV3Column,
    rows: usize,
    codec: PlanV2PhysicalCodec,
    limits: V3ValueLimits,
    out: &mut Vec<u8>,
) -> Result<()> {
    if matches!(
        codec,
        PlanV2PhysicalCodec::TimestampPreviousDeltaZigZagUleb128
            | PlanV2PhysicalCodec::TimestampDeltaOfDeltaZigZagUleb128
    ) {
        return encode_temporal_lane(column, codec, out);
    }
    if codec == PlanV2PhysicalCodec::PreviousCommonPrefixSuffixBytes {
        out.extend_from_slice(&encode_prefix_suffix_lane(column, rows, limits)?.bytes);
        return Ok(());
    }
    if codec == PlanV2PhysicalCodec::VariableByteDictionaryBitpacked {
        out.extend_from_slice(&encode_dictionary_lane(column, rows)?.bytes);
        return Ok(());
    }
    if codec == PlanV2PhysicalCodec::FixedWidth {
        let mut encoded = Vec::new();
        encode_column(column, rows, &mut encoded)?;
        out.extend_from_slice(&encoded[20..]);
        return Ok(());
    }
    if let Some(validity) = &column.validity {
        out.extend_from_slice(validity);
    }
    macro_rules! unsigned {
        ($values:expr) => {
            for value in $values {
                crate::varint::encode_u64((*value).into(), out);
            }
        };
    }
    macro_rules! signed {
        ($values:expr) => {
            for value in $values {
                crate::varint::encode_i64((*value).into(), out);
            }
        };
    }
    match (&column.values, codec) {
        (AuraV3ColumnValues::U8(v), PlanV2PhysicalCodec::UnsignedUleb128) => unsigned!(v),
        (AuraV3ColumnValues::U16(v), PlanV2PhysicalCodec::UnsignedUleb128) => unsigned!(v),
        (AuraV3ColumnValues::U32(v), PlanV2PhysicalCodec::UnsignedUleb128) => unsigned!(v),
        (AuraV3ColumnValues::U64(v), PlanV2PhysicalCodec::UnsignedUleb128) => {
            for value in v {
                crate::varint::encode_u64(*value, out);
            }
        }
        (AuraV3ColumnValues::I8(v), PlanV2PhysicalCodec::SignedZigZagUleb128) => signed!(v),
        (AuraV3ColumnValues::I16(v), PlanV2PhysicalCodec::SignedZigZagUleb128) => signed!(v),
        (AuraV3ColumnValues::I32(v), PlanV2PhysicalCodec::SignedZigZagUleb128) => signed!(v),
        (
            AuraV3ColumnValues::I64(v)
            | AuraV3ColumnValues::TimestampNs(v)
            | AuraV3ColumnValues::TimestampMs(v),
            PlanV2PhysicalCodec::SignedZigZagUleb128,
        ) => {
            for value in v {
                crate::varint::encode_i64(*value, out);
            }
        }
        _ => return Err(AuraError::InvalidValue("planned flat codec type")),
    }
    Ok(())
}

fn encode_temporal_lane(
    column: &AuraV3Column,
    codec: PlanV2PhysicalCodec,
    out: &mut Vec<u8>,
) -> Result<()> {
    if column.validity.is_some() {
        return Err(AuraError::InvalidValue("planned flat temporal nullable"));
    }
    let values = match &column.values {
        AuraV3ColumnValues::TimestampNs(values) | AuraV3ColumnValues::TimestampMs(values) => values,
        _ => return Err(AuraError::InvalidValue("planned flat temporal type")),
    };
    let Some(first) = values.first().copied() else {
        return Ok(());
    };
    crate::varint::encode_i64(first, out);
    let mut previous_value = first;
    let mut previous_delta = None::<i64>;
    for value in values.iter().copied().skip(1) {
        let delta = i64::try_from(i128::from(value) - i128::from(previous_value))
            .map_err(|_| AuraError::InvalidValue("planned flat temporal overflow"))?;
        match codec {
            PlanV2PhysicalCodec::TimestampPreviousDeltaZigZagUleb128 => {
                crate::varint::encode_i64(delta, out);
            }
            PlanV2PhysicalCodec::TimestampDeltaOfDeltaZigZagUleb128 => {
                if let Some(previous_delta) = previous_delta {
                    let delta2 = i64::try_from(i128::from(delta) - i128::from(previous_delta))
                        .map_err(|_| AuraError::InvalidValue("planned flat temporal overflow"))?;
                    crate::varint::encode_i64(delta2, out);
                } else {
                    crate::varint::encode_i64(delta, out);
                }
            }
            _ => return Err(AuraError::InvalidValue("planned flat temporal codec")),
        }
        previous_value = value;
        previous_delta = Some(delta);
    }
    Ok(())
}

fn planned_body_versions(plan: &FlatAuraPlanV2) -> Result<(u16, u16, &'static [u8; 8])> {
    match plan.registry_version {
        FLAT_PLAN_V2_REGISTRY_VERSION => Ok((
            V3_PLANNED_FLAT_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_BLOCK_VERSION,
            BLOCK_MAGIC_V1,
        )),
        FLAT_PLAN_V2_DICTIONARY_REGISTRY_VERSION => Ok((
            V3_PLANNED_FLAT_DICTIONARY_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_DICTIONARY_BLOCK_VERSION,
            BLOCK_MAGIC_V2,
        )),
        FLAT_PLAN_V2_TEMPORAL_REGISTRY_VERSION => Ok((
            V3_PLANNED_FLAT_TEMPORAL_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_TEMPORAL_BLOCK_VERSION,
            BLOCK_MAGIC_V3,
        )),
        FLAT_PLAN_V2_PREFIX_SUFFIX_REGISTRY_VERSION => Ok((
            V3_PLANNED_FLAT_PREFIX_SUFFIX_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_PREFIX_SUFFIX_BLOCK_VERSION,
            BLOCK_MAGIC_V4,
        )),
        _ => Err(AuraError::InvalidValue("planned flat plan registry")),
    }
}

fn validate_stored_body_versions(
    plan: &FlatAuraPlanV2,
    body_layout_version: u16,
    block_version: u16,
) -> Result<()> {
    let (inner_body_layout_version, inner_block_version, _) = planned_body_versions(plan)?;
    let valid = match plan.registry_version {
        FLAT_PLAN_V2_TEMPORAL_REGISTRY_VERSION => {
            body_layout_version == V3_PLANNED_FLAT_TEMPORAL_ZSTD_BODY_LAYOUT_VERSION
                && block_version == V3_PLANNED_FLAT_TEMPORAL_ZSTD_BLOCK_VERSION
        }
        FLAT_PLAN_V2_PREFIX_SUFFIX_REGISTRY_VERSION => {
            body_layout_version == V3_PLANNED_FLAT_PREFIX_SUFFIX_ZSTD_BODY_LAYOUT_VERSION
                && block_version == V3_PLANNED_FLAT_PREFIX_SUFFIX_ZSTD_BLOCK_VERSION
        }
        _ => {
            (body_layout_version == inner_body_layout_version
                && block_version == inner_block_version)
                || (body_layout_version == V3_PLANNED_FLAT_ZSTD_BODY_LAYOUT_VERSION
                    && block_version == V3_PLANNED_FLAT_ZSTD_BLOCK_VERSION)
        }
    };
    if valid {
        Ok(())
    } else {
        Err(AuraError::InvalidValue("planned flat versions"))
    }
}

fn decode_block(
    schema: &SchemaDescriptor,
    bytes: &[u8],
    plan: &FlatAuraPlanV2,
    limits: V3ValueLimits,
) -> Result<AuraV3Batch> {
    let limits = limits.effective();
    let (_, expected_block_version, expected_magic) = planned_body_versions(plan)?;
    if bytes.len() > limits.max_block_bytes || bytes.len() < BLOCK_HEADER_BYTES {
        return Err(AuraError::InvalidValue("planned flat block length"));
    }
    let mut reader = ByteReader::new(bytes);
    if reader.read_exact(8)? != expected_magic
        || reader.read_u16_le()? != expected_block_version
        || reader.read_u16_le()? != 0
        || reader.read_u32_le()? != schema.schema_id
        || reader.read_exact(32)? != canonical_v3_schema_fingerprint(schema)?
    {
        return Err(AuraError::InvalidValue("planned flat block header"));
    }
    let rows = reader.read_u32_le()?;
    let rows_usize =
        usize::try_from(rows).map_err(|_| AuraError::InvalidValue("planned flat row count"))?;
    if rows_usize > limits.max_rows {
        return Err(AuraError::InvalidValue("planned flat row count"));
    }
    let declared_fields = reader.read_u16_le()?;
    let reserved = reader.read_u16_le()?;
    let stored_len = reader.read_u64_le()?;
    if usize::from(declared_fields) != schema.fields.len()
        || reserved != 0
        || stored_len
            != u64::try_from(bytes.len())
                .map_err(|_| AuraError::InvalidValue("planned flat block length"))?
    {
        return Err(AuraError::InvalidValue("planned flat block layout"));
    }
    let mut columns = Vec::new();
    columns
        .try_reserve_exact(schema.fields.len())
        .map_err(|_| AuraError::InvalidValue("planned flat column allocation"))?;
    let mut remaining_logical_bytes = if plan.registry_version != FLAT_PLAN_V2_REGISTRY_VERSION {
        Some(
            limits
                .max_block_bytes
                .checked_sub(BLOCK_HEADER_BYTES)
                .ok_or(AuraError::InvalidValue(
                    "planned flat logical output length",
                ))?,
        )
    } else {
        None
    };
    for field in &schema.fields {
        let codec = *plan
            .codecs
            .get(usize::from(field.index))
            .ok_or(AuraError::InvalidValue("planned flat codec slot"))?;
        let column = decode_lane(
            FlatLaneDecodeSpec {
                slot: field.index,
                field_type: field.field_type,
                nullable: field.nullable,
                rows: rows_usize,
                codec,
                logical_budget: remaining_logical_bytes.unwrap_or(usize::MAX),
            },
            &mut reader,
            limits,
        )?;
        if let Some(remaining) = &mut remaining_logical_bytes {
            let logical_bytes = decoded_lane_logical_bytes(&column, rows_usize)?;
            *remaining = remaining
                .checked_sub(logical_bytes)
                .ok_or(AuraError::InvalidValue(
                    "planned flat logical output length",
                ))?;
        }
        columns.push(column);
    }
    reader.finish()?;
    let batch = AuraV3Batch {
        schema_id: schema.schema_id,
        row_count: rows,
        columns,
    };
    validate_v3_batch(
        schema,
        &batch,
        V3ValueLimits {
            max_block_bytes: MAX_V3_VALUE_BLOCK_BYTES,
            ..limits
        },
    )?;
    if plan.registry_version != FLAT_PLAN_V2_REGISTRY_VERSION
        && encode_block(schema, &batch, plan, limits)? != bytes
    {
        return Err(AuraError::InvalidValue("planned flat noncanonical"));
    }
    Ok(batch)
}

#[derive(Debug, Clone, Copy)]
struct FlatLaneDecodeSpec {
    slot: u16,
    field_type: crate::FieldType,
    nullable: bool,
    rows: usize,
    codec: PlanV2PhysicalCodec,
    logical_budget: usize,
}

fn decode_lane(
    spec: FlatLaneDecodeSpec,
    reader: &mut ByteReader<'_>,
    limits: V3ValueLimits,
) -> Result<AuraV3Column> {
    let FlatLaneDecodeSpec {
        slot,
        field_type,
        nullable,
        rows,
        codec,
        logical_budget,
    } = spec;
    if matches!(
        codec,
        PlanV2PhysicalCodec::TimestampPreviousDeltaZigZagUleb128
            | PlanV2PhysicalCodec::TimestampDeltaOfDeltaZigZagUleb128
    ) {
        return decode_temporal_lane(
            slot,
            field_type,
            nullable,
            rows,
            codec,
            reader,
            logical_budget,
        );
    }
    if codec == PlanV2PhysicalCodec::PreviousCommonPrefixSuffixBytes {
        return decode_prefix_suffix_lane(
            slot,
            field_type,
            nullable,
            rows,
            reader,
            limits,
            logical_budget,
        );
    }
    if codec == PlanV2PhysicalCodec::VariableByteDictionaryBitpacked {
        return decode_dictionary_lane(
            slot,
            field_type,
            nullable,
            rows,
            reader,
            limits,
            logical_budget,
        );
    }
    if codec == PlanV2PhysicalCodec::FixedWidth {
        return decode_fixed_lane(
            slot,
            field_type,
            nullable,
            rows,
            reader,
            limits,
            logical_budget,
        );
    }
    let validity = if nullable {
        let len = checked_validity_len(rows)?;
        Some(reader.read_exact(len)?.to_vec())
    } else {
        None
    };
    let logical_validity_len = if nullable {
        checked_validity_len(rows)?
    } else {
        0
    };
    let logical_bytes = logical_validity_len
        .checked_add(
            rows.checked_mul(
                fixed_width(field_type)
                    .ok_or(AuraError::InvalidValue("planned flat fixed width"))?,
            )
            .ok_or(AuraError::InvalidValue(
                "planned flat logical output length",
            ))?,
        )
        .filter(|length| *length <= logical_budget)
        .ok_or(AuraError::InvalidValue(
            "planned flat logical output length",
        ))?;
    let _ = logical_bytes;
    macro_rules! u {
        ($variant:ident,$type:ty) => {{
            let mut v = Vec::new();
            v.try_reserve_exact(rows)
                .map_err(|_| AuraError::InvalidValue("planned flat lane allocation"))?;
            for _ in 0..rows {
                v.push(
                    <$type>::try_from(decode_canonical_uleb128(reader)?)
                        .map_err(|_| AuraError::InvalidValue("planned flat varint range"))?,
                );
            }
            AuraV3ColumnValues::$variant(v)
        }};
    }
    macro_rules! i {
        ($variant:ident,$type:ty) => {{
            let mut v = Vec::new();
            v.try_reserve_exact(rows)
                .map_err(|_| AuraError::InvalidValue("planned flat lane allocation"))?;
            for _ in 0..rows {
                v.push(
                    <$type>::try_from(decode_canonical_zigzag(reader)?)
                        .map_err(|_| AuraError::InvalidValue("planned flat varint range"))?,
                );
            }
            AuraV3ColumnValues::$variant(v)
        }};
    }
    let values = match (field_type, codec) {
        (crate::FieldType::U8, PlanV2PhysicalCodec::UnsignedUleb128) => u!(U8, u8),
        (crate::FieldType::U16, PlanV2PhysicalCodec::UnsignedUleb128) => u!(U16, u16),
        (crate::FieldType::U32, PlanV2PhysicalCodec::UnsignedUleb128) => u!(U32, u32),
        (crate::FieldType::U64, PlanV2PhysicalCodec::UnsignedUleb128) => u!(U64, u64),
        (crate::FieldType::I8, PlanV2PhysicalCodec::SignedZigZagUleb128) => i!(I8, i8),
        (crate::FieldType::I16, PlanV2PhysicalCodec::SignedZigZagUleb128) => i!(I16, i16),
        (crate::FieldType::I32, PlanV2PhysicalCodec::SignedZigZagUleb128) => i!(I32, i32),
        (crate::FieldType::I64, PlanV2PhysicalCodec::SignedZigZagUleb128) => i!(I64, i64),
        (crate::FieldType::TimestampNs, PlanV2PhysicalCodec::SignedZigZagUleb128) => {
            i!(TimestampNs, i64)
        }
        (crate::FieldType::TimestampMs, PlanV2PhysicalCodec::SignedZigZagUleb128) => {
            i!(TimestampMs, i64)
        }
        _ => return Err(AuraError::InvalidValue("planned flat codec type")),
    };
    Ok(AuraV3Column {
        slot,
        validity,
        values,
    })
}

fn decode_temporal_lane(
    slot: u16,
    field_type: crate::FieldType,
    nullable: bool,
    rows: usize,
    codec: PlanV2PhysicalCodec,
    reader: &mut ByteReader<'_>,
    logical_budget: usize,
) -> Result<AuraV3Column> {
    if nullable
        || !matches!(
            field_type,
            crate::FieldType::TimestampNs | crate::FieldType::TimestampMs
        )
        || rows
            .checked_mul(8)
            .is_none_or(|length| length > logical_budget)
    {
        return Err(AuraError::InvalidValue("planned flat temporal type"));
    }
    let mut values = Vec::<i64>::new();
    values
        .try_reserve_exact(rows)
        .map_err(|_| AuraError::InvalidValue("planned flat temporal allocation"))?;
    if rows != 0 {
        let first = decode_canonical_zigzag(reader)?;
        values.push(first);
        let mut previous_value = first;
        let mut previous_delta = None::<i64>;
        for _ in 1..rows {
            let encoded = decode_canonical_zigzag(reader)?;
            let delta = match codec {
                PlanV2PhysicalCodec::TimestampPreviousDeltaZigZagUleb128 => encoded,
                PlanV2PhysicalCodec::TimestampDeltaOfDeltaZigZagUleb128 => {
                    if let Some(previous_delta) = previous_delta {
                        i64::try_from(i128::from(previous_delta) + i128::from(encoded)).map_err(
                            |_| AuraError::InvalidValue("planned flat temporal overflow"),
                        )?
                    } else {
                        encoded
                    }
                }
                _ => return Err(AuraError::InvalidValue("planned flat temporal codec")),
            };
            let value = i64::try_from(i128::from(previous_value) + i128::from(delta))
                .map_err(|_| AuraError::InvalidValue("planned flat temporal overflow"))?;
            values.push(value);
            previous_value = value;
            previous_delta = Some(delta);
        }
    }
    let values = match field_type {
        crate::FieldType::TimestampNs => AuraV3ColumnValues::TimestampNs(values),
        crate::FieldType::TimestampMs => AuraV3ColumnValues::TimestampMs(values),
        _ => return Err(AuraError::InvalidValue("planned flat temporal type")),
    };
    Ok(AuraV3Column {
        slot,
        validity: None,
        values,
    })
}

fn decode_prefix_suffix_lane(
    slot: u16,
    field_type: crate::FieldType,
    nullable: bool,
    rows: usize,
    reader: &mut ByteReader<'_>,
    limits: V3ValueLimits,
    logical_budget: usize,
) -> Result<AuraV3Column> {
    if !matches!(
        field_type,
        crate::FieldType::Utf8 | crate::FieldType::DecimalText
    ) {
        return Err(AuraError::InvalidValue("planned flat prefix suffix type"));
    }
    let validity = if nullable {
        let length = checked_validity_len(rows)?;
        let bytes = reader.read_exact(length)?.to_vec();
        validate_bitmap_padding(&bytes, rows)?;
        Some(bytes)
    } else {
        None
    };
    if reader.read_u16_le()? != PREFIX_SUFFIX_LANE_VERSION || reader.read_u16_le()? != 0 {
        return Err(AuraError::InvalidValue("planned flat prefix suffix header"));
    }
    let declared_rows = usize::try_from(reader.read_u32_le()?)
        .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix row count"))?;
    let declared_present = usize::try_from(reader.read_u32_le()?)
        .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix count"))?;
    let records_len = usize::try_from(reader.read_u32_le()?)
        .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix lane length"))?;
    let expected_present = (0..rows)
        .filter(|row| is_present(validity.as_deref(), *row))
        .count();
    if declared_rows != rows || declared_present != expected_present {
        return Err(AuraError::InvalidValue("planned flat prefix suffix count"));
    }
    let record_bytes = reader.read_exact(records_len)?;
    let mut records = ByteReader::new(record_bytes);
    let logical_validity_len = if nullable {
        checked_validity_len(rows)?
    } else {
        0
    };
    let logical_prefix_len = logical_validity_len
        .checked_add(
            rows.checked_add(1)
                .and_then(|count| count.checked_mul(4))
                .ok_or(AuraError::InvalidValue(
                    "planned flat logical output length",
                ))?,
        )
        .filter(|length| *length <= logical_budget)
        .ok_or(AuraError::InvalidValue(
            "planned flat logical output length",
        ))?;
    let max_data_len = logical_budget - logical_prefix_len;
    let mut offsets = Vec::<u32>::new();
    offsets
        .try_reserve_exact(rows.checked_add(1).ok_or(AuraError::InvalidValue(
            "planned flat prefix suffix offsets",
        ))?)
        .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix allocation"))?;
    let mut data = Vec::<u8>::new();
    let mut previous_range = None::<(usize, usize)>;
    offsets.push(0);
    for row in 0..rows {
        if !is_present(validity.as_deref(), row) {
            offsets.push(
                u32::try_from(data.len())
                    .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix offsets"))?,
            );
            continue;
        }
        let prefix = usize::try_from(decode_canonical_uleb128(&mut records)?)
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix length"))?;
        let suffix = usize::try_from(decode_canonical_uleb128(&mut records)?)
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix length"))?;
        let middle_len = usize::try_from(decode_canonical_uleb128(&mut records)?)
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix length"))?;
        let previous = previous_range.map_or(&[][..], |(start, end)| &data[start..end]);
        if prefix > previous.len() || suffix > previous.len().saturating_sub(prefix) {
            return Err(AuraError::InvalidValue(
                "planned flat prefix suffix lengths",
            ));
        }
        let current_len = prefix
            .checked_add(middle_len)
            .and_then(|length| length.checked_add(suffix))
            .filter(|length| *length <= limits.max_variable_value_bytes)
            .ok_or(AuraError::InvalidValue(
                "planned flat prefix suffix value length",
            ))?;
        let next_data_len = data
            .len()
            .checked_add(current_len)
            .filter(|length| *length <= max_data_len && *length <= u32::MAX as usize)
            .ok_or(AuraError::InvalidValue(
                "planned flat logical output length",
            ))?;
        let middle = records.read_exact(middle_len)?;
        let mut current = Vec::<u8>::new();
        current
            .try_reserve_exact(current_len)
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix allocation"))?;
        current.extend_from_slice(&previous[..prefix]);
        current.extend_from_slice(middle);
        current.extend_from_slice(&previous[previous.len() - suffix..]);
        let (canonical_prefix, canonical_suffix) = common_prefix_suffix_lengths(previous, &current);
        if prefix != canonical_prefix || suffix != canonical_suffix {
            return Err(AuraError::InvalidValue(
                "planned flat prefix suffix noncanonical",
            ));
        }
        let text = std::str::from_utf8(&current)
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix utf8"))?;
        if field_type == crate::FieldType::DecimalText {
            validate_decimal_text_v1(text)?;
        }
        let start = data.len();
        data.try_reserve_exact(current_len)
            .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix allocation"))?;
        data.extend_from_slice(&current);
        debug_assert_eq!(data.len(), next_data_len);
        previous_range = Some((start, next_data_len));
        offsets.push(
            u32::try_from(next_data_len)
                .map_err(|_| AuraError::InvalidValue("planned flat prefix suffix offsets"))?,
        );
    }
    records.finish()?;
    let variable = AuraV3VariableColumn { offsets, data };
    let values = match field_type {
        crate::FieldType::Utf8 => AuraV3ColumnValues::Utf8(variable),
        crate::FieldType::DecimalText => AuraV3ColumnValues::DecimalText(variable),
        _ => return Err(AuraError::InvalidValue("planned flat prefix suffix type")),
    };
    Ok(AuraV3Column {
        slot,
        validity,
        values,
    })
}

fn decode_dictionary_lane(
    slot: u16,
    field_type: crate::FieldType,
    nullable: bool,
    rows: usize,
    reader: &mut ByteReader<'_>,
    limits: V3ValueLimits,
    logical_budget: usize,
) -> Result<AuraV3Column> {
    if !matches!(
        field_type,
        crate::FieldType::Utf8 | crate::FieldType::DecimalText
    ) {
        return Err(AuraError::InvalidValue("planned flat dictionary type"));
    }
    let validity = if nullable {
        let length = checked_validity_len(rows)?;
        let bytes = reader.read_exact(length)?.to_vec();
        validate_bitmap_padding(&bytes, rows)?;
        Some(bytes)
    } else {
        None
    };
    let entry_count_u32 = reader.read_u32_le()?;
    let entry_count = usize::try_from(entry_count_u32)
        .map_err(|_| AuraError::InvalidValue("planned flat dictionary count"))?;
    let dictionary_data_len = usize::try_from(reader.read_u32_le()?)
        .map_err(|_| AuraError::InvalidValue("planned flat dictionary data length"))?;
    let index_bit_width = reader.read_u8()?;
    if reader.read_exact(3)? != [0, 0, 0] {
        return Err(AuraError::InvalidValue("planned flat dictionary reserved"));
    }
    let present_count = (0..rows)
        .filter(|row| is_present(validity.as_deref(), *row))
        .count();
    if entry_count > present_count || (present_count == 0) != (entry_count == 0) {
        return Err(AuraError::InvalidValue("planned flat dictionary count"));
    }
    let expected_width = unsigned_bitpack_width(u64::from(entry_count_u32.saturating_sub(1)));
    if index_bit_width != expected_width {
        return Err(AuraError::InvalidValue(
            "planned flat dictionary index width",
        ));
    }
    let offsets_count = entry_count.checked_add(1).ok_or(AuraError::InvalidValue(
        "planned flat dictionary offsets length",
    ))?;
    let offsets_len = offsets_count.checked_mul(4).ok_or(AuraError::InvalidValue(
        "planned flat dictionary offsets length",
    ))?;
    let offsets_bytes = reader.read_exact(offsets_len)?;
    let mut dictionary_offsets = Vec::<u32>::new();
    dictionary_offsets
        .try_reserve_exact(offsets_count)
        .map_err(|_| AuraError::InvalidValue("planned flat dictionary allocation"))?;
    for chunk in offsets_bytes.chunks_exact(4) {
        dictionary_offsets.push(u32::from_le_bytes(
            chunk
                .try_into()
                .map_err(|_| AuraError::InvalidValue("planned flat dictionary offsets"))?,
        ));
    }
    if dictionary_offsets.first() != Some(&0)
        || dictionary_offsets.last().copied()
            != Some(
                u32::try_from(dictionary_data_len)
                    .map_err(|_| AuraError::InvalidValue("planned flat dictionary data length"))?,
            )
    {
        return Err(AuraError::InvalidValue("planned flat dictionary offsets"));
    }
    let dictionary_data = reader.read_exact(dictionary_data_len)?;
    let mut previous: Option<&[u8]> = None;
    for pair in dictionary_offsets.windows(2) {
        let start = usize::try_from(pair[0])
            .map_err(|_| AuraError::InvalidValue("planned flat dictionary offsets"))?;
        let end = usize::try_from(pair[1])
            .map_err(|_| AuraError::InvalidValue("planned flat dictionary offsets"))?;
        let value = dictionary_data
            .get(start..end)
            .filter(|value| value.len() <= limits.max_variable_value_bytes)
            .ok_or(AuraError::InvalidValue("planned flat dictionary offsets"))?;
        if previous.is_some_and(|previous| previous >= value) {
            return Err(AuraError::InvalidValue("planned flat dictionary order"));
        }
        let text = std::str::from_utf8(value)
            .map_err(|_| AuraError::InvalidValue("planned flat dictionary utf8"))?;
        if field_type == crate::FieldType::DecimalText {
            validate_decimal_text_v1(text)?;
        }
        previous = Some(value);
    }
    let index_bytes_len = usize::try_from(bitpacked_byte_len(
        u64::try_from(present_count)
            .map_err(|_| AuraError::InvalidValue("planned flat dictionary count"))?,
        index_bit_width,
    ))
    .map_err(|_| AuraError::InvalidValue("planned flat dictionary index length"))?;
    let index_bytes = reader.read_exact(index_bytes_len)?;
    validate_zero_bitpack_padding(index_bytes, present_count, index_bit_width)?;
    let indexes = unpack_unsigned_values(index_bytes, index_bit_width, present_count)?;
    let mut referenced = Vec::<bool>::new();
    referenced
        .try_reserve_exact(entry_count)
        .map_err(|_| AuraError::InvalidValue("planned flat dictionary allocation"))?;
    referenced.resize(entry_count, false);
    for index in &indexes {
        let index = usize::try_from(*index)
            .map_err(|_| AuraError::InvalidValue("planned flat dictionary index"))?;
        let referenced = referenced
            .get_mut(index)
            .ok_or(AuraError::InvalidValue("planned flat dictionary index"))?;
        *referenced = true;
    }
    if referenced.iter().any(|referenced| !referenced) {
        return Err(AuraError::InvalidValue(
            "planned flat dictionary unreferenced entry",
        ));
    }
    let logical_validity_len = if nullable {
        checked_validity_len(rows)?
    } else {
        0
    };
    let logical_prefix_len = logical_validity_len
        .checked_add(
            rows.checked_add(1)
                .and_then(|count| count.checked_mul(4))
                .ok_or(AuraError::InvalidValue(
                    "planned flat logical output length",
                ))?,
        )
        .filter(|length| *length <= logical_budget)
        .ok_or(AuraError::InvalidValue(
            "planned flat logical output length",
        ))?;
    let max_reconstructed_len = logical_budget - logical_prefix_len;
    let mut reconstructed_len = 0usize;
    for index in &indexes {
        let index = usize::try_from(*index)
            .map_err(|_| AuraError::InvalidValue("planned flat dictionary index"))?;
        let start = usize::try_from(dictionary_offsets[index])
            .map_err(|_| AuraError::InvalidValue("planned flat dictionary offsets"))?;
        let end = usize::try_from(dictionary_offsets[index + 1])
            .map_err(|_| AuraError::InvalidValue("planned flat dictionary offsets"))?;
        reconstructed_len = reconstructed_len
            .checked_add(end - start)
            .filter(|length| *length <= max_reconstructed_len)
            .ok_or(AuraError::InvalidValue(
                "planned flat dictionary inverse length",
            ))?;
    }
    let mut offsets = Vec::<u32>::new();
    offsets
        .try_reserve_exact(
            rows.checked_add(1)
                .ok_or(AuraError::InvalidValue("planned flat dictionary offsets"))?,
        )
        .map_err(|_| AuraError::InvalidValue("planned flat dictionary allocation"))?;
    let mut data = Vec::<u8>::new();
    data.try_reserve_exact(reconstructed_len)
        .map_err(|_| AuraError::InvalidValue("planned flat dictionary allocation"))?;
    offsets.push(0);
    let mut indexes = indexes.into_iter();
    for row in 0..rows {
        if is_present(validity.as_deref(), row) {
            let index = usize::try_from(
                indexes
                    .next()
                    .ok_or(AuraError::InvalidValue("planned flat dictionary index"))?,
            )
            .map_err(|_| AuraError::InvalidValue("planned flat dictionary index"))?;
            let start = usize::try_from(dictionary_offsets[index])
                .map_err(|_| AuraError::InvalidValue("planned flat dictionary offsets"))?;
            let end = usize::try_from(dictionary_offsets[index + 1])
                .map_err(|_| AuraError::InvalidValue("planned flat dictionary offsets"))?;
            data.extend_from_slice(&dictionary_data[start..end]);
        }
        offsets.push(
            u32::try_from(data.len())
                .map_err(|_| AuraError::InvalidValue("planned flat dictionary inverse length"))?,
        );
    }
    if indexes.next().is_some() {
        return Err(AuraError::InvalidValue("planned flat dictionary index"));
    }
    let variable = AuraV3VariableColumn { offsets, data };
    let values = match field_type {
        crate::FieldType::Utf8 => AuraV3ColumnValues::Utf8(variable),
        crate::FieldType::DecimalText => AuraV3ColumnValues::DecimalText(variable),
        _ => return Err(AuraError::InvalidValue("planned flat dictionary type")),
    };
    Ok(AuraV3Column {
        slot,
        validity,
        values,
    })
}

fn validate_zero_bitpack_padding(bytes: &[u8], count: usize, width: u8) -> Result<()> {
    let used = u64::try_from(count)
        .ok()
        .and_then(|count| count.checked_mul(u64::from(width)))
        .ok_or(AuraError::InvalidValue(
            "planned flat dictionary index length",
        ))?
        % 8;
    if used != 0 {
        let mask = !((1u8 << used) - 1);
        if bytes.last().is_some_and(|byte| byte & mask != 0) {
            return Err(AuraError::InvalidValue(
                "planned flat dictionary index padding",
            ));
        }
    }
    Ok(())
}

fn decode_fixed_lane(
    slot: u16,
    field_type: crate::FieldType,
    nullable: bool,
    rows: usize,
    reader: &mut ByteReader<'_>,
    limits: V3ValueLimits,
    logical_budget: usize,
) -> Result<AuraV3Column> {
    let validity_len = if nullable {
        checked_validity_len(rows)?
    } else {
        0
    };
    let width = fixed_width(field_type);
    let variable = width.is_none();
    let (fixed_len, offsets_len, data_len, payload) = if variable {
        let offsets_len = rows
            .checked_add(1)
            .and_then(|count| count.checked_mul(4))
            .ok_or(AuraError::InvalidValue("planned flat lane length"))?;
        let prefix_len = validity_len
            .checked_add(offsets_len)
            .ok_or(AuraError::InvalidValue("planned flat lane length"))?;
        let prefix = reader.read_exact(prefix_len)?;
        let final_offset_start = prefix
            .len()
            .checked_sub(4)
            .ok_or(AuraError::InvalidValue("planned flat offsets"))?;
        let final_offset = prefix
            .get(final_offset_start..)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .ok_or(AuraError::InvalidValue("planned flat offsets"))?;
        let data_len = usize::try_from(u32::from_le_bytes(final_offset))
            .map_err(|_| AuraError::InvalidValue("planned flat lane length"))?;
        if data_len > limits.max_variable_value_bytes {
            return Err(AuraError::InvalidValue("planned flat variable data length"));
        }
        if prefix_len
            .checked_add(data_len)
            .is_none_or(|length| length > logical_budget)
        {
            return Err(AuraError::InvalidValue(
                "planned flat logical output length",
            ));
        }
        let data = reader.read_exact(data_len)?;
        let payload_len = prefix_len
            .checked_add(data_len)
            .ok_or(AuraError::InvalidValue("planned flat lane length"))?;
        let mut payload = Vec::new();
        payload
            .try_reserve_exact(payload_len)
            .map_err(|_| AuraError::InvalidValue("planned flat lane allocation"))?;
        payload.extend_from_slice(prefix);
        payload.extend_from_slice(data);
        (0, offsets_len, data_len, payload)
    } else {
        let fixed_len = rows
            .checked_mul(width.ok_or(AuraError::InvalidValue("planned flat fixed width"))?)
            .ok_or(AuraError::InvalidValue("planned flat lane length"))?;
        let payload_len = validity_len
            .checked_add(fixed_len)
            .ok_or(AuraError::InvalidValue("planned flat lane length"))?;
        if payload_len > logical_budget {
            return Err(AuraError::InvalidValue(
                "planned flat logical output length",
            ));
        }
        (fixed_len, 0, 0, reader.read_exact(payload_len)?.to_vec())
    };
    let synthetic_len = 20usize
        .checked_add(payload.len())
        .ok_or(AuraError::InvalidValue("planned flat lane length"))?;
    let mut synthetic = Vec::new();
    synthetic
        .try_reserve_exact(synthetic_len)
        .map_err(|_| AuraError::InvalidValue("planned flat lane allocation"))?;
    synthetic.extend_from_slice(&slot.to_le_bytes());
    synthetic.push(field_type as u8);
    synthetic.push(u8::from(nullable) | (u8::from(variable) << 1));
    for len in [validity_len, fixed_len, offsets_len, data_len] {
        let len =
            u32::try_from(len).map_err(|_| AuraError::InvalidValue("planned flat lane length"))?;
        synthetic.extend_from_slice(&len.to_le_bytes());
    }
    synthetic.extend_from_slice(&payload);
    decode_column(
        slot,
        field_type,
        nullable,
        rows,
        &mut ByteReader::new(&synthetic),
        limits,
    )
}

fn decoded_lane_logical_bytes(column: &AuraV3Column, rows: usize) -> Result<usize> {
    let validity_len = column.validity.as_ref().map_or(0, Vec::len);
    let payload_len = match &column.values {
        AuraV3ColumnValues::Utf8(variable) | AuraV3ColumnValues::DecimalText(variable) => rows
            .checked_add(1)
            .and_then(|count| count.checked_mul(4))
            .and_then(|length| length.checked_add(variable.data.len())),
        values => fixed_width(values.field_type()).and_then(|width| rows.checked_mul(width)),
    }
    .ok_or(AuraError::InvalidValue(
        "planned flat logical output length",
    ))?;
    validity_len
        .checked_add(payload_len)
        .ok_or(AuraError::InvalidValue(
            "planned flat logical output length",
        ))
}

fn checked_validity_len(rows: usize) -> Result<usize> {
    rows.checked_add(7)
        .map(|bits| bits / 8)
        .ok_or(AuraError::InvalidValue("planned flat validity length"))
}

fn seal(
    header: Vec<u8>,
    body: Vec<u8>,
    footer: V3PlannedFlatFooter,
    limits: V3FlatLimits,
) -> Result<V3PlannedFlatArtifact> {
    let footer_bytes = encode_v3_planned_flat_footer(&footer, limits)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(&footer_bytes);
    bytes.extend_from_slice(&(footer_bytes.len() as u32).to_le_bytes());
    bytes.extend_from_slice(SEAL_MAGIC);
    let accounted = header.len() as u64 + footer.body_len + footer_bytes.len() as u64 + 12;
    if accounted != bytes.len() as u64 {
        return Err(AuraError::InvalidValue("planned flat accounting"));
    }
    Ok(V3PlannedFlatArtifact {
        summary: V3PlannedFlatSummary {
            row_count: footer.record_count,
            chunk_count: footer.chunks.len() as u32,
            header_bytes: header.len() as u64,
            body_bytes: footer.body_len,
            footer_bytes: footer_bytes.len() as u32,
            file_bytes: bytes.len() as u64,
            accounted_file_bytes: accounted,
            schema_fingerprint: footer.schema_fingerprint,
            plan_sha256: footer.plan_sha256,
            global_logical_sha256: footer.global_logical_sha256,
        },
        bytes,
        inspection: V3PlannedFlatInspection {
            candidates: Vec::new(),
            codecs: Vec::new(),
            dictionary_candidate_codecs: Vec::new(),
            zstd_candidate: None,
            temporal_zstd_candidate: None,
            temporal_candidate_codecs: Vec::new(),
            prefix_suffix_zstd_candidate: None,
            prefix_suffix_candidate_codecs: Vec::new(),
        },
    })
}

pub fn encode_v3_planned_flat_footer(
    footer: &V3PlannedFlatFooter,
    limits: V3FlatLimits,
) -> Result<Vec<u8>> {
    let limits = limits.effective();
    footer.plan.validate(&footer.schema)?;
    if footer.record_count > limits.max_rows
        || footer.body_len > limits.max_body_bytes
        || footer.chunks.len() > limits.max_chunks
    {
        return Err(AuraError::InvalidValue("planned flat footer limits"));
    }
    let schema = encode_schema_descriptor(&footer.schema)?;
    let plan = footer.plan.encode(&footer.schema)?;
    validate_stored_body_versions(
        &footer.plan,
        footer.body_layout_version,
        footer.block_version,
    )?;
    let len = 208usize
        .checked_add(schema.len())
        .and_then(|v| v.checked_add(plan.len()))
        .and_then(|v| v.checked_add(8 + footer.chunks.len() * 104 + 32))
        .filter(|v| {
            *v <= limits
                .max_footer_bytes
                .min(MAX_V3_PLANNED_FLAT_FOOTER_BYTES)
        })
        .ok_or(AuraError::InvalidValue("planned flat footer length"))?;
    let mut out = Vec::new();
    out.extend_from_slice(b"AURP");
    put16(&mut out, 3);
    put16(&mut out, 3);
    out.extend_from_slice(&[4, 0, 0, 0]);
    put64(&mut out, footer.record_count);
    put64(&mut out, footer.body_len);
    put32(&mut out, footer.chunks.len() as u32);
    put32(&mut out, footer.schema.schema_id);
    put16(&mut out, footer.body_layout_version);
    put16(&mut out, footer.block_version);
    put32(&mut out, schema.len() as u32);
    put32(&mut out, plan.len() as u32);
    for hash in [
        footer.schema_fingerprint,
        footer.plan_sha256,
        footer.header_sha256,
        footer.body_sha256,
        footer.global_logical_sha256,
    ] {
        out.extend_from_slice(&hash);
    }
    debug_assert_eq!(out.len(), 208);
    out.extend_from_slice(&schema);
    out.extend_from_slice(&plan);
    put16(&mut out, 1);
    put16(&mut out, 104);
    put32(&mut out, footer.chunks.len() as u32);
    let mut row = 0u64;
    let mut offset = 0u64;
    for (index, chunk) in footer.chunks.iter().enumerate() {
        if chunk.chunk_id as usize != index
            || chunk.row_count == 0
            || chunk.first_global_row != row
            || chunk.body_relative_offset != offset
            || chunk.stored_len == 0
            || chunk.stored_len > limits.value_limits.max_block_bytes as u64
        {
            return Err(AuraError::InvalidValue("planned flat chunk range"));
        }
        put32(&mut out, chunk.chunk_id);
        put32(&mut out, 0);
        put64(&mut out, chunk.first_global_row);
        put32(&mut out, chunk.row_count);
        put16(&mut out, footer.block_version);
        put16(&mut out, 0);
        put64(&mut out, chunk.body_relative_offset);
        put64(&mut out, chunk.stored_len);
        out.extend_from_slice(&chunk.stored_sha256);
        out.extend_from_slice(&chunk.chunk_logical_sha256);
        row += u64::from(chunk.row_count);
        offset += chunk.stored_len;
    }
    if row != footer.record_count || offset != footer.body_len {
        return Err(AuraError::InvalidValue("planned flat totals"));
    }
    let hash = domain_hash(FOOTER_HASH_DOMAIN, &out)?;
    out.extend_from_slice(&hash);
    debug_assert_eq!(out.len(), len);
    Ok(out)
}

pub fn decode_v3_planned_flat_footer(
    bytes: &[u8],
    limits: V3FlatLimits,
) -> Result<V3PlannedFlatFooter> {
    let limits = limits.effective();
    if bytes.len()
        > limits
            .max_footer_bytes
            .min(MAX_V3_PLANNED_FLAT_FOOTER_BYTES)
        || bytes.len() < 248
    {
        return Err(AuraError::InvalidValue("planned flat footer length"));
    }
    let hs = bytes.len() - 32;
    if domain_hash(FOOTER_HASH_DOMAIN, &bytes[..hs])? != bytes[hs..] {
        return Err(AuraError::InvalidValue("planned flat footer hash"));
    }
    let mut r = ByteReader::new(&bytes[..hs]);
    if r.read_exact(4)? != b"AURP"
        || r.read_u16_le() != Ok(3)
        || r.read_u16_le() != Ok(3)
        || r.read_exact(4)? != [4, 0, 0, 0]
    {
        return Err(AuraError::InvalidValue("planned flat footer tuple"));
    }
    let record_count = r.read_u64_le()?;
    let body_len = r.read_u64_le()?;
    let count = r.read_u32_le()? as usize;
    let schema_id = r.read_u32_le()?;
    let body_layout_version = r.read_u16_le()?;
    let block_version = r.read_u16_le()?;
    let sl = r.read_u32_le()? as usize;
    let pl = r.read_u32_le()? as usize;
    let schema_fingerprint = r.read_exact(32)?.try_into().unwrap();
    let plan_sha256 = r.read_exact(32)?.try_into().unwrap();
    let header_sha256 = r.read_exact(32)?.try_into().unwrap();
    let body_sha256 = r.read_exact(32)?.try_into().unwrap();
    let global_logical_sha256 = r.read_exact(32)?.try_into().unwrap();
    let schema = decode_schema_descriptor(r.read_exact(sl)?)?;
    if schema.schema_id != schema_id {
        return Err(AuraError::InvalidValue("planned flat schema"));
    }
    let plan = FlatAuraPlanV2::decode(&schema, r.read_exact(pl)?)?;
    validate_stored_body_versions(&plan, body_layout_version, block_version)?;
    if r.read_u16_le() != Ok(1)
        || r.read_u16_le() != Ok(104)
        || r.read_u32_le()? as usize != count
        || count > limits.max_chunks
        || count > r.remaining() / V3_PLANNED_FLAT_CHUNK_DESCRIPTOR_BYTES
    {
        return Err(AuraError::InvalidValue("planned flat chunks"));
    }
    let mut chunks = Vec::new();
    for _ in 0..count {
        let id = r.read_u32_le()?;
        if r.read_u32_le()? != 0 {
            return Err(AuraError::InvalidValue("planned flat flags"));
        }
        let first = r.read_u64_le()?;
        let rows = r.read_u32_le()?;
        if r.read_u16_le()? != block_version || r.read_u16_le()? != 0 {
            return Err(AuraError::InvalidValue("planned flat chunk version"));
        }
        let off = r.read_u64_le()?;
        let len = r.read_u64_le()?;
        let stored = r.read_exact(32)?.try_into().unwrap();
        let logical = r.read_exact(32)?.try_into().unwrap();
        chunks.push(V3PlannedFlatChunkDescriptor {
            chunk_id: id,
            first_global_row: first,
            row_count: rows,
            body_relative_offset: off,
            stored_len: len,
            stored_sha256: stored,
            chunk_logical_sha256: logical,
        });
    }
    r.finish()?;
    let footer = V3PlannedFlatFooter {
        record_count,
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
    if footer.schema_fingerprint != canonical_v3_schema_fingerprint(&footer.schema)?
        || footer.plan_sha256 != footer.plan.hash(&footer.schema)?
    {
        return Err(AuraError::InvalidValue("planned flat identity"));
    }
    if encode_v3_planned_flat_footer(&footer, limits)? != bytes {
        return Err(AuraError::InvalidValue("planned flat noncanonical"));
    }
    Ok(footer)
}

/// Decodes only the additive planned-flat `(layout=3, body=4)` container.
///
/// Compiler output may instead select the legacy exact flat tuple; callers
/// consuming [`compile_v3_planned_flat`] output should use
/// [`decode_v3_selected_flat`].
pub fn decode_v3_planned_flat(bytes: &[u8], limits: V3FlatLimits) -> Result<DecodedV3PlannedFlat> {
    let limits = limits.effective();
    if bytes.len() < crate::V3_HEADER_PREFIX_SIZE + 12 || &bytes[bytes.len() - 8..] != SEAL_MAGIC {
        return Err(AuraError::UnexpectedEof);
    }
    let flo = bytes.len() - 12;
    let fl = u32::from_le_bytes(bytes[flo..flo + 4].try_into().unwrap()) as usize;
    let fs = flo.checked_sub(fl).ok_or(AuraError::UnexpectedEof)?;
    let hl = AuraHeader::encoded_len(bytes)?;
    if hl > fs || hl > bytes.len() {
        return Err(AuraError::InvalidValue("planned flat file ranges"));
    }
    let header = AuraHeader::decode(&bytes[..hl])?;
    let footer = decode_v3_planned_flat_footer(&bytes[fs..flo], limits)?;
    if canonical_header(&footer.schema)?.encode()? != bytes[..hl] {
        return Err(AuraError::InvalidValue("planned flat canonical header"));
    }
    let body = &bytes[hl..fs];
    if footer.body_len
        != u64::try_from(body.len())
            .map_err(|_| AuraError::InvalidValue("planned flat body length"))?
        || footer.header_sha256 != domain_hash(HEADER_HASH_DOMAIN, &bytes[..hl])?
        || footer.body_sha256 != domain_hash(BODY_HASH_DOMAIN, body)?
    {
        return Err(AuraError::InvalidValue("planned flat envelope"));
    }
    let structural = V3ValueLimits {
        max_block_bytes: MAX_V3_VALUE_BLOCK_BYTES,
        ..limits.value_limits
    };
    let total_rows = u32::try_from(footer.record_count)
        .map_err(|_| AuraError::InvalidValue("planned flat row count"))?;
    let mut global = CanonicalV3RowHasher::new(&footer.schema, total_rows, structural)?;
    let mut batches = Vec::new();
    for chunk in &footer.chunks {
        let s = usize::try_from(chunk.body_relative_offset)
            .map_err(|_| AuraError::InvalidValue("planned flat chunk range"))?;
        let stored_len = usize::try_from(chunk.stored_len)
            .map_err(|_| AuraError::InvalidValue("planned flat chunk range"))?;
        let e = s
            .checked_add(stored_len)
            .ok_or(AuraError::InvalidValue("planned flat chunk range"))?;
        let stored = body.get(s..e).ok_or(AuraError::UnexpectedEof)?;
        if Sha256::digest(stored).as_slice() != chunk.stored_sha256 {
            return Err(AuraError::InvalidValue("planned flat stored hash"));
        }
        let inner_block;
        let block = if footer.body_layout_version
            == V3_PLANNED_FLAT_PREFIX_SUFFIX_ZSTD_BODY_LAYOUT_VERSION
        {
            inner_block =
                decode_prefix_suffix_zstd_wrapper(stored, &footer.plan, limits.value_limits)?;
            inner_block.as_slice()
        } else if footer.body_layout_version == V3_PLANNED_FLAT_TEMPORAL_ZSTD_BODY_LAYOUT_VERSION {
            inner_block = decode_temporal_zstd_wrapper(stored, &footer.plan, limits.value_limits)?;
            inner_block.as_slice()
        } else if footer.body_layout_version == V3_PLANNED_FLAT_ZSTD_BODY_LAYOUT_VERSION {
            inner_block = decode_zstd_wrapper(stored, &footer.plan, limits.value_limits)?;
            inner_block.as_slice()
        } else {
            stored
        };
        let batch = decode_block(&footer.schema, block, &footer.plan, limits.value_limits)?;
        if batch.row_count != chunk.row_count
            || canonical_v3_batch_sha256(&footer.schema, &batch, structural)?
                != chunk.chunk_logical_sha256
        {
            return Err(AuraError::InvalidValue("planned flat chunk identity"));
        }
        global.update_batch(&footer.schema, &batch)?;
        batches.push(batch);
    }
    if global.finalize() != Ok(footer.global_logical_sha256) {
        return Err(AuraError::InvalidValue("planned flat logical hash"));
    }
    let summary = V3PlannedFlatSummary {
        row_count: footer.record_count,
        chunk_count: footer.chunks.len() as u32,
        header_bytes: hl as u64,
        body_bytes: footer.body_len,
        footer_bytes: fl as u32,
        file_bytes: bytes.len() as u64,
        accounted_file_bytes: bytes.len() as u64,
        schema_fingerprint: footer.schema_fingerprint,
        plan_sha256: footer.plan_sha256,
        global_logical_sha256: footer.global_logical_sha256,
    };
    Ok(DecodedV3PlannedFlat {
        header,
        footer,
        batches,
        summary,
    })
}

/// Decodes whichever flat container was selected by
/// [`compile_v3_planned_flat`], preserving the exact legacy fallback.
pub fn decode_v3_selected_flat(
    bytes: &[u8],
    limits: V3FlatLimits,
) -> Result<DecodedV3SelectedFlat> {
    if bytes.len() < 12 || bytes.get(bytes.len() - 8..) != Some(SEAL_MAGIC.as_slice()) {
        return Err(AuraError::UnexpectedEof);
    }
    let footer_len_offset = bytes
        .len()
        .checked_sub(12)
        .ok_or(AuraError::UnexpectedEof)?;
    let footer_len_bytes = bytes
        .get(footer_len_offset..footer_len_offset + 4)
        .and_then(|slice| <[u8; 4]>::try_from(slice).ok())
        .ok_or(AuraError::UnexpectedEof)?;
    let footer_len = usize::try_from(u32::from_le_bytes(footer_len_bytes))
        .map_err(|_| AuraError::InvalidValue("selected flat footer length"))?;
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    let footer = bytes
        .get(footer_start..footer_len_offset)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer.len() < 9 || &footer[..4] != b"AURP" {
        return Err(AuraError::InvalidValue("selected flat footer tuple"));
    }
    if u16::from_le_bytes([footer[4], footer[5]]) != AuraContainerVersion::V3.wire_value() {
        return Err(AuraError::InvalidValue("selected flat footer tuple"));
    }
    let layout = u16::from_le_bytes([footer[6], footer[7]]);
    match (layout, footer[8]) {
        (V3_FLAT_FOOTER_LAYOUT_VERSION, V3_FLAT_BODY_ENCODING_EXACT_BLOCKS) => {
            decode_v3_flat_aura0_with_limits(bytes, limits)
                .map(Box::new)
                .map(DecodedV3SelectedFlat::Exact)
        }
        (V3_PLANNED_FLAT_FOOTER_LAYOUT_VERSION, V3_PLANNED_FLAT_BODY_ENCODING) => {
            decode_v3_planned_flat(bytes, limits)
                .map(Box::new)
                .map(DecodedV3SelectedFlat::Planned)
        }
        _ => Err(AuraError::InvalidValue("selected flat footer tuple")),
    }
}

fn candidate_inapplicable(error: &AuraError) -> bool {
    matches!(
        error,
        AuraError::InvalidValue(
            "v3 value block length"
                | "planned flat block length"
                | "v3 flat body length"
                | "planned flat body length"
                | "v3 flat footer length"
                | "planned flat footer length"
                | "planned flat dictionary no lane win"
                | "planned flat logical output length"
                | "planned flat zstd block length"
                | "planned flat zstd wrapper length"
                | "planned flat zstd compressed length"
                | "planned flat temporal no lane win"
                | "planned flat prefix suffix no lane win"
                | "planned flat prefix suffix block length"
        )
    )
}
fn canonical_header(schema: &SchemaDescriptor) -> Result<AuraHeader> {
    AuraHeader::new(Profile::Aura0)
        .with_container_version(AuraContainerVersion::V3)
        .with_schema_mapping(
            schema
                .compact_schema_map
                .clone()
                .ok_or(AuraError::InvalidValue("planned flat schema map"))?,
        )
}
fn domain_hash(domain: &[u8], bytes: &[u8]) -> Result<[u8; 32]> {
    let mut h = Sha256::new();
    h.update(domain);
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
    Ok(h.finalize().into())
}
fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[cfg(test)]
mod dictionary_tests {
    use super::*;
    use crate::{FieldRole, FieldTransform, FieldType, SchemaBuilder, TransformCandidates};

    fn variable(parts: &[&[u8]]) -> AuraV3VariableColumn {
        let mut offsets = vec![0u32];
        let mut data = Vec::new();
        for part in parts {
            data.extend_from_slice(part);
            offsets.push(u32::try_from(data.len()).unwrap());
        }
        AuraV3VariableColumn { offsets, data }
    }

    fn utf8_column(parts: &[&[u8]], validity: Option<Vec<u8>>) -> AuraV3Column {
        AuraV3Column {
            slot: 0,
            validity,
            values: AuraV3ColumnValues::Utf8(variable(parts)),
        }
    }

    fn decode_lane_bytes(
        bytes: &[u8],
        rows: usize,
        field_type: FieldType,
        nullable: bool,
        max_block_bytes: usize,
    ) -> Result<AuraV3Column> {
        let mut reader = ByteReader::new(bytes);
        let decoded = decode_dictionary_lane(
            0,
            field_type,
            nullable,
            rows,
            &mut reader,
            V3ValueLimits {
                max_block_bytes,
                ..V3ValueLimits::HARD
            },
            max_block_bytes,
        )?;
        reader.finish()?;
        Ok(decoded)
    }

    fn decode_prefix_lane_bytes(
        bytes: &[u8],
        rows: usize,
        field_type: FieldType,
        nullable: bool,
        limits: V3ValueLimits,
    ) -> Result<AuraV3Column> {
        let mut reader = ByteReader::new(bytes);
        let decoded = decode_prefix_suffix_lane(
            0,
            field_type,
            nullable,
            rows,
            &mut reader,
            limits,
            limits.max_block_bytes,
        )?;
        reader.finish()?;
        Ok(decoded)
    }

    fn prefix_lane_frame(
        validity: Option<&[u8]>,
        rows: u32,
        present: u32,
        records: &[u8],
    ) -> Vec<u8> {
        let mut bytes = validity.unwrap_or(&[]).to_vec();
        bytes.extend_from_slice(&PREFIX_SUFFIX_LANE_VERSION.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&rows.to_le_bytes());
        bytes.extend_from_slice(&present.to_le_bytes());
        bytes.extend_from_slice(&(records.len() as u32).to_le_bytes());
        bytes.extend_from_slice(records);
        bytes
    }

    #[test]
    fn prefix_suffix_lane_preserves_null_empty_and_all_present_validity_exactly() {
        let parts = [
            b"alpha-tail".as_slice(),
            b"",
            b"",
            b"alpine-tail",
            b"",
            b"alpine-sail",
        ];
        let column = utf8_column(&parts, Some(vec![0b0010_1111]));
        let encoded = encode_prefix_suffix_lane(&column, parts.len(), V3ValueLimits::HARD).unwrap();
        assert_eq!(encoded.present_count, 5);
        assert_eq!(encoded.null_count, 1);
        assert_eq!(encoded.present_empty_count, 2);
        assert_eq!(encoded.bytes[0], 0b0010_1111);
        assert_eq!(
            decode_prefix_lane_bytes(
                &encoded.bytes,
                parts.len(),
                FieldType::Utf8,
                true,
                V3ValueLimits::HARD,
            )
            .unwrap(),
            column
        );

        let all_present = utf8_column(&[b"first", b"second"], Some(vec![0b0000_0011]));
        let encoded = encode_prefix_suffix_lane(&all_present, 2, V3ValueLimits::HARD).unwrap();
        assert_eq!(
            encoded.bytes.len(),
            1 + PREFIX_SUFFIX_LANE_HEADER_BYTES + 17
        );
        assert_eq!(
            decode_prefix_lane_bytes(
                &encoded.bytes,
                2,
                FieldType::Utf8,
                true,
                V3ValueLimits::HARD,
            )
            .unwrap(),
            all_present
        );
    }

    #[test]
    fn prefix_suffix_lane_malformed_and_noncanonical_forms_fail_closed() {
        let valid = prefix_lane_frame(None, 1, 1, &[0, 0, 1, b'a']);
        assert!(
            decode_prefix_lane_bytes(&valid, 1, FieldType::Utf8, false, V3ValueLimits::HARD,)
                .is_ok()
        );

        let malformed = [
            prefix_lane_frame(None, 1, 1, &[0x80, 0, 0, 1, b'a']),
            prefix_lane_frame(None, 1, 1, &[1, 0, 0]),
            prefix_lane_frame(None, 1, 1, &[0, 0, 2, b'a']),
            prefix_lane_frame(None, 1, 1, &[0, 0, 1, b'a', 0]),
            prefix_lane_frame(None, 1, 1, &[0, 0, 1, 0xff]),
            prefix_lane_frame(None, 2, 2, &[0, 0, 1, b'a', 0, 1, 0]),
            prefix_lane_frame(None, 2, 2, &[0, 0, 1, b'a', 1, 1, 0]),
        ];
        for bytes in malformed {
            let rows = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
            assert!(decode_prefix_lane_bytes(
                &bytes,
                rows,
                FieldType::Utf8,
                false,
                V3ValueLimits::HARD,
            )
            .is_err());
        }

        let mut wrong_header = valid.clone();
        wrong_header[0..2].copy_from_slice(&2u16.to_le_bytes());
        assert!(decode_prefix_lane_bytes(
            &wrong_header,
            1,
            FieldType::Utf8,
            false,
            V3ValueLimits::HARD,
        )
        .is_err());
        for (range, value) in [
            (2..4, 1u32),
            (4..8, 2u32),
            (8..12, 0u32),
            (12..16, u32::MAX),
        ] {
            let mut malformed_header = valid.clone();
            let width = range.end - range.start;
            malformed_header[range].copy_from_slice(&value.to_le_bytes()[..width]);
            assert!(decode_prefix_lane_bytes(
                &malformed_header,
                1,
                FieldType::Utf8,
                false,
                V3ValueLimits::HARD,
            )
            .is_err());
        }
        let mut overflow = vec![0x80; 9];
        overflow.push(0x02);
        overflow.extend_from_slice(&[0, 0]);
        assert!(decode_prefix_lane_bytes(
            &prefix_lane_frame(None, 1, 1, &overflow),
            1,
            FieldType::Utf8,
            false,
            V3ValueLimits::HARD,
        )
        .is_err());
        assert!(decode_prefix_lane_bytes(
            &prefix_lane_frame(Some(&[0x81]), 1, 1, &[0, 0, 1, b'a']),
            1,
            FieldType::Utf8,
            true,
            V3ValueLimits::HARD,
        )
        .is_err());
        assert!(decode_prefix_lane_bytes(
            &valid,
            1,
            FieldType::DecimalText,
            false,
            V3ValueLimits::HARD,
        )
        .is_err());
        assert!(decode_prefix_lane_bytes(
            &valid,
            1,
            FieldType::Utf8,
            false,
            V3ValueLimits {
                max_block_bytes: 7,
                ..V3ValueLimits::HARD
            },
        )
        .is_err());
        assert!(decode_prefix_lane_bytes(
            &valid,
            1,
            FieldType::Utf8,
            false,
            V3ValueLimits {
                max_variable_value_bytes: 0,
                ..V3ValueLimits::HARD
            },
        )
        .is_err());
        let bounded_column = utf8_column(&[b"caller-bounded-value"], None);
        let caller_bound = V3ValueLimits {
            max_block_bytes: 16,
            ..V3ValueLimits::HARD
        };
        assert!(encode_prefix_suffix_lane(&bounded_column, 1, caller_bound).is_err());
        let batch = AuraV3Batch {
            schema_id: 0,
            row_count: 1,
            columns: vec![bounded_column],
        };
        assert!(prefix_suffix_lane_totals(&[batch], 0, caller_bound).is_err());
    }

    #[test]
    fn appended_complete_candidate_loses_exact_ties_to_existing_candidates() {
        assert_eq!(
            select_complete_candidate(&[
                Some(900),
                Some(800),
                Some(700),
                Some(600),
                Some(500),
                Some(400),
                Some(400),
            ]),
            Ok(5)
        );
        assert_eq!(
            select_complete_candidate(&[None, None, None, None, None, None, Some(400)]),
            Ok(6)
        );
    }

    #[test]
    fn dictionary_cardinality_widths_nulls_and_present_empty_are_canonical() {
        let cases = [
            (vec![b"".as_slice(), b"", b""], Some(vec![0]), 0, 0),
            (vec![b"a".as_slice(), b"a", b"a"], None, 1, 0),
            (vec![b"a".as_slice(), b"b", b"a"], None, 2, 1),
            (vec![b"a".as_slice(), b"b", b"c", b"d"], None, 4, 2),
            (vec![b"a".as_slice(), b"b", b"c", b"d", b"e"], None, 5, 3),
        ];
        for (parts, validity, expected_entries, expected_width) in cases {
            let column = utf8_column(&parts, validity);
            let encoded = encode_dictionary_lane(&column, parts.len()).unwrap();
            let validity_len = column.validity.as_ref().map_or(0, Vec::len);
            assert_eq!(
                u32::from_le_bytes(
                    encoded.bytes[validity_len..validity_len + 4]
                        .try_into()
                        .unwrap()
                ),
                expected_entries
            );
            assert_eq!(encoded.bytes[validity_len + 8], expected_width);
            assert_eq!(
                decode_lane_bytes(
                    &encoded.bytes,
                    parts.len(),
                    FieldType::Utf8,
                    column.validity.is_some(),
                    V3ValueLimits::HARD.max_block_bytes,
                )
                .unwrap(),
                column
            );
        }
        for (cardinality, expected_width) in [(8usize, 3u8), (9, 4), (256, 8), (257, 9)] {
            let owned = (0..cardinality)
                .map(|index| format!("value-{index:04}"))
                .collect::<Vec<_>>();
            let parts = owned
                .iter()
                .map(|value| value.as_bytes())
                .collect::<Vec<_>>();
            let column = utf8_column(&parts, None);
            let encoded = encode_dictionary_lane(&column, cardinality).unwrap();
            assert_eq!(
                u32::from_le_bytes(encoded.bytes[..4].try_into().unwrap()),
                cardinality as u32
            );
            assert_eq!(encoded.bytes[8], expected_width);
            assert_eq!(
                decode_lane_bytes(
                    &encoded.bytes,
                    cardinality,
                    FieldType::Utf8,
                    false,
                    V3ValueLimits::HARD.max_block_bytes,
                )
                .unwrap(),
                column
            );
        }
    }

    #[test]
    fn dictionary_lane_malformed_forms_fail_closed() {
        let column = utf8_column(&[b"a", b"b", b"a"], None);
        let encoded = encode_dictionary_lane(&column, 3).unwrap().bytes;
        assert_eq!(encoded[8], 1);
        let dictionary_data_start = 12 + 3 * 4;
        let index_start = dictionary_data_start + 2;

        let mut mutations = Vec::new();
        let mut bad_count = encoded.clone();
        bad_count[..4].copy_from_slice(&4u32.to_le_bytes());
        mutations.push(bad_count);
        let mut bad_reserved = encoded.clone();
        bad_reserved[9] = 1;
        mutations.push(bad_reserved);
        let mut bad_width = encoded.clone();
        bad_width[8] = 2;
        mutations.push(bad_width);
        let mut bad_offsets = encoded.clone();
        bad_offsets[16..20].copy_from_slice(&3u32.to_le_bytes());
        mutations.push(bad_offsets);
        let mut duplicate = encoded.clone();
        duplicate[dictionary_data_start + 1] = b'a';
        mutations.push(duplicate);
        let mut unsorted = encoded.clone();
        unsorted[dictionary_data_start] = b'b';
        unsorted[dictionary_data_start + 1] = b'a';
        mutations.push(unsorted);
        let mut invalid_utf8 = encoded.clone();
        invalid_utf8[dictionary_data_start] = 0xff;
        mutations.push(invalid_utf8);
        let mut bad_padding = encoded.clone();
        bad_padding[index_start] |= 0x80;
        mutations.push(bad_padding);
        for mutation in mutations {
            assert!(decode_lane_bytes(
                &mutation,
                3,
                FieldType::Utf8,
                false,
                V3ValueLimits::HARD.max_block_bytes,
            )
            .is_err());
        }
        for length in 0..encoded.len() {
            assert!(decode_lane_bytes(
                &encoded[..length],
                3,
                FieldType::Utf8,
                false,
                V3ValueLimits::HARD.max_block_bytes,
            )
            .is_err());
        }
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(decode_lane_bytes(
            &trailing,
            3,
            FieldType::Utf8,
            false,
            V3ValueLimits::HARD.max_block_bytes,
        )
        .is_err());

        let three = utf8_column(&[b"a", b"b", b"c"], None);
        let mut unreferenced = encode_dictionary_lane(&three, 3).unwrap().bytes;
        let three_data_start = 12 + 4 * 4;
        let three_index_start = three_data_start + 3;
        unreferenced[three_index_start] = 0b0001_0100;
        assert!(decode_lane_bytes(
            &unreferenced,
            3,
            FieldType::Utf8,
            false,
            V3ValueLimits::HARD.max_block_bytes,
        )
        .is_err());
        let mut out_of_range = encode_dictionary_lane(&three, 3).unwrap().bytes;
        out_of_range[three_index_start] |= 0b11;
        assert!(decode_lane_bytes(
            &out_of_range,
            3,
            FieldType::Utf8,
            false,
            V3ValueLimits::HARD.max_block_bytes,
        )
        .is_err());

        let decimal = AuraV3Column {
            slot: 0,
            validity: None,
            values: AuraV3ColumnValues::DecimalText(variable(&[b"1", b"2", b"1"])),
        };
        let mut invalid_decimal = encode_dictionary_lane(&decimal, 3).unwrap().bytes;
        invalid_decimal[dictionary_data_start] = b'x';
        assert!(decode_lane_bytes(
            &invalid_decimal,
            3,
            FieldType::DecimalText,
            false,
            V3ValueLimits::HARD.max_block_bytes,
        )
        .is_err());
    }

    #[test]
    fn cumulative_dictionary_inverse_budget_rejects_before_second_lane_output() {
        let schema = SchemaBuilder::new("anonymous_dictionary_budget")
            .v3()
            .field("left", FieldType::Utf8, FieldRole::Value)
            .field("right", FieldType::Utf8, FieldRole::Value)
            .finish()
            .unwrap();
        let rows = 64usize;
        let value = vec![b'x'; 128];
        let parts = vec![value.as_slice(); rows];
        let batch = AuraV3Batch {
            schema_id: schema.schema_id,
            row_count: rows as u32,
            columns: vec![
                AuraV3Column {
                    slot: 0,
                    validity: None,
                    values: AuraV3ColumnValues::Utf8(variable(&parts)),
                },
                AuraV3Column {
                    slot: 1,
                    validity: None,
                    values: AuraV3ColumnValues::Utf8(variable(&parts)),
                },
            ],
        };
        let mut plan = FlatAuraPlanV2::all_fixed(&schema).unwrap();
        plan.registry_version = FLAT_PLAN_V2_DICTIONARY_REGISTRY_VERSION;
        plan.codecs
            .fill(PlanV2PhysicalCodec::VariableByteDictionaryBitpacked);
        plan.validate(&schema).unwrap();
        let block = encode_block(&schema, &batch, &plan, V3ValueLimits::HARD).unwrap();
        assert!(block.len() < 10_000);
        let result = decode_block(
            &schema,
            &block,
            &plan,
            V3ValueLimits {
                max_block_bytes: 10_000,
                max_rows: rows,
                ..V3ValueLimits::HARD
            },
        );
        assert_eq!(
            result,
            Err(AuraError::InvalidValue(
                "planned flat dictionary inverse length"
            ))
        );
    }

    fn temporal_plan(
        field_type: FieldType,
        codec: PlanV2PhysicalCodec,
        delta2: bool,
    ) -> (SchemaDescriptor, FlatAuraPlanV2) {
        let mut candidates = TransformCandidates::default_for_role(FieldRole::Timestamp);
        if delta2 {
            candidates = candidates.with(FieldTransform::Delta2);
        }
        let schema = SchemaBuilder::new("anonymous_temporal_lane")
            .v3()
            .field_with_candidates("clock", field_type, FieldRole::Timestamp, candidates)
            .finish()
            .unwrap();
        let mut plan = FlatAuraPlanV2::all_fixed(&schema).unwrap();
        plan.registry_version = FLAT_PLAN_V2_TEMPORAL_REGISTRY_VERSION;
        plan.codecs[0] = codec;
        plan.validate(&schema).unwrap();
        (schema, plan)
    }

    #[test]
    fn temporal_lanes_roundtrip_reset_and_reject_noncanonical_or_overflow_inverse() {
        for (field_type, codec) in [
            (
                FieldType::TimestampNs,
                PlanV2PhysicalCodec::TimestampPreviousDeltaZigZagUleb128,
            ),
            (
                FieldType::TimestampMs,
                PlanV2PhysicalCodec::TimestampDeltaOfDeltaZigZagUleb128,
            ),
        ] {
            let values = vec![100, 107, 114, 114, 108, -3];
            let column = AuraV3Column {
                slot: 0,
                validity: None,
                values: match field_type {
                    FieldType::TimestampNs => AuraV3ColumnValues::TimestampNs(values.clone()),
                    FieldType::TimestampMs => AuraV3ColumnValues::TimestampMs(values.clone()),
                    _ => unreachable!(),
                },
            };
            let mut bytes = Vec::new();
            encode_temporal_lane(&column, codec, &mut bytes).unwrap();
            let mut reader = ByteReader::new(&bytes);
            let decoded = decode_temporal_lane(
                0,
                field_type,
                false,
                values.len(),
                codec,
                &mut reader,
                values.len() * 8,
            )
            .unwrap();
            reader.finish().unwrap();
            assert_eq!(decoded, column);
            for length in 0..bytes.len() {
                let mut reader = ByteReader::new(&bytes[..length]);
                assert!(decode_temporal_lane(
                    0,
                    field_type,
                    false,
                    values.len(),
                    codec,
                    &mut reader,
                    values.len() * 8,
                )
                .is_err());
            }
        }

        let mut nonminimal_zero = ByteReader::new(&[0x80, 0]);
        assert!(decode_temporal_lane(
            0,
            FieldType::TimestampNs,
            false,
            1,
            PlanV2PhysicalCodec::TimestampPreviousDeltaZigZagUleb128,
            &mut nonminimal_zero,
            8,
        )
        .is_err());

        let mut previous_overflow = Vec::new();
        crate::varint::encode_i64(i64::MAX, &mut previous_overflow);
        crate::varint::encode_i64(1, &mut previous_overflow);
        let mut reader = ByteReader::new(&previous_overflow);
        assert!(decode_temporal_lane(
            0,
            FieldType::TimestampNs,
            false,
            2,
            PlanV2PhysicalCodec::TimestampPreviousDeltaZigZagUleb128,
            &mut reader,
            16,
        )
        .is_err());

        let mut delta2_overflow = Vec::new();
        crate::varint::encode_i64(0, &mut delta2_overflow);
        crate::varint::encode_i64(i64::MAX, &mut delta2_overflow);
        crate::varint::encode_i64(1, &mut delta2_overflow);
        let mut reader = ByteReader::new(&delta2_overflow);
        assert!(decode_temporal_lane(
            0,
            FieldType::TimestampMs,
            false,
            3,
            PlanV2PhysicalCodec::TimestampDeltaOfDeltaZigZagUleb128,
            &mut reader,
            24,
        )
        .is_err());
    }

    #[test]
    fn temporal_wrapper_v2_is_distinct_deterministic_and_canonical() {
        let (schema, plan) = temporal_plan(
            FieldType::TimestampNs,
            PlanV2PhysicalCodec::TimestampPreviousDeltaZigZagUleb128,
            false,
        );
        let batch = AuraV3Batch {
            schema_id: schema.schema_id,
            row_count: 512,
            columns: vec![AuraV3Column {
                slot: 0,
                validity: None,
                values: AuraV3ColumnValues::TimestampNs(
                    (0..512).map(|row| 1_000 + row * 7).collect(),
                ),
            }],
        };
        let inner = encode_block(&schema, &batch, &plan, V3ValueLimits::HARD).unwrap();
        assert_eq!(&inner[..8], BLOCK_MAGIC_V3);
        let first = encode_temporal_zstd_wrapper(
            &inner,
            V3_PLANNED_FLAT_TEMPORAL_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_TEMPORAL_BLOCK_VERSION,
            V3ValueLimits::HARD,
        )
        .unwrap();
        let second = encode_temporal_zstd_wrapper(
            &inner,
            V3_PLANNED_FLAT_TEMPORAL_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_TEMPORAL_BLOCK_VERSION,
            V3ValueLimits::HARD,
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(&first[..8], TEMPORAL_ZSTD_WRAPPER_MAGIC);
        assert_eq!(
            decode_temporal_zstd_wrapper(&first, &plan, V3ValueLimits::HARD).unwrap(),
            inner
        );
        assert!(decode_zstd_wrapper(&first, &plan, V3ValueLimits::HARD).is_err());
        for length in 0..first.len() {
            assert!(
                decode_temporal_zstd_wrapper(&first[..length], &plan, V3ValueLimits::HARD).is_err()
            );
        }
        for offset in [0usize, 8, 10, 11, 12, 13, 14, 16, 18, 20, 28, 36] {
            let mut corrupted = first.clone();
            corrupted[offset] ^= 1;
            assert!(decode_temporal_zstd_wrapper(&corrupted, &plan, V3ValueLimits::HARD).is_err());
        }
    }

    #[test]
    fn prefix_suffix_wrapper_v3_is_distinct_and_raw_layout_is_rejected() {
        let schema = SchemaBuilder::new("anonymous_prefix_suffix_wrapper")
            .v3()
            .field("clock", FieldType::TimestampMs, FieldRole::Timestamp)
            .field("text", FieldType::Utf8, FieldRole::Value)
            .finish()
            .unwrap();
        let owned = (0..128)
            .map(|row| format!("common-prefix-{row:04}-common-suffix"))
            .collect::<Vec<_>>();
        let refs = owned
            .iter()
            .map(|value| value.as_bytes())
            .collect::<Vec<_>>();
        let batch = AuraV3Batch {
            schema_id: schema.schema_id,
            row_count: 128,
            columns: vec![
                AuraV3Column {
                    slot: 0,
                    validity: None,
                    values: AuraV3ColumnValues::TimestampMs((0..128).collect()),
                },
                AuraV3Column {
                    slot: 1,
                    validity: None,
                    values: AuraV3ColumnValues::Utf8(variable(&refs)),
                },
            ],
        };
        let mut plan = FlatAuraPlanV2::all_fixed(&schema).unwrap();
        plan.registry_version = FLAT_PLAN_V2_PREFIX_SUFFIX_REGISTRY_VERSION;
        plan.codecs[0] = PlanV2PhysicalCodec::TimestampPreviousDeltaZigZagUleb128;
        plan.codecs[1] = PlanV2PhysicalCodec::PreviousCommonPrefixSuffixBytes;
        plan.validate(&schema).unwrap();
        assert!(compile_plan(
            &schema,
            std::slice::from_ref(&batch),
            V3FlatLimits::HARD,
            plan.clone()
        )
        .is_err());
        assert!(validate_stored_body_versions(
            &plan,
            V3_PLANNED_FLAT_PREFIX_SUFFIX_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_PREFIX_SUFFIX_BLOCK_VERSION,
        )
        .is_err());
        let inner = encode_block(&schema, &batch, &plan, V3ValueLimits::HARD).unwrap();
        assert_eq!(&inner[..8], BLOCK_MAGIC_V4);
        let first = encode_prefix_suffix_zstd_wrapper(
            &inner,
            V3_PLANNED_FLAT_PREFIX_SUFFIX_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_PREFIX_SUFFIX_BLOCK_VERSION,
            V3ValueLimits::HARD,
        )
        .unwrap();
        let second = encode_prefix_suffix_zstd_wrapper(
            &inner,
            V3_PLANNED_FLAT_PREFIX_SUFFIX_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_PREFIX_SUFFIX_BLOCK_VERSION,
            V3ValueLimits::HARD,
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(&first[..8], PREFIX_SUFFIX_ZSTD_WRAPPER_MAGIC);
        assert_eq!(
            decode_prefix_suffix_zstd_wrapper(&first, &plan, V3ValueLimits::HARD).unwrap(),
            inner
        );
        assert!(decode_zstd_wrapper(&first, &plan, V3ValueLimits::HARD).is_err());
        assert!(decode_temporal_zstd_wrapper(&first, &plan, V3ValueLimits::HARD).is_err());
        for offset in [0usize, 8, 10, 11, 12, 13, 14, 16, 18, 20, 28, 36] {
            let mut corrupted = first.clone();
            corrupted[offset] ^= 1;
            assert!(
                decode_prefix_suffix_zstd_wrapper(&corrupted, &plan, V3ValueLimits::HARD).is_err()
            );
        }
    }

    fn zstd_test_inner() -> (SchemaDescriptor, FlatAuraPlanV2, Vec<u8>) {
        let schema = SchemaBuilder::new("anonymous_zstd_wrapper")
            .v3()
            .field("value", FieldType::I64, FieldRole::Value)
            .finish()
            .unwrap();
        let batch = AuraV3Batch {
            schema_id: schema.schema_id,
            row_count: 512,
            columns: vec![AuraV3Column {
                slot: 0,
                validity: None,
                values: AuraV3ColumnValues::I64(vec![7; 512]),
            }],
        };
        let plan = FlatAuraPlanV2::all_fixed(&schema).unwrap();
        let inner = encode_block(&schema, &batch, &plan, V3ValueLimits::HARD).unwrap();
        (schema, plan, inner)
    }

    fn alternate_zstd_frame(
        inner: &[u8],
        level: i32,
        checksum: bool,
        content_size: bool,
    ) -> Vec<u8> {
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), level).unwrap();
        encoder
            .set_pledged_src_size(Some(inner.len() as u64))
            .unwrap();
        encoder.include_contentsize(content_size).unwrap();
        encoder.include_checksum(checksum).unwrap();
        encoder.include_dictid(false).unwrap();
        encoder.long_distance_matching(false).unwrap();
        encoder
            .window_log(u32::from(V3_PLANNED_FLAT_ZSTD_WINDOW_LOG))
            .unwrap();
        encoder.write_all(inner).unwrap();
        encoder.finish().unwrap()
    }

    fn wrapper_with_frame(canonical: &[u8], frame: &[u8]) -> Vec<u8> {
        let mut wrapper = canonical[..ZSTD_WRAPPER_HEADER_BYTES].to_vec();
        wrapper[28..36].copy_from_slice(&(frame.len() as u64).to_le_bytes());
        wrapper.extend_from_slice(frame);
        wrapper
    }

    #[test]
    fn zstd_wrapper_profile_is_deterministic_exact_and_canonical() {
        let (_, plan, inner) = zstd_test_inner();
        let first = encode_zstd_wrapper(
            &inner,
            V3_PLANNED_FLAT_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_BLOCK_VERSION,
            V3ValueLimits::HARD,
        )
        .unwrap();
        let second = encode_zstd_wrapper(
            &inner,
            V3_PLANNED_FLAT_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_BLOCK_VERSION,
            V3ValueLimits::HARD,
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(&first[..8], ZSTD_WRAPPER_MAGIC);
        assert_eq!(
            first.len(),
            ZSTD_WRAPPER_HEADER_BYTES
                + u64::from_le_bytes(first[28..36].try_into().unwrap()) as usize
        );
        assert_eq!(
            decode_zstd_wrapper(&first, &plan, V3ValueLimits::HARD).unwrap(),
            inner
        );
        for length in 0..first.len() {
            assert!(decode_zstd_wrapper(&first[..length], &plan, V3ValueLimits::HARD).is_err());
        }
    }

    #[test]
    fn zstd_wrapper_fields_frames_and_declarations_fail_closed() {
        let (_, plan, inner) = zstd_test_inner();
        let canonical = encode_zstd_wrapper(
            &inner,
            V3_PLANNED_FLAT_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_BLOCK_VERSION,
            V3ValueLimits::HARD,
        )
        .unwrap();
        for offset in [0usize, 8, 10, 11, 12, 13, 14, 16, 18, 20, 28, 36] {
            let mut corrupted = canonical.clone();
            corrupted[offset] ^= 1;
            assert!(decode_zstd_wrapper(&corrupted, &plan, V3ValueLimits::HARD).is_err());
        }
        for (offset, value) in [(20usize, u64::MAX), (28, u64::MAX)] {
            let mut bomb = canonical.clone();
            bomb[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            assert!(decode_zstd_wrapper(&bomb, &plan, V3ValueLimits::HARD).is_err());
        }
        let compressed = &canonical[ZSTD_WRAPPER_HEADER_BYTES..];
        let mut concatenated = compressed.to_vec();
        concatenated.extend_from_slice(compressed);
        assert!(decode_zstd_wrapper(
            &wrapper_with_frame(&canonical, &concatenated),
            &plan,
            V3ValueLimits::HARD,
        )
        .is_err());
        let mut trailing = compressed.to_vec();
        trailing.push(0);
        assert!(decode_zstd_wrapper(
            &wrapper_with_frame(&canonical, &trailing),
            &plan,
            V3ValueLimits::HARD,
        )
        .is_err());
        let mut skippable = compressed.to_vec();
        skippable[..4].copy_from_slice(&[0x50, 0x2a, 0x4d, 0x18]);
        assert!(decode_zstd_wrapper(
            &wrapper_with_frame(&canonical, &skippable),
            &plan,
            V3ValueLimits::HARD,
        )
        .is_err());
        let mut checksum = compressed.to_vec();
        *checksum.last_mut().unwrap() ^= 1;
        assert!(decode_zstd_wrapper(
            &wrapper_with_frame(&canonical, &checksum),
            &plan,
            V3ValueLimits::HARD,
        )
        .is_err());
        for frame in [
            alternate_zstd_frame(&inner, 3, true, true),
            alternate_zstd_frame(&inner, V3_PLANNED_FLAT_ZSTD_LEVEL, false, true),
            alternate_zstd_frame(&inner, V3_PLANNED_FLAT_ZSTD_LEVEL, true, false),
        ] {
            assert!(decode_zstd_wrapper(
                &wrapper_with_frame(&canonical, &frame),
                &plan,
                V3ValueLimits::HARD,
            )
            .is_err());
        }
        let mut low_limits = V3ValueLimits::HARD;
        low_limits.max_block_bytes = inner.len() - 1;
        assert!(decode_zstd_wrapper(&canonical, &plan, low_limits).is_err());
    }

    #[test]
    fn zstd_wrapper_inner_schema_plan_and_version_mismatch_fail_closed() {
        let (schema, plan, inner) = zstd_test_inner();
        let mut wrong_schema = inner.clone();
        wrong_schema[12..16].copy_from_slice(&schema.schema_id.wrapping_add(1).to_le_bytes());
        let wrapper = encode_zstd_wrapper(
            &wrong_schema,
            V3_PLANNED_FLAT_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_BLOCK_VERSION,
            V3ValueLimits::HARD,
        )
        .unwrap();
        let decoded = decode_zstd_wrapper(&wrapper, &plan, V3ValueLimits::HARD).unwrap();
        assert!(decode_block(&schema, &decoded, &plan, V3ValueLimits::HARD).is_err());

        let mut wrong_version = inner.clone();
        wrong_version[8..10].copy_from_slice(&99u16.to_le_bytes());
        let wrapper = encode_zstd_wrapper(
            &wrong_version,
            V3_PLANNED_FLAT_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_BLOCK_VERSION,
            V3ValueLimits::HARD,
        )
        .unwrap();
        let decoded = decode_zstd_wrapper(&wrapper, &plan, V3ValueLimits::HARD).unwrap();
        assert!(decode_block(&schema, &decoded, &plan, V3ValueLimits::HARD).is_err());

        let mut wrong_plan = plan.clone();
        wrong_plan.codecs[0] = PlanV2PhysicalCodec::SignedZigZagUleb128;
        wrong_plan.validate(&schema).unwrap();
        let wrapper = encode_zstd_wrapper(
            &inner,
            V3_PLANNED_FLAT_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_BLOCK_VERSION,
            V3ValueLimits::HARD,
        )
        .unwrap();
        let decoded = decode_zstd_wrapper(&wrapper, &wrong_plan, V3ValueLimits::HARD).unwrap();
        assert!(decode_block(&schema, &decoded, &wrong_plan, V3ValueLimits::HARD).is_err());
    }

    #[test]
    fn zstd_wrapper_rejects_frame_requiring_window_above_profile_cap() {
        let (_, plan, small_inner) = zstd_test_inner();
        let canonical = encode_zstd_wrapper(
            &small_inner,
            V3_PLANNED_FLAT_BODY_LAYOUT_VERSION,
            V3_PLANNED_FLAT_BLOCK_VERSION,
            V3ValueLimits::HARD,
        )
        .unwrap();
        let inner = vec![0x5au8; (8 * 1024 * 1024) + 1];
        let mut encoder =
            zstd::stream::write::Encoder::new(Vec::new(), V3_PLANNED_FLAT_ZSTD_LEVEL).unwrap();
        encoder
            .set_pledged_src_size(Some(inner.len() as u64))
            .unwrap();
        encoder.include_contentsize(true).unwrap();
        encoder.include_checksum(true).unwrap();
        encoder.include_dictid(false).unwrap();
        encoder.long_distance_matching(false).unwrap();
        encoder.window_log(24).unwrap();
        encoder.write_all(&inner).unwrap();
        let frame = encoder.finish().unwrap();
        let mut permissive =
            zstd::stream::read::Decoder::new(Cursor::new(frame.as_slice())).unwrap();
        permissive.window_log_max(24).unwrap();
        let mut decoded = Vec::new();
        permissive.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, inner);

        let mut wrapper = wrapper_with_frame(&canonical, &frame);
        wrapper[20..28].copy_from_slice(&(inner.len() as u64).to_le_bytes());
        let inner_sha256: [u8; 32] = Sha256::digest(&inner).into();
        wrapper[36..68].copy_from_slice(&inner_sha256);
        assert!(decode_zstd_wrapper(&wrapper, &plan, V3ValueLimits::HARD).is_err());
    }

    #[test]
    fn registry1_flat_plan_and_container_hashes_are_golden() {
        let schema = SchemaBuilder::new("anonymous_registry1_golden")
            .v3()
            .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
            .field("value", FieldType::I64, FieldRole::Value)
            .finish()
            .unwrap();
        let batch = AuraV3Batch {
            schema_id: schema.schema_id,
            row_count: 3,
            columns: vec![
                AuraV3Column {
                    slot: 0,
                    validity: None,
                    values: AuraV3ColumnValues::TimestampMs(vec![1, 2, 3]),
                },
                AuraV3Column {
                    slot: 1,
                    validity: None,
                    values: AuraV3ColumnValues::I64(vec![-1, 0, 1]),
                },
            ],
        };
        let plan = FlatAuraPlanV2::all_fixed(&schema).unwrap();
        let plan_bytes = plan.encode(&schema).unwrap();
        let artifact = compile_plan(&schema, &[batch], V3FlatLimits::HARD, plan.clone()).unwrap();
        assert_eq!(
            hex(&Sha256::digest(&plan_bytes)),
            "edb401d1a5e5bea099c3afa0084c751b26c84c9de837cb2b2d45cf39d22de9d8"
        );
        assert_eq!(
            hex(&Sha256::digest(&artifact.bytes)),
            "2394a5bf0ff8476df8dd5ce8bb0daca2aa9e1f7f14817b26f5d88bbd83361954"
        );
        assert_eq!(plan.registry_version, FLAT_PLAN_V2_REGISTRY_VERSION);
        let header_len = AuraHeader::encoded_len(&artifact.bytes).unwrap();
        assert_eq!(&artifact.bytes[header_len..header_len + 8], b"AUFPVB01");
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
