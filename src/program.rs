use crate::bytes::{put_i64_le, put_u16_le, put_u32_le, put_u64_le, put_u8, ByteReader};
use crate::chunk::ChunkDescriptor;
use crate::footer::{CompressionDescriptor, CompressionKind};
use crate::format::FORMAT_VERSION;
use crate::plan::{Aura0Plan, Aura1Plan, FieldEncoding, PhysicalFieldPlan};
use crate::schema::{decode_schema_block, encode_schema_block, SchemaDescriptor};
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
    BitpackedDeltaPrevious = 5,
    BitpackedDeltaBase = 6,
    BitpackedDeltaRelated = 7,
    DerivedOffset = 8,
    BitpackedDeltaRelatedOffset = 9,
    BitpackedDeltaPreviousOffset = 10,
    BitpackedDeltaPreviousFieldOffset = 11,
    BitpackedCandleMaxOffset = 12,
    BitpackedCandleMinOffset = 13,
    BitpackedProductResidual = 14,
    BitpackedProportionalResidual = 15,
}

impl ProgramOp {
    pub fn from_code(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Absolute),
            1 => Ok(Self::DeltaBase),
            2 => Ok(Self::DeltaPrevious),
            3 => Ok(Self::DeltaRelated),
            4 => Ok(Self::FixedStep),
            5 => Ok(Self::BitpackedDeltaPrevious),
            6 => Ok(Self::BitpackedDeltaBase),
            7 => Ok(Self::BitpackedDeltaRelated),
            8 => Ok(Self::DerivedOffset),
            9 => Ok(Self::BitpackedDeltaRelatedOffset),
            10 => Ok(Self::BitpackedDeltaPreviousOffset),
            11 => Ok(Self::BitpackedDeltaPreviousFieldOffset),
            12 => Ok(Self::BitpackedCandleMaxOffset),
            13 => Ok(Self::BitpackedCandleMinOffset),
            14 => Ok(Self::BitpackedProductResidual),
            15 => Ok(Self::BitpackedProportionalResidual),
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
            Self::BitpackedDeltaPrevious => FieldEncoding::BitpackedDeltaPrevious,
            Self::BitpackedDeltaBase => FieldEncoding::BitpackedDeltaBase,
            Self::BitpackedDeltaRelated => FieldEncoding::BitpackedDeltaRelated,
            Self::DerivedOffset => FieldEncoding::DerivedOffset,
            Self::BitpackedDeltaRelatedOffset => FieldEncoding::BitpackedDeltaRelatedOffset,
            Self::BitpackedDeltaPreviousOffset => FieldEncoding::BitpackedDeltaPreviousOffset,
            Self::BitpackedDeltaPreviousFieldOffset => {
                FieldEncoding::BitpackedDeltaPreviousFieldOffset
            }
            Self::BitpackedCandleMaxOffset => FieldEncoding::BitpackedCandleMaxOffset,
            Self::BitpackedCandleMinOffset => FieldEncoding::BitpackedCandleMinOffset,
            Self::BitpackedProductResidual => FieldEncoding::BitpackedProductResidual,
            Self::BitpackedProportionalResidual => FieldEncoding::BitpackedProportionalResidual,
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
    pub bit_width: Option<u8>,
}

impl FieldProgram {
    pub fn from_plan(field: PhysicalFieldPlan) -> Result<Self> {
        let op = match field.encoding {
            FieldEncoding::Absolute => ProgramOp::Absolute,
            FieldEncoding::DeltaBase => ProgramOp::DeltaBase,
            FieldEncoding::DeltaPrevious => ProgramOp::DeltaPrevious,
            FieldEncoding::DeltaRelated => ProgramOp::DeltaRelated,
            FieldEncoding::TimestampStep | FieldEncoding::ImplicitFixedStep => ProgramOp::FixedStep,
            FieldEncoding::BitpackedDeltaPrevious => ProgramOp::BitpackedDeltaPrevious,
            FieldEncoding::BitpackedDeltaBase => ProgramOp::BitpackedDeltaBase,
            FieldEncoding::BitpackedDeltaRelated => ProgramOp::BitpackedDeltaRelated,
            FieldEncoding::DerivedOffset => ProgramOp::DerivedOffset,
            FieldEncoding::BitpackedDeltaRelatedOffset => ProgramOp::BitpackedDeltaRelatedOffset,
            FieldEncoding::BitpackedDeltaPreviousOffset => ProgramOp::BitpackedDeltaPreviousOffset,
            FieldEncoding::BitpackedDeltaPreviousFieldOffset => {
                ProgramOp::BitpackedDeltaPreviousFieldOffset
            }
            FieldEncoding::BitpackedCandleMaxOffset => ProgramOp::BitpackedCandleMaxOffset,
            FieldEncoding::BitpackedCandleMinOffset => ProgramOp::BitpackedCandleMinOffset,
            FieldEncoding::BitpackedProductResidual => ProgramOp::BitpackedProductResidual,
            FieldEncoding::BitpackedProportionalResidual => {
                ProgramOp::BitpackedProportionalResidual
            }
        };
        let has_base = matches!(
            op,
            ProgramOp::DeltaBase
                | ProgramOp::DeltaPrevious
                | ProgramOp::FixedStep
                | ProgramOp::BitpackedDeltaPrevious
                | ProgramOp::BitpackedDeltaBase
                | ProgramOp::DerivedOffset
                | ProgramOp::BitpackedDeltaRelatedOffset
                | ProgramOp::BitpackedDeltaPreviousOffset
                | ProgramOp::BitpackedDeltaPreviousFieldOffset
                | ProgramOp::BitpackedCandleMaxOffset
                | ProgramOp::BitpackedCandleMinOffset
                | ProgramOp::BitpackedProductResidual
                | ProgramOp::BitpackedProportionalResidual
        );
        let has_step = matches!(
            op,
            ProgramOp::FixedStep
                | ProgramOp::BitpackedDeltaPreviousOffset
                | ProgramOp::BitpackedDeltaPreviousFieldOffset
                | ProgramOp::BitpackedCandleMaxOffset
                | ProgramOp::BitpackedCandleMinOffset
                | ProgramOp::BitpackedProductResidual
                | ProgramOp::BitpackedProportionalResidual
        );
        let is_bitpacked = is_bitpacked_op(op);
        if is_bitpacked && field.bit_width > 64 {
            return Err(AuraError::InvalidValue("bit width"));
        }
        let reference_field_index = field.reference_field_index.filter(|_| {
            matches!(
                op,
                ProgramOp::DeltaRelated
                    | ProgramOp::BitpackedDeltaRelated
                    | ProgramOp::DerivedOffset
                    | ProgramOp::BitpackedDeltaRelatedOffset
                    | ProgramOp::BitpackedDeltaPreviousFieldOffset
                    | ProgramOp::BitpackedCandleMaxOffset
                    | ProgramOp::BitpackedCandleMinOffset
                    | ProgramOp::BitpackedProductResidual
                    | ProgramOp::BitpackedProportionalResidual
            )
        });
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
            bit_width: is_bitpacked.then_some(field.bit_width),
        })
    }

    pub fn to_plan(self, field_index: u16) -> Result<PhysicalFieldPlan> {
        let op = self.code.op()?;
        Ok(PhysicalFieldPlan {
            field_index,
            encoding: op.to_encoding(),
            width: self.code.width()?,
            bit_width: self.bit_width.unwrap_or(0),
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
        if is_bitpacked_op(self.code.op()?) {
            put_u8(
                out,
                self.bit_width.ok_or(AuraError::InvalidValue("bit width"))?,
            );
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
        } else if matches!(
            code.op()?,
            ProgramOp::DeltaRelated
                | ProgramOp::BitpackedDeltaRelated
                | ProgramOp::DerivedOffset
                | ProgramOp::BitpackedDeltaRelatedOffset
                | ProgramOp::BitpackedDeltaPreviousFieldOffset
                | ProgramOp::BitpackedCandleMaxOffset
                | ProgramOp::BitpackedCandleMinOffset
                | ProgramOp::BitpackedProductResidual
                | ProgramOp::BitpackedProportionalResidual
        ) {
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
        let bit_width = if is_bitpacked_op(code.op()?) {
            let bit_width = reader.read_u8()?;
            if bit_width > 64 {
                return Err(AuraError::InvalidValue("bit width"));
            }
            Some(bit_width)
        } else {
            None
        };
        Ok(Self {
            code,
            reference_field_index,
            base_value,
            step,
            bit_width,
        })
    }

    pub fn encoded_len(self) -> Result<usize> {
        Ok(self.encode()?.len())
    }
}

const fn is_bitpacked_op(op: ProgramOp) -> bool {
    matches!(
        op,
        ProgramOp::BitpackedDeltaPrevious
            | ProgramOp::BitpackedDeltaBase
            | ProgramOp::BitpackedDeltaRelated
            | ProgramOp::BitpackedDeltaRelatedOffset
            | ProgramOp::BitpackedDeltaPreviousOffset
            | ProgramOp::BitpackedDeltaPreviousFieldOffset
            | ProgramOp::BitpackedCandleMaxOffset
            | ProgramOp::BitpackedCandleMinOffset
            | ProgramOp::BitpackedProductResidual
            | ProgramOp::BitpackedProportionalResidual
    )
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
        encode_schema_block(&self.schema, &mut out)?;
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
        let schema = decode_schema_block(&mut reader)?;
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
        PhysicalWidth::I128 => {
            out.extend_from_slice(&i128::from(value).to_le_bytes());
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
        PhysicalWidth::I128 => {
            let bytes = reader.read_exact(16)?;
            let value = i128::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]);
            i64::try_from(value).map_err(|_| AuraError::InvalidValue("i128 const"))
        }
    }
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
