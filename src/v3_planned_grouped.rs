//! Reference planned-grouped Aura0 V3 container (layout 2, body encoding 3).
//!
//! Attempt 1 stores exact direct `AURAV3EB` chunks and a complete canonical
//! Aura Plan v2. Schema relationship permissions are recorded but no
//! relationship transform is attempted.

use sha2::{Digest, Sha256};

use crate::format::{AuraContainerVersion, SEAL_MAGIC};
use crate::schema::{decode_schema_descriptor, encode_schema_descriptor, SchemaDescriptor};
use crate::v3_events::{
    canonical_v3_event_batch_sha256, decode_v3_event_block, encode_v3_event_block,
    validate_v3_event_batch, AuraV3EventBatch, CanonicalV3EventHasher,
};
use crate::v3_grouped_container::V3GroupedLimits;
use crate::v3_plan_v2::{AuraPlanV2, PlanV2Inspection, MAX_AURA_PLAN_V2_BYTES};
use crate::v3_values::canonical_v3_schema_fingerprint;
use crate::{AuraError, AuraHeader, Profile, Result};

pub const V3_PLANNED_GROUPED_FOOTER_LAYOUT_VERSION: u16 = 2;
pub const V3_PLANNED_GROUPED_BODY_ENCODING: u8 = 3;
pub const V3_PLANNED_GROUPED_BODY_LAYOUT_VERSION: u16 = 1;
pub const V3_PLANNED_GROUPED_DIRECT_BLOCK_VERSION: u16 = crate::V3_EVENT_BLOCK_VERSION;
pub const V3_PLANNED_GROUPED_FOOTER_PREFIX_BYTES: usize = 216;
pub const V3_PLANNED_GROUPED_CHUNK_DESCRIPTOR_BYTES: usize = 120;
pub const MAX_V3_PLANNED_GROUPED_FOOTER_BYTES: usize = 64 * 1024 * 1024;

const FOOTER_MAGIC: &[u8; 4] = b"AURP";
const FOOTER_HASH_DOMAIN: &[u8] = b"aura-v3-planned-grouped-footer-v1\0";
const HEADER_HASH_DOMAIN: &[u8] = b"aura-v3-planned-grouped-header-v1\0";
const BODY_HASH_DOMAIN: &[u8] = b"aura-v3-planned-grouped-body-v1\0";
const CHUNK_TABLE_VERSION: u16 = 1;
const TRAILER_BYTES: usize = 12;

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
        let batch = decode_v3_event_block(&footer.schema, block, limits.event_limits)?;
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
    put_u16(&mut bytes, V3_PLANNED_GROUPED_BODY_LAYOUT_VERSION);
    put_u16(&mut bytes, V3_PLANNED_GROUPED_DIRECT_BLOCK_VERSION);
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
        put_u16(&mut bytes, V3_PLANNED_GROUPED_DIRECT_BLOCK_VERSION);
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
    if reader.u16()? != V3_PLANNED_GROUPED_BODY_LAYOUT_VERSION
        || reader.u16()? != V3_PLANNED_GROUPED_DIRECT_BLOCK_VERSION
    {
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
        if reader.u16()? != V3_PLANNED_GROUPED_DIRECT_BLOCK_VERSION || reader.u16()? != 0 {
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
    if footer.event_count > limits.max_events
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
        body_layout_version: V3_PLANNED_GROUPED_BODY_LAYOUT_VERSION,
        direct_block_version: V3_PLANNED_GROUPED_DIRECT_BLOCK_VERSION,
        compression: "none",
        plan: footer.plan.inspection(),
        schema_relationships_authorized_only: true,
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
