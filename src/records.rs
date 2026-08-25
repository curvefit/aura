use std::time::Instant;

use crate::bitpack::{
    bitpacked_byte_len, pack_signed_values, pack_unsigned_values, unpack_signed_values,
    unpack_unsigned_values,
};
use crate::body::{decode_generic_stream_body, encode_generic_stream_body, GenericStreamBodyValue};
use crate::bytes::{put_i64_le, put_u16_le, put_u32_le, put_u64_le, ByteGuard, ByteReader};
use crate::execution::{
    Aura0ColumnPath, Aura1BodyPath, Aura1EffectiveBodyPath, Aura1ExecutionOptions,
    Aura1ExecutionTrace, UnsupportedPathBehavior,
};
use crate::footer::AuraFooter;
use crate::format::{AuraContainerVersion, SEAL_MAGIC};
use crate::generic_planner::{
    decode_generic_i64_events_body, decode_generic_i64_rows, decode_generic_i64_rows_body,
    decode_generic_i64_stream_values_profiled, encode_generic_i64_columns_with_plan,
    encode_generic_i64_columns_with_plan_direct_streams, encode_generic_i64_events,
    encode_generic_i64_rows, encode_generic_i64_rows_body, encode_generic_i64_rows_with_plan,
    explicit_event_group, plan_generic_i64_rows, plan_uuid_const_mask_stream,
    try_decode_generic_i64_columns_body, try_encode_generic_i64_aura1_body,
    try_encode_generic_i64_aura1_body_streaming, try_write_generic_i64_aura1_body,
    try_write_generic_i64_aura1_body_from_streams_profiled,
    try_write_generic_i64_aura1_body_guarded, try_write_generic_i64_aura1_body_streaming,
    try_write_partitioned_sparse_i64_aura1_body_profiled, DirectAura1DecodeStats,
    DirectAura1DecodeTimings, DirectAura1WriterStats, DirectAura1WriterTimings,
    GenericColumnEncodeStats, GenericEncodedI64Rows, GenericEncodedStream,
};
use crate::header::{AuraHeader, LEGACY_HEADER_PREFIX_SIZE};
use crate::instructions::{GenericInstructionPlan, GenericStreamOp};
use crate::plan::{
    unpack_ref_divisor, unpack_two_refs, Aura0Plan, Aura1Plan, FieldEncoding, PhysicalFieldPlan,
};
use crate::program::{
    validate_aura1_byte_lane_limits, Aura1ByteLaneDescriptor, CompiledAuraPlan, CompiledFooter,
    DecodeProgram, AURA1_BYTE_LANE_VERSION, BYTE_LANE_CHECKSUM_BYTE_GUARD, BYTE_LANE_CHECKSUM_NONE,
    BYTE_LANE_CODEC_LZ4, BYTE_LANE_CODEC_RAW, BYTE_LANE_CODEC_ZSTD, COMPILED_FOOTER_MAGIC,
    MAX_AURA1_BYTE_LANES_TOTAL_OUTPUT_BYTES, MAX_AURA1_BYTE_LANE_COMPRESSED_BYTES,
    MAX_AURA1_BYTE_LANE_OUTPUT_BYTES,
};
use crate::schema::{
    schema_parent_mapping, validate_schema_container_compatibility, FieldRole, FieldScope,
    FieldType, SchemaDescriptor,
};
use crate::stats::IngestStats;
use crate::varint::{decode_i64 as decode_varint_i64, decode_u64 as decode_varint_u64};
use crate::varint::{encode_i64 as encode_varint_i64, encode_u64 as encode_varint_u64};
use crate::{AuraError, AuraTypedValue, PhysicalWidth, Profile, Result};

/// Hard ceilings for V2 row-materializing decoders. These are format-reader
/// safety limits, not caller-tunable performance options.
pub const MAX_V2_I64_DECODE_ROWS: usize = 4 * 1024 * 1024;
pub const MAX_V2_I64_DECODE_FIELDS: usize = 256;
pub const MAX_V2_I64_DECODE_VALUES: usize = 64 * 1024 * 1024;
pub const MAX_V2_I64_DECODE_BODY_BYTES: usize = 512 * 1024 * 1024;

pub(crate) fn validate_i64_decode_dimensions(
    record_count: u64,
    field_count: usize,
    body_len: usize,
) -> Result<(usize, usize)> {
    if body_len > MAX_V2_I64_DECODE_BODY_BYTES {
        return Err(AuraError::InvalidValue("i64 decode body limit"));
    }
    let record_count = usize::try_from(record_count)
        .map_err(|_| AuraError::InvalidValue("i64 decode record count"))?;
    if record_count > MAX_V2_I64_DECODE_ROWS {
        return Err(AuraError::InvalidValue("i64 decode row limit"));
    }
    if field_count > MAX_V2_I64_DECODE_FIELDS {
        return Err(AuraError::InvalidValue("i64 decode field limit"));
    }
    if record_count > 0 && field_count == 0 {
        return Err(AuraError::InvalidValue("zero-width record count"));
    }
    let value_count = record_count
        .checked_mul(field_count)
        .ok_or(AuraError::InvalidValue("i64 decode value count"))?;
    if value_count > MAX_V2_I64_DECODE_VALUES {
        return Err(AuraError::InvalidValue("i64 decode value limit"));
    }
    let _value_bytes = value_count
        .checked_mul(std::mem::size_of::<i64>())
        .ok_or(AuraError::InvalidValue("i64 decode allocation size"))?;
    let _row_bytes = record_count
        .checked_mul(std::mem::size_of::<Vec<i64>>())
        .ok_or(AuraError::InvalidValue("i64 decode allocation size"))?;
    Ok((record_count, value_count))
}

pub(crate) fn validate_i64_decode_dimensions_usize(
    record_count: usize,
    field_count: usize,
    body_len: usize,
) -> Result<usize> {
    let record_count_u64 = u64::try_from(record_count)
        .map_err(|_| AuraError::InvalidValue("i64 decode record count"))?;
    Ok(validate_i64_decode_dimensions(record_count_u64, field_count, body_len)?.1)
}

pub(crate) fn validate_compiled_i64_decode_limits(
    footer: &CompiledFooter,
    body_len: usize,
) -> Result<usize> {
    if footer.schema.fields.is_empty() && !footer.aura1_byte_lanes.is_empty() {
        return Ok(validate_i64_decode_dimensions(footer.record_count, 1, body_len)?.0);
    }
    let (record_count, _) =
        validate_i64_decode_dimensions(footer.record_count, footer.schema.fields.len(), body_len)?;
    if record_count > 0 && footer.schema.fields.is_empty() {
        return Err(AuraError::InvalidValue("zero-width record count"));
    }
    let aura1_plan = footer.aura1_program.to_aura1_plan(footer.block_capacity)?;
    validate_aura1_plan_fields(&aura1_plan, footer.schema.fields.len())?;
    Ok(record_count)
}

pub(crate) fn try_reserve_exact<T>(
    values: &mut Vec<T>,
    additional: usize,
    label: &'static str,
) -> Result<()> {
    values
        .try_reserve_exact(additional)
        .map_err(|_| AuraError::InvalidValue(label))
}

pub(crate) fn zero_i64_rows(record_count: usize, field_count: usize) -> Result<Vec<Vec<i64>>> {
    let _ = validate_i64_decode_dimensions_usize(record_count, field_count, 0)?;
    let mut rows = Vec::new();
    try_reserve_exact(&mut rows, record_count, "i64 decode row allocation")?;
    for _ in 0..record_count {
        let mut row = Vec::new();
        try_reserve_exact(&mut row, field_count, "i64 decode value allocation")?;
        row.resize(field_count, 0);
        rows.push(row);
    }
    Ok(rows)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I64FileInput {
    pub schema: SchemaDescriptor,
    pub rows: Vec<Vec<i64>>,
    pub stream_id: u16,
    pub dictionary_id: u16,
    pub header_comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I64Event {
    /// Values in schema event-scope field order.
    pub event_values: Vec<i64>,
    /// Child rows in schema repeated-scope field order.
    pub children: Vec<Vec<i64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I64EventFileInput {
    pub schema: SchemaDescriptor,
    pub events: Vec<I64Event>,
    pub stream_id: u16,
    pub dictionary_id: u16,
    pub header_comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedFileInput {
    pub schema: SchemaDescriptor,
    pub rows: Vec<Vec<AuraTypedValue>>,
    pub stream_id: u16,
    pub dictionary_id: u16,
    pub header_comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedI64File {
    pub header: AuraHeader,
    pub schema: SchemaDescriptor,
    pub ingest_footer: Option<AuraFooter>,
    pub compiled_footer: Option<CompiledFooter>,
    pub rows: Vec<Vec<i64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedI64EventFile {
    pub header: AuraHeader,
    pub schema: SchemaDescriptor,
    pub ingest_footer: Option<AuraFooter>,
    pub compiled_footer: Option<CompiledFooter>,
    pub events: Vec<I64Event>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedI64FileMetadata {
    pub header: AuraHeader,
    pub schema: SchemaDescriptor,
    pub ingest_footer: Option<AuraFooter>,
    pub compiled_footer: Option<CompiledFooter>,
    pub record_count: usize,
    pub header_len: usize,
    pub footer_start: usize,
    pub footer_len_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedI64ColumnsFile {
    pub header: AuraHeader,
    pub schema: SchemaDescriptor,
    pub compiled_footer: CompiledFooter,
    pub record_count: usize,
    pub columns: Vec<Vec<i64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aura1FixedLayoutInfo {
    pub record_count: usize,
    pub field_count: usize,
    pub record_width: usize,
    pub body_offset: usize,
    pub body_bytes: usize,
    pub footer_offset: usize,
    pub output_size: usize,
    pub conversion_plan_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedTypedFile {
    pub header: AuraHeader,
    pub schema: SchemaDescriptor,
    pub ingest_footer: Option<AuraFooter>,
    pub compiled_footer: Option<CompiledFooter>,
    pub rows: Vec<Vec<AuraTypedValue>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedCompileOutput {
    pub bytes: Vec<u8>,
    pub guard: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputGuardMode {
    NoGuard,
    FusedOutputGuard,
    OldPostOutputGuard,
    BlockBatchedOutputGuard,
}

impl OutputGuardMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoGuard => "no_guard",
            Self::FusedOutputGuard => "fused_output_guard",
            Self::OldPostOutputGuard => "old_post_output_guard",
            Self::BlockBatchedOutputGuard => "block_batched_output_guard",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscodePath {
    Auto,
    Materialized,
    Direct,
}

impl TranscodePath {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Materialized => "materialized",
            Self::Direct => "direct",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Aura0EncoderPath {
    #[default]
    Materialized,
    DirectStreams,
    ColumnFree,
}

impl Aura0EncoderPath {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Materialized => "materialized",
            Self::DirectStreams => "direct-streams",
            Self::ColumnFree => "column-free",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Aura0DecodePath {
    #[default]
    Materialized,
    Cursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aura0FileProfile {
    Compact,
    Fast,
    Hybrid,
}

impl Aura0FileProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Fast => "fast",
            Self::Hybrid => "hybrid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aura0ByteLaneCodec {
    Raw,
    Lz4,
    Zstd1,
    Zstd3,
    Zstd9,
}

impl Aura0ByteLaneCodec {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Lz4 => "lz4",
            Self::Zstd1 => "zstd1",
            Self::Zstd3 => "zstd3",
            Self::Zstd9 => "zstd9",
        }
    }

    const fn codec_id(self) -> u8 {
        match self {
            Self::Raw => BYTE_LANE_CODEC_RAW,
            Self::Lz4 => BYTE_LANE_CODEC_LZ4,
            Self::Zstd1 | Self::Zstd3 | Self::Zstd9 => BYTE_LANE_CODEC_ZSTD,
        }
    }

    const fn codec_level(self) -> u8 {
        match self {
            Self::Raw | Self::Lz4 => 0,
            Self::Zstd1 => 1,
            Self::Zstd3 => 3,
            Self::Zstd9 => 9,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aura0ByteLaneUse {
    Auto,
    Always,
    Never,
}

impl Aura0DecodePath {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Materialized => "materialized",
            Self::Cursor => "cursor",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectAura1TranscodeTimings {
    pub metadata_ns: u128,
    pub allocate_header_ns: u128,
    pub decode_input_streams: DirectAura1DecodeTimings,
    pub partitioned_sparse_writer: DirectAura1WriterTimings,
    pub trailer_ns: u128,
    pub post_output_guard_ns: u128,
    pub total_ns: u128,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectAura1TranscodeStats {
    pub decode: DirectAura1DecodeStats,
    pub writer: DirectAura1WriterStats,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectAura0TranscodeTimings {
    pub metadata_ns: u128,
    pub fixed_row_scan_ns: u128,
    pub stats_frequency_collection_ns: u128,
    pub direct_stream_construction_ns: u128,
    pub field_extraction_ns: u128,
    pub dictionary_state_update_ns: u128,
    pub delta_stream_construction_ns: u128,
    pub compression_encoding_ns: u128,
    pub writer_finalization_ns: u128,
    pub canonical_hash_ns: u128,
    pub post_output_guard_ns: u128,
    pub total_ns: u128,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectAura0TranscodeStats {
    pub encoder_path: Aura0EncoderPath,
    pub direct_streams_enabled: bool,
    pub column_vector_count: usize,
    pub column_value_count: usize,
    pub rows_scanned: usize,
    pub aura1_scan_passes: usize,
    pub row_allocations: usize,
    pub stream_vector_count: usize,
    pub stream_vector_allocations: usize,
    pub stream_count: usize,
    pub stream_value_count: usize,
    pub direct_stream_count: usize,
    pub direct_stream_value_count: usize,
    pub bytes_read: usize,
    pub bytes_written: usize,
    pub copied_bytes: usize,
    pub temporary_buffer_bytes: usize,
    pub encoder_allocations: usize,
    pub output_blocks: usize,
    pub compression_blocks: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfiledCompileTimings {
    pub aura0_to_aura1: Option<DirectAura1TranscodeTimings>,
    pub aura1_to_aura0: Option<DirectAura0TranscodeTimings>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfiledCompileStats {
    pub aura0_to_aura1: Option<DirectAura1TranscodeStats>,
    pub aura1_to_aura0: Option<DirectAura0TranscodeStats>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfiledCompileOutput {
    pub bytes: Vec<u8>,
    pub output_byte_guard: Option<u64>,
    pub guard_mode: OutputGuardMode,
    pub transcode_path: TranscodePath,
    pub encoder_path: Aura0EncoderPath,
    pub conversion_plan_hash: Option<u64>,
    pub timings: ProfiledCompileTimings,
    pub stats: ProfiledCompileStats,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfiledCompileWithOptionsOutput {
    pub profiled: ProfiledCompileOutput,
    pub execution: Aura1ExecutionTrace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfiledCompileTraceOutput {
    pub profiled: ProfiledCompileOutput,
    pub effective_aura1_body_path: Option<Aura1EffectiveBodyPath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedAura1CompileOutput {
    pub bytes: Vec<u8>,
    pub rows_materialized: usize,
    pub fields_materialized: usize,
    pub values_materialized: usize,
}

pub fn encode_ingest_i64_file(input: I64FileInput) -> Result<Vec<u8>> {
    crate::writer::encode_i64(input)
}

/// Reference Aura0-to-Aura1 compiler that always reconstructs logical rows
/// before emitting fixed-width Aura1 bytes. It never uses an embedded Aura1
/// lane or a direct stream/file writer.
pub fn compile_aura0_to_aura1_materialized(bytes: &[u8]) -> Result<MaterializedAura1CompileOutput> {
    let source_header_len = AuraHeader::encoded_len(bytes)?;
    let source_header = AuraHeader::decode(
        bytes
            .get(..source_header_len)
            .ok_or(AuraError::UnexpectedEof)?,
    )?;
    if source_header.profile != Profile::Aura0 {
        return Err(AuraError::InvalidValue(
            "materialized compiler requires Aura0 input",
        ));
    }
    let decoded = decode_i64_file_inner(bytes)?;
    let mut footer = decoded.compiled_footer_for_compile()?;
    if compiled_footer_has_explicit_events(&footer) {
        return Err(AuraError::InvalidValue(
            "materialized Aura1 explicit events unsupported",
        ));
    }
    footer.aura1_byte_lanes.clear();
    let plan = footer.aura1_program.to_aura1_plan(footer.block_capacity)?;
    let fields_materialized = decoded.schema.fields.len();
    let rows_materialized = decoded.rows.len();
    let values_materialized = rows_materialized
        .checked_mul(fields_materialized)
        .ok_or(AuraError::InvalidValue("materialized value count"))?;
    let body = encode_aura1_body(&decoded.rows, &plan)?;
    let output = encode_compiled_file(
        Profile::Aura1,
        source_header.stream_id,
        source_header.dictionary_id,
        source_header.base_time_ns,
        source_header.comment.as_str(),
        body,
        footer,
    )?;
    Ok(MaterializedAura1CompileOutput {
        bytes: output,
        rows_materialized,
        fields_materialized,
        values_materialized,
    })
}

pub fn encode_ingest_i64_events_file(input: I64EventFileInput) -> Result<Vec<u8>> {
    encode_ingest_i64_events_file_inner(input)
}

/// Seal an explicit-event input directly into the requested production profile.
///
/// Aura0 reuses the already-planned compact event body and converts only the
/// ingest footer into its compiled form. This is byte-identical to sealing an
/// ingest file and compiling it through [`compile_i64_file`], but avoids the
/// intermediate complete file, decode, event reconstruction, replanning, and
/// second body encode.
pub fn encode_i64_events_profile(
    input: I64EventFileInput,
    target_profile: Profile,
) -> Result<Vec<u8>> {
    match target_profile {
        Profile::Ingest => encode_ingest_i64_events_file_inner(input),
        Profile::Aura0 => {
            let record_count = input.events.iter().try_fold(0u64, |count, event| {
                count
                    .checked_add(
                        u64::try_from(event.children.len())
                            .map_err(|_| AuraError::InvalidValue("record count"))?,
                    )
                    .ok_or(AuraError::InvalidValue("record count"))
            })?;
            validate_direct_i64_event_profile_limits(&input.schema, record_count, 0)?;
            let prepared = prepare_i64_events(input)?;
            validate_direct_i64_event_profile_limits(
                &prepared.footer.schema,
                record_count,
                prepared.body.len(),
            )?;
            let record_count = usize::try_from(record_count)
                .map_err(|_| AuraError::InvalidValue("record count"))?;
            let compiled_footer =
                compiled_footer_from_ingest_footer(&prepared.footer, record_count)?;
            let _ = validate_compiled_i64_decode_limits(&compiled_footer, prepared.body.len())?;
            encode_compiled_file(
                Profile::Aura0,
                prepared.stream_id,
                prepared.dictionary_id,
                prepared.base_time_ns,
                &prepared.header_comment,
                prepared.body,
                compiled_footer,
            )
        }
        Profile::Aura1 => {
            let ingest = encode_ingest_i64_events_file_inner(input)?;
            compile_i64_file_inner(&ingest, Profile::Aura1)
        }
    }
}

fn validate_direct_i64_event_profile_limits(
    schema: &SchemaDescriptor,
    record_count: u64,
    body_len: usize,
) -> Result<()> {
    if schema_has_wide_fields(schema) {
        return Err(AuraError::InvalidValue("i64 schema"));
    }
    let _ = validate_i64_decode_dimensions(record_count, schema.fields.len(), body_len)?;
    Ok(())
}

#[cfg(test)]
mod direct_i64_event_profile_limit_tests {
    use super::*;
    use crate::schema::{FieldRole, FieldType, SchemaBuilder};

    fn schema(field_count: usize) -> SchemaDescriptor {
        let mut builder = SchemaBuilder::new("direct-event-profile-limits").field(
            "ts",
            FieldType::TimestampNs,
            FieldRole::Timestamp,
        );
        for index in 1..field_count {
            builder =
                builder.repeated_field(format!("value_{index}"), FieldType::I64, FieldRole::Value);
        }
        builder.finish().unwrap()
    }

    #[test]
    fn direct_event_profile_enforces_reader_dimension_and_body_limits() {
        assert_eq!(
            validate_direct_i64_event_profile_limits(
                &schema(2),
                MAX_V2_I64_DECODE_ROWS as u64 + 1,
                0,
            )
            .unwrap_err(),
            AuraError::InvalidValue("i64 decode row limit")
        );
        assert_eq!(
            validate_direct_i64_event_profile_limits(&schema(257), 1, 0).unwrap_err(),
            AuraError::InvalidValue("i64 decode field limit")
        );
        assert_eq!(
            validate_direct_i64_event_profile_limits(
                &schema(17),
                MAX_V2_I64_DECODE_ROWS as u64,
                0,
            )
            .unwrap_err(),
            AuraError::InvalidValue("i64 decode value limit")
        );
        assert_eq!(
            validate_direct_i64_event_profile_limits(
                &schema(2),
                1,
                MAX_V2_I64_DECODE_BODY_BYTES + 1,
            )
            .unwrap_err(),
            AuraError::InvalidValue("i64 decode body limit")
        );
    }
}

pub fn encode_ingest_typed_file(input: TypedFileInput) -> Result<Vec<u8>> {
    crate::writer::encode_typed(input)
}

pub(crate) fn encode_ingest_typed_file_inner(input: TypedFileInput) -> Result<Vec<u8>> {
    validate_typed_rows(&input.schema, &input.rows)?;
    let mut stats = IngestStats::new_for_schema(&input.schema)?;
    for row in &input.rows {
        observe_typed_record(&mut stats, &input.schema, row)?;
    }
    let timestamp_index = timestamp_field_index(&input.schema);
    if let Some(timestamp_index) = timestamp_index {
        observe_typed_timestamp_runs(&mut stats, &input.rows, timestamp_index);
    }

    let footer = AuraFooter::new(input.schema.clone(), stats);
    let body = encode_typed_body(&input.schema, &input.rows)?;
    let base_time_ns = timestamp_index
        .and_then(|index| {
            input
                .rows
                .first()
                .and_then(|row| row.get(index))
                .and_then(typed_value_as_i64)
        })
        .unwrap_or(0);
    let header_comment = input.header_comment.as_deref().unwrap_or("");

    encode_file(
        Profile::Ingest,
        input.stream_id,
        input.dictionary_id,
        base_time_ns,
        header_comment,
        body,
        footer,
    )
}

pub(crate) fn encode_ingest_i64_file_inner(input: I64FileInput) -> Result<Vec<u8>> {
    validate_rows(&input.schema, &input.rows)?;
    let mut stats = IngestStats::new_for_schema(&input.schema)?;
    for row in &input.rows {
        stats.observe_i64_record(&input.schema, row)?;
    }
    let timestamp_index = timestamp_field_index(&input.schema);
    if let Some(timestamp_index) = timestamp_index {
        observe_timestamp_runs(&mut stats, &input.rows, timestamp_index);
    }

    let aura0_plan = Aura0Plan::from_schema_rows_stats(&input.schema, &stats, &input.rows)?;
    let aura1_plan = Aura1Plan::from_stats(&stats, 1);
    let generic_aura0_plan = plan_generic_i64_rows(&input.schema, &input.rows).ok();
    let mut footer = AuraFooter::new(input.schema.clone(), stats)
        .with_aura0_plan(aura0_plan)
        .with_aura1_plan(aura1_plan);
    if let Some(generic_aura0_plan) = generic_aura0_plan {
        footer = footer.with_generic_aura0_plan(generic_aura0_plan);
    }
    let body = encode_raw_body(input.schema.fields.len(), &input.rows)?;
    let base_time_ns = timestamp_index
        .and_then(|index| input.rows.first().and_then(|row| row.get(index)).copied())
        .unwrap_or(0);
    let header_comment = input.header_comment.as_deref().unwrap_or("");

    encode_file(
        Profile::Ingest,
        input.stream_id,
        input.dictionary_id,
        base_time_ns,
        header_comment,
        body,
        footer,
    )
}

struct PreparedI64Events {
    stream_id: u16,
    dictionary_id: u16,
    base_time_ns: i64,
    header_comment: String,
    body: Vec<u8>,
    footer: AuraFooter,
}

fn prepare_i64_events(input: I64EventFileInput) -> Result<PreparedI64Events> {
    let event_values = input
        .events
        .iter()
        .map(|event| event.event_values.clone())
        .collect::<Vec<_>>();
    let children = input
        .events
        .iter()
        .map(|event| event.children.clone())
        .collect::<Vec<_>>();
    let encoded = encode_generic_i64_events(&input.schema, &event_values, &children)?;
    let rows = flatten_i64_events(&input.schema, &input.events)?;
    let mut stats = IngestStats::new_for_schema(&input.schema)?;
    let event_slots = input
        .schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Event)
        .map(|field| field.index)
        .collect::<Vec<_>>();
    let repeated_slots = input
        .schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Repeated)
        .map(|field| field.index)
        .collect::<Vec<_>>();
    for event in &input.events {
        for (slot, value) in event_slots.iter().copied().zip(&event.event_values) {
            stats.observe_i64(slot, *value)?;
        }
        for related in &mut stats.related_fields {
            let field_scope = input.schema.fields[usize::from(related.field_index)].scope;
            let parent_scope = input.schema.fields[usize::from(related.related_field_index)].scope;
            if field_scope == FieldScope::Event && parent_scope == FieldScope::Event {
                let field_position = event_slots
                    .iter()
                    .position(|slot| *slot == related.field_index)
                    .ok_or(AuraError::InvalidValue("related field index"))?;
                let parent_position = event_slots
                    .iter()
                    .position(|slot| *slot == related.related_field_index)
                    .ok_or(AuraError::InvalidValue("related field index"))?;
                related.observe(
                    event.event_values[field_position],
                    event.event_values[parent_position],
                );
            }
        }
        for child in &event.children {
            stats.observe_record();
            for (slot, value) in repeated_slots.iter().copied().zip(child) {
                stats.observe_i64(slot, *value)?;
            }
            for related in &mut stats.related_fields {
                let field_scope = input.schema.fields[usize::from(related.field_index)].scope;
                let parent_scope =
                    input.schema.fields[usize::from(related.related_field_index)].scope;
                if field_scope == FieldScope::Event && parent_scope == FieldScope::Event {
                    continue;
                }
                let value_for = |slot: u16, scope: FieldScope| -> Result<i64> {
                    let (slots, values) = if scope == FieldScope::Event {
                        (&event_slots, &event.event_values)
                    } else {
                        (&repeated_slots, child)
                    };
                    let position = slots
                        .iter()
                        .position(|candidate| *candidate == slot)
                        .ok_or(AuraError::InvalidValue("related field index"))?;
                    values
                        .get(position)
                        .copied()
                        .ok_or(AuraError::InvalidValue("related field index"))
                };
                related.observe(
                    value_for(related.field_index, field_scope)?,
                    value_for(related.related_field_index, parent_scope)?,
                );
            }
        }
    }
    let timestamp_index = timestamp_field_index(&input.schema);
    if let Some(timestamp_index) = timestamp_index {
        observe_timestamp_runs(&mut stats, &rows, timestamp_index);
    }
    let aura0_plan = Aura0Plan::from_schema_rows_stats(&input.schema, &stats, &rows)?;
    let aura1_plan = Aura1Plan::from_stats(&stats, 1);
    let footer = AuraFooter::new(input.schema.clone(), stats)
        .with_aura0_plan(aura0_plan)
        .with_aura1_plan(aura1_plan)
        .with_generic_aura0_plan(encoded.plan.clone());
    let body = encode_generic_i64_rows_body(&encoded)?;
    let timestamp_event_index = timestamp_index.and_then(|slot| {
        input
            .schema
            .fields
            .iter()
            .filter(|field| field.scope == FieldScope::Event)
            .position(|field| usize::from(field.index) == slot)
    });
    let base_time_ns = timestamp_event_index
        .and_then(|index| input.events.first()?.event_values.get(index).copied())
        .unwrap_or(0);
    Ok(PreparedI64Events {
        stream_id: input.stream_id,
        dictionary_id: input.dictionary_id,
        base_time_ns,
        header_comment: input.header_comment.unwrap_or_default(),
        body,
        footer,
    })
}

pub(crate) fn encode_ingest_i64_events_file_inner(input: I64EventFileInput) -> Result<Vec<u8>> {
    let prepared = prepare_i64_events(input)?;
    encode_file(
        Profile::Ingest,
        prepared.stream_id,
        prepared.dictionary_id,
        prepared.base_time_ns,
        &prepared.header_comment,
        prepared.body,
        prepared.footer,
    )
}

fn flatten_i64_events(schema: &SchemaDescriptor, events: &[I64Event]) -> Result<Vec<Vec<i64>>> {
    let event_slots = schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Event)
        .map(|field| usize::from(field.index))
        .collect::<Vec<_>>();
    let repeated_slots = schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Repeated)
        .map(|field| usize::from(field.index))
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    for event in events {
        if event.event_values.len() != event_slots.len()
            || event
                .children
                .iter()
                .any(|child| child.len() != repeated_slots.len())
        {
            return Err(AuraError::InvalidValue("event field count"));
        }
        for child in &event.children {
            let mut row = vec![0i64; schema.fields.len()];
            for (slot, value) in event_slots.iter().copied().zip(&event.event_values) {
                row[slot] = *value;
            }
            for (slot, value) in repeated_slots.iter().copied().zip(child) {
                row[slot] = *value;
            }
            rows.push(row);
        }
    }
    validate_rows(schema, &rows)?;
    Ok(rows)
}

pub fn compile_i64_file(bytes: &[u8], target_profile: Profile) -> Result<Vec<u8>> {
    crate::writer::compile_i64(bytes, target_profile)
}

/// Compile a typed file containing flat `Opaque16` fields into Aura0 or Aura1.
///
/// Opaque values remain one logical field and are encoded by the generic UUID
/// constant-mask operation in Aura0 and one final 16-byte slot in Aura1. Wide
/// arithmetic (`I128`) and repeated-field schemas remain unsupported until they
/// have an exact typed reconstruction path of their own.
pub fn compile_typed_file(bytes: &[u8], target_profile: Profile) -> Result<Vec<u8>> {
    if !schema_has_wide_fields_from_sealed_file(bytes)? {
        return compile_i64_file(bytes, target_profile);
    }
    compile_typed_file_inner(bytes, target_profile)
}

pub fn compile_i64_file_with_aura0_profile(
    bytes: &[u8],
    profile: Aura0FileProfile,
    byte_lane_codec: Aura0ByteLaneCodec,
) -> Result<Vec<u8>> {
    match profile {
        Aura0FileProfile::Compact => compile_i64_file_inner(bytes, Profile::Aura0),
        Aura0FileProfile::Fast | Aura0FileProfile::Hybrid => {
            let aura1 = if sealed_profile(bytes)? == Profile::Aura1 {
                bytes.to_vec()
            } else {
                compile_i64_file_inner(bytes, Profile::Aura1)?
            };
            let aura1_parts = parse_compiled_file_parts(&aura1, Profile::Aura1)?;
            let (lane_body, lane_descriptor) = encode_aura1_byte_lane_file(
                byte_lane_codec,
                &aura1,
                0,
                aura1_parts.footer.record_count,
            )?;

            if profile == Aura0FileProfile::Fast {
                let footer = aura1_parts
                    .footer
                    .clone()
                    .with_aura1_byte_lanes(vec![lane_descriptor]);
                return encode_compiled_file(
                    Profile::Aura0,
                    aura1_parts.header.stream_id,
                    aura1_parts.header.dictionary_id,
                    aura1_parts.header.base_time_ns,
                    aura1_parts.header.comment.as_str(),
                    lane_body,
                    footer,
                );
            }

            let compact = compile_i64_file_inner(&aura1, Profile::Aura0)?;
            let compact_parts = parse_compiled_file_parts(&compact, Profile::Aura0)?;
            let mut body = compact_parts.body.to_vec();
            let lane_offset = u64::try_from(body.len())
                .map_err(|_| AuraError::InvalidValue("byte lane offset"))?;
            let (lane_body, mut lane_descriptor) = encode_aura1_byte_lane_file(
                byte_lane_codec,
                &aura1,
                lane_offset,
                compact_parts.footer.record_count,
            )?;
            body.extend_from_slice(&lane_body);
            lane_descriptor.compressed_offset = lane_offset;
            let footer = compact_parts
                .footer
                .clone()
                .with_aura1_byte_lanes(vec![lane_descriptor]);
            encode_compiled_file(
                Profile::Aura0,
                compact_parts.header.stream_id,
                compact_parts.header.dictionary_id,
                compact_parts.header.base_time_ns,
                compact_parts.header.comment.as_str(),
                body,
                footer,
            )
        }
    }
}

pub fn compile_aura0_to_aura1_bytes_with_lane(
    bytes: &[u8],
    use_byte_lane: Aura0ByteLaneUse,
    verify_byte_lane: bool,
) -> Result<Vec<u8>> {
    let offsets = parse_sealed_file_offsets(bytes, Profile::Aura0)?;
    let body = &bytes[offsets.header_len..offsets.footer_start];
    let footer_bytes = &bytes[offsets.footer_start..offsets.footer_len_offset];
    match use_byte_lane {
        Aura0ByteLaneUse::Auto | Aura0ByteLaneUse::Always => {
            if let Some(output) = try_decode_aura1_byte_lane_from_footer_tail(
                body,
                footer_bytes,
                offsets.header.container_version,
                verify_byte_lane,
            )? {
                return Ok(output);
            }
            if use_byte_lane == Aura0ByteLaneUse::Always {
                return Err(AuraError::InvalidValue("aura0 byte lane"));
            }
        }
        Aura0ByteLaneUse::Never => {}
    }

    let parts = parse_compiled_file_parts(bytes, Profile::Aura0)?;
    let semantic_len = aura0_semantic_body_len(&parts.footer, parts.body.len())?;
    if semantic_len == 0 {
        return Err(AuraError::InvalidValue("aura0 semantic lane"));
    }
    let mut footer = parts.footer.clone();
    footer.aura1_byte_lanes.clear();
    let compact = encode_compiled_file(
        Profile::Aura0,
        parts.header.stream_id,
        parts.header.dictionary_id,
        parts.header.base_time_ns,
        parts.header.comment.as_str(),
        parts.body[..semantic_len].to_vec(),
        footer,
    )?;
    compile_i64_file_inner(&compact, Profile::Aura1)
}

pub fn try_compile_i64_file_with_fused_output_guard(
    bytes: &[u8],
    target_profile: Profile,
) -> Result<Option<GuardedCompileOutput>> {
    if target_profile != Profile::Aura1 {
        return Ok(None);
    }
    if bytes.len() < LEGACY_HEADER_PREFIX_SIZE + FOOTER_LEN_SIZE + SEAL_MAGIC.len() {
        return Ok(None);
    }
    let seal_offset = bytes.len() - SEAL_MAGIC.len();
    if &bytes[seal_offset..] != SEAL_MAGIC {
        return Ok(None);
    }
    let footer_len_offset = seal_offset - FOOTER_LEN_SIZE;
    let footer_len = read_trailer_footer_len(bytes, footer_len_offset)?;
    let header_len = AuraHeader::encoded_len(bytes)?;
    if header_len > footer_len_offset {
        return Err(AuraError::UnexpectedEof);
    }
    let header = AuraHeader::decode(&bytes[..header_len])?;
    if header.profile != Profile::Aura0 {
        return Ok(None);
    }
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_start < header_len {
        return Err(AuraError::UnexpectedEof);
    }
    try_compile_aura0_to_aura1_fast_guarded(
        bytes,
        header,
        header_len,
        footer_start,
        footer_len_offset,
    )
}

pub fn try_compile_i64_file_profiled(
    bytes: &[u8],
    target_profile: Profile,
    guard_mode: OutputGuardMode,
    transcode_path: TranscodePath,
    encoder_path: Aura0EncoderPath,
    decode_path: Aura0DecodePath,
) -> Result<Option<ProfiledCompileOutput>> {
    Ok(try_compile_i64_file_profiled_with_trace(
        bytes,
        target_profile,
        guard_mode,
        transcode_path,
        encoder_path,
        decode_path,
    )?
    .map(|output| output.profiled))
}

pub fn try_compile_i64_file_profiled_with_trace(
    bytes: &[u8],
    target_profile: Profile,
    guard_mode: OutputGuardMode,
    transcode_path: TranscodePath,
    encoder_path: Aura0EncoderPath,
    decode_path: Aura0DecodePath,
) -> Result<Option<ProfiledCompileTraceOutput>> {
    let Some(parts) = profiled_compile_parts(bytes)? else {
        return Ok(None);
    };
    match (parts.header.profile, target_profile, transcode_path) {
        (Profile::Aura0, Profile::Aura1, TranscodePath::Auto | TranscodePath::Direct) => {
            Ok(try_compile_aura0_to_aura1_fast_profiled(
                bytes,
                parts.header,
                parts.header_len,
                parts.footer_start,
                parts.footer_len_offset,
                guard_mode,
                decode_path,
            )?
            .map(
                |(profiled, effective_aura1_body_path)| ProfiledCompileTraceOutput {
                    profiled,
                    effective_aura1_body_path: Some(effective_aura1_body_path),
                },
            ))
        }
        (Profile::Aura1, Profile::Aura0, TranscodePath::Direct) => {
            Ok(try_compile_aura1_to_aura0_direct_profiled(
                bytes,
                parts.header,
                parts.header_len,
                parts.footer_start,
                parts.footer_len_offset,
                guard_mode,
                encoder_path,
            )?
            .map(|profiled| ProfiledCompileTraceOutput {
                profiled,
                effective_aura1_body_path: None,
            }))
        }
        _ => Ok(None),
    }
}

pub fn try_compile_i64_file_profiled_with_options(
    bytes: &[u8],
    target_profile: Profile,
    guard_mode: OutputGuardMode,
    transcode_path: TranscodePath,
    options: Aura1ExecutionOptions,
) -> Result<Option<ProfiledCompileWithOptionsOutput>> {
    try_compile_i64_file_profiled_with_options_and_lane_use(
        bytes,
        target_profile,
        guard_mode,
        transcode_path,
        options,
        Aura0ByteLaneUse::Auto,
    )
}

pub fn try_compile_i64_file_profiled_with_options_and_lane_use(
    bytes: &[u8],
    target_profile: Profile,
    guard_mode: OutputGuardMode,
    transcode_path: TranscodePath,
    options: Aura1ExecutionOptions,
    lane_use: Aura0ByteLaneUse,
) -> Result<Option<ProfiledCompileWithOptionsOutput>> {
    if target_profile != Profile::Aura1
        || !matches!(transcode_path, TranscodePath::Auto | TranscodePath::Direct)
    {
        return Err(AuraError::InvalidValue(
            "Aura1 execution options require Aura0 to Aura1",
        ));
    }
    let Some(parts) = profiled_compile_parts(bytes)? else {
        return Ok(None);
    };
    if parts.header.profile != Profile::Aura0 {
        return Err(AuraError::InvalidValue(
            "Aura1 execution options require Aura0 to Aura1",
        ));
    }

    let fallback_to_stable = |reason: &'static str| {
        if lane_use == Aura0ByteLaneUse::Never
            && matches!(
                guard_mode,
                OutputGuardMode::NoGuard | OutputGuardMode::OldPostOutputGuard
            )
        {
            let started = Instant::now();
            let output = try_compile_aura0_to_aura1_with_execution_options(
                bytes,
                parts.header.clone(),
                parts.header_len,
                parts.footer_start,
                parts.footer_len_offset,
                Aura1BodyPath::StableAuto,
                options.column_path,
                Aura0ByteLaneUse::Never,
            )?;
            let Some(output) = output else {
                return Err(AuraError::InvalidValue("stable Aura1 fallback unsupported"));
            };
            return Ok(Some(profile_execution_output(
                output,
                options,
                guard_mode,
                started,
                Some(reason),
            )));
        }
        let attempt = try_compile_aura0_to_aura1_fast_profiled(
            bytes,
            parts.header.clone(),
            parts.header_len,
            parts.footer_start,
            parts.footer_len_offset,
            guard_mode,
            Aura0DecodePath::Materialized,
        )?;
        let Some((profiled, effective_body_path)) = attempt else {
            return Err(AuraError::InvalidValue("stable Aura1 fallback unsupported"));
        };
        Ok(Some(ProfiledCompileWithOptionsOutput {
            profiled,
            execution: Aura1ExecutionTrace {
                requested_body_path: options.body_path,
                effective_body_path,
                requested_column_path: options.column_path,
                effective_column_path: None,
                unsupported_path: options.unsupported_path,
                fallback_reason: Some(reason),
            },
        }))
    };

    if options.body_path != Aura1BodyPath::StableAuto && lane_use == Aura0ByteLaneUse::Always {
        return Err(AuraError::InvalidValue(
            "embedded byte lane conflicts with requested semantic path",
        ));
    }

    if options.body_path == Aura1BodyPath::StableAuto
        && (lane_use != Aura0ByteLaneUse::Never
            || matches!(
                guard_mode,
                OutputGuardMode::FusedOutputGuard | OutputGuardMode::BlockBatchedOutputGuard
            ))
    {
        let attempt = try_compile_aura0_to_aura1_fast_profiled(
            bytes,
            parts.header.clone(),
            parts.header_len,
            parts.footer_start,
            parts.footer_len_offset,
            guard_mode,
            Aura0DecodePath::Materialized,
        )?;
        if lane_use == Aura0ByteLaneUse::Always
            && attempt
                .as_ref()
                .is_none_or(|(_, effective)| *effective != Aura1EffectiveBodyPath::EmbeddedByteLane)
        {
            return Err(AuraError::InvalidValue("embedded byte lane required"));
        }
        if let Some((profiled, effective_body_path)) = attempt {
            return Ok(Some(ProfiledCompileWithOptionsOutput {
                profiled,
                execution: Aura1ExecutionTrace {
                    requested_body_path: options.body_path,
                    effective_body_path,
                    requested_column_path: options.column_path,
                    effective_column_path: (effective_body_path == Aura1EffectiveBodyPath::Columns)
                        .then_some(options.column_path),
                    unsupported_path: options.unsupported_path,
                    fallback_reason: None,
                },
            }));
        }
        if options.unsupported_path == UnsupportedPathBehavior::Error {
            return Err(AuraError::InvalidValue(
                "requested stable Aura1 guard/path unsupported",
            ));
        }
        if matches!(
            guard_mode,
            OutputGuardMode::FusedOutputGuard | OutputGuardMode::BlockBatchedOutputGuard
        ) {
            if lane_use == Aura0ByteLaneUse::Never {
                return Err(AuraError::InvalidValue("stable Aura1 fallback unsupported"));
            }
            let fallback = try_compile_aura0_to_aura1_fast_profiled(
                bytes,
                parts.header.clone(),
                parts.header_len,
                parts.footer_start,
                parts.footer_len_offset,
                OutputGuardMode::OldPostOutputGuard,
                Aura0DecodePath::Materialized,
            )?;
            let Some((profiled, effective_body_path)) = fallback else {
                return Err(AuraError::InvalidValue("stable Aura1 fallback unsupported"));
            };
            return Ok(Some(ProfiledCompileWithOptionsOutput {
                profiled,
                execution: Aura1ExecutionTrace {
                    requested_body_path: options.body_path,
                    effective_body_path,
                    requested_column_path: options.column_path,
                    effective_column_path: None,
                    unsupported_path: options.unsupported_path,
                    fallback_reason: Some(
                        "requested guard mode unsupported; used old post-output guard",
                    ),
                },
            }));
        }
        return Ok(None);
    }

    if matches!(
        guard_mode,
        OutputGuardMode::FusedOutputGuard | OutputGuardMode::BlockBatchedOutputGuard
    ) {
        if options.unsupported_path == UnsupportedPathBehavior::Error {
            return Err(AuraError::InvalidValue(
                "requested Aura1 body path does not support guard mode",
            ));
        }
        return fallback_to_stable("requested Aura1 body path does not support guard mode");
    }

    let started = Instant::now();
    let output = try_compile_aura0_to_aura1_with_execution_options(
        bytes,
        parts.header.clone(),
        parts.header_len,
        parts.footer_start,
        parts.footer_len_offset,
        options.body_path,
        options.column_path,
        lane_use,
    )?;
    let Some(output) = output else {
        if options.unsupported_path == UnsupportedPathBehavior::Error {
            return Err(AuraError::InvalidValue(
                "requested Aura1 body path unsupported",
            ));
        }
        return fallback_to_stable("requested Aura1 body path unsupported");
    };
    Ok(Some(profile_execution_output(
        output, options, guard_mode, started, None,
    )))
}

fn profile_execution_output(
    output: Aura1ExecutionOutput,
    options: Aura1ExecutionOptions,
    guard_mode: OutputGuardMode,
    started: Instant,
    fallback_reason: Option<&'static str>,
) -> ProfiledCompileWithOptionsOutput {
    let guard_start = Instant::now();
    let output_byte_guard = (guard_mode == OutputGuardMode::OldPostOutputGuard)
        .then(|| bytes_guard_value(&output.bytes));
    let post_output_guard_ns = if output_byte_guard.is_some() {
        guard_start.elapsed().as_nanos()
    } else {
        0
    };
    ProfiledCompileWithOptionsOutput {
        profiled: ProfiledCompileOutput {
            bytes: output.bytes,
            output_byte_guard,
            guard_mode,
            transcode_path: TranscodePath::Direct,
            encoder_path: Aura0EncoderPath::Materialized,
            conversion_plan_hash: output.conversion_plan_hash,
            timings: ProfiledCompileTimings {
                aura0_to_aura1: Some(DirectAura1TranscodeTimings {
                    total_ns: started.elapsed().as_nanos(),
                    post_output_guard_ns,
                    ..DirectAura1TranscodeTimings::default()
                }),
                aura1_to_aura0: None,
            },
            stats: ProfiledCompileStats::default(),
        },
        execution: Aura1ExecutionTrace {
            requested_body_path: options.body_path,
            effective_body_path: output.effective_path,
            requested_column_path: options.column_path,
            effective_column_path: (output.effective_path == Aura1EffectiveBodyPath::Columns)
                .then_some(options.column_path),
            unsupported_path: options.unsupported_path,
            fallback_reason,
        },
    }
}

#[derive(Debug)]
struct ProfiledCompileParts {
    header: AuraHeader,
    header_len: usize,
    footer_start: usize,
    footer_len_offset: usize,
}

fn profiled_compile_parts(bytes: &[u8]) -> Result<Option<ProfiledCompileParts>> {
    if bytes.len() < LEGACY_HEADER_PREFIX_SIZE + FOOTER_LEN_SIZE + SEAL_MAGIC.len() {
        return Ok(None);
    }
    let seal_offset = bytes.len() - SEAL_MAGIC.len();
    if &bytes[seal_offset..] != SEAL_MAGIC {
        return Ok(None);
    }
    let footer_len_offset = seal_offset - FOOTER_LEN_SIZE;
    let footer_len = read_trailer_footer_len(bytes, footer_len_offset)?;
    let header_len = AuraHeader::encoded_len(bytes)?;
    if header_len > footer_len_offset {
        return Err(AuraError::UnexpectedEof);
    }
    let header = AuraHeader::decode(&bytes[..header_len])?;
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_start < header_len {
        return Err(AuraError::UnexpectedEof);
    }
    Ok(Some(ProfiledCompileParts {
        header,
        header_len,
        footer_start,
        footer_len_offset,
    }))
}

pub(crate) fn compile_i64_file_inner(bytes: &[u8], target_profile: Profile) -> Result<Vec<u8>> {
    if target_profile == Profile::Ingest {
        return Err(AuraError::InvalidValue("target profile"));
    }
    if let Some(compiled) = try_compile_explicit_i64_events(bytes, target_profile)? {
        return Ok(compiled);
    }
    if let Some(compiled) = try_compile_i64_fast(bytes, target_profile)? {
        return Ok(compiled);
    }
    let decoded = decode_i64_file_inner(bytes)?;
    let compiled_footer = decoded.compiled_footer_for_compile()?;
    let body = match target_profile {
        Profile::Ingest => unreachable!(),
        Profile::Aura0 => {
            if let Some(plan) = &compiled_footer.generic_aura0_plan {
                let encoded = encode_generic_i64_rows_with_plan(
                    &decoded.schema,
                    &decoded.rows,
                    plan.clone(),
                )?;
                encode_generic_i64_rows_body(&encoded)?
            } else {
                let plan = decoded.aura0_plan()?;
                encode_aura0_body(&decoded.rows, &plan)?
            }
        }
        Profile::Aura1 => {
            let plan = decoded.aura1_plan()?;
            encode_aura1_body(&decoded.rows, &plan)?
        }
    };
    let out = encode_compiled_file(
        target_profile,
        decoded.header.stream_id,
        decoded.header.dictionary_id,
        decoded.header.base_time_ns,
        decoded.header.comment.as_str(),
        body,
        compiled_footer,
    )?;
    Ok(out)
}

fn try_compile_explicit_i64_events(
    bytes: &[u8],
    target_profile: Profile,
) -> Result<Option<Vec<u8>>> {
    let metadata = decode_i64_file_metadata(bytes)?;
    let plan = metadata
        .ingest_footer
        .as_ref()
        .and_then(|footer| footer.generic_aura0_plan.clone())
        .or_else(|| {
            metadata
                .compiled_footer
                .as_ref()
                .and_then(|footer| footer.generic_aura0_plan.clone())
        });
    let Some(plan) = plan else {
        return Ok(None);
    };
    if explicit_event_group(&plan).is_none() {
        return Ok(None);
    }
    if metadata.header.profile == target_profile {
        return Ok(Some(bytes.to_vec()));
    }
    let full_body = &bytes[metadata.header_len..metadata.footer_start];
    if metadata.header.profile == Profile::Aura0 {
        let footer = metadata
            .compiled_footer
            .as_ref()
            .ok_or(AuraError::InvalidValue("compiled footer"))?;
        if aura0_semantic_body_len(footer, full_body.len())? == 0 {
            let aura1 = decode_aura1_byte_lanes_from_body(full_body, footer, false)?;
            decode_i64_events_file(&aura1)?;
            return Ok(Some(aura1));
        }
    }
    let events = match metadata.header.profile {
        Profile::Ingest => {
            let decoded = decode_generic_i64_events_body(
                &metadata.schema,
                plan.clone(),
                full_body,
                metadata.record_count,
            )?;
            decoded
                .event_values
                .into_iter()
                .zip(decoded.children)
                .map(|(event_values, children)| I64Event {
                    event_values,
                    children,
                })
                .collect::<Vec<_>>()
        }
        Profile::Aura0 => {
            let footer = metadata
                .compiled_footer
                .as_ref()
                .ok_or(AuraError::InvalidValue("compiled footer"))?;
            let semantic_len = aura0_semantic_body_len(footer, full_body.len())?;
            let decoded = decode_generic_i64_events_body(
                &metadata.schema,
                plan.clone(),
                &full_body[..semantic_len],
                metadata.record_count,
            )?;
            decoded
                .event_values
                .into_iter()
                .zip(decoded.children)
                .map(|(event_values, children)| I64Event {
                    event_values,
                    children,
                })
                .collect::<Vec<_>>()
        }
        Profile::Aura1 => {
            let (fixed_body, sidecar) = split_explicit_event_sidecar(full_body, Some(&plan))?;
            let footer = metadata
                .compiled_footer
                .as_ref()
                .ok_or(AuraError::InvalidValue("compiled footer"))?;
            let aura1_plan = footer.aura1_program.to_aura1_plan(footer.block_capacity)?;
            let rows = decode_aura1_body(
                fixed_body,
                &aura1_plan,
                metadata.record_count,
                metadata.schema.fields.len(),
            )?;
            decode_explicit_event_sidecar(
                &metadata.schema,
                &rows,
                sidecar.ok_or(AuraError::InvalidValue("explicit event sidecar"))?,
            )?
        }
    };
    let event_values = events
        .iter()
        .map(|event| event.event_values.clone())
        .collect::<Vec<_>>();
    let children = events
        .iter()
        .map(|event| event.children.clone())
        .collect::<Vec<_>>();
    let encoded = encode_generic_i64_events(&metadata.schema, &event_values, &children)?;
    let compact_body = encode_generic_i64_rows_body(&encoded)?;
    let rows = flatten_i64_events(&metadata.schema, &events)?;
    let mut footer = if let Some(footer) = metadata.compiled_footer.clone() {
        footer
    } else {
        compiled_footer_from_ingest_footer(
            metadata
                .ingest_footer
                .as_ref()
                .ok_or(AuraError::InvalidValue("ingest footer"))?,
            metadata.record_count,
        )?
    };
    footer.generic_aura0_plan = Some(encoded.plan);
    let body = match target_profile {
        Profile::Aura0 => compact_body,
        Profile::Aura1 => {
            let aura1_plan = footer.aura1_program.to_aura1_plan(footer.block_capacity)?;
            let event_field_count = metadata
                .schema
                .fields
                .iter()
                .filter(|field| field.scope == FieldScope::Event)
                .count();
            append_explicit_event_sidecar(
                encode_aura1_body(&rows, &aura1_plan)?,
                &events,
                event_field_count,
            )?
        }
        Profile::Ingest => unreachable!(),
    };
    encode_compiled_file(
        target_profile,
        metadata.header.stream_id,
        metadata.header.dictionary_id,
        metadata.header.base_time_ns,
        metadata.header.comment.as_str(),
        body,
        footer,
    )
    .map(Some)
}

fn compile_typed_file_inner(bytes: &[u8], target_profile: Profile) -> Result<Vec<u8>> {
    if target_profile == Profile::Ingest {
        return Err(AuraError::InvalidValue("typed target profile"));
    }
    if sealed_profile(bytes)? == target_profile {
        return Ok(bytes.to_vec());
    }
    let decoded = decode_typed_file_inner(bytes)?;
    validate_flat_opaque16_schema(&decoded.schema)?;

    let mut rows = Vec::with_capacity(decoded.rows.len());
    for typed_row in &decoded.rows {
        let mut row = Vec::with_capacity(typed_row.len());
        for (field, value) in decoded.schema.fields.iter().zip(typed_row) {
            row.push(match field.field_type {
                FieldType::Opaque16 => 0,
                FieldType::I128 => return Err(AuraError::InvalidValue("typed i128 compiled")),
                _ => typed_value_i64_for_field(field.field_type, value)?,
            });
        }
        rows.push(row);
    }

    let (body, generic_aura0_plan) = match target_profile {
        Profile::Aura0 => encode_typed_aura0_body(&decoded.schema, &decoded.rows, &rows)?,
        Profile::Aura1 => {
            let plan = absolute_typed_field_plans(&decoded.schema, decoded.rows.len())?;
            let body = encode_typed_aura1_body(&decoded.schema, &decoded.rows, &plan)?;
            let generic_plan = decoded
                .compiled_footer
                .as_ref()
                .and_then(|footer| footer.generic_aura0_plan.clone());
            (body, generic_plan)
        }
        Profile::Ingest => unreachable!(),
    };
    let footer = compiled_typed_footer(
        decoded.header.container_version,
        &decoded.schema,
        decoded.rows.len(),
        generic_aura0_plan,
    )?;
    encode_compiled_file(
        target_profile,
        decoded.header.stream_id,
        decoded.header.dictionary_id,
        decoded.header.base_time_ns,
        decoded.header.comment.as_str(),
        body,
        footer,
    )
}

fn encode_typed_aura0_body(
    schema: &SchemaDescriptor,
    typed_rows: &[Vec<AuraTypedValue>],
    placeholder_rows: &[Vec<i64>],
) -> Result<(Vec<u8>, Option<GenericInstructionPlan>)> {
    let mut encoded = encode_generic_i64_rows(schema, placeholder_rows)?;
    if !encoded.plan.groups.is_empty() {
        return Err(AuraError::InvalidValue("typed aura0 group plan"));
    }
    for field in schema
        .fields
        .iter()
        .filter(|field| field.field_type == FieldType::Opaque16)
    {
        let instruction_indexes = encoded
            .plan
            .streams
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| {
                (instruction.target_slot == Some(field.index)).then_some(index)
            })
            .collect::<Vec<_>>();
        if instruction_indexes.len() != 1 {
            return Err(AuraError::InvalidValue("opaque16 stream"));
        }
        let instruction_index = instruction_indexes[0];
        let stream_id = encoded.plan.streams[instruction_index].stream_id;
        let values = typed_rows
            .iter()
            .map(|row| match row.get(usize::from(field.index)) {
                Some(AuraTypedValue::Opaque16(value)) => Ok(u128::from_le_bytes(*value)),
                _ => Err(AuraError::InvalidValue("typed value")),
            })
            .collect::<Result<Vec<_>>>()?;
        let instruction = plan_uuid_const_mask_stream(stream_id, Some(field.index), &values)?;
        let body = encode_generic_stream_body(&instruction, &GenericStreamBodyValue::U128(values))?;
        encoded.plan.streams[instruction_index] = instruction;
        let stream = encoded
            .streams
            .iter_mut()
            .find(|stream| stream.stream_id == stream_id)
            .ok_or(AuraError::InvalidValue("opaque16 stream"))?;
        stream.value_count = typed_rows.len();
        stream.body = body;
    }
    let body = encode_generic_i64_rows_body(&encoded)?;
    Ok((body, Some(encoded.plan)))
}

fn compiled_typed_footer(
    container_version: AuraContainerVersion,
    schema: &SchemaDescriptor,
    record_count: usize,
    generic_aura0_plan: Option<GenericInstructionPlan>,
) -> Result<CompiledFooter> {
    let container_version = conversion_output_container_version(container_version)?;
    let field_plans = absolute_typed_field_plans(schema, record_count)?;
    let aura0_plan = Aura0Plan {
        fields: field_plans.clone(),
    };
    let aura1_plan = Aura1Plan {
        block_capacity: 1,
        fields: field_plans,
    };
    let mut footer = CompiledFooter::new(
        schema.clone(),
        u64::try_from(record_count).map_err(|_| AuraError::InvalidValue("record count"))?,
        1,
        DecodeProgram::from_aura0_plan(&aura0_plan, schema.fields.len())?,
        DecodeProgram::from_aura1_plan(&aura1_plan, schema.fields.len())?,
    )?
    .with_container_version(container_version);
    if let Some(plan) = generic_aura0_plan {
        footer = footer.with_generic_aura0_plan(plan);
    }
    Ok(footer)
}

fn try_compile_i64_fast(bytes: &[u8], target_profile: Profile) -> Result<Option<Vec<u8>>> {
    if target_profile != Profile::Aura1 {
        return Ok(None);
    }
    if bytes.len() < LEGACY_HEADER_PREFIX_SIZE + FOOTER_LEN_SIZE + SEAL_MAGIC.len() {
        return Ok(None);
    }
    let seal_offset = bytes.len() - SEAL_MAGIC.len();
    if &bytes[seal_offset..] != SEAL_MAGIC {
        return Ok(None);
    }
    let footer_len_offset = seal_offset - FOOTER_LEN_SIZE;
    let footer_len = read_trailer_footer_len(bytes, footer_len_offset)?;
    let header_len = AuraHeader::encoded_len(bytes)?;
    if header_len > footer_len_offset {
        return Err(AuraError::UnexpectedEof);
    }
    let header = AuraHeader::decode(&bytes[..header_len])?;
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_start < header_len {
        return Err(AuraError::UnexpectedEof);
    }
    if header.profile == Profile::Aura0 {
        return try_compile_aura0_to_aura1_fast(
            bytes,
            header,
            header_len,
            footer_start,
            footer_len_offset,
        );
    }
    if header.profile != Profile::Ingest {
        return Ok(None);
    }
    let footer = AuraFooter::decode(&bytes[footer_start..footer_len_offset])?;
    validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
    if schema_has_wide_fields(&footer.schema) {
        return Err(AuraError::InvalidValue("i64 schema"));
    }
    let aura1_plan = footer
        .aura1_plan
        .clone()
        .ok_or(AuraError::InvalidValue("aura1 plan"))?;
    let (body, record_count) = encode_aura1_body_from_raw_body(
        &bytes[header_len..footer_start],
        &footer.schema,
        &aura1_plan,
    )?;
    let compiled_footer = compiled_footer_from_ingest_footer(&footer, record_count)?;
    Ok(Some(encode_compiled_file(
        Profile::Aura1,
        header.stream_id,
        header.dictionary_id,
        header.base_time_ns,
        header.comment.as_str(),
        body,
        compiled_footer,
    )?))
}

fn try_compile_aura0_to_aura1_fast(
    bytes: &[u8],
    header: AuraHeader,
    header_len: usize,
    footer_start: usize,
    footer_len_offset: usize,
) -> Result<Option<Vec<u8>>> {
    Ok(try_compile_aura0_to_aura1_with_execution_options(
        bytes,
        header,
        header_len,
        footer_start,
        footer_len_offset,
        Aura1BodyPath::StableAuto,
        Aura0ColumnPath::Materialized,
        Aura0ByteLaneUse::Auto,
    )?
    .map(|output| output.bytes))
}

#[derive(Debug)]
struct Aura1ExecutionOutput {
    bytes: Vec<u8>,
    effective_path: Aura1EffectiveBodyPath,
    conversion_plan_hash: Option<u64>,
}

#[allow(clippy::too_many_arguments)]
fn try_compile_aura0_to_aura1_with_execution_options(
    bytes: &[u8],
    header: AuraHeader,
    header_len: usize,
    footer_start: usize,
    footer_len_offset: usize,
    body_path: Aura1BodyPath,
    column_path: Aura0ColumnPath,
    lane_use: Aura0ByteLaneUse,
) -> Result<Option<Aura1ExecutionOutput>> {
    if body_path == Aura1BodyPath::StableAuto && lane_use != Aura0ByteLaneUse::Never {
        if let Some(out) = try_decode_aura1_byte_lane_from_footer_tail(
            &bytes[header_len..footer_start],
            &bytes[footer_start..footer_len_offset],
            header.container_version,
            false,
        )? {
            return Ok(Some(Aura1ExecutionOutput {
                bytes: out,
                effective_path: Aura1EffectiveBodyPath::EmbeddedByteLane,
                conversion_plan_hash: None,
            }));
        }
        if lane_use == Aura0ByteLaneUse::Always {
            return Err(AuraError::InvalidValue("embedded byte lane required"));
        }
    }
    let footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
    let _ = validate_compiled_i64_decode_limits(&footer, footer_start - header_len)?;
    validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
    if compiled_footer_has_explicit_events(&footer) {
        return Ok(None);
    }
    if schema_has_wide_fields(&footer.schema) {
        return Err(AuraError::InvalidValue("i64 schema"));
    }
    let Some(plan) = footer.generic_aura0_plan.clone() else {
        return Ok(None);
    };
    let record_count = usize::try_from(footer.record_count)
        .map_err(|_| AuraError::InvalidValue("record count"))?;
    let field_count = footer.schema.fields.len();
    let semantic_len = aura0_semantic_body_len(&footer, footer_start - header_len)?;
    if semantic_len == 0 {
        return Ok(None);
    }
    let aura1_plan = footer.aura1_program.to_aura1_plan(footer.block_capacity)?;
    let conversion_plan_hash = Some(CompiledAuraPlan::from_footer(&footer)?.conversion_plan_hash);
    let mut output_footer = footer.clone();
    output_footer.aura1_byte_lanes.clear();

    let try_path = |path: Aura1BodyPath| -> Result<Option<Vec<u8>>> {
        let semantic_body = &bytes[header_len..header_len + semantic_len];
        if path == Aura1BodyPath::DirectFile {
            let plan = plan.clone();
            let aura1_plan = aura1_plan.clone();
            let footer = output_footer.clone();
            let header = header.clone();
            let body_capacity = aura1_body_capacity(record_count, &aura1_plan)?;
            return Ok(try_encode_compiled_file_with_body_writer(
                Profile::Aura1,
                header.stream_id,
                header.dictionary_id,
                header.base_time_ns,
                header.comment.as_str(),
                body_capacity,
                footer,
                |out| {
                    try_write_generic_i64_aura1_body(
                        plan.clone(),
                        semantic_body,
                        record_count,
                        field_count,
                        &aura1_plan,
                        out,
                    )
                },
            )?
            .map(|(out, _body_len)| out));
        }

        let body = match path {
            Aura1BodyPath::StableAuto | Aura1BodyPath::DirectFile => unreachable!(),
            Aura1BodyPath::StreamingCursor => try_encode_generic_i64_aura1_body_streaming(
                plan.clone(),
                semantic_body,
                record_count,
                field_count,
                &aura1_plan,
            )?,
            Aura1BodyPath::DirectStreams => try_encode_generic_i64_aura1_body(
                plan.clone(),
                semantic_body,
                record_count,
                field_count,
                &aura1_plan,
            )?,
            Aura1BodyPath::Columns => try_decode_generic_i64_columns_body(
                plan.clone(),
                semantic_body,
                record_count,
                field_count,
                column_path,
            )?
            .map(|columns| encode_aura1_body_from_columns(&columns, &aura1_plan))
            .transpose()?,
        };
        body.map(|body| {
            encode_compiled_file(
                Profile::Aura1,
                header.stream_id,
                header.dictionary_id,
                header.base_time_ns,
                header.comment.as_str(),
                body,
                output_footer.clone(),
            )
        })
        .transpose()
    };

    let requested = body_path;
    if requested == Aura1BodyPath::StableAuto {
        for path in [Aura1BodyPath::DirectFile, Aura1BodyPath::Columns] {
            if let Some(bytes) = try_path(path)? {
                return Ok(Some(Aura1ExecutionOutput {
                    bytes,
                    effective_path: effective_body_path(path),
                    conversion_plan_hash,
                }));
            }
        }
        return Ok(None);
    }
    if let Some(bytes) = try_path(requested)? {
        return Ok(Some(Aura1ExecutionOutput {
            bytes,
            effective_path: effective_body_path(requested),
            conversion_plan_hash,
        }));
    }
    Ok(None)
}

const fn effective_body_path(path: Aura1BodyPath) -> Aura1EffectiveBodyPath {
    match path {
        Aura1BodyPath::StableAuto => unreachable!(),
        Aura1BodyPath::DirectFile => Aura1EffectiveBodyPath::DirectFile,
        Aura1BodyPath::StreamingCursor => Aura1EffectiveBodyPath::StreamingCursor,
        Aura1BodyPath::DirectStreams => Aura1EffectiveBodyPath::DirectStreams,
        Aura1BodyPath::Columns => Aura1EffectiveBodyPath::Columns,
    }
}

fn try_compile_aura0_to_aura1_fast_guarded(
    bytes: &[u8],
    header: AuraHeader,
    header_len: usize,
    footer_start: usize,
    footer_len_offset: usize,
) -> Result<Option<GuardedCompileOutput>> {
    let mut footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
    let _ = validate_compiled_i64_decode_limits(&footer, footer_start - header_len)?;
    validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
    if compiled_footer_has_explicit_events(&footer) {
        return Ok(None);
    }
    if schema_has_wide_fields(&footer.schema) {
        return Err(AuraError::InvalidValue("i64 schema"));
    }
    let semantic_len = aura0_semantic_body_len(&footer, footer_start - header_len)?;
    if semantic_len == 0 {
        return Ok(None);
    }
    let semantic_body = &bytes[header_len..header_len + semantic_len];
    footer.aura1_byte_lanes.clear();
    let compiled_plan = CompiledAuraPlan::from_footer(&footer)?;
    let Some(plan) = compiled_plan.generic_aura0_plan.clone() else {
        return Ok(None);
    };
    let record_count = compiled_plan.record_count;
    let field_count = compiled_plan.field_count;
    let aura1_plan = compiled_plan.aura1_plan.clone();
    let body_capacity = compiled_plan.aura1_body_size;
    let mut guard = ByteGuard::new();
    let Some((bytes, _body_len)) = try_encode_compiled_file_with_body_writer_guarded(
        Profile::Aura1,
        header.stream_id,
        header.dictionary_id,
        header.base_time_ns,
        header.comment.as_str(),
        body_capacity,
        footer,
        &mut guard,
        |out, output_guard| {
            try_write_generic_i64_aura1_body_guarded(
                plan,
                semantic_body,
                record_count,
                field_count,
                &aura1_plan,
                out,
                output_guard,
            )
        },
    )?
    else {
        return Ok(None);
    };
    Ok(Some(GuardedCompileOutput {
        bytes,
        guard: guard.value(),
    }))
}

#[allow(clippy::too_many_arguments)]
fn try_compile_aura0_to_aura1_fast_profiled(
    bytes: &[u8],
    header: AuraHeader,
    header_len: usize,
    footer_start: usize,
    footer_len_offset: usize,
    guard_mode: OutputGuardMode,
    decode_path: Aura0DecodePath,
) -> Result<Option<(ProfiledCompileOutput, Aura1EffectiveBodyPath)>> {
    let total_start = Instant::now();
    let mut timings = DirectAura1TranscodeTimings::default();
    let mut stats = DirectAura1TranscodeStats::default();

    let metadata_start = Instant::now();
    let byte_lane_start = Instant::now();
    let lane_guard_compatible = matches!(
        guard_mode,
        OutputGuardMode::NoGuard | OutputGuardMode::OldPostOutputGuard
    );
    if lane_guard_compatible {
        if let Some(out) = try_decode_aura1_byte_lane_from_footer_tail(
            &bytes[header_len..footer_start],
            &bytes[footer_start..footer_len_offset],
            header.container_version,
            guard_mode == OutputGuardMode::OldPostOutputGuard,
        )? {
            let byte_lane_ns = byte_lane_start.elapsed().as_nanos();
            timings.metadata_ns = metadata_start
                .elapsed()
                .as_nanos()
                .saturating_sub(byte_lane_ns);
            timings.decode_input_streams.total_ns = byte_lane_ns;
            timings.decode_input_streams.allocation_reuse_ns = byte_lane_ns;
            timings.decode_input_streams.close_sum();
            stats.decode.direct_cursor_stream_count = 1;
            stats.decode.direct_cursor_value_count = out.len();
            stats.writer.output_slices = 1;
            stats.writer.non_contiguous_writes = 1;
            stats.writer.copied_bytes = out.len();
            stats.writer.guard_update_bytes = if guard_mode == OutputGuardMode::NoGuard {
                0
            } else {
                out.len()
            };
            stats.writer.guard_update_calls = if guard_mode == OutputGuardMode::NoGuard {
                0
            } else {
                1
            };
            let mut output_byte_guard = None;
            if guard_mode != OutputGuardMode::NoGuard {
                let guard_start = Instant::now();
                output_byte_guard = Some(bytes_guard_value(&out));
                timings.post_output_guard_ns = guard_start.elapsed().as_nanos();
            }
            timings.total_ns = total_start.elapsed().as_nanos();
            return Ok(Some((
                ProfiledCompileOutput {
                    bytes: out,
                    output_byte_guard,
                    guard_mode,
                    transcode_path: TranscodePath::Direct,
                    encoder_path: Aura0EncoderPath::Materialized,
                    conversion_plan_hash: None,
                    timings: ProfiledCompileTimings {
                        aura0_to_aura1: Some(timings),
                        aura1_to_aura0: None,
                    },
                    stats: ProfiledCompileStats {
                        aura0_to_aura1: Some(stats),
                        aura1_to_aura0: None,
                    },
                },
                Aura1EffectiveBodyPath::EmbeddedByteLane,
            )));
        }
    }
    let mut footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
    let _ = validate_compiled_i64_decode_limits(&footer, footer_start - header_len)?;
    validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
    if compiled_footer_has_explicit_events(&footer) {
        return Ok(None);
    }
    if schema_has_wide_fields(&footer.schema) {
        return Err(AuraError::InvalidValue("i64 schema"));
    }
    let semantic_len = aura0_semantic_body_len(&footer, footer_start - header_len)?;
    if semantic_len == 0 {
        return Ok(None);
    }
    let semantic_body = &bytes[header_len..header_len + semantic_len];
    footer.aura1_byte_lanes.clear();
    let compiled_plan = CompiledAuraPlan::from_footer(&footer)?;
    let Some(plan) = compiled_plan.generic_aura0_plan.clone() else {
        return Ok(None);
    };
    let record_count = compiled_plan.record_count;
    let field_count = compiled_plan.field_count;
    let aura1_plan = compiled_plan.aura1_plan.clone();
    let body_capacity = compiled_plan.aura1_body_size;
    let footer_bytes = footer.encode()?;
    let footer_len =
        u32::try_from(footer_bytes.len()).map_err(|_| AuraError::InvalidValue("footer length"))?;
    let header_out = AuraHeader::new(Profile::Aura1)
        .with_container_version(conversion_output_container_version(
            footer.container_version,
        )?)
        .with_stream(header.stream_id, header.dictionary_id, header.base_time_ns)
        .with_schema_mapping(schema_parent_mapping(&footer.schema)?)?
        .with_derived_expressions(footer.schema.derived_expressions.clone())?
        .with_comment(header.comment.as_str())?;
    let header_bytes = header_out.encode()?;
    timings.metadata_ns = metadata_start.elapsed().as_nanos();

    let allocate_start = Instant::now();
    let mut out = Vec::with_capacity(
        header_bytes.len()
            + body_capacity
            + footer_bytes.len()
            + FOOTER_LEN_SIZE
            + SEAL_MAGIC.len(),
    );
    out.extend_from_slice(&header_bytes);
    timings.allocate_header_ns = allocate_start.elapsed().as_nanos();

    let mut guard = ByteGuard::new();
    let mut output_byte_guard = None;
    let guard_active = matches!(
        guard_mode,
        OutputGuardMode::FusedOutputGuard | OutputGuardMode::BlockBatchedOutputGuard
    );
    if guard_active {
        guard.update(&header_bytes);
    }

    let body_start = out.len();
    let writer_supported = if decode_path == Aura0DecodePath::Cursor {
        let cursor_start = Instant::now();
        let body_start_len = out.len();
        let writer_supported = try_write_generic_i64_aura1_body_streaming(
            plan.clone(),
            semantic_body,
            record_count,
            field_count,
            &aura1_plan,
            &mut out,
        )?;
        let cursor_ns = cursor_start.elapsed().as_nanos();
        if !writer_supported {
            return Ok(None);
        }
        let body_len = out.len().saturating_sub(body_start_len);
        timings.decode_input_streams.total_ns = 0;
        timings.decode_input_streams.close_sum();
        timings.partitioned_sparse_writer.total_ns = cursor_ns;
        timings.partitioned_sparse_writer.output_byte_stores_ns = cursor_ns;
        timings.partitioned_sparse_writer.close_sum();
        stats.decode.stream_count = plan.streams.len();
        stats.decode.stream_value_count = stream_value_count_from_body(semantic_body)?;
        stats.decode.direct_cursor_stream_count = stats.decode.stream_count;
        stats.decode.direct_cursor_value_count = stats.decode.stream_value_count;
        stats.writer.partition_count = 1;
        stats.writer.records_per_partition_min = record_count;
        stats.writer.records_per_partition_max = record_count;
        stats.writer.output_slices = 1;
        stats.writer.non_contiguous_writes = 1;
        stats.writer.temporary_buffer_bytes = 0;
        stats.writer.copied_bytes = 0;
        stats.writer.allocation_count = 0;
        if guard_mode == OutputGuardMode::FusedOutputGuard {
            let guard_start = Instant::now();
            guard.update(&out[body_start..]);
            let guard_ns = guard_start.elapsed().as_nanos();
            timings.partitioned_sparse_writer.guard_hash_update_ns = timings
                .partitioned_sparse_writer
                .guard_hash_update_ns
                .saturating_add(guard_ns);
            timings.partitioned_sparse_writer.total_ns = timings
                .partitioned_sparse_writer
                .total_ns
                .saturating_add(guard_ns);
            timings.partitioned_sparse_writer.close_sum();
            stats.writer.guard_update_calls = stats.writer.guard_update_calls.saturating_add(1);
            stats.writer.guard_update_bytes =
                stats.writer.guard_update_bytes.saturating_add(body_len);
        }
        true
    } else {
        let stream_values = decode_generic_i64_stream_values_profiled(
            &plan,
            semantic_body,
            Some(&mut timings.decode_input_streams),
        )?;
        stats.decode.stream_count = stream_values.len();
        stats.decode.stream_value_count = stream_values.values().map(Vec::len).sum();
        stats.decode.materialized_stream_count = stats.decode.stream_count;
        stats.decode.materialized_value_count = stats.decode.stream_value_count;

        let writer_supported = match guard_mode {
            OutputGuardMode::FusedOutputGuard => {
                try_write_partitioned_sparse_i64_aura1_body_profiled(
                    &plan,
                    &stream_values,
                    record_count,
                    field_count,
                    &aura1_plan,
                    &mut out,
                    Some(&mut guard),
                    &mut timings.partitioned_sparse_writer,
                    &mut stats.writer,
                )?
            }
            OutputGuardMode::NoGuard
            | OutputGuardMode::OldPostOutputGuard
            | OutputGuardMode::BlockBatchedOutputGuard => {
                try_write_partitioned_sparse_i64_aura1_body_profiled(
                    &plan,
                    &stream_values,
                    record_count,
                    field_count,
                    &aura1_plan,
                    &mut out,
                    None,
                    &mut timings.partitioned_sparse_writer,
                    &mut stats.writer,
                )?
            }
        };
        if writer_supported {
            true
        } else {
            match guard_mode {
                OutputGuardMode::FusedOutputGuard => {
                    try_write_generic_i64_aura1_body_from_streams_profiled(
                        &plan,
                        &stream_values,
                        record_count,
                        field_count,
                        &aura1_plan,
                        &mut out,
                        Some(&mut guard),
                        &mut timings.partitioned_sparse_writer,
                        &mut stats.writer,
                    )?
                }
                OutputGuardMode::NoGuard
                | OutputGuardMode::OldPostOutputGuard
                | OutputGuardMode::BlockBatchedOutputGuard => {
                    try_write_generic_i64_aura1_body_from_streams_profiled(
                        &plan,
                        &stream_values,
                        record_count,
                        field_count,
                        &aura1_plan,
                        &mut out,
                        None,
                        &mut timings.partitioned_sparse_writer,
                        &mut stats.writer,
                    )?
                }
            }
        }
    };
    if !writer_supported {
        return Ok(None);
    }
    let body_end = out.len();
    if guard_mode == OutputGuardMode::BlockBatchedOutputGuard {
        let guard_start = Instant::now();
        guard.update(&out[body_start..body_end]);
        let guard_ns = guard_start.elapsed().as_nanos();
        timings.partitioned_sparse_writer.guard_hash_update_ns = timings
            .partitioned_sparse_writer
            .guard_hash_update_ns
            .saturating_add(guard_ns);
        timings.partitioned_sparse_writer.total_ns = timings
            .partitioned_sparse_writer
            .total_ns
            .saturating_add(guard_ns);
        timings.partitioned_sparse_writer.close_sum();
        stats.writer.guard_update_calls = stats.writer.guard_update_calls.saturating_add(1);
        stats.writer.guard_update_bytes = stats
            .writer
            .guard_update_bytes
            .saturating_add(body_end - body_start);
    }

    let trailer_start = Instant::now();
    out.extend_from_slice(&footer_bytes);
    if guard_active {
        guard.update(&footer_bytes);
    }
    let footer_len_bytes = footer_len.to_le_bytes();
    out.extend_from_slice(&footer_len_bytes);
    if guard_active {
        guard.update(&footer_len_bytes);
    }
    out.extend_from_slice(SEAL_MAGIC);
    if guard_active {
        guard.update(SEAL_MAGIC);
        output_byte_guard = Some(guard.value());
    }
    timings.trailer_ns = trailer_start.elapsed().as_nanos();

    if guard_mode == OutputGuardMode::OldPostOutputGuard {
        let guard_start = Instant::now();
        let mut post_guard = ByteGuard::new();
        post_guard.update(&out);
        timings.post_output_guard_ns = guard_start.elapsed().as_nanos();
        output_byte_guard = Some(post_guard.value());
    }

    timings.total_ns = total_start.elapsed().as_nanos();
    let effective_path = if decode_path == Aura0DecodePath::Cursor {
        Aura1EffectiveBodyPath::StreamingCursor
    } else {
        Aura1EffectiveBodyPath::DirectStreams
    };
    Ok(Some((
        ProfiledCompileOutput {
            bytes: out,
            output_byte_guard,
            guard_mode,
            transcode_path: TranscodePath::Direct,
            encoder_path: Aura0EncoderPath::Materialized,
            conversion_plan_hash: Some(compiled_plan.conversion_plan_hash),
            timings: ProfiledCompileTimings {
                aura0_to_aura1: Some(timings),
                aura1_to_aura0: None,
            },
            stats: ProfiledCompileStats {
                aura0_to_aura1: Some(stats),
                aura1_to_aura0: None,
            },
        },
        effective_path,
    )))
}

fn stream_value_count_from_body(bytes: &[u8]) -> Result<usize> {
    let mut reader = ByteReader::new(bytes);
    let stream_count = reader.read_u16_le()? as usize;
    let mut value_count = 0usize;
    for _ in 0..stream_count {
        let _stream_id = reader.read_u16_le()?;
        value_count = value_count
            .checked_add(
                usize::try_from(reader.read_u64_le()?)
                    .map_err(|_| AuraError::InvalidValue("stream value count"))?,
            )
            .ok_or(AuraError::InvalidValue("stream value count"))?;
        let body_len = reader.read_u32_le()? as usize;
        let _body = reader.read_exact(body_len)?;
    }
    reader.finish()?;
    Ok(value_count)
}

fn try_compile_aura1_to_aura0_direct_profiled(
    bytes: &[u8],
    header: AuraHeader,
    header_len: usize,
    footer_start: usize,
    footer_len_offset: usize,
    guard_mode: OutputGuardMode,
    encoder_path: Aura0EncoderPath,
) -> Result<Option<ProfiledCompileOutput>> {
    let total_start = Instant::now();
    let mut timings = DirectAura0TranscodeTimings::default();
    let mut stats = DirectAura0TranscodeStats::default();

    let metadata_start = Instant::now();
    let footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
    let _ = validate_compiled_i64_decode_limits(&footer, footer_start - header_len)?;
    validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
    if compiled_footer_has_explicit_events(&footer) {
        return Ok(None);
    }
    if schema_has_wide_fields(&footer.schema) {
        return Err(AuraError::InvalidValue("i64 schema"));
    }
    let compiled_plan = CompiledAuraPlan::from_footer(&footer)?;
    let Some(plan) = compiled_plan.generic_aura0_plan.clone() else {
        return Ok(None);
    };
    let record_count = compiled_plan.record_count;
    let field_count = compiled_plan.field_count;
    let aura1_plan = compiled_plan.aura1_plan.clone();
    validate_aura1_plan_fields(&aura1_plan, field_count)?;
    timings.metadata_ns = metadata_start.elapsed().as_nanos();

    if encoder_path == Aura0EncoderPath::ColumnFree {
        return Err(AuraError::InvalidValue(
            "column-free encoder rejected: current Aura1->Aura0 encoder API requires Aura1 column buffers for dictionary/Huffman stream construction",
        ));
    }

    let body = &bytes[header_len..footer_start];
    stats.bytes_read = body.len();
    let scan_start = Instant::now();
    let columns = decode_aura1_body_to_columns(body, &aura1_plan, record_count, field_count)?;
    timings.fixed_row_scan_ns = scan_start.elapsed().as_nanos();
    stats.rows_scanned = record_count;
    stats.aura1_scan_passes = 1;
    stats.encoder_path = encoder_path;
    stats.direct_streams_enabled = encoder_path == Aura0EncoderPath::DirectStreams;
    stats.column_vector_count = columns.len();
    stats.column_value_count = columns.iter().map(Vec::len).sum();

    let encode_start = Instant::now();
    let mut column_stats = GenericColumnEncodeStats::default();
    let encoded = match encoder_path {
        Aura0EncoderPath::Materialized => encode_generic_i64_columns_with_plan(
            &footer.schema,
            &columns,
            record_count,
            plan,
            Some(&mut column_stats),
        )?,
        Aura0EncoderPath::DirectStreams => encode_generic_i64_columns_with_plan_direct_streams(
            &footer.schema,
            &columns,
            record_count,
            plan,
            Some(&mut column_stats),
        )?,
        Aura0EncoderPath::ColumnFree => unreachable!("column-free path returns before columns"),
    };
    let body_assembly_start = Instant::now();
    let aura0_body = encode_generic_i64_rows_body(&encoded)?;
    let body_assembly_ns = body_assembly_start.elapsed().as_nanos();
    let encode_total_ns = encode_start.elapsed().as_nanos();
    timings.stats_frequency_collection_ns = column_stats.frequency_pass_ns;
    timings.direct_stream_construction_ns = column_stats.direct_stream_emit_ns;
    timings.compression_encoding_ns = if encoder_path == Aura0EncoderPath::DirectStreams {
        column_stats
            .compression_encoding_ns
            .saturating_add(body_assembly_ns)
    } else {
        encode_total_ns
    };
    stats.stream_vector_allocations = column_stats.stream_vector_allocations;
    stats.stream_vector_count = column_stats.stream_vector_allocations;
    stats.stream_count = column_stats.stream_count;
    stats.stream_value_count = column_stats.stream_value_count;
    stats.direct_stream_count = column_stats.direct_stream_count;
    stats.direct_stream_value_count = column_stats.direct_stream_value_count;
    stats.temporary_buffer_bytes = column_stats.temporary_buffer_bytes;
    stats.encoder_allocations = column_stats.encoder_allocations;
    stats.compression_blocks = column_stats.stream_count;
    stats.copied_bytes = aura0_body.len();

    let writer_start = Instant::now();
    let out = encode_compiled_file(
        Profile::Aura0,
        header.stream_id,
        header.dictionary_id,
        header.base_time_ns,
        header.comment.as_str(),
        aura0_body,
        footer,
    )?;
    timings.writer_finalization_ns = writer_start.elapsed().as_nanos();
    stats.bytes_written = out.len();
    stats.output_blocks = 1;

    let mut output_byte_guard = None;
    if guard_mode != OutputGuardMode::NoGuard {
        let guard_start = Instant::now();
        let mut guard = ByteGuard::new();
        guard.update(&out);
        timings.post_output_guard_ns = guard_start.elapsed().as_nanos();
        output_byte_guard = Some(guard.value());
    }

    timings.total_ns = total_start.elapsed().as_nanos();
    Ok(Some(ProfiledCompileOutput {
        bytes: out,
        output_byte_guard,
        guard_mode,
        transcode_path: TranscodePath::Direct,
        encoder_path,
        conversion_plan_hash: Some(compiled_plan.conversion_plan_hash),
        timings: ProfiledCompileTimings {
            aura0_to_aura1: None,
            aura1_to_aura0: Some(timings),
        },
        stats: ProfiledCompileStats {
            aura0_to_aura1: None,
            aura1_to_aura0: Some(stats),
        },
    }))
}

fn validate_aura1_plan_fields(plan: &Aura1Plan, field_count: usize) -> Result<()> {
    if plan.fields.len() != field_count {
        return Err(AuraError::InvalidValue("program field count"));
    }
    let mut seen = vec![false; field_count];
    for field in &plan.fields {
        let index = usize::from(field.field_index);
        if index >= field_count || seen[index] {
            return Err(AuraError::InvalidValue("field index"));
        }
        seen[index] = true;
    }
    if seen.iter().any(|seen| !*seen) {
        return Err(AuraError::InvalidValue("program field count"));
    }
    Ok(())
}

fn decode_aura1_body_to_columns(
    bytes: &[u8],
    plan: &Aura1Plan,
    record_count: usize,
    field_count: usize,
) -> Result<Vec<Vec<i64>>> {
    let _ = validate_i64_decode_dimensions_usize(record_count, field_count, bytes.len())?;
    validate_aura1_plan_fields(plan, field_count)?;
    if bytes.len() != aura1_body_capacity(record_count, plan)? {
        return Err(AuraError::InvalidValue("aura1 body length"));
    }
    let mut reader = ByteReader::new(bytes);
    let mut columns = Vec::new();
    try_reserve_exact(&mut columns, field_count, "i64 decode column allocation")?;
    for _ in 0..field_count {
        let mut column = Vec::new();
        try_reserve_exact(
            &mut column,
            record_count,
            "i64 decode column value allocation",
        )?;
        columns.push(column);
    }
    for _ in 0..record_count {
        for field_plan in &plan.fields {
            let index = usize::from(field_plan.field_index);
            columns[index].push(read_i64_width(&mut reader, field_plan.width)?);
        }
    }
    reader.finish()?;
    if columns.iter().any(|column| column.len() != record_count) {
        return Err(AuraError::InvalidValue("record count"));
    }
    Ok(columns)
}

pub fn decode_i64_file(bytes: &[u8]) -> Result<DecodedI64File> {
    crate::reader::decode_i64(bytes)
}

pub fn decode_i64_events_file(bytes: &[u8]) -> Result<DecodedI64EventFile> {
    let metadata = decode_i64_file_metadata(bytes)?;
    let plan = metadata
        .ingest_footer
        .as_ref()
        .and_then(|footer| footer.generic_aura0_plan.clone())
        .or_else(|| {
            metadata
                .compiled_footer
                .as_ref()
                .and_then(|footer| footer.generic_aura0_plan.clone())
        })
        .ok_or(AuraError::InvalidValue("explicit event plan"))?;
    if explicit_event_group(&plan).is_none() {
        return Err(AuraError::InvalidValue("explicit event plan"));
    }
    let body = &bytes[metadata.header_len..metadata.footer_start];
    if metadata.header.profile == Profile::Aura0 {
        let footer = metadata
            .compiled_footer
            .as_ref()
            .ok_or(AuraError::InvalidValue("compiled footer"))?;
        if aura0_semantic_body_len(footer, body.len())? == 0 {
            let aura1 = decode_aura1_byte_lanes_from_body(body, footer, false)?;
            return decode_i64_events_file(&aura1);
        }
    }
    let events = match metadata.header.profile {
        Profile::Ingest => {
            decode_compact_i64_events(&metadata.schema, plan, body, metadata.record_count)?
        }
        Profile::Aura0 => {
            let footer = metadata
                .compiled_footer
                .as_ref()
                .ok_or(AuraError::InvalidValue("compiled footer"))?;
            let semantic_len = aura0_semantic_body_len(footer, body.len())?;
            decode_compact_i64_events(
                &metadata.schema,
                plan,
                &body[..semantic_len],
                metadata.record_count,
            )?
        }
        Profile::Aura1 => {
            let (fixed_body, sidecar) = split_explicit_event_sidecar(body, Some(&plan))?;
            let sidecar = sidecar.ok_or(AuraError::InvalidValue("explicit event sidecar"))?;
            let footer = metadata
                .compiled_footer
                .as_ref()
                .ok_or(AuraError::InvalidValue("compiled footer"))?;
            let aura1_plan = footer.aura1_program.to_aura1_plan(footer.block_capacity)?;
            let rows = decode_aura1_body(
                fixed_body,
                &aura1_plan,
                metadata.record_count,
                metadata.schema.fields.len(),
            )?;
            decode_explicit_event_sidecar(&metadata.schema, &rows, sidecar)?
        }
    };
    Ok(DecodedI64EventFile {
        header: metadata.header,
        schema: metadata.schema,
        ingest_footer: metadata.ingest_footer,
        compiled_footer: metadata.compiled_footer,
        events,
    })
}

fn decode_compact_i64_events(
    schema: &SchemaDescriptor,
    plan: GenericInstructionPlan,
    body: &[u8],
    record_count: usize,
) -> Result<Vec<I64Event>> {
    let decoded = decode_generic_i64_events_body(schema, plan, body, record_count)?;
    Ok(decoded
        .event_values
        .into_iter()
        .zip(decoded.children)
        .map(|(event_values, children)| I64Event {
            event_values,
            children,
        })
        .collect())
}

pub fn decode_i64_file_metadata(bytes: &[u8]) -> Result<DecodedI64FileMetadata> {
    if bytes.len() < LEGACY_HEADER_PREFIX_SIZE + FOOTER_LEN_SIZE + SEAL_MAGIC.len() {
        return Err(AuraError::UnexpectedEof);
    }
    let seal_offset = bytes.len() - SEAL_MAGIC.len();
    if &bytes[seal_offset..] != SEAL_MAGIC {
        return Err(AuraError::InvalidMagic {
            expected: "sealed:)",
        });
    }
    let footer_len_offset = seal_offset - FOOTER_LEN_SIZE;
    let footer_len = read_trailer_footer_len(bytes, footer_len_offset)?;
    let header_len = AuraHeader::encoded_len(bytes)?;
    if header_len > footer_len_offset {
        return Err(AuraError::UnexpectedEof);
    }
    let header = AuraHeader::decode(&bytes[..header_len])?;
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_start < header_len {
        return Err(AuraError::UnexpectedEof);
    }
    match header.profile {
        Profile::Ingest => {
            let footer = AuraFooter::decode(&bytes[footer_start..footer_len_offset])?;
            validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
            if schema_has_wide_fields(&footer.schema) {
                return Err(AuraError::InvalidValue("i64 schema"));
            }
            let record_count = if footer
                .generic_aura0_plan
                .as_ref()
                .is_some_and(|plan| explicit_event_group(plan).is_some())
            {
                validate_i64_decode_dimensions(
                    footer.stats.record_count,
                    footer.schema.fields.len(),
                    footer_start - header_len,
                )?
                .0
            } else {
                let (record_count, field_count) =
                    preflight_raw_i64_body(&bytes[header_len..footer_start])?;
                if u64::try_from(record_count)
                    .map_err(|_| AuraError::InvalidValue("record count"))?
                    != footer.stats.record_count
                    || field_count != footer.schema.fields.len()
                {
                    return Err(AuraError::InvalidValue("raw body dimensions"));
                }
                record_count
            };
            Ok(DecodedI64FileMetadata {
                header,
                schema: footer.schema.clone(),
                ingest_footer: Some(footer),
                compiled_footer: None,
                record_count,
                header_len,
                footer_start,
                footer_len_offset,
            })
        }
        Profile::Aura0 | Profile::Aura1 => {
            let footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
            let record_count =
                validate_compiled_i64_decode_limits(&footer, footer_start - header_len)?;
            validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
            if schema_has_wide_fields(&footer.schema) {
                return Err(AuraError::InvalidValue("i64 schema"));
            }
            Ok(DecodedI64FileMetadata {
                header,
                schema: footer.schema.clone(),
                ingest_footer: None,
                compiled_footer: Some(footer),
                record_count,
                header_len,
                footer_start,
                footer_len_offset,
            })
        }
    }
}

pub fn decode_i64_columns_file(bytes: &[u8]) -> Result<Option<DecodedI64ColumnsFile>> {
    if bytes.len() < LEGACY_HEADER_PREFIX_SIZE + FOOTER_LEN_SIZE + SEAL_MAGIC.len() {
        return Err(AuraError::UnexpectedEof);
    }
    let seal_offset = bytes.len() - SEAL_MAGIC.len();
    if &bytes[seal_offset..] != SEAL_MAGIC {
        return Err(AuraError::InvalidMagic {
            expected: "sealed:)",
        });
    }
    let footer_len_offset = seal_offset - FOOTER_LEN_SIZE;
    let footer_len = read_trailer_footer_len(bytes, footer_len_offset)?;
    let header_len = AuraHeader::encoded_len(bytes)?;
    if header_len > footer_len_offset {
        return Err(AuraError::UnexpectedEof);
    }
    let header = AuraHeader::decode(&bytes[..header_len])?;
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_start < header_len {
        return Err(AuraError::UnexpectedEof);
    }
    if header.profile != Profile::Aura0 {
        return Ok(None);
    }

    let footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
    let record_count = validate_compiled_i64_decode_limits(&footer, footer_start - header_len)?;
    validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
    if schema_has_wide_fields(&footer.schema) {
        return Err(AuraError::InvalidValue("i64 schema"));
    }
    let Some(plan) = footer.generic_aura0_plan.clone() else {
        return Ok(None);
    };
    let field_count = footer.schema.fields.len();
    let semantic_len = aura0_semantic_body_len(&footer, footer_start - header_len)?;
    if semantic_len == 0 {
        return Ok(None);
    }
    let Some(columns) = try_decode_generic_i64_columns_body(
        plan,
        &bytes[header_len..header_len + semantic_len],
        record_count,
        field_count,
        Aura0ColumnPath::Materialized,
    )?
    else {
        return Ok(None);
    };
    validate_columns(&footer.schema, record_count, &columns)?;

    Ok(Some(DecodedI64ColumnsFile {
        header,
        schema: footer.schema.clone(),
        compiled_footer: footer,
        record_count,
        columns,
    }))
}

pub fn aura1_fixed_layout_info(bytes: &[u8]) -> Result<Aura1FixedLayoutInfo> {
    if bytes.len() < LEGACY_HEADER_PREFIX_SIZE + FOOTER_LEN_SIZE + SEAL_MAGIC.len() {
        return Err(AuraError::UnexpectedEof);
    }
    let seal_offset = bytes.len() - SEAL_MAGIC.len();
    if &bytes[seal_offset..] != SEAL_MAGIC {
        return Err(AuraError::InvalidMagic {
            expected: "sealed:)",
        });
    }
    let footer_len_offset = seal_offset - FOOTER_LEN_SIZE;
    let footer_len = read_trailer_footer_len(bytes, footer_len_offset)?;
    let header_len = AuraHeader::encoded_len(bytes)?;
    if header_len > footer_len_offset {
        return Err(AuraError::UnexpectedEof);
    }
    let header = AuraHeader::decode(&bytes[..header_len])?;
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_start < header_len {
        return Err(AuraError::UnexpectedEof);
    }
    if header.profile != Profile::Aura1 {
        return Err(AuraError::InvalidValue("aura1 fixed layout profile"));
    }

    let footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
    let _ = validate_compiled_i64_decode_limits(&footer, footer_start - header_len)?;
    validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
    if schema_has_wide_fields(&footer.schema) {
        return Err(AuraError::InvalidValue("i64 schema"));
    }
    if compiled_footer_has_explicit_events(&footer) {
        decode_i64_events_file(bytes)?;
    }
    let compiled_plan = CompiledAuraPlan::from_footer(&footer)?;
    let physical_body = &bytes[header_len..footer_start];
    let (fixed_body, _) =
        split_explicit_event_sidecar(physical_body, footer.generic_aura0_plan.as_ref())?;
    Ok(Aura1FixedLayoutInfo {
        record_count: compiled_plan.record_count,
        field_count: compiled_plan.field_count,
        record_width: compiled_plan.aura1_record_width,
        body_offset: header_len,
        body_bytes: fixed_body.len(),
        footer_offset: footer_start,
        output_size: bytes.len(),
        conversion_plan_hash: compiled_plan.conversion_plan_hash,
    })
}

pub fn visit_i64_rows_file<F>(bytes: &[u8], mut visitor: F) -> Result<usize>
where
    F: FnMut(&[i64]) -> Result<()>,
{
    if bytes.len() < LEGACY_HEADER_PREFIX_SIZE + FOOTER_LEN_SIZE + SEAL_MAGIC.len() {
        return Err(AuraError::UnexpectedEof);
    }
    let seal_offset = bytes.len() - SEAL_MAGIC.len();
    if &bytes[seal_offset..] != SEAL_MAGIC {
        return Err(AuraError::InvalidMagic {
            expected: "sealed:)",
        });
    }
    let footer_len_offset = seal_offset - FOOTER_LEN_SIZE;
    let footer_len = read_trailer_footer_len(bytes, footer_len_offset)?;
    let header_len = AuraHeader::encoded_len(bytes)?;
    if header_len > footer_len_offset {
        return Err(AuraError::UnexpectedEof);
    }
    let header = AuraHeader::decode(&bytes[..header_len])?;
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_start < header_len {
        return Err(AuraError::UnexpectedEof);
    }
    if header.profile != Profile::Aura1 {
        return Err(AuraError::InvalidValue("aura1 visitor profile"));
    }

    let footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
    let _ = validate_compiled_i64_decode_limits(&footer, footer_start - header_len)?;
    validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
    if schema_has_wide_fields(&footer.schema) {
        return Err(AuraError::InvalidValue("i64 schema"));
    }
    if compiled_footer_has_explicit_events(&footer) {
        decode_i64_events_file(bytes)?;
    }
    let compiled_plan = CompiledAuraPlan::from_footer(&footer)?;
    let physical_body = &bytes[header_len..footer_start];
    let (fixed_body, sidecar) =
        split_explicit_event_sidecar(physical_body, footer.generic_aura0_plan.as_ref())?;
    if compiled_footer_has_explicit_events(&footer) && sidecar.is_none() {
        return Err(AuraError::InvalidValue("explicit event sidecar"));
    }
    visit_aura1_body(
        fixed_body,
        &compiled_plan.aura1_plan,
        compiled_plan.record_count,
        compiled_plan.field_count,
        &mut visitor,
    )
}

pub fn visit_i64_rows_file_range<F>(
    bytes: &[u8],
    start_row: usize,
    max_rows: usize,
    mut visitor: F,
) -> Result<usize>
where
    F: FnMut(&[i64]) -> Result<()>,
{
    if bytes.len() < LEGACY_HEADER_PREFIX_SIZE + FOOTER_LEN_SIZE + SEAL_MAGIC.len() {
        return Err(AuraError::UnexpectedEof);
    }
    let seal_offset = bytes.len() - SEAL_MAGIC.len();
    if &bytes[seal_offset..] != SEAL_MAGIC {
        return Err(AuraError::InvalidMagic {
            expected: "sealed:)",
        });
    }
    let footer_len_offset = seal_offset - FOOTER_LEN_SIZE;
    let footer_len = read_trailer_footer_len(bytes, footer_len_offset)?;
    let header_len = AuraHeader::encoded_len(bytes)?;
    if header_len > footer_len_offset {
        return Err(AuraError::UnexpectedEof);
    }
    let header = AuraHeader::decode(&bytes[..header_len])?;
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_start < header_len {
        return Err(AuraError::UnexpectedEof);
    }
    if header.profile != Profile::Aura1 {
        return Err(AuraError::InvalidValue("aura1 visitor profile"));
    }

    let footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
    let _ = validate_compiled_i64_decode_limits(&footer, footer_start - header_len)?;
    validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
    if schema_has_wide_fields(&footer.schema) {
        return Err(AuraError::InvalidValue("i64 schema"));
    }
    if compiled_footer_has_explicit_events(&footer) {
        decode_i64_events_file(bytes)?;
    }
    let compiled_plan = CompiledAuraPlan::from_footer(&footer)?;
    if start_row > compiled_plan.record_count {
        return Err(AuraError::InvalidValue("row range"));
    }
    let rows_to_visit = max_rows.min(compiled_plan.record_count - start_row);
    if rows_to_visit == 0 {
        return Ok(0);
    }
    let body = &bytes[header_len..footer_start];
    if body.len() > MAX_V2_I64_DECODE_BODY_BYTES {
        return Err(AuraError::InvalidValue("i64 decode body limit"));
    }
    if matches!(header.profile, Profile::Aura0 | Profile::Aura1) {
        preflight_compiled_footer_record_count(
            &bytes[footer_start..footer_len_offset],
            body.len(),
        )?;
    }
    let byte_start = start_row
        .checked_mul(compiled_plan.aura1_record_width)
        .ok_or(AuraError::InvalidValue("row range"))?;
    let byte_len = rows_to_visit
        .checked_mul(compiled_plan.aura1_record_width)
        .ok_or(AuraError::InvalidValue("row range"))?;
    let byte_end = byte_start
        .checked_add(byte_len)
        .ok_or(AuraError::InvalidValue("row range"))?;
    let range = body
        .get(byte_start..byte_end)
        .ok_or(AuraError::UnexpectedEof)?;
    visit_aura1_body(
        range,
        &compiled_plan.aura1_plan,
        rows_to_visit,
        compiled_plan.field_count,
        &mut visitor,
    )
}

fn preflight_compiled_footer_record_count(footer: &[u8], body_len: usize) -> Result<()> {
    let mut reader = ByteReader::new(footer);
    if reader.read_exact(4)? != COMPILED_FOOTER_MAGIC {
        return Err(AuraError::InvalidMagic { expected: "AURP" });
    }
    let _container_version = AuraContainerVersion::from_wire(reader.read_u16_le()?)?;
    let _compression_kind = reader.read_u8()?;
    let _compression_level = reader.read_u8()?;
    let record_count = reader.read_u64_le()?;
    let _ = validate_i64_decode_dimensions(record_count, 1, body_len)?;
    Ok(())
}

pub fn decode_typed_file(bytes: &[u8]) -> Result<DecodedTypedFile> {
    crate::reader::decode_typed(bytes)
}

pub(crate) fn decode_i64_file_inner(bytes: &[u8]) -> Result<DecodedI64File> {
    if bytes.len() < LEGACY_HEADER_PREFIX_SIZE + FOOTER_LEN_SIZE + SEAL_MAGIC.len() {
        return Err(AuraError::UnexpectedEof);
    }
    let seal_offset = bytes.len() - SEAL_MAGIC.len();
    if &bytes[seal_offset..] != SEAL_MAGIC {
        return Err(AuraError::InvalidMagic {
            expected: "sealed:)",
        });
    }
    let footer_len_offset = seal_offset - FOOTER_LEN_SIZE;
    let footer_len = read_trailer_footer_len(bytes, footer_len_offset)?;
    let header_len = AuraHeader::encoded_len(bytes)?;
    if header_len > footer_len_offset {
        return Err(AuraError::UnexpectedEof);
    }
    let header = AuraHeader::decode(&bytes[..header_len])?;
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_start < header_len {
        return Err(AuraError::UnexpectedEof);
    }
    let body = &bytes[header_len..footer_start];
    if body.len() > MAX_V2_I64_DECODE_BODY_BYTES {
        return Err(AuraError::InvalidValue("i64 decode body limit"));
    }
    if matches!(header.profile, Profile::Aura0 | Profile::Aura1) {
        preflight_compiled_footer_record_count(
            &bytes[footer_start..footer_len_offset],
            body.len(),
        )?;
    }
    match header.profile {
        Profile::Ingest => {
            let footer = AuraFooter::decode(&bytes[footer_start..footer_len_offset])?;
            validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
            if schema_has_wide_fields(&footer.schema) {
                return Err(AuraError::InvalidValue("i64 schema"));
            }
            let (record_count, _) = validate_i64_decode_dimensions(
                footer.stats.record_count,
                footer.schema.fields.len(),
                body.len(),
            )?;
            let rows = if let Some(plan) = footer
                .generic_aura0_plan
                .clone()
                .filter(|plan| explicit_event_group(plan).is_some())
            {
                decode_generic_i64_rows_body(plan, body, record_count, footer.schema.fields.len())?
            } else {
                decode_raw_body(body)?
            };
            validate_rows(&footer.schema, &rows)?;
            Ok(DecodedI64File {
                header,
                schema: footer.schema.clone(),
                ingest_footer: Some(footer),
                compiled_footer: None,
                rows,
            })
        }
        Profile::Aura0 => {
            let footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
            let _ = validate_compiled_i64_decode_limits(&footer, body.len())?;
            validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
            if schema_has_wide_fields(&footer.schema) {
                return Err(AuraError::InvalidValue("i64 schema"));
            }
            let (record_count, _) = validate_i64_decode_dimensions(
                footer.record_count,
                footer.schema.fields.len(),
                body.len(),
            )?;
            let semantic_len = aura0_semantic_body_len(&footer, body.len())?;
            let rows = if semantic_len == 0 {
                let aura1 = decode_aura1_byte_lanes_from_body(body, &footer, false)?;
                return decode_i64_file_inner(&aura1);
            } else if let Some(plan) = footer.generic_aura0_plan.clone() {
                decode_generic_i64_rows_body(
                    plan,
                    &body[..semantic_len],
                    record_count,
                    footer.schema.fields.len(),
                )?
            } else {
                let plan = footer.aura0_program.to_aura0_plan()?;
                decode_aura0_body(
                    &body[..semantic_len],
                    &plan,
                    record_count,
                    footer.schema.fields.len(),
                )?
            };
            validate_rows(&footer.schema, &rows)?;
            Ok(DecodedI64File {
                header,
                schema: footer.schema.clone(),
                ingest_footer: None,
                compiled_footer: Some(footer),
                rows,
            })
        }
        Profile::Aura1 => {
            let footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
            let _ = validate_compiled_i64_decode_limits(&footer, body.len())?;
            validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
            if schema_has_wide_fields(&footer.schema) {
                return Err(AuraError::InvalidValue("i64 schema"));
            }
            let (record_count, _) = validate_i64_decode_dimensions(
                footer.record_count,
                footer.schema.fields.len(),
                body.len(),
            )?;
            let plan = footer.aura1_program.to_aura1_plan(footer.block_capacity)?;
            let (fixed_body, event_sidecar) =
                split_explicit_event_sidecar(body, footer.generic_aura0_plan.as_ref())?;
            if footer
                .generic_aura0_plan
                .as_ref()
                .and_then(explicit_event_group)
                .is_some()
                && event_sidecar.is_none()
            {
                return Err(AuraError::InvalidValue("explicit event sidecar"));
            }
            let expected_fixed_body = aura1_body_capacity(record_count, &plan)?;
            if fixed_body.len() != expected_fixed_body {
                return Err(AuraError::InvalidValue("aura1 body length"));
            }
            let rows =
                decode_aura1_body(fixed_body, &plan, record_count, footer.schema.fields.len())?;
            if let Some(sidecar) = event_sidecar {
                decode_explicit_event_sidecar(&footer.schema, &rows, sidecar)?;
            }
            validate_rows(&footer.schema, &rows)?;
            Ok(DecodedI64File {
                header,
                schema: footer.schema.clone(),
                ingest_footer: None,
                compiled_footer: Some(footer),
                rows,
            })
        }
    }
}

pub(crate) fn decode_typed_file_inner(bytes: &[u8]) -> Result<DecodedTypedFile> {
    if bytes.len() < LEGACY_HEADER_PREFIX_SIZE + FOOTER_LEN_SIZE + SEAL_MAGIC.len() {
        return Err(AuraError::UnexpectedEof);
    }
    let seal_offset = bytes.len() - SEAL_MAGIC.len();
    if &bytes[seal_offset..] != SEAL_MAGIC {
        return Err(AuraError::InvalidMagic {
            expected: "sealed:)",
        });
    }
    let footer_len_offset = seal_offset - FOOTER_LEN_SIZE;
    let footer_len = read_trailer_footer_len(bytes, footer_len_offset)?;
    let header_len = AuraHeader::encoded_len(bytes)?;
    if header_len > footer_len_offset {
        return Err(AuraError::UnexpectedEof);
    }
    let header = AuraHeader::decode(&bytes[..header_len])?;
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_start < header_len {
        return Err(AuraError::UnexpectedEof);
    }
    let body = &bytes[header_len..footer_start];
    if body.len() > MAX_V2_I64_DECODE_BODY_BYTES {
        return Err(AuraError::InvalidValue("i64 decode body limit"));
    }
    if matches!(header.profile, Profile::Aura0 | Profile::Aura1) {
        preflight_compiled_footer_record_count(
            &bytes[footer_start..footer_len_offset],
            body.len(),
        )?;
    }
    match header.profile {
        Profile::Ingest => {
            let footer = AuraFooter::decode(&bytes[footer_start..footer_len_offset])?;
            validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
            let rows = if schema_has_wide_fields(&footer.schema) {
                decode_typed_body(&footer.schema, body)?
            } else {
                i64_rows_to_typed(decode_raw_body(body)?)
            };
            validate_typed_rows(&footer.schema, &rows)?;
            Ok(DecodedTypedFile {
                header,
                schema: footer.schema.clone(),
                ingest_footer: Some(footer),
                compiled_footer: None,
                rows,
            })
        }
        Profile::Aura0
            if schema_has_wide_fields_in_compiled_footer(
                &bytes[footer_start..footer_len_offset],
            )? =>
        {
            let footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
            let _ = validate_compiled_i64_decode_limits(&footer, body.len())?;
            validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
            validate_flat_opaque16_schema(&footer.schema)?;
            let (record_count, _) = validate_i64_decode_dimensions(
                footer.record_count,
                footer.schema.fields.len(),
                body.len(),
            )?;
            let semantic_len = aura0_semantic_body_len(&footer, body.len())?;
            if semantic_len == 0 {
                return Err(AuraError::InvalidValue("typed aura0 semantic lane"));
            }
            let plan = footer
                .generic_aura0_plan
                .clone()
                .ok_or(AuraError::InvalidValue("typed aura0 plan"))?;
            let encoded = decode_generic_encoded_rows_body(
                plan,
                &body[..semantic_len],
                record_count,
                footer.schema.fields.len(),
            )?;
            let rows = decode_typed_generic_rows(&footer.schema, encoded)?;
            validate_typed_rows(&footer.schema, &rows)?;
            Ok(DecodedTypedFile {
                header,
                schema: footer.schema.clone(),
                ingest_footer: None,
                compiled_footer: Some(footer),
                rows,
            })
        }
        Profile::Aura1
            if schema_has_wide_fields_in_compiled_footer(
                &bytes[footer_start..footer_len_offset],
            )? =>
        {
            let footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
            let _ = validate_compiled_i64_decode_limits(&footer, body.len())?;
            validate_header_footer_agreement(&header, footer.container_version, &footer.schema)?;
            validate_flat_opaque16_schema(&footer.schema)?;
            let (record_count, _) = validate_i64_decode_dimensions(
                footer.record_count,
                footer.schema.fields.len(),
                body.len(),
            )?;
            let plan = footer.aura1_program.to_aura1_plan(footer.block_capacity)?;
            let rows = decode_typed_aura1_body(&footer.schema, body, &plan, record_count)?;
            validate_typed_rows(&footer.schema, &rows)?;
            Ok(DecodedTypedFile {
                header,
                schema: footer.schema.clone(),
                ingest_footer: None,
                compiled_footer: Some(footer),
                rows,
            })
        }
        Profile::Aura0 | Profile::Aura1 => {
            let decoded = decode_i64_file_inner(bytes)?;
            Ok(DecodedTypedFile {
                header: decoded.header,
                schema: decoded.schema,
                ingest_footer: decoded.ingest_footer,
                compiled_footer: decoded.compiled_footer,
                rows: i64_rows_to_typed(decoded.rows),
            })
        }
    }
}

fn schema_has_wide_fields_in_compiled_footer(bytes: &[u8]) -> Result<bool> {
    Ok(schema_has_wide_fields(
        &CompiledFooter::decode(bytes)?.schema,
    ))
}

fn decode_generic_encoded_rows_body(
    plan: GenericInstructionPlan,
    bytes: &[u8],
    record_count: usize,
    field_count: usize,
) -> Result<GenericEncodedI64Rows> {
    let _ = validate_i64_decode_dimensions_usize(record_count, field_count, bytes.len())?;
    let mut reader = ByteReader::new(bytes);
    let stream_count = usize::from(reader.read_u16_le()?);
    let max_streams = field_count
        .checked_mul(16)
        .ok_or(AuraError::InvalidValue("generic stream count"))?;
    if stream_count > max_streams || stream_count.saturating_mul(14) > reader.remaining() {
        return Err(AuraError::InvalidValue("generic stream count"));
    }
    let mut streams = Vec::new();
    try_reserve_exact(&mut streams, stream_count, "generic stream allocation")?;
    let mut total_values = 0usize;
    for _ in 0..stream_count {
        let stream_id = reader.read_u16_le()?;
        let value_count = usize::try_from(reader.read_u64_le()?)
            .map_err(|_| AuraError::InvalidValue("stream value count"))?;
        total_values = total_values
            .checked_add(value_count)
            .ok_or(AuraError::InvalidValue("generic stream value count"))?;
        if value_count > MAX_V2_I64_DECODE_VALUES || total_values > MAX_V2_I64_DECODE_VALUES {
            return Err(AuraError::InvalidValue("generic stream value limit"));
        }
        let body_len = reader.read_u32_le()? as usize;
        let encoded_body = reader.read_exact(body_len)?;
        let mut body = Vec::new();
        try_reserve_exact(&mut body, body_len, "generic stream body allocation")?;
        body.extend_from_slice(encoded_body);
        streams.push(GenericEncodedStream {
            stream_id,
            value_count,
            body,
        });
    }
    reader.finish()?;
    Ok(GenericEncodedI64Rows {
        plan,
        streams,
        record_count,
        field_count,
    })
}

fn decode_typed_generic_rows(
    schema: &SchemaDescriptor,
    mut encoded: GenericEncodedI64Rows,
) -> Result<Vec<Vec<AuraTypedValue>>> {
    validate_flat_opaque16_schema(schema)?;
    let mut opaque_columns = Vec::new();
    for field in schema
        .fields
        .iter()
        .filter(|field| field.field_type == FieldType::Opaque16)
    {
        let instruction_indexes = encoded
            .plan
            .streams
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| {
                (instruction.target_slot == Some(field.index)).then_some(index)
            })
            .collect::<Vec<_>>();
        if instruction_indexes.len() != 1 {
            return Err(AuraError::InvalidValue("opaque16 stream"));
        }
        let instruction_index = instruction_indexes[0];
        if !matches!(
            encoded.plan.streams[instruction_index].op,
            GenericStreamOp::UuidConstMask { .. }
        ) {
            return Err(AuraError::InvalidValue("opaque16 stream operation"));
        }
        let stream_id = encoded.plan.streams[instruction_index].stream_id;
        let stream_index = encoded
            .streams
            .iter()
            .position(|stream| stream.stream_id == stream_id)
            .ok_or(AuraError::InvalidValue("opaque16 stream"))?;
        if encoded.streams[stream_index].value_count != encoded.record_count {
            return Err(AuraError::InvalidValue("opaque16 stream value count"));
        }
        let values = match decode_typed_uuid_stream(
            &encoded.plan.streams[instruction_index],
            &encoded.streams[stream_index],
        )? {
            GenericStreamBodyValue::U128(values) => values,
            GenericStreamBodyValue::I64(_) => {
                return Err(AuraError::InvalidValue("opaque16 stream body"));
            }
        };
        opaque_columns.push((field.index, values));

        let replacement = crate::instructions::GenericStreamInstruction {
            stream_id,
            target_slot: Some(field.index),
            op: GenericStreamOp::FixedStep { base: 0, step: 0 },
        };
        encoded.streams[stream_index].body.clear();
        encoded.plan.streams[instruction_index] = replacement;
    }

    if encoded
        .plan
        .streams
        .iter()
        .any(|instruction| matches!(instruction.op, GenericStreamOp::UuidConstMask { .. }))
    {
        return Err(AuraError::InvalidValue("opaque16 target slot"));
    }
    let mut rows = i64_rows_to_typed(decode_generic_i64_rows(&encoded)?);
    for (slot, values) in opaque_columns {
        for (row, value) in rows.iter_mut().zip(values) {
            row[usize::from(slot)] = AuraTypedValue::Opaque16(value.to_le_bytes());
        }
    }
    Ok(rows)
}

fn decode_typed_uuid_stream(
    instruction: &crate::instructions::GenericStreamInstruction,
    stream: &GenericEncodedStream,
) -> Result<GenericStreamBodyValue> {
    if matches!(
        instruction.op,
        GenericStreamOp::UuidConstMask {
            constant_bits: 0,
            variable_bits: 128
        }
    ) {
        if stream.value_count > MAX_V2_I64_DECODE_VALUES {
            return Err(AuraError::InvalidValue("uuid stream value limit"));
        }
        let value_bytes = stream
            .value_count
            .checked_mul(std::mem::size_of::<u128>())
            .ok_or(AuraError::InvalidValue("uuid stream body length"))?;
        let expected_body_len = 32usize
            .checked_add(value_bytes)
            .ok_or(AuraError::InvalidValue("uuid stream body length"))?;
        if expected_body_len != stream.body.len()
            || expected_body_len > MAX_V2_I64_DECODE_BODY_BYTES
        {
            return Err(AuraError::InvalidValue("uuid stream body length"));
        }
        let mut reader = ByteReader::new(&stream.body);
        if reader.read_exact(32)?.iter().any(|byte| *byte != 0) {
            return Err(AuraError::InvalidValue("uuid bit mask"));
        }
        let mut values = Vec::new();
        try_reserve_exact(
            &mut values,
            stream.value_count,
            "uuid stream value allocation",
        )?;
        for _ in 0..stream.value_count {
            let bytes = reader.read_exact(16)?;
            values.push(u128::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]));
        }
        reader.finish()?;
        return Ok(GenericStreamBodyValue::U128(values));
    }
    decode_generic_stream_body(instruction, &stream.body, stream.value_count)
}

const FOOTER_LEN_SIZE: usize = 4;
const EXPLICIT_EVENT_SIDECAR_MAGIC: &[u8; 4] = b"AUEV";
const EXPLICIT_EVENT_SIDECAR_TRAILER: usize = 12;

fn append_explicit_event_sidecar(
    mut body: Vec<u8>,
    events: &[I64Event],
    event_field_count: usize,
) -> Result<Vec<u8>> {
    let mut sidecar = Vec::new();
    encode_varint_u64(
        u64::try_from(events.len()).map_err(|_| AuraError::InvalidValue("event count"))?,
        &mut sidecar,
    );
    encode_varint_u64(
        u64::try_from(event_field_count)
            .map_err(|_| AuraError::InvalidValue("event field count"))?,
        &mut sidecar,
    );
    for event in events {
        if event.event_values.len() != event_field_count {
            return Err(AuraError::InvalidValue("event field count"));
        }
        encode_varint_u64(
            u64::try_from(event.children.len())
                .map_err(|_| AuraError::InvalidValue("child count"))?,
            &mut sidecar,
        );
    }
    // Nonempty event headers are already present on their first fixed Aura1
    // child row. Store only headers that otherwise have no physical row.
    for event in events.iter().filter(|event| event.children.is_empty()) {
        for value in &event.event_values {
            encode_varint_i64(*value, &mut sidecar);
        }
    }
    body.extend_from_slice(&sidecar);
    put_u64_le(
        &mut body,
        u64::try_from(sidecar.len())
            .map_err(|_| AuraError::InvalidValue("explicit event sidecar length"))?,
    );
    body.extend_from_slice(EXPLICIT_EVENT_SIDECAR_MAGIC);
    Ok(body)
}

fn decode_explicit_event_sidecar(
    schema: &SchemaDescriptor,
    rows: &[Vec<i64>],
    sidecar: &[u8],
) -> Result<Vec<I64Event>> {
    let mut reader = ByteReader::new(sidecar);
    let event_count = usize::try_from(decode_varint_u64(&mut reader)?)
        .map_err(|_| AuraError::InvalidValue("event count"))?;
    let event_field_count = usize::try_from(decode_varint_u64(&mut reader)?)
        .map_err(|_| AuraError::InvalidValue("event field count"))?;
    let event_slots = schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Event)
        .map(|field| usize::from(field.index))
        .collect::<Vec<_>>();
    let repeated_slots = schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Repeated)
        .map(|field| usize::from(field.index))
        .collect::<Vec<_>>();
    if event_field_count != event_slots.len() {
        return Err(AuraError::InvalidValue("event field count"));
    }
    if event_count > reader.remaining() {
        return Err(AuraError::InvalidValue("event count"));
    }
    let mut counts = Vec::new();
    counts
        .try_reserve(event_count)
        .map_err(|_| AuraError::InvalidValue("event count"))?;
    for _ in 0..event_count {
        counts.push(
            usize::try_from(decode_varint_u64(&mut reader)?)
                .map_err(|_| AuraError::InvalidValue("child count"))?,
        );
    }
    let mut cursor = 0usize;
    let mut events = Vec::new();
    events
        .try_reserve(event_count)
        .map_err(|_| AuraError::InvalidValue("event count"))?;
    for count in counts {
        let end = cursor
            .checked_add(count)
            .ok_or(AuraError::InvalidValue("child count"))?;
        if end > rows.len() {
            return Err(AuraError::InvalidValue("child count"));
        }
        let event_values = if count == 0 {
            (0..event_field_count)
                .map(|_| decode_varint_i64(&mut reader))
                .collect::<Result<Vec<_>>>()?
        } else {
            event_slots
                .iter()
                .map(|slot| {
                    rows[cursor]
                        .get(*slot)
                        .copied()
                        .ok_or(AuraError::InvalidValue("event slot"))
                })
                .collect::<Result<Vec<_>>>()?
        };
        for row in &rows[cursor..end] {
            for (event_index, slot) in event_slots.iter().copied().enumerate() {
                if row.get(slot).copied() != Some(event_values[event_index]) {
                    return Err(AuraError::InvalidValue("event value mismatch"));
                }
            }
        }
        let children = rows[cursor..end]
            .iter()
            .map(|row| {
                repeated_slots
                    .iter()
                    .map(|slot| {
                        row.get(*slot)
                            .copied()
                            .ok_or(AuraError::InvalidValue("repeated slot"))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        events.push(I64Event {
            event_values,
            children,
        });
        cursor = end;
    }
    if cursor != rows.len() {
        return Err(AuraError::InvalidValue("child count"));
    }
    reader.finish()?;
    Ok(events)
}

pub(crate) fn split_explicit_event_sidecar<'a>(
    body: &'a [u8],
    plan: Option<&GenericInstructionPlan>,
) -> Result<(&'a [u8], Option<&'a [u8]>)> {
    if plan.and_then(explicit_event_group).is_none() {
        return Ok((body, None));
    }
    if body.len() < EXPLICIT_EVENT_SIDECAR_TRAILER
        || &body[body.len() - EXPLICIT_EVENT_SIDECAR_MAGIC.len()..] != EXPLICIT_EVENT_SIDECAR_MAGIC
    {
        return Ok((body, None));
    }
    let length_offset = body.len() - EXPLICIT_EVENT_SIDECAR_TRAILER;
    let length_bytes = &body[length_offset..length_offset + 8];
    let sidecar_len = usize::try_from(u64::from_le_bytes([
        length_bytes[0],
        length_bytes[1],
        length_bytes[2],
        length_bytes[3],
        length_bytes[4],
        length_bytes[5],
        length_bytes[6],
        length_bytes[7],
    ]))
    .map_err(|_| AuraError::InvalidValue("explicit event sidecar length"))?;
    let sidecar_start = length_offset
        .checked_sub(sidecar_len)
        .ok_or(AuraError::UnexpectedEof)?;
    Ok((
        &body[..sidecar_start],
        Some(&body[sidecar_start..length_offset]),
    ))
}

pub(crate) fn compiled_footer_has_explicit_events(footer: &CompiledFooter) -> bool {
    footer
        .generic_aura0_plan
        .as_ref()
        .and_then(explicit_event_group)
        .is_some()
}
const MAX_VISITOR_FIELDS: usize = 64;

struct CompiledFileParts<'a> {
    header: AuraHeader,
    body: &'a [u8],
    footer: CompiledFooter,
}

struct SealedFileOffsets {
    header: AuraHeader,
    header_len: usize,
    footer_start: usize,
    footer_len_offset: usize,
}

fn sealed_profile(bytes: &[u8]) -> Result<Profile> {
    if bytes.len() < LEGACY_HEADER_PREFIX_SIZE + FOOTER_LEN_SIZE + SEAL_MAGIC.len() {
        return Err(AuraError::UnexpectedEof);
    }
    let seal_offset = bytes.len() - SEAL_MAGIC.len();
    if &bytes[seal_offset..] != SEAL_MAGIC {
        return Err(AuraError::InvalidMagic {
            expected: "sealed:)",
        });
    }
    let header_len = AuraHeader::encoded_len(bytes)?;
    Ok(AuraHeader::decode(&bytes[..header_len])?.profile)
}

fn parse_compiled_file_parts(
    bytes: &[u8],
    expected_profile: Profile,
) -> Result<CompiledFileParts<'_>> {
    let offsets = parse_sealed_file_offsets(bytes, expected_profile)?;
    let footer = CompiledFooter::decode(&bytes[offsets.footer_start..offsets.footer_len_offset])?;
    let _ =
        validate_compiled_i64_decode_limits(&footer, offsets.footer_start - offsets.header_len)?;
    validate_header_footer_agreement(&offsets.header, footer.container_version, &footer.schema)?;
    Ok(CompiledFileParts {
        header: offsets.header,
        body: &bytes[offsets.header_len..offsets.footer_start],
        footer,
    })
}

fn parse_sealed_file_offsets(bytes: &[u8], expected_profile: Profile) -> Result<SealedFileOffsets> {
    if bytes.len() < LEGACY_HEADER_PREFIX_SIZE + FOOTER_LEN_SIZE + SEAL_MAGIC.len() {
        return Err(AuraError::UnexpectedEof);
    }
    let seal_offset = bytes.len() - SEAL_MAGIC.len();
    if &bytes[seal_offset..] != SEAL_MAGIC {
        return Err(AuraError::InvalidMagic {
            expected: "sealed:)",
        });
    }
    let footer_len_offset = seal_offset - FOOTER_LEN_SIZE;
    let footer_len = read_trailer_footer_len(bytes, footer_len_offset)?;
    let header_len = AuraHeader::encoded_len(bytes)?;
    if header_len > footer_len_offset {
        return Err(AuraError::UnexpectedEof);
    }
    let header = AuraHeader::decode(&bytes[..header_len])?;
    if header.profile != expected_profile {
        return Err(AuraError::InvalidValue("profile"));
    }
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_start < header_len {
        return Err(AuraError::UnexpectedEof);
    }
    Ok(SealedFileOffsets {
        header,
        header_len,
        footer_start,
        footer_len_offset,
    })
}

fn encode_aura1_byte_lane_file(
    codec: Aura0ByteLaneCodec,
    aura1_bytes: &[u8],
    compressed_offset: u64,
    record_count: u64,
) -> Result<(Vec<u8>, Aura1ByteLaneDescriptor)> {
    let source_len = validate_aura1_byte_lane_source_len(aura1_bytes.len())?;
    let bytes = match codec {
        Aura0ByteLaneCodec::Raw => aura1_bytes.to_vec(),
        Aura0ByteLaneCodec::Lz4 => lz4_flex::compress_prepend_size(aura1_bytes),
        Aura0ByteLaneCodec::Zstd1 | Aura0ByteLaneCodec::Zstd3 | Aura0ByteLaneCodec::Zstd9 => {
            zstd::stream::encode_all(
                std::io::Cursor::new(aura1_bytes),
                i32::from(codec.codec_level()),
            )
            .map_err(|_| AuraError::InvalidValue("byte lane zstd"))?
        }
    };
    let compressed_len = u64::try_from(bytes.len())
        .map_err(|_| AuraError::InvalidValue("byte lane compressed length"))?;
    if compressed_len > MAX_AURA1_BYTE_LANE_COMPRESSED_BYTES {
        return Err(AuraError::InvalidValue("byte lane compressed length"));
    }
    let _compressed_end = compressed_offset
        .checked_add(compressed_len)
        .ok_or(AuraError::InvalidValue("byte lane compressed range"))?;
    let checksum = bytes_guard_value(aura1_bytes);
    let row_count =
        u32::try_from(record_count).map_err(|_| AuraError::InvalidValue("record count"))?;
    let descriptor = Aura1ByteLaneDescriptor {
        lane_version: AURA1_BYTE_LANE_VERSION,
        codec_id: codec.codec_id(),
        codec_level: codec.codec_level(),
        block_index: 0,
        row_start: 0,
        row_count,
        aura1_output_offset: 0,
        uncompressed_len: source_len,
        compressed_offset,
        compressed_len,
        checksum_kind: BYTE_LANE_CHECKSUM_BYTE_GUARD,
        checksum,
        flags: 0,
    };
    let _output_len = validate_aura1_byte_lane_limits(std::slice::from_ref(&descriptor))?;
    Ok((bytes, descriptor))
}

fn validate_aura1_byte_lane_source_len(source_len: usize) -> Result<u64> {
    let source_len = u64::try_from(source_len)
        .map_err(|_| AuraError::InvalidValue("byte lane output length"))?;
    if source_len > MAX_AURA1_BYTE_LANE_OUTPUT_BYTES
        || source_len > MAX_AURA1_BYTE_LANES_TOTAL_OUTPUT_BYTES
    {
        return Err(AuraError::InvalidValue("byte lane output length"));
    }
    Ok(source_len)
}

fn decode_aura1_byte_lanes_from_body(
    body: &[u8],
    footer: &CompiledFooter,
    validate_checksum: bool,
) -> Result<Vec<u8>> {
    let decoded = decode_aura1_byte_lanes_from_descriptors(
        body,
        &footer.aura1_byte_lanes,
        validate_checksum,
    )?;
    validate_embedded_aura1_container_version(&decoded, footer.container_version)?;
    Ok(decoded)
}

fn decode_aura1_byte_lanes_from_descriptors(
    body: &[u8],
    lanes: &[Aura1ByteLaneDescriptor],
    validate_checksum: bool,
) -> Result<Vec<u8>> {
    if lanes.is_empty() {
        return Err(AuraError::InvalidValue("aura0 byte lane"));
    }
    let bounded_output_len = validate_aura1_byte_lane_limits(lanes)?;

    if lanes.len() == 1 {
        let lane = &lanes[0];
        validate_aura1_byte_lane_descriptor(lane)?;
        if lane.aura1_output_offset == 0 {
            let (compressed_start, compressed_end) = byte_lane_body_range(lane, body.len())?;
            let compressed = body
                .get(compressed_start..compressed_end)
                .ok_or(AuraError::UnexpectedEof)?;
            let decoded = decode_aura1_byte_lane_payload(lane, compressed)?;
            let expected_len = usize::try_from(lane.uncompressed_len)
                .map_err(|_| AuraError::InvalidValue("byte lane length"))?;
            if decoded.len() != expected_len {
                return Err(AuraError::InvalidValue("byte lane length"));
            }
            if validate_checksum {
                validate_aura1_byte_lane_checksum(lane, &decoded)?;
            }
            return Ok(decoded);
        }
    }

    for lane in lanes {
        validate_aura1_byte_lane_descriptor(lane)?;
        let _compressed_range = byte_lane_body_range(lane, body.len())?;
    }
    let mut output = allocate_zeroed_byte_lane_output(bounded_output_len)?;

    for lane in lanes {
        let (compressed_start, compressed_end) = byte_lane_body_range(lane, body.len())?;
        let compressed = body
            .get(compressed_start..compressed_end)
            .ok_or(AuraError::UnexpectedEof)?;
        let expected_len = usize::try_from(lane.uncompressed_len)
            .map_err(|_| AuraError::InvalidValue("byte lane length"))?;
        let output_start = usize::try_from(lane.aura1_output_offset)
            .map_err(|_| AuraError::InvalidValue("byte lane offset"))?;
        let output_end = output_start
            .checked_add(expected_len)
            .ok_or(AuraError::UnexpectedEof)?;
        let decoded = output
            .get_mut(output_start..output_end)
            .ok_or(AuraError::UnexpectedEof)?;
        decode_aura1_byte_lane_payload_into(lane, compressed, decoded)?;
        if validate_checksum {
            validate_aura1_byte_lane_checksum(lane, decoded)?;
        }
    }
    Ok(output)
}

fn decode_aura1_byte_lane_payload(
    lane: &Aura1ByteLaneDescriptor,
    compressed: &[u8],
) -> Result<Vec<u8>> {
    let expected_len = usize::try_from(lane.uncompressed_len)
        .map_err(|_| AuraError::InvalidValue("byte lane length"))?;
    let mut decoded = allocate_zeroed_byte_lane_output(expected_len)?;
    decode_aura1_byte_lane_payload_into(lane, compressed, &mut decoded)?;
    Ok(decoded)
}

fn allocate_zeroed_byte_lane_output(len: usize) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(len)
        .map_err(|_| AuraError::InvalidValue("byte lane output allocation"))?;
    output.resize(len, 0);
    Ok(output)
}

fn byte_lane_body_range(lane: &Aura1ByteLaneDescriptor, body_len: usize) -> Result<(usize, usize)> {
    let end = lane
        .compressed_offset
        .checked_add(lane.compressed_len)
        .ok_or(AuraError::InvalidValue("byte lane compressed range"))?;
    let start = usize::try_from(lane.compressed_offset)
        .map_err(|_| AuraError::InvalidValue("byte lane offset"))?;
    let end = usize::try_from(end).map_err(|_| AuraError::InvalidValue("byte lane length"))?;
    if end > body_len {
        return Err(AuraError::UnexpectedEof);
    }
    Ok((start, end))
}

fn decode_aura1_byte_lane_payload_into(
    lane: &Aura1ByteLaneDescriptor,
    compressed: &[u8],
    decoded: &mut [u8],
) -> Result<()> {
    match (lane.codec_id, lane.codec_level) {
        (BYTE_LANE_CODEC_RAW, 0) => {
            if compressed.len() != decoded.len() {
                return Err(AuraError::InvalidValue("byte lane length"));
            }
            decoded.copy_from_slice(compressed);
        }
        (BYTE_LANE_CODEC_LZ4, 0) => {
            let size = compressed
                .get(..4)
                .ok_or(AuraError::InvalidValue("byte lane lz4"))?;
            let stamped_len =
                usize::try_from(u32::from_le_bytes([size[0], size[1], size[2], size[3]]))
                    .map_err(|_| AuraError::InvalidValue("byte lane length"))?;
            if stamped_len != decoded.len() {
                return Err(AuraError::InvalidValue("byte lane length"));
            }
            let written = lz4_flex::block::decompress_into(&compressed[4..], decoded)
                .map_err(|_| AuraError::InvalidValue("byte lane lz4"))?;
            if written != decoded.len() {
                return Err(AuraError::InvalidValue("byte lane length"));
            }
        }
        (BYTE_LANE_CODEC_ZSTD, 1 | 3 | 9) => {
            let written = zstd::bulk::decompress_to_buffer(compressed, decoded)
                .map_err(|_| AuraError::InvalidValue("byte lane zstd"))?;
            if written != decoded.len() {
                return Err(AuraError::InvalidValue("byte lane length"));
            }
        }
        _ => return Err(AuraError::InvalidValue("byte lane codec")),
    }
    Ok(())
}

fn validate_compiled_footer_version_prefix(
    footer_bytes: &[u8],
    expected_version: AuraContainerVersion,
) -> Result<AuraContainerVersion> {
    let mut reader = ByteReader::new(footer_bytes);
    if reader.read_exact(COMPILED_FOOTER_MAGIC.len())? != COMPILED_FOOTER_MAGIC {
        return Err(AuraError::InvalidMagic { expected: "AURP" });
    }
    let footer_version = AuraContainerVersion::from_wire(reader.read_u16_le()?)?;
    footer_version.require_supported_container_layout()?;
    validate_container_version_agreement(expected_version, footer_version)?;
    Ok(footer_version)
}

fn validate_embedded_aura1_container_version(
    bytes: &[u8],
    expected_version: AuraContainerVersion,
) -> Result<()> {
    let offsets = parse_sealed_file_offsets(bytes, Profile::Aura1)?;
    validate_container_version_agreement(expected_version, offsets.header.container_version)?;
    validate_compiled_footer_version_prefix(
        &bytes[offsets.footer_start..offsets.footer_len_offset],
        expected_version,
    )?;
    Ok(())
}

fn try_decode_aura1_byte_lane_from_footer_tail(
    body: &[u8],
    footer_bytes: &[u8],
    outer_header_version: AuraContainerVersion,
    validate_checksum: bool,
) -> Result<Option<Vec<u8>>> {
    let footer = CompiledFooter::decode(footer_bytes)?;
    let _ = validate_compiled_i64_decode_limits(&footer, body.len())?;
    validate_container_version_agreement(outer_header_version, footer.container_version)?;
    if footer.aura1_byte_lanes.is_empty() {
        return Ok(None);
    }
    let decoded = decode_aura1_byte_lanes_from_descriptors(
        body,
        &footer.aura1_byte_lanes,
        validate_checksum,
    )?;
    validate_embedded_aura1_container_version(&decoded, outer_header_version)?;
    Ok(Some(decoded))
}

fn validate_aura1_byte_lane_descriptor(lane: &Aura1ByteLaneDescriptor) -> Result<()> {
    let _output_len = validate_aura1_byte_lane_limits(std::slice::from_ref(lane))?;
    if lane.lane_version != AURA1_BYTE_LANE_VERSION {
        return Err(AuraError::UnsupportedVersion(u16::from(lane.lane_version)));
    }
    match (lane.codec_id, lane.codec_level) {
        (BYTE_LANE_CODEC_RAW, 0) | (BYTE_LANE_CODEC_LZ4, 0) | (BYTE_LANE_CODEC_ZSTD, 1 | 3 | 9) => {
        }
        _ => return Err(AuraError::InvalidValue("byte lane codec")),
    }
    match lane.checksum_kind {
        BYTE_LANE_CHECKSUM_NONE | BYTE_LANE_CHECKSUM_BYTE_GUARD => {}
        _ => return Err(AuraError::InvalidValue("byte lane checksum")),
    }
    Ok(())
}

fn validate_aura1_byte_lane_checksum(lane: &Aura1ByteLaneDescriptor, bytes: &[u8]) -> Result<()> {
    match lane.checksum_kind {
        BYTE_LANE_CHECKSUM_NONE => Ok(()),
        BYTE_LANE_CHECKSUM_BYTE_GUARD => {
            if bytes_guard_value(bytes) == lane.checksum {
                Ok(())
            } else {
                Err(AuraError::InvalidValue("byte lane checksum"))
            }
        }
        _ => Err(AuraError::InvalidValue("byte lane checksum")),
    }
}

fn bytes_guard_value(bytes: &[u8]) -> u64 {
    let mut guard = ByteGuard::new();
    guard.update(bytes);
    guard.value()
}

fn aura0_semantic_body_len(footer: &CompiledFooter, body_len: usize) -> Result<usize> {
    let mut semantic_len = body_len;
    for lane in &footer.aura1_byte_lanes {
        let offset = usize::try_from(lane.compressed_offset)
            .map_err(|_| AuraError::InvalidValue("byte lane offset"))?;
        semantic_len = semantic_len.min(offset);
    }
    Ok(semantic_len)
}

fn read_trailer_footer_len(bytes: &[u8], offset: usize) -> Result<usize> {
    let end = offset
        .checked_add(FOOTER_LEN_SIZE)
        .ok_or(AuraError::UnexpectedEof)?;
    let footer_len_bytes = bytes.get(offset..end).ok_or(AuraError::UnexpectedEof)?;
    Ok(u32::from_le_bytes([
        footer_len_bytes[0],
        footer_len_bytes[1],
        footer_len_bytes[2],
        footer_len_bytes[3],
    ]) as usize)
}

pub(crate) fn validate_header_schema_agreement(
    header: &AuraHeader,
    schema: &SchemaDescriptor,
) -> Result<()> {
    validate_schema_container_compatibility(schema, header.container_version)?;
    match header.container_version {
        AuraContainerVersion::V2 => {
            if !header.groups.is_empty() || !schema.groups.is_empty() {
                return Err(AuraError::InvalidValue("header schema groups"));
            }
        }
        AuraContainerVersion::V3 => {
            if header.groups != schema.groups {
                return Err(AuraError::InvalidValue("header schema groups"));
            }
        }
        AuraContainerVersion::LegacyV1 => {
            return Err(AuraError::InvalidValue("schema container version"));
        }
    }
    let expected_mapping = schema_parent_mapping(schema)?;
    if header.schema_mapping != expected_mapping {
        return Err(AuraError::InvalidValue("header schema mapping"));
    }
    if header.derived_expressions != schema.derived_expressions {
        return Err(AuraError::InvalidValue("header derived expressions"));
    }
    Ok(())
}

pub(crate) fn validate_header_footer_agreement(
    header: &AuraHeader,
    footer_version: AuraContainerVersion,
    schema: &SchemaDescriptor,
) -> Result<()> {
    validate_container_version_agreement(header.container_version, footer_version)?;
    validate_header_schema_agreement(header, schema)
}

fn validate_container_version_agreement(
    expected: AuraContainerVersion,
    observed: AuraContainerVersion,
) -> Result<()> {
    if expected != observed {
        return Err(AuraError::InvalidValue("header footer container version"));
    }
    Ok(())
}

fn conversion_output_container_version(
    source_version: AuraContainerVersion,
) -> Result<AuraContainerVersion> {
    // TODO(aura-v3): before V3 decode is enabled, choose and test an explicit
    // Preserve-vs-Upgrade conversion/restamp policy here. Keeping this match
    // exhaustive prevents a newly supported version from defaulting to V2.
    match source_version {
        AuraContainerVersion::V2 => Ok(AuraContainerVersion::V2),
        AuraContainerVersion::LegacyV1 | AuraContainerVersion::V3 => {
            Err(AuraError::UnsupportedVersion(source_version.wire_value()))
        }
    }
}

pub(crate) fn validate_compiled_i64_metadata(
    header: &AuraHeader,
    footer: &CompiledFooter,
    body_len: usize,
) -> Result<usize> {
    validate_header_footer_agreement(header, footer.container_version, &footer.schema)?;
    if schema_has_wide_fields(&footer.schema) {
        return Err(AuraError::InvalidValue("i64 schema"));
    }
    validate_compiled_i64_decode_limits(footer, body_len)
}

impl DecodedI64File {
    pub(crate) fn aura0_plan(&self) -> Result<Aura0Plan> {
        if let Some(footer) = &self.ingest_footer {
            return footer
                .aura0_plan
                .clone()
                .ok_or(AuraError::InvalidValue("aura0 plan"));
        }
        self.compiled_footer
            .as_ref()
            .ok_or(AuraError::InvalidValue("compiled footer"))?
            .aura0_program
            .to_aura0_plan()
    }

    pub(crate) fn aura1_plan(&self) -> Result<Aura1Plan> {
        if let Some(footer) = &self.ingest_footer {
            return footer
                .aura1_plan
                .clone()
                .ok_or(AuraError::InvalidValue("aura1 plan"));
        }
        let footer = self
            .compiled_footer
            .as_ref()
            .ok_or(AuraError::InvalidValue("compiled footer"))?;
        footer.aura1_program.to_aura1_plan(footer.block_capacity)
    }

    pub(crate) fn generic_aura0_plan(&self) -> Option<GenericInstructionPlan> {
        if let Some(footer) = &self.ingest_footer {
            return footer.generic_aura0_plan.clone();
        }
        self.compiled_footer
            .as_ref()
            .and_then(|footer| footer.generic_aura0_plan.clone())
    }

    pub(crate) fn compiled_footer_for_compile(&self) -> Result<CompiledFooter> {
        if let Some(footer) = &self.compiled_footer {
            return Ok(footer.clone());
        }
        let aura0_plan = self.aura0_plan()?;
        let aura1_plan = self.aura1_plan()?;
        let block_capacity = aura1_plan.block_capacity;
        let field_count = self.schema.fields.len();
        let container_version = conversion_output_container_version(self.header.container_version)?;
        let mut footer = CompiledFooter::new(
            self.schema.clone(),
            self.rows.len() as u64,
            block_capacity,
            DecodeProgram::from_aura0_plan(&aura0_plan, field_count)?,
            DecodeProgram::from_aura1_plan(&aura1_plan, field_count)?,
        )?
        .with_container_version(container_version);
        if let Some(plan) = self.generic_aura0_plan() {
            footer = footer.with_generic_aura0_plan(plan);
        }
        Ok(footer)
    }
}

fn compiled_footer_from_ingest_footer(
    footer: &AuraFooter,
    record_count: usize,
) -> Result<CompiledFooter> {
    let record_count =
        u64::try_from(record_count).map_err(|_| AuraError::InvalidValue("record count"))?;
    if record_count != footer.stats.record_count {
        return Err(AuraError::InvalidValue("record count"));
    }
    let aura0_plan = footer
        .aura0_plan
        .clone()
        .ok_or(AuraError::InvalidValue("aura0 plan"))?;
    let aura1_plan = footer
        .aura1_plan
        .clone()
        .ok_or(AuraError::InvalidValue("aura1 plan"))?;
    let block_capacity = aura1_plan.block_capacity;
    let field_count = footer.schema.fields.len();
    let container_version = conversion_output_container_version(footer.container_version)?;
    let mut compiled = CompiledFooter::new(
        footer.schema.clone(),
        record_count,
        block_capacity,
        DecodeProgram::from_aura0_plan(&aura0_plan, field_count)?,
        DecodeProgram::from_aura1_plan(&aura1_plan, field_count)?,
    )?
    .with_container_version(container_version);
    if let Some(plan) = footer.generic_aura0_plan.clone() {
        compiled = compiled.with_generic_aura0_plan(plan);
    }
    Ok(compiled)
}

fn encode_file(
    profile: Profile,
    stream_id: u16,
    dictionary_id: u16,
    base_time_ns: i64,
    header_comment: &str,
    body: Vec<u8>,
    footer: AuraFooter,
) -> Result<Vec<u8>> {
    let footer_bytes = footer.encode()?;
    let footer_len =
        u32::try_from(footer_bytes.len()).map_err(|_| AuraError::InvalidValue("footer length"))?;
    let container_version = conversion_output_container_version(footer.container_version)?;
    let header = AuraHeader::new(profile)
        .with_container_version(container_version)
        .with_stream(stream_id, dictionary_id, base_time_ns)
        .with_schema_mapping(schema_parent_mapping(&footer.schema)?)?
        .with_derived_expressions(footer.schema.derived_expressions.clone())?
        .with_comment(header_comment)?;
    let header_bytes = header.encode()?;

    let mut out = Vec::with_capacity(
        header_bytes.len() + body.len() + footer_bytes.len() + FOOTER_LEN_SIZE + SEAL_MAGIC.len(),
    );
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&body);
    out.extend_from_slice(&footer_bytes);
    put_u32_le(&mut out, footer_len);
    out.extend_from_slice(SEAL_MAGIC);
    Ok(out)
}

fn encode_compiled_file(
    profile: Profile,
    stream_id: u16,
    dictionary_id: u16,
    base_time_ns: i64,
    header_comment: &str,
    body: Vec<u8>,
    footer: CompiledFooter,
) -> Result<Vec<u8>> {
    let footer_bytes = footer.encode()?;
    let footer_len =
        u32::try_from(footer_bytes.len()).map_err(|_| AuraError::InvalidValue("footer length"))?;
    let container_version = conversion_output_container_version(footer.container_version)?;
    let header = AuraHeader::new(profile)
        .with_container_version(container_version)
        .with_stream(stream_id, dictionary_id, base_time_ns)
        .with_schema_mapping(schema_parent_mapping(&footer.schema)?)?
        .with_derived_expressions(footer.schema.derived_expressions.clone())?
        .with_comment(header_comment)?;
    let header_bytes = header.encode()?;

    let mut out = Vec::with_capacity(
        header_bytes.len() + body.len() + footer_bytes.len() + FOOTER_LEN_SIZE + SEAL_MAGIC.len(),
    );
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&body);
    out.extend_from_slice(&footer_bytes);
    put_u32_le(&mut out, footer_len);
    out.extend_from_slice(SEAL_MAGIC);
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn try_encode_compiled_file_with_body_writer<F>(
    profile: Profile,
    stream_id: u16,
    dictionary_id: u16,
    base_time_ns: i64,
    header_comment: &str,
    body_capacity: usize,
    footer: CompiledFooter,
    write_body: F,
) -> Result<Option<(Vec<u8>, usize)>>
where
    F: FnOnce(&mut Vec<u8>) -> Result<bool>,
{
    try_encode_compiled_file_with_body_writer_inner(
        profile,
        stream_id,
        dictionary_id,
        base_time_ns,
        header_comment,
        body_capacity,
        footer,
        None,
        |out, _guard| write_body(out),
    )
}

#[allow(clippy::too_many_arguments)]
fn try_encode_compiled_file_with_body_writer_guarded<F>(
    profile: Profile,
    stream_id: u16,
    dictionary_id: u16,
    base_time_ns: i64,
    header_comment: &str,
    body_capacity: usize,
    footer: CompiledFooter,
    output_guard: &mut ByteGuard,
    write_body: F,
) -> Result<Option<(Vec<u8>, usize)>>
where
    F: FnOnce(&mut Vec<u8>, &mut ByteGuard) -> Result<bool>,
{
    try_encode_compiled_file_with_body_writer_inner(
        profile,
        stream_id,
        dictionary_id,
        base_time_ns,
        header_comment,
        body_capacity,
        footer,
        Some(output_guard),
        |out, guard| {
            let guard = guard.ok_or(AuraError::InvalidValue("output guard"))?;
            write_body(out, guard)
        },
    )
}

#[allow(clippy::needless_option_as_deref, clippy::too_many_arguments)]
fn try_encode_compiled_file_with_body_writer_inner<F>(
    profile: Profile,
    stream_id: u16,
    dictionary_id: u16,
    base_time_ns: i64,
    header_comment: &str,
    body_capacity: usize,
    footer: CompiledFooter,
    mut output_guard: Option<&mut ByteGuard>,
    write_body: F,
) -> Result<Option<(Vec<u8>, usize)>>
where
    F: FnOnce(&mut Vec<u8>, Option<&mut ByteGuard>) -> Result<bool>,
{
    let footer_bytes = footer.encode()?;
    let footer_len =
        u32::try_from(footer_bytes.len()).map_err(|_| AuraError::InvalidValue("footer length"))?;
    let container_version = conversion_output_container_version(footer.container_version)?;
    let header = AuraHeader::new(profile)
        .with_container_version(container_version)
        .with_stream(stream_id, dictionary_id, base_time_ns)
        .with_schema_mapping(schema_parent_mapping(&footer.schema)?)?
        .with_derived_expressions(footer.schema.derived_expressions.clone())?
        .with_comment(header_comment)?;
    let header_bytes = header.encode()?;
    let mut out = Vec::with_capacity(
        header_bytes.len()
            + body_capacity
            + footer_bytes.len()
            + FOOTER_LEN_SIZE
            + SEAL_MAGIC.len(),
    );
    let header_start = out.len();
    out.extend_from_slice(&header_bytes);
    if let Some(output_guard) = output_guard.as_deref_mut() {
        output_guard.update(&out[header_start..]);
    }
    let body_start = out.len();
    if !write_body(&mut out, output_guard.as_deref_mut())? {
        return Ok(None);
    }
    let body_len = out.len() - body_start;
    let footer_start = out.len();
    out.extend_from_slice(&footer_bytes);
    if let Some(output_guard) = output_guard.as_deref_mut() {
        output_guard.update(&out[footer_start..]);
    }
    let footer_len_start = out.len();
    put_u32_le(&mut out, footer_len);
    if let Some(output_guard) = output_guard.as_deref_mut() {
        output_guard.update(&out[footer_len_start..]);
    }
    let seal_start = out.len();
    out.extend_from_slice(SEAL_MAGIC);
    if let Some(output_guard) = output_guard.as_deref_mut() {
        output_guard.update(&out[seal_start..]);
    }
    Ok(Some((out, body_len)))
}

fn encode_raw_body(field_count: usize, rows: &[Vec<i64>]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    put_u64_le(&mut out, rows.len() as u64);
    put_u16_len(&mut out, field_count, "field count")?;
    for row in rows {
        for value in row {
            put_i64_le(&mut out, *value);
        }
    }
    Ok(out)
}

fn encode_typed_body(schema: &SchemaDescriptor, rows: &[Vec<AuraTypedValue>]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    put_u64_le(&mut out, rows.len() as u64);
    put_u16_len(&mut out, schema.fields.len(), "field count")?;
    for row in rows {
        if row.len() != schema.fields.len() {
            return Err(AuraError::InvalidValue("record field count"));
        }
        for field in &schema.fields {
            let value = &row[usize::from(field.index)];
            match field.field_type {
                FieldType::I128 => {
                    let value = match value {
                        AuraTypedValue::I128(value) => *value,
                        AuraTypedValue::I64(value) => i128::from(*value),
                        AuraTypedValue::Opaque16(_) => {
                            return Err(AuraError::InvalidValue("typed value"));
                        }
                    };
                    out.extend_from_slice(&value.to_le_bytes());
                }
                FieldType::Opaque16 => {
                    let AuraTypedValue::Opaque16(value) = value else {
                        return Err(AuraError::InvalidValue("typed value"));
                    };
                    out.extend_from_slice(value);
                }
                _ => put_i64_le(
                    &mut out,
                    typed_value_i64_for_field(field.field_type, value)?,
                ),
            }
        }
    }
    Ok(out)
}

fn encode_typed_aura1_body(
    schema: &SchemaDescriptor,
    rows: &[Vec<AuraTypedValue>],
    plan: &[PhysicalFieldPlan],
) -> Result<Vec<u8>> {
    if plan.len() != schema.fields.len() {
        return Err(AuraError::InvalidValue("program field count"));
    }
    let record_width = plan.iter().try_fold(0usize, |width, field| {
        width
            .checked_add(usize::from(field.width.byte_width()))
            .ok_or(AuraError::InvalidValue("body length"))
    })?;
    let mut out = Vec::with_capacity(
        rows.len()
            .checked_mul(record_width)
            .ok_or(AuraError::InvalidValue("body length"))?,
    );
    for row in rows {
        for field_plan in plan {
            if field_plan.encoding != FieldEncoding::Absolute {
                return Err(AuraError::InvalidValue("typed aura1 field encoding"));
            }
            let field = schema
                .fields
                .get(usize::from(field_plan.field_index))
                .ok_or(AuraError::InvalidValue("field index"))?;
            let value = row
                .get(usize::from(field.index))
                .ok_or(AuraError::InvalidValue("record field count"))?;
            match (field.field_type, value) {
                (FieldType::Opaque16, AuraTypedValue::Opaque16(value)) => {
                    if field_plan.width != PhysicalWidth::I128 {
                        return Err(AuraError::InvalidValue("opaque16 physical width"));
                    }
                    out.extend_from_slice(value);
                }
                (FieldType::Opaque16, _) => return Err(AuraError::InvalidValue("typed value")),
                (FieldType::I128, _) => return Err(AuraError::InvalidValue("typed i128 aura1")),
                (_, value) => write_i64_width(
                    &mut out,
                    typed_value_i64_for_field(field.field_type, value)?,
                    field_plan.width,
                )?,
            }
        }
    }
    Ok(out)
}

fn decode_raw_body(bytes: &[u8]) -> Result<Vec<Vec<i64>>> {
    let (record_count, field_count) = preflight_raw_i64_body(bytes)?;
    let mut reader = ByteReader::new(bytes);
    let _declared_record_count = reader.read_u64_le()?;
    let _declared_field_count = reader.read_u16_le()?;
    let mut rows = Vec::new();
    try_reserve_exact(&mut rows, record_count, "i64 decode row allocation")?;
    for _ in 0..record_count {
        let mut row = Vec::new();
        try_reserve_exact(&mut row, field_count, "i64 decode value allocation")?;
        for _ in 0..field_count {
            row.push(reader.read_i64_le()?);
        }
        rows.push(row);
    }
    reader.finish()?;
    Ok(rows)
}

fn preflight_raw_i64_body(bytes: &[u8]) -> Result<(usize, usize)> {
    let mut reader = ByteReader::new(bytes);
    let declared_record_count = reader.read_u64_le()?;
    let field_count = reader.read_u16_le()? as usize;
    let (record_count, value_count) =
        validate_i64_decode_dimensions(declared_record_count, field_count, bytes.len())?;
    let expected_bytes = value_count
        .checked_mul(std::mem::size_of::<i64>())
        .ok_or(AuraError::InvalidValue("raw body length"))?;
    if reader.remaining() != expected_bytes {
        return Err(AuraError::InvalidValue("raw body length"));
    }
    Ok((record_count, field_count))
}

fn decode_typed_body(schema: &SchemaDescriptor, bytes: &[u8]) -> Result<Vec<Vec<AuraTypedValue>>> {
    let mut reader = ByteReader::new(bytes);
    let declared_record_count = reader.read_u64_le()?;
    let field_count = reader.read_u16_le()? as usize;
    if field_count != schema.fields.len() {
        return Err(AuraError::InvalidValue("field count"));
    }
    let (record_count, _) =
        validate_i64_decode_dimensions(declared_record_count, field_count, bytes.len())?;
    let row_width = schema.fields.iter().try_fold(0usize, |width, field| {
        width
            .checked_add(match field.field_type {
                FieldType::I128 | FieldType::Opaque16 => 16,
                _ => 8,
            })
            .ok_or(AuraError::InvalidValue("typed body length"))
    })?;
    let expected_bytes = record_count
        .checked_mul(row_width)
        .ok_or(AuraError::InvalidValue("typed body length"))?;
    if reader.remaining() != expected_bytes {
        return Err(AuraError::InvalidValue("typed body length"));
    }
    let mut rows = Vec::new();
    try_reserve_exact(&mut rows, record_count, "typed decode row allocation")?;
    for _ in 0..record_count {
        let mut row = Vec::new();
        try_reserve_exact(&mut row, field_count, "typed decode value allocation")?;
        for field in &schema.fields {
            row.push(match field.field_type {
                FieldType::I128 => {
                    let bytes = reader.read_exact(16)?;
                    AuraTypedValue::I128(i128::from_le_bytes([
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                        bytes[7], bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13],
                        bytes[14], bytes[15],
                    ]))
                }
                FieldType::Opaque16 => {
                    let bytes = reader.read_exact(16)?;
                    AuraTypedValue::Opaque16([
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                        bytes[7], bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13],
                        bytes[14], bytes[15],
                    ])
                }
                _ => AuraTypedValue::I64(reader.read_i64_le()?),
            });
        }
        rows.push(row);
    }
    reader.finish()?;
    Ok(rows)
}

fn decode_typed_aura1_body(
    schema: &SchemaDescriptor,
    bytes: &[u8],
    plan: &Aura1Plan,
    record_count: usize,
) -> Result<Vec<Vec<AuraTypedValue>>> {
    let _ = validate_i64_decode_dimensions_usize(record_count, schema.fields.len(), bytes.len())?;
    if plan.fields.len() != schema.fields.len() {
        return Err(AuraError::InvalidValue("program field count"));
    }
    validate_aura1_plan_fields(plan, schema.fields.len())?;
    let row_width = plan.fields.iter().try_fold(0usize, |width, field| {
        width
            .checked_add(usize::from(field.width.byte_width()))
            .ok_or(AuraError::InvalidValue("typed aura1 body length"))
    })?;
    let expected_body_len = record_count
        .checked_mul(row_width)
        .ok_or(AuraError::InvalidValue("typed aura1 body length"))?;
    if bytes.len() != expected_body_len {
        return Err(AuraError::InvalidValue("typed aura1 body length"));
    }
    let mut reader = ByteReader::new(bytes);
    let mut rows = Vec::new();
    try_reserve_exact(&mut rows, record_count, "typed decode row allocation")?;
    for _ in 0..record_count {
        let mut row = Vec::new();
        try_reserve_exact(
            &mut row,
            schema.fields.len(),
            "typed decode value allocation",
        )?;
        row.resize(schema.fields.len(), AuraTypedValue::I64(0));
        for field_plan in &plan.fields {
            if field_plan.encoding != FieldEncoding::Absolute {
                return Err(AuraError::InvalidValue("typed aura1 field encoding"));
            }
            let field = schema
                .fields
                .get(usize::from(field_plan.field_index))
                .ok_or(AuraError::InvalidValue("field index"))?;
            row[usize::from(field.index)] = match field.field_type {
                FieldType::Opaque16 => {
                    if field_plan.width != PhysicalWidth::I128 {
                        return Err(AuraError::InvalidValue("opaque16 physical width"));
                    }
                    let bytes = reader.read_exact(16)?;
                    AuraTypedValue::Opaque16([
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                        bytes[7], bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13],
                        bytes[14], bytes[15],
                    ])
                }
                FieldType::I128 => return Err(AuraError::InvalidValue("typed i128 aura1")),
                _ => AuraTypedValue::I64(read_i64_width(&mut reader, field_plan.width)?),
            };
        }
        rows.push(row);
    }
    reader.finish()?;
    Ok(rows)
}

fn i64_rows_to_typed(rows: Vec<Vec<i64>>) -> Vec<Vec<AuraTypedValue>> {
    rows.into_iter()
        .map(|row| row.into_iter().map(AuraTypedValue::I64).collect())
        .collect()
}

fn encode_aura0_body(rows: &[Vec<i64>], plan: &Aura0Plan) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let field_count = rows.first().map(|row| row.len()).unwrap_or(0);
    for field_plan in &plan.fields {
        let field_index = usize::from(field_plan.field_index);
        if field_index >= field_count {
            return Err(AuraError::InvalidValue("field index"));
        }
        encode_aura0_column(&mut out, rows, field_plan)?;
    }
    Ok(out)
}

fn encode_aura0_column(
    out: &mut Vec<u8>,
    rows: &[Vec<i64>],
    field_plan: &crate::PhysicalFieldPlan,
) -> Result<()> {
    let field_index = usize::from(field_plan.field_index);
    match field_plan.encoding {
        FieldEncoding::Absolute => {
            for row in rows {
                write_i64_width(out, row[field_index], field_plan.width)?;
            }
        }
        FieldEncoding::DeltaBase => {
            for row in rows {
                write_i64_width(
                    out,
                    checked_delta(row[field_index], field_plan.base_value)?,
                    field_plan.width,
                )?;
            }
        }
        FieldEncoding::DeltaPrevious => {
            if let Some(first) = rows.first() {
                if first[field_index] != field_plan.base_value {
                    return Err(AuraError::InvalidValue("delta previous base"));
                }
            }
            for pair in rows.windows(2) {
                write_i64_width(
                    out,
                    checked_delta(pair[1][field_index], pair[0][field_index])?,
                    field_plan.width,
                )?;
            }
        }
        FieldEncoding::TimestampStep | FieldEncoding::ImplicitFixedStep => {
            for (row_index, row) in rows.iter().enumerate() {
                let expected =
                    checked_step_value(field_plan.base_value, row_index, field_plan.step)?;
                if row[field_index] != expected {
                    return Err(AuraError::InvalidValue("fixed step field"));
                }
            }
        }
        FieldEncoding::DeltaRelated => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            for row in rows {
                write_i64_width(
                    out,
                    checked_delta(row[field_index], row[reference_index])?,
                    field_plan.width,
                )?;
            }
        }
        FieldEncoding::DerivedOffset => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            for row in rows {
                let expected = checked_sum(row[reference_index], field_plan.base_value)?;
                if row[field_index] != expected {
                    return Err(AuraError::InvalidValue("derived offset field"));
                }
            }
        }
        FieldEncoding::BitpackedDeltaPreviousFieldOffset => {
            let reference_index = field_reference_index(field_plan, rows, field_index)?;
            if let Some(first) = rows.first() {
                if first[field_index] != field_plan.base_value {
                    return Err(AuraError::InvalidValue("delta previous base"));
                }
            }
            let values = rows
                .iter()
                .skip(1)
                .zip(rows)
                .map(|(row, previous_row)| {
                    checked_biased_delta(
                        row[field_index],
                        previous_row[reference_index],
                        field_plan.step,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_unsigned_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedDeltaPrevious => {
            if let Some(first) = rows.first() {
                if first[field_index] != field_plan.base_value {
                    return Err(AuraError::InvalidValue("delta previous base"));
                }
            }
            let values = rows
                .windows(2)
                .map(|pair| checked_delta(pair[1][field_index], pair[0][field_index]))
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_signed_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedDeltaBase => {
            let values = rows
                .iter()
                .map(|row| checked_unsigned_delta(row[field_index], field_plan.base_value))
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_unsigned_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedDeltaRelated => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            let values = rows
                .iter()
                .map(|row| checked_delta(row[field_index], row[reference_index]))
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_signed_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedDeltaRelatedOffset => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            let values = rows
                .iter()
                .map(|row| {
                    checked_biased_delta(
                        row[field_index],
                        row[reference_index],
                        field_plan.base_value,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_unsigned_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedDeltaPreviousOffset => {
            if let Some(first) = rows.first() {
                if first[field_index] != field_plan.base_value {
                    return Err(AuraError::InvalidValue("delta previous base"));
                }
            }
            let values = rows
                .windows(2)
                .map(|pair| {
                    checked_biased_delta(
                        pair[1][field_index],
                        pair[0][field_index],
                        field_plan.step,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_unsigned_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedMaxPlusResidual | FieldEncoding::BitpackedMinMinusResidual => {
            let first_reference_index = field_reference_index(field_plan, rows, field_index)?;
            let second_reference_index = step_reference_index(field_plan, rows)?;
            let values = rows
                .iter()
                .map(|row| {
                    let reference = match field_plan.encoding {
                        FieldEncoding::BitpackedMaxPlusResidual => {
                            row[first_reference_index].max(row[second_reference_index])
                        }
                        FieldEncoding::BitpackedMinMinusResidual => {
                            row[first_reference_index].min(row[second_reference_index])
                        }
                        _ => unreachable!(),
                    };
                    match field_plan.encoding {
                        FieldEncoding::BitpackedMaxPlusResidual => {
                            checked_biased_delta(row[field_index], reference, field_plan.base_value)
                        }
                        FieldEncoding::BitpackedMinMinusResidual => {
                            checked_biased_delta(reference, row[field_index], field_plan.base_value)
                        }
                        _ => unreachable!(),
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_unsigned_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedProductResidual => {
            let quantity_index = field_reference_index(field_plan, rows, field_index)?;
            let (price_index, divisor) = product_args(field_plan, rows)?;
            let values = rows
                .iter()
                .map(|row| {
                    let predicted =
                        checked_product_div(row[quantity_index], row[price_index], divisor)?;
                    checked_biased_i128_delta(row[field_index], predicted, field_plan.base_value)
                })
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_unsigned_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedProportionalResidual => {
            let total_value_index = field_reference_index(field_plan, rows, field_index)?;
            let (child_quantity_index, total_quantity_index) = proportional_args(field_plan, rows)?;
            let values = rows
                .iter()
                .map(|row| {
                    let predicted = checked_product_div(
                        row[total_value_index],
                        row[child_quantity_index],
                        row[total_quantity_index],
                    )?;
                    checked_biased_i128_delta(row[field_index], predicted, field_plan.base_value)
                })
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_unsigned_values(&values, field_plan.bit_width)?);
        }
    }
    Ok(())
}

fn decode_aura0_body(
    bytes: &[u8],
    plan: &Aura0Plan,
    record_count: usize,
    field_count: usize,
) -> Result<Vec<Vec<i64>>> {
    let _ = validate_i64_decode_dimensions_usize(record_count, field_count, bytes.len())?;
    if bytes.len() != aura0_body_capacity(record_count, plan)? {
        return Err(AuraError::InvalidValue("aura0 body length"));
    }
    let mut reader = ByteReader::new(bytes);
    let mut rows = zero_i64_rows(record_count, field_count)?;
    let mut pending = Vec::new();
    for field_plan in &plan.fields {
        let field_index = usize::from(field_plan.field_index);
        if field_index >= field_count {
            return Err(AuraError::InvalidValue("field index"));
        }
        match field_plan.encoding {
            FieldEncoding::BitpackedDeltaPreviousFieldOffset => {
                let values = read_bitpacked_unsigned_values(
                    &mut reader,
                    field_plan.bit_width,
                    rows.len().saturating_sub(1),
                )?;
                pending.push((*field_plan, values));
            }
            FieldEncoding::BitpackedDeltaRelatedOffset
            | FieldEncoding::BitpackedProductResidual
            | FieldEncoding::BitpackedProportionalResidual => {
                let values =
                    read_bitpacked_unsigned_values(&mut reader, field_plan.bit_width, rows.len())?;
                pending.push((*field_plan, values));
            }
            FieldEncoding::BitpackedMaxPlusResidual | FieldEncoding::BitpackedMinMinusResidual => {
                let values =
                    read_bitpacked_unsigned_values(&mut reader, field_plan.bit_width, rows.len())?;
                pending.push((*field_plan, values));
            }
            _ => decode_aura0_column(&mut reader, &mut rows, field_plan)?,
        }
    }
    if rows.is_empty() {
        reader.finish()?;
        return Ok(rows);
    }
    let mut consumed = vec![false; pending.len()];
    for previous_index in 0..pending.len() {
        let (previous_plan, previous_values) = &pending[previous_index];
        if previous_plan.encoding != FieldEncoding::BitpackedDeltaPreviousFieldOffset {
            continue;
        }
        let Some(related_index) = pending.iter().position(|(related_plan, _)| {
            related_plan.encoding == FieldEncoding::BitpackedDeltaRelatedOffset
                && Some(related_plan.field_index) == previous_plan.reference_field_index
                && related_plan.reference_field_index == Some(previous_plan.field_index)
        }) else {
            decode_pending_previous_field_offset(&mut rows, previous_plan, previous_values)?;
            consumed[previous_index] = true;
            continue;
        };
        let (related_plan, related_values) = &pending[related_index];
        decode_pending_previous_related_pair(
            &mut rows,
            previous_plan,
            previous_values,
            related_plan,
            related_values,
        )?;
        consumed[previous_index] = true;
        consumed[related_index] = true;
    }
    for (index, (field_plan, values)) in pending.iter().enumerate() {
        if consumed[index] {
            continue;
        }
        match field_plan.encoding {
            FieldEncoding::BitpackedDeltaPreviousFieldOffset => {
                decode_pending_previous_field_offset(&mut rows, field_plan, values)?;
                consumed[index] = true;
            }
            FieldEncoding::BitpackedDeltaRelatedOffset => {
                decode_pending_related_offset(&mut rows, field_plan, values)?;
                consumed[index] = true;
            }
            _ => {}
        }
    }
    for (index, (field_plan, values)) in pending.iter().enumerate() {
        if consumed[index] {
            continue;
        }
        if field_plan.encoding == FieldEncoding::BitpackedProductResidual {
            decode_pending_product_residual(&mut rows, field_plan, values)?;
            consumed[index] = true;
        }
    }
    for (index, (field_plan, values)) in pending.iter().enumerate() {
        if consumed[index] {
            continue;
        }
        if field_plan.encoding == FieldEncoding::BitpackedProportionalResidual {
            decode_pending_proportional_residual(&mut rows, field_plan, values)?;
            consumed[index] = true;
        }
    }
    for (index, (field_plan, values)) in pending.iter().enumerate() {
        if consumed[index] {
            continue;
        }
        if matches!(
            field_plan.encoding,
            FieldEncoding::BitpackedMaxPlusResidual | FieldEncoding::BitpackedMinMinusResidual
        ) {
            decode_pending_aura0_column(&mut rows, field_plan, values)?;
            consumed[index] = true;
        }
    }
    reader.finish()?;
    Ok(rows)
}

fn aura0_body_capacity(record_count: usize, plan: &Aura0Plan) -> Result<usize> {
    plan.fields.iter().try_fold(0usize, |total, field| {
        let value_count = match field.encoding {
            FieldEncoding::TimestampStep
            | FieldEncoding::ImplicitFixedStep
            | FieldEncoding::DerivedOffset => 0,
            FieldEncoding::DeltaPrevious
            | FieldEncoding::BitpackedDeltaPreviousFieldOffset
            | FieldEncoding::BitpackedDeltaPrevious
            | FieldEncoding::BitpackedDeltaPreviousOffset => record_count.saturating_sub(1),
            FieldEncoding::Absolute
            | FieldEncoding::DeltaBase
            | FieldEncoding::DeltaRelated
            | FieldEncoding::BitpackedDeltaBase
            | FieldEncoding::BitpackedDeltaRelated
            | FieldEncoding::BitpackedDeltaRelatedOffset
            | FieldEncoding::BitpackedMaxPlusResidual
            | FieldEncoding::BitpackedMinMinusResidual
            | FieldEncoding::BitpackedProductResidual
            | FieldEncoding::BitpackedProportionalResidual => record_count,
        };
        let bytes = match field.encoding {
            FieldEncoding::Absolute
            | FieldEncoding::DeltaBase
            | FieldEncoding::DeltaPrevious
            | FieldEncoding::DeltaRelated => value_count
                .checked_mul(usize::from(field.width.byte_width()))
                .ok_or(AuraError::InvalidValue("aura0 body length"))?,
            FieldEncoding::TimestampStep
            | FieldEncoding::ImplicitFixedStep
            | FieldEncoding::DerivedOffset => 0,
            _ => usize::try_from(bitpacked_byte_len(
                u64::try_from(value_count)
                    .map_err(|_| AuraError::InvalidValue("aura0 body length"))?,
                field.bit_width,
            ))
            .map_err(|_| AuraError::InvalidValue("aura0 body length"))?,
        };
        total
            .checked_add(bytes)
            .ok_or(AuraError::InvalidValue("aura0 body length"))
    })
}

fn decode_aura0_column(
    reader: &mut ByteReader<'_>,
    rows: &mut [Vec<i64>],
    field_plan: &crate::PhysicalFieldPlan,
) -> Result<()> {
    let field_index = usize::from(field_plan.field_index);
    match field_plan.encoding {
        FieldEncoding::Absolute => {
            for row in rows {
                row[field_index] = read_i64_width(reader, field_plan.width)?;
            }
        }
        FieldEncoding::DeltaBase => {
            for row in rows {
                row[field_index] = checked_sum(
                    field_plan.base_value,
                    read_i64_width(reader, field_plan.width)?,
                )?;
            }
        }
        FieldEncoding::DeltaPrevious => {
            if let Some(first) = rows.first_mut() {
                first[field_index] = field_plan.base_value;
            }
            for row_index in 1..rows.len() {
                let delta = read_i64_width(reader, field_plan.width)?;
                rows[row_index][field_index] =
                    checked_sum(rows[row_index - 1][field_index], delta)?;
            }
        }
        FieldEncoding::TimestampStep | FieldEncoding::ImplicitFixedStep => {
            for (row_index, row) in rows.iter_mut().enumerate() {
                row[field_index] =
                    checked_step_value(field_plan.base_value, row_index, field_plan.step)?;
            }
        }
        FieldEncoding::DeltaRelated => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            for row in rows {
                row[field_index] = checked_sum(
                    row[reference_index],
                    read_i64_width(reader, field_plan.width)?,
                )?;
            }
        }
        FieldEncoding::DerivedOffset => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            for row in rows {
                row[field_index] = checked_sum(row[reference_index], field_plan.base_value)?;
            }
        }
        FieldEncoding::BitpackedDeltaPreviousFieldOffset => {
            let field_count = rows
                .first()
                .map(Vec::len)
                .ok_or(AuraError::InvalidValue("zero-row field reference"))?;
            let reference_index = field_reference_index_for_count(field_plan, field_count)?;
            if let Some(first) = rows.first_mut() {
                first[field_index] = field_plan.base_value;
            }
            let deltas = read_bitpacked_unsigned_values(
                reader,
                field_plan.bit_width,
                rows.len().saturating_sub(1),
            )?;
            for (offset, delta) in deltas.into_iter().enumerate() {
                let row_index = offset + 1;
                let delta = checked_sum_unsigned(field_plan.step, delta)?;
                rows[row_index][field_index] =
                    checked_sum(rows[row_index - 1][reference_index], delta)?;
            }
        }
        FieldEncoding::BitpackedDeltaPrevious => {
            if let Some(first) = rows.first_mut() {
                first[field_index] = field_plan.base_value;
            }
            let deltas =
                read_bitpacked_values(reader, field_plan.bit_width, rows.len().saturating_sub(1))?;
            for (offset, delta) in deltas.into_iter().enumerate() {
                let row_index = offset + 1;
                rows[row_index][field_index] =
                    checked_sum(rows[row_index - 1][field_index], delta)?;
            }
        }
        FieldEncoding::BitpackedDeltaBase => {
            let deltas = read_bitpacked_unsigned_values(reader, field_plan.bit_width, rows.len())?;
            for (row, delta) in rows.iter_mut().zip(deltas) {
                row[field_index] = checked_sum_unsigned(field_plan.base_value, delta)?;
            }
        }
        FieldEncoding::BitpackedDeltaRelated => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            let deltas = read_bitpacked_values(reader, field_plan.bit_width, rows.len())?;
            for (row, delta) in rows.iter_mut().zip(deltas) {
                row[field_index] = checked_sum(row[reference_index], delta)?;
            }
        }
        FieldEncoding::BitpackedDeltaRelatedOffset => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            let deltas = read_bitpacked_unsigned_values(reader, field_plan.bit_width, rows.len())?;
            for (row, delta) in rows.iter_mut().zip(deltas) {
                let delta = checked_sum_unsigned(field_plan.base_value, delta)?;
                row[field_index] = checked_sum(row[reference_index], delta)?;
            }
        }
        FieldEncoding::BitpackedDeltaPreviousOffset => {
            if let Some(first) = rows.first_mut() {
                first[field_index] = field_plan.base_value;
            }
            let deltas = read_bitpacked_unsigned_values(
                reader,
                field_plan.bit_width,
                rows.len().saturating_sub(1),
            )?;
            for (offset, delta) in deltas.into_iter().enumerate() {
                let row_index = offset + 1;
                let delta = checked_sum_unsigned(field_plan.step, delta)?;
                rows[row_index][field_index] =
                    checked_sum(rows[row_index - 1][field_index], delta)?;
            }
        }
        FieldEncoding::BitpackedMaxPlusResidual | FieldEncoding::BitpackedMinMinusResidual => {
            return Err(AuraError::InvalidValue("pending field"));
        }
        FieldEncoding::BitpackedProductResidual => {
            let field_count = rows
                .first()
                .map(Vec::len)
                .ok_or(AuraError::InvalidValue("zero-row field reference"))?;
            let quantity_index = field_reference_index_for_count(field_plan, field_count)?;
            let (price_index, divisor) = product_args_for_count(field_plan, field_count)?;
            let residuals =
                read_bitpacked_unsigned_values(reader, field_plan.bit_width, rows.len())?;
            for (row, residual) in rows.iter_mut().zip(residuals) {
                let predicted =
                    checked_product_div(row[quantity_index], row[price_index], divisor)?;
                row[field_index] = checked_i128_sum_unsigned(
                    predicted + i128::from(field_plan.base_value),
                    residual,
                )?;
            }
        }
        FieldEncoding::BitpackedProportionalResidual => {
            let field_count = rows
                .first()
                .map(Vec::len)
                .ok_or(AuraError::InvalidValue("zero-row field reference"))?;
            let total_value_index = field_reference_index_for_count(field_plan, field_count)?;
            let (child_quantity_index, total_quantity_index) =
                proportional_args_for_count(field_plan, field_count)?;
            let residuals =
                read_bitpacked_unsigned_values(reader, field_plan.bit_width, rows.len())?;
            for (row, residual) in rows.iter_mut().zip(residuals) {
                let predicted = checked_product_div(
                    row[total_value_index],
                    row[child_quantity_index],
                    row[total_quantity_index],
                )?;
                row[field_index] = checked_i128_sum_unsigned(
                    predicted + i128::from(field_plan.base_value),
                    residual,
                )?;
            }
        }
    }
    Ok(())
}

fn decode_pending_aura0_column(
    rows: &mut [Vec<i64>],
    field_plan: &crate::PhysicalFieldPlan,
    values: &[u64],
) -> Result<()> {
    let field_index = usize::from(field_plan.field_index);
    let first_reference_index = field_reference_index_for_count(field_plan, rows[0].len())?;
    let second_reference_index = step_reference_index_for_count(field_plan, rows[0].len())?;
    for (row, residual) in rows.iter_mut().zip(values.iter().copied()) {
        let reference = match field_plan.encoding {
            FieldEncoding::BitpackedMaxPlusResidual => {
                row[first_reference_index].max(row[second_reference_index])
            }
            FieldEncoding::BitpackedMinMinusResidual => {
                row[first_reference_index].min(row[second_reference_index])
            }
            _ => return Err(AuraError::InvalidValue("pending field")),
        };
        let delta = checked_sum_unsigned(field_plan.base_value, residual)?;
        row[field_index] = match field_plan.encoding {
            FieldEncoding::BitpackedMaxPlusResidual => checked_sum(reference, delta)?,
            FieldEncoding::BitpackedMinMinusResidual => reference
                .checked_sub(delta)
                .ok_or(AuraError::InvalidValue("delta value"))?,
            _ => unreachable!(),
        };
    }
    Ok(())
}

fn decode_pending_previous_field_offset(
    rows: &mut [Vec<i64>],
    field_plan: &crate::PhysicalFieldPlan,
    values: &[u64],
) -> Result<()> {
    let field_index = usize::from(field_plan.field_index);
    let reference_index = field_reference_index_for_count(field_plan, rows[0].len())?;
    if let Some(first) = rows.first_mut() {
        first[field_index] = field_plan.base_value;
    }
    for (offset, residual) in values.iter().copied().enumerate() {
        let row_index = offset + 1;
        let delta = checked_sum_unsigned(field_plan.step, residual)?;
        rows[row_index][field_index] = checked_sum(rows[row_index - 1][reference_index], delta)?;
    }
    Ok(())
}

fn decode_pending_previous_related_pair(
    rows: &mut [Vec<i64>],
    previous_plan: &crate::PhysicalFieldPlan,
    previous_values: &[u64],
    related_plan: &crate::PhysicalFieldPlan,
    related_values: &[u64],
) -> Result<()> {
    let previous_field_index = usize::from(previous_plan.field_index);
    let related_field_index = usize::from(related_plan.field_index);
    if related_values.len() != rows.len()
        || previous_values.len() != rows.len().saturating_sub(1)
        || previous_plan.reference_field_index != Some(related_plan.field_index)
        || related_plan.reference_field_index != Some(previous_plan.field_index)
    {
        return Err(AuraError::InvalidValue("previous related pair"));
    }
    if let Some(first) = rows.first_mut() {
        first[previous_field_index] = previous_plan.base_value;
    }
    for row_index in 0..rows.len() {
        if row_index > 0 {
            let delta = checked_sum_unsigned(previous_plan.step, previous_values[row_index - 1])?;
            rows[row_index][previous_field_index] =
                checked_sum(rows[row_index - 1][related_field_index], delta)?;
        }
        let related_delta =
            checked_sum_unsigned(related_plan.base_value, related_values[row_index])?;
        rows[row_index][related_field_index] =
            checked_sum(rows[row_index][previous_field_index], related_delta)?;
    }
    Ok(())
}

fn decode_pending_related_offset(
    rows: &mut [Vec<i64>],
    field_plan: &crate::PhysicalFieldPlan,
    values: &[u64],
) -> Result<()> {
    let field_index = usize::from(field_plan.field_index);
    let reference_index = field_reference_index_for_count(field_plan, rows[0].len())?;
    if values.len() != rows.len() {
        return Err(AuraError::InvalidValue("pending field"));
    }
    for (row, residual) in rows.iter_mut().zip(values.iter().copied()) {
        let delta = checked_sum_unsigned(field_plan.base_value, residual)?;
        row[field_index] = checked_sum(row[reference_index], delta)?;
    }
    Ok(())
}

fn decode_pending_product_residual(
    rows: &mut [Vec<i64>],
    field_plan: &crate::PhysicalFieldPlan,
    values: &[u64],
) -> Result<()> {
    let field_index = usize::from(field_plan.field_index);
    let quantity_index = field_reference_index_for_count(field_plan, rows[0].len())?;
    let (price_index, divisor) = product_args_for_count(field_plan, rows[0].len())?;
    if values.len() != rows.len() {
        return Err(AuraError::InvalidValue("pending field"));
    }
    for (row, residual) in rows.iter_mut().zip(values.iter().copied()) {
        let predicted = checked_product_div(row[quantity_index], row[price_index], divisor)?;
        row[field_index] =
            checked_i128_sum_unsigned(predicted + i128::from(field_plan.base_value), residual)?;
    }
    Ok(())
}

fn decode_pending_proportional_residual(
    rows: &mut [Vec<i64>],
    field_plan: &crate::PhysicalFieldPlan,
    values: &[u64],
) -> Result<()> {
    let field_index = usize::from(field_plan.field_index);
    let total_value_index = field_reference_index_for_count(field_plan, rows[0].len())?;
    let (child_quantity_index, total_quantity_index) =
        proportional_args_for_count(field_plan, rows[0].len())?;
    if values.len() != rows.len() {
        return Err(AuraError::InvalidValue("pending field"));
    }
    for (row, residual) in rows.iter_mut().zip(values.iter().copied()) {
        let predicted = checked_product_div(
            row[total_value_index],
            row[child_quantity_index],
            row[total_quantity_index],
        )?;
        row[field_index] =
            checked_i128_sum_unsigned(predicted + i128::from(field_plan.base_value), residual)?;
    }
    Ok(())
}

fn read_bitpacked_values(
    reader: &mut ByteReader<'_>,
    bit_width: u8,
    value_count: usize,
) -> Result<Vec<i64>> {
    let byte_len = usize::try_from(bitpacked_byte_len(
        u64::try_from(value_count).map_err(|_| AuraError::InvalidValue("bitpacked value count"))?,
        bit_width,
    ))
    .map_err(|_| AuraError::InvalidValue("bitpacked length"))?;
    unpack_signed_values(reader.read_exact(byte_len)?, bit_width, value_count)
}

fn read_bitpacked_unsigned_values(
    reader: &mut ByteReader<'_>,
    bit_width: u8,
    value_count: usize,
) -> Result<Vec<u64>> {
    let byte_len = usize::try_from(bitpacked_byte_len(
        u64::try_from(value_count).map_err(|_| AuraError::InvalidValue("bitpacked value count"))?,
        bit_width,
    ))
    .map_err(|_| AuraError::InvalidValue("bitpacked length"))?;
    unpack_unsigned_values(reader.read_exact(byte_len)?, bit_width, value_count)
}

fn related_reference_index(
    field_plan: &crate::PhysicalFieldPlan,
    field_index: usize,
) -> Result<usize> {
    let reference_index = usize::from(
        field_plan
            .reference_field_index
            .ok_or(AuraError::InvalidValue("reference field"))?,
    );
    if reference_index >= field_index {
        return Err(AuraError::InvalidValue("reference field order"));
    }
    Ok(reference_index)
}

fn field_reference_index(
    field_plan: &crate::PhysicalFieldPlan,
    rows: &[Vec<i64>],
    field_index: usize,
) -> Result<usize> {
    let field_count = rows.first().map(|row| row.len()).unwrap_or(0);
    let reference_index = field_reference_index_for_count(field_plan, field_count)?;
    if reference_index == field_index {
        return Err(AuraError::InvalidValue("reference field"));
    }
    Ok(reference_index)
}

fn field_reference_index_for_count(
    field_plan: &crate::PhysicalFieldPlan,
    field_count: usize,
) -> Result<usize> {
    let reference_index = usize::from(
        field_plan
            .reference_field_index
            .ok_or(AuraError::InvalidValue("reference field"))?,
    );
    if reference_index >= field_count {
        return Err(AuraError::InvalidValue("reference field"));
    }
    Ok(reference_index)
}

fn step_reference_index(field_plan: &crate::PhysicalFieldPlan, rows: &[Vec<i64>]) -> Result<usize> {
    let field_count = rows.first().map(|row| row.len()).unwrap_or(0);
    step_reference_index_for_count(field_plan, field_count)
}

fn step_reference_index_for_count(
    field_plan: &crate::PhysicalFieldPlan,
    field_count: usize,
) -> Result<usize> {
    let index =
        usize::try_from(field_plan.step).map_err(|_| AuraError::InvalidValue("reference field"))?;
    if index >= field_count {
        return Err(AuraError::InvalidValue("reference field"));
    }
    Ok(index)
}

fn product_args(field_plan: &crate::PhysicalFieldPlan, rows: &[Vec<i64>]) -> Result<(usize, i64)> {
    let field_count = rows.first().map(|row| row.len()).unwrap_or(0);
    product_args_for_count(field_plan, field_count)
}

fn product_args_for_count(
    field_plan: &crate::PhysicalFieldPlan,
    field_count: usize,
) -> Result<(usize, i64)> {
    let (reference, divisor) =
        unpack_ref_divisor(field_plan.step).ok_or(AuraError::InvalidValue("product args"))?;
    let reference = usize::from(reference);
    if reference >= field_count {
        return Err(AuraError::InvalidValue("reference field"));
    }
    Ok((reference, i64::from(divisor)))
}

fn proportional_args(
    field_plan: &crate::PhysicalFieldPlan,
    rows: &[Vec<i64>],
) -> Result<(usize, usize)> {
    let field_count = rows.first().map(|row| row.len()).unwrap_or(0);
    proportional_args_for_count(field_plan, field_count)
}

fn proportional_args_for_count(
    field_plan: &crate::PhysicalFieldPlan,
    field_count: usize,
) -> Result<(usize, usize)> {
    let (first, second) =
        unpack_two_refs(field_plan.step).ok_or(AuraError::InvalidValue("proportional args"))?;
    let first = usize::from(first);
    let second = usize::from(second);
    if first >= field_count || second >= field_count {
        return Err(AuraError::InvalidValue("reference field"));
    }
    Ok((first, second))
}

fn checked_delta(value: i64, reference: i64) -> Result<i64> {
    value
        .checked_sub(reference)
        .ok_or(AuraError::InvalidValue("delta value"))
}

fn checked_unsigned_delta(value: i64, reference: i64) -> Result<u64> {
    let delta = i128::from(value) - i128::from(reference);
    u64::try_from(delta).map_err(|_| AuraError::InvalidValue("delta value"))
}

fn checked_biased_delta(value: i64, reference: i64, bias: i64) -> Result<u64> {
    let delta = i128::from(value) - i128::from(reference) - i128::from(bias);
    u64::try_from(delta).map_err(|_| AuraError::InvalidValue("delta value"))
}

fn checked_biased_i128_delta(value: i64, reference: i128, bias: i64) -> Result<u64> {
    let delta = i128::from(value) - reference - i128::from(bias);
    u64::try_from(delta).map_err(|_| AuraError::InvalidValue("delta value"))
}

fn checked_sum(value: i64, delta: i64) -> Result<i64> {
    value
        .checked_add(delta)
        .ok_or(AuraError::InvalidValue("delta value"))
}

fn checked_sum_unsigned(value: i64, delta: u64) -> Result<i64> {
    let sum = i128::from(value) + i128::from(delta);
    i64::try_from(sum).map_err(|_| AuraError::InvalidValue("delta value"))
}

fn checked_i128_sum_unsigned(value: i128, delta: u64) -> Result<i64> {
    let sum = value
        .checked_add(i128::from(delta))
        .ok_or(AuraError::InvalidValue("delta value"))?;
    i64::try_from(sum).map_err(|_| AuraError::InvalidValue("delta value"))
}

fn checked_product_div(left: i64, right: i64, divisor: i64) -> Result<i128> {
    if divisor == 0 {
        return Err(AuraError::InvalidValue("product divisor"));
    }
    i128::from(left)
        .checked_mul(i128::from(right))
        .and_then(|value| value.checked_div(i128::from(divisor)))
        .ok_or(AuraError::InvalidValue("product value"))
}

fn checked_step_value(base: i64, row_index: usize, step: i64) -> Result<i64> {
    let offset = step
        .checked_mul(i64::try_from(row_index).map_err(|_| AuraError::InvalidValue("row index"))?)
        .ok_or(AuraError::InvalidValue("fixed step field"))?;
    checked_sum(base, offset)
}

fn encode_aura1_body(rows: &[Vec<i64>], plan: &Aura1Plan) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for row in rows {
        for field_plan in &plan.fields {
            write_i64_width(
                &mut out,
                row[usize::from(field_plan.field_index)],
                field_plan.width,
            )?;
        }
    }
    Ok(out)
}

fn encode_aura1_body_from_raw_body(
    raw_body: &[u8],
    schema: &SchemaDescriptor,
    plan: &Aura1Plan,
) -> Result<(Vec<u8>, usize)> {
    let mut reader = ByteReader::new(raw_body);
    let record_count = usize::try_from(reader.read_u64_le()?)
        .map_err(|_| AuraError::InvalidValue("record count"))?;
    let field_count = usize::from(reader.read_u16_le()?);
    if field_count != schema.fields.len() {
        return Err(AuraError::InvalidValue("field count"));
    }
    for field_plan in &plan.fields {
        if usize::from(field_plan.field_index) >= field_count {
            return Err(AuraError::InvalidValue("field index"));
        }
    }

    let row_width = plan
        .fields
        .iter()
        .map(|field| usize::from(field.width.byte_width()))
        .try_fold(0usize, |acc, width| {
            acc.checked_add(width)
                .ok_or(AuraError::InvalidValue("body length"))
        })?;
    let mut out = Vec::with_capacity(
        record_count
            .checked_mul(row_width)
            .ok_or(AuraError::InvalidValue("body length"))?,
    );
    let mut row = vec![0i64; field_count];
    for _ in 0..record_count {
        for value in &mut row {
            *value = reader.read_i64_le()?;
        }
        for field_plan in &plan.fields {
            write_i64_width(
                &mut out,
                row[usize::from(field_plan.field_index)],
                field_plan.width,
            )?;
        }
    }
    reader.finish()?;
    Ok((out, record_count))
}

#[allow(clippy::needless_range_loop)]
fn encode_aura1_body_from_columns(columns: &[Vec<i64>], plan: &Aura1Plan) -> Result<Vec<u8>> {
    let field_count = columns.len();
    let record_count = columns.first().map_or(0, Vec::len);
    for column in columns {
        if column.len() != record_count {
            return Err(AuraError::InvalidValue("record count"));
        }
    }
    for field_plan in &plan.fields {
        if usize::from(field_plan.field_index) >= field_count {
            return Err(AuraError::InvalidValue("field index"));
        }
    }
    if let Some(body) =
        encode_aura1_partitioned_sparse_body_from_columns(columns, plan, record_count)?
    {
        return Ok(body);
    }
    let mut out = Vec::with_capacity(aura1_body_capacity(record_count, plan)?);
    for row_index in 0..record_count {
        for field_plan in &plan.fields {
            write_i64_width(
                &mut out,
                columns[usize::from(field_plan.field_index)][row_index],
                field_plan.width,
            )?;
        }
    }
    Ok(out)
}

fn aura1_body_capacity(record_count: usize, plan: &Aura1Plan) -> Result<usize> {
    let row_width = plan
        .fields
        .iter()
        .map(|field| usize::from(field.width.byte_width()))
        .try_fold(0usize, |acc, width| {
            acc.checked_add(width)
                .ok_or(AuraError::InvalidValue("body length"))
        })?;
    record_count
        .checked_mul(row_width)
        .ok_or(AuraError::InvalidValue("body length"))
}

fn encode_aura1_partitioned_sparse_body_from_columns(
    columns: &[Vec<i64>],
    plan: &Aura1Plan,
    record_count: usize,
) -> Result<Option<Vec<u8>>> {
    if columns.len() != 8 || plan.fields.len() != 8 {
        return Ok(None);
    }
    let expected = [
        (0u16, PhysicalWidth::I64),
        (1, PhysicalWidth::I64),
        (2, PhysicalWidth::I32),
        (3, PhysicalWidth::I8),
        (4, PhysicalWidth::I64),
        (5, PhysicalWidth::I64),
        (6, PhysicalWidth::I64),
        (7, PhysicalWidth::I8),
    ];
    if !plan
        .fields
        .iter()
        .zip(expected)
        .all(|(field, (index, width))| field.field_index == index && field.width == width)
    {
        return Ok(None);
    }

    const ROW_WIDTH: usize = 46;
    let body_len = record_count
        .checked_mul(ROW_WIDTH)
        .ok_or(AuraError::InvalidValue("body length"))?;
    let c0 = columns[0].as_slice();
    let c1 = columns[1].as_slice();
    let c2 = columns[2].as_slice();
    let c3 = columns[3].as_slice();
    let c4 = columns[4].as_slice();
    let c5 = columns[5].as_slice();
    let c6 = columns[6].as_slice();
    let c7 = columns[7].as_slice();

    let mut out = vec![0u8; body_len];
    for row_index in 0..record_count {
        let v2 = i32::try_from(c2[row_index]).map_err(|_| AuraError::InvalidValue("i32 value"))?;
        let v3 = i8::try_from(c3[row_index]).map_err(|_| AuraError::InvalidValue("i8 value"))?;
        let v7 = i8::try_from(c7[row_index]).map_err(|_| AuraError::InvalidValue("i8 value"))?;
        let row = &mut out[row_index * ROW_WIDTH..][..ROW_WIDTH];
        write_unaligned_i64_le(row, 0, c0[row_index]);
        write_unaligned_i64_le(row, 8, c1[row_index]);
        write_unaligned_i32_le(row, 16, v2);
        row[20] = v3 as u8;
        write_unaligned_i64_le(row, 21, c4[row_index]);
        write_unaligned_i64_le(row, 29, c5[row_index]);
        write_unaligned_i64_le(row, 37, c6[row_index]);
        row[45] = v7 as u8;
    }
    Ok(Some(out))
}

fn write_unaligned_i64_le(row: &mut [u8], offset: usize, value: i64) {
    debug_assert!(offset + std::mem::size_of::<i64>() <= row.len());
    unsafe {
        row.as_mut_ptr()
            .add(offset)
            .cast::<i64>()
            .write_unaligned(value.to_le());
    }
}

fn write_unaligned_i32_le(row: &mut [u8], offset: usize, value: i32) {
    debug_assert!(offset + std::mem::size_of::<i32>() <= row.len());
    unsafe {
        row.as_mut_ptr()
            .add(offset)
            .cast::<i32>()
            .write_unaligned(value.to_le());
    }
}

fn decode_aura1_body(
    bytes: &[u8],
    plan: &Aura1Plan,
    record_count: usize,
    field_count: usize,
) -> Result<Vec<Vec<i64>>> {
    let _ = validate_i64_decode_dimensions_usize(record_count, field_count, bytes.len())?;
    validate_aura1_plan_fields(plan, field_count)?;
    let expected_body_len = aura1_body_capacity(record_count, plan)?;
    if bytes.len() != expected_body_len {
        return Err(AuraError::InvalidValue("aura1 body length"));
    }
    let mut reader = ByteReader::new(bytes);
    let mut rows = Vec::new();
    try_reserve_exact(&mut rows, record_count, "i64 decode row allocation")?;
    for _ in 0..record_count {
        let mut row = Vec::new();
        try_reserve_exact(&mut row, field_count, "i64 decode value allocation")?;
        row.resize(field_count, 0);
        for field_plan in &plan.fields {
            row[usize::from(field_plan.field_index)] =
                read_i64_width(&mut reader, field_plan.width)?;
        }
        rows.push(row);
    }
    reader.finish()?;
    Ok(rows)
}

pub(crate) fn visit_aura1_body<F>(
    bytes: &[u8],
    plan: &Aura1Plan,
    record_count: usize,
    field_count: usize,
    visitor: &mut F,
) -> Result<usize>
where
    F: FnMut(&[i64]) -> Result<()>,
{
    if field_count > MAX_VISITOR_FIELDS {
        return Err(AuraError::InvalidValue("field count"));
    }
    for field_plan in &plan.fields {
        if usize::from(field_plan.field_index) >= field_count {
            return Err(AuraError::InvalidValue("field index"));
        }
    }

    let mut reader = ByteReader::new(bytes);
    let mut row = [0i64; MAX_VISITOR_FIELDS];
    for _ in 0..record_count {
        for value in &mut row[..field_count] {
            *value = 0;
        }
        for field_plan in &plan.fields {
            row[usize::from(field_plan.field_index)] =
                read_i64_width(&mut reader, field_plan.width)?;
        }
        visitor(&row[..field_count])?;
    }
    reader.finish()?;
    Ok(record_count)
}

fn write_i64_width(out: &mut Vec<u8>, value: i64, width: PhysicalWidth) -> Result<()> {
    match width {
        PhysicalWidth::Zero => {
            if value == 0 {
                Ok(())
            } else {
                Err(AuraError::InvalidValue("zero-width value"))
            }
        }
        PhysicalWidth::I8 => {
            let value = i8::try_from(value).map_err(|_| AuraError::InvalidValue("i8 value"))?;
            out.push(value as u8);
            Ok(())
        }
        PhysicalWidth::I16 => {
            let value = i16::try_from(value).map_err(|_| AuraError::InvalidValue("i16 value"))?;
            out.extend_from_slice(&value.to_le_bytes());
            Ok(())
        }
        PhysicalWidth::I32 => {
            let value = i32::try_from(value).map_err(|_| AuraError::InvalidValue("i32 value"))?;
            out.extend_from_slice(&value.to_le_bytes());
            Ok(())
        }
        PhysicalWidth::I64 => {
            put_i64_le(out, value);
            Ok(())
        }
        PhysicalWidth::I128 => {
            out.extend_from_slice(&i128::from(value).to_le_bytes());
            Ok(())
        }
    }
}

fn read_i64_width(reader: &mut ByteReader<'_>, width: PhysicalWidth) -> Result<i64> {
    match width {
        PhysicalWidth::Zero => Ok(0),
        PhysicalWidth::I8 => Ok(reader.read_u8()? as i8 as i64),
        PhysicalWidth::I16 => {
            let bytes = reader.read_exact(2)?;
            Ok(i16::from_le_bytes([bytes[0], bytes[1]]) as i64)
        }
        PhysicalWidth::I32 => {
            let bytes = reader.read_exact(4)?;
            Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i64)
        }
        PhysicalWidth::I64 => reader.read_i64_le(),
        PhysicalWidth::I128 => {
            let bytes = reader.read_exact(16)?;
            let value = i128::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]);
            i64::try_from(value).map_err(|_| AuraError::InvalidValue("i128 value"))
        }
    }
}

pub(crate) fn validate_rows(schema: &SchemaDescriptor, rows: &[Vec<i64>]) -> Result<()> {
    for row in rows {
        if row.len() != schema.fields.len() {
            return Err(AuraError::InvalidValue("record field count"));
        }
    }
    Ok(())
}

fn validate_columns(
    schema: &SchemaDescriptor,
    record_count: usize,
    columns: &[Vec<i64>],
) -> Result<()> {
    if columns.len() != schema.fields.len() {
        return Err(AuraError::InvalidValue("record field count"));
    }
    for column in columns {
        if column.len() != record_count {
            return Err(AuraError::InvalidValue("stream value count"));
        }
    }
    Ok(())
}

pub(crate) fn validate_typed_rows(
    schema: &SchemaDescriptor,
    rows: &[Vec<AuraTypedValue>],
) -> Result<()> {
    for row in rows {
        if row.len() != schema.fields.len() {
            return Err(AuraError::InvalidValue("record field count"));
        }
        for field in &schema.fields {
            let value = &row[usize::from(field.index)];
            let _ = match field.field_type {
                FieldType::I128 => match value {
                    AuraTypedValue::I128(value) => *value,
                    AuraTypedValue::I64(value) => i128::from(*value),
                    AuraTypedValue::Opaque16(_) => {
                        return Err(AuraError::InvalidValue("typed value"));
                    }
                },
                FieldType::Opaque16 => {
                    if !matches!(value, AuraTypedValue::Opaque16(_)) {
                        return Err(AuraError::InvalidValue("typed value"));
                    }
                    0
                }
                _ => i128::from(typed_value_i64_for_field(field.field_type, value)?),
            };
        }
    }
    Ok(())
}

fn observe_typed_record(
    stats: &mut IngestStats,
    schema: &SchemaDescriptor,
    row: &[AuraTypedValue],
) -> Result<()> {
    if row.len() != schema.fields.len() {
        return Err(AuraError::InvalidValue("record field count"));
    }
    stats.observe_record();
    for field in &schema.fields {
        if let Some(value) = typed_value_as_i64(&row[usize::from(field.index)]) {
            stats.observe_i64(field.index, value)?;
        }
    }
    Ok(())
}

fn typed_value_as_i64(value: &AuraTypedValue) -> Option<i64> {
    match value {
        AuraTypedValue::I64(value) => Some(*value),
        AuraTypedValue::I128(value) => i64::try_from(*value).ok(),
        AuraTypedValue::Opaque16(_) => None,
    }
}

fn typed_value_i64_for_field(field_type: FieldType, value: &AuraTypedValue) -> Result<i64> {
    let value = match value {
        AuraTypedValue::I64(value) => *value,
        AuraTypedValue::I128(value) => {
            i64::try_from(*value).map_err(|_| AuraError::InvalidValue("typed value"))?
        }
        AuraTypedValue::Opaque16(_) => return Err(AuraError::InvalidValue("typed value")),
    };
    validate_i64_field_range(field_type, value)?;
    Ok(value)
}

fn validate_i64_field_range(field_type: FieldType, value: i64) -> Result<()> {
    let valid = match field_type {
        FieldType::I8 => (i8::MIN as i64..=i8::MAX as i64).contains(&value),
        FieldType::U8 => (0..=u8::MAX as i64).contains(&value),
        FieldType::I16 => (i16::MIN as i64..=i16::MAX as i64).contains(&value),
        FieldType::U16 => (0..=u16::MAX as i64).contains(&value),
        FieldType::I32 => (i32::MIN as i64..=i32::MAX as i64).contains(&value),
        FieldType::U32 => (0..=u32::MAX as i64).contains(&value),
        FieldType::U64 => value >= 0,
        FieldType::TimestampNs | FieldType::I64 => true,
        FieldType::I128 | FieldType::Opaque16 => false,
        FieldType::TimestampMs | FieldType::Utf8 | FieldType::DecimalText => false,
    };
    if valid {
        Ok(())
    } else {
        Err(AuraError::InvalidValue("typed value"))
    }
}

fn timestamp_field_index(schema: &SchemaDescriptor) -> Option<usize> {
    schema
        .fields
        .iter()
        .find(|field| field.role == FieldRole::Timestamp)
        .map(|field| usize::from(field.index))
}

fn observe_timestamp_runs(stats: &mut IngestStats, rows: &[Vec<i64>], timestamp_index: usize) {
    let mut previous_ts = None;
    let mut run_len = 0u32;
    for row in rows {
        let ts = row.get(timestamp_index).copied();
        if ts == previous_ts {
            run_len += 1;
        } else {
            stats.observe_timestamp_run(run_len);
            previous_ts = ts;
            run_len = 1;
        }
    }
    stats.observe_timestamp_run(run_len);
}

fn observe_typed_timestamp_runs(
    stats: &mut IngestStats,
    rows: &[Vec<AuraTypedValue>],
    timestamp_index: usize,
) {
    let mut previous_ts = None;
    let mut run_len = 0u32;
    for row in rows {
        let ts = row.get(timestamp_index).and_then(typed_value_as_i64);
        if ts == previous_ts {
            run_len += 1;
        } else {
            stats.observe_timestamp_run(run_len);
            previous_ts = ts;
            run_len = 1;
        }
    }
    stats.observe_timestamp_run(run_len);
}

fn schema_has_wide_fields(schema: &SchemaDescriptor) -> bool {
    schema.fields.iter().any(|field| {
        matches!(
            field.field_type,
            FieldType::I128
                | FieldType::Opaque16
                | FieldType::TimestampMs
                | FieldType::Utf8
                | FieldType::DecimalText
        )
    })
}

fn schema_has_wide_fields_from_sealed_file(bytes: &[u8]) -> Result<bool> {
    let profile = sealed_profile(bytes)?;
    let offsets = parse_sealed_file_offsets(bytes, profile)?;
    let schema = match profile {
        Profile::Ingest => {
            let footer =
                AuraFooter::decode(&bytes[offsets.footer_start..offsets.footer_len_offset])?;
            validate_header_footer_agreement(
                &offsets.header,
                footer.container_version,
                &footer.schema,
            )?;
            footer.schema
        }
        Profile::Aura0 | Profile::Aura1 => {
            let footer =
                CompiledFooter::decode(&bytes[offsets.footer_start..offsets.footer_len_offset])?;
            validate_header_footer_agreement(
                &offsets.header,
                footer.container_version,
                &footer.schema,
            )?;
            footer.schema
        }
    };
    Ok(schema_has_wide_fields(&schema))
}

fn validate_flat_opaque16_schema(schema: &SchemaDescriptor) -> Result<()> {
    if schema
        .fields
        .iter()
        .any(|field| field.scope != FieldScope::Event)
    {
        return Err(AuraError::InvalidValue("typed compiled repeated field"));
    }
    if schema.fields.iter().any(|field| field.nullable) {
        return Err(AuraError::InvalidValue("typed compiled nullable field"));
    }
    if schema
        .fields
        .iter()
        .any(|field| field.field_type == FieldType::I128)
    {
        return Err(AuraError::InvalidValue("typed i128 compiled"));
    }
    let opaque_slots = schema
        .fields
        .iter()
        .filter(|field| field.field_type == FieldType::Opaque16)
        .map(|field| field.index)
        .collect::<Vec<_>>();
    if opaque_slots.is_empty() {
        return Err(AuraError::InvalidValue("typed opaque16 schema"));
    }
    if schema.derived_expressions.iter().any(|expression| {
        opaque_slots.contains(&expression.output_slot)
            || expression
                .input_slots
                .iter()
                .any(|slot| opaque_slots.contains(slot))
    }) {
        return Err(AuraError::InvalidValue("opaque16 derived expression"));
    }
    Ok(())
}

fn absolute_typed_field_plans(
    schema: &SchemaDescriptor,
    record_count: usize,
) -> Result<Vec<PhysicalFieldPlan>> {
    schema
        .fields
        .iter()
        .map(|field| {
            let width = match field.field_type {
                FieldType::I8 => PhysicalWidth::I8,
                FieldType::U8 | FieldType::I16 => PhysicalWidth::I16,
                FieldType::U16 | FieldType::I32 => PhysicalWidth::I32,
                FieldType::U32 => PhysicalWidth::I64,
                FieldType::I64 | FieldType::U64 | FieldType::TimestampNs => PhysicalWidth::I64,
                FieldType::I128 | FieldType::Opaque16 => PhysicalWidth::I128,
                FieldType::TimestampMs | FieldType::Utf8 | FieldType::DecimalText => {
                    return Err(AuraError::InvalidValue("v3-only field type"));
                }
            };
            Ok(PhysicalFieldPlan {
                field_index: field.index,
                encoding: FieldEncoding::Absolute,
                width,
                bit_width: 0,
                reference_field_index: None,
                base_value: 0,
                step: 0,
                estimated_bytes: u64::try_from(record_count)
                    .map_err(|_| AuraError::InvalidValue("record count"))?
                    .saturating_mul(u64::from(width.byte_width())),
            })
        })
        .collect()
}

fn put_u16_len(out: &mut Vec<u8>, len: usize, name: &'static str) -> Result<()> {
    let len = u16::try_from(len).map_err(|_| AuraError::InvalidValue(name))?;
    put_u16_le(out, len);
    Ok(())
}

#[cfg(test)]
mod byte_lane_tests {
    use super::*;

    fn v3_agreement_schema() -> SchemaDescriptor {
        crate::schema::SchemaBuilder::new("v3-header-schema-agreement")
            .v3()
            .field("ts", FieldType::TimestampNs, FieldRole::Timestamp)
            .repeated_field("side", FieldType::U8, FieldRole::Side)
            .repeated_field("value", FieldType::I64, FieldRole::Value)
            .dual_domain_repeated_group(
                7,
                vec![1, 2],
                1,
                crate::schema::RelationshipPermissions::none()
                    .with_split()
                    .with_within_domain(),
            )
            .finish()
            .unwrap()
    }

    fn v3_agreement_header(schema: &SchemaDescriptor) -> AuraHeader {
        AuraHeader::new(Profile::Ingest)
            .with_container_version(AuraContainerVersion::V3)
            .with_schema_mapping(schema_parent_mapping(schema).unwrap())
            .unwrap()
            .with_groups(schema.groups.clone())
            .unwrap()
    }

    fn bounded_raw_lane(
        output_offset: u64,
        output_len: u64,
        compressed_offset: u64,
        compressed_len: u64,
    ) -> Aura1ByteLaneDescriptor {
        Aura1ByteLaneDescriptor {
            lane_version: AURA1_BYTE_LANE_VERSION,
            codec_id: BYTE_LANE_CODEC_RAW,
            codec_level: 0,
            block_index: 0,
            row_start: 0,
            row_count: 1,
            aura1_output_offset: output_offset,
            uncompressed_len: output_len,
            compressed_offset,
            compressed_len,
            checksum_kind: BYTE_LANE_CHECKSUM_NONE,
            checksum: 0,
            flags: 0,
        }
    }

    #[test]
    fn header_footer_container_versions_must_agree() {
        let v2_schema =
            crate::schema::generic_i64_parent_schema("version-agreement", &[100, 0]).unwrap();
        let header = AuraHeader::new(Profile::Ingest)
            .with_container_version(AuraContainerVersion::V3)
            .with_schema_mapping(schema_parent_mapping(&v2_schema).unwrap())
            .unwrap();

        assert_eq!(
            validate_header_footer_agreement(&header, AuraContainerVersion::V2, &v2_schema),
            Err(AuraError::InvalidValue("header footer container version"))
        );
        assert_eq!(
            validate_header_footer_agreement(&header, AuraContainerVersion::V3, &v2_schema),
            Err(AuraError::InvalidValue("schema container version"))
        );

        let v3_schema = v3_agreement_schema();
        let v3_header = v3_agreement_header(&v3_schema);
        assert_eq!(
            validate_header_footer_agreement(&v3_header, AuraContainerVersion::V3, &v3_schema),
            Ok(())
        );
    }

    #[test]
    fn v3_header_schema_group_declarations_must_match_exactly() {
        let schema = v3_agreement_schema();
        let header = v3_agreement_header(&schema);

        let mut changed_relationships = header.clone();
        changed_relationships.groups[0].relationships =
            crate::schema::RelationshipPermissions::none().with_split();
        assert_eq!(
            validate_header_schema_agreement(&changed_relationships, &schema),
            Err(AuraError::InvalidValue("header schema groups"))
        );

        let mut changed_discriminator = header.clone();
        changed_discriminator.groups[0]
            .dual_domain
            .as_mut()
            .unwrap()
            .discriminator_slot = 2;
        assert_eq!(
            validate_header_schema_agreement(&changed_discriminator, &schema),
            Err(AuraError::InvalidValue("header schema groups"))
        );

        let mut changed_children = header.clone();
        changed_children.groups[0].child_slots = vec![1];
        assert_eq!(
            validate_header_schema_agreement(&changed_children, &schema),
            Err(AuraError::InvalidValue("header schema groups"))
        );

        let mut changed_order = header.clone();
        changed_order.groups[0].child_slots.reverse();
        assert_eq!(
            validate_header_schema_agreement(&changed_order, &schema),
            Err(AuraError::InvalidValue("header schema groups"))
        );

        let mut missing_group = header;
        missing_group.groups.clear();
        assert_eq!(
            validate_header_schema_agreement(&missing_group, &schema),
            Err(AuraError::InvalidValue("header schema groups"))
        );
    }

    #[test]
    fn header_schema_agreement_rejects_schema_dialect_mismatches_and_v2_groups() {
        let v2_schema =
            crate::schema::generic_i64_parent_schema("v2-schema-agreement", &[100, 0]).unwrap();
        let v3_schema = v3_agreement_schema();
        let v3_header = v3_agreement_header(&v3_schema);
        assert_eq!(
            validate_header_schema_agreement(&v3_header, &v2_schema),
            Err(AuraError::InvalidValue("schema container version"))
        );

        let mut v2_header = AuraHeader::new(Profile::Ingest)
            .with_schema_mapping(schema_parent_mapping(&v2_schema).unwrap())
            .unwrap();
        assert_eq!(
            validate_header_schema_agreement(&v2_header, &v3_schema),
            Err(AuraError::InvalidValue("schema container version"))
        );

        v2_header.groups = v3_schema.groups;
        assert_eq!(
            validate_header_schema_agreement(&v2_header, &v2_schema),
            Err(AuraError::InvalidValue("header schema groups"))
        );
    }

    #[test]
    fn conversion_policy_cannot_silently_downgrade_future_versions() {
        assert_eq!(
            conversion_output_container_version(AuraContainerVersion::V2),
            Ok(AuraContainerVersion::V2)
        );
        assert_eq!(
            conversion_output_container_version(AuraContainerVersion::LegacyV1),
            Err(AuraError::UnsupportedVersion(1))
        );
        assert_eq!(
            conversion_output_container_version(AuraContainerVersion::V3),
            Err(AuraError::UnsupportedVersion(3))
        );
    }

    #[test]
    fn byte_lane_footer_prefix_validation_is_bounded_and_versioned() {
        assert_eq!(
            validate_compiled_footer_version_prefix(b"AURP\x02", AuraContainerVersion::V2),
            Err(AuraError::UnexpectedEof)
        );
        assert_eq!(
            validate_compiled_footer_version_prefix(b"NOPE\x02\x00", AuraContainerVersion::V2),
            Err(AuraError::InvalidMagic { expected: "AURP" })
        );
        assert_eq!(
            validate_compiled_footer_version_prefix(b"AURP\x02\x00", AuraContainerVersion::V3),
            Err(AuraError::InvalidValue("header footer container version"))
        );
        assert_eq!(
            validate_compiled_footer_version_prefix(b"AURP\x03\x00", AuraContainerVersion::V2),
            Err(AuraError::UnsupportedVersion(3))
        );
    }

    #[test]
    fn byte_lane_output_and_range_limits_reject_allocation_bombs() {
        let u64_output_bomb = bounded_raw_lane(0, u64::MAX, 0, 0);
        assert_eq!(
            decode_aura1_byte_lanes_from_descriptors(&[], &[u64_output_bomb], false),
            Err(AuraError::InvalidValue("byte lane output length"))
        );

        let per_lane_output_bomb = bounded_raw_lane(
            0,
            crate::program::MAX_AURA1_BYTE_LANE_OUTPUT_BYTES + 1,
            0,
            0,
        );
        assert_eq!(
            decode_aura1_byte_lanes_from_descriptors(&[], &[per_lane_output_bomb], false),
            Err(AuraError::InvalidValue("byte lane output length"))
        );

        let compressed_length_bomb = bounded_raw_lane(
            0,
            0,
            0,
            crate::program::MAX_AURA1_BYTE_LANE_COMPRESSED_BYTES + 1,
        );
        assert_eq!(
            decode_aura1_byte_lanes_from_descriptors(&[], &[compressed_length_bomb], false),
            Err(AuraError::InvalidValue("byte lane compressed length"))
        );

        let output_offset_overflow = bounded_raw_lane(u64::MAX, 1, 0, 0);
        assert_eq!(
            decode_aura1_byte_lanes_from_descriptors(&[], &[output_offset_overflow], false),
            Err(AuraError::InvalidValue("byte lane output range"))
        );

        let compressed_offset_overflow = bounded_raw_lane(0, 0, u64::MAX, 1);
        assert_eq!(
            decode_aura1_byte_lanes_from_descriptors(&[], &[compressed_offset_overflow], false),
            Err(AuraError::InvalidValue("byte lane compressed range"))
        );

        let half_plus_one = crate::program::MAX_AURA1_BYTE_LANES_TOTAL_OUTPUT_BYTES / 2 + 1;
        let oversized_total = [
            bounded_raw_lane(0, half_plus_one, 0, 0),
            bounded_raw_lane(0, half_plus_one, 0, 0),
        ];
        assert_eq!(
            decode_aura1_byte_lanes_from_descriptors(&[], &oversized_total, false),
            Err(AuraError::InvalidValue("byte lane total output length"))
        );
    }

    #[test]
    fn byte_lane_encoder_preflights_source_length_before_codec_work() {
        let supported = usize::try_from(MAX_AURA1_BYTE_LANE_OUTPUT_BYTES).unwrap();
        assert_eq!(
            validate_aura1_byte_lane_source_len(supported),
            Ok(MAX_AURA1_BYTE_LANE_OUTPUT_BYTES)
        );
        assert_eq!(
            validate_aura1_byte_lane_source_len(supported + 1),
            Err(AuraError::InvalidValue("byte lane output length"))
        );
        assert_eq!(
            encode_aura1_byte_lane_file(Aura0ByteLaneCodec::Raw, b"x", u64::MAX, 1),
            Err(AuraError::InvalidValue("byte lane compressed range"))
        );
    }

    #[test]
    fn typed_uuid_decoder_rejects_uuid_stream_targeting_numeric_slot() {
        let schema = crate::schema::SchemaBuilder::new("malformed_uuid_target")
            .field("price", FieldType::I64, FieldRole::Price)
            .field("exec_id", FieldType::Opaque16, FieldRole::Identifier)
            .finish()
            .unwrap();
        let uuid = plan_uuid_const_mask_stream(0, Some(0), &[1]).unwrap();
        let opaque_placeholder = crate::instructions::GenericStreamInstruction {
            stream_id: 1,
            target_slot: Some(1),
            op: GenericStreamOp::FixedStep { base: 0, step: 0 },
        };
        let encoded = GenericEncodedI64Rows {
            plan: GenericInstructionPlan {
                streams: vec![uuid.clone(), opaque_placeholder.clone()],
                groups: Vec::new(),
            },
            streams: vec![
                GenericEncodedStream {
                    stream_id: 0,
                    value_count: 1,
                    body: encode_generic_stream_body(&uuid, &GenericStreamBodyValue::U128(vec![1]))
                        .unwrap(),
                },
                GenericEncodedStream {
                    stream_id: 1,
                    value_count: 1,
                    body: encode_generic_stream_body(
                        &opaque_placeholder,
                        &GenericStreamBodyValue::I64(vec![0]),
                    )
                    .unwrap(),
                },
            ],
            record_count: 1,
            field_count: 2,
        };

        assert!(decode_typed_generic_rows(&schema, encoded).is_err());
    }

    #[test]
    fn mixed_byte_lanes_decode_into_exact_output_ranges() {
        let left = b"left-";
        let right = b"right";
        let left_compressed = lz4_flex::compress_prepend_size(left);
        let right_compressed = zstd::bulk::compress(right, 3).unwrap();
        let mut body = left_compressed.clone();
        body.extend_from_slice(&right_compressed);

        let lanes = [
            Aura1ByteLaneDescriptor {
                lane_version: AURA1_BYTE_LANE_VERSION,
                codec_id: BYTE_LANE_CODEC_LZ4,
                codec_level: 0,
                block_index: 0,
                row_start: 0,
                row_count: 1,
                aura1_output_offset: 0,
                uncompressed_len: left.len() as u64,
                compressed_offset: 0,
                compressed_len: left_compressed.len() as u64,
                checksum_kind: BYTE_LANE_CHECKSUM_BYTE_GUARD,
                checksum: bytes_guard_value(left),
                flags: 0,
            },
            Aura1ByteLaneDescriptor {
                lane_version: AURA1_BYTE_LANE_VERSION,
                codec_id: BYTE_LANE_CODEC_ZSTD,
                codec_level: 3,
                block_index: 1,
                row_start: 1,
                row_count: 1,
                aura1_output_offset: left.len() as u64,
                uncompressed_len: right.len() as u64,
                compressed_offset: left_compressed.len() as u64,
                compressed_len: right_compressed.len() as u64,
                checksum_kind: BYTE_LANE_CHECKSUM_BYTE_GUARD,
                checksum: bytes_guard_value(right),
                flags: 0,
            },
        ];

        let decoded = decode_aura1_byte_lanes_from_descriptors(&body, &lanes, true).unwrap();
        assert_eq!(b"left-right", decoded.as_slice());
    }
}
