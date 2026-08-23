//! Strict, canonical external JSON representation of Aura v3 logical schemas.

use std::collections::BTreeSet;
use std::io::{self, Write};

use serde::{Deserialize, Serialize};

use crate::header::{DerivedExpression, DerivedExpressionOp, DerivedExpressionSource};
use crate::schema::{
    encode_schema_descriptor, DualDomainDescriptor, FieldDescriptor, FieldRelation, FieldRole,
    FieldScope, FieldTransform, FieldType, GroupDescriptor, GroupKind, RelationshipPermissions,
    SchemaDescriptor, SchemaEncodingVersion, TransformCandidates,
};
use crate::{AuraError, Result};

/// Maximum accepted or emitted UTF-8 JSON schema size (16 MiB).
///
/// Programmatic descriptors are conservatively complexity-checked against the
/// same envelope before binary identity validation or JSON DTO allocation.
pub const MAX_SCHEMA_JSON_BYTES: usize = 16 * 1024 * 1024;

#[cfg(test)]
static POST_PREFLIGHT_STEPS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

const TRANSFORM_REGISTRY: &[(TransformDto, FieldTransform)] = &[
    (TransformDto::Absolute, FieldTransform::Absolute),
    (TransformDto::DeltaBase, FieldTransform::DeltaBase),
    (TransformDto::DeltaPrevious, FieldTransform::DeltaPrevious),
    (TransformDto::DeltaRelated, FieldTransform::DeltaRelated),
    (TransformDto::FixedStep, FieldTransform::FixedStep),
    (TransformDto::Delta2, FieldTransform::Delta2),
    (TransformDto::Midpoint, FieldTransform::Midpoint),
    (TransformDto::RoughStep, FieldTransform::RoughStep),
    (TransformDto::ZigzagVarint, FieldTransform::ZigzagVarint),
    (TransformDto::Bitpack, FieldTransform::Bitpack),
];

const PERMISSION_REGISTRY: &[(PermissionDto, u8)] = &[
    (PermissionDto::Split, RelationshipPermissions::SPLIT),
    (
        PermissionDto::WithinDomain,
        RelationshipPermissions::WITHIN_DOMAIN,
    ),
    (
        PermissionDto::AcrossDomainSameField,
        RelationshipPermissions::ACROSS_DOMAIN_SAME_FIELD,
    ),
    (
        PermissionDto::JointSameField,
        RelationshipPermissions::JOINT_SAME_FIELD,
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum SchemaFormatDto {
    #[serde(rename = "aura-schema")]
    AuraSchema,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum SchemaEncodingDto {
    #[serde(rename = "v3")]
    V3,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SchemaDto {
    schema_format: SchemaFormatDto,
    schema_version: u16,
    schema_encoding: SchemaEncodingDto,
    name: String,
    #[serde(default, deserialize_with = "deserialize_optional_schema_id")]
    schema_id: Option<u32>,
    fields: Vec<FieldDto>,
    groups: Vec<GroupDto>,
    derived_expressions: Vec<DerivedExpressionDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldDto {
    id: u16,
    name: String,
    #[serde(rename = "type")]
    field_type: FieldTypeDto,
    role: FieldRoleDto,
    scale: i8,
    scope: FieldScopeDto,
    nullable: bool,
    relation: RelationDto,
    transform_candidates: Vec<TransformDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelationDto {
    kind: RelationKindDto,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_field_id",
        skip_serializing_if = "Option::is_none"
    )]
    field_id: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupDto {
    id: u16,
    kind: GroupKindDto,
    child_slots: Vec<u16>,
    dual_domain: RequiredNullableDualDomainDto,
    relationship_permissions: Vec<PermissionDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DualDomainDto {
    discriminator_slot: u16,
    domain_count: u8,
}

/// Required JSON key whose value may explicitly be an object or null.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
struct RequiredNullableDualDomainDto(Option<DualDomainDto>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DerivedExpressionDto {
    id: u8,
    output_slot: u16,
    op: DerivedOpDto,
    input_slots: Vec<u16>,
    literals: Vec<i64>,
    source: DerivedSourceDto,
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        enum $name {
            $(#[serde(rename = $wire)] $variant),+
        }
    };
}

string_enum!(FieldTypeDto {
    I8 => "i8", U8 => "u8", I16 => "i16", U16 => "u16", I32 => "i32",
    U32 => "u32", I64 => "i64", U64 => "u64", TimestampNs => "timestamp_ns",
    I128 => "i128", Opaque16 => "opaque16", TimestampMs => "timestamp_ms",
    Utf8 => "utf8", DecimalText => "decimal_text"
});
string_enum!(FieldRoleDto {
    Timestamp => "timestamp", Sequence => "sequence", Identifier => "identifier",
    Side => "side", Price => "price", Quantity => "quantity", Value => "value",
    Count => "count", Flag => "flag", PriceAnchor => "price_anchor", Boolean => "boolean",
    Enum => "enum", Bitfield => "bitfield"
});
string_enum!(FieldScopeDto { Event => "event", Repeated => "repeated" });
string_enum!(RelationKindDto { None => "none", DeltaFromField => "delta_from_field" });
string_enum!(GroupKindDto { Repeated => "repeated" });
string_enum!(TransformDto {
    Absolute => "absolute", DeltaBase => "delta_base", DeltaPrevious => "delta_previous",
    DeltaRelated => "delta_related", FixedStep => "fixed_step", Delta2 => "delta2",
    Midpoint => "midpoint", RoughStep => "rough_step", ZigzagVarint => "zigzag_varint",
    Bitpack => "bitpack"
});
string_enum!(PermissionDto {
    Split => "split", WithinDomain => "within_domain",
    AcrossDomainSameField => "across_domain_same_field", JointSameField => "joint_same_field"
});
string_enum!(DerivedSourceDto { External => "external", Internal => "internal" });
string_enum!(DerivedOpDto {
    Add => "add", Sub => "sub", Mul => "mul", Div => "div", Min => "min", Max => "max",
    AddResidual => "add_residual", SubtractResidual => "subtract_residual",
    MaxPlusResidual => "max_plus_residual", MinMinusResidual => "min_minus_residual",
    FirstOffsetThenDelta => "first_offset_then_delta", MulDiv => "mul_div",
    PreviousSnapshotSameKeyResidual => "previous_snapshot_same_key_residual",
    PreviousMutationSameKeyResidual => "previous_mutation_same_key_residual",
    PreviousOutputByKeyResidual => "previous_output_by_key_residual"
});

fn deserialize_optional_schema_id<'de, D>(
    deserializer: D,
) -> core::result::Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    u32::deserialize(deserializer).map(Some)
}

fn deserialize_optional_field_id<'de, D>(
    deserializer: D,
) -> core::result::Result<Option<u16>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    u16::deserialize(deserializer).map(Some)
}

/// Parse and semantically validate one Aura schema JSON v1 document.
pub fn parse_schema_json(json: &str) -> Result<SchemaDescriptor> {
    if json.len() > MAX_SCHEMA_JSON_BYTES {
        return Err(AuraError::InvalidValue("schema json length"));
    }
    let dto: SchemaDto =
        serde_json::from_str(json).map_err(|_| AuraError::InvalidValue("schema json"))?;
    dto.into_schema()
}

/// Parse and re-emit one schema as canonical UTF-8 JSON with one trailing newline.
pub fn canonicalize_schema_json(json: &str) -> Result<String> {
    parse_schema_json(json)?.to_canonical_json()
}

impl SchemaDescriptor {
    pub fn from_json(json: &str) -> Result<Self> {
        parse_schema_json(json)
    }

    pub fn to_canonical_json(&self) -> Result<String> {
        if self.encoding_version != SchemaEncodingVersion::V3 {
            return Err(AuraError::InvalidValue("schema json encoding"));
        }
        preflight_schema_json_complexity(self, MAX_SCHEMA_JSON_BYTES)?;
        #[cfg(test)]
        POST_PREFLIGHT_STEPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // This also verifies that the stored schema hash matches the validated
        // binary descriptor identity rather than trusting public mutable fields.
        encode_schema_descriptor(self).map(|_| ())?;
        let dto = SchemaDto::from_schema(self)?;
        serialize_canonical_dto_with_limit(&dto, MAX_SCHEMA_JSON_BYTES)
    }
}

pub(crate) fn preflight_schema_json_complexity(
    schema: &SchemaDescriptor,
    limit: usize,
) -> Result<()> {
    // These charges conservatively cover pretty-print syntax, decimal integer
    // widths, binary descriptor bytes, and the owned DTO vectors/strings.
    const ROOT_BYTES: usize = 4_096;
    const FIELD_BYTES: usize = 1_024;
    const GROUP_BYTES: usize = 1_024;
    const EXPRESSION_BYTES: usize = 1_024;
    const COLLECTION_ELEMENT_BYTES: usize = 64;
    const WORST_JSON_ESCAPE_PER_UTF8_BYTE: usize = 6;

    fn add(total: &mut usize, count: usize, unit: usize, limit: usize) -> Result<()> {
        *total = total
            .checked_add(
                count
                    .checked_mul(unit)
                    .ok_or(AuraError::InvalidValue("schema json length"))?,
            )
            .ok_or(AuraError::InvalidValue("schema json length"))?;
        if *total > limit {
            return Err(AuraError::InvalidValue("schema json length"));
        }
        Ok(())
    }

    let mut total = ROOT_BYTES;
    add(
        &mut total,
        schema.name.len(),
        WORST_JSON_ESCAPE_PER_UTF8_BYTE,
        limit,
    )?;
    add(&mut total, schema.fields.len(), FIELD_BYTES, limit)?;
    for field in &schema.fields {
        add(
            &mut total,
            field.name.len(),
            WORST_JSON_ESCAPE_PER_UTF8_BYTE,
            limit,
        )?;
        add(
            &mut total,
            TRANSFORM_REGISTRY
                .iter()
                .filter(|(_, transform)| field.candidates.contains(*transform))
                .count(),
            COLLECTION_ELEMENT_BYTES,
            limit,
        )?;
    }
    add(
        &mut total,
        schema.compact_schema_map.as_ref().map_or(0, Vec::len),
        COLLECTION_ELEMENT_BYTES,
        limit,
    )?;
    add(&mut total, schema.groups.len(), GROUP_BYTES, limit)?;
    for group in &schema.groups {
        add(
            &mut total,
            group.child_slots.len(),
            COLLECTION_ELEMENT_BYTES,
            limit,
        )?;
        add(
            &mut total,
            group.relationships.bits().count_ones() as usize,
            COLLECTION_ELEMENT_BYTES,
            limit,
        )?;
    }
    add(
        &mut total,
        schema.derived_expressions.len(),
        EXPRESSION_BYTES,
        limit,
    )?;
    for expression in &schema.derived_expressions {
        add(
            &mut total,
            expression.input_slots.len(),
            COLLECTION_ELEMENT_BYTES,
            limit,
        )?;
        add(
            &mut total,
            expression.literals.len(),
            COLLECTION_ELEMENT_BYTES,
            limit,
        )?;
    }
    Ok(())
}

fn serialize_canonical_dto_with_limit(dto: &SchemaDto, limit: usize) -> Result<String> {
    let mut writer = BoundedJsonWriter::new(limit);
    if serde_json::to_writer_pretty(&mut writer, dto).is_err() {
        return Err(writer.aura_error());
    }
    if writer.write_all(b"\n").is_err() {
        return Err(writer.aura_error());
    }
    String::from_utf8(writer.buffer).map_err(|_| AuraError::InvalidValue("schema json"))
}

struct BoundedJsonWriter {
    buffer: Vec<u8>,
    limit: usize,
    length_exceeded: bool,
    allocation_failed: bool,
}

impl BoundedJsonWriter {
    fn new(limit: usize) -> Self {
        Self {
            buffer: Vec::new(),
            limit,
            length_exceeded: false,
            allocation_failed: false,
        }
    }

    fn aura_error(&self) -> AuraError {
        if self.length_exceeded {
            AuraError::InvalidValue("schema json length")
        } else if self.allocation_failed {
            AuraError::InvalidValue("schema json allocation")
        } else {
            AuraError::InvalidValue("schema json")
        }
    }
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(end) = self.buffer.len().checked_add(bytes.len()) else {
            self.length_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "schema json length",
            ));
        };
        if end > self.limit {
            self.length_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "schema json length",
            ));
        }
        if self.buffer.try_reserve_exact(bytes.len()).is_err() {
            self.allocation_failed = true;
            return Err(io::Error::other("schema json allocation"));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl SchemaDto {
    fn into_schema(mut self) -> Result<SchemaDescriptor> {
        if self.schema_format != SchemaFormatDto::AuraSchema
            || self.schema_version != 1
            || self.schema_encoding != SchemaEncodingDto::V3
        {
            return Err(AuraError::InvalidValue("schema json version"));
        }
        self.fields.sort_by_key(|field| field.id);
        self.groups.sort_by_key(|group| group.id);
        self.derived_expressions
            .sort_by_key(|expression| expression.id);

        let mut field_names = BTreeSet::new();
        let mut fields = Vec::new();
        fields
            .try_reserve_exact(self.fields.len())
            .map_err(|_| AuraError::InvalidValue("schema json allocation"))?;
        for (slot, field) in self.fields.into_iter().enumerate() {
            if usize::from(field.id) != slot {
                return Err(AuraError::InvalidValue("stable field id"));
            }
            if !field_names.insert(field.name.clone()) {
                return Err(AuraError::InvalidValue("duplicate field name"));
            }
            fields.push(field.into_field()?);
        }

        let mut group_ids = BTreeSet::new();
        let mut groups = Vec::new();
        groups
            .try_reserve_exact(self.groups.len())
            .map_err(|_| AuraError::InvalidValue("schema json allocation"))?;
        for group in self.groups {
            if !group_ids.insert(group.id) {
                return Err(AuraError::InvalidValue("duplicate group id"));
            }
            groups.push(group.into_group()?);
        }

        let mut expression_ids = BTreeSet::new();
        let mut expressions = Vec::new();
        expressions
            .try_reserve_exact(self.derived_expressions.len())
            .map_err(|_| AuraError::InvalidValue("schema json allocation"))?;
        for expression in self.derived_expressions {
            if !expression_ids.insert(expression.id) {
                return Err(AuraError::InvalidValue("derived expression id"));
            }
            expressions.push(expression.into_expression()?);
        }

        let schema = SchemaDescriptor {
            schema_id: 0,
            encoding_version: SchemaEncodingVersion::V2,
            name: self.name,
            fields,
            compact_schema_map: None,
            derived_expressions: Vec::new(),
            groups: Vec::new(),
        }
        .with_derived_expressions(expressions)?
        .with_v3_groups(groups)?;
        if self
            .schema_id
            .is_some_and(|schema_id| schema_id != schema.schema_id)
        {
            return Err(AuraError::InvalidValue("schema id"));
        }
        Ok(schema)
    }

    fn from_schema(schema: &SchemaDescriptor) -> Result<Self> {
        schema.validate()?;
        let mut fields = schema.fields.iter().map(FieldDto::from).collect::<Vec<_>>();
        fields.sort_by_key(|field| field.id);
        let mut groups = schema.groups.iter().map(GroupDto::from).collect::<Vec<_>>();
        groups.sort_by_key(|group| group.id);
        let mut derived_expressions = schema
            .derived_expressions
            .iter()
            .map(DerivedExpressionDto::from)
            .collect::<Vec<_>>();
        derived_expressions.sort_by_key(|expression| expression.id);
        Ok(Self {
            schema_format: SchemaFormatDto::AuraSchema,
            schema_version: 1,
            schema_encoding: SchemaEncodingDto::V3,
            name: schema.name.clone(),
            schema_id: Some(schema.schema_id),
            fields,
            groups,
            derived_expressions,
        })
    }
}

impl FieldDto {
    fn into_field(self) -> Result<FieldDescriptor> {
        let relation = match (self.relation.kind, self.relation.field_id) {
            (RelationKindDto::None, None) => FieldRelation::None,
            (RelationKindDto::DeltaFromField, Some(field_id)) => {
                FieldRelation::DeltaFromField(field_id)
            }
            _ => return Err(AuraError::InvalidValue("field relation")),
        };
        let mut seen = BTreeSet::new();
        let mut bits = 0u16;
        for candidate in self.transform_candidates {
            if !seen.insert(candidate) {
                return Err(AuraError::InvalidValue("transform candidates"));
            }
            let transform = TRANSFORM_REGISTRY
                .iter()
                .find_map(|(dto, transform)| (*dto == candidate).then_some(*transform))
                .ok_or(AuraError::InvalidValue("transform candidates"))?;
            bits |= transform.bit();
        }
        Ok(FieldDescriptor {
            index: self.id,
            name: self.name,
            field_type: self.field_type.into(),
            role: self.role.into(),
            scale: self.scale,
            scope: self.scope.into(),
            nullable: self.nullable,
            relation,
            candidates: TransformCandidates::from_bits(bits)?,
        })
    }
}

impl GroupDto {
    fn into_group(self) -> Result<GroupDescriptor> {
        if self.kind != GroupKindDto::Repeated {
            return Err(AuraError::InvalidValue("group kind"));
        }
        let mut seen = BTreeSet::new();
        let mut bits = 0u8;
        for permission in self.relationship_permissions {
            if !seen.insert(permission) {
                return Err(AuraError::InvalidValue("group relationship flags"));
            }
            bits |= PERMISSION_REGISTRY
                .iter()
                .find_map(|(dto, bit)| (*dto == permission).then_some(*bit))
                .ok_or(AuraError::InvalidValue("group relationship flags"))?;
        }
        Ok(GroupDescriptor {
            group_id: self.id,
            kind: GroupKind::Repeated,
            child_slots: self.child_slots,
            dual_domain: self.dual_domain.0.map(|dual| DualDomainDescriptor {
                discriminator_slot: dual.discriminator_slot,
                domain_count: dual.domain_count,
            }),
            relationships: RelationshipPermissions::from_bits(bits)?,
        })
    }
}

impl DerivedExpressionDto {
    fn into_expression(self) -> Result<DerivedExpression> {
        DerivedExpression::with_literals(
            self.id,
            self.output_slot,
            self.op.into(),
            self.input_slots,
            self.literals,
            0,
        )?
        .with_source(self.source.into())
    }
}

impl From<&FieldDescriptor> for FieldDto {
    fn from(field: &FieldDescriptor) -> Self {
        let relation = match field.relation {
            FieldRelation::None => RelationDto {
                kind: RelationKindDto::None,
                field_id: None,
            },
            FieldRelation::DeltaFromField(field_id) => RelationDto {
                kind: RelationKindDto::DeltaFromField,
                field_id: Some(field_id),
            },
        };
        let transform_candidates = TRANSFORM_REGISTRY
            .iter()
            .filter_map(|(dto, transform)| field.candidates.contains(*transform).then_some(*dto))
            .collect();
        Self {
            id: field.index,
            name: field.name.clone(),
            field_type: field.field_type.into(),
            role: field.role.into(),
            scale: field.scale,
            scope: field.scope.into(),
            nullable: field.nullable,
            relation,
            transform_candidates,
        }
    }
}

impl From<&GroupDescriptor> for GroupDto {
    fn from(group: &GroupDescriptor) -> Self {
        let relationship_permissions = PERMISSION_REGISTRY
            .iter()
            .filter_map(|(dto, bit)| (group.relationships.bits() & bit != 0).then_some(*dto))
            .collect();
        Self {
            id: group.group_id,
            kind: GroupKindDto::Repeated,
            child_slots: group.child_slots.clone(),
            dual_domain: RequiredNullableDualDomainDto(group.dual_domain.map(|dual| {
                DualDomainDto {
                    discriminator_slot: dual.discriminator_slot,
                    domain_count: dual.domain_count,
                }
            })),
            relationship_permissions,
        }
    }
}

impl From<&DerivedExpression> for DerivedExpressionDto {
    fn from(expression: &DerivedExpression) -> Self {
        Self {
            id: expression.expression_id,
            output_slot: expression.output_slot,
            op: expression.op.into(),
            input_slots: expression.input_slots.clone(),
            literals: expression.literals.clone(),
            source: expression.source().into(),
        }
    }
}

macro_rules! bidirectional_enum {
    ($dto:ty, $ir:ty { $($dto_variant:ident => $ir_variant:ident),+ $(,)? }) => {
        impl From<$dto> for $ir {
            fn from(value: $dto) -> Self {
                match value { $(<$dto>::$dto_variant => <$ir>::$ir_variant),+ }
            }
        }
        impl From<$ir> for $dto {
            fn from(value: $ir) -> Self {
                match value { $(<$ir>::$ir_variant => <$dto>::$dto_variant),+ }
            }
        }
    };
}

bidirectional_enum!(FieldTypeDto, FieldType {
    I8 => I8, U8 => U8, I16 => I16, U16 => U16, I32 => I32, U32 => U32,
    I64 => I64, U64 => U64, TimestampNs => TimestampNs, I128 => I128, Opaque16 => Opaque16,
    TimestampMs => TimestampMs, Utf8 => Utf8, DecimalText => DecimalText
});
bidirectional_enum!(FieldRoleDto, FieldRole {
    Timestamp => Timestamp, Sequence => Sequence, Identifier => Identifier, Side => Side,
    Price => Price, Quantity => Quantity, Value => Value, Count => Count, Flag => Flag,
    PriceAnchor => PriceAnchor, Boolean => Boolean, Enum => Enum, Bitfield => Bitfield
});
bidirectional_enum!(FieldScopeDto, FieldScope { Event => Event, Repeated => Repeated });
bidirectional_enum!(DerivedSourceDto, DerivedExpressionSource {
    External => External, Internal => Internal
});
bidirectional_enum!(DerivedOpDto, DerivedExpressionOp {
    Add => Add, Sub => Sub, Mul => Mul, Div => Div, Min => Min, Max => Max,
    AddResidual => AddResidual, SubtractResidual => SubtractResidual,
    MaxPlusResidual => MaxPlusResidual, MinMinusResidual => MinMinusResidual,
    FirstOffsetThenDelta => FirstOffsetThenDelta, MulDiv => MulDiv,
    PreviousSnapshotSameKeyResidual => PreviousSnapshotSameKeyResidual,
    PreviousMutationSameKeyResidual => PreviousMutationSameKeyResidual,
    PreviousOutputByKeyResidual => PreviousOutputByKeyResidual
});

#[cfg(test)]
mod tests {
    use super::*;

    const TINY: &str = r#"{
      "schema_format":"aura-schema",
      "schema_version":1,
      "schema_encoding":"v3",
      "name":"bounded",
      "fields":[{
        "id":0,"name":"value","type":"i64","role":"value","scale":0,
        "scope":"event","nullable":false,"relation":{"kind":"none"},
        "transform_candidates":["absolute"]
      }],
      "groups":[],
      "derived_expressions":[]
    }"#;

    #[test]
    fn bounded_serializer_counts_newline_and_never_returns_a_partial_document() {
        let schema = parse_schema_json(TINY).unwrap();
        let dto = SchemaDto::from_schema(&schema).unwrap();
        let canonical = serialize_canonical_dto_with_limit(&dto, 4096).unwrap();
        assert_eq!(Some(&b'\n'), canonical.as_bytes().last());
        assert!(serde_json::from_str::<serde_json::Value>(&canonical).is_ok());
        assert_eq!(
            canonical,
            serialize_canonical_dto_with_limit(&dto, canonical.len()).unwrap()
        );
        assert_eq!(
            serialize_canonical_dto_with_limit(&dto, canonical.len() - 1),
            Err(AuraError::InvalidValue("schema json length"))
        );

        let mut exact_writer = BoundedJsonWriter::new(4);
        exact_writer.write_all(b"1234").unwrap();
        assert_eq!(4, exact_writer.buffer.len());
        assert!(exact_writer.buffer.capacity() <= 4);
        assert!(exact_writer.write_all(b"5").is_err());
        assert_eq!(
            AuraError::InvalidValue("schema json length"),
            exact_writer.aura_error()
        );
    }

    #[test]
    fn complexity_preflight_rejects_before_binary_identity_or_dto_allocation() {
        let long_name = "x".repeat(65_000);
        let fields = (0..260u16)
            .map(|index| FieldDescriptor {
                index,
                name: format!("{long_name}{index:03}"),
                field_type: FieldType::I64,
                role: FieldRole::Value,
                scale: 0,
                scope: FieldScope::Event,
                nullable: false,
                relation: FieldRelation::None,
                candidates: TransformCandidates::empty().with(FieldTransform::Absolute),
            })
            .collect();
        let schema = SchemaDescriptor {
            schema_id: 0,
            encoding_version: SchemaEncodingVersion::V2,
            name: "oversized_canonical_json".to_owned(),
            fields,
            compact_schema_map: None,
            derived_expressions: Vec::new(),
            groups: Vec::new(),
        }
        .into_v3()
        .unwrap();
        let before = POST_PREFLIGHT_STEPS.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            schema.to_canonical_json(),
            Err(AuraError::InvalidValue("schema json length"))
        );
        let after = POST_PREFLIGHT_STEPS.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(before, after);
    }
}
