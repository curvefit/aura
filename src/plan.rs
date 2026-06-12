use crate::stats::{IngestStats, PhysicalWidth};

/// Per-field physical transform selected for a compiled Aura level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FieldEncoding {
    Absolute = 0,
    DeltaPrevious = 1,
    DeltaBase = 2,
    TimestampStep = 3,
}

impl FieldEncoding {
    pub fn from_code(value: u8) -> crate::Result<Self> {
        match value {
            0 => Ok(Self::Absolute),
            1 => Ok(Self::DeltaPrevious),
            2 => Ok(Self::DeltaBase),
            3 => Ok(Self::TimestampStep),
            _ => Err(crate::AuraError::InvalidValue("field encoding")),
        }
    }
}

/// One field's compiled representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalFieldPlan {
    pub field_index: u16,
    pub encoding: FieldEncoding,
    pub width: PhysicalWidth,
}

/// Compact `.aura0` plan selected from ingest stats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aura0Plan {
    pub fields: Vec<PhysicalFieldPlan>,
}

impl Aura0Plan {
    pub fn from_stats(stats: &IngestStats) -> Self {
        Self {
            fields: stats
                .fields
                .iter()
                .map(|field| PhysicalFieldPlan {
                    field_index: field.field_index,
                    encoding: FieldEncoding::DeltaPrevious,
                    width: field.delta_width(),
                })
                .collect(),
        }
    }
}

/// Replay `.aura1` plan selected from ingest stats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aura1Plan {
    pub block_capacity: u16,
    pub fields: Vec<PhysicalFieldPlan>,
}

impl Aura1Plan {
    pub fn from_stats(stats: &IngestStats, block_capacity: u16) -> Self {
        Self {
            block_capacity: block_capacity.max(1),
            fields: stats
                .fields
                .iter()
                .map(|field| PhysicalFieldPlan {
                    field_index: field.field_index,
                    encoding: FieldEncoding::Absolute,
                    width: field.absolute_width(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aura0_uses_delta_widths_from_stats() {
        let mut stats = IngestStats::new(1).unwrap();
        stats.observe_i64(0, 10_000).unwrap();
        stats.observe_i64(0, 10_010).unwrap();

        let plan = Aura0Plan::from_stats(&stats);

        assert_eq!(FieldEncoding::DeltaPrevious, plan.fields[0].encoding);
        assert_eq!(PhysicalWidth::I8, plan.fields[0].width);
    }

    #[test]
    fn aura1_uses_absolute_widths_and_block_capacity() {
        let mut stats = IngestStats::new(1).unwrap();
        stats.observe_i64(0, 10_000).unwrap();
        stats.observe_i64(0, 80_000).unwrap();

        let plan = Aura1Plan::from_stats(&stats, 4);

        assert_eq!(4, plan.block_capacity);
        assert_eq!(FieldEncoding::Absolute, plan.fields[0].encoding);
        assert_eq!(PhysicalWidth::I32, plan.fields[0].width);
    }
}
