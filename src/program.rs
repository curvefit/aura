use crate::bytes::{put_i64_le, put_u16_le, put_u32_le, put_u64_le, put_u8, ByteReader};
use crate::chunk::ChunkDescriptor;
use crate::footer::{CompressionDescriptor, CompressionKind};
use crate::format::{
    AuraContainerVersion, AURA_CHUNK_DESCRIPTOR_SIZE, DEFAULT_CONTAINER_VERSION,
    MAX_AURA_CHUNK_COUNT,
};
use crate::generic_planner::validate_generic_plan_schema_authorization;
use crate::instructions::GenericInstructionPlan;
use crate::plan::{Aura0Plan, Aura1Plan, FieldEncoding, PhysicalFieldPlan};
use crate::schema::{
    decode_schema_block, encode_schema_block, validate_schema_container_compatibility, AuraSchema,
    FieldType, SchemaDescriptor,
};
use crate::stats::PhysicalWidth;
use crate::{AuraError, Result};

pub const COMPILED_FOOTER_MAGIC: &[u8; 4] = b"AURP";
pub const AURA1_BYTE_LANE_MAGIC: &[u8; 4] = b"AUBL";
pub const AURA1_BYTE_LANE_VERSION: u8 = 1;
pub const BYTE_LANE_CODEC_RAW: u8 = 0;
pub const BYTE_LANE_CODEC_LZ4: u8 = 1;
pub const BYTE_LANE_CODEC_ZSTD: u8 = 2;
pub const BYTE_LANE_CHECKSUM_NONE: u8 = 0;
pub const BYTE_LANE_CHECKSUM_BYTE_GUARD: u8 = 1;
pub const FIELD_AUX_EXTENDED: u8 = 7;
/// Fixed wire size of one descriptor in the `AUBL` footer extension.
pub const AURA1_BYTE_LANE_DESCRIPTOR_SIZE: usize = 64;
/// Normative V2+ supported-subset security ceiling for all-memory lane tables.
/// Files above this limit are unsupported; there is no unsafe override.
pub const MAX_AURA1_BYTE_LANE_COUNT: usize = 65_536;
/// Normative V2+ supported-subset security ceiling for one lane's compressed
/// bytes in current all-memory APIs. There is no unsafe override.
pub const MAX_AURA1_BYTE_LANE_COMPRESSED_BYTES: u64 = 1 << 30;
/// Normative V2+ supported-subset security ceiling for one lane's decoded bytes
/// in current all-memory APIs. There is no unsafe override.
pub const MAX_AURA1_BYTE_LANE_OUTPUT_BYTES: u64 = 1 << 30;
/// Normative V2+ supported-subset security ceiling for both output span and
/// cumulative decoded bytes across all lanes in current all-memory APIs. There
/// is no unsafe override.
pub const MAX_AURA1_BYTE_LANES_TOTAL_OUTPUT_BYTES: u64 = 1 << 30;

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
    BitpackedMaxPlusResidual = 12,
    BitpackedMinMinusResidual = 13,
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
            12 => Ok(Self::BitpackedMaxPlusResidual),
            13 => Ok(Self::BitpackedMinMinusResidual),
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
            Self::BitpackedMaxPlusResidual => FieldEncoding::BitpackedMaxPlusResidual,
            Self::BitpackedMinMinusResidual => FieldEncoding::BitpackedMinMinusResidual,
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
            FieldEncoding::BitpackedMaxPlusResidual => ProgramOp::BitpackedMaxPlusResidual,
            FieldEncoding::BitpackedMinMinusResidual => ProgramOp::BitpackedMinMinusResidual,
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
                | ProgramOp::BitpackedMaxPlusResidual
                | ProgramOp::BitpackedMinMinusResidual
                | ProgramOp::BitpackedProductResidual
                | ProgramOp::BitpackedProportionalResidual
        );
        let has_step = matches!(
            op,
            ProgramOp::FixedStep
                | ProgramOp::BitpackedDeltaPreviousOffset
                | ProgramOp::BitpackedDeltaPreviousFieldOffset
                | ProgramOp::BitpackedMaxPlusResidual
                | ProgramOp::BitpackedMinMinusResidual
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
                    | ProgramOp::BitpackedMaxPlusResidual
                    | ProgramOp::BitpackedMinMinusResidual
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
                | ProgramOp::BitpackedMaxPlusResidual
                | ProgramOp::BitpackedMinMinusResidual
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
            | ProgramOp::BitpackedMaxPlusResidual
            | ProgramOp::BitpackedMinMinusResidual
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
pub struct CompiledAuraPlan {
    /// Explicit container metadata. This intentionally replaces the former
    /// experimental `format_version: u16` field so compiled plans cannot lose
    /// the distinction between recognized and implemented Aura versions.
    pub container_version: AuraContainerVersion,
    pub schema_hash: u32,
    pub record_count: usize,
    pub field_count: usize,
    pub block_capacity: u16,
    pub aura0_plan: Aura0Plan,
    pub aura1_plan: Aura1Plan,
    pub generic_aura0_plan: Option<GenericInstructionPlan>,
    pub aura1_record_width: usize,
    pub aura1_body_size: usize,
    pub canonical_field_order: Vec<u16>,
    pub conversion_plan_hash: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompiledAuraField {
    pub field_index: u16,
    pub offset: usize,
    pub width: usize,
}

impl CompiledAuraPlan {
    pub fn from_footer(footer: &CompiledFooter) -> Result<Self> {
        footer
            .container_version
            .require_supported_container_layout()?;
        let field_count = footer.schema.fields.len();
        let record_count = usize::try_from(footer.record_count)
            .map_err(|_| AuraError::InvalidValue("record count"))?;
        let aura0_plan = footer.aura0_program.to_aura0_plan()?;
        let aura1_plan = footer.aura1_program.to_aura1_plan(footer.block_capacity)?;
        validate_plan_fields(&aura0_plan.fields, field_count)?;
        validate_plan_fields(&aura1_plan.fields, field_count)?;
        let aura1_record_width = aura1_plan
            .fields
            .iter()
            .map(|field| usize::from(field.width.byte_width()))
            .try_fold(0usize, |acc, width| {
                acc.checked_add(width)
                    .ok_or(AuraError::InvalidValue("body length"))
            })?;
        let aura1_body_size = record_count
            .checked_mul(aura1_record_width)
            .ok_or(AuraError::InvalidValue("body length"))?;
        let canonical_field_order = (0..field_count)
            .map(|index| u16::try_from(index).map_err(|_| AuraError::InvalidValue("field index")))
            .collect::<Result<Vec<_>>>()?;
        let footer_bytes = footer.encode()?;
        Ok(Self {
            container_version: footer.container_version,
            schema_hash: footer.schema.schema_id,
            record_count,
            field_count,
            block_capacity: footer.block_capacity,
            aura0_plan,
            aura1_plan,
            generic_aura0_plan: footer.generic_aura0_plan.clone(),
            aura1_record_width,
            aura1_body_size,
            canonical_field_order,
            conversion_plan_hash: plan_hash_bytes(&footer_bytes),
        })
    }

    pub fn from_schema(schema: &AuraSchema) -> Result<Self> {
        let descriptor = schema.descriptor();
        let field_count = descriptor.fields.len();
        if field_count == 0 {
            return Err(AuraError::InvalidValue("schema fields"));
        }
        let fields = descriptor
            .fields
            .iter()
            .map(|field| {
                let width = physical_width_for_field_type(field.field_type)?;
                Ok(PhysicalFieldPlan {
                    field_index: field.index,
                    encoding: FieldEncoding::Absolute,
                    width,
                    bit_width: 0,
                    reference_field_index: None,
                    base_value: 0,
                    step: 0,
                    estimated_bytes: 0,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        validate_plan_fields(&fields, field_count)?;
        let aura0_plan = Aura0Plan {
            fields: fields.clone(),
        };
        let aura1_plan = Aura1Plan {
            block_capacity: 1,
            fields,
        };
        let aura1_record_width = aura1_plan
            .fields
            .iter()
            .map(|field| usize::from(field.width.byte_width()))
            .sum();
        let canonical_field_order = (0..field_count)
            .map(|index| u16::try_from(index).map_err(|_| AuraError::InvalidValue("field index")))
            .collect::<Result<Vec<_>>>()?;
        let mut schema_bytes = Vec::new();
        encode_schema_block(descriptor, &mut schema_bytes)?;
        Ok(Self {
            container_version: DEFAULT_CONTAINER_VERSION,
            schema_hash: descriptor.schema_id,
            record_count: 0,
            field_count,
            block_capacity: 1,
            aura0_plan,
            aura1_plan,
            generic_aura0_plan: None,
            aura1_record_width,
            aura1_body_size: 0,
            canonical_field_order,
            conversion_plan_hash: plan_hash_bytes(&schema_bytes),
        })
    }

    pub fn aura1_field_offsets(&self) -> Vec<CompiledAuraField> {
        let mut offset = 0usize;
        self.aura1_plan
            .fields
            .iter()
            .map(|field| {
                let width = usize::from(field.width.byte_width());
                let compiled = CompiledAuraField {
                    field_index: field.field_index,
                    offset,
                    width,
                };
                offset += width;
                compiled
            })
            .collect()
    }

    pub fn aura1_field_offset(&self, field_index: u16) -> Option<CompiledAuraField> {
        self.aura1_field_offsets()
            .into_iter()
            .find(|field| field.field_index == field_index)
    }

    pub const fn container_version(&self) -> AuraContainerVersion {
        self.container_version
    }

    /// Numeric compatibility accessor for callers of the former experimental
    /// `format_version` field.
    pub const fn format_version(&self) -> u16 {
        self.container_version.wire_value()
    }
}

fn physical_width_for_field_type(field_type: FieldType) -> Result<PhysicalWidth> {
    match field_type {
        FieldType::I8 | FieldType::U8 => Ok(PhysicalWidth::I8),
        FieldType::I16 | FieldType::U16 => Ok(PhysicalWidth::I16),
        FieldType::I32 | FieldType::U32 => Ok(PhysicalWidth::I32),
        FieldType::I64 | FieldType::U64 | FieldType::TimestampNs => Ok(PhysicalWidth::I64),
        FieldType::I128 | FieldType::Opaque16 => Ok(PhysicalWidth::I128),
        FieldType::TimestampMs | FieldType::Utf8 | FieldType::DecimalText => {
            Err(AuraError::InvalidValue("v3-only field type"))
        }
    }
}

fn validate_plan_fields(fields: &[PhysicalFieldPlan], field_count: usize) -> Result<()> {
    if fields.len() != field_count {
        return Err(AuraError::InvalidValue("program field count"));
    }
    let mut seen = vec![false; field_count];
    for field in fields {
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

fn plan_hash_bytes(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |acc, byte| {
        acc.wrapping_mul(0x100000001b3)
            .wrapping_add(u64::from(*byte))
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aura1ByteLaneDescriptor {
    pub lane_version: u8,
    pub codec_id: u8,
    pub codec_level: u8,
    pub block_index: u32,
    pub row_start: u64,
    pub row_count: u32,
    pub aura1_output_offset: u64,
    pub uncompressed_len: u64,
    pub compressed_offset: u64,
    pub compressed_len: u64,
    pub checksum_kind: u8,
    pub checksum: u64,
    pub flags: u32,
}

pub(crate) fn validate_aura1_byte_lane_limits(lanes: &[Aura1ByteLaneDescriptor]) -> Result<usize> {
    if lanes.len() > MAX_AURA1_BYTE_LANE_COUNT {
        return Err(AuraError::InvalidValue("byte lane count"));
    }
    let mut output_span = 0u64;
    let mut cumulative_output = 0u64;
    for lane in lanes {
        if lane.compressed_len > MAX_AURA1_BYTE_LANE_COMPRESSED_BYTES {
            return Err(AuraError::InvalidValue("byte lane compressed length"));
        }
        if lane.uncompressed_len > MAX_AURA1_BYTE_LANE_OUTPUT_BYTES {
            return Err(AuraError::InvalidValue("byte lane output length"));
        }
        let output_end = lane
            .aura1_output_offset
            .checked_add(lane.uncompressed_len)
            .ok_or(AuraError::InvalidValue("byte lane output range"))?;
        if output_end > MAX_AURA1_BYTE_LANES_TOTAL_OUTPUT_BYTES {
            return Err(AuraError::InvalidValue("byte lane total output length"));
        }
        let _compressed_end = lane
            .compressed_offset
            .checked_add(lane.compressed_len)
            .ok_or(AuraError::InvalidValue("byte lane compressed range"))?;
        let _row_end = lane
            .row_start
            .checked_add(u64::from(lane.row_count))
            .ok_or(AuraError::InvalidValue("byte lane row range"))?;
        cumulative_output = cumulative_output
            .checked_add(lane.uncompressed_len)
            .ok_or(AuraError::InvalidValue("byte lane total output length"))?;
        if cumulative_output > MAX_AURA1_BYTE_LANES_TOTAL_OUTPUT_BYTES {
            return Err(AuraError::InvalidValue("byte lane total output length"));
        }
        output_span = output_span.max(output_end);
    }
    usize::try_from(output_span).map_err(|_| AuraError::InvalidValue("byte lane output length"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledFooter {
    pub container_version: AuraContainerVersion,
    pub schema: SchemaDescriptor,
    pub compression: CompressionDescriptor,
    pub record_count: u64,
    pub block_capacity: u16,
    pub aura0_program: DecodeProgram,
    pub aura1_program: DecodeProgram,
    pub generic_aura0_plan: Option<GenericInstructionPlan>,
    pub chunks: Vec<ChunkDescriptor>,
    pub aura1_byte_lanes: Vec<Aura1ByteLaneDescriptor>,
}

impl CompiledFooter {
    pub fn new(
        schema: SchemaDescriptor,
        record_count: u64,
        block_capacity: u16,
        aura0_program: DecodeProgram,
        aura1_program: DecodeProgram,
    ) -> Result<Self> {
        Ok(Self {
            container_version: DEFAULT_CONTAINER_VERSION,
            schema,
            compression: CompressionDescriptor::none(),
            record_count,
            block_capacity,
            aura0_program,
            aura1_program,
            generic_aura0_plan: None,
            chunks: Vec::new(),
            aura1_byte_lanes: Vec::new(),
        })
    }

    pub const fn with_container_version(mut self, container_version: AuraContainerVersion) -> Self {
        self.container_version = container_version;
        self
    }

    pub const fn container_version(&self) -> AuraContainerVersion {
        self.container_version
    }

    pub fn with_generic_aura0_plan(mut self, plan: GenericInstructionPlan) -> Self {
        self.generic_aura0_plan = Some(plan);
        self
    }

    pub fn with_aura1_byte_lanes(mut self, lanes: Vec<Aura1ByteLaneDescriptor>) -> Self {
        self.aura1_byte_lanes = lanes;
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
        validate_schema_container_compatibility(&self.schema, self.container_version)?;
        let mut out = Vec::new();
        out.extend_from_slice(COMPILED_FOOTER_MAGIC);
        put_u16_le(&mut out, self.container_version.wire_value());
        put_u8(&mut out, self.compression.kind as u8);
        put_u8(&mut out, self.compression.level);
        put_u64_le(&mut out, self.record_count);
        put_u16_le(&mut out, self.block_capacity);
        encode_schema_block(&self.schema, &mut out)?;
        self.aura0_program.encode_to(&mut out)?;
        self.aura1_program.encode_to(&mut out)?;
        encode_generic_plan(&self.generic_aura0_plan, &mut out)?;
        encode_chunks(&self.chunks, &mut out)?;
        encode_aura1_byte_lanes(&self.aura1_byte_lanes, &mut out)?;
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = ByteReader::new(bytes);
        if reader.read_exact(4)? != COMPILED_FOOTER_MAGIC {
            return Err(AuraError::InvalidMagic { expected: "AURP" });
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
        let record_count = reader.read_u64_le()?;
        let block_capacity = reader.read_u16_le()?;
        let schema = decode_schema_block(&mut reader)?;
        validate_schema_container_compatibility(&schema, container_version)?;
        let aura0_program = DecodeProgram::decode_from(&mut reader)?;
        let aura1_program = DecodeProgram::decode_from(&mut reader)?;
        let generic_aura0_plan = decode_generic_plan(&mut reader)?;
        let chunks = decode_chunks(&mut reader)?;
        let aura1_byte_lanes = decode_aura1_byte_lanes(&mut reader)?;
        reader.finish()?;
        if let Some(plan) = &generic_aura0_plan {
            validate_generic_plan_schema_authorization(&schema, plan)?;
        }
        Ok(Self {
            container_version,
            schema,
            compression,
            record_count,
            block_capacity,
            aura0_program,
            aura1_program,
            generic_aura0_plan,
            chunks,
            aura1_byte_lanes,
        })
    }
}

fn encode_aura1_byte_lanes(lanes: &[Aura1ByteLaneDescriptor], out: &mut Vec<u8>) -> Result<()> {
    if lanes.is_empty() {
        return Ok(());
    }
    let _output_len = validate_aura1_byte_lane_limits(lanes)?;
    out.extend_from_slice(AURA1_BYTE_LANE_MAGIC);
    put_u32_len(out, lanes.len(), "byte lane count")?;
    for lane in lanes {
        put_u8(out, lane.lane_version);
        put_u8(out, lane.codec_id);
        put_u8(out, lane.codec_level);
        put_u8(out, lane.checksum_kind);
        put_u32_le(out, lane.block_index);
        put_u64_le(out, lane.row_start);
        put_u32_le(out, lane.row_count);
        put_u64_le(out, lane.aura1_output_offset);
        put_u64_le(out, lane.uncompressed_len);
        put_u64_le(out, lane.compressed_offset);
        put_u64_le(out, lane.compressed_len);
        put_u64_le(out, lane.checksum);
        put_u32_le(out, lane.flags);
    }
    Ok(())
}

fn decode_aura1_byte_lanes(reader: &mut ByteReader<'_>) -> Result<Vec<Aura1ByteLaneDescriptor>> {
    if reader.remaining() == 0 {
        return Ok(Vec::new());
    }
    if reader.read_exact(4)? != AURA1_BYTE_LANE_MAGIC {
        return Err(AuraError::InvalidMagic { expected: "AUBL" });
    }
    let lane_count = reader.read_u32_le()? as usize;
    if lane_count > MAX_AURA1_BYTE_LANE_COUNT {
        return Err(AuraError::InvalidValue("byte lane count"));
    }
    let descriptor_bytes = lane_count
        .checked_mul(AURA1_BYTE_LANE_DESCRIPTOR_SIZE)
        .ok_or(AuraError::InvalidValue("byte lane descriptor bytes"))?;
    if descriptor_bytes > reader.remaining() {
        return Err(AuraError::UnexpectedEof);
    }
    let mut lanes = Vec::new();
    lanes
        .try_reserve_exact(lane_count)
        .map_err(|_| AuraError::InvalidValue("byte lane descriptor allocation"))?;
    for _ in 0..lane_count {
        let lane_version = reader.read_u8()?;
        if lane_version != AURA1_BYTE_LANE_VERSION {
            return Err(AuraError::UnsupportedVersion(u16::from(lane_version)));
        }
        let codec_id = reader.read_u8()?;
        let codec_level = reader.read_u8()?;
        let checksum_kind = reader.read_u8()?;
        lanes.push(Aura1ByteLaneDescriptor {
            lane_version,
            codec_id,
            codec_level,
            checksum_kind,
            block_index: reader.read_u32_le()?,
            row_start: reader.read_u64_le()?,
            row_count: reader.read_u32_le()?,
            aura1_output_offset: reader.read_u64_le()?,
            uncompressed_len: reader.read_u64_le()?,
            compressed_offset: reader.read_u64_le()?,
            compressed_len: reader.read_u64_le()?,
            checksum: reader.read_u64_le()?,
            flags: reader.read_u32_le()?,
        });
    }
    let _output_len = validate_aura1_byte_lane_limits(&lanes)?;
    Ok(lanes)
}

fn encode_generic_plan(plan: &Option<GenericInstructionPlan>, out: &mut Vec<u8>) -> Result<()> {
    if let Some(plan) = plan {
        put_u8(out, 1);
        let bytes = plan.encode()?;
        put_u32_len(out, bytes.len(), "generic plan length")?;
        out.extend_from_slice(&bytes);
    } else {
        put_u8(out, 0);
    }
    Ok(())
}

fn decode_generic_plan(reader: &mut ByteReader<'_>) -> Result<Option<GenericInstructionPlan>> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => {
            let len = reader.read_u32_le()? as usize;
            Ok(Some(GenericInstructionPlan::decode(
                reader.read_exact(len)?,
            )?))
        }
        _ => Err(AuraError::InvalidValue("generic plan flag")),
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
mod version_tests {
    use super::*;
    use crate::schema::book_delta_schema;

    fn compiled_footer() -> CompiledFooter {
        let schema = book_delta_schema().unwrap();
        let fields = schema
            .fields
            .iter()
            .map(|field| PhysicalFieldPlan {
                field_index: field.index,
                encoding: FieldEncoding::Absolute,
                width: physical_width_for_field_type(field.field_type).unwrap(),
                bit_width: 0,
                reference_field_index: None,
                base_value: 0,
                step: 0,
                estimated_bytes: 0,
            })
            .collect::<Vec<_>>();
        let aura0_plan = Aura0Plan {
            fields: fields.clone(),
        };
        let aura1_plan = Aura1Plan {
            block_capacity: 1,
            fields,
        };
        CompiledFooter::new(
            schema,
            0,
            1,
            DecodeProgram::from_aura0_plan(&aura0_plan, aura0_plan.fields.len()).unwrap(),
            DecodeProgram::from_aura1_plan(&aura1_plan, aura1_plan.fields.len()).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn compiled_footer_and_plan_carry_v2_container_version() {
        let footer = CompiledFooter::decode(&compiled_footer().encode().unwrap()).unwrap();
        let plan = CompiledAuraPlan::from_footer(&footer).unwrap();

        assert_eq!(AuraContainerVersion::V2, footer.container_version);
        assert_eq!(footer.container_version, plan.container_version);
        assert_eq!(plan.format_version(), 2);
    }

    #[test]
    fn v3_compiled_footer_layout_is_an_explicit_unsupported_skeleton() {
        let footer = compiled_footer();
        let v2 = footer.encode().unwrap();
        let mut advertised_v1 = v2.clone();
        advertised_v1[4..6]
            .copy_from_slice(&AuraContainerVersion::LegacyV1.wire_value().to_le_bytes());
        assert_eq!(
            CompiledFooter::decode(&advertised_v1),
            Err(AuraError::UnsupportedVersion(1)),
            "legacy V1 support does not extend to compiled footers"
        );

        let mut advertised_v3 = v2;
        advertised_v3[4..6].copy_from_slice(&AuraContainerVersion::V3.wire_value().to_le_bytes());

        assert_eq!(
            CompiledFooter::decode(&advertised_v3),
            Err(AuraError::UnsupportedVersion(3))
        );
        assert_eq!(
            footer
                .with_container_version(AuraContainerVersion::V3)
                .encode(),
            Err(AuraError::UnsupportedVersion(3))
        );
    }

    #[test]
    fn aubl_descriptor_table_is_bounded_before_allocation() {
        assert_eq!(AURA1_BYTE_LANE_DESCRIPTOR_SIZE, 64);

        let mut huge_count = AURA1_BYTE_LANE_MAGIC.to_vec();
        put_u32_le(&mut huge_count, u32::MAX);
        assert_eq!(
            decode_aura1_byte_lanes(&mut ByteReader::new(&huge_count)),
            Err(AuraError::InvalidValue("byte lane count"))
        );

        let mut truncated = AURA1_BYTE_LANE_MAGIC.to_vec();
        put_u32_le(&mut truncated, 1);
        truncated.resize(truncated.len() + AURA1_BYTE_LANE_DESCRIPTOR_SIZE - 1, 0);
        assert_eq!(
            decode_aura1_byte_lanes(&mut ByteReader::new(&truncated)),
            Err(AuraError::UnexpectedEof)
        );
    }

    #[test]
    fn compiled_chunk_table_is_bounded_before_allocation() {
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
