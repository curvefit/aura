//! Bounded all-memory reference planned-flat Aura0 V3 container.
//!
//! Exact, all-fixed and mixed-codec complete artifacts coexist during scoring;
//! peak memory is therefore roughly their summed bytes plus lane scratch. This
//! is an honest replayable reference API, not a streaming writer claim.

use std::io::Cursor;

use sha2::{Digest, Sha256};

use crate::bytes::ByteReader;
use crate::format::{AuraContainerVersion, SEAL_MAGIC};
use crate::v3_codecs::{
    decode_canonical_uleb128, decode_canonical_zigzag, fixed_width, integer_varint_codec,
    PlanV2PhysicalCodec,
};
use crate::v3_flat_plan_v2::FlatAuraPlanV2;
use crate::v3_values::{decode_column, encode_column};
use crate::{
    canonical_v3_batch_sha256, canonical_v3_schema_fingerprint, decode_schema_descriptor,
    decode_v3_flat_aura0_with_limits, encode_schema_descriptor, validate_v3_batch, AuraError,
    AuraHeader, AuraV3Batch, AuraV3Column, AuraV3ColumnValues, CanonicalV3RowHasher,
    DecodedV3FlatAura0, Profile, Result, SchemaDescriptor, V3FlatAura0Writer, V3FlatLimits,
    V3FlatWriterOptions, V3ValueLimits, MAX_V3_VALUE_BLOCK_BYTES,
    V3_FLAT_BODY_ENCODING_EXACT_BLOCKS, V3_FLAT_FOOTER_LAYOUT_VERSION,
};

pub const V3_PLANNED_FLAT_FOOTER_LAYOUT_VERSION: u16 = 3;
pub const V3_PLANNED_FLAT_BODY_ENCODING: u8 = 4;
pub const V3_PLANNED_FLAT_BODY_LAYOUT_VERSION: u16 = 1;
pub const V3_PLANNED_FLAT_BLOCK_VERSION: u16 = 1;
pub const V3_PLANNED_FLAT_FOOTER_PREFIX_BYTES: usize = 208;
pub const V3_PLANNED_FLAT_CHUNK_DESCRIPTOR_BYTES: usize = 104;
pub const MAX_V3_PLANNED_FLAT_FOOTER_BYTES: usize = 64 * 1024 * 1024;

const BLOCK_MAGIC: &[u8; 8] = b"AUFPVB01";
const BLOCK_HEADER_BYTES: usize = 64;
const FOOTER_HASH_DOMAIN: &[u8] = b"aura-v3-planned-flat-footer-v1\0";
const HEADER_HASH_DOMAIN: &[u8] = b"aura-v3-planned-flat-header-v1\0";
const BODY_HASH_DOMAIN: &[u8] = b"aura-v3-planned-flat-body-v1\0";

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
    pub selected: PlanV2PhysicalCodec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedFlatInspection {
    pub candidates: Vec<V3PlannedFlatCandidateInspection>,
    pub codecs: Vec<V3PlannedFlatCodecInspection>,
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
    validate_inputs(schema, batches, limits)?;
    let exact = compile_exact(schema, batches, limits);
    let fixed_plan = FlatAuraPlanV2::all_fixed(schema);
    let fixed = fixed_plan
        .clone()
        .and_then(|plan| compile_plan(schema, batches, limits, plan));
    let mixed_plan_rows = fixed_plan.and_then(|plan| select_codecs(schema, batches, plan));
    let rows = mixed_plan_rows.as_ref().ok().map(|(_, rows)| rows.clone());
    let mixed = mixed_plan_rows.and_then(|(plan, _)| compile_plan(schema, batches, limits, plan));
    let candidates = vec![exact, fixed, mixed];
    if let Some(error) = candidates
        .iter()
        .filter_map(|candidate| candidate.as_ref().err())
        .find(|error| !candidate_limit(error))
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
    let selected_index = sizes
        .iter()
        .enumerate()
        .filter_map(|(index, bytes)| bytes.map(|bytes| (index, bytes)))
        .min_by_key(|(index, bytes)| (*bytes, *index))
        .map(|(index, _)| index)
        .ok_or(AuraError::InvalidValue(
            "planned flat no applicable candidate",
        ))?;
    let ids = [
        "exact-flat",
        "planned-flat-fixed",
        "planned-flat-integer-codecs",
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
    selected.inspection.codecs = if selected_index == 2 {
        rows.unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(selected)
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
            selected,
        });
    }
    plan.validate(schema)?;
    Ok((plan, rows))
}

fn lane_total(batches: &[AuraV3Batch], slot: usize, codec: PlanV2PhysicalCodec) -> Result<usize> {
    let mut total = 0usize;
    for batch in batches {
        let mut out = Vec::new();
        encode_lane(
            &batch.columns[slot],
            batch.row_count as usize,
            codec,
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
    let limits = limits.effective();
    let structural = V3ValueLimits {
        max_block_bytes: MAX_V3_VALUE_BLOCK_BYTES,
        ..limits.value_limits
    };
    let header = canonical_header(schema)?;
    let header_bytes = header.encode()?;
    let mut body = Vec::new();
    let mut chunks = Vec::new();
    let mut first_row = 0u64;
    let total_rows = batches
        .iter()
        .map(|batch| u64::from(batch.row_count))
        .sum::<u64>();
    let mut global = CanonicalV3RowHasher::new(schema, total_rows as u32, structural)?;
    for (index, batch) in batches.iter().enumerate() {
        let block = encode_block(schema, batch, &plan, limits.value_limits)?;
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
        schema: schema.clone(),
        plan: plan.clone(),
        schema_fingerprint: canonical_v3_schema_fingerprint(schema)?,
        plan_sha256: plan.hash(schema)?,
        header_sha256: domain_hash(HEADER_HASH_DOMAIN, &header_bytes)?,
        body_sha256: domain_hash(BODY_HASH_DOMAIN, &body)?,
        global_logical_sha256: global.finalize()?,
        chunks,
    };
    seal(header_bytes, body, footer, limits)
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
    let mut out = Vec::new();
    out.extend_from_slice(BLOCK_MAGIC);
    out.extend_from_slice(&V3_PLANNED_FLAT_BLOCK_VERSION.to_le_bytes());
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
    out: &mut Vec<u8>,
) -> Result<()> {
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

fn decode_block(
    schema: &SchemaDescriptor,
    bytes: &[u8],
    plan: &FlatAuraPlanV2,
    limits: V3ValueLimits,
) -> Result<AuraV3Batch> {
    let limits = limits.effective();
    if bytes.len() > limits.max_block_bytes || bytes.len() < BLOCK_HEADER_BYTES {
        return Err(AuraError::InvalidValue("planned flat block length"));
    }
    let mut reader = ByteReader::new(bytes);
    if reader.read_exact(8)? != BLOCK_MAGIC
        || reader.read_u16_le()? != 1
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
    for field in &schema.fields {
        let codec = *plan
            .codecs
            .get(usize::from(field.index))
            .ok_or(AuraError::InvalidValue("planned flat codec slot"))?;
        columns.push(decode_lane(
            field.index,
            field.field_type,
            field.nullable,
            rows_usize,
            codec,
            &mut reader,
            limits,
        )?);
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
    Ok(batch)
}

fn decode_lane(
    slot: u16,
    field_type: crate::FieldType,
    nullable: bool,
    rows: usize,
    codec: PlanV2PhysicalCodec,
    reader: &mut ByteReader<'_>,
    limits: V3ValueLimits,
) -> Result<AuraV3Column> {
    if codec == PlanV2PhysicalCodec::FixedWidth {
        return decode_fixed_lane(slot, field_type, nullable, rows, reader, limits);
    }
    let validity = if nullable {
        let len = checked_validity_len(rows)?;
        Some(reader.read_exact(len)?.to_vec())
    } else {
        None
    };
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

fn decode_fixed_lane(
    slot: u16,
    field_type: crate::FieldType,
    nullable: bool,
    rows: usize,
    reader: &mut ByteReader<'_>,
    limits: V3ValueLimits,
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
    put16(&mut out, 1);
    put16(&mut out, 1);
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
        put16(&mut out, 1);
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
    if r.read_u16_le() != Ok(1) || r.read_u16_le() != Ok(1) {
        return Err(AuraError::InvalidValue("planned flat versions"));
    }
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
        if r.read_u16_le() != Ok(1) || r.read_u16_le() != Ok(0) {
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
        let batch = decode_block(&footer.schema, stored, &footer.plan, limits.value_limits)?;
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

fn candidate_limit(error: &AuraError) -> bool {
    matches!(
        error,
        AuraError::InvalidValue(
            "v3 value block length"
                | "planned flat block length"
                | "v3 flat body length"
                | "planned flat body length"
                | "v3 flat footer length"
                | "planned flat footer length"
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
