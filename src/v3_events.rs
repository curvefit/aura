//! Standalone Aura v3 grouped exact-event reference block.
//!
//! `AURAV3EB` is deliberately not an Aura0 container. It defines exact
//! event/child boundaries and scoped scalar columns for the smallest grouped
//! subset needed by order-book research.

use sha2::{Digest, Sha256};

use crate::bytes::ByteReader;
use crate::schema::{
    FieldDescriptor, FieldRole, FieldScope, FieldType, GroupKind, SchemaDescriptor,
    SchemaEncodingVersion,
};
use crate::v3_values::{
    canonical_v3_schema_fingerprint, decode_column, encode_column, encoded_v3_column_len,
    hash_present_value, is_present, validate_v3_column_exact, AuraV3Column, AuraV3ColumnValues,
    V3ValueLimits, COLUMN_HEADER_BYTES, MAX_V3_VALUE_BLOCK_BYTES, MAX_V3_VARIABLE_VALUE_BYTES,
};
use crate::{AuraError, Result};

const MAGIC: &[u8; 8] = b"AURAV3EB";
pub const V3_EVENT_BLOCK_VERSION: u16 = 1;
const HEADER_BYTES: usize = 76;
const HASH_DOMAIN: &[u8] = b"aura-v3-canonical-exact-events-v1\0";
const DUAL_DOMAIN_SCHEMA_MARKER: u8 = 200;

pub const MAX_V3_EVENT_EVENTS: usize = 4 * 1024 * 1024;
pub const MAX_V3_EVENT_CHILDREN: usize = 16 * 1024 * 1024;
pub const MAX_V3_EVENT_VALUES: usize = 64 * 1024 * 1024;
pub const MAX_V3_EVENT_OFFSETS_BYTES: usize = (MAX_V3_EVENT_EVENTS + 1) * 4;
pub const MAX_V3_EVENT_BLOCK_BYTES: usize = MAX_V3_VALUE_BLOCK_BYTES;
pub const DEFAULT_V3_EVENT_EVENTS: usize = 1024 * 1024;
pub const DEFAULT_V3_EVENT_CHILDREN: usize = 4 * 1024 * 1024;
pub const DEFAULT_V3_EVENT_VALUES: usize = 16 * 1024 * 1024;
pub const DEFAULT_V3_EVENT_BLOCK_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V3EventLimits {
    pub max_block_bytes: usize,
    pub max_variable_value_bytes: usize,
    pub max_events: usize,
    pub max_children: usize,
    pub max_values: usize,
}

impl V3EventLimits {
    /// Conservative all-memory defaults. The hard format envelope requires
    /// callers to opt in explicitly through [`Self::HARD`].
    pub const DEFAULT_IN_MEMORY: Self = Self {
        max_block_bytes: DEFAULT_V3_EVENT_BLOCK_BYTES,
        max_variable_value_bytes: MAX_V3_VARIABLE_VALUE_BYTES,
        max_events: DEFAULT_V3_EVENT_EVENTS,
        max_children: DEFAULT_V3_EVENT_CHILDREN,
        max_values: DEFAULT_V3_EVENT_VALUES,
    };

    pub const HARD: Self = Self {
        max_block_bytes: MAX_V3_EVENT_BLOCK_BYTES,
        max_variable_value_bytes: MAX_V3_VARIABLE_VALUE_BYTES,
        max_events: MAX_V3_EVENT_EVENTS,
        max_children: MAX_V3_EVENT_CHILDREN,
        max_values: MAX_V3_EVENT_VALUES,
    };

    pub(crate) const fn effective(self) -> Self {
        Self {
            max_block_bytes: min_usize(self.max_block_bytes, MAX_V3_EVENT_BLOCK_BYTES),
            max_variable_value_bytes: min_usize(
                self.max_variable_value_bytes,
                MAX_V3_VARIABLE_VALUE_BYTES,
            ),
            max_events: min_usize(self.max_events, MAX_V3_EVENT_EVENTS),
            max_children: min_usize(self.max_children, MAX_V3_EVENT_CHILDREN),
            max_values: min_usize(self.max_values, MAX_V3_EVENT_VALUES),
        }
    }

    fn scalar_limits(self) -> V3ValueLimits {
        V3ValueLimits {
            max_block_bytes: self.max_block_bytes,
            max_variable_value_bytes: self.max_variable_value_bytes,
            max_rows: self.max_children.max(self.max_events),
        }
    }
}

impl Default for V3EventLimits {
    fn default() -> Self {
        Self::DEFAULT_IN_MEMORY
    }
}

const fn min_usize(left: usize, right: usize) -> usize {
    if left < right {
        left
    } else {
        right
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraV3EventBatch {
    pub schema_id: u32,
    pub event_count: u32,
    /// Authoritative child boundaries. Length is exactly `event_count + 1`.
    pub child_offsets: Vec<u32>,
    pub event_columns: Vec<AuraV3Column>,
    pub repeated_columns: Vec<AuraV3Column>,
}

impl AuraV3EventBatch {
    pub fn child_count(&self) -> u32 {
        self.child_offsets.last().copied().unwrap_or(0)
    }
}

/// Validate the deliberately narrow schema contract accepted by the grouped
/// exact-event reference block.
pub fn validate_v3_grouped_exact_subset(schema: &SchemaDescriptor) -> Result<()> {
    if schema.encoding_version != SchemaEncodingVersion::V3
        || !schema.derived_expressions.is_empty()
    {
        return Err(AuraError::InvalidValue("v3 grouped exact schema"));
    }
    crate::schema::validate_v3_schema_identity(schema)?;
    let mapping = schema
        .compact_schema_map
        .as_deref()
        .ok_or(AuraError::InvalidValue("v3 grouped schema map"))?;
    if mapping.len() != schema.fields.len() {
        return Err(AuraError::InvalidValue("v3 grouped schema map"));
    }
    let [group] = schema.groups.as_slice() else {
        return Err(AuraError::InvalidValue("v3 grouped group count"));
    };
    if group.kind != GroupKind::Repeated {
        return Err(AuraError::InvalidValue("v3 grouped group kind"));
    }
    let repeated_slots = schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Repeated)
        .map(|field| field.index)
        .collect::<Vec<_>>();
    if repeated_slots.is_empty() || group.child_slots != repeated_slots {
        return Err(AuraError::InvalidValue("v3 grouped repeated fields"));
    }
    let dual = group
        .dual_domain
        .ok_or(AuraError::InvalidValue("v3 grouped dual domain"))?;
    if dual.domain_count != 2 || !group.child_slots.contains(&dual.discriminator_slot) {
        return Err(AuraError::InvalidValue("v3 grouped dual domain"));
    }
    let discriminator = schema
        .fields
        .get(usize::from(dual.discriminator_slot))
        .filter(|field| field.index == dual.discriminator_slot)
        .ok_or(AuraError::InvalidValue("v3 grouped discriminator slot"))?;
    if discriminator.scope != FieldScope::Repeated
        || discriminator.field_type != FieldType::U8
        || discriminator.role != FieldRole::Side
        || discriminator.nullable
        || mapping.get(usize::from(discriminator.index)).copied() != Some(DUAL_DOMAIN_SCHEMA_MARKER)
    {
        return Err(AuraError::InvalidValue("v3 grouped discriminator"));
    }
    Ok(())
}

pub fn validate_v3_event_batch(
    schema: &SchemaDescriptor,
    batch: &AuraV3EventBatch,
    limits: V3EventLimits,
) -> Result<()> {
    let limits = limits.effective();
    validate_v3_grouped_exact_subset(schema)?;
    if batch.schema_id != schema.schema_id {
        return Err(AuraError::InvalidValue("v3 event schema id"));
    }
    let events = usize::try_from(batch.event_count)
        .map_err(|_| AuraError::InvalidValue("v3 event count"))?;
    if events > limits.max_events {
        return Err(AuraError::InvalidValue("v3 event count"));
    }
    let offset_count = events
        .checked_add(1)
        .ok_or(AuraError::InvalidValue("v3 event offsets"))?;
    if batch.child_offsets.len() != offset_count || batch.child_offsets.first() != Some(&0) {
        return Err(AuraError::InvalidValue("v3 event offsets"));
    }
    let mut previous = 0u32;
    for offset in &batch.child_offsets {
        if *offset < previous {
            return Err(AuraError::InvalidValue("v3 event offsets"));
        }
        previous = *offset;
    }
    let children =
        usize::try_from(previous).map_err(|_| AuraError::InvalidValue("v3 child count"))?;
    if children > limits.max_children {
        return Err(AuraError::InvalidValue("v3 child count"));
    }
    let (event_fields, repeated_fields) = scoped_fields(schema);
    if batch.event_columns.len() != event_fields.len()
        || batch.repeated_columns.len() != repeated_fields.len()
    {
        return Err(AuraError::InvalidValue("v3 event column count"));
    }
    let total_values =
        checked_total_values(events, event_fields.len(), children, repeated_fields.len())?;
    if total_values > limits.max_values {
        return Err(AuraError::InvalidValue("v3 event value count"));
    }
    validate_scoped_columns(
        &event_fields,
        &batch.event_columns,
        events,
        limits.max_variable_value_bytes,
    )?;
    validate_scoped_columns(
        &repeated_fields,
        &batch.repeated_columns,
        children,
        limits.max_variable_value_bytes,
    )?;
    validate_runtime_side(schema, &batch.repeated_columns)?;
    let length = encoded_event_len(batch, events)?;
    if length > limits.max_block_bytes {
        return Err(AuraError::InvalidValue("v3 event block length"));
    }
    Ok(())
}

pub fn encode_v3_event_block(
    schema: &SchemaDescriptor,
    batch: &AuraV3EventBatch,
    limits: V3EventLimits,
) -> Result<Vec<u8>> {
    validate_v3_event_batch(schema, batch, limits)?;
    let events = usize::try_from(batch.event_count)
        .map_err(|_| AuraError::InvalidValue("v3 event count"))?;
    let children = batch.child_count();
    let total_len = encoded_event_len(batch, events)?;
    let fingerprint = canonical_v3_schema_fingerprint(schema)?;
    let offsets_len = batch
        .child_offsets
        .len()
        .checked_mul(4)
        .ok_or(AuraError::InvalidValue("v3 event offsets length"))?;
    let mut out = Vec::new();
    out.try_reserve_exact(total_len)
        .map_err(|_| AuraError::InvalidValue("v3 event allocation"))?;
    out.extend_from_slice(MAGIC);
    put_u16(&mut out, V3_EVENT_BLOCK_VERSION);
    put_u16(&mut out, 0);
    put_u32(&mut out, batch.schema_id);
    out.extend_from_slice(&fingerprint);
    put_u32(&mut out, batch.event_count);
    put_u32(&mut out, children);
    put_u32_len(&mut out, batch.event_columns.len(), "v3 event column count")?;
    put_u32_len(
        &mut out,
        batch.repeated_columns.len(),
        "v3 repeated column count",
    )?;
    put_u32_len(&mut out, offsets_len, "v3 event offsets length")?;
    put_u64_len(&mut out, total_len, "v3 event block length")?;
    for offset in &batch.child_offsets {
        put_u32(&mut out, *offset);
    }
    for column in &batch.event_columns {
        encode_column(column, events, &mut out)?;
    }
    let children =
        usize::try_from(children).map_err(|_| AuraError::InvalidValue("v3 child count"))?;
    for column in &batch.repeated_columns {
        encode_column(column, children, &mut out)?;
    }
    debug_assert_eq!(out.len(), total_len);
    Ok(out)
}

pub fn decode_v3_event_block(
    schema: &SchemaDescriptor,
    bytes: &[u8],
    limits: V3EventLimits,
) -> Result<AuraV3EventBatch> {
    let limits = limits.effective();
    if bytes.len() > limits.max_block_bytes || bytes.len() > MAX_V3_EVENT_BLOCK_BYTES {
        return Err(AuraError::InvalidValue("v3 event block length"));
    }
    if bytes.len() < HEADER_BYTES {
        return Err(AuraError::UnexpectedEof);
    }
    validate_v3_grouped_exact_subset(schema)?;
    let mut reader = ByteReader::new(bytes);
    if reader.read_exact(MAGIC.len())? != MAGIC {
        return Err(AuraError::InvalidMagic {
            expected: "AURAV3EB",
        });
    }
    let version = reader.read_u16_le()?;
    if version != V3_EVENT_BLOCK_VERSION {
        return Err(AuraError::UnsupportedVersion(version));
    }
    if reader.read_u16_le()? != 0 {
        return Err(AuraError::InvalidValue("v3 event flags"));
    }
    let schema_id = reader.read_u32_le()?;
    if schema_id != schema.schema_id {
        return Err(AuraError::InvalidValue("v3 event schema id"));
    }
    let fingerprint = canonical_v3_schema_fingerprint(schema)?;
    if reader.read_exact(fingerprint.len())? != fingerprint {
        return Err(AuraError::InvalidValue("v3 event schema fingerprint"));
    }
    let event_count = reader.read_u32_le()?;
    let child_count = reader.read_u32_le()?;
    let events =
        usize::try_from(event_count).map_err(|_| AuraError::InvalidValue("v3 event count"))?;
    let children =
        usize::try_from(child_count).map_err(|_| AuraError::InvalidValue("v3 child count"))?;
    if events > limits.max_events || children > limits.max_children {
        return Err(AuraError::InvalidValue("v3 event count"));
    }
    let event_column_count = usize::try_from(reader.read_u32_le()?)
        .map_err(|_| AuraError::InvalidValue("v3 event column count"))?;
    let repeated_column_count = usize::try_from(reader.read_u32_le()?)
        .map_err(|_| AuraError::InvalidValue("v3 repeated column count"))?;
    let (event_fields, repeated_fields) = scoped_fields(schema);
    if event_column_count != event_fields.len() || repeated_column_count != repeated_fields.len() {
        return Err(AuraError::InvalidValue("v3 event column count"));
    }
    let value_count =
        checked_total_values(events, event_column_count, children, repeated_column_count)?;
    if value_count > limits.max_values {
        return Err(AuraError::InvalidValue("v3 event value count"));
    }
    let offsets_len = usize::try_from(reader.read_u32_le()?)
        .map_err(|_| AuraError::InvalidValue("v3 event offsets length"))?;
    let expected_offsets_len = events
        .checked_add(1)
        .and_then(|value| value.checked_mul(4))
        .ok_or(AuraError::InvalidValue("v3 event offsets length"))?;
    if offsets_len != expected_offsets_len || offsets_len > MAX_V3_EVENT_OFFSETS_BYTES {
        return Err(AuraError::InvalidValue("v3 event offsets length"));
    }
    let declared_len = reader.read_u64_le()?;
    if declared_len
        != u64::try_from(bytes.len())
            .map_err(|_| AuraError::InvalidValue("v3 event block length"))?
    {
        return Err(AuraError::InvalidValue("v3 event block length"));
    }
    let column_count = event_column_count
        .checked_add(repeated_column_count)
        .ok_or(AuraError::InvalidValue("v3 event column count"))?;
    let minimum_len = HEADER_BYTES
        .checked_add(offsets_len)
        .and_then(|value| value.checked_add(column_count.checked_mul(COLUMN_HEADER_BYTES)?))
        .ok_or(AuraError::InvalidValue("v3 event block length"))?;
    if minimum_len > bytes.len() {
        return Err(AuraError::UnexpectedEof);
    }
    let offsets_bytes = reader.read_exact(offsets_len)?;
    let mut child_offsets = Vec::new();
    child_offsets
        .try_reserve_exact(events + 1)
        .map_err(|_| AuraError::InvalidValue("v3 event allocation"))?;
    let mut previous = 0u32;
    for (index, chunk) in offsets_bytes.chunks_exact(4).enumerate() {
        let offset = u32::from_le_bytes(
            chunk
                .try_into()
                .map_err(|_| AuraError::InvalidValue("v3 event offsets"))?,
        );
        if (index == 0 && offset != 0) || offset < previous || offset > child_count {
            return Err(AuraError::InvalidValue("v3 event offsets"));
        }
        previous = offset;
        child_offsets.push(offset);
    }
    if previous != child_count {
        return Err(AuraError::InvalidValue("v3 event offsets"));
    }
    let scalar_limits = limits.scalar_limits().effective();
    let mut event_columns = Vec::new();
    event_columns
        .try_reserve_exact(event_column_count)
        .map_err(|_| AuraError::InvalidValue("v3 event allocation"))?;
    for field in &event_fields {
        event_columns.push(decode_column(
            field.index,
            field.field_type,
            field.nullable,
            events,
            &mut reader,
            scalar_limits,
        )?);
    }
    let mut repeated_columns = Vec::new();
    repeated_columns
        .try_reserve_exact(repeated_column_count)
        .map_err(|_| AuraError::InvalidValue("v3 event allocation"))?;
    for field in &repeated_fields {
        repeated_columns.push(decode_column(
            field.index,
            field.field_type,
            field.nullable,
            children,
            &mut reader,
            scalar_limits,
        )?);
    }
    reader.finish()?;
    let batch = AuraV3EventBatch {
        schema_id,
        event_count,
        child_offsets,
        event_columns,
        repeated_columns,
    };
    validate_v3_event_batch(schema, &batch, limits)?;
    Ok(batch)
}

pub fn canonical_v3_event_batch_sha256(
    schema: &SchemaDescriptor,
    batch: &AuraV3EventBatch,
    limits: V3EventLimits,
) -> Result<[u8; 32]> {
    let mut hasher =
        CanonicalV3EventHasher::new(schema, batch.event_count, batch.child_count(), limits)?;
    hasher.update_batch(schema, batch)?;
    hasher.finalize()
}

/// Incremental form of the canonical grouped-event hash.
///
/// Total event and child counts are committed before any values. Batches are
/// supplied in file order and local event/child indices are rebased into the
/// global logical stream, making the final hash independent of chunking.
#[derive(Clone)]
pub struct CanonicalV3EventHasher {
    hasher: Sha256,
    schema_id: u32,
    schema_fingerprint: [u8; 32],
    event_column_count: usize,
    repeated_column_count: usize,
    total_events: u32,
    total_children: u32,
    hashed_events: u32,
    hashed_children: u32,
    limits: V3EventLimits,
}

impl core::fmt::Debug for CanonicalV3EventHasher {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CanonicalV3EventHasher")
            .field("schema_id", &self.schema_id)
            .field("event_column_count", &self.event_column_count)
            .field("repeated_column_count", &self.repeated_column_count)
            .field("total_events", &self.total_events)
            .field("total_children", &self.total_children)
            .field("hashed_events", &self.hashed_events)
            .field("hashed_children", &self.hashed_children)
            .finish_non_exhaustive()
    }
}

impl CanonicalV3EventHasher {
    pub fn new(
        schema: &SchemaDescriptor,
        total_events: u32,
        total_children: u32,
        limits: V3EventLimits,
    ) -> Result<Self> {
        validate_v3_grouped_exact_subset(schema)?;
        let schema_fingerprint = canonical_v3_schema_fingerprint(schema)?;
        let (event_fields, repeated_fields) = scoped_fields(schema);
        let event_column_count = event_fields.len();
        let repeated_column_count = repeated_fields.len();
        let mut hasher = Sha256::new();
        hasher.update(HASH_DOMAIN);
        hasher.update(schema.schema_id.to_le_bytes());
        hasher.update(schema_fingerprint);
        hasher.update(total_events.to_le_bytes());
        hasher.update(total_children.to_le_bytes());
        hasher.update(
            u32::try_from(event_column_count)
                .map_err(|_| AuraError::InvalidValue("v3 event column count"))?
                .to_le_bytes(),
        );
        hasher.update(
            u32::try_from(repeated_column_count)
                .map_err(|_| AuraError::InvalidValue("v3 repeated column count"))?
                .to_le_bytes(),
        );
        Ok(Self {
            hasher,
            schema_id: schema.schema_id,
            schema_fingerprint,
            event_column_count,
            repeated_column_count,
            total_events,
            total_children,
            hashed_events: 0,
            hashed_children: 0,
            limits,
        })
    }

    pub const fn hashed_events(&self) -> u32 {
        self.hashed_events
    }

    pub const fn hashed_children(&self) -> u32 {
        self.hashed_children
    }

    pub fn update_batch(
        &mut self,
        schema: &SchemaDescriptor,
        batch: &AuraV3EventBatch,
    ) -> Result<()> {
        validate_v3_event_batch(schema, batch, self.limits)?;
        if schema.schema_id != self.schema_id
            || canonical_v3_schema_fingerprint(schema)? != self.schema_fingerprint
            || batch.event_columns.len() != self.event_column_count
            || batch.repeated_columns.len() != self.repeated_column_count
        {
            return Err(AuraError::InvalidValue("v3 canonical event hash schema"));
        }
        let next_events = self
            .hashed_events
            .checked_add(batch.event_count)
            .filter(|count| *count <= self.total_events)
            .ok_or(AuraError::InvalidValue(
                "v3 canonical event hash event count",
            ))?;
        let batch_children = batch.child_count();
        let next_children = self
            .hashed_children
            .checked_add(batch_children)
            .filter(|count| *count <= self.total_children)
            .ok_or(AuraError::InvalidValue(
                "v3 canonical event hash child count",
            ))?;
        let events = usize::try_from(batch.event_count)
            .map_err(|_| AuraError::InvalidValue("v3 event count"))?;
        for event in 0..events {
            let event =
                u32::try_from(event).map_err(|_| AuraError::InvalidValue("v3 event count"))?;
            let global_event =
                self.hashed_events
                    .checked_add(event)
                    .ok_or(AuraError::InvalidValue(
                        "v3 canonical event hash event count",
                    ))?;
            let start = self
                .hashed_children
                .checked_add(batch.child_offsets[event as usize])
                .ok_or(AuraError::InvalidValue(
                    "v3 canonical event hash child count",
                ))?;
            let end = self
                .hashed_children
                .checked_add(batch.child_offsets[event as usize + 1])
                .ok_or(AuraError::InvalidValue(
                    "v3 canonical event hash child count",
                ))?;
            self.hasher.update(b"E");
            self.hasher.update(global_event.to_le_bytes());
            self.hasher.update(start.to_le_bytes());
            self.hasher.update(end.to_le_bytes());
            hash_columns_at(&mut self.hasher, &batch.event_columns, event as usize)?;
            for child in start..end {
                self.hasher.update(b"C");
                self.hasher.update(child.to_le_bytes());
                let local_child =
                    child
                        .checked_sub(self.hashed_children)
                        .ok_or(AuraError::InvalidValue(
                            "v3 canonical event hash child count",
                        ))?;
                hash_columns_at(
                    &mut self.hasher,
                    &batch.repeated_columns,
                    usize::try_from(local_child)
                        .map_err(|_| AuraError::InvalidValue("v3 child count"))?,
                )?;
            }
        }
        self.hashed_events = next_events;
        self.hashed_children = next_children;
        Ok(())
    }

    pub fn finalize(self) -> Result<[u8; 32]> {
        if self.hashed_events != self.total_events {
            return Err(AuraError::InvalidValue(
                "v3 canonical event hash event count",
            ));
        }
        if self.hashed_children != self.total_children {
            return Err(AuraError::InvalidValue(
                "v3 canonical event hash child count",
            ));
        }
        Ok(self.hasher.finalize().into())
    }
}

fn hash_columns_at(hasher: &mut Sha256, columns: &[AuraV3Column], row: usize) -> Result<()> {
    for column in columns {
        hasher.update(column.slot.to_le_bytes());
        hasher.update([column.values.field_type() as u8]);
        let present = is_present(column.validity.as_deref(), row);
        hasher.update([u8::from(present)]);
        if present {
            hash_present_value(hasher, &column.values, row)?;
        }
    }
    Ok(())
}

fn validate_scoped_columns(
    fields: &[&FieldDescriptor],
    columns: &[AuraV3Column],
    rows: usize,
    value_limit: usize,
) -> Result<()> {
    for (field, column) in fields.iter().zip(columns) {
        if column.slot != field.index {
            return Err(AuraError::InvalidValue("v3 event column slot"));
        }
        validate_v3_column_exact(field, column, rows, value_limit)?;
    }
    Ok(())
}

fn validate_runtime_side(schema: &SchemaDescriptor, columns: &[AuraV3Column]) -> Result<()> {
    let group = schema
        .groups
        .first()
        .ok_or(AuraError::InvalidValue("v3 grouped group count"))?;
    let slot = group
        .dual_domain
        .ok_or(AuraError::InvalidValue("v3 grouped dual domain"))?
        .discriminator_slot;
    let column = columns
        .iter()
        .find(|column| column.slot == slot)
        .ok_or(AuraError::InvalidValue("v3 grouped discriminator slot"))?;
    let AuraV3ColumnValues::U8(values) = &column.values else {
        return Err(AuraError::InvalidValue("v3 grouped discriminator"));
    };
    if values.iter().any(|side| *side > 1) {
        return Err(AuraError::InvalidValue("v3 grouped side value"));
    }
    Ok(())
}

fn scoped_fields(schema: &SchemaDescriptor) -> (Vec<&FieldDescriptor>, Vec<&FieldDescriptor>) {
    let mut event = Vec::new();
    let mut repeated = Vec::new();
    for field in &schema.fields {
        match field.scope {
            FieldScope::Event => event.push(field),
            FieldScope::Repeated => repeated.push(field),
        }
    }
    (event, repeated)
}

fn checked_total_values(
    events: usize,
    event_columns: usize,
    children: usize,
    repeated_columns: usize,
) -> Result<usize> {
    events
        .checked_mul(event_columns)
        .and_then(|value| {
            children
                .checked_mul(repeated_columns)
                .and_then(|children| value.checked_add(children))
        })
        .ok_or(AuraError::InvalidValue("v3 event value count"))
}

fn encoded_event_len(batch: &AuraV3EventBatch, events: usize) -> Result<usize> {
    let mut total = HEADER_BYTES
        .checked_add(
            batch
                .child_offsets
                .len()
                .checked_mul(4)
                .ok_or(AuraError::InvalidValue("v3 event block length"))?,
        )
        .ok_or(AuraError::InvalidValue("v3 event block length"))?;
    for column in &batch.event_columns {
        total = total
            .checked_add(encoded_v3_column_len(column, events)?)
            .ok_or(AuraError::InvalidValue("v3 event block length"))?;
    }
    let children = usize::try_from(batch.child_count())
        .map_err(|_| AuraError::InvalidValue("v3 child count"))?;
    for column in &batch.repeated_columns {
        total = total
            .checked_add(encoded_v3_column_len(column, children)?)
            .ok_or(AuraError::InvalidValue("v3 event block length"))?;
    }
    Ok(total)
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32_len(out: &mut Vec<u8>, value: usize, name: &'static str) -> Result<()> {
    put_u32(
        out,
        u32::try_from(value).map_err(|_| AuraError::InvalidValue(name))?,
    );
    Ok(())
}

fn put_u64_len(out: &mut Vec<u8>, value: usize, name: &'static str) -> Result<()> {
    out.extend_from_slice(
        &u64::try_from(value)
            .map_err(|_| AuraError::InvalidValue(name))?
            .to_le_bytes(),
    );
    Ok(())
}
