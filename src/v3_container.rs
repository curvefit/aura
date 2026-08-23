//! Decode-first Aura0 V3 flat container support.
//!
//! This module intentionally exposes no complete-file writer. The footer
//! codec is pure so fixtures and future writers can share one canonical wire
//! implementation, while complete files are decoded and verified in memory.

use sha2::{Digest, Sha256};

use crate::format::{AuraContainerVersion, SEAL_MAGIC};
use crate::program::CompiledFooter;
use crate::schema::{
    decode_schema_descriptor, encode_schema_descriptor, FieldRole, FieldScope, FieldType,
    SchemaDescriptor, SchemaEncodingVersion,
};
use crate::v3_values::{
    canonical_v3_batch_sha256, canonical_v3_schema_fingerprint, decode_v3_value_block, AuraV3Batch,
    AuraV3ColumnValues, CanonicalV3RowHasher, V3ValueLimits, MAX_V3_VALUE_BLOCK_BYTES,
};
use crate::{AuraError, AuraHeader, Profile, Result};

pub const V3_FLAT_FOOTER_MAGIC: &[u8; 4] = b"AURP";
pub const V3_FLAT_FOOTER_LAYOUT_VERSION: u16 = 1;
pub const V3_FLAT_BODY_ENCODING_EXACT_BLOCKS: u8 = 1;
pub const V3_FLAT_FOOTER_PREFIX_BYTES: usize = 176;
pub const V3_FLAT_STATS_DESCRIPTOR_BYTES: usize = 36;
pub const V3_FLAT_CHUNK_DESCRIPTOR_BYTES: usize = 136;
pub const MAX_V3_FLAT_FOOTER_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_V3_FLAT_BODY_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
pub const MAX_V3_FLAT_CHUNKS: usize = 65_536;
pub const MAX_V3_FLAT_ROWS: u64 = 16_777_216;
pub const MAX_V3_FLAT_SCHEMA_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_V3_FLAT_IN_MEMORY_BODY_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_V3_FLAT_IN_MEMORY_ROWS: u64 = 4_194_304;
pub const DEFAULT_V3_FLAT_IN_MEMORY_CHUNKS: usize = 4_096;
pub const DEFAULT_V3_FLAT_VALUE_BLOCK_BYTES: usize = 256 * 1024 * 1024;

const HEADER_HASH_DOMAIN: &[u8] = b"aura-v3-flat-aura0-header-v1\0";
const BODY_HASH_DOMAIN: &[u8] = b"aura-v3-flat-aura0-body-v1\0";
const FOOTER_HASH_DOMAIN: &[u8] = b"aura-v3-flat-aura0-footer-v1\0";
const STATS_TABLE_VERSION: u16 = 1;
const CHUNK_TABLE_VERSION: u16 = 1;
const STAT_NULLABLE: u8 = 1;
const STAT_VARIABLE: u8 = 2;
const STAT_NUMERIC: u8 = 4;
const STAT_KNOWN_FLAGS: u8 = STAT_NULLABLE | STAT_VARIABLE | STAT_NUMERIC;
const CHUNK_HAS_TIMESTAMP: u32 = 1;
const CHUNK_HAS_SEQUENCE: u32 = 2;
const CHUNK_KNOWN_FLAGS: u32 = CHUNK_HAS_TIMESTAMP | CHUNK_HAS_SEQUENCE;
const NO_SLOT: u16 = u16::MAX;
const TRAILER_BYTES: usize = 4 + 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V3FlatLimits {
    pub max_footer_bytes: usize,
    pub max_body_bytes: u64,
    pub max_chunks: usize,
    pub max_rows: u64,
    pub value_limits: V3ValueLimits,
}

impl V3FlatLimits {
    pub const HARD: Self = Self {
        max_footer_bytes: MAX_V3_FLAT_FOOTER_BYTES,
        max_body_bytes: MAX_V3_FLAT_BODY_BYTES,
        max_chunks: MAX_V3_FLAT_CHUNKS,
        max_rows: MAX_V3_FLAT_ROWS,
        value_limits: V3ValueLimits::HARD,
    };

    pub const DEFAULT_IN_MEMORY: Self = Self {
        max_footer_bytes: MAX_V3_FLAT_FOOTER_BYTES,
        max_body_bytes: DEFAULT_V3_FLAT_IN_MEMORY_BODY_BYTES,
        max_chunks: DEFAULT_V3_FLAT_IN_MEMORY_CHUNKS,
        max_rows: DEFAULT_V3_FLAT_IN_MEMORY_ROWS,
        value_limits: V3ValueLimits {
            max_block_bytes: DEFAULT_V3_FLAT_VALUE_BLOCK_BYTES,
            max_variable_value_bytes: crate::v3_values::MAX_V3_VARIABLE_VALUE_BYTES,
            max_rows: DEFAULT_V3_FLAT_IN_MEMORY_ROWS as usize,
        },
    };

    const fn effective(self) -> Self {
        let max_value_rows = self.value_limits.max_rows as u64;
        Self {
            max_footer_bytes: min_usize(self.max_footer_bytes, MAX_V3_FLAT_FOOTER_BYTES),
            max_body_bytes: min_u64(self.max_body_bytes, MAX_V3_FLAT_BODY_BYTES),
            max_chunks: min_usize(self.max_chunks, MAX_V3_FLAT_CHUNKS),
            max_rows: min_u64(min_u64(self.max_rows, MAX_V3_FLAT_ROWS), max_value_rows),
            value_limits: self.value_limits,
        }
    }
}

impl Default for V3FlatLimits {
    fn default() -> Self {
        Self::DEFAULT_IN_MEMORY
    }
}

const fn min_usize(left: usize, right: usize) -> usize {
    if left < right {
        left
    } else {
        right
    }
}

const fn min_u64(left: u64, right: u64) -> u64 {
    if left < right {
        left
    } else {
        right
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3FlatColumnStats {
    pub slot: u16,
    pub field_type: FieldType,
    pub flags: u8,
    pub present_count: u64,
    pub null_count: u64,
    pub logical_payload_bytes: u64,
    pub max_present_value_byte_len: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3FlatChunkDescriptor {
    pub chunk_id: u32,
    pub flags: u32,
    pub first_global_row: u64,
    pub row_count: u32,
    pub body_relative_offset: u64,
    pub stored_len: u64,
    pub stored_sha256: [u8; 32],
    pub chunk_logical_sha256: [u8; 32],
    pub first_timestamp: i64,
    pub last_timestamp: i64,
    pub first_sequence: u64,
    pub last_sequence: u64,
}

impl V3FlatChunkDescriptor {
    pub const fn has_timestamp_bounds(&self) -> bool {
        self.flags & CHUNK_HAS_TIMESTAMP != 0
    }

    pub const fn has_sequence_bounds(&self) -> bool {
        self.flags & CHUNK_HAS_SEQUENCE != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3FlatFooter {
    pub record_count: u64,
    pub body_len: u64,
    pub schema: SchemaDescriptor,
    pub primary_timestamp_slot: u16,
    pub primary_sequence_slot: Option<u16>,
    pub schema_fingerprint: [u8; 32],
    pub header_sha256: [u8; 32],
    pub body_sha256: [u8; 32],
    pub global_logical_sha256: [u8; 32],
    pub stats: Vec<V3FlatColumnStats>,
    pub chunks: Vec<V3FlatChunkDescriptor>,
}

pub type V3Aura0Footer = V3FlatFooter;
pub type V3Aura0ChunkDescriptor = V3FlatChunkDescriptor;
pub type V3Aura0ColumnStats = V3FlatColumnStats;
pub type DecodedV3Aura0File = DecodedV3FlatAura0;

/// Version-aware AURP footer routing. Existing [`CompiledFooter`] V2 decode
/// semantics remain untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnyCompiledFooter {
    V2(CompiledFooter),
    V3Flat(V3FlatFooter),
}

impl AnyCompiledFooter {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 6 {
            return Err(AuraError::UnexpectedEof);
        }
        if &bytes[..4] != V3_FLAT_FOOTER_MAGIC {
            return Err(AuraError::InvalidMagic { expected: "AURP" });
        }
        match AuraContainerVersion::from_wire(u16::from_le_bytes([bytes[4], bytes[5]]))? {
            AuraContainerVersion::V2 => CompiledFooter::decode(bytes).map(Self::V2),
            AuraContainerVersion::V3 => decode_v3_flat_footer(bytes).map(Self::V3Flat),
            AuraContainerVersion::LegacyV1 => Err(AuraError::UnsupportedVersion(1)),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            Self::V2(footer) => footer.encode(),
            Self::V3Flat(footer) => encode_v3_flat_footer(footer),
        }
    }
}

pub fn decode_any_compiled_footer(bytes: &[u8]) -> Result<AnyCompiledFooter> {
    AnyCompiledFooter::decode(bytes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedV3FlatAura0 {
    pub header: AuraHeader,
    pub footer: V3FlatFooter,
    pub batches: Vec<AuraV3Batch>,
}

pub fn v3_flat_header_sha256(header: &[u8]) -> Result<[u8; 32]> {
    domain_hash(HEADER_HASH_DOMAIN, header, "v3 header length")
}

pub fn v3_flat_body_sha256(body: &[u8]) -> Result<[u8; 32]> {
    domain_hash(BODY_HASH_DOMAIN, body, "v3 body length")
}

pub fn encode_v3_flat_footer(footer: &V3FlatFooter) -> Result<Vec<u8>> {
    validate_footer_metadata(footer, V3FlatLimits::HARD)?;
    let schema_bytes = encode_schema_descriptor(&footer.schema)?;
    if schema_bytes.len() > MAX_V3_FLAT_SCHEMA_BYTES {
        return Err(AuraError::InvalidValue("v3 flat schema length"));
    }
    let stats_bytes = footer
        .stats
        .len()
        .checked_mul(V3_FLAT_STATS_DESCRIPTOR_BYTES)
        .ok_or(AuraError::InvalidValue("v3 flat footer length"))?;
    let chunks_bytes = footer
        .chunks
        .len()
        .checked_mul(V3_FLAT_CHUNK_DESCRIPTOR_BYTES)
        .ok_or(AuraError::InvalidValue("v3 flat footer length"))?;
    let total_len = V3_FLAT_FOOTER_PREFIX_BYTES
        .checked_add(schema_bytes.len())
        .and_then(|len| len.checked_add(8))
        .and_then(|len| len.checked_add(stats_bytes))
        .and_then(|len| len.checked_add(8))
        .and_then(|len| len.checked_add(chunks_bytes))
        .and_then(|len| len.checked_add(32))
        .filter(|len| *len <= MAX_V3_FLAT_FOOTER_BYTES)
        .ok_or(AuraError::InvalidValue("v3 flat footer length"))?;
    let mut out = Vec::new();
    out.try_reserve_exact(total_len)
        .map_err(|_| AuraError::InvalidValue("v3 flat footer allocation"))?;
    out.extend_from_slice(V3_FLAT_FOOTER_MAGIC);
    put_u16(&mut out, 3);
    put_u16(&mut out, V3_FLAT_FOOTER_LAYOUT_VERSION);
    out.extend_from_slice(&[V3_FLAT_BODY_ENCODING_EXACT_BLOCKS, 0, 0, 0]);
    put_u64(&mut out, footer.record_count);
    put_u64(&mut out, footer.body_len);
    put_u32_len(&mut out, footer.stats.len(), "v3 flat column count")?;
    put_u32_len(&mut out, footer.chunks.len(), "v3 flat chunk count")?;
    put_u32(&mut out, footer.schema.schema_id);
    put_u16(&mut out, footer.primary_timestamp_slot);
    put_u16(&mut out, footer.primary_sequence_slot.unwrap_or(NO_SLOT));
    put_u32(&mut out, 0);
    out.extend_from_slice(&footer.schema_fingerprint);
    out.extend_from_slice(&footer.header_sha256);
    out.extend_from_slice(&footer.body_sha256);
    out.extend_from_slice(&footer.global_logical_sha256);
    debug_assert_eq!(out.len(), V3_FLAT_FOOTER_PREFIX_BYTES);
    out.extend_from_slice(&schema_bytes);
    put_u16(&mut out, STATS_TABLE_VERSION);
    put_u16(&mut out, V3_FLAT_STATS_DESCRIPTOR_BYTES as u16);
    put_u32_len(&mut out, footer.stats.len(), "v3 flat stats count")?;
    for stat in &footer.stats {
        put_u16(&mut out, stat.slot);
        out.push(stat.field_type as u8);
        out.push(stat.flags);
        put_u64(&mut out, stat.present_count);
        put_u64(&mut out, stat.null_count);
        put_u64(&mut out, stat.logical_payload_bytes);
        put_u32(&mut out, stat.max_present_value_byte_len);
        put_u32(&mut out, 0);
    }
    put_u16(&mut out, CHUNK_TABLE_VERSION);
    put_u16(&mut out, V3_FLAT_CHUNK_DESCRIPTOR_BYTES as u16);
    put_u32_len(&mut out, footer.chunks.len(), "v3 flat chunk count")?;
    for chunk in &footer.chunks {
        put_u32(&mut out, chunk.chunk_id);
        put_u32(&mut out, chunk.flags);
        put_u64(&mut out, chunk.first_global_row);
        put_u32(&mut out, chunk.row_count);
        put_u16(&mut out, 1);
        put_u16(&mut out, 0);
        put_u64(&mut out, chunk.body_relative_offset);
        put_u64(&mut out, chunk.stored_len);
        out.extend_from_slice(&chunk.stored_sha256);
        out.extend_from_slice(&chunk.chunk_logical_sha256);
        put_i64(&mut out, chunk.first_timestamp);
        put_i64(&mut out, chunk.last_timestamp);
        put_u64(&mut out, chunk.first_sequence);
        put_u64(&mut out, chunk.last_sequence);
    }
    let hash = footer_self_hash(&out)?;
    out.extend_from_slice(&hash);
    debug_assert_eq!(out.len(), total_len);
    Ok(out)
}

pub fn decode_v3_flat_footer(bytes: &[u8]) -> Result<V3FlatFooter> {
    decode_v3_flat_footer_with_limits(bytes, V3FlatLimits::HARD)
}

pub fn decode_v3_flat_footer_with_limits(
    bytes: &[u8],
    limits: V3FlatLimits,
) -> Result<V3FlatFooter> {
    let limits = limits.effective();
    if bytes.len() > limits.max_footer_bytes {
        return Err(AuraError::InvalidValue("v3 flat footer length"));
    }
    if bytes.len() < V3_FLAT_FOOTER_PREFIX_BYTES + 4 + 8 + 8 + 32 {
        return Err(AuraError::UnexpectedEof);
    }
    let hash_start = bytes
        .len()
        .checked_sub(32)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_self_hash(&bytes[..hash_start])? != bytes[hash_start..] {
        return Err(AuraError::InvalidValue("v3 flat footer hash"));
    }
    let mut reader = Reader::new(&bytes[..hash_start]);
    if reader.take(4)? != V3_FLAT_FOOTER_MAGIC {
        return Err(AuraError::InvalidMagic { expected: "AURP" });
    }
    let version = reader.u16()?;
    if version != 3 {
        return Err(AuraError::UnsupportedVersion(version));
    }
    if reader.u16()? != V3_FLAT_FOOTER_LAYOUT_VERSION {
        return Err(AuraError::InvalidValue("v3 flat footer layout"));
    }
    if reader.u8()? != V3_FLAT_BODY_ENCODING_EXACT_BLOCKS {
        return Err(AuraError::InvalidValue("v3 flat body encoding"));
    }
    if reader.u8()? != 0 || reader.u8()? != 0 || reader.u8()? != 0 {
        return Err(AuraError::InvalidValue("v3 flat compression"));
    }
    let record_count = reader.u64()?;
    let body_len = reader.u64()?;
    let column_count = reader.u32()? as usize;
    let chunk_count = reader.u32()? as usize;
    if record_count > limits.max_rows {
        return Err(AuraError::InvalidValue("v3 flat record count"));
    }
    if body_len > limits.max_body_bytes {
        return Err(AuraError::InvalidValue("v3 flat body length"));
    }
    if chunk_count > limits.max_chunks {
        return Err(AuraError::InvalidValue("v3 flat chunk count"));
    }
    let schema_id = reader.u32()?;
    let primary_timestamp_slot = reader.u16()?;
    let sequence_raw = reader.u16()?;
    let primary_sequence_slot = (sequence_raw != NO_SLOT).then_some(sequence_raw);
    if reader.u32()? != 0 {
        return Err(AuraError::InvalidValue("v3 flat footer reserved"));
    }
    let schema_fingerprint = reader.array32()?;
    let header_sha256 = reader.array32()?;
    let body_sha256 = reader.array32()?;
    let global_logical_sha256 = reader.array32()?;
    debug_assert_eq!(reader.offset, V3_FLAT_FOOTER_PREFIX_BYTES);

    let schema_payload_len = reader.peek_u32()? as usize;
    let schema_len = schema_payload_len
        .checked_add(4)
        .filter(|len| *len <= MAX_V3_FLAT_SCHEMA_BYTES)
        .ok_or(AuraError::InvalidValue("v3 flat schema length"))?;
    let schema = decode_schema_descriptor(reader.take(schema_len)?)?;
    if schema.schema_id != schema_id {
        return Err(AuraError::InvalidValue("v3 flat schema id"));
    }
    if canonical_v3_schema_fingerprint(&schema)? != schema_fingerprint {
        return Err(AuraError::InvalidValue("v3 flat schema fingerprint"));
    }
    if column_count != schema.fields.len() {
        return Err(AuraError::InvalidValue("v3 flat column count"));
    }

    if reader.u16()? != STATS_TABLE_VERSION
        || reader.u16()? as usize != V3_FLAT_STATS_DESCRIPTOR_BYTES
    {
        return Err(AuraError::InvalidValue("v3 flat stats table"));
    }
    let stats_count = reader.u32()? as usize;
    if stats_count != column_count
        || stats_count > reader.remaining() / V3_FLAT_STATS_DESCRIPTOR_BYTES
    {
        return Err(AuraError::InvalidValue("v3 flat stats count"));
    }
    let mut stats = Vec::new();
    stats
        .try_reserve_exact(stats_count)
        .map_err(|_| AuraError::InvalidValue("v3 flat footer allocation"))?;
    for _ in 0..stats_count {
        let slot = reader.u16()?;
        let field_type = FieldType::from_code(reader.u8()?)?;
        let flags = reader.u8()?;
        let present_count = reader.u64()?;
        let null_count = reader.u64()?;
        let logical_payload_bytes = reader.u64()?;
        let max_present_value_byte_len = reader.u32()?;
        if reader.u32()? != 0 {
            return Err(AuraError::InvalidValue("v3 flat stats reserved"));
        }
        stats.push(V3FlatColumnStats {
            slot,
            field_type,
            flags,
            present_count,
            null_count,
            logical_payload_bytes,
            max_present_value_byte_len,
        });
    }

    if reader.u16()? != CHUNK_TABLE_VERSION
        || reader.u16()? as usize != V3_FLAT_CHUNK_DESCRIPTOR_BYTES
    {
        return Err(AuraError::InvalidValue("v3 flat chunk table"));
    }
    let table_chunk_count = reader.u32()? as usize;
    if table_chunk_count != chunk_count
        || table_chunk_count > reader.remaining() / V3_FLAT_CHUNK_DESCRIPTOR_BYTES
    {
        return Err(AuraError::InvalidValue("v3 flat chunk count"));
    }
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(table_chunk_count)
        .map_err(|_| AuraError::InvalidValue("v3 flat footer allocation"))?;
    for _ in 0..table_chunk_count {
        let chunk_id = reader.u32()?;
        let flags = reader.u32()?;
        let first_global_row = reader.u64()?;
        let row_count = reader.u32()?;
        if reader.u16()? != 1 {
            return Err(AuraError::InvalidValue("v3 exact block version"));
        }
        if reader.u16()? != 0 {
            return Err(AuraError::InvalidValue("v3 flat chunk reserved"));
        }
        chunks.push(V3FlatChunkDescriptor {
            chunk_id,
            flags,
            first_global_row,
            row_count,
            body_relative_offset: reader.u64()?,
            stored_len: reader.u64()?,
            stored_sha256: reader.array32()?,
            chunk_logical_sha256: reader.array32()?,
            first_timestamp: reader.i64()?,
            last_timestamp: reader.i64()?,
            first_sequence: reader.u64()?,
            last_sequence: reader.u64()?,
        });
    }
    reader.finish()?;
    let footer = V3FlatFooter {
        record_count,
        body_len,
        schema,
        primary_timestamp_slot,
        primary_sequence_slot,
        schema_fingerprint,
        header_sha256,
        body_sha256,
        global_logical_sha256,
        stats,
        chunks,
    };
    validate_footer_metadata(&footer, limits)?;
    Ok(footer)
}

pub fn decode_v3_flat_aura0(bytes: &[u8]) -> Result<DecodedV3FlatAura0> {
    decode_v3_flat_aura0_with_limits(bytes, V3FlatLimits::default())
}

pub fn decode_v3_flat_aura0_with_limits(
    bytes: &[u8],
    limits: V3FlatLimits,
) -> Result<DecodedV3FlatAura0> {
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
    if footer_len > limits.max_footer_bytes {
        return Err(AuraError::InvalidValue("v3 flat footer length"));
    }
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::InvalidValue("v3 flat footer length"))?;
    let header_len = AuraHeader::encoded_len(bytes)?;
    if header_len > footer_start {
        return Err(AuraError::InvalidValue("v3 flat file ranges"));
    }
    let header_bytes = &bytes[..header_len];
    let body = &bytes[header_len..footer_start];
    if body.len() as u64 > limits.max_body_bytes {
        return Err(AuraError::InvalidValue("v3 flat body length"));
    }
    let header = AuraHeader::decode(header_bytes)?;
    let footer =
        decode_v3_flat_footer_with_limits(&bytes[footer_start..footer_len_offset], limits)?;
    validate_header_footer(&header, &footer)?;
    if footer.body_len != body.len() as u64 {
        return Err(AuraError::InvalidValue("v3 flat body length"));
    }
    if v3_flat_header_sha256(header_bytes)? != footer.header_sha256 {
        return Err(AuraError::InvalidValue("v3 flat header hash"));
    }
    if v3_flat_body_sha256(body)? != footer.body_sha256 {
        return Err(AuraError::InvalidValue("v3 flat body hash"));
    }

    let total_rows = u32::try_from(footer.record_count)
        .map_err(|_| AuraError::InvalidValue("v3 flat record count"))?;
    let mut global_hasher =
        CanonicalV3RowHasher::new(&footer.schema, total_rows, limits.value_limits)?;
    let mut batches = Vec::new();
    batches
        .try_reserve_exact(footer.chunks.len())
        .map_err(|_| AuraError::InvalidValue("v3 flat batch allocation"))?;
    let mut actual_stats = zero_stats(&footer.schema);
    for chunk in &footer.chunks {
        let start = usize::try_from(chunk.body_relative_offset)
            .map_err(|_| AuraError::InvalidValue("v3 flat chunk range"))?;
        let stored_len = usize::try_from(chunk.stored_len)
            .map_err(|_| AuraError::InvalidValue("v3 flat chunk range"))?;
        let end = start
            .checked_add(stored_len)
            .ok_or(AuraError::InvalidValue("v3 flat chunk range"))?;
        let stored = body
            .get(start..end)
            .ok_or(AuraError::InvalidValue("v3 flat chunk range"))?;
        if plain_sha256(stored) != chunk.stored_sha256 {
            return Err(AuraError::InvalidValue("v3 flat stored chunk hash"));
        }
        let batch = decode_v3_value_block(&footer.schema, stored, limits.value_limits)?;
        if batch.row_count != chunk.row_count {
            return Err(AuraError::InvalidValue("v3 flat chunk row count"));
        }
        if canonical_v3_batch_sha256(&footer.schema, &batch, limits.value_limits)?
            != chunk.chunk_logical_sha256
        {
            return Err(AuraError::InvalidValue("v3 flat chunk logical hash"));
        }
        validate_chunk_bounds(&footer, chunk, &batch)?;
        accumulate_stats(&mut actual_stats, &batch)?;
        global_hasher.update_batch(&footer.schema, &batch)?;
        batches.push(batch);
    }
    if actual_stats != footer.stats {
        return Err(AuraError::InvalidValue("v3 flat stats"));
    }
    if global_hasher.finalize()? != footer.global_logical_sha256 {
        return Err(AuraError::InvalidValue("v3 flat global logical hash"));
    }
    Ok(DecodedV3FlatAura0 {
        header,
        footer,
        batches,
    })
}

pub fn decode_v3_aura0_file(bytes: &[u8]) -> Result<DecodedV3FlatAura0> {
    decode_v3_flat_aura0(bytes)
}

pub fn encode_v3_aura0_footer(footer: &V3FlatFooter) -> Result<Vec<u8>> {
    encode_v3_flat_footer(footer)
}

pub fn decode_v3_aura0_footer(bytes: &[u8]) -> Result<V3FlatFooter> {
    decode_v3_flat_footer(bytes)
}

fn validate_footer_metadata(footer: &V3FlatFooter, limits: V3FlatLimits) -> Result<()> {
    let limits = limits.effective();
    validate_flat_schema(&footer.schema)?;
    if footer.record_count > limits.max_rows {
        return Err(AuraError::InvalidValue("v3 flat record count"));
    }
    if footer.body_len > limits.max_body_bytes {
        return Err(AuraError::InvalidValue("v3 flat body length"));
    }
    if footer.chunks.len() > limits.max_chunks {
        return Err(AuraError::InvalidValue("v3 flat chunk count"));
    }
    if footer.schema_fingerprint != canonical_v3_schema_fingerprint(&footer.schema)? {
        return Err(AuraError::InvalidValue("v3 flat schema fingerprint"));
    }
    let mapping = footer
        .schema
        .compact_schema_map
        .as_deref()
        .ok_or(AuraError::InvalidValue("v3 flat schema map"))?;
    if footer.primary_timestamp_slot != primary_timestamp_slot(mapping) {
        return Err(AuraError::InvalidValue("v3 flat primary timestamp"));
    }
    let sequence_slot = primary_sequence_slot(&footer.schema)?;
    if footer.primary_sequence_slot != sequence_slot {
        return Err(AuraError::InvalidValue("v3 flat primary sequence"));
    }
    validate_stats_metadata(footer, limits)?;
    validate_chunk_metadata(footer, limits)?;
    Ok(())
}

fn validate_flat_schema(schema: &SchemaDescriptor) -> Result<()> {
    if schema.encoding_version != SchemaEncodingVersion::V3 {
        return Err(AuraError::InvalidValue("v3 flat schema"));
    }
    schema.validate()?;
    if !schema.groups.is_empty()
        || !schema.derived_expressions.is_empty()
        || schema
            .fields
            .iter()
            .any(|field| field.scope != FieldScope::Event)
    {
        return Err(AuraError::InvalidValue("v3 flat schema"));
    }
    let mapping = schema
        .compact_schema_map
        .as_deref()
        .ok_or(AuraError::InvalidValue("v3 flat schema map"))?;
    if mapping.len() != schema.fields.len()
        || mapping
            .iter()
            .any(|byte| *byte == 200 || (101..=239).contains(byte))
    {
        return Err(AuraError::InvalidValue("v3 flat schema map"));
    }
    Ok(())
}

fn primary_sequence_slot(schema: &SchemaDescriptor) -> Result<Option<u16>> {
    let mut slot = None;
    for field in &schema.fields {
        if field.role == FieldRole::Sequence
            && (field.field_type != FieldType::U64 || slot.replace(field.index).is_some())
        {
            return Err(AuraError::InvalidValue("v3 flat primary sequence"));
        }
    }
    Ok(slot)
}

fn primary_timestamp_slot(mapping: &[u8]) -> u16 {
    if mapping.first() == Some(&100) {
        0
    } else {
        NO_SLOT
    }
}

fn validate_stats_metadata(footer: &V3FlatFooter, limits: V3FlatLimits) -> Result<()> {
    if footer.stats.len() != footer.schema.fields.len() {
        return Err(AuraError::InvalidValue("v3 flat stats count"));
    }
    for (field, stat) in footer.schema.fields.iter().zip(&footer.stats) {
        let structurally_invalid = stat.slot != field.index
            || stat.field_type != field.field_type
            || stat.flags != stats_flags(field.field_type, field.nullable)
            || stat.flags & !STAT_KNOWN_FLAGS != 0
            || stat.present_count.checked_add(stat.null_count) != Some(footer.record_count)
            || (!field.nullable && stat.null_count != 0)
            || (stat.present_count == 0
                && (stat.logical_payload_bytes != 0 || stat.max_present_value_byte_len != 0));
        if structurally_invalid {
            return Err(AuraError::InvalidValue("v3 flat stats"));
        }
        if let Some(width) = stats_fixed_width(field.field_type) {
            let expected_payload = stat
                .present_count
                .checked_mul(u64::from(width))
                .ok_or(AuraError::InvalidValue("v3 flat stats"))?;
            let expected_max = if stat.present_count == 0 { 0 } else { width };
            if stat.logical_payload_bytes != expected_payload
                || stat.max_present_value_byte_len != expected_max
            {
                return Err(AuraError::InvalidValue("v3 flat stats"));
            }
        } else {
            let variable_limit = min_usize(
                limits.value_limits.max_variable_value_bytes,
                crate::v3_values::MAX_V3_VARIABLE_VALUE_BYTES,
            );
            let framed_payload = stat
                .present_count
                .checked_mul(4)
                .ok_or(AuraError::InvalidValue("v3 flat stats"))?;
            let minimum_payload = if stat.present_count == 0 {
                framed_payload
            } else {
                framed_payload
                    .checked_add(u64::from(stat.max_present_value_byte_len))
                    .ok_or(AuraError::InvalidValue("v3 flat stats"))?
            };
            let maximum_per_value = u64::from(stat.max_present_value_byte_len)
                .checked_add(4)
                .ok_or(AuraError::InvalidValue("v3 flat stats"))?;
            let maximum_payload = stat
                .present_count
                .checked_mul(maximum_per_value)
                .ok_or(AuraError::InvalidValue("v3 flat stats"))?;
            if stat.max_present_value_byte_len as usize > variable_limit
                || stat.logical_payload_bytes < minimum_payload
                || stat.logical_payload_bytes > maximum_payload
            {
                return Err(AuraError::InvalidValue("v3 flat stats"));
            }
        }
    }
    Ok(())
}

fn validate_chunk_metadata(footer: &V3FlatFooter, limits: V3FlatLimits) -> Result<()> {
    if footer.record_count == 0 {
        if footer.body_len != 0 || !footer.chunks.is_empty() {
            return Err(AuraError::InvalidValue("v3 flat zero-row file"));
        }
        return Ok(());
    }
    if footer.chunks.is_empty() {
        return Err(AuraError::InvalidValue("v3 flat chunk count"));
    }
    let mut next_row = 0u64;
    let mut next_offset = 0u64;
    let max_block_bytes = min_usize(
        limits.value_limits.max_block_bytes,
        MAX_V3_VALUE_BLOCK_BYTES,
    ) as u64;
    let timestamp_bounds_required = footer.primary_timestamp_slot != NO_SLOT
        && !footer.schema.fields[usize::from(footer.primary_timestamp_slot)].nullable;
    let sequence_bounds_required = footer
        .primary_sequence_slot
        .is_some_and(|slot| !footer.schema.fields[usize::from(slot)].nullable);
    for (index, chunk) in footer.chunks.iter().enumerate() {
        if chunk.chunk_id != index as u32
            || chunk.flags & !CHUNK_KNOWN_FLAGS != 0
            || chunk.row_count == 0
            || chunk.first_global_row != next_row
            || chunk.body_relative_offset != next_offset
            || chunk.stored_len == 0
            || chunk.stored_len > max_block_bytes
            || (!chunk.has_timestamp_bounds()
                && (chunk.first_timestamp != 0 || chunk.last_timestamp != 0))
            || (!chunk.has_sequence_bounds()
                && (chunk.first_sequence != 0 || chunk.last_sequence != 0))
            || (footer.primary_sequence_slot.is_none() && chunk.has_sequence_bounds())
            || (footer.primary_timestamp_slot == NO_SLOT && chunk.has_timestamp_bounds())
            || (timestamp_bounds_required && !chunk.has_timestamp_bounds())
            || (sequence_bounds_required && !chunk.has_sequence_bounds())
        {
            return Err(AuraError::InvalidValue("v3 flat chunk descriptor"));
        }
        next_row = next_row
            .checked_add(u64::from(chunk.row_count))
            .ok_or(AuraError::InvalidValue("v3 flat record count"))?;
        next_offset = next_offset
            .checked_add(chunk.stored_len)
            .ok_or(AuraError::InvalidValue("v3 flat body length"))?;
    }
    if next_row != footer.record_count || next_offset != footer.body_len {
        return Err(AuraError::InvalidValue("v3 flat chunk ranges"));
    }
    Ok(())
}

fn validate_header_footer(header: &AuraHeader, footer: &V3FlatFooter) -> Result<()> {
    if header.container_version != AuraContainerVersion::V3
        || header.profile != Profile::Aura0
        || header.stream_id != 0
        || header.dictionary_id != 0
        || header.base_time_ns != 0
        || !header.groups.is_empty()
        || !header.derived_expressions.is_empty()
        || header.schema_mapping
            != footer
                .schema
                .compact_schema_map
                .as_deref()
                .unwrap_or_default()
    {
        return Err(AuraError::InvalidValue("v3 flat header/footer agreement"));
    }
    Ok(())
}

fn validate_chunk_bounds(
    footer: &V3FlatFooter,
    chunk: &V3FlatChunkDescriptor,
    batch: &AuraV3Batch,
) -> Result<()> {
    if footer.primary_timestamp_slot != NO_SLOT {
        let timestamp = &batch.columns[usize::from(footer.primary_timestamp_slot)];
        let mut first_timestamp = None;
        let mut last_timestamp = None;
        for row in 0..batch.row_count as usize {
            if let Some(value) = timestamp.value_ref(row)? {
                let value = timestamp_i64(Some(value))?;
                first_timestamp.get_or_insert(value);
                last_timestamp = Some(value);
            }
        }
        match (first_timestamp, last_timestamp) {
            (Some(first), Some(last))
                if chunk.has_timestamp_bounds()
                    && chunk.first_timestamp == first
                    && chunk.last_timestamp == last => {}
            (None, None)
                if !chunk.has_timestamp_bounds()
                    && chunk.first_timestamp == 0
                    && chunk.last_timestamp == 0 => {}
            _ => return Err(AuraError::InvalidValue("v3 flat timestamp bounds")),
        }
    }
    if let Some(slot) = footer.primary_sequence_slot {
        let column = &batch.columns[usize::from(slot)];
        let mut first = None;
        let mut last = None;
        for row in 0..batch.row_count as usize {
            if let Some(crate::AuraV3ValueRef::U64(value)) = column.value_ref(row)? {
                first.get_or_insert(value);
                last = Some(value);
            }
        }
        match (first, last) {
            (Some(first), Some(last))
                if chunk.has_sequence_bounds()
                    && chunk.first_sequence == first
                    && chunk.last_sequence == last => {}
            (None, None)
                if !chunk.has_sequence_bounds()
                    && chunk.first_sequence == 0
                    && chunk.last_sequence == 0 => {}
            _ => return Err(AuraError::InvalidValue("v3 flat sequence bounds")),
        }
    }
    Ok(())
}

fn timestamp_i64(value: Option<crate::AuraV3ValueRef<'_>>) -> Result<i64> {
    match value {
        Some(crate::AuraV3ValueRef::TimestampMs(value))
        | Some(crate::AuraV3ValueRef::TimestampNs(value))
        | Some(crate::AuraV3ValueRef::I64(value)) => Ok(value),
        _ => Err(AuraError::InvalidValue("v3 flat timestamp bounds")),
    }
}

fn zero_stats(schema: &SchemaDescriptor) -> Vec<V3FlatColumnStats> {
    schema
        .fields
        .iter()
        .map(|field| V3FlatColumnStats {
            slot: field.index,
            field_type: field.field_type,
            flags: stats_flags(field.field_type, field.nullable),
            present_count: 0,
            null_count: 0,
            logical_payload_bytes: 0,
            max_present_value_byte_len: 0,
        })
        .collect()
}

fn accumulate_stats(stats: &mut [V3FlatColumnStats], batch: &AuraV3Batch) -> Result<()> {
    for (stat, column) in stats.iter_mut().zip(&batch.columns) {
        for row in 0..batch.row_count as usize {
            match column.value_ref(row)? {
                None => {
                    stat.null_count = stat
                        .null_count
                        .checked_add(1)
                        .ok_or(AuraError::InvalidValue("v3 flat stats"))?;
                }
                Some(value) => {
                    let len = value_payload_len(value)?;
                    stat.present_count = stat
                        .present_count
                        .checked_add(1)
                        .ok_or(AuraError::InvalidValue("v3 flat stats"))?;
                    stat.logical_payload_bytes = stat
                        .logical_payload_bytes
                        .checked_add(u64::from(len))
                        .and_then(|bytes| {
                            if matches!(
                                column.values,
                                AuraV3ColumnValues::Utf8(_) | AuraV3ColumnValues::DecimalText(_)
                            ) {
                                bytes.checked_add(4)
                            } else {
                                Some(bytes)
                            }
                        })
                        .ok_or(AuraError::InvalidValue("v3 flat stats"))?;
                    stat.max_present_value_byte_len = stat.max_present_value_byte_len.max(len);
                }
            }
        }
    }
    Ok(())
}

fn value_payload_len(value: crate::AuraV3ValueRef<'_>) -> Result<u32> {
    let len = match value {
        crate::AuraV3ValueRef::I8(_) | crate::AuraV3ValueRef::U8(_) => 1,
        crate::AuraV3ValueRef::I16(_) | crate::AuraV3ValueRef::U16(_) => 2,
        crate::AuraV3ValueRef::I32(_) | crate::AuraV3ValueRef::U32(_) => 4,
        crate::AuraV3ValueRef::I64(_)
        | crate::AuraV3ValueRef::U64(_)
        | crate::AuraV3ValueRef::TimestampNs(_)
        | crate::AuraV3ValueRef::TimestampMs(_) => 8,
        crate::AuraV3ValueRef::I128(_) | crate::AuraV3ValueRef::Opaque16(_) => 16,
        crate::AuraV3ValueRef::Utf8(value) | crate::AuraV3ValueRef::DecimalText(value) => {
            u32::try_from(value.len()).map_err(|_| AuraError::InvalidValue("v3 flat stats"))?
        }
    };
    Ok(len)
}

const fn stats_flags(field_type: FieldType, nullable: bool) -> u8 {
    let variable = matches!(field_type, FieldType::Utf8 | FieldType::DecimalText);
    let numeric = !matches!(
        field_type,
        FieldType::Utf8 | FieldType::DecimalText | FieldType::Opaque16
    );
    (if nullable { STAT_NULLABLE } else { 0 })
        | (if variable { STAT_VARIABLE } else { 0 })
        | (if numeric { STAT_NUMERIC } else { 0 })
}

const fn stats_fixed_width(field_type: FieldType) -> Option<u32> {
    match field_type {
        FieldType::I8 | FieldType::U8 => Some(1),
        FieldType::I16 | FieldType::U16 => Some(2),
        FieldType::I32 | FieldType::U32 => Some(4),
        FieldType::I64 | FieldType::U64 | FieldType::TimestampNs | FieldType::TimestampMs => {
            Some(8)
        }
        FieldType::I128 | FieldType::Opaque16 => Some(16),
        FieldType::Utf8 | FieldType::DecimalText => None,
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

fn footer_self_hash(bytes: &[u8]) -> Result<[u8; 32]> {
    domain_hash(FOOTER_HASH_DOMAIN, bytes, "v3 flat footer length")
}

fn plain_sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
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

    fn peek_u32(&self) -> Result<u32> {
        let bytes = self
            .bytes
            .get(self.offset..self.offset.saturating_add(4))
            .ok_or(AuraError::UnexpectedEof)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn array32(&mut self) -> Result<[u8; 32]> {
        Ok(self.take(32)?.try_into().unwrap())
    }

    fn finish(self) -> Result<()> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(AuraError::TrailingBytes(self.remaining()))
        }
    }
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32_len(out: &mut Vec<u8>, value: usize, name: &'static str) -> Result<()> {
    put_u32(
        out,
        u32::try_from(value).map_err(|_| AuraError::InvalidValue(name))?,
    );
    Ok(())
}
