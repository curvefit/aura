use crate::bytes::{put_i64_le, put_u16_le, put_u32_le, put_u64_le, put_u8, ByteReader};
use crate::chunk::ChunkDescriptor;
use crate::format::FORMAT_VERSION;
use crate::plan::{Aura0Plan, Aura1Plan, FieldEncoding, PhysicalFieldPlan};
use crate::schema::{
    FieldDescriptor, FieldRelation, FieldRole, FieldType, SchemaDescriptor, TransformCandidates,
};
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
    pub schema: SchemaDescriptor,
    pub stats: IngestStats,
    pub compression: CompressionDescriptor,
    pub aura0_plan: Option<Aura0Plan>,
    pub aura1_plan: Option<Aura1Plan>,
    pub chunks: Vec<ChunkDescriptor>,
}

impl AuraFooter {
    pub fn new(schema: SchemaDescriptor, stats: IngestStats) -> Self {
        Self {
            schema,
            stats,
            compression: CompressionDescriptor::none(),
            aura0_plan: None,
            aura1_plan: None,
            chunks: Vec::new(),
        }
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

    pub fn with_chunks(mut self, chunks: Vec<ChunkDescriptor>) -> Self {
        self.chunks = chunks;
        self
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        out.extend_from_slice(FOOTER_MAGIC);
        put_u16_le(&mut out, FORMAT_VERSION);
        put_u8(&mut out, self.compression.kind as u8);
        put_u8(&mut out, self.compression.level);
        encode_schema(&self.schema, &mut out)?;
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
        let version = reader.read_u16_le()?;
        if version != FORMAT_VERSION {
            return Err(AuraError::UnsupportedVersion(version));
        }
        let compression = CompressionDescriptor {
            kind: CompressionKind::from_code(reader.read_u8()?)?,
            level: reader.read_u8()?,
        };
        let schema = decode_schema(&mut reader)?;
        let stats = decode_stats(&mut reader)?;
        let (aura0_plan, aura1_plan) = decode_plans(&mut reader)?;
        let chunks = decode_chunks(&mut reader)?;
        reader.finish()?;

        Ok(Self {
            schema,
            stats,
            compression,
            aura0_plan,
            aura1_plan,
            chunks,
        })
    }
}

fn encode_schema(schema: &SchemaDescriptor, out: &mut Vec<u8>) -> Result<()> {
    put_u32_le(out, schema.schema_id);
    put_string(out, &schema.name)?;
    put_u16_len(out, schema.fields.len(), "schema field count")?;
    for field in &schema.fields {
        put_u16_le(out, field.index);
        put_u8(out, field.field_type as u8);
        put_u8(out, field.role as u8);
        put_u8(out, field.nullable as u8);
        put_u8(out, field.relation.kind_code());
        put_u16_le(
            out,
            field.relation.related_field_index().unwrap_or(u16::MAX),
        );
        put_u16_le(out, field.candidates.bits());
        put_string(out, &field.name)?;
    }
    Ok(())
}

fn decode_schema(reader: &mut ByteReader<'_>) -> Result<SchemaDescriptor> {
    let schema_id = reader.read_u32_le()?;
    let name = read_string(reader)?;
    let field_count = reader.read_u16_le()? as usize;
    let mut fields = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        let index = reader.read_u16_le()?;
        let field_type = FieldType::from_code(reader.read_u8()?)?;
        let role = FieldRole::from_code(reader.read_u8()?)?;
        let nullable = reader.read_u8()? != 0;
        let relation_kind = reader.read_u8()?;
        let related_field_index = reader.read_u16_le()?;
        let candidates = TransformCandidates::from_bits(reader.read_u16_le()?)?;
        let name = read_string(reader)?;
        fields.push(FieldDescriptor {
            index,
            name,
            field_type,
            role,
            nullable,
            relation: FieldRelation::from_codes(relation_kind, related_field_index)?,
            candidates,
        });
    }
    Ok(SchemaDescriptor {
        schema_id,
        name,
        fields,
    })
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
        put_u8(out, 0);
    }
    put_u16_len(out, stats.related_fields.len(), "related stats count")?;
    for related in &stats.related_fields {
        put_u16_le(out, related.field_index);
        put_u16_le(out, related.related_field_index);
        put_u64_le(out, related.observed);
        put_i64_le(out, related.min_delta);
        put_i64_le(out, related.max_delta);
        put_u64_le(out, related.max_abs_delta);
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
        let _reserved = reader.read_u8()?;
        fields.push(FieldStats::from_summary(FieldStatsSummary {
            field_index,
            observed,
            min,
            max,
            max_abs_delta,
            monotonic_non_decreasing,
            first_value,
            fixed_step,
            fixed_step_valid,
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
    let plan_count = footer.aura0_plan.is_some() as usize + footer.aura1_plan.is_some() as usize;
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
    Ok(())
}

fn decode_plans(reader: &mut ByteReader<'_>) -> Result<(Option<Aura0Plan>, Option<Aura1Plan>)> {
    let plan_count = reader.read_u8()?;
    let mut aura0_plan = None;
    let mut aura1_plan = None;
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
            _ => return Err(AuraError::InvalidValue("plan kind")),
        }
    }
    Ok((aura0_plan, aura1_plan))
}

fn encode_plan_field(field: PhysicalFieldPlan, out: &mut Vec<u8>) {
    put_u16_le(out, field.field_index);
    put_u8(out, field.encoding as u8);
    put_u8(out, field.width.code());
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
    let mut chunks = Vec::with_capacity(chunk_count);
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

fn put_string(out: &mut Vec<u8>, value: &str) -> Result<()> {
    put_u16_len(out, value.len(), "string length")?;
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn read_string(reader: &mut ByteReader<'_>) -> Result<String> {
    let len = reader.read_u16_le()? as usize;
    let bytes = reader.read_exact(len)?;
    std::str::from_utf8(bytes)
        .map(|value| value.to_owned())
        .map_err(|_| AuraError::InvalidValue("utf8 string"))
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
            .with_chunks(vec![chunk]);

        let encoded = footer.encode().unwrap();
        let decoded = AuraFooter::decode(&encoded).unwrap();

        assert_eq!(footer, decoded);
    }
}
