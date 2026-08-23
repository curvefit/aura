use std::collections::{BTreeMap, BTreeSet};

use crate::bytes::{put_u16_le, put_u32_le, put_u8, ByteReader};
use crate::format::AuraContainerVersion;
use crate::header::{
    decode_derived_expression_table, encode_derived_expression_table, validate_derived_expressions,
    DerivedExpression, DerivedExpressionOp, HEADER_PREFIX_SIZE,
};
use crate::{AuraError, Result};

const SCHEMA_ENCODING_PARENT_VECTOR: u8 = 0;
const SCHEMA_ENCODING_FULL_FIELDS: u8 = 1;
const SCHEMA_ENCODING_NAMED_PARENT_VECTOR: u8 = 2;
const SCHEMA_ENCODING_NAMED_FULL_FIELDS: u8 = 3;
const SCHEMA_ENCODING_V3_NAMED_FULL_FIELDS: u8 = 4;
pub const AURA_V3_GROUP_DESCRIPTOR_TABLE_VERSION: u8 = 1;
pub const MAX_DUAL_DOMAIN_COUNT: u8 = 2;
const GROUP_DESCRIPTOR_KNOWN_RELATIONSHIP_FLAGS: u8 = 0b0000_1111;
const DECODED_SCHEMA_NAME: &str = "schema";
pub(crate) const SCHEMA_MAP_PARENT_MAX: u8 = 99;
pub(crate) const SCHEMA_MAP_TIME_SLOT: u8 = 100;
pub(crate) const SCHEMA_MAP_DERIVED_EXPR_BASE: u8 = 100;
pub(crate) const SCHEMA_MAP_DERIVED_MAX: u8 = 199;
pub(crate) const SCHEMA_MAP_DUAL_DOMAIN_GROUP: u8 = 200;
pub(crate) const SCHEMA_MAP_GROUP_BASE: u8 = 200;
pub(crate) const SCHEMA_MAP_GROUP_MAX: u8 = 239;
pub(crate) const SCHEMA_MAP_BOOL_1BIT: u8 = 241;
pub(crate) const SCHEMA_MAP_ENUM_2BIT: u8 = 242;
pub(crate) const SCHEMA_MAP_BITFIELD_8BIT: u8 = 243;
pub(crate) const SCHEMA_MAP_DO_NOT_ATTEMPT: u8 = u8::MAX;

/// Logical schema encoding selected independently of whether groups are present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum SchemaEncodingVersion {
    V2 = 2,
    V3 = 3,
}

/// Validate that a full logical schema may be serialized by a container wire version.
///
/// This is a cross-version gate: v2 containers may only carry the frozen v2
/// schema dialect, and future v3 containers may only carry explicitly v3
/// schemas. Complete v3 container serialization remains unsupported elsewhere.
pub fn validate_schema_container_compatibility(
    schema: &SchemaDescriptor,
    container_version: AuraContainerVersion,
) -> Result<()> {
    if container_version == AuraContainerVersion::V2
        && schema
            .fields
            .iter()
            .any(|field| field.field_type.is_v3_only())
    {
        return Err(AuraError::InvalidValue("v3-only field type"));
    }
    let compatible = matches!(
        (container_version, schema.encoding_version),
        (AuraContainerVersion::V2, SchemaEncodingVersion::V2)
            | (AuraContainerVersion::V3, SchemaEncodingVersion::V3)
    );
    if compatible {
        Ok(())
    } else {
        Err(AuraError::InvalidValue("schema container version"))
    }
}

/// The semantic shape of a group declared by an Aura v3 schema.
///
/// Repeated groups are column subsets of the single repeated child row owned
/// by each logical event. All groups therefore share the same authoritative
/// per-event child count; a group never introduces an independent row set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum GroupKind {
    Repeated = 1,
}

impl GroupKind {
    fn from_code(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Repeated),
            _ => Err(AuraError::InvalidValue("group kind")),
        }
    }
}

/// Relationship classes that a planner may test for a group.
///
/// These are semantic permissions, not codec or transform selections.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelationshipPermissions(u8);

impl RelationshipPermissions {
    pub const SPLIT: u8 = 0b0000_0001;
    pub const WITHIN_DOMAIN: u8 = 0b0000_0010;
    pub const ACROSS_DOMAIN_SAME_FIELD: u8 = 0b0000_0100;
    pub const JOINT_SAME_FIELD: u8 = 0b0000_1000;

    pub const fn none() -> Self {
        Self(0)
    }

    pub fn from_bits(bits: u8) -> Result<Self> {
        if bits & !GROUP_DESCRIPTOR_KNOWN_RELATIONSHIP_FLAGS != 0 {
            return Err(AuraError::InvalidValue("group relationship flags"));
        }
        Ok(Self(bits))
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn with_split(self) -> Self {
        Self(self.0 | Self::SPLIT)
    }

    pub const fn with_within_domain(self) -> Self {
        Self(self.0 | Self::WITHIN_DOMAIN)
    }

    pub const fn with_across_domain_same_field(self) -> Self {
        Self(self.0 | Self::ACROSS_DOMAIN_SAME_FIELD)
    }

    pub const fn with_joint_same_field(self) -> Self {
        Self(self.0 | Self::JOINT_SAME_FIELD)
    }

    pub const fn allows_split(self) -> bool {
        self.0 & Self::SPLIT != 0
    }

    pub const fn allows_within_domain(self) -> bool {
        self.0 & Self::WITHIN_DOMAIN != 0
    }

    pub const fn allows_across_domain_same_field(self) -> bool {
        self.0 & Self::ACROSS_DOMAIN_SAME_FIELD != 0
    }

    pub const fn allows_joint_same_field(self) -> bool {
        self.0 & Self::JOINT_SAME_FIELD != 0
    }
}

/// Dual-domain declaration attached to a repeated group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DualDomainDescriptor {
    pub discriminator_slot: u16,
    pub domain_count: u8,
}

impl DualDomainDescriptor {
    pub fn new(discriminator_slot: u16) -> Self {
        Self {
            discriminator_slot,
            domain_count: MAX_DUAL_DOMAIN_COUNT,
        }
    }
}

/// One explicitly declared group in an Aura v3 logical schema.
///
/// `child_slots` is a disjoint column subset of the event's shared repeated
/// child row. Slots must be strictly increasing in global field order. Multiple
/// groups may partition that row into disjoint column subsets, but all use the
/// same authoritative per-event child count.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GroupDescriptor {
    pub group_id: u16,
    pub kind: GroupKind,
    /// Logical slots in source order. Serialization never sorts this vector.
    pub child_slots: Vec<u16>,
    pub dual_domain: Option<DualDomainDescriptor>,
    pub relationships: RelationshipPermissions,
}

impl GroupDescriptor {
    pub fn repeated(
        group_id: u16,
        child_slots: Vec<u16>,
        relationships: RelationshipPermissions,
    ) -> Self {
        Self {
            group_id,
            kind: GroupKind::Repeated,
            child_slots,
            dual_domain: None,
            relationships,
        }
    }

    pub fn dual_domain_repeated(
        group_id: u16,
        child_slots: Vec<u16>,
        discriminator_slot: u16,
        relationships: RelationshipPermissions,
    ) -> Self {
        Self {
            group_id,
            kind: GroupKind::Repeated,
            child_slots,
            dual_domain: Some(DualDomainDescriptor::new(discriminator_slot)),
            relationships,
        }
    }
}

/// Public SDK field type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AuraType {
    Bool,
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    TimestampNanos,
    TimestampMicros,
    TimestampMillis,
    I64Scaled { scale: i8 },
    PriceI64Scaled { scale: i8 },
    EnumU8,
    FlagsU32,
    F32,
    F64,
    I128,
    Opaque16,
    Binary,
    Utf8,
    DecimalText,
}

impl AuraType {
    pub const fn is_supported(self) -> bool {
        !matches!(
            self,
            Self::F32
                | Self::F64
                | Self::I128
                | Self::Opaque16
                | Self::Binary
                | Self::Utf8
                | Self::TimestampMillis
                | Self::DecimalText
        )
    }

    pub const fn nullable_supported(self) -> bool {
        let _ = self;
        false
    }

    pub const fn byte_width(self) -> Option<usize> {
        match self {
            Self::Bool | Self::U8 | Self::I8 | Self::EnumU8 => Some(1),
            Self::U16 | Self::I16 => Some(2),
            Self::U32 | Self::I32 | Self::FlagsU32 | Self::F32 => Some(4),
            Self::U64
            | Self::I64
            | Self::TimestampNanos
            | Self::TimestampMicros
            | Self::TimestampMillis
            | Self::I64Scaled { .. }
            | Self::PriceI64Scaled { .. }
            | Self::F64 => Some(8),
            Self::I128 | Self::Opaque16 => Some(16),
            Self::Binary | Self::Utf8 | Self::DecimalText => None,
        }
    }

    const fn field_type(self) -> Option<FieldType> {
        match self {
            Self::Bool | Self::U8 | Self::EnumU8 => Some(FieldType::U8),
            Self::I8 => Some(FieldType::I8),
            Self::U16 => Some(FieldType::U16),
            Self::I16 => Some(FieldType::I16),
            Self::U32 | Self::FlagsU32 => Some(FieldType::U32),
            Self::I32 => Some(FieldType::I32),
            Self::U64 => Some(FieldType::U64),
            Self::I64 | Self::I64Scaled { .. } | Self::PriceI64Scaled { .. } => {
                Some(FieldType::I64)
            }
            Self::TimestampNanos => Some(FieldType::TimestampNs),
            Self::TimestampMicros => Some(FieldType::I64),
            Self::TimestampMillis => Some(FieldType::TimestampMs),
            Self::I128 => Some(FieldType::I128),
            Self::Opaque16 => Some(FieldType::Opaque16),
            Self::Utf8 => Some(FieldType::Utf8),
            Self::DecimalText => Some(FieldType::DecimalText),
            Self::F32 | Self::F64 | Self::Binary => None,
        }
    }

    const fn role(self) -> FieldRole {
        match self {
            Self::TimestampNanos | Self::TimestampMicros | Self::TimestampMillis => {
                FieldRole::Timestamp
            }
            Self::PriceI64Scaled { .. } => FieldRole::Price,
            Self::Bool => FieldRole::Boolean,
            Self::EnumU8 => FieldRole::Enum,
            Self::FlagsU32 => FieldRole::Bitfield,
            _ => FieldRole::Value,
        }
    }

    const fn scale(self) -> i8 {
        match self {
            Self::I64Scaled { scale } | Self::PriceI64Scaled { scale } => scale,
            Self::TimestampMicros => -6,
            _ => 0,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::TimestampNanos => "timestamp_nanos",
            Self::TimestampMicros => "timestamp_micros",
            Self::TimestampMillis => "timestamp_ms",
            Self::I64Scaled { .. } => "i64_scaled",
            Self::PriceI64Scaled { .. } => "price_i64_scaled",
            Self::EnumU8 => "enum_u8",
            Self::FlagsU32 => "flags_u32",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::I128 => "i128",
            Self::Opaque16 => "opaque16",
            Self::Binary => "binary",
            Self::Utf8 => "utf8",
            Self::DecimalText => "decimal_text",
        }
    }
}

/// Public SDK schema field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraField {
    pub id: u16,
    pub name: String,
    pub aura_type: AuraType,
    pub nullable: bool,
}

impl AuraField {
    pub fn new(id: u16, name: impl Into<String>, aura_type: AuraType) -> Self {
        Self {
            id,
            name: name.into(),
            aura_type,
            nullable: false,
        }
    }

    pub fn nullable(mut self, nullable: bool) -> Self {
        self.nullable = nullable;
        self
    }
}

/// Public SDK dynamic schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraSchema {
    descriptor: SchemaDescriptor,
    fields: Vec<AuraField>,
}

impl AuraSchema {
    pub fn builder() -> AuraSchemaBuilder {
        AuraSchemaBuilder::new("aura_schema")
    }

    pub fn named(name: impl Into<String>) -> AuraSchemaBuilder {
        AuraSchemaBuilder::new(name)
    }

    pub const fn descriptor(&self) -> &SchemaDescriptor {
        &self.descriptor
    }

    pub fn into_descriptor(self) -> SchemaDescriptor {
        self.descriptor
    }

    pub fn fields(&self) -> &[AuraField] {
        &self.fields
    }

    pub fn field(&self, name: &str) -> Option<&AuraField> {
        self.fields.iter().find(|field| field.name == name)
    }

    pub const fn hash(&self) -> u32 {
        self.descriptor.schema_id
    }

    pub fn field_count(&self) -> usize {
        self.fields.len()
    }
}

impl From<AuraSchema> for SchemaDescriptor {
    fn from(schema: AuraSchema) -> Self {
        schema.descriptor
    }
}

impl From<SchemaDescriptor> for AuraSchema {
    fn from(descriptor: SchemaDescriptor) -> Self {
        let fields = descriptor
            .fields
            .iter()
            .map(|field| AuraField {
                id: field.index,
                name: field.name.clone(),
                aura_type: aura_type_from_descriptor(field),
                nullable: field.nullable,
            })
            .collect();
        Self { descriptor, fields }
    }
}

/// Builder for public SDK schemas.
#[derive(Debug, Clone)]
pub struct AuraSchemaBuilder {
    name: String,
    fields: Vec<AuraField>,
    next_id: u16,
}

impl AuraSchemaBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            fields: Vec::new(),
            next_id: 0,
        }
    }

    pub fn field(mut self, name: impl Into<String>, aura_type: AuraType) -> Self {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.fields.push(AuraField::new(id, name, aura_type));
        self
    }

    pub fn field_with_id(mut self, id: u16, name: impl Into<String>, aura_type: AuraType) -> Self {
        self.next_id = self.next_id.max(id.saturating_add(1));
        self.fields.push(AuraField::new(id, name, aura_type));
        self
    }

    pub fn nullable_field(mut self, name: impl Into<String>, aura_type: AuraType) -> Self {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.fields
            .push(AuraField::new(id, name, aura_type).nullable(true));
        self
    }

    pub fn build(self) -> Result<AuraSchema> {
        if self.fields.is_empty() {
            return Err(AuraError::InvalidValue("schema fields"));
        }
        let mut names = BTreeSet::new();
        let mut ids = BTreeSet::new();
        let mut builder = SchemaBuilder::new(self.name);
        for (index, field) in self.fields.iter().enumerate() {
            if !names.insert(field.name.as_str()) {
                return Err(AuraError::InvalidValue("duplicate field name"));
            }
            if !ids.insert(field.id) {
                return Err(AuraError::InvalidValue("duplicate field id"));
            }
            if !field.aura_type.is_supported() {
                return Err(AuraError::InvalidValue("unsupported aura type"));
            }
            if field.nullable && !field.aura_type.nullable_supported() {
                return Err(AuraError::InvalidValue("nullable field"));
            }
            let field_type = field
                .aura_type
                .field_type()
                .ok_or(AuraError::InvalidValue("unsupported aura type"))?;
            let role = if index != 0 && field.aura_type == AuraType::TimestampNanos {
                FieldRole::Value
            } else {
                field.aura_type.role()
            };
            builder = builder.field_with_candidates(
                field.name.clone(),
                field_type,
                role,
                TransformCandidates::default_for_role(role),
            );
        }
        let descriptor = builder.finish()?.with_field_scales(
            self.fields
                .iter()
                .map(|field| field.aura_type.scale())
                .collect(),
        )?;
        Ok(AuraSchema {
            descriptor,
            fields: self.fields,
        })
    }
}

fn aura_type_from_descriptor(field: &FieldDescriptor) -> AuraType {
    match (field.field_type, field.role, field.scale) {
        (FieldType::TimestampNs, _, _) => AuraType::TimestampNanos,
        (FieldType::I64, FieldRole::Timestamp, -6) => AuraType::TimestampMicros,
        (FieldType::I64, FieldRole::Price, scale) => AuraType::PriceI64Scaled { scale },
        (FieldType::I64, _, scale) if scale != 0 => AuraType::I64Scaled { scale },
        (FieldType::I8, _, _) => AuraType::I8,
        (FieldType::U8, FieldRole::Boolean, _) => AuraType::Bool,
        (FieldType::U8, FieldRole::Enum, _) => AuraType::EnumU8,
        (FieldType::U8, _, _) => AuraType::U8,
        (FieldType::I16, _, _) => AuraType::I16,
        (FieldType::U16, _, _) => AuraType::U16,
        (FieldType::I32, _, _) => AuraType::I32,
        (FieldType::U32, FieldRole::Bitfield, _) => AuraType::FlagsU32,
        (FieldType::U32, _, _) => AuraType::U32,
        (FieldType::I64, _, _) => AuraType::I64,
        (FieldType::U64, _, _) => AuraType::U64,
        (FieldType::I128, _, _) => AuraType::I128,
        (FieldType::Opaque16, _, _) => AuraType::Opaque16,
        (FieldType::TimestampMs, _, _) => AuraType::TimestampMillis,
        (FieldType::Utf8, _, _) => AuraType::Utf8,
        (FieldType::DecimalText, _, _) => AuraType::DecimalText,
    }
}

/// Logical field type recorded by an Aura schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum FieldType {
    I8 = 1,
    U8 = 2,
    I16 = 3,
    U16 = 4,
    I32 = 5,
    U32 = 6,
    I64 = 7,
    U64 = 8,
    TimestampNs = 9,
    I128 = 10,
    Opaque16 = 11,
    TimestampMs = 12,
    Utf8 = 13,
    DecimalText = 14,
}

impl FieldType {
    pub fn from_code(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::I8),
            2 => Ok(Self::U8),
            3 => Ok(Self::I16),
            4 => Ok(Self::U16),
            5 => Ok(Self::I32),
            6 => Ok(Self::U32),
            7 => Ok(Self::I64),
            8 => Ok(Self::U64),
            9 => Ok(Self::TimestampNs),
            10 => Ok(Self::I128),
            11 => Ok(Self::Opaque16),
            12 => Ok(Self::TimestampMs),
            13 => Ok(Self::Utf8),
            14 => Ok(Self::DecimalText),
            _ => Err(AuraError::InvalidValue("field type")),
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::I8 => "i8",
            Self::U8 => "u8",
            Self::I16 => "i16",
            Self::U16 => "u16",
            Self::I32 => "i32",
            Self::U32 => "u32",
            Self::I64 => "i64",
            Self::U64 => "u64",
            Self::TimestampNs => "timestamp_ns",
            Self::I128 => "i128",
            Self::Opaque16 => "opaque16",
            Self::TimestampMs => "timestamp_ms",
            Self::Utf8 => "utf8",
            Self::DecimalText => "decimal_text",
        }
    }

    /// Whether this type belongs exclusively to the standalone Aura v3 value dialect.
    pub const fn is_v3_only(self) -> bool {
        matches!(self, Self::TimestampMs | Self::Utf8 | Self::DecimalText)
    }
}

/// Semantic role used by stats planners and physical compilers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum FieldRole {
    Timestamp = 1,
    Sequence = 2,
    Identifier = 3,
    Side = 4,
    Price = 5,
    Quantity = 6,
    Value = 7,
    Count = 8,
    Flag = 9,
    PriceAnchor = 10,
    Boolean = 11,
    Enum = 12,
    Bitfield = 13,
}

impl FieldRole {
    pub fn from_code(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Timestamp),
            2 => Ok(Self::Sequence),
            3 => Ok(Self::Identifier),
            4 => Ok(Self::Side),
            5 => Ok(Self::Price),
            6 => Ok(Self::Quantity),
            7 => Ok(Self::Value),
            8 => Ok(Self::Count),
            9 => Ok(Self::Flag),
            10 => Ok(Self::PriceAnchor),
            11 => Ok(Self::Boolean),
            12 => Ok(Self::Enum),
            13 => Ok(Self::Bitfield),
            _ => Err(AuraError::InvalidValue("field role")),
        }
    }
}

/// Positional scope encoded by the compact front-header schema map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum FieldScope {
    /// One value per logical event/record.
    Event = 0,
    /// Repeated child value inside a logical event, such as an orderbook level.
    Repeated = 1,
}

impl FieldScope {
    pub fn from_code(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Event),
            1 => Ok(Self::Repeated),
            _ => Err(AuraError::InvalidValue("field scope")),
        }
    }
}

/// Logical relationship between fields used by Aura0 planners.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum FieldRelation {
    None = 0,
    DeltaFromField(u16) = 1,
}

impl FieldRelation {
    pub const fn kind_code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::DeltaFromField(_) => 1,
        }
    }

    pub const fn related_field_index(self) -> Option<u16> {
        match self {
            Self::None => None,
            Self::DeltaFromField(index) => Some(index),
        }
    }

    pub fn from_codes(kind: u8, field_index: u16) -> Result<Self> {
        match kind {
            0 => Ok(Self::None),
            1 => Ok(Self::DeltaFromField(field_index)),
            _ => Err(AuraError::InvalidValue("field relation")),
        }
    }
}

/// Reversible transforms a schema allows the physical planner to test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum FieldTransform {
    Absolute = 0,
    DeltaBase = 1,
    DeltaPrevious = 2,
    DeltaRelated = 3,
    FixedStep = 4,
    Delta2 = 5,
    Midpoint = 6,
    RoughStep = 7,
    ZigzagVarint = 8,
    Bitpack = 9,
}

impl FieldTransform {
    pub const fn bit(self) -> u16 {
        1u16 << (self as u8)
    }
}

const KNOWN_TRANSFORM_BITS: u16 = FieldTransform::Absolute.bit()
    | FieldTransform::DeltaBase.bit()
    | FieldTransform::DeltaPrevious.bit()
    | FieldTransform::DeltaRelated.bit()
    | FieldTransform::FixedStep.bit()
    | FieldTransform::Delta2.bit()
    | FieldTransform::Midpoint.bit()
    | FieldTransform::RoughStep.bit()
    | FieldTransform::ZigzagVarint.bit()
    | FieldTransform::Bitpack.bit();

/// Bitset of transform candidates declared by a logical schema field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TransformCandidates(u16);

impl TransformCandidates {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub fn from_bits(bits: u16) -> Result<Self> {
        if bits & !KNOWN_TRANSFORM_BITS != 0 {
            return Err(AuraError::InvalidValue("transform candidates"));
        }
        Ok(Self(bits))
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn contains(self, transform: FieldTransform) -> bool {
        self.0 & transform.bit() != 0
    }

    pub const fn with(self, transform: FieldTransform) -> Self {
        Self(self.0 | transform.bit())
    }

    pub const fn default_for_role(role: FieldRole) -> Self {
        match role {
            FieldRole::Timestamp => Self::empty()
                .with(FieldTransform::Absolute)
                .with(FieldTransform::DeltaBase)
                .with(FieldTransform::DeltaPrevious)
                .with(FieldTransform::FixedStep)
                .with(FieldTransform::RoughStep),
            FieldRole::Identifier => Self::empty()
                .with(FieldTransform::Absolute)
                .with(FieldTransform::DeltaBase)
                .with(FieldTransform::DeltaPrevious)
                .with(FieldTransform::Bitpack),
            FieldRole::Side
            | FieldRole::Flag
            | FieldRole::Boolean
            | FieldRole::Enum
            | FieldRole::Bitfield => Self::empty()
                .with(FieldTransform::Absolute)
                .with(FieldTransform::DeltaBase)
                .with(FieldTransform::Bitpack),
            _ => Self::empty()
                .with(FieldTransform::Absolute)
                .with(FieldTransform::DeltaBase)
                .with(FieldTransform::DeltaPrevious)
                .with(FieldTransform::Delta2)
                .with(FieldTransform::Midpoint)
                .with(FieldTransform::ZigzagVarint)
                .with(FieldTransform::Bitpack),
        }
    }
}

/// Positional relationship used by generic integer schemas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelatedFieldMapping {
    pub field_index: u16,
    pub related_field_index: u16,
}

impl RelatedFieldMapping {
    pub const fn new(field_index: u16, related_field_index: u16) -> Self {
        Self {
            field_index,
            related_field_index,
        }
    }
}

/// One logical field in an Aura schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDescriptor {
    pub index: u16,
    pub name: String,
    pub field_type: FieldType,
    pub role: FieldRole,
    pub scale: i8,
    pub scope: FieldScope,
    pub nullable: bool,
    pub relation: FieldRelation,
    pub candidates: TransformCandidates,
}

/// Decoded generic hint carried by one compact front-header schema-map byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaMapHint {
    Root,
    Parent {
        parent_index: u16,
    },
    Timestamp,
    DerivedExpression {
        expression_index: u8,
    },
    DualDomainGroup {
        width: u8,
    },
    /// Aura v3 slot-level discriminator. Unlike the v2 control byte, this is a field.
    DualDomainDiscriminator,
    Group {
        width: u8,
    },
    Boolean {
        bits: u8,
    },
    Enum {
        bits: u8,
    },
    Bitfield {
        bits: u8,
    },
    DoNotAttempt,
}

/// Decoded meaning of one compact front-header schema-map byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaMapEntry {
    pub field_index: u16,
    pub raw_byte: u8,
    pub scope: FieldScope,
    pub is_timestamp: bool,
    pub relation: FieldRelation,
    pub hint: SchemaMapHint,
}

/// Logical schema descriptor shared by ingest, Aura0, and Aura1.
///
/// For v3, the full schema block (encoding tag 4) is authoritative for field
/// names, logical types, roles, scales, and nullability. The front header only
/// carries relationship, expression, and repeated-group declarations needed
/// before a footer/full-schema block is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDescriptor {
    pub schema_id: u32,
    pub encoding_version: SchemaEncodingVersion,
    pub name: String,
    pub fields: Vec<FieldDescriptor>,
    pub compact_schema_map: Option<Vec<u8>>,
    pub derived_expressions: Vec<DerivedExpression>,
    pub groups: Vec<GroupDescriptor>,
}

impl SchemaDescriptor {
    pub fn field(&self, name: &str) -> Option<&FieldDescriptor> {
        self.fields.iter().find(|field| field.name == name)
    }

    pub fn with_field_scales(mut self, scales: Vec<i8>) -> Result<Self> {
        if scales.len() != self.fields.len() {
            return Err(AuraError::InvalidValue("field scales"));
        }
        for (field, scale) in self.fields.iter_mut().zip(scales) {
            field.scale = scale;
        }
        self.refresh_schema_id();
        Ok(self)
    }

    pub fn with_derived_expressions(
        mut self,
        mut derived_expressions: Vec<DerivedExpression>,
    ) -> Result<Self> {
        if self.encoding_version == SchemaEncodingVersion::V3 {
            derived_expressions.sort_by_key(|expression| expression.expression_id);
        }
        validate_schema_derived_expressions(
            &self.fields,
            self.compact_schema_map.as_deref(),
            &derived_expressions,
            self.encoding_version,
            &self.groups,
        )?;
        self.derived_expressions = derived_expressions;
        self.refresh_schema_id();
        Ok(self)
    }

    /// Promote this descriptor to Aura v3 and install its authoritative groups.
    pub fn with_v3_groups(mut self, groups: Vec<GroupDescriptor>) -> Result<Self> {
        self.encoding_version = SchemaEncodingVersion::V3;
        self.derived_expressions
            .sort_by_key(|expression| expression.expression_id);
        self.groups = groups;
        self.groups.sort_by_key(|group| group.group_id);
        if self.compact_schema_map.is_none() {
            self.compact_schema_map = Some(derive_v3_schema_mapping(&self)?);
        }
        self.validate()?;
        self.refresh_schema_id();
        Ok(self)
    }

    /// Promote a flat descriptor to Aura v3 without inferring identity from groups.
    pub fn into_v3(mut self) -> Result<Self> {
        self.encoding_version = SchemaEncodingVersion::V3;
        self.derived_expressions
            .sort_by_key(|expression| expression.expression_id);
        if self.compact_schema_map.is_none() {
            self.compact_schema_map = Some(derive_v3_schema_mapping(&self)?);
        }
        self.validate()?;
        self.refresh_schema_id();
        Ok(self)
    }

    pub fn validate(&self) -> Result<()> {
        validate_stable_field_ids(&self.fields)?;
        validate_field_descriptors(&self.fields)?;
        self.validate_derived_expressions()?;
        match self.encoding_version {
            SchemaEncodingVersion::V2 => {
                if self
                    .fields
                    .iter()
                    .any(|field| field.field_type.is_v3_only())
                {
                    return Err(AuraError::InvalidValue("v3-only field type"));
                }
                if !self.groups.is_empty() {
                    return Err(AuraError::InvalidValue("v2 schema groups"));
                }
                if let Some(mapping) = self.compact_schema_map.as_deref() {
                    if decode_schema_map(mapping)?.len() != self.fields.len() {
                        return Err(AuraError::InvalidValue("schema parent mapping"));
                    }
                }
            }
            SchemaEncodingVersion::V3 => validate_v3_schema_parts(
                &self.fields,
                self.compact_schema_map.as_deref(),
                &self.groups,
            )?,
        }
        Ok(())
    }

    pub(crate) fn validate_derived_expressions(&self) -> Result<()> {
        validate_schema_derived_expressions(
            &self.fields,
            self.compact_schema_map.as_deref(),
            &self.derived_expressions,
            self.encoding_version,
            &self.groups,
        )
    }

    fn refresh_schema_id(&mut self) {
        self.schema_id = schema_hash_for_version(self);
    }
}

/// Code-defined reusable schema definition for generic positional i64 ingest.
///
/// Source adapters can keep one of these beside their mapper code, then pass the
/// schema and comment into the generic Aura writer. The emitted Aura file remains
/// self-describing through its header mapping and stamped footer schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I64SchemaDefinition {
    schema: SchemaDescriptor,
    header_comment: String,
    parent_slots: Vec<u8>,
}

impl I64SchemaDefinition {
    pub fn new(name: &str, header_comment: impl Into<String>, parent_slots: &[u8]) -> Result<Self> {
        let header_comment = header_comment.into();
        validate_i64_schema_definition_header(parent_slots.len(), header_comment.len())?;
        let schema = generic_i64_parent_schema(name, parent_slots)?;

        Ok(Self {
            schema,
            header_comment,
            parent_slots: parent_slots.to_vec(),
        })
    }

    pub fn from_field_names(name: &str, field_names: &[&str], parent_slots: &[u8]) -> Result<Self> {
        if field_names.len() != decode_schema_map(parent_slots)?.len() {
            return Err(AuraError::InvalidValue("schema field names"));
        }
        Self::new(name, field_names.join(","), parent_slots)
    }

    pub const fn schema(&self) -> &SchemaDescriptor {
        &self.schema
    }

    pub fn into_schema(self) -> SchemaDescriptor {
        self.schema
    }

    pub fn header_comment(&self) -> &str {
        &self.header_comment
    }

    pub fn parent_slots(&self) -> &[u8] {
        &self.parent_slots
    }
}

/// Builder used by schema plug-ins to define a logical Aura stream.
#[derive(Debug, Clone)]
pub struct SchemaBuilder {
    name: String,
    fields: Vec<FieldDescriptor>,
    encoding_version: SchemaEncodingVersion,
    compact_schema_map: Option<Vec<u8>>,
    groups: Vec<GroupDescriptor>,
}

impl SchemaBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            fields: Vec::new(),
            encoding_version: SchemaEncodingVersion::V2,
            compact_schema_map: None,
            groups: Vec::new(),
        }
    }

    /// Select Aura v3 schema semantics even for a flat schema.
    pub fn v3(mut self) -> Self {
        self.encoding_version = SchemaEncodingVersion::V3;
        self
    }

    pub fn group(mut self, group: GroupDescriptor) -> Self {
        self.encoding_version = SchemaEncodingVersion::V3;
        self.groups.push(group);
        self
    }

    /// Set the one-byte-per-field Aura v3 schema map.
    pub fn v3_schema_mapping(mut self, schema_mapping: Vec<u8>) -> Self {
        self.encoding_version = SchemaEncodingVersion::V3;
        self.compact_schema_map = Some(schema_mapping);
        self
    }

    pub fn repeated_group(
        self,
        group_id: u16,
        child_slots: Vec<u16>,
        relationships: RelationshipPermissions,
    ) -> Self {
        self.group(GroupDescriptor::repeated(
            group_id,
            child_slots,
            relationships,
        ))
    }

    pub fn dual_domain_repeated_group(
        self,
        group_id: u16,
        child_slots: Vec<u16>,
        discriminator_slot: u16,
        relationships: RelationshipPermissions,
    ) -> Self {
        self.group(GroupDescriptor::dual_domain_repeated(
            group_id,
            child_slots,
            discriminator_slot,
            relationships,
        ))
    }

    pub fn field(self, name: impl Into<String>, field_type: FieldType, role: FieldRole) -> Self {
        self.field_with_nullability(name, field_type, role, false)
    }

    pub fn field_with_candidates(
        self,
        name: impl Into<String>,
        field_type: FieldType,
        role: FieldRole,
        candidates: TransformCandidates,
    ) -> Self {
        self.field_with_relation_and_candidates(
            name,
            field_type,
            role,
            FieldScope::Event,
            false,
            FieldRelation::None,
            candidates,
        )
    }

    pub fn nullable_field(
        self,
        name: impl Into<String>,
        field_type: FieldType,
        role: FieldRole,
    ) -> Self {
        self.field_with_nullability(name, field_type, role, true)
    }

    pub fn field_related_to(
        self,
        name: impl Into<String>,
        field_type: FieldType,
        role: FieldRole,
        related_field_name: &str,
    ) -> Self {
        let related_index = self
            .fields
            .iter()
            .find(|field| field.name == related_field_name)
            .map(|field| field.index)
            .unwrap_or(u16::MAX);
        self.field_with_relation(
            name,
            field_type,
            role,
            FieldScope::Event,
            false,
            FieldRelation::DeltaFromField(related_index),
        )
    }

    pub fn repeated_field(
        self,
        name: impl Into<String>,
        field_type: FieldType,
        role: FieldRole,
    ) -> Self {
        self.field_with_relation(
            name,
            field_type,
            role,
            FieldScope::Repeated,
            false,
            FieldRelation::None,
        )
    }

    pub fn repeated_field_related_to(
        self,
        name: impl Into<String>,
        field_type: FieldType,
        role: FieldRole,
        related_field_name: &str,
    ) -> Self {
        let related_index = self
            .fields
            .iter()
            .find(|field| field.name == related_field_name)
            .map(|field| field.index)
            .unwrap_or(u16::MAX);
        self.field_with_relation(
            name,
            field_type,
            role,
            FieldScope::Repeated,
            false,
            FieldRelation::DeltaFromField(related_index),
        )
    }

    fn field_with_nullability(
        self,
        name: impl Into<String>,
        field_type: FieldType,
        role: FieldRole,
        nullable: bool,
    ) -> Self {
        self.field_with_relation(
            name,
            field_type,
            role,
            FieldScope::Event,
            nullable,
            FieldRelation::None,
        )
    }

    fn field_with_relation(
        self,
        name: impl Into<String>,
        field_type: FieldType,
        role: FieldRole,
        scope: FieldScope,
        nullable: bool,
        relation: FieldRelation,
    ) -> Self {
        let mut candidates = if matches!(
            field_type,
            FieldType::Opaque16 | FieldType::Utf8 | FieldType::DecimalText
        ) {
            TransformCandidates::empty().with(FieldTransform::Absolute)
        } else {
            TransformCandidates::default_for_role(role)
        };
        if matches!(relation, FieldRelation::DeltaFromField(_)) {
            candidates = candidates.with(FieldTransform::DeltaRelated);
        }
        self.field_with_relation_and_candidates(
            name, field_type, role, scope, nullable, relation, candidates,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn field_with_relation_and_candidates(
        mut self,
        name: impl Into<String>,
        field_type: FieldType,
        role: FieldRole,
        scope: FieldScope,
        nullable: bool,
        relation: FieldRelation,
        candidates: TransformCandidates,
    ) -> Self {
        self.fields.push(FieldDescriptor {
            index: self.fields.len() as u16,
            name: name.into(),
            field_type,
            role,
            scale: 0,
            scope,
            nullable,
            relation,
            candidates,
        });
        self
    }

    pub fn finish(self) -> Result<SchemaDescriptor> {
        validate_schema_name(&self.name)?;
        if self.fields.is_empty() {
            return Err(AuraError::InvalidValue("schema fields"));
        }

        let mut names = BTreeSet::new();
        for field in &self.fields {
            validate_schema_name(&field.name)?;
            if !names.insert(field.name.as_str()) {
                return Err(AuraError::InvalidValue("duplicate field name"));
            }
            if matches!(field.relation, FieldRelation::DeltaFromField(u16::MAX)) {
                return Err(AuraError::InvalidValue("related field name"));
            }
            if field.role == FieldRole::Timestamp && field.scope != FieldScope::Event {
                return Err(AuraError::InvalidValue("timestamp scope"));
            }
            if field.field_type == FieldType::Opaque16
                && field.relation != FieldRelation::None
                && field.candidates.contains(FieldTransform::DeltaRelated)
            {
                return Err(AuraError::InvalidValue("opaque field relation"));
            }
            if let FieldRelation::DeltaFromField(related_index) = field.relation {
                if usize::from(related_index) >= self.fields.len() || related_index == field.index {
                    return Err(AuraError::InvalidValue("related field index"));
                }
                if !field.candidates.contains(FieldTransform::DeltaRelated) {
                    return Err(AuraError::InvalidValue("related field candidates"));
                }
            }
        }

        let mut groups = self.groups;
        groups.sort_by_key(|group| group.group_id);
        let mut schema = SchemaDescriptor {
            schema_id: schema_hash(&self.name, &self.fields, None, &[]),
            encoding_version: self.encoding_version,
            name: self.name,
            fields: self.fields,
            compact_schema_map: self.compact_schema_map,
            derived_expressions: Vec::new(),
            groups,
        };
        if schema.encoding_version == SchemaEncodingVersion::V3
            && schema.compact_schema_map.is_none()
        {
            schema.compact_schema_map = Some(derive_v3_schema_mapping(&schema)?);
        }
        schema.validate()?;
        schema.refresh_schema_id();
        Ok(schema)
    }
}

pub fn generic_i64_schema(
    name: &str,
    value_field_count: u16,
    related_fields: &[RelatedFieldMapping],
) -> Result<SchemaDescriptor> {
    let total_field_count = value_field_count
        .checked_add(1)
        .ok_or(AuraError::InvalidValue("field count"))?;
    let mut mapped_fields = BTreeSet::new();
    for mapping in related_fields {
        if mapping.field_index == 0
            || mapping.field_index >= total_field_count
            || mapping.related_field_index >= total_field_count
            || mapping.field_index == mapping.related_field_index
            || !mapped_fields.insert(mapping.field_index)
        {
            return Err(AuraError::InvalidValue("related field index"));
        }
    }
    let mut builder =
        SchemaBuilder::new(name).field("ts", FieldType::TimestampNs, FieldRole::Timestamp);
    for field_index in 1..total_field_count {
        let relation = related_fields
            .iter()
            .find(|mapping| mapping.field_index == field_index)
            .map(|mapping| FieldRelation::DeltaFromField(mapping.related_field_index))
            .unwrap_or(FieldRelation::None);
        builder = builder.field_with_relation(
            format!("v{field_index}"),
            FieldType::I64,
            FieldRole::Value,
            FieldScope::Event,
            false,
            relation,
        );
    }
    builder.finish()
}

/// Build a positional i64 schema from compact time/parent bytes.
///
/// A byte of `100` marks the timestamp slot, normally at slot 0. If no
/// timestamp marker is present, the schema is treated as non-time-series data.
/// `0` means root, `1..99` means parent slot `value - 1`, `101..199`
/// references a header-declared derived expression `value - 100`, `200`
/// marks the immediately following repeated group as dual-domain without
/// consuming a logical field, `201..239` marks a repeated group width,
/// `241..243` marks compact leaf types, and `255` marks an opaque
/// do-not-attempt slot.
pub fn generic_i64_parent_schema(name: &str, parent_slots: &[u8]) -> Result<SchemaDescriptor> {
    let entries = decode_schema_map(parent_slots)?;
    if entries.len() > u16::MAX as usize {
        return Err(AuraError::InvalidValue("parent slot count"));
    }

    let mut builder = SchemaBuilder::new(name);
    for entry in entries {
        if entry.is_timestamp {
            builder = builder.field("ts", FieldType::TimestampNs, FieldRole::Timestamp);
            continue;
        }

        let (field_type, role) = match entry.hint {
            SchemaMapHint::Boolean { .. } => (FieldType::U8, FieldRole::Boolean),
            SchemaMapHint::Enum { .. } => (FieldType::U8, FieldRole::Enum),
            SchemaMapHint::Bitfield { .. } => (FieldType::U8, FieldRole::Bitfield),
            SchemaMapHint::DoNotAttempt => (FieldType::Opaque16, FieldRole::Identifier),
            _ => (FieldType::I64, FieldRole::Value),
        };

        builder = builder.field_with_relation(
            format!("v{}", entry.field_index),
            field_type,
            role,
            entry.scope,
            false,
            entry.relation,
        );
    }
    let mut schema = builder.finish()?;
    schema.compact_schema_map = Some(parent_slots.to_vec());
    schema.schema_id = schema_hash_for_version(&schema);
    Ok(schema)
}

pub fn decode_schema_map(parent_slots: &[u8]) -> Result<Vec<SchemaMapEntry>> {
    if parent_slots.is_empty() || parent_slots.len() > u16::MAX as usize {
        return Err(AuraError::InvalidValue("parent slot count"));
    }
    let mut entries = Vec::with_capacity(parent_slots.len());
    let mut time_slot = None;
    let mut repeated_until = None;
    let mut dual_domain_group = false;
    for (raw_index, parent_slot) in parent_slots.iter().copied().enumerate() {
        let in_group = repeated_until.is_some_and(|end| raw_index < end);
        if parent_slot == SCHEMA_MAP_DUAL_DOMAIN_GROUP {
            if dual_domain_group
                || in_group
                || !parent_slots
                    .get(raw_index + 1)
                    .is_some_and(|next| matches!(*next, 201..=SCHEMA_MAP_GROUP_MAX))
            {
                return Err(AuraError::InvalidValue("dual-domain group"));
            }
            dual_domain_group = true;
            continue;
        }

        let field_index = entries.len();
        let field_index_u16 =
            u16::try_from(field_index).map_err(|_| AuraError::InvalidValue("field index"))?;
        let entry = match parent_slot {
            SCHEMA_MAP_TIME_SLOT => {
                if time_slot.replace(field_index).is_some() || field_index != 0 {
                    return Err(AuraError::InvalidValue("time slot"));
                }
                SchemaMapEntry {
                    field_index: field_index_u16,
                    raw_byte: parent_slot,
                    scope: FieldScope::Event,
                    is_timestamp: true,
                    relation: FieldRelation::None,
                    hint: SchemaMapHint::Timestamp,
                }
            }
            0 => SchemaMapEntry {
                field_index: field_index_u16,
                raw_byte: parent_slot,
                scope: scope_for_group(in_group),
                is_timestamp: false,
                relation: FieldRelation::None,
                hint: SchemaMapHint::Root,
            },
            1..=SCHEMA_MAP_PARENT_MAX => {
                let parent_index = u16::from(parent_slot - 1);
                if usize::from(parent_index) >= field_index {
                    return Err(AuraError::InvalidValue("parent slot"));
                }
                SchemaMapEntry {
                    field_index: field_index_u16,
                    raw_byte: parent_slot,
                    scope: scope_for_group(in_group),
                    is_timestamp: false,
                    relation: FieldRelation::DeltaFromField(parent_index),
                    hint: SchemaMapHint::Parent { parent_index },
                }
            }
            101..=SCHEMA_MAP_DERIVED_MAX => SchemaMapEntry {
                field_index: field_index_u16,
                raw_byte: parent_slot,
                scope: scope_for_group(in_group),
                is_timestamp: false,
                relation: FieldRelation::None,
                hint: SchemaMapHint::DerivedExpression {
                    expression_index: parent_slot - SCHEMA_MAP_DERIVED_EXPR_BASE,
                },
            },
            201..=SCHEMA_MAP_GROUP_MAX => {
                let width = parent_slot - SCHEMA_MAP_GROUP_BASE;
                let end = raw_index
                    .checked_add(usize::from(width))
                    .ok_or(AuraError::InvalidValue("group width"))?;
                if end > parent_slots.len() {
                    return Err(AuraError::InvalidValue("group width"));
                }
                repeated_until =
                    Some(repeated_until.map_or(end, |current: usize| current.max(end)));
                let hint = if dual_domain_group {
                    dual_domain_group = false;
                    SchemaMapHint::DualDomainGroup { width }
                } else {
                    SchemaMapHint::Group { width }
                };
                SchemaMapEntry {
                    field_index: field_index_u16,
                    raw_byte: parent_slot,
                    scope: FieldScope::Repeated,
                    is_timestamp: false,
                    relation: FieldRelation::None,
                    hint,
                }
            }
            SCHEMA_MAP_BOOL_1BIT => leaf_entry(
                field_index_u16,
                parent_slot,
                in_group,
                SchemaMapHint::Boolean { bits: 1 },
            ),
            SCHEMA_MAP_ENUM_2BIT => leaf_entry(
                field_index_u16,
                parent_slot,
                in_group,
                SchemaMapHint::Enum { bits: 2 },
            ),
            SCHEMA_MAP_BITFIELD_8BIT => leaf_entry(
                field_index_u16,
                parent_slot,
                in_group,
                SchemaMapHint::Bitfield { bits: 8 },
            ),
            SCHEMA_MAP_DO_NOT_ATTEMPT => leaf_entry(
                field_index_u16,
                parent_slot,
                in_group,
                SchemaMapHint::DoNotAttempt,
            ),
            _ => return Err(AuraError::InvalidValue("schema map byte")),
        };
        entries.push(entry);
    }
    if dual_domain_group {
        return Err(AuraError::InvalidValue("dual-domain group"));
    }
    Ok(entries)
}

/// Decode an Aura v3 one-byte-per-logical-field schema map.
///
/// Group membership comes only from `groups`; bytes `201..=239` retain no v2
/// structural meaning. Byte `200` consumes the discriminator field at that slot.
pub fn decode_v3_schema_map(
    schema_mapping: &[u8],
    groups: &[GroupDescriptor],
) -> Result<Vec<SchemaMapEntry>> {
    validate_v3_group_mapping(schema_mapping, groups)?;
    let repeated_slots = groups
        .iter()
        .flat_map(|group| group.child_slots.iter().copied())
        .collect::<BTreeSet<_>>();
    let mut time_slot = None;
    let mut entries = Vec::with_capacity(schema_mapping.len());
    for (index, byte) in schema_mapping.iter().copied().enumerate() {
        let field_index =
            u16::try_from(index).map_err(|_| AuraError::InvalidValue("field index"))?;
        let scope = if repeated_slots.contains(&field_index) {
            FieldScope::Repeated
        } else {
            FieldScope::Event
        };
        let (is_timestamp, relation, hint) = match byte {
            SCHEMA_MAP_TIME_SLOT => {
                if time_slot.replace(index).is_some() || index != 0 || scope != FieldScope::Event {
                    return Err(AuraError::InvalidValue("time slot"));
                }
                (true, FieldRelation::None, SchemaMapHint::Timestamp)
            }
            0 => (false, FieldRelation::None, SchemaMapHint::Root),
            1..=SCHEMA_MAP_PARENT_MAX => {
                let parent_index = u16::from(byte - 1);
                if usize::from(parent_index) >= index {
                    return Err(AuraError::InvalidValue("parent slot"));
                }
                (
                    false,
                    FieldRelation::DeltaFromField(parent_index),
                    SchemaMapHint::Parent { parent_index },
                )
            }
            101..=SCHEMA_MAP_DERIVED_MAX => (
                false,
                FieldRelation::None,
                SchemaMapHint::DerivedExpression {
                    expression_index: byte - SCHEMA_MAP_DERIVED_EXPR_BASE,
                },
            ),
            SCHEMA_MAP_DUAL_DOMAIN_GROUP => (
                false,
                FieldRelation::None,
                SchemaMapHint::DualDomainDiscriminator,
            ),
            201..=SCHEMA_MAP_GROUP_MAX => {
                return Err(AuraError::InvalidValue("v3 structural schema map byte"));
            }
            SCHEMA_MAP_BOOL_1BIT => (
                false,
                FieldRelation::None,
                SchemaMapHint::Boolean { bits: 1 },
            ),
            SCHEMA_MAP_ENUM_2BIT => (false, FieldRelation::None, SchemaMapHint::Enum { bits: 2 }),
            SCHEMA_MAP_BITFIELD_8BIT => (
                false,
                FieldRelation::None,
                SchemaMapHint::Bitfield { bits: 8 },
            ),
            SCHEMA_MAP_DO_NOT_ATTEMPT => (false, FieldRelation::None, SchemaMapHint::DoNotAttempt),
            _ => return Err(AuraError::InvalidValue("schema map byte")),
        };
        entries.push(SchemaMapEntry {
            field_index,
            raw_byte: byte,
            scope,
            is_timestamp,
            relation,
            hint,
        });
    }
    Ok(entries)
}

fn validate_stable_field_ids(fields: &[FieldDescriptor]) -> Result<()> {
    let mut ids = BTreeSet::new();
    for (slot, field) in fields.iter().enumerate() {
        if !ids.insert(field.index) || usize::from(field.index) != slot {
            return Err(AuraError::InvalidValue("stable field id"));
        }
    }
    Ok(())
}

fn validate_field_descriptors(fields: &[FieldDescriptor]) -> Result<()> {
    if fields.is_empty() {
        return Err(AuraError::InvalidValue("schema fields"));
    }
    let mut names = BTreeSet::new();
    for field in fields {
        validate_schema_name(&field.name)?;
        if !names.insert(field.name.as_str()) {
            return Err(AuraError::InvalidValue("duplicate field name"));
        }
        if field.role == FieldRole::Timestamp && field.scope != FieldScope::Event {
            return Err(AuraError::InvalidValue("timestamp scope"));
        }
        match field.field_type {
            FieldType::TimestampMs => {
                if field.role != FieldRole::Timestamp || field.scale != 0 {
                    return Err(AuraError::InvalidValue("timestamp_ms field"));
                }
            }
            FieldType::Opaque16 | FieldType::Utf8 | FieldType::DecimalText => {
                let absolute = TransformCandidates::empty().with(FieldTransform::Absolute);
                if field.scale != 0
                    || field.relation != FieldRelation::None
                    || field.candidates != absolute
                {
                    return Err(AuraError::InvalidValue(match field.field_type {
                        FieldType::Opaque16 => "opaque16 field",
                        _ => "exact text field",
                    }));
                }
            }
            _ => {}
        }
        if let FieldRelation::DeltaFromField(related_index) = field.relation {
            if usize::from(related_index) >= fields.len() || related_index == field.index {
                return Err(AuraError::InvalidValue("related field index"));
            }
            if !field.candidates.contains(FieldTransform::DeltaRelated) {
                return Err(AuraError::InvalidValue("related field candidates"));
            }
        }
    }
    Ok(())
}

fn validate_group_descriptors_basic(groups: &[GroupDescriptor]) -> Result<()> {
    let mut group_ids = BTreeSet::new();
    let mut claimed_slots = BTreeSet::new();
    for group in groups {
        if !group_ids.insert(group.group_id) {
            return Err(AuraError::InvalidValue("duplicate group id"));
        }
        if group.kind != GroupKind::Repeated || group.child_slots.is_empty() {
            return Err(AuraError::InvalidValue("group children"));
        }
        RelationshipPermissions::from_bits(group.relationships.bits())?;
        let mut local_slots = BTreeSet::new();
        let mut previous_slot = None;
        for child_slot in &group.child_slots {
            if !local_slots.insert(*child_slot) {
                return Err(AuraError::InvalidValue("duplicate group child slot"));
            }
            if previous_slot.is_some_and(|previous| previous > *child_slot) {
                return Err(AuraError::InvalidValue("group child slot order"));
            }
            if !claimed_slots.insert(*child_slot) {
                return Err(AuraError::InvalidValue("overlapping group child slot"));
            }
            previous_slot = Some(*child_slot);
        }
        match group.dual_domain {
            Some(dual) => {
                if dual.domain_count != MAX_DUAL_DOMAIN_COUNT {
                    return Err(AuraError::InvalidValue("dual-domain count"));
                }
                if group
                    .child_slots
                    .iter()
                    .filter(|slot| **slot == dual.discriminator_slot)
                    .count()
                    != 1
                {
                    return Err(AuraError::InvalidValue("dual-domain discriminator slot"));
                }
            }
            None => {
                if group.relationships.allows_across_domain_same_field()
                    || group.relationships.allows_joint_same_field()
                {
                    return Err(AuraError::InvalidValue("group relationship flags"));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_v3_group_mapping(
    schema_mapping: &[u8],
    groups: &[GroupDescriptor],
) -> Result<()> {
    if schema_mapping.is_empty() || schema_mapping.len() > u32::MAX as usize {
        return Err(AuraError::InvalidValue("schema mapping length"));
    }
    validate_group_descriptors_basic(groups)?;
    let mut discriminator_slots = BTreeSet::new();
    for group in groups {
        for child_slot in &group.child_slots {
            if usize::from(*child_slot) >= schema_mapping.len() {
                return Err(AuraError::InvalidValue("group child slot"));
            }
        }
        if let Some(dual) = group.dual_domain {
            if !discriminator_slots.insert(dual.discriminator_slot) {
                return Err(AuraError::InvalidValue("dual-domain discriminator slot"));
            }
            if schema_mapping[usize::from(dual.discriminator_slot)] != SCHEMA_MAP_DUAL_DOMAIN_GROUP
            {
                return Err(AuraError::InvalidValue("dual-domain schema marker"));
            }
        }
    }
    for (slot, byte) in schema_mapping.iter().copied().enumerate() {
        if (201..=SCHEMA_MAP_GROUP_MAX).contains(&byte) {
            return Err(AuraError::InvalidValue("v3 structural schema map byte"));
        }
        if byte == SCHEMA_MAP_DUAL_DOMAIN_GROUP
            && !discriminator_slots
                .contains(&u16::try_from(slot).map_err(|_| AuraError::InvalidValue("field index"))?)
        {
            return Err(AuraError::InvalidValue("dual-domain schema marker"));
        }
    }
    Ok(())
}

pub(crate) fn validate_v3_header_schema(
    schema_mapping: &[u8],
    groups: &[GroupDescriptor],
    derived_expressions: &[DerivedExpression],
) -> Result<()> {
    let entries = decode_v3_schema_map(schema_mapping, groups)?;
    for expression in derived_expressions {
        if usize::from(expression.output_slot) >= schema_mapping.len()
            || expression
                .input_slots
                .iter()
                .any(|slot| usize::from(*slot) >= schema_mapping.len())
        {
            return Err(AuraError::InvalidValue("derived expression slot"));
        }
        validate_expression_shape(expression)?;
    }
    validate_compact_expression_refs(&entries, derived_expressions)?;
    validate_expression_graph(derived_expressions)
}

fn validate_v3_schema_parts(
    fields: &[FieldDescriptor],
    schema_mapping: Option<&[u8]>,
    groups: &[GroupDescriptor],
) -> Result<()> {
    validate_group_descriptors_basic(groups)?;
    let mapping = schema_mapping.ok_or(AuraError::InvalidValue("v3 schema mapping"))?;
    if mapping.len() != fields.len() {
        return Err(AuraError::InvalidValue("schema parent mapping"));
    }
    let entries = decode_v3_schema_map(mapping, groups)?;
    let claimed_slots = groups
        .iter()
        .flat_map(|group| group.child_slots.iter().copied())
        .collect::<BTreeSet<_>>();
    for (field, entry) in fields.iter().zip(entries) {
        let expected_scope = if claimed_slots.contains(&field.index) {
            FieldScope::Repeated
        } else {
            FieldScope::Event
        };
        if field.scope != expected_scope || entry.scope != expected_scope {
            return Err(AuraError::InvalidValue("group field scope"));
        }
        if entry.relation != field.relation {
            return Err(AuraError::InvalidValue("schema parent mapping"));
        }
        validate_v3_timestamp_field(field)?;
        let is_primary_timestamp = field.index == 0 && field.role == FieldRole::Timestamp;
        if entry.is_timestamp != is_primary_timestamp {
            return Err(AuraError::InvalidValue("time slot"));
        }
    }
    Ok(())
}

fn validate_v3_timestamp_field(field: &FieldDescriptor) -> Result<()> {
    if field.role != FieldRole::Timestamp {
        return Ok(());
    }
    if field.scope != FieldScope::Event {
        return Err(AuraError::InvalidValue("timestamp scope"));
    }
    let valid_type_and_scale = match field.field_type {
        FieldType::TimestampNs | FieldType::TimestampMs => field.scale == 0,
        // Scale zero retains the existing generic i64 primary timestamp form;
        // scale -6 is the public TimestampMicros representation.
        FieldType::I64 => matches!(field.scale, 0 | -6),
        _ => false,
    };
    if !valid_type_and_scale {
        return Err(AuraError::InvalidValue("v3 timestamp field"));
    }
    Ok(())
}

/// Encode the standalone, versioned Aura v3 group descriptor table.
pub fn encode_group_descriptor_table(groups: &[GroupDescriptor]) -> Result<Vec<u8>> {
    validate_group_descriptors_basic(groups)?;
    let group_count =
        u16::try_from(groups.len()).map_err(|_| AuraError::InvalidValue("group count"))?;
    let encoded_len = groups.iter().try_fold(3usize, |len, group| {
        len.checked_add(10)?
            .checked_add(group.child_slots.len().checked_mul(2)?)
    });
    let encoded_len = encoded_len.ok_or(AuraError::InvalidValue("group descriptor length"))?;
    let mut ordered = Vec::new();
    ordered
        .try_reserve_exact(groups.len())
        .map_err(|_| AuraError::InvalidValue("group descriptor allocation"))?;
    ordered.extend(groups.iter());
    ordered.sort_by_key(|group| group.group_id);
    let mut out = Vec::new();
    out.try_reserve_exact(encoded_len)
        .map_err(|_| AuraError::InvalidValue("group descriptor allocation"))?;
    put_u8(&mut out, AURA_V3_GROUP_DESCRIPTOR_TABLE_VERSION);
    put_u16_le(&mut out, group_count);
    for group in ordered {
        put_u16_le(&mut out, group.group_id);
        put_u8(&mut out, group.kind as u8);
        put_u8(&mut out, group.relationships.bits());
        put_u8(&mut out, u8::from(group.dual_domain.is_some()));
        let (domain_count, discriminator_slot) = group.dual_domain.map_or((0, u16::MAX), |dual| {
            (dual.domain_count, dual.discriminator_slot)
        });
        put_u8(&mut out, domain_count);
        put_u16_le(&mut out, discriminator_slot);
        let child_count = u16::try_from(group.child_slots.len())
            .map_err(|_| AuraError::InvalidValue("group child count"))?;
        put_u16_le(&mut out, child_count);
        for child_slot in &group.child_slots {
            put_u16_le(&mut out, *child_slot);
        }
    }
    Ok(out)
}

/// Decode the standalone, versioned Aura v3 group descriptor table.
pub fn decode_group_descriptor_table(bytes: &[u8]) -> Result<Vec<GroupDescriptor>> {
    let mut reader = ByteReader::new(bytes);
    if reader.read_u8()? != AURA_V3_GROUP_DESCRIPTOR_TABLE_VERSION {
        return Err(AuraError::InvalidValue("group descriptor table version"));
    }
    let group_count = reader.read_u16_le()? as usize;
    if group_count > reader.remaining() / 10 {
        return Err(AuraError::UnexpectedEof);
    }
    let mut groups = Vec::new();
    groups
        .try_reserve_exact(group_count)
        .map_err(|_| AuraError::InvalidValue("group descriptor allocation"))?;
    for _ in 0..group_count {
        let group_id = reader.read_u16_le()?;
        let kind = GroupKind::from_code(reader.read_u8()?)?;
        let relationships = RelationshipPermissions::from_bits(reader.read_u8()?)?;
        let descriptor_flags = reader.read_u8()?;
        if descriptor_flags & !1 != 0 {
            return Err(AuraError::InvalidValue("group descriptor flags"));
        }
        let domain_count = reader.read_u8()?;
        let discriminator_slot = reader.read_u16_le()?;
        let child_count = reader.read_u16_le()? as usize;
        if child_count > reader.remaining() / 2 {
            return Err(AuraError::UnexpectedEof);
        }
        let mut child_slots = Vec::new();
        child_slots
            .try_reserve_exact(child_count)
            .map_err(|_| AuraError::InvalidValue("group child allocation"))?;
        for _ in 0..child_count {
            child_slots.push(reader.read_u16_le()?);
        }
        let dual_domain = if descriptor_flags == 1 {
            Some(DualDomainDescriptor {
                discriminator_slot,
                domain_count,
            })
        } else {
            if domain_count != 0 || discriminator_slot != u16::MAX {
                return Err(AuraError::InvalidValue("group dual-domain fields"));
            }
            None
        };
        groups.push(GroupDescriptor {
            group_id,
            kind,
            child_slots,
            dual_domain,
            relationships,
        });
    }
    reader.finish()?;
    validate_group_descriptors_basic(&groups)?;
    groups.sort_by_key(|group| group.group_id);
    Ok(groups)
}

pub fn schema_parent_mapping(schema: &SchemaDescriptor) -> Result<Vec<u8>> {
    schema.validate_derived_expressions()?;
    if let Some(mapping) = &schema.compact_schema_map {
        let entries = match schema.encoding_version {
            SchemaEncodingVersion::V2 => decode_schema_map(mapping)?,
            SchemaEncodingVersion::V3 => decode_v3_schema_map(mapping, &schema.groups)?,
        };
        if entries.len() != schema.fields.len() {
            return Err(AuraError::InvalidValue("schema parent mapping"));
        }
        return Ok(mapping.clone());
    }

    let expression_ids = expression_ids_by_output(&schema.derived_expressions)?;
    let mut mapping = Vec::with_capacity(schema.fields.len());
    let mut index = 0;
    while index < schema.fields.len() {
        let field = &schema.fields[index];
        if field.scope == FieldScope::Repeated {
            let group_end = repeated_group_end(schema, index)?;
            let width = group_end - index;
            if width > 39 {
                return Err(AuraError::InvalidValue("schema group width"));
            }
            mapping.push(SCHEMA_MAP_GROUP_BASE + width as u8);
            for repeated_index in index + 1..group_end {
                mapping.push(schema_field_map_byte(
                    &schema.fields[repeated_index],
                    &expression_ids,
                )?);
            }
            index = group_end;
            continue;
        }

        mapping.push(schema_field_map_byte(field, &expression_ids)?);
        index += 1;
    }
    Ok(mapping)
}

fn derive_v3_schema_mapping(schema: &SchemaDescriptor) -> Result<Vec<u8>> {
    let expression_ids = expression_ids_by_output(&schema.derived_expressions)?;
    let mut mapping = schema
        .fields
        .iter()
        .map(|field| schema_field_map_byte(field, &expression_ids))
        .collect::<Result<Vec<_>>>()?;
    for dual in schema.groups.iter().filter_map(|group| group.dual_domain) {
        let byte = mapping
            .get_mut(usize::from(dual.discriminator_slot))
            .ok_or(AuraError::InvalidValue("dual-domain discriminator slot"))?;
        *byte = SCHEMA_MAP_DUAL_DOMAIN_GROUP;
    }
    Ok(mapping)
}

fn schema_field_map_byte(
    field: &FieldDescriptor,
    expression_ids: &BTreeMap<u16, u8>,
) -> Result<u8> {
    if let Some(expression_id) = expression_ids.get(&field.index) {
        return SCHEMA_MAP_DERIVED_EXPR_BASE
            .checked_add(*expression_id)
            .ok_or(AuraError::InvalidValue("schema parent mapping"));
    }
    if matches!(
        field.field_type,
        FieldType::Opaque16 | FieldType::Utf8 | FieldType::DecimalText
    ) {
        return Ok(SCHEMA_MAP_DO_NOT_ATTEMPT);
    }
    match field.role {
        FieldRole::Timestamp
            if field.index == 0
                && field.scope == FieldScope::Event
                && field.relation == FieldRelation::None =>
        {
            return Ok(SCHEMA_MAP_TIME_SLOT);
        }
        FieldRole::Timestamp => return Ok(SCHEMA_MAP_DO_NOT_ATTEMPT),
        FieldRole::Boolean if field.relation == FieldRelation::None => {
            return Ok(SCHEMA_MAP_BOOL_1BIT)
        }
        FieldRole::Enum if field.relation == FieldRelation::None => {
            return Ok(SCHEMA_MAP_ENUM_2BIT)
        }
        FieldRole::Bitfield if field.relation == FieldRelation::None => {
            return Ok(SCHEMA_MAP_BITFIELD_8BIT);
        }
        _ => {}
    }

    match field.relation {
        FieldRelation::None => Ok(0),
        FieldRelation::DeltaFromField(parent_index) => {
            if parent_index >= field.index {
                return Err(AuraError::InvalidValue("schema parent mapping"));
            }
            let parent_slot = parent_index
                .checked_add(1)
                .ok_or(AuraError::InvalidValue("schema parent mapping"))?;
            if parent_slot > u16::from(SCHEMA_MAP_PARENT_MAX) {
                return Err(AuraError::InvalidValue("schema parent mapping"));
            }
            Ok(parent_slot as u8)
        }
    }
}

fn repeated_group_end(schema: &SchemaDescriptor, start: usize) -> Result<usize> {
    let mut end = start;
    while end < schema.fields.len() && schema.fields[end].scope == FieldScope::Repeated {
        end += 1;
    }
    if end == start {
        return Err(AuraError::InvalidValue("schema group width"));
    }
    Ok(end)
}

fn scope_for_group(in_group: bool) -> FieldScope {
    if in_group {
        FieldScope::Repeated
    } else {
        FieldScope::Event
    }
}

fn leaf_entry(
    field_index: u16,
    raw_byte: u8,
    in_group: bool,
    hint: SchemaMapHint,
) -> SchemaMapEntry {
    SchemaMapEntry {
        field_index,
        raw_byte,
        scope: scope_for_group(in_group),
        is_timestamp: false,
        relation: FieldRelation::None,
        hint,
    }
}

fn validate_i64_schema_definition_header(schema_len: usize, comment_len: usize) -> Result<()> {
    if schema_len == 0 || schema_len > u8::MAX as usize {
        return Err(AuraError::InvalidValue("schema mapping"));
    }
    if comment_len > u8::MAX as usize {
        return Err(AuraError::InvalidValue("schema comment"));
    }
    if HEADER_PREFIX_SIZE + schema_len + comment_len > u16::MAX as usize {
        return Err(AuraError::InvalidValue("schema header"));
    }
    Ok(())
}

pub(crate) fn encode_schema_block(schema: &SchemaDescriptor, out: &mut Vec<u8>) -> Result<()> {
    if schema.encoding_version == SchemaEncodingVersion::V2
        && schema
            .fields
            .iter()
            .any(|field| field.field_type.is_v3_only())
    {
        return Err(AuraError::InvalidValue("v3-only field type"));
    }
    if schema.encoding_version == SchemaEncodingVersion::V3 {
        schema.validate()?;
        if schema.schema_id != schema_hash_for_version(schema) {
            return Err(AuraError::InvalidValue("schema id"));
        }
    }
    let mut schema_encoding = Vec::new();
    if schema.encoding_version == SchemaEncodingVersion::V3 {
        encode_v3_full_field_schema(schema, &mut schema_encoding)?;
    } else if let Some(parent_slots) = parent_slots_for_generic_i64_schema(schema) {
        put_u8(&mut schema_encoding, SCHEMA_ENCODING_NAMED_PARENT_VECTOR);
        put_string(&mut schema_encoding, &schema.name)?;
        put_u32_le(&mut schema_encoding, schema.schema_id);
        put_u8(&mut schema_encoding, parent_slots.len() as u8);
        schema_encoding.extend_from_slice(&parent_slots);
    } else {
        encode_full_field_schema(schema, &mut schema_encoding)?;
    }

    put_u32_len(out, schema_encoding.len(), "schema length")?;
    out.extend_from_slice(&schema_encoding);
    Ok(())
}

pub(crate) fn decode_schema_block(reader: &mut ByteReader<'_>) -> Result<SchemaDescriptor> {
    let schema_len = reader.read_u32_le()? as usize;
    let schema_bytes = reader.read_exact(schema_len)?;
    let mut schema_reader = ByteReader::new(schema_bytes);
    let schema = match schema_reader.read_u8()? {
        SCHEMA_ENCODING_PARENT_VECTOR => decode_parent_vector_schema(&mut schema_reader)?,
        SCHEMA_ENCODING_FULL_FIELDS => decode_full_field_schema(&mut schema_reader)?,
        SCHEMA_ENCODING_NAMED_PARENT_VECTOR => {
            decode_named_parent_vector_schema(&mut schema_reader)?
        }
        SCHEMA_ENCODING_NAMED_FULL_FIELDS => decode_named_full_field_schema(&mut schema_reader)?,
        SCHEMA_ENCODING_V3_NAMED_FULL_FIELDS => {
            decode_v3_named_full_field_schema(&mut schema_reader)?
        }
        _ => return Err(AuraError::InvalidValue("schema encoding")),
    };
    schema_reader.finish()?;
    Ok(schema)
}

/// Encode one independently length-prefixed full schema descriptor block.
///
/// A v3 block is the authoritative in-file declaration of field names, logical
/// types, roles, scales, and nullability.
pub fn encode_schema_descriptor(schema: &SchemaDescriptor) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    encode_schema_block(schema, &mut out)?;
    Ok(out)
}

/// Decode one independently length-prefixed schema descriptor block.
pub fn decode_schema_descriptor(bytes: &[u8]) -> Result<SchemaDescriptor> {
    let mut reader = ByteReader::new(bytes);
    let schema = decode_schema_block(&mut reader)?;
    reader.finish()?;
    Ok(schema)
}

fn parent_slots_for_generic_i64_schema(schema: &SchemaDescriptor) -> Option<Vec<u8>> {
    if schema.encoding_version != SchemaEncodingVersion::V2
        || !schema.derived_expressions.is_empty()
        || schema.fields.iter().any(|field| field.scale != 0)
    {
        return None;
    }
    if let Some(mapping) = &schema.compact_schema_map {
        return Some(mapping.clone());
    }
    if schema.fields.is_empty() || schema.fields.len() > u8::MAX as usize {
        return None;
    }

    let mut parent_slots = Vec::with_capacity(schema.fields.len());
    for (index, field) in schema.fields.iter().enumerate() {
        if field.index != index as u16 || field.nullable {
            return None;
        }
        if index == 0 {
            if field.name != "ts"
                || field.field_type != FieldType::TimestampNs
                || field.role != FieldRole::Timestamp
                || field.scope != FieldScope::Event
                || field.relation != FieldRelation::None
            {
                return None;
            }
            parent_slots.push(SCHEMA_MAP_TIME_SLOT);
            continue;
        }

        if field.name != format!("v{index}")
            || field.field_type != FieldType::I64
            || field.role != FieldRole::Value
        {
            return None;
        }
        parent_slots.push(schema_parent_mapping(schema).ok()?.get(index).copied()?);
    }

    Some(parent_slots)
}

fn decode_parent_vector_schema(reader: &mut ByteReader<'_>) -> Result<SchemaDescriptor> {
    let slot_count = reader.read_u8()? as usize;
    let parent_slots = reader.read_exact(slot_count)?;
    generic_i64_parent_schema(DECODED_SCHEMA_NAME, parent_slots)
}

fn decode_named_parent_vector_schema(reader: &mut ByteReader<'_>) -> Result<SchemaDescriptor> {
    let name = read_string(reader)?;
    let schema_id = reader.read_u32_le()?;
    let slot_count = reader.read_u8()? as usize;
    let parent_slots = reader.read_exact(slot_count)?;
    let mut schema = generic_i64_parent_schema(&name, parent_slots)?;
    schema.schema_id = schema_id;
    Ok(schema)
}

fn encode_full_field_schema(schema: &SchemaDescriptor, out: &mut Vec<u8>) -> Result<()> {
    put_u8(out, SCHEMA_ENCODING_NAMED_FULL_FIELDS);
    put_string(out, &schema.name)?;
    put_u32_le(out, schema.schema_id);
    encode_full_field_schema_payload(schema, out)
}

fn encode_full_field_schema_payload(schema: &SchemaDescriptor, out: &mut Vec<u8>) -> Result<()> {
    put_u16_len(out, schema.fields.len(), "schema field count")?;
    for field in &schema.fields {
        put_u16_le(out, field.index);
        put_u8(out, field.field_type as u8);
        put_u8(out, field.role as u8);
        put_u8(out, field.scale as u8);
        put_u8(out, field.scope as u8);
        put_u8(out, field.nullable as u8);
        put_u8(out, field.relation.kind_code());
        put_u16_le(
            out,
            field.relation.related_field_index().unwrap_or(u16::MAX),
        );
        put_u16_le(out, field.candidates.bits());
        put_string(out, &field.name)?;
    }
    if let Some(compact_schema_map) = &schema.compact_schema_map {
        put_u16_len(out, compact_schema_map.len(), "schema mapping length")?;
        out.extend_from_slice(compact_schema_map);
    } else {
        put_u16_le(out, 0);
    }
    let expression_table = encode_derived_expression_table(&schema.derived_expressions)?;
    put_u16_len(out, expression_table.len(), "derived expression length")?;
    out.extend_from_slice(&expression_table);
    Ok(())
}

fn encode_v3_full_field_schema(schema: &SchemaDescriptor, out: &mut Vec<u8>) -> Result<()> {
    put_u8(out, SCHEMA_ENCODING_V3_NAMED_FULL_FIELDS);
    put_string(out, &schema.name)?;
    put_u32_le(out, schema.schema_id);
    put_u32_len(out, schema.fields.len(), "schema field count")?;
    for field in &schema.fields {
        encode_field_descriptor(field, out)?;
    }
    let mapping = schema
        .compact_schema_map
        .as_deref()
        .ok_or(AuraError::InvalidValue("v3 schema mapping"))?;
    put_u32_len(out, mapping.len(), "schema mapping length")?;
    out.extend_from_slice(mapping);
    let mut derived_expressions = schema.derived_expressions.clone();
    derived_expressions.sort_by_key(|expression| expression.expression_id);
    let expressions = encode_derived_expression_table(&derived_expressions)?;
    put_u32_len(out, expressions.len(), "derived expression length")?;
    out.extend_from_slice(&expressions);
    let groups = encode_group_descriptor_table(&schema.groups)?;
    put_u32_len(out, groups.len(), "group descriptor length")?;
    out.extend_from_slice(&groups);
    Ok(())
}

fn encode_field_descriptor(field: &FieldDescriptor, out: &mut Vec<u8>) -> Result<()> {
    put_u16_le(out, field.index);
    put_u8(out, field.field_type as u8);
    put_u8(out, field.role as u8);
    put_u8(out, field.scale as u8);
    put_u8(out, field.scope as u8);
    put_u8(out, field.nullable as u8);
    put_u8(out, field.relation.kind_code());
    put_u16_le(
        out,
        field.relation.related_field_index().unwrap_or(u16::MAX),
    );
    put_u16_le(out, field.candidates.bits());
    put_string(out, &field.name)
}

fn decode_full_field_schema(reader: &mut ByteReader<'_>) -> Result<SchemaDescriptor> {
    decode_full_field_schema_with_name(reader, DECODED_SCHEMA_NAME, None)
}

fn decode_named_full_field_schema(reader: &mut ByteReader<'_>) -> Result<SchemaDescriptor> {
    let name = read_string(reader)?;
    let schema_id = reader.read_u32_le()?;
    decode_full_field_schema_with_name(reader, &name, Some(schema_id))
}

fn decode_v3_named_full_field_schema(reader: &mut ByteReader<'_>) -> Result<SchemaDescriptor> {
    let name = read_string(reader)?;
    let schema_id = reader.read_u32_le()?;
    let field_count = reader.read_u32_le()? as usize;
    // Every field requires at least its fixed 14 bytes, including an empty name.
    if field_count > reader.remaining() / 14 || field_count > u16::MAX as usize {
        return Err(AuraError::InvalidValue("schema field count"));
    }
    let mut fields = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        fields.push(decode_field_descriptor(reader)?);
    }
    let mapping_len = reader.read_u32_le()? as usize;
    if mapping_len != field_count || mapping_len > reader.remaining() {
        return Err(AuraError::InvalidValue("schema mapping length"));
    }
    let compact_schema_map = Some(reader.read_exact(mapping_len)?.to_vec());
    let expression_len = reader.read_u32_le()? as usize;
    if expression_len > reader.remaining() {
        return Err(AuraError::UnexpectedEof);
    }
    let mut derived_expressions =
        decode_derived_expression_table(reader.read_exact(expression_len)?)?;
    derived_expressions.sort_by_key(|expression| expression.expression_id);
    let group_len = reader.read_u32_le()? as usize;
    if group_len > reader.remaining() {
        return Err(AuraError::UnexpectedEof);
    }
    let groups = decode_group_descriptor_table(reader.read_exact(group_len)?)?;
    let mut schema = SchemaDescriptor {
        schema_id,
        encoding_version: SchemaEncodingVersion::V3,
        name,
        fields,
        compact_schema_map,
        derived_expressions,
        groups,
    };
    schema.validate()?;
    if schema_hash_for_version(&schema) != schema_id {
        return Err(AuraError::InvalidValue("schema id"));
    }
    schema.schema_id = schema_id;
    Ok(schema)
}

fn decode_field_descriptor(reader: &mut ByteReader<'_>) -> Result<FieldDescriptor> {
    let index = reader.read_u16_le()?;
    let field_type = FieldType::from_code(reader.read_u8()?)?;
    let role = FieldRole::from_code(reader.read_u8()?)?;
    let scale = reader.read_u8()? as i8;
    let scope = FieldScope::from_code(reader.read_u8()?)?;
    let nullable = match reader.read_u8()? {
        0 => false,
        1 => true,
        _ => return Err(AuraError::InvalidValue("field nullable")),
    };
    let relation_kind = reader.read_u8()?;
    let related_field_index = reader.read_u16_le()?;
    let candidates = TransformCandidates::from_bits(reader.read_u16_le()?)?;
    let name = read_string(reader)?;
    Ok(FieldDescriptor {
        index,
        name,
        field_type,
        role,
        scale,
        scope,
        nullable,
        relation: FieldRelation::from_codes(relation_kind, related_field_index)?,
        candidates,
    })
}

fn decode_full_field_schema_with_name(
    reader: &mut ByteReader<'_>,
    name: &str,
    schema_id: Option<u32>,
) -> Result<SchemaDescriptor> {
    let field_count = reader.read_u16_le()? as usize;
    if field_count > 256 || field_count > reader.remaining() / 14 {
        return Err(AuraError::InvalidValue("schema field count"));
    }
    let mut fields = Vec::new();
    fields
        .try_reserve_exact(field_count)
        .map_err(|_| AuraError::InvalidValue("schema field allocation"))?;
    for _ in 0..field_count {
        let index = reader.read_u16_le()?;
        let field_type = FieldType::from_code(reader.read_u8()?)?;
        let role = FieldRole::from_code(reader.read_u8()?)?;
        let scale = reader.read_u8()? as i8;
        let scope = FieldScope::from_code(reader.read_u8()?)?;
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
            scale,
            scope,
            nullable,
            relation: FieldRelation::from_codes(relation_kind, related_field_index)?,
            candidates,
        });
    }
    let compact_schema_map = if reader.remaining() == 0 {
        None
    } else {
        let len = reader.read_u16_le()? as usize;
        let mapping = reader.read_exact(len)?.to_vec();
        if mapping.is_empty() {
            None
        } else {
            Some(mapping)
        }
    };
    let derived_expressions = if reader.remaining() == 0 {
        Vec::new()
    } else {
        let len = reader.read_u16_le()? as usize;
        decode_derived_expression_table(reader.read_exact(len)?)?
    };
    let mut schema = schema_from_fields(name, fields, compact_schema_map, derived_expressions)?;
    if let Some(schema_id) = schema_id {
        schema.schema_id = schema_id;
    }
    Ok(schema)
}

fn schema_from_fields(
    name: &str,
    fields: Vec<FieldDescriptor>,
    compact_schema_map: Option<Vec<u8>>,
    derived_expressions: Vec<DerivedExpression>,
) -> Result<SchemaDescriptor> {
    if fields.iter().any(|field| field.field_type.is_v3_only()) {
        return Err(AuraError::InvalidValue("v3-only field type"));
    }
    validate_schema_derived_expressions(
        &fields,
        compact_schema_map.as_deref(),
        &derived_expressions,
        SchemaEncodingVersion::V2,
        &[],
    )?;
    Ok(SchemaDescriptor {
        schema_id: schema_hash(
            name,
            &fields,
            compact_schema_map.as_deref(),
            &derived_expressions,
        ),
        encoding_version: SchemaEncodingVersion::V2,
        name: name.to_owned(),
        fields,
        compact_schema_map,
        derived_expressions,
        groups: Vec::new(),
    })
}

fn validate_schema_derived_expressions(
    fields: &[FieldDescriptor],
    compact_schema_map: Option<&[u8]>,
    derived_expressions: &[DerivedExpression],
    encoding_version: SchemaEncodingVersion,
    groups: &[GroupDescriptor],
) -> Result<()> {
    validate_derived_expressions(derived_expressions)?;
    let field_count = fields.len();
    let mut output_slots = BTreeSet::new();
    for expression in derived_expressions {
        validate_expression_shape(expression)?;
        if usize::from(expression.output_slot) >= field_count {
            return Err(AuraError::InvalidValue("derived expression output"));
        }
        if !output_slots.insert(expression.output_slot) {
            return Err(AuraError::InvalidValue("derived expression output"));
        }
        if fields[usize::from(expression.output_slot)].relation != FieldRelation::None {
            return Err(AuraError::InvalidValue("derived expression output"));
        }
        if matches!(
            fields[usize::from(expression.output_slot)].field_type,
            FieldType::Opaque16 | FieldType::Utf8 | FieldType::DecimalText
        ) {
            return Err(AuraError::InvalidValue("exact value derived expression"));
        }
        for input_slot in &expression.input_slots {
            if usize::from(*input_slot) >= field_count {
                return Err(AuraError::InvalidValue("derived expression input"));
            }
            if matches!(
                fields[usize::from(*input_slot)].field_type,
                FieldType::Opaque16 | FieldType::Utf8 | FieldType::DecimalText
            ) {
                return Err(AuraError::InvalidValue("exact value derived expression"));
            }
        }
        if expression.op == DerivedExpressionOp::PreviousSnapshotSameKeyResidual
            && (fields[usize::from(expression.output_slot)].scope != FieldScope::Repeated
                || expression.input_slots.iter().any(|input_slot| {
                    fields[usize::from(*input_slot)].scope != FieldScope::Repeated
                        || *input_slot == expression.output_slot
                })
                || expression
                    .input_slots
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>()
                    .len()
                    != expression.input_slots.len())
        {
            return Err(AuraError::InvalidValue(
                "previous snapshot same-key expression",
            ));
        }
        if matches!(
            expression.op,
            DerivedExpressionOp::PreviousMutationSameKeyResidual
                | DerivedExpressionOp::PreviousOutputByKeyResidual
        ) && (fields[usize::from(expression.output_slot)].scope != FieldScope::Repeated
            || expression.input_slots.len() < 2
            || fields[usize::from(expression.input_slots[0])].scope != FieldScope::Event
            || expression.input_slots[1..].iter().any(|input_slot| {
                fields[usize::from(*input_slot)].scope != FieldScope::Repeated
                    || *input_slot == expression.output_slot
            })
            || expression
                .input_slots
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                != expression.input_slots.len())
        {
            return Err(AuraError::InvalidValue(
                "previous mutation same-key expression",
            ));
        }
    }

    if let Some(compact_schema_map) = compact_schema_map {
        let entries = match encoding_version {
            SchemaEncodingVersion::V2 => decode_schema_map(compact_schema_map)?,
            SchemaEncodingVersion::V3 => decode_v3_schema_map(compact_schema_map, groups)?,
        };
        validate_compact_expression_refs(&entries, derived_expressions)?;
    }
    validate_expression_graph(derived_expressions)
}

fn validate_expression_shape(expression: &DerivedExpression) -> Result<()> {
    match expression.op {
        DerivedExpressionOp::Add
        | DerivedExpressionOp::Mul
        | DerivedExpressionOp::Min
        | DerivedExpressionOp::Max => {
            if expression.input_slots.is_empty() && expression.literals.is_empty() {
                return Err(AuraError::InvalidValue("derived expression inputs"));
            }
        }
        DerivedExpressionOp::Sub | DerivedExpressionOp::Div => {
            if expression.input_slots.is_empty() {
                return Err(AuraError::InvalidValue("derived expression inputs"));
            }
        }
        DerivedExpressionOp::MulDiv => {
            if expression.input_slots.is_empty()
                || expression.literals.len() != 1
                || expression.literals[0] == 0
            {
                return Err(AuraError::InvalidValue("derived expression inputs"));
            }
        }
        DerivedExpressionOp::AddResidual
        | DerivedExpressionOp::SubtractResidual
        | DerivedExpressionOp::FirstOffsetThenDelta => {
            if expression.input_slots.len() != 1 || !expression.literals.is_empty() {
                return Err(AuraError::InvalidValue("derived expression inputs"));
            }
        }
        DerivedExpressionOp::MaxPlusResidual | DerivedExpressionOp::MinMinusResidual => {
            if expression.input_slots.is_empty() || !expression.literals.is_empty() {
                return Err(AuraError::InvalidValue("derived expression inputs"));
            }
        }
        DerivedExpressionOp::PreviousSnapshotSameKeyResidual => {
            if expression.input_slots.is_empty() || !expression.literals.is_empty() {
                return Err(AuraError::InvalidValue("derived expression inputs"));
            }
        }
        DerivedExpressionOp::PreviousMutationSameKeyResidual
        | DerivedExpressionOp::PreviousOutputByKeyResidual => {
            if expression.input_slots.len() < 2 || !expression.literals.is_empty() {
                return Err(AuraError::InvalidValue("derived expression inputs"));
            }
        }
    }
    Ok(())
}

fn validate_compact_expression_refs(
    entries: &[SchemaMapEntry],
    derived_expressions: &[DerivedExpression],
) -> Result<()> {
    let expressions_by_id = derived_expressions
        .iter()
        .map(|expression| (expression.expression_id, expression))
        .collect::<BTreeMap<_, _>>();
    let mut referenced_ids = BTreeSet::new();
    for entry in entries {
        let SchemaMapHint::DerivedExpression { expression_index } = entry.hint else {
            continue;
        };
        let expression = expressions_by_id
            .get(&expression_index)
            .ok_or(AuraError::InvalidValue("derived expression map"))?;
        if expression.output_slot != entry.field_index {
            return Err(AuraError::InvalidValue("derived expression map"));
        }
        referenced_ids.insert(expression_index);
    }
    for expression in derived_expressions {
        if !referenced_ids.contains(&expression.expression_id) {
            return Err(AuraError::InvalidValue("derived expression map"));
        }
    }
    Ok(())
}

fn validate_expression_graph(derived_expressions: &[DerivedExpression]) -> Result<()> {
    let graph = derived_expressions
        .iter()
        .map(|expression| (expression.output_slot, expression.input_slots.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for output_slot in graph.keys().copied() {
        visit_expression_slot(output_slot, &graph, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit_expression_slot(
    slot: u16,
    graph: &BTreeMap<u16, Vec<u16>>,
    visiting: &mut BTreeSet<u16>,
    visited: &mut BTreeSet<u16>,
) -> Result<()> {
    if visited.contains(&slot) {
        return Ok(());
    }
    if !visiting.insert(slot) {
        return Err(AuraError::InvalidValue("derived expression cycle"));
    }
    if let Some(input_slots) = graph.get(&slot) {
        for input_slot in input_slots {
            if graph.contains_key(input_slot) {
                visit_expression_slot(*input_slot, graph, visiting, visited)?;
            }
        }
    }
    visiting.remove(&slot);
    visited.insert(slot);
    Ok(())
}

fn expression_ids_by_output(
    derived_expressions: &[DerivedExpression],
) -> Result<BTreeMap<u16, u8>> {
    validate_derived_expressions(derived_expressions)?;
    let mut out = BTreeMap::new();
    for expression in derived_expressions {
        if out
            .insert(expression.output_slot, expression.expression_id)
            .is_some()
        {
            return Err(AuraError::InvalidValue("derived expression output"));
        }
    }
    Ok(out)
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

pub fn book_delta_schema() -> Result<SchemaDescriptor> {
    SchemaBuilder::new("book_delta_v1")
        .field("ts_event", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("sequence", FieldType::U64, FieldRole::Sequence)
        .field("book_id", FieldType::U8, FieldRole::Identifier)
        .field("side", FieldType::U8, FieldRole::Side)
        .field("price", FieldType::I64, FieldRole::Price)
        .field("qty_a", FieldType::I64, FieldRole::Quantity)
        .field("qty_b", FieldType::I64, FieldRole::Quantity)
        .finish()
}

pub fn tick_schema() -> Result<SchemaDescriptor> {
    SchemaBuilder::new("tick_v1")
        .field("ts_event", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("sequence", FieldType::U64, FieldRole::Sequence)
        .field("instrument_id", FieldType::U32, FieldRole::Identifier)
        .field("price", FieldType::I64, FieldRole::Price)
        .field("quantity", FieldType::I64, FieldRole::Quantity)
        .field("side", FieldType::U8, FieldRole::Side)
        .finish()
}

pub fn ohlcv_schema() -> Result<SchemaDescriptor> {
    SchemaBuilder::new("ohlcv_v1")
        .field("ts_open", FieldType::TimestampNs, FieldRole::Timestamp)
        .field("open", FieldType::I64, FieldRole::PriceAnchor)
        .field_related_to("high", FieldType::I64, FieldRole::Price, "open")
        .field_related_to("low", FieldType::I64, FieldRole::Price, "open")
        .field_related_to("close", FieldType::I64, FieldRole::Price, "open")
        .field("volume", FieldType::I64, FieldRole::Quantity)
        .finish()
}

fn validate_schema_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > u16::MAX as usize {
        return Err(AuraError::InvalidValue("schema name"));
    }
    Ok(())
}

fn schema_hash(
    name: &str,
    fields: &[FieldDescriptor],
    compact_schema_map: Option<&[u8]>,
    derived_expressions: &[DerivedExpression],
) -> u32 {
    let mut hash = schema_hash_prefix(name, fields, compact_schema_map);
    for expression in derived_expressions {
        update_expression_hash(&mut hash, expression);
    }
    hash
}

fn schema_hash_prefix(
    name: &str,
    fields: &[FieldDescriptor],
    compact_schema_map: Option<&[u8]>,
) -> u32 {
    let mut hash = 0x811c9dc5u32;
    update_hash(&mut hash, name.as_bytes());
    for field in fields {
        update_hash(&mut hash, &field.index.to_le_bytes());
        update_hash(&mut hash, field.name.as_bytes());
        update_hash(
            &mut hash,
            &[
                field.field_type as u8,
                field.role as u8,
                field.scale as u8,
                field.scope as u8,
                field.nullable as u8,
                field.relation.kind_code(),
            ],
        );
        update_hash(
            &mut hash,
            &field
                .relation
                .related_field_index()
                .unwrap_or(u16::MAX)
                .to_le_bytes(),
        );
        update_hash(&mut hash, &field.candidates.bits().to_le_bytes());
    }
    if let Some(compact_schema_map) = compact_schema_map {
        update_hash(&mut hash, compact_schema_map);
    }
    hash
}

fn update_expression_hash(hash: &mut u32, expression: &DerivedExpression) {
    update_hash(hash, &[expression.expression_id, expression.op as u8]);
    update_hash(hash, &expression.output_slot.to_le_bytes());
    update_hash(hash, &[expression.flags]);
    for input_slot in &expression.input_slots {
        update_hash(hash, &input_slot.to_le_bytes());
    }
    for literal in &expression.literals {
        update_hash(hash, &literal.to_le_bytes());
    }
}

/// Validate the stored identity used by the standalone v3 exact-value block.
///
/// Sorting allocates only borrowed references and uses fallible reservation;
/// nested schema vectors and strings are never cloned.
pub(crate) fn validate_v3_schema_identity(schema: &SchemaDescriptor) -> Result<()> {
    if schema.encoding_version != SchemaEncodingVersion::V3 {
        return Err(AuraError::InvalidValue("v3 value schema"));
    }
    schema.validate()?;
    let mut expressions = Vec::new();
    expressions
        .try_reserve_exact(schema.derived_expressions.len())
        .map_err(|_| AuraError::InvalidValue("schema identity allocation"))?;
    expressions.extend(schema.derived_expressions.iter());
    expressions.sort_by_key(|expression| expression.expression_id);
    let mut groups = Vec::new();
    groups
        .try_reserve_exact(schema.groups.len())
        .map_err(|_| AuraError::InvalidValue("schema identity allocation"))?;
    groups.extend(schema.groups.iter());
    groups.sort_by_key(|group| group.group_id);
    let hash = canonical_v3_schema_hash(schema, expressions.into_iter(), groups.into_iter());
    if hash != schema.schema_id {
        return Err(AuraError::InvalidValue("schema id"));
    }
    Ok(())
}

fn schema_hash_for_version(schema: &SchemaDescriptor) -> u32 {
    if schema.encoding_version == SchemaEncodingVersion::V3 {
        let mut expressions = schema.derived_expressions.iter().collect::<Vec<_>>();
        expressions.sort_by_key(|expression| expression.expression_id);
        let mut groups = schema.groups.iter().collect::<Vec<_>>();
        groups.sort_by_key(|group| group.group_id);
        canonical_v3_schema_hash(schema, expressions.into_iter(), groups.into_iter())
    } else {
        schema_hash(
            &schema.name,
            &schema.fields,
            schema.compact_schema_map.as_deref(),
            &schema.derived_expressions,
        )
    }
}

fn canonical_v3_schema_hash<'a>(
    schema: &SchemaDescriptor,
    expressions: impl Iterator<Item = &'a DerivedExpression>,
    groups: impl Iterator<Item = &'a GroupDescriptor>,
) -> u32 {
    let mut hash = schema_hash_prefix(
        &schema.name,
        &schema.fields,
        schema.compact_schema_map.as_deref(),
    );
    for expression in expressions {
        update_expression_hash(&mut hash, expression);
    }
    update_hash(&mut hash, b"AuraSchemaV3\0");
    update_hash(&mut hash, &(schema.groups.len() as u32).to_le_bytes());
    for group in groups {
        update_hash(&mut hash, &group.group_id.to_le_bytes());
        update_hash(
            &mut hash,
            &[
                group.kind as u8,
                group.relationships.bits(),
                u8::from(group.dual_domain.is_some()),
            ],
        );
        match group.dual_domain {
            Some(dual) => {
                update_hash(&mut hash, &[dual.domain_count]);
                update_hash(&mut hash, &dual.discriminator_slot.to_le_bytes());
            }
            None => {
                update_hash(&mut hash, &[0]);
                update_hash(&mut hash, &u16::MAX.to_le_bytes());
            }
        }
        update_hash(&mut hash, &(group.child_slots.len() as u32).to_le_bytes());
        for child_slot in &group.child_slots {
            update_hash(&mut hash, &child_slot.to_le_bytes());
        }
    }
    hash
}

fn update_hash(hash: &mut u32, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u32::from(*byte);
        *hash = hash.wrapping_mul(0x01000193);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_rejects_duplicate_field_names() {
        let result = SchemaBuilder::new("bad_v1")
            .field("ts_event", FieldType::TimestampNs, FieldRole::Timestamp)
            .field("ts_event", FieldType::U64, FieldRole::Sequence)
            .finish();

        assert_eq!(Err(AuraError::InvalidValue("duplicate field name")), result);
    }

    #[test]
    fn starter_schemas_have_stable_required_fields() {
        let book = book_delta_schema().unwrap();
        let tick = tick_schema().unwrap();
        let ohlcv = ohlcv_schema().unwrap();

        assert_eq!(Some(FieldRole::Price), book.field("price").map(|f| f.role));
        assert_eq!(
            Some(FieldRole::Quantity),
            tick.field("quantity").map(|f| f.role)
        );
        assert_eq!(Some(FieldRole::Price), ohlcv.field("close").map(|f| f.role));
        assert_ne!(book.schema_id, tick.schema_id);
        assert_ne!(tick.schema_id, ohlcv.schema_id);
    }
}
