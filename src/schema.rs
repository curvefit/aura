use std::collections::BTreeSet;

use crate::{AuraError, Result};

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

/// One logical field in an Aura schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDescriptor {
    pub index: u16,
    pub name: String,
    pub field_type: FieldType,
    pub role: FieldRole,
    pub nullable: bool,
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
        mut self,
        name: impl Into<String>,
        field_type: FieldType,
        role: FieldRole,
        nullable: bool,
        relation: FieldRelation,
    ) -> Self {
        self.fields.push(FieldDescriptor {
            index: self.fields.len() as u16,
            name: name.into(),
            field_type,
            role,
            nullable,
            relation,
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
            }
        }

        Ok(SchemaDescriptor {
            schema_id: schema_hash(&self.name, &self.fields),
            name: self.name,
            fields: self.fields,
        })
    }
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
