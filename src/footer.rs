use crate::bytes::{put_i64_le, put_u16_le, put_u32_le, put_u64_le, put_u8, ByteReader};
use crate::chunk::ChunkDescriptor;
use crate::format::{
    AuraContainerVersion, AURA_CHUNK_DESCRIPTOR_SIZE, DEFAULT_CONTAINER_VERSION,
    MAX_AURA_CHUNK_COUNT,
};
use crate::generic_planner::validate_generic_plan_schema_authorization;
use crate::instructions::GenericInstructionPlan;
use crate::plan::{Aura0Plan, Aura1Plan, FieldEncoding, PhysicalFieldPlan};
use crate::schema::{decode_schema_block, encode_schema_block, SchemaDescriptor};
use crate::stats::{
    FieldStats, FieldStatsSummary, IngestStats, PhysicalWidth, RelatedFieldStats,
    RunHistogramEntry, ShapeStats,
};
use crate::{AuraError, Result};

pub const FOOTER_MAGIC: &[u8; 4] = b"AURF";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CompressionKind {
    None = 0,
    Zstd = 1,
}

impl CompressionKind {
    pub fn from_code(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Zstd),
            _ => Err(AuraError::InvalidValue("compression kind")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressionDescriptor {
    pub kind: CompressionKind,
    pub level: u8,
}

impl CompressionDescriptor {
    pub const fn none() -> Self {
        Self {
            kind: CompressionKind::None,
            level: 0,
        }
    }

    pub const fn zstd(level: u8) -> Self {
        Self {
            kind: CompressionKind::Zstd,
            level,
        }
    }
}

/// Seal-time manifest appended to an Aura file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraFooter {
    pub container_version: AuraContainerVersion,
    pub schema: SchemaDescriptor,
    pub stats: IngestStats,
    pub compression: CompressionDescriptor,
    pub aura0_plan: Option<Aura0Plan>,
    pub aura1_plan: Option<Aura1Plan>,
    pub generic_aura0_plan: Option<GenericInstructionPlan>,
    pub chunks: Vec<ChunkDescriptor>,
}

impl AuraFooter {
    pub fn new(schema: SchemaDescriptor, stats: IngestStats) -> Self {
        Self {
            container_version: DEFAULT_CONTAINER_VERSION,
            schema,
            stats,
            compression: CompressionDescriptor::none(),
            aura0_plan: None,
            aura1_plan: None,
            generic_aura0_plan: None,
            chunks: Vec::new(),
        }
    }

    pub const fn with_container_version(mut self, container_version: AuraContainerVersion) -> Self {
        self.container_version = container_version;
        self
    }

    pub const fn container_version(&self) -> AuraContainerVersion {
        self.container_version
    }

    pub fn with_compression(mut self, compression: CompressionDescriptor) -> Self {
        self.compression = compression;
        self
    }

    pub fn with_aura0_plan(mut self, plan: Aura0Plan) -> Self {
        self.aura0_plan = Some(plan);
        self
    }

    pub fn with_aura1_plan(mut self, plan: Aura1Plan) -> Self {
        self.aura1_plan = Some(plan);
        self
    }

    pub fn with_generic_aura0_plan(mut self, plan: GenericInstructionPlan) -> Self {
        self.generic_aura0_plan = Some(plan);
        self
    }

    pub fn with_chunks(mut self, chunks: Vec<ChunkDescriptor>) -> Self {
        self.chunks = chunks;
        self
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        match self.container_version {
            AuraContainerVersion::V2 => self.encode_v2(),
            AuraContainerVersion::LegacyV1 | AuraContainerVersion::V3 => Err(
                AuraError::UnsupportedVersion(self.container_version.wire_value()),
            ),
        }
    }

    fn encode_v2(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        out.extend_from_slice(FOOTER_MAGIC);
        put_u16_le(&mut out, self.container_version.wire_value());
        put_u8(&mut out, self.compression.kind as u8);
        put_u8(&mut out, self.compression.level);
        encode_schema_block(&self.schema, &mut out)?;
        encode_stats(&self.stats, &mut out)?;
        encode_plans(self, &mut out)?;
        encode_chunks(&self.chunks, &mut out)?;
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = ByteReader::new(bytes);
        if reader.read_exact(4)? != FOOTER_MAGIC {
            return Err(AuraError::InvalidMagic { expected: "AURF" });
        }
        let container_version = AuraContainerVersion::from_wire(reader.read_u16_le()?)?;
        match container_version {
            AuraContainerVersion::V2 => Self::decode_v2(reader, container_version),
            AuraContainerVersion::LegacyV1 => Err(AuraError::UnsupportedVersion(
                container_version.wire_value(),
            )),
            AuraContainerVersion::V3 => Self::decode_v3(container_version),
        }
    }

    fn decode_v3(container_version: AuraContainerVersion) -> Result<Self> {
        Err(AuraError::UnsupportedVersion(
            container_version.wire_value(),
        ))
    }

    fn decode_v2(
        mut reader: ByteReader<'_>,
        container_version: AuraContainerVersion,
    ) -> Result<Self> {
        let compression = CompressionDescriptor {
            kind: CompressionKind::from_code(reader.read_u8()?)?,
            level: reader.read_u8()?,
        };
        let schema = decode_schema_block(&mut reader)?;
        let stats = decode_stats(&mut reader)?;
        let (aura0_plan, aura1_plan, generic_aura0_plan) = decode_plans(&mut reader)?;
        let chunks = decode_chunks(&mut reader)?;
        reader.finish()?;
        if let Some(plan) = &generic_aura0_plan {
            validate_generic_plan_schema_authorization(&schema, plan)?;
        }

        Ok(Self {
            container_version,
            schema,
            stats,
            compression,
            aura0_plan,
            aura1_plan,
            generic_aura0_plan,
            chunks,
        })
    }
}

fn encode_stats(stats: &IngestStats, out: &mut Vec<u8>) -> Result<()> {
    put_u64_le(out, stats.record_count);
    put_u16_len(out, stats.fields.len(), "stats field count")?;
    for field in &stats.fields {
        put_u16_le(out, field.field_index);
        put_u64_le(out, field.observed);
        put_i64_le(out, field.min);
        put_i64_le(out, field.max);
        put_u64_le(out, field.max_abs_delta);
        put_u8(out, field.monotonic_non_decreasing as u8);
        put_u8(out, field.first_value.is_some() as u8);
        put_i64_le(out, field.first_value.unwrap_or(0));
        put_u8(out, field.fixed_step.is_some() as u8);
        put_i64_le(out, field.fixed_step.unwrap_or(0));
        put_u8(out, field.fixed_step_valid as u8);
        put_u8(
            out,
            (field.delta_valid as u8) | ((field.delta2_valid as u8) << 1),
        );
    }
    put_u16_len(out, stats.related_fields.len(), "related stats count")?;
    for related in &stats.related_fields {
        put_u16_le(out, related.field_index);
        put_u16_le(out, related.related_field_index);
        put_u64_le(out, related.observed);
        put_i64_le(out, related.min_delta);
        put_i64_le(out, related.max_delta);
        put_u64_le(out, related.max_abs_delta);
        put_u8(out, related.delta_valid as u8);
    }
    put_u32_le(out, stats.shape.max_records_per_timestamp);
    put_u32_len(
        out,
        stats.shape.timestamp_run_histogram.len(),
        "timestamp run histogram count",
    )?;
    for entry in &stats.shape.timestamp_run_histogram {
        put_u32_le(out, entry.run_len);
        put_u64_le(out, entry.count);
    }
    Ok(())
}

fn decode_stats(reader: &mut ByteReader<'_>) -> Result<IngestStats> {
    let record_count = reader.read_u64_le()?;
    let field_count = reader.read_u16_le()? as usize;
    let mut fields = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        let field_index = reader.read_u16_le()?;
        let observed = reader.read_u64_le()?;
        let min = reader.read_i64_le()?;
        let max = reader.read_i64_le()?;
        let max_abs_delta = reader.read_u64_le()?;
        let monotonic_non_decreasing = reader.read_u8()? != 0;
        let first_value = if reader.read_u8()? != 0 {
            Some(reader.read_i64_le()?)
        } else {
            let _unused = reader.read_i64_le()?;
            None
        };
        let fixed_step = if reader.read_u8()? != 0 {
            Some(reader.read_i64_le()?)
        } else {
            let _unused = reader.read_i64_le()?;
            None
        };
        let fixed_step_valid = reader.read_u8()? != 0;
        let flags = reader.read_u8()?;
        fields.push(FieldStats::from_summary(FieldStatsSummary {
            field_index,
            observed,
            min,
            max,
            max_abs_delta,
            delta_valid: flags & 1 != 0,
            monotonic_non_decreasing,
            first_value,
            fixed_step,
            fixed_step_valid,
            delta2_valid: flags & 2 != 0,
        }));
    }
    let related_count = reader.read_u16_le()? as usize;
    let mut related_fields = Vec::with_capacity(related_count);
    for _ in 0..related_count {
        related_fields.push(RelatedFieldStats {
            field_index: reader.read_u16_le()?,
            related_field_index: reader.read_u16_le()?,
            observed: reader.read_u64_le()?,
            min_delta: reader.read_i64_le()?,
            max_delta: reader.read_i64_le()?,
            max_abs_delta: reader.read_u64_le()?,
            delta_valid: reader.read_u8()? != 0,
        });
    }
    let max_records_per_timestamp = reader.read_u32_le()?;
    let histogram_count = reader.read_u32_le()? as usize;
    let mut timestamp_run_histogram = Vec::with_capacity(histogram_count);
    for _ in 0..histogram_count {
        timestamp_run_histogram.push(RunHistogramEntry {
            run_len: reader.read_u32_le()?,
            count: reader.read_u64_le()?,
        });
    }
    Ok(IngestStats {
        record_count,
        fields,
        related_fields,
        shape: ShapeStats {
            max_records_per_timestamp,
            timestamp_run_histogram,
        },
    })
}

fn encode_plans(footer: &AuraFooter, out: &mut Vec<u8>) -> Result<()> {
    let plan_count = footer.aura0_plan.is_some() as usize
        + footer.aura1_plan.is_some() as usize
        + footer.generic_aura0_plan.is_some() as usize;
    put_u8(out, plan_count as u8);
    if let Some(plan) = &footer.aura0_plan {
        put_u8(out, 0);
        put_u16_len(out, plan.fields.len(), "Aura0 plan field count")?;
        for field in &plan.fields {
            encode_plan_field(*field, out);
        }
    }
    if let Some(plan) = &footer.aura1_plan {
        put_u8(out, 1);
        put_u16_le(out, plan.block_capacity);
        put_u16_len(out, plan.fields.len(), "Aura1 plan field count")?;
        for field in &plan.fields {
            encode_plan_field(*field, out);
        }
    }
    if let Some(plan) = &footer.generic_aura0_plan {
        put_u8(out, 2);
        let bytes = plan.encode()?;
        put_u32_len(out, bytes.len(), "generic Aura0 plan length")?;
        out.extend_from_slice(&bytes);
    }
    Ok(())
}

fn decode_plans(
    reader: &mut ByteReader<'_>,
) -> Result<(
    Option<Aura0Plan>,
    Option<Aura1Plan>,
    Option<GenericInstructionPlan>,
)> {
    let plan_count = reader.read_u8()?;
    let mut aura0_plan = None;
    let mut aura1_plan = None;
    let mut generic_aura0_plan = None;
    for _ in 0..plan_count {
        match reader.read_u8()? {
            0 => {
                let field_count = reader.read_u16_le()? as usize;
                let fields = decode_plan_fields(reader, field_count)?;
                aura0_plan = Some(Aura0Plan { fields });
            }
            1 => {
                let block_capacity = reader.read_u16_le()?;
                let field_count = reader.read_u16_le()? as usize;
                let fields = decode_plan_fields(reader, field_count)?;
                aura1_plan = Some(Aura1Plan {
                    block_capacity,
                    fields,
                });
            }
            2 => {
                let len = reader.read_u32_le()? as usize;
                generic_aura0_plan = Some(GenericInstructionPlan::decode(reader.read_exact(len)?)?);
            }
            _ => return Err(AuraError::InvalidValue("plan kind")),
        }
    }
    Ok((aura0_plan, aura1_plan, generic_aura0_plan))
}

fn encode_plan_field(field: PhysicalFieldPlan, out: &mut Vec<u8>) {
    put_u16_le(out, field.field_index);
    put_u8(out, field.encoding as u8);
    put_u8(out, field.width.code());
    put_u8(out, field.bit_width);
    put_u16_le(out, field.reference_field_index.unwrap_or(u16::MAX));
    put_i64_le(out, field.base_value);
    put_i64_le(out, field.step);
    put_u64_le(out, field.estimated_bytes);
}

fn decode_plan_fields(
    reader: &mut ByteReader<'_>,
    field_count: usize,
) -> Result<Vec<PhysicalFieldPlan>> {
    let mut fields = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        fields.push(PhysicalFieldPlan {
            field_index: reader.read_u16_le()?,
            encoding: FieldEncoding::from_code(reader.read_u8()?)?,
            width: PhysicalWidth::from_code(reader.read_u8()?)?,
            bit_width: reader.read_u8()?,
            reference_field_index: match reader.read_u16_le()? {
                u16::MAX => None,
                index => Some(index),
            },
            base_value: reader.read_i64_le()?,
            step: reader.read_i64_le()?,
            estimated_bytes: reader.read_u64_le()?,
        });
    }
    Ok(fields)
}

fn encode_chunks(chunks: &[ChunkDescriptor], out: &mut Vec<u8>) -> Result<()> {
    if chunks.len() > MAX_AURA_CHUNK_COUNT {
        return Err(AuraError::InvalidValue("chunk count"));
    }
    put_u32_len(out, chunks.len(), "chunk count")?;
    for chunk in chunks {
        put_u32_le(out, chunk.chunk_id);
        put_u64_le(out, chunk.first_event_index);
        put_u32_le(out, chunk.event_count);
        put_u64_le(out, chunk.compressed_offset);
        put_u64_le(out, chunk.compressed_len);
        put_u64_le(out, chunk.uncompressed_len);
        put_u64_le(out, chunk.first_ts_event);
        put_u64_le(out, chunk.last_ts_event);
        put_u64_le(out, chunk.first_sequence);
        put_u64_le(out, chunk.last_sequence);
        put_u32_le(out, chunk.checksum);
    }
    Ok(())
}

fn decode_chunks(reader: &mut ByteReader<'_>) -> Result<Vec<ChunkDescriptor>> {
    let chunk_count = reader.read_u32_le()? as usize;
    if chunk_count > MAX_AURA_CHUNK_COUNT {
        return Err(AuraError::InvalidValue("chunk count"));
    }
    let descriptor_bytes = chunk_count
        .checked_mul(AURA_CHUNK_DESCRIPTOR_SIZE)
        .ok_or(AuraError::InvalidValue("chunk descriptor bytes"))?;
    if descriptor_bytes > reader.remaining() {
        return Err(AuraError::UnexpectedEof);
    }
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(chunk_count)
        .map_err(|_| AuraError::InvalidValue("chunk descriptor allocation"))?;
    for _ in 0..chunk_count {
        chunks.push(ChunkDescriptor {
            chunk_id: reader.read_u32_le()?,
            first_event_index: reader.read_u64_le()?,
            event_count: reader.read_u32_le()?,
            compressed_offset: reader.read_u64_le()?,
            compressed_len: reader.read_u64_le()?,
            uncompressed_len: reader.read_u64_le()?,
            first_ts_event: reader.read_u64_le()?,
            last_ts_event: reader.read_u64_le()?,
            first_sequence: reader.read_u64_le()?,
            last_sequence: reader.read_u64_le()?,
            checksum: reader.read_u32_le()?,
        });
    }
    Ok(chunks)
}

fn put_u16_len(out: &mut Vec<u8>, len: usize, name: &'static str) -> Result<()> {
    let len = u16::try_from(len).map_err(|_| AuraError::InvalidValue(name))?;
    put_u16_le(out, len);
    Ok(())
}

fn put_u32_len(out: &mut Vec<u8>, len: usize, name: &'static str) -> Result<()> {
    let len = u32::try_from(len).map_err(|_| AuraError::InvalidValue(name))?;
    put_u32_le(out, len);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::{GenericStreamInstruction, GenericStreamOp};
    use crate::schema::book_delta_schema;

    #[test]
    fn footer_round_trips_schema_stats_plans_and_chunks() {
        let schema = book_delta_schema().unwrap();
        let mut stats = IngestStats::new(schema.fields.len()).unwrap();
        stats.observe_record();
        stats.observe_i64(0, 1_000).unwrap();
        stats.observe_i64(1, 10).unwrap();
        stats.observe_timestamp_run(4);

        let chunk = ChunkDescriptor {
            chunk_id: 0,
            first_event_index: 0,
            event_count: 1,
            compressed_offset: 128,
            compressed_len: 64,
            uncompressed_len: 96,
            first_ts_event: 1_000,
            last_ts_event: 1_000,
            first_sequence: 10,
            last_sequence: 10,
            checksum: 7,
        };
        let footer = AuraFooter::new(schema, stats.clone())
            .with_compression(CompressionDescriptor::zstd(12))
            .with_aura0_plan(Aura0Plan::from_stats(&stats))
            .with_aura1_plan(Aura1Plan::from_stats(&stats, 4))
            .with_generic_aura0_plan(GenericInstructionPlan {
                streams: vec![GenericStreamInstruction {
                    stream_id: 0,
                    target_slot: Some(0),
                    op: GenericStreamOp::FixedStep {
                        base: 1_000,
                        step: 0,
                    },
                }],
                groups: Vec::new(),
            })
            .with_chunks(vec![chunk]);

        let encoded = footer.encode().unwrap();
        let decoded = AuraFooter::decode(&encoded).unwrap();

        assert_eq!(footer.schema.fields, decoded.schema.fields);
        assert_eq!(AuraContainerVersion::V2, decoded.container_version);
        assert_eq!(footer.stats, decoded.stats);
        assert_eq!(footer.compression, decoded.compression);
        assert_eq!(footer.aura0_plan, decoded.aura0_plan);
        assert_eq!(footer.aura1_plan, decoded.aura1_plan);
        assert_eq!(footer.generic_aura0_plan, decoded.generic_aura0_plan);
        assert_eq!(footer.chunks, decoded.chunks);
    }

    #[test]
    fn v3_footer_layout_is_an_explicit_unsupported_skeleton() {
        let schema = book_delta_schema().unwrap();
        let stats = IngestStats::new(schema.fields.len()).unwrap();
        let v2 = AuraFooter::new(schema.clone(), stats.clone())
            .encode()
            .unwrap();
        let mut advertised_v1 = v2.clone();
        advertised_v1[4..6]
            .copy_from_slice(&AuraContainerVersion::LegacyV1.wire_value().to_le_bytes());
        assert_eq!(
            AuraFooter::decode(&advertised_v1),
            Err(AuraError::UnsupportedVersion(1)),
            "legacy V1 support does not extend beyond the front header"
        );

        let mut advertised_v3 = v2;
        advertised_v3[4..6].copy_from_slice(&AuraContainerVersion::V3.wire_value().to_le_bytes());

        assert_eq!(
            AuraFooter::decode(&advertised_v3),
            Err(AuraError::UnsupportedVersion(3))
        );
        assert_eq!(
            AuraFooter::new(schema, stats)
                .with_container_version(AuraContainerVersion::V3)
                .encode(),
            Err(AuraError::UnsupportedVersion(3))
        );
    }

    #[test]
    fn ingest_chunk_table_is_bounded_before_allocation() {
        assert_eq!(AURA_CHUNK_DESCRIPTOR_SIZE, 76);

        let huge_count = u32::MAX.to_le_bytes();
        assert_eq!(
            decode_chunks(&mut ByteReader::new(&huge_count)),
            Err(AuraError::InvalidValue("chunk count"))
        );

        let mut truncated = 1u32.to_le_bytes().to_vec();
        truncated.resize(4 + AURA_CHUNK_DESCRIPTOR_SIZE - 1, 0);
        assert_eq!(
            decode_chunks(&mut ByteReader::new(&truncated)),
            Err(AuraError::UnexpectedEof)
        );
    }
}
