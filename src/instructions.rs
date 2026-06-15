use std::collections::BTreeSet;

use crate::bytes::{put_i64_le, put_u16_le, put_u32_le, put_u8, ByteReader};
use crate::{AuraError, Result};

const MAGIC: &[u8; 4] = b"AURI";
const VERSION: u8 = 1;
const NO_SLOT: u16 = u16::MAX;
const NO_GROUP: u16 = u16::MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericInstructionPlan {
    pub streams: Vec<GenericStreamInstruction>,
    pub groups: Vec<GenericGroupInstruction>,
}

impl GenericInstructionPlan {
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        put_u8(&mut out, VERSION);
        put_u16_len(&mut out, self.streams.len(), "stream instruction count")?;
        put_u16_len(&mut out, self.groups.len(), "group instruction count")?;
        for stream in &self.streams {
            stream.encode_to(&mut out)?;
        }
        for group in &self.groups {
            group.encode_to(&mut out)?;
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = ByteReader::new(bytes);
        if reader.read_exact(4)? != MAGIC {
            return Err(AuraError::InvalidMagic { expected: "AURI" });
        }
        let version = reader.read_u8()?;
        if version != VERSION {
            return Err(AuraError::UnsupportedVersion(u16::from(version)));
        }
        let stream_count = reader.read_u16_le()? as usize;
        let group_count = reader.read_u16_le()? as usize;
        let mut streams = Vec::with_capacity(stream_count);
        for _ in 0..stream_count {
            streams.push(GenericStreamInstruction::decode_from(&mut reader)?);
        }
        let mut groups = Vec::with_capacity(group_count);
        for _ in 0..group_count {
            groups.push(GenericGroupInstruction::decode_from(&mut reader)?);
        }
        reader.finish()?;
        let plan = Self { streams, groups };
        plan.validate()?;
        Ok(plan)
    }

    fn validate(&self) -> Result<()> {
        let mut stream_ids = BTreeSet::new();
        for stream in &self.streams {
            stream.validate()?;
            if !stream_ids.insert(stream.stream_id) {
                return Err(AuraError::InvalidValue("stream instruction id"));
            }
        }

        let mut group_ids = BTreeSet::new();
        for group in &self.groups {
            group.validate()?;
            if !group_ids.insert(group.group_id()) {
                return Err(AuraError::InvalidValue("group instruction id"));
            }
        }

        for group in &self.groups {
            match group {
                GenericGroupInstruction::PartitionRuns {
                    parent_group_id,
                    count_stream_id,
                    ..
                } => {
                    ensure_group_ref(&group_ids, *parent_group_id)?;
                    ensure_stream_ref(&stream_ids, *count_stream_id)?;
                }
                GenericGroupInstruction::PartitionRunLengths {
                    parent_group_id,
                    value_stream_id,
                    count_stream_id,
                    event_count_stream_id,
                    fixed_order: _,
                    ..
                } => {
                    ensure_group_ref(&group_ids, *parent_group_id)?;
                    ensure_stream_ref(&stream_ids, *value_stream_id)?;
                    ensure_stream_ref(&stream_ids, *count_stream_id)?;
                    if let Some(event_count_stream_id) = event_count_stream_id {
                        ensure_stream_ref(&stream_ids, *event_count_stream_id)?;
                    }
                }
                GenericGroupInstruction::SegmentedDeltaStream {
                    parent_group_id,
                    base_stream_id,
                    first_stream_id,
                    delta_stream_id,
                    ..
                } => {
                    ensure_group_ref(&group_ids, *parent_group_id)?;
                    if let Some(base_stream_id) = base_stream_id {
                        ensure_stream_ref(&stream_ids, *base_stream_id)?;
                    }
                    ensure_stream_ref(&stream_ids, *first_stream_id)?;
                    ensure_stream_ref(&stream_ids, *delta_stream_id)?;
                }
                GenericGroupInstruction::GroupValueStream {
                    parent_group_id,
                    stream_id,
                    ..
                } => {
                    ensure_group_ref(&group_ids, *parent_group_id)?;
                    ensure_stream_ref(&stream_ids, *stream_id)?;
                }
                GenericGroupInstruction::PresenceMap {
                    parent_group_id,
                    stream_id,
                    ..
                } => {
                    ensure_group_ref(&group_ids, *parent_group_id)?;
                    ensure_stream_ref(&stream_ids, *stream_id)?;
                }
                GenericGroupInstruction::DerivedStream {
                    parent_group_id,
                    stream_id,
                    ..
                } => {
                    if let Some(parent_group_id) = parent_group_id {
                        ensure_group_ref(&group_ids, *parent_group_id)?;
                    }
                    ensure_stream_ref(&stream_ids, *stream_id)?;
                }
                GenericGroupInstruction::SparseStream {
                    parent_group_id,
                    presence_group_id,
                    stream_id,
                    ..
                } => {
                    ensure_group_ref(&group_ids, *parent_group_id)?;
                    ensure_group_ref(&group_ids, *presence_group_id)?;
                    ensure_stream_ref(&stream_ids, *stream_id)?;
                }
                GenericGroupInstruction::PresenceValue {
                    parent_group_id,
                    presence_group_id,
                    ..
                } => {
                    ensure_group_ref(&group_ids, *parent_group_id)?;
                    ensure_group_ref(&group_ids, *presence_group_id)?;
                }
                GenericGroupInstruction::Group { .. } => {}
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericStreamInstruction {
    pub stream_id: u16,
    pub target_slot: Option<u16>,
    pub op: GenericStreamOp,
}

impl GenericStreamInstruction {
    fn encode_to(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        put_u16_le(out, self.stream_id);
        put_u16_le(out, encode_optional_slot(self.target_slot)?);
        self.op.encode_to(out)
    }

    fn decode_from(reader: &mut ByteReader<'_>) -> Result<Self> {
        let stream_id = reader.read_u16_le()?;
        let target_slot = decode_optional_slot(reader.read_u16_le()?);
        let op = GenericStreamOp::decode_from(reader)?;
        let instruction = Self {
            stream_id,
            target_slot,
            op,
        };
        instruction.validate()?;
        Ok(instruction)
    }

    fn validate(&self) -> Result<()> {
        if self.target_slot == Some(NO_SLOT) {
            return Err(AuraError::InvalidValue("target slot"));
        }
        self.op.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenericStreamOp {
    FixedStep {
        base: i64,
        step: i64,
    },
    BaseBitpack {
        base: i64,
        unit: i64,
        bit_width: u8,
    },
    PrevDelta {
        base: i64,
        unit: i64,
        bit_width: u8,
    },
    BlockLocal {
        block_size: u16,
        mode_count: u32,
    },
    PatchedBitpack {
        base: i64,
        unit: i64,
        low_width: u8,
        high_width: u8,
        exception_count: u32,
    },
    Rle {
        base: i64,
        unit: i64,
        bit_width: u8,
        run_count: u32,
    },
    BitplaneRle {
        base: i64,
        unit: i64,
        bit_width: u8,
    },
    Dictionary {
        unit: i64,
        entry_count: u32,
        code_width: u8,
    },
    UuidConstMask {
        constant_bits: u8,
        variable_bits: u8,
    },
}

impl GenericStreamOp {
    fn encode_to(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        match *self {
            Self::FixedStep { base, step } => {
                put_u8(out, 0);
                put_i64_le(out, base);
                put_i64_le(out, step);
            }
            Self::BaseBitpack {
                base,
                unit,
                bit_width,
            } => {
                put_u8(out, 1);
                put_i64_le(out, base);
                put_i64_le(out, unit);
                put_u8(out, bit_width);
            }
            Self::PrevDelta {
                base,
                unit,
                bit_width,
            } => {
                put_u8(out, 2);
                put_i64_le(out, base);
                put_i64_le(out, unit);
                put_u8(out, bit_width);
            }
            Self::BlockLocal {
                block_size,
                mode_count,
            } => {
                put_u8(out, 3);
                put_u16_le(out, block_size);
                put_u32_le(out, mode_count);
            }
            Self::PatchedBitpack {
                base,
                unit,
                low_width,
                high_width,
                exception_count,
            } => {
                put_u8(out, 4);
                put_i64_le(out, base);
                put_i64_le(out, unit);
                put_u8(out, low_width);
                put_u8(out, high_width);
                put_u32_le(out, exception_count);
            }
            Self::Rle {
                base,
                unit,
                bit_width,
                run_count,
            } => {
                put_u8(out, 5);
                put_i64_le(out, base);
                put_i64_le(out, unit);
                put_u8(out, bit_width);
                put_u32_le(out, run_count);
            }
            Self::BitplaneRle {
                base,
                unit,
                bit_width,
            } => {
                put_u8(out, 6);
                put_i64_le(out, base);
                put_i64_le(out, unit);
                put_u8(out, bit_width);
            }
            Self::Dictionary {
                unit,
                entry_count,
                code_width,
            } => {
                put_u8(out, 7);
                put_i64_le(out, unit);
                put_u32_le(out, entry_count);
                put_u8(out, code_width);
            }
            Self::UuidConstMask {
                constant_bits,
                variable_bits,
            } => {
                put_u8(out, 8);
                put_u8(out, constant_bits);
                put_u8(out, variable_bits);
            }
        }
        Ok(())
    }

    fn decode_from(reader: &mut ByteReader<'_>) -> Result<Self> {
        let op = match reader.read_u8()? {
            0 => Self::FixedStep {
                base: reader.read_i64_le()?,
                step: reader.read_i64_le()?,
            },
            1 => Self::BaseBitpack {
                base: reader.read_i64_le()?,
                unit: reader.read_i64_le()?,
                bit_width: reader.read_u8()?,
            },
            2 => Self::PrevDelta {
                base: reader.read_i64_le()?,
                unit: reader.read_i64_le()?,
                bit_width: reader.read_u8()?,
            },
            3 => Self::BlockLocal {
                block_size: reader.read_u16_le()?,
                mode_count: reader.read_u32_le()?,
            },
            4 => Self::PatchedBitpack {
                base: reader.read_i64_le()?,
                unit: reader.read_i64_le()?,
                low_width: reader.read_u8()?,
                high_width: reader.read_u8()?,
                exception_count: reader.read_u32_le()?,
            },
            5 => Self::Rle {
                base: reader.read_i64_le()?,
                unit: reader.read_i64_le()?,
                bit_width: reader.read_u8()?,
                run_count: reader.read_u32_le()?,
            },
            6 => Self::BitplaneRle {
                base: reader.read_i64_le()?,
                unit: reader.read_i64_le()?,
                bit_width: reader.read_u8()?,
            },
            7 => Self::Dictionary {
                unit: reader.read_i64_le()?,
                entry_count: reader.read_u32_le()?,
                code_width: reader.read_u8()?,
            },
            8 => Self::UuidConstMask {
                constant_bits: reader.read_u8()?,
                variable_bits: reader.read_u8()?,
            },
            _ => return Err(AuraError::InvalidValue("stream instruction op")),
        };
        op.validate()?;
        Ok(op)
    }

    fn validate(&self) -> Result<()> {
        match *self {
            Self::FixedStep { .. } => Ok(()),
            Self::BaseBitpack {
                unit, bit_width, ..
            }
            | Self::PrevDelta {
                unit, bit_width, ..
            }
            | Self::Rle {
                unit, bit_width, ..
            }
            | Self::BitplaneRle {
                unit, bit_width, ..
            } => validate_unit_width(unit, bit_width),
            Self::BlockLocal { block_size, .. } => {
                if block_size == 0 {
                    Err(AuraError::InvalidValue("block size"))
                } else {
                    Ok(())
                }
            }
            Self::PatchedBitpack {
                unit,
                low_width,
                high_width,
                ..
            } => {
                validate_unit(unit)?;
                validate_bit_width(low_width)?;
                validate_bit_width(high_width)
            }
            Self::Dictionary {
                unit,
                entry_count,
                code_width,
            } => {
                validate_unit(unit)?;
                validate_bit_width(code_width)?;
                if entry_count == 0 {
                    Err(AuraError::InvalidValue("dictionary entry count"))
                } else {
                    Ok(())
                }
            }
            Self::UuidConstMask {
                constant_bits,
                variable_bits,
            } => {
                if u16::from(constant_bits) + u16::from(variable_bits) != 128 {
                    Err(AuraError::InvalidValue("uuid bit mask"))
                } else {
                    Ok(())
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DerivedOp {
    AddResidual = 0,
    SubtractResidual = 1,
    MaxPlusResidual = 2,
    MinMinusResidual = 3,
    FirstOffsetThenDelta = 4,
}

impl DerivedOp {
    fn from_code(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::AddResidual),
            1 => Ok(Self::SubtractResidual),
            2 => Ok(Self::MaxPlusResidual),
            3 => Ok(Self::MinMinusResidual),
            4 => Ok(Self::FirstOffsetThenDelta),
            _ => Err(AuraError::InvalidValue("derived op")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenericGroupInstruction {
    Group {
        group_id: u16,
        event_slots: Vec<u16>,
        repeated_slots: Vec<u16>,
    },
    PartitionRuns {
        group_id: u16,
        parent_group_id: u16,
        partition_slot: u16,
        count_stream_id: u16,
        fixed_order: bool,
    },
    PartitionRunLengths {
        group_id: u16,
        parent_group_id: u16,
        partition_slot: u16,
        fixed_order: bool,
        value_stream_id: u16,
        count_stream_id: u16,
        event_count_stream_id: Option<u16>,
    },
    SegmentedDeltaStream {
        group_id: u16,
        parent_group_id: u16,
        output_slot: u16,
        base_stream_id: Option<u16>,
        first_stream_id: u16,
        delta_stream_id: u16,
    },
    GroupValueStream {
        group_id: u16,
        parent_group_id: u16,
        output_slot: u16,
        stream_id: u16,
    },
    PresenceMap {
        group_id: u16,
        parent_group_id: u16,
        slots: Vec<u16>,
        stream_id: u16,
    },
    DerivedStream {
        group_id: u16,
        parent_group_id: Option<u16>,
        output_slot: u16,
        op: DerivedOp,
        input_slots: Vec<u16>,
        stream_id: u16,
    },
    SparseStream {
        group_id: u16,
        parent_group_id: u16,
        presence_group_id: u16,
        output_slot: u16,
        presence_index: u16,
        stream_id: u16,
    },
    PresenceValue {
        group_id: u16,
        parent_group_id: u16,
        presence_group_id: u16,
        output_slot: u16,
        presence_index: u16,
        value: i64,
    },
}

impl GenericGroupInstruction {
    pub const fn group_id(&self) -> u16 {
        match *self {
            Self::Group { group_id, .. }
            | Self::PartitionRuns { group_id, .. }
            | Self::PartitionRunLengths { group_id, .. }
            | Self::SegmentedDeltaStream { group_id, .. }
            | Self::GroupValueStream { group_id, .. }
            | Self::PresenceMap { group_id, .. }
            | Self::DerivedStream { group_id, .. }
            | Self::SparseStream { group_id, .. }
            | Self::PresenceValue { group_id, .. } => group_id,
        }
    }

    fn encode_to(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        match self {
            Self::Group {
                group_id,
                event_slots,
                repeated_slots,
            } => {
                put_u8(out, 0);
                put_u16_le(out, *group_id);
                put_u16_vec(out, event_slots, "event slots")?;
                put_u16_vec(out, repeated_slots, "repeated slots")?;
            }
            Self::PartitionRuns {
                group_id,
                parent_group_id,
                partition_slot,
                count_stream_id,
                fixed_order,
            } => {
                put_u8(out, 1);
                put_u16_le(out, *group_id);
                put_u16_le(out, *parent_group_id);
                put_u16_le(out, *partition_slot);
                put_u16_le(out, *count_stream_id);
                put_u8(out, u8::from(*fixed_order));
            }
            Self::PartitionRunLengths {
                group_id,
                parent_group_id,
                partition_slot,
                fixed_order,
                value_stream_id,
                count_stream_id,
                event_count_stream_id,
            } => {
                put_u8(out, 6);
                put_u16_le(out, *group_id);
                put_u16_le(out, *parent_group_id);
                put_u16_le(out, *partition_slot);
                put_u8(out, u8::from(*fixed_order));
                put_u16_le(out, *value_stream_id);
                put_u16_le(out, *count_stream_id);
                put_u16_le(out, encode_optional_slot(*event_count_stream_id)?);
            }
            Self::SegmentedDeltaStream {
                group_id,
                parent_group_id,
                output_slot,
                base_stream_id,
                first_stream_id,
                delta_stream_id,
            } => {
                put_u8(out, 7);
                put_u16_le(out, *group_id);
                put_u16_le(out, *parent_group_id);
                put_u16_le(out, *output_slot);
                put_u16_le(out, encode_optional_slot(*base_stream_id)?);
                put_u16_le(out, *first_stream_id);
                put_u16_le(out, *delta_stream_id);
            }
            Self::GroupValueStream {
                group_id,
                parent_group_id,
                output_slot,
                stream_id,
            } => {
                put_u8(out, 8);
                put_u16_le(out, *group_id);
                put_u16_le(out, *parent_group_id);
                put_u16_le(out, *output_slot);
                put_u16_le(out, *stream_id);
            }
            Self::PresenceMap {
                group_id,
                parent_group_id,
                slots,
                stream_id,
            } => {
                put_u8(out, 2);
                put_u16_le(out, *group_id);
                put_u16_le(out, *parent_group_id);
                put_u16_vec(out, slots, "presence slots")?;
                put_u16_le(out, *stream_id);
            }
            Self::DerivedStream {
                group_id,
                parent_group_id,
                output_slot,
                op,
                input_slots,
                stream_id,
            } => {
                put_u8(out, 3);
                put_u16_le(out, *group_id);
                put_u16_le(out, encode_optional_group(*parent_group_id));
                put_u16_le(out, *output_slot);
                put_u8(out, *op as u8);
                put_u16_vec(out, input_slots, "input slots")?;
                put_u16_le(out, *stream_id);
            }
            Self::SparseStream {
                group_id,
                parent_group_id,
                presence_group_id,
                output_slot,
                presence_index,
                stream_id,
            } => {
                put_u8(out, 4);
                put_u16_le(out, *group_id);
                put_u16_le(out, *parent_group_id);
                put_u16_le(out, *presence_group_id);
                put_u16_le(out, *output_slot);
                put_u16_le(out, *presence_index);
                put_u16_le(out, *stream_id);
            }
            Self::PresenceValue {
                group_id,
                parent_group_id,
                presence_group_id,
                output_slot,
                presence_index,
                value,
            } => {
                put_u8(out, 5);
                put_u16_le(out, *group_id);
                put_u16_le(out, *parent_group_id);
                put_u16_le(out, *presence_group_id);
                put_u16_le(out, *output_slot);
                put_u16_le(out, *presence_index);
                put_i64_le(out, *value);
            }
        }
        Ok(())
    }

    fn decode_from(reader: &mut ByteReader<'_>) -> Result<Self> {
        let group = match reader.read_u8()? {
            0 => Self::Group {
                group_id: reader.read_u16_le()?,
                event_slots: read_u16_vec(reader)?,
                repeated_slots: read_u16_vec(reader)?,
            },
            1 => Self::PartitionRuns {
                group_id: reader.read_u16_le()?,
                parent_group_id: reader.read_u16_le()?,
                partition_slot: reader.read_u16_le()?,
                count_stream_id: reader.read_u16_le()?,
                fixed_order: match reader.read_u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(AuraError::InvalidValue("partition order flag")),
                },
            },
            2 => Self::PresenceMap {
                group_id: reader.read_u16_le()?,
                parent_group_id: reader.read_u16_le()?,
                slots: read_u16_vec(reader)?,
                stream_id: reader.read_u16_le()?,
            },
            3 => Self::DerivedStream {
                group_id: reader.read_u16_le()?,
                parent_group_id: decode_optional_group(reader.read_u16_le()?),
                output_slot: reader.read_u16_le()?,
                op: DerivedOp::from_code(reader.read_u8()?)?,
                input_slots: read_u16_vec(reader)?,
                stream_id: reader.read_u16_le()?,
            },
            4 => Self::SparseStream {
                group_id: reader.read_u16_le()?,
                parent_group_id: reader.read_u16_le()?,
                presence_group_id: reader.read_u16_le()?,
                output_slot: reader.read_u16_le()?,
                presence_index: reader.read_u16_le()?,
                stream_id: reader.read_u16_le()?,
            },
            5 => Self::PresenceValue {
                group_id: reader.read_u16_le()?,
                parent_group_id: reader.read_u16_le()?,
                presence_group_id: reader.read_u16_le()?,
                output_slot: reader.read_u16_le()?,
                presence_index: reader.read_u16_le()?,
                value: reader.read_i64_le()?,
            },
            6 => Self::PartitionRunLengths {
                group_id: reader.read_u16_le()?,
                parent_group_id: reader.read_u16_le()?,
                partition_slot: reader.read_u16_le()?,
                fixed_order: match reader.read_u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(AuraError::InvalidValue("partition order flag")),
                },
                value_stream_id: reader.read_u16_le()?,
                count_stream_id: reader.read_u16_le()?,
                event_count_stream_id: decode_optional_slot(reader.read_u16_le()?),
            },
            7 => Self::SegmentedDeltaStream {
                group_id: reader.read_u16_le()?,
                parent_group_id: reader.read_u16_le()?,
                output_slot: reader.read_u16_le()?,
                base_stream_id: decode_optional_slot(reader.read_u16_le()?),
                first_stream_id: reader.read_u16_le()?,
                delta_stream_id: reader.read_u16_le()?,
            },
            8 => Self::GroupValueStream {
                group_id: reader.read_u16_le()?,
                parent_group_id: reader.read_u16_le()?,
                output_slot: reader.read_u16_le()?,
                stream_id: reader.read_u16_le()?,
            },
            _ => return Err(AuraError::InvalidValue("group instruction op")),
        };
        group.validate()?;
        Ok(group)
    }

    fn validate(&self) -> Result<()> {
        match self {
            Self::Group {
                event_slots,
                repeated_slots,
                ..
            } => {
                if event_slots.is_empty() && repeated_slots.is_empty() {
                    return Err(AuraError::InvalidValue("group slots"));
                }
            }
            Self::PresenceMap { slots, .. } => {
                if slots.is_empty() {
                    return Err(AuraError::InvalidValue("presence slots"));
                }
            }
            Self::DerivedStream { input_slots, .. } => {
                if input_slots.is_empty() {
                    return Err(AuraError::InvalidValue("input slots"));
                }
            }
            Self::SparseStream { .. } | Self::PresenceValue { .. } => {}
            Self::PartitionRuns { .. } => {}
            Self::PartitionRunLengths { .. }
            | Self::SegmentedDeltaStream { .. }
            | Self::GroupValueStream { .. } => {}
        }
        Ok(())
    }
}

fn ensure_stream_ref(stream_ids: &BTreeSet<u16>, stream_id: u16) -> Result<()> {
    if stream_ids.contains(&stream_id) {
        Ok(())
    } else {
        Err(AuraError::InvalidValue("stream instruction reference"))
    }
}

fn ensure_group_ref(group_ids: &BTreeSet<u16>, group_id: u16) -> Result<()> {
    if group_ids.contains(&group_id) {
        Ok(())
    } else {
        Err(AuraError::InvalidValue("group instruction reference"))
    }
}

fn validate_unit(unit: i64) -> Result<()> {
    if unit <= 0 {
        Err(AuraError::InvalidValue("storage unit"))
    } else {
        Ok(())
    }
}

fn validate_bit_width(bit_width: u8) -> Result<()> {
    if bit_width > 128 {
        Err(AuraError::InvalidValue("bit width"))
    } else {
        Ok(())
    }
}

fn validate_unit_width(unit: i64, bit_width: u8) -> Result<()> {
    validate_unit(unit)?;
    validate_bit_width(bit_width)
}

fn encode_optional_slot(slot: Option<u16>) -> Result<u16> {
    match slot {
        Some(NO_SLOT) => Err(AuraError::InvalidValue("target slot")),
        Some(slot) => Ok(slot),
        None => Ok(NO_SLOT),
    }
}

const fn decode_optional_slot(slot: u16) -> Option<u16> {
    if slot == NO_SLOT {
        None
    } else {
        Some(slot)
    }
}

const fn encode_optional_group(group_id: Option<u16>) -> u16 {
    match group_id {
        Some(group_id) => group_id,
        None => NO_GROUP,
    }
}

const fn decode_optional_group(group_id: u16) -> Option<u16> {
    if group_id == NO_GROUP {
        None
    } else {
        Some(group_id)
    }
}

fn put_u16_vec(out: &mut Vec<u8>, values: &[u16], name: &'static str) -> Result<()> {
    put_u16_len(out, values.len(), name)?;
    for value in values {
        put_u16_le(out, *value);
    }
    Ok(())
}

fn read_u16_vec(reader: &mut ByteReader<'_>) -> Result<Vec<u16>> {
    let len = reader.read_u16_le()? as usize;
    let mut values = Vec::with_capacity(len);
    for _ in 0..len {
        values.push(reader.read_u16_le()?);
    }
    Ok(values)
}

fn put_u16_len(out: &mut Vec<u8>, len: usize, name: &'static str) -> Result<()> {
    let len = u16::try_from(len).map_err(|_| AuraError::InvalidValue(name))?;
    put_u16_le(out, len);
    Ok(())
}
