use crate::bytes::{put_i64_le, put_u16_le, put_u32_le, put_u64_le, put_u8, ByteReader};
use crate::chunk::ChunkDescriptor;
use crate::footer::{CompressionDescriptor, CompressionKind};
use crate::format::FORMAT_VERSION;
use crate::plan::{Aura0Plan, Aura1Plan, FieldEncoding, PhysicalFieldPlan};
use crate::schema::{
    FieldDescriptor, FieldRelation, FieldRole, FieldType, SchemaDescriptor, TransformCandidates,
};
use crate::stats::PhysicalWidth;
use crate::{AuraError, Profile, Result};

pub const COMPILED_FOOTER_MAGIC: &[u8; 4] = b"AURP";
pub const FIELD_AUX_EXTENDED: u8 = 7;

const OP_MASK: u16 = 0b1_1111;
const WIDTH_SHIFT: u16 = 5;
const CONST_WIDTH_SHIFT: u16 = 8;
const AUX_SHIFT: u16 = 11;
const BASE_FLAG: u16 = 1 << 14;
const STEP_FLAG: u16 = 1 << 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProgramOp {
    Absolute = 0,
    DeltaBase = 1,
    DeltaPrevious = 2,
    DeltaRelated = 3,
    FixedStep = 4,
}

impl ProgramOp {
    pub fn from_code(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Absolute),
            1 => Ok(Self::DeltaBase),
            2 => Ok(Self::DeltaPrevious),
            3 => Ok(Self::DeltaRelated),
            4 => Ok(Self::FixedStep),
            _ => Err(AuraError::InvalidValue("program op")),
        }
    }

    pub const fn to_encoding(self) -> FieldEncoding {
        match self {
            Self::Absolute => FieldEncoding::Absolute,
            Self::DeltaBase => FieldEncoding::DeltaBase,
            Self::DeltaPrevious => FieldEncoding::DeltaPrevious,
            Self::DeltaRelated => FieldEncoding::DeltaRelated,
            Self::FixedStep => FieldEncoding::ImplicitFixedStep,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldCode(u16);

impl FieldCode {
    pub fn new(
        op: ProgramOp,
        width: PhysicalWidth,
        const_width: PhysicalWidth,
        aux: u8,
        has_base: bool,
        has_step: bool,
    ) -> Result<Self> {
        if aux > FIELD_AUX_EXTENDED {
            return Err(AuraError::InvalidValue("field code aux"));
        }
        let mut raw = u16::from(op as u8);
        raw |= u16::from(width.code()) << WIDTH_SHIFT;
        raw |= u16::from(const_width.code()) << CONST_WIDTH_SHIFT;
        raw |= u16::from(aux) << AUX_SHIFT;
        if has_base {
            raw |= BASE_FLAG;
        }
        if has_step {
            raw |= STEP_FLAG;
        }
        Ok(Self(raw))
    }

    pub fn from_raw(raw: u16) -> Result<Self> {
        let code = Self(raw);
        let _op = code.op()?;
        let _width = code.width()?;
        let _const_width = code.const_width()?;
        Ok(code)
    }

    pub const fn raw(self) -> u16 {
        self.0
    }

    pub fn op(self) -> Result<ProgramOp> {
        ProgramOp::from_code((self.0 & OP_MASK) as u8)
    }

    pub fn width(self) -> Result<PhysicalWidth> {
        PhysicalWidth::from_code(((self.0 >> WIDTH_SHIFT) & 0b111) as u8)
    }

    pub fn const_width(self) -> Result<PhysicalWidth> {
        PhysicalWidth::from_code(((self.0 >> CONST_WIDTH_SHIFT) & 0b111) as u8)
    }

    pub const fn aux(self) -> u8 {
        ((self.0 >> AUX_SHIFT) & 0b111) as u8
    }

    pub const fn has_base(self) -> bool {
        self.0 & BASE_FLAG != 0
    }

    pub const fn has_step(self) -> bool {
        self.0 & STEP_FLAG != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldProgram {
    pub code: FieldCode,
    pub reference_field_index: Option<u16>,
    pub base_value: Option<i64>,
    pub step: Option<i64>,
}

impl FieldProgram {
    pub fn from_plan(field: PhysicalFieldPlan) -> Result<Self> {
        let op = match field.encoding {
            FieldEncoding::Absolute => ProgramOp::Absolute,
            FieldEncoding::DeltaBase => ProgramOp::DeltaBase,
            FieldEncoding::DeltaPrevious => ProgramOp::DeltaPrevious,
            FieldEncoding::DeltaRelated => ProgramOp::DeltaRelated,
            FieldEncoding::TimestampStep | FieldEncoding::ImplicitFixedStep => ProgramOp::FixedStep,
        };
        let has_base = matches!(
            op,
            ProgramOp::DeltaBase | ProgramOp::DeltaPrevious | ProgramOp::FixedStep
        );
        let has_step = matches!(op, ProgramOp::FixedStep);
        let reference_field_index = field
            .reference_field_index
            .filter(|_| op == ProgramOp::DeltaRelated);
        let aux = reference_field_index
            .and_then(|index| u8::try_from(index).ok())
            .filter(|index| *index < FIELD_AUX_EXTENDED)
            .unwrap_or(if reference_field_index.is_some() {
                FIELD_AUX_EXTENDED
            } else {
                0
            });
        let const_width = if has_base || has_step {
            PhysicalWidth::I64
        } else {
            PhysicalWidth::Zero
        };
        Ok(Self {
            code: FieldCode::new(op, field.width, const_width, aux, has_base, has_step)?,
            reference_field_index,
            base_value: has_base.then_some(field.base_value),
            step: has_step.then_some(field.step),
        })
    }

    pub fn to_plan(self, field_index: u16) -> Result<PhysicalFieldPlan> {
        let op = self.code.op()?;
        Ok(PhysicalFieldPlan {
            field_index,
            encoding: op.to_encoding(),
            width: self.code.width()?,
            reference_field_index: self.reference_field_index,
            base_value: self.base_value.unwrap_or(0),
            step: self.step.unwrap_or(0),
            estimated_bytes: 0,
        })
    }

    pub fn encode(self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.encode_to(&mut out)?;
        Ok(out)
    }

    pub fn encode_to(self, out: &mut Vec<u8>) -> Result<()> {
        put_u16_le(out, self.code.raw());
        if self.code.aux() == FIELD_AUX_EXTENDED {
            put_u16_le(
                out,
                self.reference_field_index
                    .ok_or(AuraError::InvalidValue("extended reference field"))?,
            );
        }
        if self.code.has_base() {
            write_const(
                out,
                self.base_value
                    .ok_or(AuraError::InvalidValue("base value"))?,
                self.code.const_width()?,
            )?;
        }
        if self.code.has_step() {
            write_const(
                out,
                self.step.ok_or(AuraError::InvalidValue("step"))?,
                self.code.const_width()?,
            )?;
        }
        Ok(())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = ByteReader::new(bytes);
        let program = Self::decode_from(&mut reader)?;
        reader.finish()?;
        Ok(program)
    }

    pub fn decode_from(reader: &mut ByteReader<'_>) -> Result<Self> {
        let code = FieldCode::from_raw(reader.read_u16_le()?)?;
        let reference_field_index = if code.aux() == FIELD_AUX_EXTENDED {
            Some(reader.read_u16_le()?)
        } else if code.op()? == ProgramOp::DeltaRelated {
            Some(u16::from(code.aux()))
        } else {
            None
        };
        let base_value = if code.has_base() {
            Some(read_const(reader, code.const_width()?)?)
        } else {
            None
        };
        let step = if code.has_step() {
            Some(read_const(reader, code.const_width()?)?)
        } else {
            None
        };
        Ok(Self {
            code,
            reference_field_index,
            base_value,
            step,
        })
    }

    pub fn encoded_len(self) -> Result<usize> {
        Ok(self.encode()?.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeProgram {
    pub fields: Vec<FieldProgram>,
}

impl DecodeProgram {
    pub fn from_aura0_plan(plan: &Aura0Plan, field_count: usize) -> Result<Self> {
        let mut fields = Vec::with_capacity(field_count);
        for index in 0..field_count {
            let field = plan
                .fields
                .iter()
                .find(|field| usize::from(field.field_index) == index)
                .ok_or(AuraError::InvalidValue("program field"))?;
            fields.push(FieldProgram::from_plan(*field)?);
        }
        Ok(Self { fields })
    }

    pub fn from_aura1_plan(plan: &Aura1Plan, field_count: usize) -> Result<Self> {
        let mut fields = Vec::with_capacity(field_count);
        for index in 0..field_count {
            let field = plan
                .fields
                .iter()
                .find(|field| usize::from(field.field_index) == index)
                .ok_or(AuraError::InvalidValue("program field"))?;
            fields.push(FieldProgram::from_plan(*field)?);
        }
        Ok(Self { fields })
    }

    pub fn to_aura0_plan(&self) -> Result<Aura0Plan> {
        Ok(Aura0Plan {
            fields: self.to_physical_fields()?,
        })
    }

    pub fn to_aura1_plan(&self, block_capacity: u16) -> Result<Aura1Plan> {
        Ok(Aura1Plan {
            block_capacity,
            fields: self.to_physical_fields()?,
        })
    }

    fn to_physical_fields(&self) -> Result<Vec<PhysicalFieldPlan>> {
        self.fields
            .iter()
            .enumerate()
            .map(|(idx, field)| field.to_plan(idx as u16))
            .collect()
    }

    pub fn encode_to(&self, out: &mut Vec<u8>) -> Result<()> {
        put_u16_len(out, self.fields.len(), "program field count")?;
        for field in &self.fields {
            field.encode_to(out)?;
        }
        Ok(())
    }

    pub fn decode_from(reader: &mut ByteReader<'_>) -> Result<Self> {
        let field_count = reader.read_u16_le()? as usize;
        let mut fields = Vec::with_capacity(field_count);
        for _ in 0..field_count {
            fields.push(FieldProgram::decode_from(reader)?);
        }
        Ok(Self { fields })
    }

    pub fn encoded_len(&self) -> Result<usize> {
        let mut out = Vec::new();
        self.encode_to(&mut out)?;
        Ok(out.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledFooter {
    pub profile: Profile,
    pub schema: SchemaDescriptor,
    pub compression: CompressionDescriptor,
    pub record_count: u64,
    pub block_capacity: u16,
    pub program: DecodeProgram,
    pub chunks: Vec<ChunkDescriptor>,
}

impl CompiledFooter {
    pub fn new(
        profile: Profile,
        schema: SchemaDescriptor,
        record_count: u64,
        block_capacity: u16,
        program: DecodeProgram,
    ) -> Result<Self> {
        if profile == Profile::Ingest {
            return Err(AuraError::InvalidValue("compiled footer profile"));
        }
        Ok(Self {
            profile,
            schema,
            compression: CompressionDescriptor::none(),
            record_count,
            block_capacity,
            program,
            chunks: Vec::new(),
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        out.extend_from_slice(COMPILED_FOOTER_MAGIC);
        put_u16_le(&mut out, FORMAT_VERSION);
        put_u8(&mut out, self.compression.kind as u8);
        put_u8(&mut out, self.compression.level);
        put_u8(&mut out, self.profile as u8);
        put_u64_le(&mut out, self.record_count);
        put_u16_le(&mut out, self.block_capacity);
        encode_schema(&self.schema, &mut out)?;
        self.program.encode_to(&mut out)?;
        encode_chunks(&self.chunks, &mut out)?;
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = ByteReader::new(bytes);
        if reader.read_exact(4)? != COMPILED_FOOTER_MAGIC {
            return Err(AuraError::InvalidMagic { expected: "AURP" });
        }
        let version = reader.read_u16_le()?;
        if version != FORMAT_VERSION {
            return Err(AuraError::UnsupportedVersion(version));
        }
        let compression = CompressionDescriptor {
            kind: CompressionKind::from_code(reader.read_u8()?)?,
            level: reader.read_u8()?,
        };
        let profile = Profile::from_byte(reader.read_u8()?)?;
        if profile == Profile::Ingest {
            return Err(AuraError::InvalidValue("compiled footer profile"));
        }
        let record_count = reader.read_u64_le()?;
        let block_capacity = reader.read_u16_le()?;
        let schema = decode_schema(&mut reader)?;
        let program = DecodeProgram::decode_from(&mut reader)?;
        let chunks = decode_chunks(&mut reader)?;
        reader.finish()?;
        Ok(Self {
            profile,
            schema,
            compression,
            record_count,
            block_capacity,
            program,
            chunks,
        })
    }
}

fn write_const(out: &mut Vec<u8>, value: i64, width: PhysicalWidth) -> Result<()> {
    match width {
        PhysicalWidth::Zero => {
            if value == 0 {
                Ok(())
            } else {
                Err(AuraError::InvalidValue("zero-width const"))
            }
        }
        PhysicalWidth::I8 => {
            let value = i8::try_from(value).map_err(|_| AuraError::InvalidValue("i8 const"))?;
            out.push(value as u8);
            Ok(())
        }
        PhysicalWidth::I16 => {
            let value = i16::try_from(value).map_err(|_| AuraError::InvalidValue("i16 const"))?;
            out.extend_from_slice(&value.to_le_bytes());
            Ok(())
        }
        PhysicalWidth::I32 => {
            let value = i32::try_from(value).map_err(|_| AuraError::InvalidValue("i32 const"))?;
            out.extend_from_slice(&value.to_le_bytes());
            Ok(())
        }
        PhysicalWidth::I64 => {
            put_i64_le(out, value);
            Ok(())
        }
    }
}

fn read_const(reader: &mut ByteReader<'_>, width: PhysicalWidth) -> Result<i64> {
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
