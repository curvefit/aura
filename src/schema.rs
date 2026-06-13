use std::collections::BTreeSet;

use crate::bytes::{put_u16_le, put_u32_le, put_u8, ByteReader};
use crate::{AuraError, Result};

const SCHEMA_ENCODING_PARENT_VECTOR: u8 = 0;
const SCHEMA_ENCODING_FULL_FIELDS: u8 = 1;
const DECODED_SCHEMA_NAME: &str = "schema";
pub(crate) const SCHEMA_MAP_TIME_SLOT: u8 = u8::MAX;

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
            FieldRole::Identifier | FieldRole::Side | FieldRole::Flag => {
                Self::empty().with(FieldTransform::Absolute)
            }
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
    pub nullable: bool,
    pub relation: FieldRelation,
    pub candidates: TransformCandidates,
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
        self.field_with_relation(name, field_type, role, nullable, FieldRelation::None)
    }

    fn field_with_relation(
        self,
        name: impl Into<String>,
        field_type: FieldType,
        role: FieldRole,
        nullable: bool,
        relation: FieldRelation,
    ) -> Self {
        let mut candidates = TransformCandidates::default_for_role(role);
        if matches!(relation, FieldRelation::DeltaFromField(_)) {
            candidates = candidates.with(FieldTransform::DeltaRelated);
        }
        self.field_with_relation_and_candidates(
            name, field_type, role, nullable, relation, candidates,
        )
    }

    fn field_with_relation_and_candidates(
        mut self,
        name: impl Into<String>,
        field_type: FieldType,
        role: FieldRole,
        nullable: bool,
        relation: FieldRelation,
        candidates: TransformCandidates,
    ) -> Self {
        self.fields.push(FieldDescriptor {
            index: self.fields.len() as u16,
            name: name.into(),
            field_type,
            role,
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
/// means no related-field delta candidate, and `1..254` means the slot may
/// delta from slot `value - 1`.
pub fn generic_i64_parent_schema(name: &str, parent_slots: &[u8]) -> Result<SchemaDescriptor> {
    if parent_slots.is_empty() || parent_slots.len() > u16::MAX as usize {
        return Err(AuraError::InvalidValue("parent slot count"));
    }
    let mut mappings = Vec::new();
    let mut time_slot = None;
    for (field_index, parent_slot) in parent_slots.iter().copied().enumerate() {
        if parent_slot == SCHEMA_MAP_TIME_SLOT {
            if time_slot.replace(field_index).is_some() || field_index != 0 {
                return Err(AuraError::InvalidValue("time slot"));
            }
            continue;
        }
        if parent_slot == 0 {
            continue;
        }
        let parent_index = usize::from(parent_slot - 1);
        if parent_index >= field_index {
            return Err(AuraError::InvalidValue("parent slot"));
        }
        mappings.push(RelatedFieldMapping::new(
            field_index as u16,
            parent_index as u16,
        ));
    }
    if time_slot != Some(0) {
        return Err(AuraError::InvalidValue("time slot"));
    }
    generic_i64_schema(name, parent_slots.len() as u16 - 1, &mappings)
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

        let parent_slot = match field.relation {
            FieldRelation::None => 0,
            FieldRelation::DeltaFromField(parent_index) => {
                if parent_index as usize >= index {
                    return None;
                }
                u8::try_from(parent_index + 1).ok()?
            }
        };
        parent_slots.push(parent_slot);
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
