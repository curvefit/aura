use std::collections::BTreeSet;

use crate::bytes::{put_u16_le, put_u32_le, put_u8, ByteReader};
use crate::header::HEADER_PREFIX_SIZE;
use crate::{AuraError, Result};

const SCHEMA_ENCODING_PARENT_VECTOR: u8 = 0;
const SCHEMA_ENCODING_FULL_FIELDS: u8 = 1;
const DECODED_SCHEMA_NAME: &str = "schema";
pub(crate) const SCHEMA_MAP_TIME_SLOT: u8 = u8::MAX;
pub(crate) const SCHEMA_MAP_REPEATED_ROOT_SLOT: u8 = 128;
pub(crate) const SCHEMA_MAP_REPEATED_PARENT_BASE: u8 = 129;
pub(crate) const SCHEMA_MAP_EVENT_PARENT_MAX: u8 = 127;

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
            _ => Err(AuraError::InvalidValue("field type")),
        }
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
            FieldRole::Side | FieldRole::Flag => Self::empty()
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
    pub scope: FieldScope,
    pub nullable: bool,
    pub relation: FieldRelation,
    pub candidates: TransformCandidates,
}

/// Decoded meaning of one compact front-header schema-map byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaMapEntry {
    pub field_index: u16,
    pub scope: FieldScope,
    pub is_timestamp: bool,
    pub relation: FieldRelation,
}

/// Logical schema descriptor shared by ingest, Aura0, and Aura1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDescriptor {
    pub schema_id: u32,
    pub name: String,
    pub fields: Vec<FieldDescriptor>,
}

impl SchemaDescriptor {
    pub fn field(&self, name: &str) -> Option<&FieldDescriptor> {
        self.fields.iter().find(|field| field.name == name)
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
        if field_names.len() != parent_slots.len() {
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
}

impl SchemaBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            fields: Vec::new(),
        }
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
        let mut candidates = TransformCandidates::default_for_role(role);
        if matches!(relation, FieldRelation::DeltaFromField(_)) {
            candidates = candidates.with(FieldTransform::DeltaRelated);
        }
        self.field_with_relation_and_candidates(
            name, field_type, role, scope, nullable, relation, candidates,
        )
    }

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
            if let FieldRelation::DeltaFromField(related_index) = field.relation {
                if usize::from(related_index) >= self.fields.len() || related_index == field.index {
                    return Err(AuraError::InvalidValue("related field index"));
                }
                if !field.candidates.contains(FieldTransform::DeltaRelated) {
                    return Err(AuraError::InvalidValue("related field candidates"));
                }
            }
        }

        Ok(SchemaDescriptor {
            schema_id: schema_hash(&self.name, &self.fields),
            name: self.name,
            fields: self.fields,
        })
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
/// `parent_slots[0]` describes the timestamp slot. Every later slot is a
/// generic `i64` value. A parent byte of `255` marks the timestamp slot, `0`
/// means an event/root slot, `1..127` means an event slot that may delta from
/// slot `value - 1`, `128` means a repeated child root slot, and `129..254`
/// means a repeated child slot that may delta from slot `value - 129`.
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

        builder = builder.field_with_relation(
            format!("v{}", entry.field_index),
            FieldType::I64,
            FieldRole::Value,
            entry.scope,
            false,
            entry.relation,
        );
    }
    builder.finish()
}

pub fn decode_schema_map(parent_slots: &[u8]) -> Result<Vec<SchemaMapEntry>> {
    if parent_slots.is_empty() || parent_slots.len() > u16::MAX as usize {
        return Err(AuraError::InvalidValue("parent slot count"));
    }
    let mut entries = Vec::with_capacity(parent_slots.len());
    let mut time_slot = None;
    for (field_index, parent_slot) in parent_slots.iter().copied().enumerate() {
        let field_index_u16 =
            u16::try_from(field_index).map_err(|_| AuraError::InvalidValue("field index"))?;
        let entry = match parent_slot {
            SCHEMA_MAP_TIME_SLOT => {
                if time_slot.replace(field_index).is_some() || field_index != 0 {
                    return Err(AuraError::InvalidValue("time slot"));
                }
                SchemaMapEntry {
                    field_index: field_index_u16,
                    scope: FieldScope::Event,
                    is_timestamp: true,
                    relation: FieldRelation::None,
                }
            }
            0 => SchemaMapEntry {
                field_index: field_index_u16,
                scope: FieldScope::Event,
                is_timestamp: false,
                relation: FieldRelation::None,
            },
            1..=SCHEMA_MAP_EVENT_PARENT_MAX => {
                let parent_index = u16::from(parent_slot - 1);
                if usize::from(parent_index) >= field_index {
                    return Err(AuraError::InvalidValue("parent slot"));
                }
                SchemaMapEntry {
                    field_index: field_index_u16,
                    scope: FieldScope::Event,
                    is_timestamp: false,
                    relation: FieldRelation::DeltaFromField(parent_index),
                }
            }
            SCHEMA_MAP_REPEATED_ROOT_SLOT => SchemaMapEntry {
                field_index: field_index_u16,
                scope: FieldScope::Repeated,
                is_timestamp: false,
                relation: FieldRelation::None,
            },
            SCHEMA_MAP_REPEATED_PARENT_BASE..=254 => {
                let parent_index = u16::from(parent_slot - SCHEMA_MAP_REPEATED_PARENT_BASE);
                if usize::from(parent_index) >= field_index {
                    return Err(AuraError::InvalidValue("parent slot"));
                }
                SchemaMapEntry {
                    field_index: field_index_u16,
                    scope: FieldScope::Repeated,
                    is_timestamp: false,
                    relation: FieldRelation::DeltaFromField(parent_index),
                }
            }
        };
        entries.push(entry);
    }
    if time_slot != Some(0) {
        return Err(AuraError::InvalidValue("time slot"));
    }
    Ok(entries)
}

pub fn schema_parent_mapping(schema: &SchemaDescriptor) -> Result<Vec<u8>> {
    let mut mapping = Vec::with_capacity(schema.fields.len());
    for (position, field) in schema.fields.iter().enumerate() {
        if usize::from(field.index) != position {
            return Err(AuraError::InvalidValue("schema field index"));
        }
        let parent = match (field.role, field.scope, field.relation) {
            (FieldRole::Timestamp, FieldScope::Event, FieldRelation::None) => SCHEMA_MAP_TIME_SLOT,
            (FieldRole::Timestamp, _, _) => {
                return Err(AuraError::InvalidValue("schema time mapping"));
            }
            (_, FieldScope::Event, FieldRelation::None) => 0,
            (_, FieldScope::Event, FieldRelation::DeltaFromField(parent_index)) => {
                if parent_index >= field.index {
                    return Err(AuraError::InvalidValue("schema parent mapping"));
                }
                let parent_slot = parent_index
                    .checked_add(1)
                    .ok_or(AuraError::InvalidValue("schema parent mapping"))?;
                if parent_slot > u16::from(SCHEMA_MAP_EVENT_PARENT_MAX) {
                    return Err(AuraError::InvalidValue("schema parent mapping"));
                }
                parent_slot as u8
            }
            (_, FieldScope::Repeated, FieldRelation::None) => SCHEMA_MAP_REPEATED_ROOT_SLOT,
            (_, FieldScope::Repeated, FieldRelation::DeltaFromField(parent_index)) => {
                if parent_index >= field.index {
                    return Err(AuraError::InvalidValue("schema parent mapping"));
                }
                let parent_slot = parent_index
                    .checked_add(u16::from(SCHEMA_MAP_REPEATED_PARENT_BASE))
                    .ok_or(AuraError::InvalidValue("schema parent mapping"))?;
                if parent_slot >= u16::from(SCHEMA_MAP_TIME_SLOT) {
                    return Err(AuraError::InvalidValue("schema parent mapping"));
                }
                parent_slot as u8
            }
        };
        mapping.push(parent);
    }
    Ok(mapping)
}

fn validate_i64_schema_definition_header(schema_len: usize, comment_len: usize) -> Result<()> {
    if schema_len == 0 || schema_len > u8::MAX as usize {
        return Err(AuraError::InvalidValue("schema mapping"));
    }
    if comment_len > u8::MAX as usize {
        return Err(AuraError::InvalidValue("schema comment"));
    }
    if HEADER_PREFIX_SIZE + schema_len + comment_len > u8::MAX as usize {
        return Err(AuraError::InvalidValue("schema header"));
    }
    Ok(())
}

pub(crate) fn encode_schema_block(schema: &SchemaDescriptor, out: &mut Vec<u8>) -> Result<()> {
    let mut schema_encoding = Vec::new();
    if let Some(parent_slots) = parent_slots_for_generic_i64_schema(schema) {
        put_u8(&mut schema_encoding, SCHEMA_ENCODING_PARENT_VECTOR);
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
        _ => return Err(AuraError::InvalidValue("schema encoding")),
    };
    schema_reader.finish()?;
    Ok(schema)
}

fn parent_slots_for_generic_i64_schema(schema: &SchemaDescriptor) -> Option<Vec<u8>> {
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

fn encode_full_field_schema(schema: &SchemaDescriptor, out: &mut Vec<u8>) -> Result<()> {
    put_u8(out, SCHEMA_ENCODING_FULL_FIELDS);
    put_u16_len(out, schema.fields.len(), "schema field count")?;
    for field in &schema.fields {
        put_u16_le(out, field.index);
        put_u8(out, field.field_type as u8);
        put_u8(out, field.role as u8);
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
    Ok(())
}

fn decode_full_field_schema(reader: &mut ByteReader<'_>) -> Result<SchemaDescriptor> {
    let field_count = reader.read_u16_le()? as usize;
    let mut fields = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        let index = reader.read_u16_le()?;
        let field_type = FieldType::from_code(reader.read_u8()?)?;
        let role = FieldRole::from_code(reader.read_u8()?)?;
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
            scope,
            nullable,
            relation: FieldRelation::from_codes(relation_kind, related_field_index)?,
            candidates,
        });
    }
    Ok(schema_from_fields(DECODED_SCHEMA_NAME, fields))
}

fn schema_from_fields(name: &str, fields: Vec<FieldDescriptor>) -> SchemaDescriptor {
    SchemaDescriptor {
        schema_id: schema_hash(name, &fields),
        name: name.to_owned(),
        fields,
    }
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

fn schema_hash(name: &str, fields: &[FieldDescriptor]) -> u32 {
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
