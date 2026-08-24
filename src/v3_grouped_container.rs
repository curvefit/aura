//! Complete Aura0 V3 grouped exact-event container support.
//!
//! The body is a gapless sequence of uncompressed `AURAV3EB` version-1
//! blocks. The footer supplies independently checked event, child, byte, and
//! logical ranges; it does not advertise a compressed or compiled body.

use sha2::{Digest, Sha256};

use crate::format::{AuraContainerVersion, SEAL_MAGIC};
use crate::schema::{decode_schema_descriptor, encode_schema_descriptor, FieldRole, FieldScope};
use crate::v3_container::{
    accumulate_column_stats, plain_sha256, primary_timestamp_slot, validate_scoped_stats_metadata,
    zero_stats, V3FlatColumnStats,
};
use crate::v3_events::{
    canonical_v3_event_batch_sha256, decode_v3_event_block, validate_v3_grouped_exact_subset,
    AuraV3EventBatch, CanonicalV3EventHasher, V3EventLimits, MAX_V3_EVENT_BLOCK_BYTES,
    V3_EVENT_BLOCK_VERSION,
};
use crate::v3_values::{canonical_v3_schema_fingerprint, AuraV3Column};
use crate::{AuraError, AuraHeader, AuraV3ValueRef, FieldType, Profile, Result, SchemaDescriptor};

pub const V3_GROUPED_FOOTER_MAGIC: &[u8; 4] = b"AURP";
pub const V3_GROUPED_FOOTER_LAYOUT_VERSION: u16 = 1;
pub const V3_GROUPED_BODY_ENCODING_EXACT_EVENTS: u8 = 2;
pub const V3_GROUPED_BODY_LAYOUT_VERSION: u16 = 1;
pub const V3_GROUPED_EVENT_BLOCK_VERSION: u16 = V3_EVENT_BLOCK_VERSION;
pub const V3_GROUPED_FOOTER_PREFIX_BYTES: usize = 184;
pub const V3_GROUPED_STATS_DESCRIPTOR_BYTES: usize = 36;
pub const V3_GROUPED_CHUNK_DESCRIPTOR_BYTES: usize = 152;

pub const MAX_V3_GROUPED_FOOTER_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_V3_GROUPED_BODY_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
pub const MAX_V3_GROUPED_CHUNKS: usize = 65_536;
pub const MAX_V3_GROUPED_EVENTS: u64 = 16_777_216;
pub const MAX_V3_GROUPED_CHILDREN: u64 = 67_108_864;
pub const MAX_V3_GROUPED_SCHEMA_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_V3_GROUPED_IN_MEMORY_BODY_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_V3_GROUPED_IN_MEMORY_CHUNKS: usize = 4_096;
pub const DEFAULT_V3_GROUPED_IN_MEMORY_EVENTS: u64 = 1_048_576;
pub const DEFAULT_V3_GROUPED_IN_MEMORY_CHILDREN: u64 = 4_194_304;

const HEADER_HASH_DOMAIN: &[u8] = b"aura-v3-grouped-aura0-header-v1\0";
const BODY_HASH_DOMAIN: &[u8] = b"aura-v3-grouped-aura0-body-v1\0";
const FOOTER_HASH_DOMAIN: &[u8] = b"aura-v3-grouped-aura0-footer-v1\0";
const STATS_TABLE_VERSION: u16 = 1;
const CHUNK_TABLE_VERSION: u16 = 1;
const CHUNK_HAS_TIMESTAMP: u32 = 1;
const CHUNK_HAS_SEQUENCE: u32 = 2;
const CHUNK_KNOWN_FLAGS: u32 = CHUNK_HAS_TIMESTAMP | CHUNK_HAS_SEQUENCE;
const NO_SLOT: u16 = u16::MAX;
const TRAILER_BYTES: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V3GroupedLimits {
    pub max_footer_bytes: usize,
    pub max_body_bytes: u64,
    pub max_chunks: usize,
    pub max_events: u64,
    pub max_children: u64,
    pub event_limits: V3EventLimits,
}

impl V3GroupedLimits {
    pub const HARD: Self = Self {
        max_footer_bytes: MAX_V3_GROUPED_FOOTER_BYTES,
        max_body_bytes: MAX_V3_GROUPED_BODY_BYTES,
        max_chunks: MAX_V3_GROUPED_CHUNKS,
        max_events: MAX_V3_GROUPED_EVENTS,
        max_children: MAX_V3_GROUPED_CHILDREN,
        event_limits: V3EventLimits::HARD,
    };

    pub const DEFAULT_IN_MEMORY: Self = Self {
        max_footer_bytes: MAX_V3_GROUPED_FOOTER_BYTES,
        max_body_bytes: DEFAULT_V3_GROUPED_IN_MEMORY_BODY_BYTES,
        max_chunks: DEFAULT_V3_GROUPED_IN_MEMORY_CHUNKS,
        max_events: DEFAULT_V3_GROUPED_IN_MEMORY_EVENTS,
        max_children: DEFAULT_V3_GROUPED_IN_MEMORY_CHILDREN,
        event_limits: V3EventLimits::DEFAULT_IN_MEMORY,
    };

    pub(crate) const fn effective(self) -> Self {
        Self {
            max_footer_bytes: min_usize(self.max_footer_bytes, MAX_V3_GROUPED_FOOTER_BYTES),
            max_body_bytes: min_u64(self.max_body_bytes, MAX_V3_GROUPED_BODY_BYTES),
            max_chunks: min_usize(self.max_chunks, MAX_V3_GROUPED_CHUNKS),
            max_events: min_u64(self.max_events, MAX_V3_GROUPED_EVENTS),
            max_children: min_u64(self.max_children, MAX_V3_GROUPED_CHILDREN),
            event_limits: self.event_limits.effective(),
        }
    }
}

impl Default for V3GroupedLimits {
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

pub type V3GroupedColumnStats = V3FlatColumnStats;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3GroupedChunkDescriptor {
    pub chunk_id: u32,
    pub flags: u32,
    pub first_global_event: u64,
    pub event_count: u32,
    pub first_global_child: u64,
    pub child_count: u32,
    pub body_relative_offset: u64,
    pub stored_len: u64,
    pub stored_sha256: [u8; 32],
    pub chunk_logical_sha256: [u8; 32],
    pub first_timestamp: i64,
    pub last_timestamp: i64,
    pub first_sequence: u64,
    pub last_sequence: u64,
}

impl V3GroupedChunkDescriptor {
    pub const fn has_timestamp_bounds(&self) -> bool {
        self.flags & CHUNK_HAS_TIMESTAMP != 0
    }

    pub const fn has_sequence_bounds(&self) -> bool {
        self.flags & CHUNK_HAS_SEQUENCE != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3GroupedFooter {
    pub event_count: u64,
    pub child_count: u64,
    pub body_len: u64,
    pub schema: SchemaDescriptor,
    pub primary_timestamp_slot: u16,
    pub primary_sequence_slot: Option<u16>,
    pub schema_fingerprint: [u8; 32],
    pub header_sha256: [u8; 32],
    pub body_sha256: [u8; 32],
    pub global_logical_sha256: [u8; 32],
    pub stats: Vec<V3GroupedColumnStats>,
    pub chunks: Vec<V3GroupedChunkDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedV3GroupedAura0 {
    pub header: AuraHeader,
    pub footer: V3GroupedFooter,
    pub batches: Vec<AuraV3EventBatch>,
}

pub fn v3_grouped_header_sha256(header: &[u8]) -> Result<[u8; 32]> {
    domain_hash(HEADER_HASH_DOMAIN, header, "v3 grouped header length")
}

pub fn v3_grouped_body_sha256(body: &[u8]) -> Result<[u8; 32]> {
    domain_hash(BODY_HASH_DOMAIN, body, "v3 grouped body length")
}

pub(crate) fn v3_grouped_body_sha256_hasher(body_len: u64) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(BODY_HASH_DOMAIN);
    hasher.update(body_len.to_le_bytes());
    hasher
}

pub fn encode_v3_grouped_footer(footer: &V3GroupedFooter) -> Result<Vec<u8>> {
    validate_grouped_footer_metadata(footer, V3GroupedLimits::HARD)?;
    let schema_bytes = encode_schema_descriptor(&footer.schema)?;
    if schema_bytes.len() > MAX_V3_GROUPED_SCHEMA_BYTES {
        return Err(AuraError::InvalidValue("v3 grouped schema length"));
    }
    let stats_bytes = footer
        .stats
        .len()
        .checked_mul(V3_GROUPED_STATS_DESCRIPTOR_BYTES)
        .ok_or(AuraError::InvalidValue("v3 grouped footer length"))?;
    let chunks_bytes = footer
        .chunks
        .len()
        .checked_mul(V3_GROUPED_CHUNK_DESCRIPTOR_BYTES)
        .ok_or(AuraError::InvalidValue("v3 grouped footer length"))?;
    let total_len = V3_GROUPED_FOOTER_PREFIX_BYTES
        .checked_add(schema_bytes.len())
        .and_then(|len| len.checked_add(8 + stats_bytes))
        .and_then(|len| len.checked_add(8 + chunks_bytes))
        .and_then(|len| len.checked_add(32))
        .filter(|len| *len <= MAX_V3_GROUPED_FOOTER_BYTES)
        .ok_or(AuraError::InvalidValue("v3 grouped footer length"))?;
    let mut out = Vec::new();
    out.try_reserve_exact(total_len)
        .map_err(|_| AuraError::InvalidValue("v3 grouped footer allocation"))?;
    out.extend_from_slice(V3_GROUPED_FOOTER_MAGIC);
    put_u16(&mut out, 3);
    put_u16(&mut out, V3_GROUPED_FOOTER_LAYOUT_VERSION);
    out.extend_from_slice(&[V3_GROUPED_BODY_ENCODING_EXACT_EVENTS, 0, 0, 0]);
    put_u64(&mut out, footer.event_count);
    put_u64(&mut out, footer.child_count);
    put_u64(&mut out, footer.body_len);
    put_u32_len(&mut out, footer.stats.len(), "v3 grouped column count")?;
    put_u32_len(&mut out, footer.chunks.len(), "v3 grouped chunk count")?;
    put_u32(&mut out, footer.schema.schema_id);
    put_u16(&mut out, footer.primary_timestamp_slot);
    put_u16(&mut out, footer.primary_sequence_slot.unwrap_or(NO_SLOT));
    put_u16(&mut out, V3_GROUPED_BODY_LAYOUT_VERSION);
    put_u16(&mut out, V3_GROUPED_EVENT_BLOCK_VERSION);
    out.extend_from_slice(&footer.schema_fingerprint);
    out.extend_from_slice(&footer.header_sha256);
    out.extend_from_slice(&footer.body_sha256);
    out.extend_from_slice(&footer.global_logical_sha256);
    debug_assert_eq!(out.len(), V3_GROUPED_FOOTER_PREFIX_BYTES);
    out.extend_from_slice(&schema_bytes);
    put_u16(&mut out, STATS_TABLE_VERSION);
    put_u16(&mut out, V3_GROUPED_STATS_DESCRIPTOR_BYTES as u16);
    put_u32_len(&mut out, footer.stats.len(), "v3 grouped stats count")?;
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
    put_u16(&mut out, V3_GROUPED_CHUNK_DESCRIPTOR_BYTES as u16);
    put_u32_len(&mut out, footer.chunks.len(), "v3 grouped chunk count")?;
    for chunk in &footer.chunks {
        put_u32(&mut out, chunk.chunk_id);
        put_u32(&mut out, chunk.flags);
        put_u64(&mut out, chunk.first_global_event);
        put_u32(&mut out, chunk.event_count);
        put_u16(&mut out, V3_GROUPED_EVENT_BLOCK_VERSION);
        put_u16(&mut out, 0);
        put_u64(&mut out, chunk.first_global_child);
        put_u32(&mut out, chunk.child_count);
        put_u32(&mut out, 0);
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

pub fn decode_v3_grouped_footer(bytes: &[u8]) -> Result<V3GroupedFooter> {
    decode_v3_grouped_footer_with_limits(bytes, V3GroupedLimits::HARD)
}

pub fn decode_v3_grouped_footer_with_limits(
    bytes: &[u8],
    limits: V3GroupedLimits,
) -> Result<V3GroupedFooter> {
    let limits = limits.effective();
    if bytes.len() > limits.max_footer_bytes {
        return Err(AuraError::InvalidValue("v3 grouped footer length"));
    }
    if bytes.len() < V3_GROUPED_FOOTER_PREFIX_BYTES + 4 + 8 + 8 + 32 {
        return Err(AuraError::UnexpectedEof);
    }
    let hash_start = bytes
        .len()
        .checked_sub(32)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_self_hash(&bytes[..hash_start])? != bytes[hash_start..] {
        return Err(AuraError::InvalidValue("v3 grouped footer hash"));
    }
    let mut reader = Reader::new(&bytes[..hash_start]);
    if reader.take(4)? != V3_GROUPED_FOOTER_MAGIC {
        return Err(AuraError::InvalidMagic { expected: "AURP" });
    }
    let version = reader.u16()?;
    if version != 3 {
        return Err(AuraError::UnsupportedVersion(version));
    }
    if reader.u16()? != V3_GROUPED_FOOTER_LAYOUT_VERSION {
        return Err(AuraError::InvalidValue("v3 grouped footer layout"));
    }
    if reader.u8()? != V3_GROUPED_BODY_ENCODING_EXACT_EVENTS {
        return Err(AuraError::InvalidValue("v3 grouped body encoding"));
    }
    if reader.u8()? != 0 || reader.u8()? != 0 || reader.u8()? != 0 {
        return Err(AuraError::InvalidValue("v3 grouped compression"));
    }
    let event_count = reader.u64()?;
    let child_count = reader.u64()?;
    let body_len = reader.u64()?;
    let column_count = reader.u32()? as usize;
    let chunk_count = reader.u32()? as usize;
    if event_count > limits.max_events {
        return Err(AuraError::InvalidValue("v3 grouped event count"));
    }
    if child_count > limits.max_children {
        return Err(AuraError::InvalidValue("v3 grouped child count"));
    }
    if body_len > limits.max_body_bytes {
        return Err(AuraError::InvalidValue("v3 grouped body length"));
    }
    if chunk_count > limits.max_chunks {
        return Err(AuraError::InvalidValue("v3 grouped chunk count"));
    }
    let schema_id = reader.u32()?;
    let primary_timestamp_slot = reader.u16()?;
    let sequence_raw = reader.u16()?;
    let primary_sequence_slot = (sequence_raw != NO_SLOT).then_some(sequence_raw);
    if reader.u16()? != V3_GROUPED_BODY_LAYOUT_VERSION {
        return Err(AuraError::InvalidValue("v3 grouped body layout version"));
    }
    if reader.u16()? != V3_GROUPED_EVENT_BLOCK_VERSION {
        return Err(AuraError::InvalidValue("v3 grouped event block version"));
    }
    let schema_fingerprint = reader.array32()?;
    let header_sha256 = reader.array32()?;
    let body_sha256 = reader.array32()?;
    let global_logical_sha256 = reader.array32()?;
    debug_assert_eq!(reader.offset, V3_GROUPED_FOOTER_PREFIX_BYTES);
    let schema_payload_len = reader.peek_u32()? as usize;
    let schema_len = schema_payload_len
        .checked_add(4)
        .filter(|len| *len <= MAX_V3_GROUPED_SCHEMA_BYTES)
        .ok_or(AuraError::InvalidValue("v3 grouped schema length"))?;
    let schema = decode_schema_descriptor(reader.take(schema_len)?)?;
    if schema.schema_id != schema_id {
        return Err(AuraError::InvalidValue("v3 grouped schema id"));
    }
    if canonical_v3_schema_fingerprint(&schema)? != schema_fingerprint {
        return Err(AuraError::InvalidValue("v3 grouped schema fingerprint"));
    }
    if column_count != schema.fields.len() {
        return Err(AuraError::InvalidValue("v3 grouped column count"));
    }
    if reader.u16()? != STATS_TABLE_VERSION
        || reader.u16()? as usize != V3_GROUPED_STATS_DESCRIPTOR_BYTES
    {
        return Err(AuraError::InvalidValue("v3 grouped stats table"));
    }
    let stats_count = reader.u32()? as usize;
    if stats_count != column_count
        || stats_count > reader.remaining() / V3_GROUPED_STATS_DESCRIPTOR_BYTES
    {
        return Err(AuraError::InvalidValue("v3 grouped stats count"));
    }
    let mut stats = Vec::new();
    stats
        .try_reserve_exact(stats_count)
        .map_err(|_| AuraError::InvalidValue("v3 grouped footer allocation"))?;
    for _ in 0..stats_count {
        let slot = reader.u16()?;
        let field_type = FieldType::from_code(reader.u8()?)?;
        let flags = reader.u8()?;
        let present_count = reader.u64()?;
        let null_count = reader.u64()?;
        let logical_payload_bytes = reader.u64()?;
        let max_present_value_byte_len = reader.u32()?;
        if reader.u32()? != 0 {
            return Err(AuraError::InvalidValue("v3 grouped stats reserved"));
        }
        stats.push(V3GroupedColumnStats {
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
        || reader.u16()? as usize != V3_GROUPED_CHUNK_DESCRIPTOR_BYTES
    {
        return Err(AuraError::InvalidValue("v3 grouped chunk table"));
    }
    let table_chunk_count = reader.u32()? as usize;
    if table_chunk_count != chunk_count
        || table_chunk_count > reader.remaining() / V3_GROUPED_CHUNK_DESCRIPTOR_BYTES
    {
        return Err(AuraError::InvalidValue("v3 grouped chunk count"));
    }
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(table_chunk_count)
        .map_err(|_| AuraError::InvalidValue("v3 grouped footer allocation"))?;
    for _ in 0..table_chunk_count {
        let chunk_id = reader.u32()?;
        let flags = reader.u32()?;
        let first_global_event = reader.u64()?;
        let event_count = reader.u32()?;
        if reader.u16()? != V3_GROUPED_EVENT_BLOCK_VERSION {
            return Err(AuraError::InvalidValue("v3 grouped event block version"));
        }
        if reader.u16()? != 0 {
            return Err(AuraError::InvalidValue("v3 grouped chunk reserved"));
        }
        let first_global_child = reader.u64()?;
        let child_count = reader.u32()?;
        if reader.u32()? != 0 {
            return Err(AuraError::InvalidValue("v3 grouped chunk reserved"));
        }
        chunks.push(V3GroupedChunkDescriptor {
            chunk_id,
            flags,
            first_global_event,
            event_count,
            first_global_child,
            child_count,
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
    let footer = V3GroupedFooter {
        event_count,
        child_count,
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
    validate_grouped_footer_metadata(&footer, limits)?;
    Ok(footer)
}

pub fn decode_v3_grouped_aura0(bytes: &[u8]) -> Result<DecodedV3GroupedAura0> {
    decode_v3_grouped_aura0_with_limits(bytes, V3GroupedLimits::default())
}

pub fn decode_v3_grouped_aura0_with_limits(
    bytes: &[u8],
    limits: V3GroupedLimits,
) -> Result<DecodedV3GroupedAura0> {
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
        return Err(AuraError::InvalidValue("v3 grouped footer length"));
    }
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::InvalidValue("v3 grouped footer length"))?;
    let header_len = AuraHeader::encoded_len(bytes)?;
    if header_len > footer_start {
        return Err(AuraError::InvalidValue("v3 grouped file ranges"));
    }
    let header_bytes = &bytes[..header_len];
    let body = &bytes[header_len..footer_start];
    if body.len() as u64 > limits.max_body_bytes {
        return Err(AuraError::InvalidValue("v3 grouped body length"));
    }
    let header = AuraHeader::decode(header_bytes)?;
    let footer =
        decode_v3_grouped_footer_with_limits(&bytes[footer_start..footer_len_offset], limits)?;
    validate_grouped_header_footer(&header, &footer)?;
    if footer.body_len != body.len() as u64 {
        return Err(AuraError::InvalidValue("v3 grouped body length"));
    }
    if v3_grouped_header_sha256(header_bytes)? != footer.header_sha256 {
        return Err(AuraError::InvalidValue("v3 grouped header hash"));
    }
    if v3_grouped_body_sha256(body)? != footer.body_sha256 {
        return Err(AuraError::InvalidValue("v3 grouped body hash"));
    }
    let total_events = u32::try_from(footer.event_count)
        .map_err(|_| AuraError::InvalidValue("v3 grouped event count"))?;
    let total_children = u32::try_from(footer.child_count)
        .map_err(|_| AuraError::InvalidValue("v3 grouped child count"))?;
    let mut global = CanonicalV3EventHasher::new(
        &footer.schema,
        total_events,
        total_children,
        limits.event_limits,
    )?;
    let mut actual_stats = zero_stats(&footer.schema);
    let mut batches = Vec::new();
    batches
        .try_reserve_exact(footer.chunks.len())
        .map_err(|_| AuraError::InvalidValue("v3 grouped batch allocation"))?;
    for chunk in &footer.chunks {
        let start = usize::try_from(chunk.body_relative_offset)
            .map_err(|_| AuraError::InvalidValue("v3 grouped chunk range"))?;
        let stored_len = usize::try_from(chunk.stored_len)
            .map_err(|_| AuraError::InvalidValue("v3 grouped chunk range"))?;
        let end = start
            .checked_add(stored_len)
            .ok_or(AuraError::InvalidValue("v3 grouped chunk range"))?;
        let stored = body
            .get(start..end)
            .ok_or(AuraError::InvalidValue("v3 grouped chunk range"))?;
        let batch = verify_grouped_chunk(&footer, chunk, stored, limits.event_limits)?;
        accumulate_grouped_stats(&footer.schema, &mut actual_stats, &batch)?;
        global.update_batch(&footer.schema, &batch)?;
        batches.push(batch);
    }
    if actual_stats != footer.stats {
        return Err(AuraError::InvalidValue("v3 grouped stats"));
    }
    if global.finalize()? != footer.global_logical_sha256 {
        return Err(AuraError::InvalidValue("v3 grouped global logical hash"));
    }
    Ok(DecodedV3GroupedAura0 {
        header,
        footer,
        batches,
    })
}

pub(crate) fn validate_grouped_footer_metadata(
    footer: &V3GroupedFooter,
    limits: V3GroupedLimits,
) -> Result<()> {
    let limits = limits.effective();
    validate_v3_grouped_exact_subset(&footer.schema)?;
    if footer.event_count > limits.max_events {
        return Err(AuraError::InvalidValue("v3 grouped event count"));
    }
    if footer.child_count > limits.max_children {
        return Err(AuraError::InvalidValue("v3 grouped child count"));
    }
    if footer.body_len > limits.max_body_bytes {
        return Err(AuraError::InvalidValue("v3 grouped body length"));
    }
    if footer.chunks.len() > limits.max_chunks {
        return Err(AuraError::InvalidValue("v3 grouped chunk count"));
    }
    if footer.schema_fingerprint != canonical_v3_schema_fingerprint(&footer.schema)? {
        return Err(AuraError::InvalidValue("v3 grouped schema fingerprint"));
    }
    let mapping = footer
        .schema
        .compact_schema_map
        .as_deref()
        .ok_or(AuraError::InvalidValue("v3 grouped schema map"))?;
    if footer.primary_timestamp_slot != primary_timestamp_slot(mapping) {
        return Err(AuraError::InvalidValue("v3 grouped primary timestamp"));
    }
    if footer.primary_sequence_slot != grouped_primary_sequence_slot(&footer.schema)? {
        return Err(AuraError::InvalidValue("v3 grouped primary sequence"));
    }
    validate_scoped_stats_metadata(
        &footer.schema,
        &footer.stats,
        footer.event_count,
        footer.child_count,
        limits.event_limits.max_variable_value_bytes,
        "v3 grouped stats",
        "v3 grouped stats count",
    )?;
    validate_grouped_chunk_metadata(footer, limits)
}

pub(crate) fn grouped_primary_sequence_slot(schema: &SchemaDescriptor) -> Result<Option<u16>> {
    let mut candidate = None;
    let mut sequence_count = 0usize;
    for field in &schema.fields {
        if field.role == FieldRole::Sequence {
            sequence_count = sequence_count
                .checked_add(1)
                .ok_or(AuraError::InvalidValue("v3 grouped primary sequence"))?;
            if field.scope == FieldScope::Event && field.field_type == FieldType::U64 {
                candidate = Some(field.index);
            }
        }
    }
    Ok((sequence_count == 1).then_some(candidate).flatten())
}

fn validate_grouped_chunk_metadata(
    footer: &V3GroupedFooter,
    limits: V3GroupedLimits,
) -> Result<()> {
    if footer.event_count == 0 {
        if footer.child_count != 0 || footer.body_len != 0 || !footer.chunks.is_empty() {
            return Err(AuraError::InvalidValue("v3 grouped zero-event file"));
        }
        return Ok(());
    }
    if footer.chunks.is_empty() {
        return Err(AuraError::InvalidValue("v3 grouped chunk count"));
    }
    let mut next_event = 0u64;
    let mut next_child = 0u64;
    let mut next_offset = 0u64;
    let max_block_bytes = min_usize(
        limits.event_limits.max_block_bytes,
        MAX_V3_EVENT_BLOCK_BYTES,
    ) as u64;
    let timestamp_required = footer.primary_timestamp_slot != NO_SLOT
        && !footer.schema.fields[usize::from(footer.primary_timestamp_slot)].nullable;
    let sequence_required = footer
        .primary_sequence_slot
        .is_some_and(|slot| !footer.schema.fields[usize::from(slot)].nullable);
    let event_field_count = footer
        .schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Event)
        .count();
    let repeated_field_count = footer
        .schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Repeated)
        .count();
    for (index, chunk) in footer.chunks.iter().enumerate() {
        let chunk_events = usize::try_from(chunk.event_count)
            .map_err(|_| AuraError::InvalidValue("v3 grouped chunk event count"))?;
        let chunk_children = usize::try_from(chunk.child_count)
            .map_err(|_| AuraError::InvalidValue("v3 grouped chunk child count"))?;
        let chunk_values = chunk_events
            .checked_mul(event_field_count)
            .and_then(|events| {
                chunk_children
                    .checked_mul(repeated_field_count)
                    .and_then(|children| events.checked_add(children))
            })
            .ok_or(AuraError::InvalidValue("v3 grouped chunk value count"))?;
        if chunk.chunk_id != index as u32
            || chunk.flags & !CHUNK_KNOWN_FLAGS != 0
            || chunk.event_count == 0
            || chunk_events > limits.event_limits.max_events
            || chunk_children > limits.event_limits.max_children
            || chunk_values > limits.event_limits.max_values
            || chunk.first_global_event != next_event
            || chunk.first_global_child != next_child
            || chunk.body_relative_offset != next_offset
            || chunk.stored_len == 0
            || chunk.stored_len > max_block_bytes
            || (!chunk.has_timestamp_bounds()
                && (chunk.first_timestamp != 0 || chunk.last_timestamp != 0))
            || (!chunk.has_sequence_bounds()
                && (chunk.first_sequence != 0 || chunk.last_sequence != 0))
            || (footer.primary_timestamp_slot == NO_SLOT && chunk.has_timestamp_bounds())
            || (footer.primary_sequence_slot.is_none() && chunk.has_sequence_bounds())
            || (timestamp_required && !chunk.has_timestamp_bounds())
            || (sequence_required && !chunk.has_sequence_bounds())
        {
            return Err(AuraError::InvalidValue("v3 grouped chunk descriptor"));
        }
        next_event = next_event
            .checked_add(u64::from(chunk.event_count))
            .ok_or(AuraError::InvalidValue("v3 grouped event count"))?;
        next_child = next_child
            .checked_add(u64::from(chunk.child_count))
            .ok_or(AuraError::InvalidValue("v3 grouped child count"))?;
        next_offset = next_offset
            .checked_add(chunk.stored_len)
            .ok_or(AuraError::InvalidValue("v3 grouped body length"))?;
    }
    if next_event != footer.event_count
        || next_child != footer.child_count
        || next_offset != footer.body_len
    {
        return Err(AuraError::InvalidValue("v3 grouped chunk ranges"));
    }
    Ok(())
}

pub(crate) fn validate_grouped_header_footer(
    header: &AuraHeader,
    footer: &V3GroupedFooter,
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
        || header.derived_expressions != footer.schema.derived_expressions
        || !header.derived_expressions.is_empty()
    {
        return Err(AuraError::InvalidValue(
            "v3 grouped header/footer agreement",
        ));
    }
    Ok(())
}

pub(crate) fn verify_grouped_chunk(
    footer: &V3GroupedFooter,
    chunk: &V3GroupedChunkDescriptor,
    stored: &[u8],
    limits: V3EventLimits,
) -> Result<AuraV3EventBatch> {
    if plain_sha256(stored) != chunk.stored_sha256 {
        return Err(AuraError::InvalidValue("v3 grouped stored chunk hash"));
    }
    let batch = decode_v3_event_block(&footer.schema, stored, limits)?;
    if batch.event_count != chunk.event_count || batch.child_count() != chunk.child_count {
        return Err(AuraError::InvalidValue("v3 grouped chunk counts"));
    }
    if canonical_v3_event_batch_sha256(&footer.schema, &batch, limits)?
        != chunk.chunk_logical_sha256
    {
        return Err(AuraError::InvalidValue("v3 grouped chunk logical hash"));
    }
    validate_grouped_chunk_bounds(footer, chunk, &batch)?;
    Ok(batch)
}

pub(crate) fn validate_grouped_chunk_bounds(
    footer: &V3GroupedFooter,
    chunk: &V3GroupedChunkDescriptor,
    batch: &AuraV3EventBatch,
) -> Result<()> {
    if footer.primary_timestamp_slot != NO_SLOT {
        let column = event_column(&footer.schema, batch, footer.primary_timestamp_slot)?;
        let mut bounds = None;
        for row in 0..batch.event_count as usize {
            if let Some(value) = column.value_ref(row)? {
                let value = match value {
                    AuraV3ValueRef::TimestampMs(value)
                    | AuraV3ValueRef::TimestampNs(value)
                    | AuraV3ValueRef::I64(value) => value,
                    _ => return Err(AuraError::InvalidValue("v3 grouped timestamp bounds")),
                };
                bounds.get_or_insert((value, value)).1 = value;
            }
        }
        match bounds {
            Some((first, last))
                if chunk.has_timestamp_bounds()
                    && chunk.first_timestamp == first
                    && chunk.last_timestamp == last => {}
            None if !chunk.has_timestamp_bounds()
                && chunk.first_timestamp == 0
                && chunk.last_timestamp == 0 => {}
            _ => return Err(AuraError::InvalidValue("v3 grouped timestamp bounds")),
        }
    }
    if let Some(slot) = footer.primary_sequence_slot {
        let column = event_column(&footer.schema, batch, slot)?;
        let mut bounds = None;
        for row in 0..batch.event_count as usize {
            if let Some(AuraV3ValueRef::U64(value)) = column.value_ref(row)? {
                bounds.get_or_insert((value, value)).1 = value;
            }
        }
        match bounds {
            Some((first, last))
                if chunk.has_sequence_bounds()
                    && chunk.first_sequence == first
                    && chunk.last_sequence == last => {}
            None if !chunk.has_sequence_bounds()
                && chunk.first_sequence == 0
                && chunk.last_sequence == 0 => {}
            _ => return Err(AuraError::InvalidValue("v3 grouped sequence bounds")),
        }
    }
    Ok(())
}

fn event_column<'a>(
    schema: &SchemaDescriptor,
    batch: &'a AuraV3EventBatch,
    slot: u16,
) -> Result<&'a AuraV3Column> {
    schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Event)
        .position(|field| field.index == slot)
        .and_then(|position| batch.event_columns.get(position))
        .ok_or(AuraError::InvalidValue("v3 grouped event column"))
}

pub(crate) fn accumulate_grouped_stats(
    schema: &SchemaDescriptor,
    stats: &mut [V3GroupedColumnStats],
    batch: &AuraV3EventBatch,
) -> Result<()> {
    let mut event = batch.event_columns.iter();
    let mut repeated = batch.repeated_columns.iter();
    for field in &schema.fields {
        let (column, rows) = match field.scope {
            FieldScope::Event => (
                event
                    .next()
                    .ok_or(AuraError::InvalidValue("v3 grouped stats"))?,
                batch.event_count as usize,
            ),
            FieldScope::Repeated => (
                repeated
                    .next()
                    .ok_or(AuraError::InvalidValue("v3 grouped stats"))?,
                batch.child_count() as usize,
            ),
        };
        let stat = stats
            .get_mut(usize::from(field.index))
            .ok_or(AuraError::InvalidValue("v3 grouped stats"))?;
        accumulate_column_stats(stat, column, rows, "v3 grouped stats")?;
    }
    if event.next().is_some() || repeated.next().is_some() {
        return Err(AuraError::InvalidValue("v3 grouped stats"));
    }
    Ok(())
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
    domain_hash(FOOTER_HASH_DOMAIN, bytes, "v3 grouped footer length")
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
