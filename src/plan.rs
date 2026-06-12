use crate::schema::{FieldRole, SchemaDescriptor};
use crate::stats::{FieldStats, IngestStats, PhysicalWidth, RelatedFieldStats};
use crate::{AuraError, Result};

/// Per-field physical transform selected for a compiled Aura level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FieldEncoding {
    Absolute = 0,
    DeltaPrevious = 1,
    DeltaBase = 2,
    TimestampStep = 3,
    DeltaRelated = 4,
    ImplicitFixedStep = 5,
}

impl FieldEncoding {
    pub fn from_code(value: u8) -> crate::Result<Self> {
        match value {
            0 => Ok(Self::Absolute),
            1 => Ok(Self::DeltaPrevious),
            2 => Ok(Self::DeltaBase),
            3 => Ok(Self::TimestampStep),
            4 => Ok(Self::DeltaRelated),
            5 => Ok(Self::ImplicitFixedStep),
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
    pub reference_field_index: Option<u16>,
    pub base_value: i64,
    pub step: i64,
    pub estimated_bytes: u64,
}

/// Compact `.aura0` plan selected from ingest stats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aura0Plan {
    pub fields: Vec<PhysicalFieldPlan>,
}

impl Aura0Plan {
    pub fn from_stats(stats: &IngestStats) -> Self {
        Self {
            fields: stats.fields.iter().map(previous_delta_plan).collect(),
        }
    }

    pub fn from_schema_stats(schema: &SchemaDescriptor, stats: &IngestStats) -> Result<Self> {
        let mut fields = Vec::with_capacity(schema.fields.len());
        for descriptor in &schema.fields {
            let field = stats
                .field(descriptor.index)
                .ok_or(AuraError::InvalidValue("field stats"))?;
            let related = stats.related_field(descriptor.index);
            fields.push(best_aura0_plan(descriptor.role, field, related));
        }
        Ok(Self { fields })
    }

    pub fn field<'a>(
        &'a self,
        name: &str,
        schema: &SchemaDescriptor,
    ) -> Option<&'a PhysicalFieldPlan> {
        let descriptor = schema.field(name)?;
        self.fields
            .iter()
            .find(|plan| plan.field_index == descriptor.index)
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
                    reference_field_index: None,
                    base_value: 0,
                    step: 0,
                    estimated_bytes: field.observed
                        * u64::from(field.absolute_width().byte_width()),
                })
                .collect(),
        }
    }
}

fn best_aura0_plan(
    role: FieldRole,
    field: &FieldStats,
    related: Option<&RelatedFieldStats>,
) -> PhysicalFieldPlan {
    if role == FieldRole::Timestamp && field.has_implicit_fixed_step() {
        return PhysicalFieldPlan {
            field_index: field.field_index,
            encoding: FieldEncoding::ImplicitFixedStep,
            width: PhysicalWidth::Zero,
            reference_field_index: None,
            base_value: field.first_value.unwrap_or(0),
            step: field.fixed_step.unwrap_or(0),
            estimated_bytes: 16,
        };
    }

    if let Some(related) = related {
        return PhysicalFieldPlan {
            field_index: field.field_index,
            encoding: FieldEncoding::DeltaRelated,
            width: related.delta_width(),
            reference_field_index: Some(related.related_field_index),
            base_value: 0,
            step: 0,
            estimated_bytes: related.observed * u64::from(related.delta_width().byte_width()),
        };
    }

    let absolute = absolute_plan(field);
    let base = base_delta_plan(field);
    if role == FieldRole::Quantity && base.width < absolute.width {
        return base;
    }

    let previous = previous_delta_plan(field);
    [absolute, base, previous]
        .into_iter()
        .min_by_key(|plan| (plan.estimated_bytes, encoding_preference(plan.encoding)))
        .unwrap_or(absolute)
}

fn absolute_plan(field: &FieldStats) -> PhysicalFieldPlan {
    let width = field.absolute_width();
    PhysicalFieldPlan {
        field_index: field.field_index,
        encoding: FieldEncoding::Absolute,
        width,
        reference_field_index: None,
        base_value: 0,
        step: 0,
        estimated_bytes: field.observed * u64::from(width.byte_width()),
    }
}

fn previous_delta_plan(field: &FieldStats) -> PhysicalFieldPlan {
    let width = field.delta_width();
    let first_width = field.absolute_width();
    PhysicalFieldPlan {
        field_index: field.field_index,
        encoding: FieldEncoding::DeltaPrevious,
        width,
        reference_field_index: None,
        base_value: field.first_value.unwrap_or(0),
        step: 0,
        estimated_bytes: u64::from(first_width.byte_width())
            + field.observed.saturating_sub(1) * u64::from(width.byte_width()),
    }
}

fn base_delta_plan(field: &FieldStats) -> PhysicalFieldPlan {
    let width = field.base_delta_width();
    PhysicalFieldPlan {
        field_index: field.field_index,
        encoding: FieldEncoding::DeltaBase,
        width,
        reference_field_index: None,
        base_value: field.base_value(),
        step: 0,
        estimated_bytes: 8 + field.observed * u64::from(width.byte_width()),
    }
}

fn encoding_preference(encoding: FieldEncoding) -> u8 {
    match encoding {
        FieldEncoding::ImplicitFixedStep => 0,
        FieldEncoding::DeltaRelated => 1,
        FieldEncoding::DeltaBase => 2,
        FieldEncoding::DeltaPrevious => 3,
        FieldEncoding::TimestampStep => 4,
        FieldEncoding::Absolute => 5,
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
        assert_eq!(10_000, plan.fields[0].base_value);
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
