use crate::bitpack::{bitpacked_byte_len, signed_bitpack_width_for_range, unsigned_bitpack_width};
use crate::schema::{FieldRole, FieldTransform, SchemaDescriptor, TransformCandidates};
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
    BitpackedDeltaPrevious = 6,
    BitpackedDeltaBase = 7,
    BitpackedDeltaRelated = 8,
    DerivedOffset = 9,
    BitpackedDeltaRelatedOffset = 10,
    BitpackedDeltaPreviousOffset = 11,
    BitpackedDeltaPreviousFieldOffset = 12,
    BitpackedCandleMaxOffset = 13,
    BitpackedCandleMinOffset = 14,
    BitpackedProductResidual = 15,
    BitpackedProportionalResidual = 16,
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
            6 => Ok(Self::BitpackedDeltaPrevious),
            7 => Ok(Self::BitpackedDeltaBase),
            8 => Ok(Self::BitpackedDeltaRelated),
            9 => Ok(Self::DerivedOffset),
            10 => Ok(Self::BitpackedDeltaRelatedOffset),
            11 => Ok(Self::BitpackedDeltaPreviousOffset),
            12 => Ok(Self::BitpackedDeltaPreviousFieldOffset),
            13 => Ok(Self::BitpackedCandleMaxOffset),
            14 => Ok(Self::BitpackedCandleMinOffset),
            15 => Ok(Self::BitpackedProductResidual),
            16 => Ok(Self::BitpackedProportionalResidual),
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
    pub bit_width: u8,
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
            fields: stats
                .fields
                .iter()
                .map(|field| {
                    best_aura0_plan(
                        FieldRole::Value,
                        TransformCandidates::default_for_role(FieldRole::Value),
                        field,
                        None,
                    )
                })
                .collect(),
        }
    }

    pub fn from_schema_stats(schema: &SchemaDescriptor, stats: &IngestStats) -> Result<Self> {
        let mut fields = Vec::with_capacity(schema.fields.len());
        for descriptor in &schema.fields {
            let field = stats
                .field(descriptor.index)
                .ok_or(AuraError::InvalidValue("field stats"))?;
            let related = stats.related_field(descriptor.index);
            fields.push(best_aura0_plan(
                descriptor.role,
                descriptor.candidates,
                field,
                related,
            ));
        }
        Ok(Self { fields })
    }

    pub fn from_schema_rows_stats(
        schema: &SchemaDescriptor,
        stats: &IngestStats,
        rows: &[Vec<i64>],
    ) -> Result<Self> {
        let mut plan = Self::from_schema_stats(schema, stats)?;
        apply_candle_shape_candidates(schema, rows, &mut plan);
        apply_residual_candidates(rows, &mut plan);
        Ok(plan)
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
                    bit_width: 0,
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
    allowed: TransformCandidates,
    field: &FieldStats,
    related: Option<&RelatedFieldStats>,
) -> PhysicalFieldPlan {
    let mut candidates = Vec::new();
    if role == FieldRole::Timestamp
        && allowed.contains(FieldTransform::FixedStep)
        && field.has_implicit_fixed_step()
    {
        candidates.push(PhysicalFieldPlan {
            field_index: field.field_index,
            encoding: FieldEncoding::ImplicitFixedStep,
            width: PhysicalWidth::Zero,
            bit_width: 0,
            reference_field_index: None,
            base_value: field.first_value.unwrap_or(0),
            step: field.fixed_step.unwrap_or(0),
            estimated_bytes: 0,
        });
    }

    if allowed.contains(FieldTransform::DeltaRelated) {
        if let Some(related) = related {
            if related.min_delta == related.max_delta {
                candidates.push(derived_offset_plan(field.field_index, related));
            }
            candidates.push(PhysicalFieldPlan {
                field_index: field.field_index,
                encoding: FieldEncoding::DeltaRelated,
                width: related.delta_width(),
                bit_width: 0,
                reference_field_index: Some(related.related_field_index),
                base_value: 0,
                step: 0,
                estimated_bytes: related.observed * u64::from(related.delta_width().byte_width()),
            });
            candidates.push(bitpacked_related_plan(field.field_index, related));
            if let Some(plan) = bitpacked_related_offset_plan(field.field_index, related) {
                candidates.push(plan);
            }
        }
    }

    if allowed.contains(FieldTransform::Absolute) {
        candidates.push(absolute_plan(field));
    }
    if allowed.contains(FieldTransform::DeltaBase) {
        candidates.push(base_delta_plan(field));
        if let Some(plan) = bitpacked_base_delta_plan(field) {
            candidates.push(plan);
        }
    }
    if allowed.contains(FieldTransform::DeltaPrevious) {
        candidates.push(previous_delta_plan(field));
        candidates.push(bitpacked_previous_delta_plan(field));
        if let Some(plan) = bitpacked_previous_delta_offset_plan(field) {
            candidates.push(plan);
        }
    }
    candidates
        .into_iter()
        .min_by_key(|plan| (plan.estimated_bytes, encoding_preference(plan.encoding)))
        .unwrap_or_else(|| absolute_plan(field))
}

fn absolute_plan(field: &FieldStats) -> PhysicalFieldPlan {
    let width = field.absolute_width();
    PhysicalFieldPlan {
        field_index: field.field_index,
        encoding: FieldEncoding::Absolute,
        width,
        bit_width: 0,
        reference_field_index: None,
        base_value: 0,
        step: 0,
        estimated_bytes: field.observed * u64::from(width.byte_width()),
    }
}

fn previous_delta_plan(field: &FieldStats) -> PhysicalFieldPlan {
    let width = field.delta_width();
    PhysicalFieldPlan {
        field_index: field.field_index,
        encoding: FieldEncoding::DeltaPrevious,
        width,
        bit_width: 0,
        reference_field_index: None,
        base_value: field.first_value.unwrap_or(0),
        step: 0,
        estimated_bytes: field.observed.saturating_sub(1) * u64::from(width.byte_width()),
    }
}

fn base_delta_plan(field: &FieldStats) -> PhysicalFieldPlan {
    let width = field.base_delta_width();
    PhysicalFieldPlan {
        field_index: field.field_index,
        encoding: FieldEncoding::DeltaBase,
        width,
        bit_width: 0,
        reference_field_index: None,
        base_value: field.base_value(),
        step: 0,
        estimated_bytes: field.observed * u64::from(width.byte_width()),
    }
}

fn bitpacked_previous_delta_plan(field: &FieldStats) -> PhysicalFieldPlan {
    let bit_width = signed_bitpack_width_for_range(field.min_delta, field.max_delta);
    let value_count = field.observed.saturating_sub(1);
    PhysicalFieldPlan {
        field_index: field.field_index,
        encoding: FieldEncoding::BitpackedDeltaPrevious,
        width: PhysicalWidth::Zero,
        bit_width,
        reference_field_index: None,
        base_value: field.first_value.unwrap_or(0),
        step: 0,
        estimated_bytes: bitpacked_byte_len(value_count, bit_width),
    }
}

fn bitpacked_base_delta_plan(field: &FieldStats) -> Option<PhysicalFieldPlan> {
    let bit_width = unsigned_bitpack_width(field.max_abs_base_delta());
    Some(PhysicalFieldPlan {
        field_index: field.field_index,
        encoding: FieldEncoding::BitpackedDeltaBase,
        width: PhysicalWidth::Zero,
        bit_width,
        reference_field_index: None,
        base_value: field.base_value(),
        step: 0,
        estimated_bytes: bitpacked_byte_len(field.observed, bit_width),
    })
}

fn bitpacked_related_plan(field_index: u16, related: &RelatedFieldStats) -> PhysicalFieldPlan {
    let bit_width = signed_bitpack_width_for_range(related.min_delta, related.max_delta);
    PhysicalFieldPlan {
        field_index,
        encoding: FieldEncoding::BitpackedDeltaRelated,
        width: PhysicalWidth::Zero,
        bit_width,
        reference_field_index: Some(related.related_field_index),
        base_value: 0,
        step: 0,
        estimated_bytes: bitpacked_byte_len(related.observed, bit_width),
    }
}

fn derived_offset_plan(field_index: u16, related: &RelatedFieldStats) -> PhysicalFieldPlan {
    PhysicalFieldPlan {
        field_index,
        encoding: FieldEncoding::DerivedOffset,
        width: PhysicalWidth::Zero,
        bit_width: 0,
        reference_field_index: Some(related.related_field_index),
        base_value: related.min_delta,
        step: 0,
        estimated_bytes: 0,
    }
}

fn bitpacked_related_offset_plan(
    field_index: u16,
    related: &RelatedFieldStats,
) -> Option<PhysicalFieldPlan> {
    let span = delta_span(related.min_delta, related.max_delta)?;
    Some(PhysicalFieldPlan {
        field_index,
        encoding: FieldEncoding::BitpackedDeltaRelatedOffset,
        width: PhysicalWidth::Zero,
        bit_width: unsigned_bitpack_width(span),
        reference_field_index: Some(related.related_field_index),
        base_value: related.min_delta,
        step: 0,
        estimated_bytes: bitpacked_byte_len(related.observed, unsigned_bitpack_width(span)),
    })
}

fn bitpacked_previous_delta_offset_plan(field: &FieldStats) -> Option<PhysicalFieldPlan> {
    let span = delta_span(field.min_delta, field.max_delta)?;
    let bit_width = unsigned_bitpack_width(span);
    let value_count = field.observed.saturating_sub(1);
    Some(PhysicalFieldPlan {
        field_index: field.field_index,
        encoding: FieldEncoding::BitpackedDeltaPreviousOffset,
        width: PhysicalWidth::Zero,
        bit_width,
        reference_field_index: None,
        base_value: field.first_value.unwrap_or(0),
        step: field.min_delta,
        estimated_bytes: bitpacked_byte_len(value_count, bit_width),
    })
}

fn bitpacked_previous_field_offset_plan(
    field_index: u16,
    reference_field_index: u16,
    values: &[i64],
    reference_values: &[i64],
) -> Option<PhysicalFieldPlan> {
    if values.len() != reference_values.len() || values.is_empty() {
        return None;
    }
    let deltas = values
        .iter()
        .skip(1)
        .zip(reference_values)
        .map(|(value, previous_reference)| checked_i64_delta(*value, *previous_reference))
        .collect::<Option<Vec<_>>>()?;
    let (min_delta, max_delta) = min_max_i64(&deltas)?;
    let span = delta_span(min_delta, max_delta)?;
    let bit_width = unsigned_bitpack_width(span);
    Some(PhysicalFieldPlan {
        field_index,
        encoding: FieldEncoding::BitpackedDeltaPreviousFieldOffset,
        width: PhysicalWidth::Zero,
        bit_width,
        reference_field_index: Some(reference_field_index),
        base_value: values[0],
        step: min_delta,
        estimated_bytes: bitpacked_byte_len(values.len().saturating_sub(1) as u64, bit_width),
    })
}

fn bitpacked_candle_wick_plan(
    field_index: u16,
    encoding: FieldEncoding,
    first_reference_field_index: u16,
    second_reference_field_index: u16,
    values: &[i64],
    first_reference_values: &[i64],
    second_reference_values: &[i64],
) -> Option<PhysicalFieldPlan> {
    if values.len() != first_reference_values.len()
        || values.len() != second_reference_values.len()
        || values.is_empty()
    {
        return None;
    }
    let residuals = values
        .iter()
        .zip(first_reference_values)
        .zip(second_reference_values)
        .map(|((value, first), second)| match encoding {
            FieldEncoding::BitpackedCandleMaxOffset => {
                checked_i64_delta(*value, (*first).max(*second))
            }
            FieldEncoding::BitpackedCandleMinOffset => {
                checked_i64_delta((*first).min(*second), *value)
            }
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let (min_residual, max_residual) = min_max_i64(&residuals)?;
    if min_residual < 0 {
        return None;
    }
    let span = delta_span(min_residual, max_residual)?;
    let bit_width = unsigned_bitpack_width(span);
    Some(PhysicalFieldPlan {
        field_index,
        encoding,
        width: PhysicalWidth::Zero,
        bit_width,
        reference_field_index: Some(first_reference_field_index),
        base_value: min_residual,
        step: i64::from(second_reference_field_index),
        estimated_bytes: bitpacked_byte_len(values.len() as u64, bit_width),
    })
}

fn bitpacked_product_residual_plan(
    field_index: u16,
    quantity_field_index: u16,
    price_field_index: u16,
    divisor: u32,
    values: &[i64],
    quantity_values: &[i64],
    price_values: &[i64],
) -> Option<PhysicalFieldPlan> {
    if divisor == 0
        || values.len() != quantity_values.len()
        || values.len() != price_values.len()
        || values.is_empty()
    {
        return None;
    }
    let residuals = values
        .iter()
        .zip(quantity_values)
        .zip(price_values)
        .map(|((value, quantity), price)| {
            let predicted = checked_i128_product_div(*quantity, *price, divisor)?;
            checked_i128_to_i64(i128::from(*value) - predicted)
        })
        .collect::<Option<Vec<_>>>()?;
    residual_plan(
        field_index,
        FieldEncoding::BitpackedProductResidual,
        quantity_field_index,
        pack_ref_divisor(price_field_index, divisor)?,
        &residuals,
    )
}

fn bitpacked_proportional_residual_plan(
    field_index: u16,
    total_value_field_index: u16,
    child_quantity_field_index: u16,
    total_quantity_field_index: u16,
    values: &[i64],
    total_values: &[i64],
    child_quantity_values: &[i64],
    total_quantity_values: &[i64],
) -> Option<PhysicalFieldPlan> {
    if values.len() != total_values.len()
        || values.len() != child_quantity_values.len()
        || values.len() != total_quantity_values.len()
        || values.is_empty()
    {
        return None;
    }
    let residuals = values
        .iter()
        .zip(total_values)
        .zip(child_quantity_values)
        .zip(total_quantity_values)
        .map(|(((value, total_value), child_quantity), total_quantity)| {
            let predicted =
                checked_i128_product_div(*total_value, *child_quantity, *total_quantity)?;
            checked_i128_to_i64(i128::from(*value) - predicted)
        })
        .collect::<Option<Vec<_>>>()?;
    residual_plan(
        field_index,
        FieldEncoding::BitpackedProportionalResidual,
        total_value_field_index,
        pack_two_refs(child_quantity_field_index, total_quantity_field_index)?,
        &residuals,
    )
}

fn residual_plan(
    field_index: u16,
    encoding: FieldEncoding,
    reference_field_index: u16,
    step: i64,
    residuals: &[i64],
) -> Option<PhysicalFieldPlan> {
    let (min_residual, max_residual) = min_max_i64(residuals)?;
    let span = delta_span(min_residual, max_residual)?;
    let bit_width = unsigned_bitpack_width(span);
    Some(PhysicalFieldPlan {
        field_index,
        encoding,
        width: PhysicalWidth::Zero,
        bit_width,
        reference_field_index: Some(reference_field_index),
        base_value: min_residual,
        step,
        estimated_bytes: bitpacked_byte_len(residuals.len() as u64, bit_width),
    })
}

fn apply_candle_shape_candidates(
    schema: &SchemaDescriptor,
    rows: &[Vec<i64>],
    plan: &mut Aura0Plan,
) {
    if rows.len() < 2 {
        return;
    }
    for open_pos in 1..schema.fields.len().saturating_sub(3) {
        let open_index = schema.fields[open_pos].index;
        let related: Vec<u16> = schema
            .fields
            .iter()
            .filter(|field| field.relation.related_field_index() == Some(open_index))
            .map(|field| field.index)
            .collect();
        if related.len() < 3 {
            continue;
        }
        let Some(candidate) = best_candle_candidate(open_index, &related, rows, plan) else {
            continue;
        };
        if candidate.estimated_bytes < plan_bytes(plan, &candidate.fields) {
            replace_plan_fields(plan, candidate.fields);
        }
    }
}

struct CandidatePlan {
    fields: Vec<PhysicalFieldPlan>,
    estimated_bytes: u64,
}

fn best_candle_candidate(
    open_index: u16,
    related: &[u16],
    rows: &[Vec<i64>],
    plan: &Aura0Plan,
) -> Option<CandidatePlan> {
    let open_values = column_values(rows, open_index)?;
    let mut best = None;
    for close_index in related {
        let close_values = column_values(rows, *close_index)?;
        for high_index in related {
            if high_index == close_index {
                continue;
            }
            let high_values = column_values(rows, *high_index)?;
            if !high_values
                .iter()
                .zip(&open_values)
                .zip(&close_values)
                .all(|((high, open), close)| *high >= (*open).max(*close))
            {
                continue;
            }
            for low_index in related {
                if low_index == close_index || low_index == high_index {
                    continue;
                }
                let low_values = column_values(rows, *low_index)?;
                if !low_values
                    .iter()
                    .zip(&open_values)
                    .zip(&close_values)
                    .all(|((low, open), close)| *low <= (*open).min(*close))
                {
                    continue;
                }
                let open_plan = bitpacked_previous_field_offset_plan(
                    open_index,
                    *close_index,
                    &open_values,
                    &close_values,
                )?;
                let close_plan = bitpacked_related_offset_plan_from_values(
                    *close_index,
                    open_index,
                    &close_values,
                    &open_values,
                )?;
                let high_plan = bitpacked_candle_wick_plan(
                    *high_index,
                    FieldEncoding::BitpackedCandleMaxOffset,
                    open_index,
                    *close_index,
                    &high_values,
                    &open_values,
                    &close_values,
                )?;
                let low_plan = bitpacked_candle_wick_plan(
                    *low_index,
                    FieldEncoding::BitpackedCandleMinOffset,
                    open_index,
                    *close_index,
                    &low_values,
                    &open_values,
                    &close_values,
                )?;
                let fields = vec![open_plan, high_plan, low_plan, close_plan];
                let candidate = CandidatePlan {
                    estimated_bytes: fields.iter().map(|field| field.estimated_bytes).sum(),
                    fields,
                };
                if candidate.estimated_bytes < plan_bytes(plan, &candidate.fields)
                    && best.as_ref().is_none_or(|current: &CandidatePlan| {
                        candidate.estimated_bytes < current.estimated_bytes
                    })
                {
                    best = Some(candidate);
                }
            }
        }
    }
    best
}

fn bitpacked_related_offset_plan_from_values(
    field_index: u16,
    related_field_index: u16,
    values: &[i64],
    related_values: &[i64],
) -> Option<PhysicalFieldPlan> {
    if values.len() != related_values.len() || values.is_empty() {
        return None;
    }
    let deltas = values
        .iter()
        .zip(related_values)
        .map(|(value, related)| checked_i64_delta(*value, *related))
        .collect::<Option<Vec<_>>>()?;
    let (min_delta, max_delta) = min_max_i64(&deltas)?;
    let span = delta_span(min_delta, max_delta)?;
    let bit_width = unsigned_bitpack_width(span);
    Some(PhysicalFieldPlan {
        field_index,
        encoding: FieldEncoding::BitpackedDeltaRelatedOffset,
        width: PhysicalWidth::Zero,
        bit_width,
        reference_field_index: Some(related_field_index),
        base_value: min_delta,
        step: 0,
        estimated_bytes: bitpacked_byte_len(values.len() as u64, bit_width),
    })
}

fn apply_residual_candidates(rows: &[Vec<i64>], plan: &mut Aura0Plan) {
    let field_count = rows.first().map(|row| row.len()).unwrap_or(0);
    if field_count == 0 {
        return;
    }
    let protected_targets = candle_protected_fields(plan);
    let pending_references = candle_pending_fields(plan);
    for target_index in 0..field_count {
        if protected_targets.contains(&(target_index as u16)) {
            continue;
        }
        let Some(target_values) = column_values(rows, target_index as u16) else {
            continue;
        };
        let mut best = plan
            .fields
            .iter()
            .find(|field| usize::from(field.field_index) == target_index)
            .copied();
        for quantity_index in 0..target_index {
            if pending_references.contains(&(quantity_index as u16)) {
                continue;
            }
            let Some(quantity_values) = column_values(rows, quantity_index as u16) else {
                continue;
            };
            for price_index in 0..target_index {
                if price_index == quantity_index {
                    continue;
                }
                if pending_references.contains(&(price_index as u16)) {
                    continue;
                }
                let Some(price_values) = column_values(rows, price_index as u16) else {
                    continue;
                };
                for divisor in PRODUCT_DIVISORS {
                    if let Some(candidate) = bitpacked_product_residual_plan(
                        target_index as u16,
                        quantity_index as u16,
                        price_index as u16,
                        *divisor,
                        &target_values,
                        &quantity_values,
                        &price_values,
                    ) {
                        best = better_field_plan(best, candidate);
                    }
                }
            }
        }
        for total_value_index in 0..target_index {
            if pending_references.contains(&(total_value_index as u16)) {
                continue;
            }
            let Some(total_values) = column_values(rows, total_value_index as u16) else {
                continue;
            };
            for child_quantity_index in 0..target_index {
                if child_quantity_index == total_value_index {
                    continue;
                }
                if pending_references.contains(&(child_quantity_index as u16)) {
                    continue;
                }
                let Some(child_quantity_values) = column_values(rows, child_quantity_index as u16)
                else {
                    continue;
                };
                for total_quantity_index in 0..target_index {
                    if total_quantity_index == total_value_index
                        || total_quantity_index == child_quantity_index
                    {
                        continue;
                    }
                    if pending_references.contains(&(total_quantity_index as u16)) {
                        continue;
                    }
                    let Some(total_quantity_values) =
                        column_values(rows, total_quantity_index as u16)
                    else {
                        continue;
                    };
                    if let Some(candidate) = bitpacked_proportional_residual_plan(
                        target_index as u16,
                        total_value_index as u16,
                        child_quantity_index as u16,
                        total_quantity_index as u16,
                        &target_values,
                        &total_values,
                        &child_quantity_values,
                        &total_quantity_values,
                    ) {
                        best = better_field_plan(best, candidate);
                    }
                }
            }
        }
        if let Some(best) = best {
            replace_plan_fields(plan, vec![best]);
        }
    }
}

fn candle_protected_fields(plan: &Aura0Plan) -> Vec<u16> {
    let mut out = Vec::new();
    for field in &plan.fields {
        match field.encoding {
            FieldEncoding::BitpackedDeltaPreviousFieldOffset
            | FieldEncoding::BitpackedCandleMaxOffset
            | FieldEncoding::BitpackedCandleMinOffset => {
                push_unique(&mut out, field.field_index);
                if let Some(reference) = field.reference_field_index {
                    push_unique(&mut out, reference);
                }
                if matches!(
                    field.encoding,
                    FieldEncoding::BitpackedCandleMaxOffset
                        | FieldEncoding::BitpackedCandleMinOffset
                ) {
                    if let Ok(reference) = u16::try_from(field.step) {
                        push_unique(&mut out, reference);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn candle_pending_fields(plan: &Aura0Plan) -> Vec<u16> {
    let mut out = Vec::new();
    for field in &plan.fields {
        if matches!(
            field.encoding,
            FieldEncoding::BitpackedCandleMaxOffset | FieldEncoding::BitpackedCandleMinOffset
        ) {
            push_unique(&mut out, field.field_index);
        }
    }
    out
}

fn push_unique(values: &mut Vec<u16>, value: u16) {
    if !values.contains(&value) {
        values.push(value);
    }
}

const PRODUCT_DIVISORS: &[u32] = &[1, 10, 100, 1_000, 10_000, 100_000, 1_000_000];

fn better_field_plan(
    current: Option<PhysicalFieldPlan>,
    candidate: PhysicalFieldPlan,
) -> Option<PhysicalFieldPlan> {
    match current {
        Some(current)
            if (
                current.estimated_bytes,
                encoding_preference(current.encoding),
            ) <= (
                candidate.estimated_bytes,
                encoding_preference(candidate.encoding),
            ) =>
        {
            Some(current)
        }
        _ => Some(candidate),
    }
}

fn plan_bytes(plan: &Aura0Plan, fields: &[PhysicalFieldPlan]) -> u64 {
    fields
        .iter()
        .filter_map(|candidate| {
            plan.fields
                .iter()
                .find(|field| field.field_index == candidate.field_index)
        })
        .map(|field| field.estimated_bytes)
        .sum()
}

fn replace_plan_fields(plan: &mut Aura0Plan, fields: Vec<PhysicalFieldPlan>) {
    for replacement in fields {
        if let Some(field) = plan
            .fields
            .iter_mut()
            .find(|field| field.field_index == replacement.field_index)
        {
            *field = replacement;
        }
    }
}

fn column_values(rows: &[Vec<i64>], field_index: u16) -> Option<Vec<i64>> {
    let index = usize::from(field_index);
    rows.iter().map(|row| row.get(index).copied()).collect()
}

fn min_max_i64(values: &[i64]) -> Option<(i64, i64)> {
    let mut iter = values.iter().copied();
    let first = iter.next()?;
    let mut min = first;
    let mut max = first;
    for value in iter {
        min = min.min(value);
        max = max.max(value);
    }
    Some((min, max))
}

fn checked_i64_delta(value: i64, reference: i64) -> Option<i64> {
    value.checked_sub(reference)
}

fn checked_i128_to_i64(value: i128) -> Option<i64> {
    i64::try_from(value).ok()
}

fn checked_i128_product_div(left: i64, right: i64, divisor: impl Into<i128>) -> Option<i128> {
    let divisor = divisor.into();
    if divisor == 0 {
        return None;
    }
    i128::from(left)
        .checked_mul(i128::from(right))?
        .checked_div(divisor)
}

pub fn pack_ref_divisor(reference_field_index: u16, divisor: u32) -> Option<i64> {
    Some((i64::from(divisor) << 16) | i64::from(reference_field_index))
}

pub fn unpack_ref_divisor(value: i64) -> Option<(u16, u32)> {
    if value < 0 {
        return None;
    }
    let reference = u16::try_from(value & 0xffff).ok()?;
    let divisor = u32::try_from(value >> 16).ok()?;
    if divisor == 0 {
        return None;
    }
    Some((reference, divisor))
}

pub fn pack_two_refs(first: u16, second: u16) -> Option<i64> {
    Some((i64::from(second) << 16) | i64::from(first))
}

pub fn unpack_two_refs(value: i64) -> Option<(u16, u16)> {
    if value < 0 {
        return None;
    }
    Some((
        u16::try_from(value & 0xffff).ok()?,
        u16::try_from((value >> 16) & 0xffff).ok()?,
    ))
}

fn delta_span(min: i64, max: i64) -> Option<u64> {
    let span = i128::from(max) - i128::from(min);
    u64::try_from(span).ok()
}

fn encoding_preference(encoding: FieldEncoding) -> u8 {
    match encoding {
        FieldEncoding::ImplicitFixedStep => 0,
        FieldEncoding::DerivedOffset => 1,
        FieldEncoding::DeltaRelated => 2,
        FieldEncoding::BitpackedDeltaRelated => 3,
        FieldEncoding::BitpackedDeltaRelatedOffset => 4,
        FieldEncoding::DeltaBase => 5,
        FieldEncoding::BitpackedDeltaBase => 6,
        FieldEncoding::DeltaPrevious => 7,
        FieldEncoding::BitpackedDeltaPrevious => 8,
        FieldEncoding::BitpackedDeltaPreviousOffset => 9,
        FieldEncoding::BitpackedDeltaPreviousFieldOffset => 10,
        FieldEncoding::BitpackedCandleMaxOffset => 11,
        FieldEncoding::BitpackedCandleMinOffset => 12,
        FieldEncoding::BitpackedProductResidual => 13,
        FieldEncoding::BitpackedProportionalResidual => 14,
        FieldEncoding::TimestampStep => 15,
        FieldEncoding::Absolute => 16,
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

        assert_eq!(
            FieldEncoding::BitpackedDeltaPreviousOffset,
            plan.fields[0].encoding
        );
        assert_eq!(PhysicalWidth::Zero, plan.fields[0].width);
        assert_eq!(0, plan.fields[0].bit_width);
        assert_eq!(10_000, plan.fields[0].base_value);
        assert_eq!(10, plan.fields[0].step);
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
