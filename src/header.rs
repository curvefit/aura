use std::collections::BTreeSet;

use crate::bytes::{put_i64_le, put_u16_le, put_u32_le, put_u8, ByteReader};
pub use crate::format::MAX_V3_HEADER_BYTES;
use crate::format::{AuraContainerVersion, AURA_MAGIC, DEFAULT_CONTAINER_VERSION};
use crate::schema::{
    decode_group_descriptor_table, encode_group_descriptor_table, validate_v3_header_schema,
    GroupDescriptor,
};
use crate::{AuraError, Profile, Result};

pub const HEADER_PREFIX_SIZE: usize = 25;
pub const LEGACY_HEADER_PREFIX_SIZE: usize = 22;
/// Exact prefix size of the authoritative Aura v3 front-header layout.
pub const V3_HEADER_PREFIX_SIZE: usize = 39;
const HEADER_LEN_OFFSET: usize = 7;
const HEADER_LEN_END: usize = 9;
const DERIVED_EXPRESSION_ID_MIN: u8 = 1;
const DERIVED_EXPRESSION_ID_MAX: u8 = 99;
pub const DERIVED_EXPRESSION_INTERNAL_FLAG: u8 = 0b0000_0001;
const DERIVED_EXPRESSION_KNOWN_FLAGS: u8 = DERIVED_EXPRESSION_INTERNAL_FLAG;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DerivedExpressionOp {
    Add = 0,
    Sub = 1,
    Mul = 2,
    Div = 3,
    Min = 4,
    Max = 5,
    AddResidual = 6,
    SubtractResidual = 7,
    MaxPlusResidual = 8,
    MinMinusResidual = 9,
    FirstOffsetThenDelta = 10,
    /// Multiply the input slots using a checked i128 intermediate, then divide
    /// by the single literal scale before converting the prediction to i64.
    MulDiv = 11,
    /// For a full-replacement repeated snapshot, subtract the value associated
    /// with the same composite key in the previous snapshot. Missing keys use
    /// zero; the input slots are the key components.
    PreviousSnapshotSameKeyResidual = 12,
    /// For a bootstrap-plus-mutation repeated stream, subtract the value
    /// associated with the same composite key in the current live state. The
    /// first input is an event-scoped reset flag and the remaining inputs are
    /// repeated key components. A reconstructed zero removes the key.
    PreviousMutationSameKeyResidual = 13,
    /// Subtract the most recently decoded output associated with a composite
    /// key. The first input is an event-scoped reset flag and the remaining
    /// inputs are repeated key components. Unlike live mutation state, zero is
    /// an ordinary remembered value and never deletes the key.
    PreviousOutputByKeyResidual = 14,
}

impl DerivedExpressionOp {
    pub fn from_code(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Add),
            1 => Ok(Self::Sub),
            2 => Ok(Self::Mul),
            3 => Ok(Self::Div),
            4 => Ok(Self::Min),
            5 => Ok(Self::Max),
            6 => Ok(Self::AddResidual),
            7 => Ok(Self::SubtractResidual),
            8 => Ok(Self::MaxPlusResidual),
            9 => Ok(Self::MinMinusResidual),
            10 => Ok(Self::FirstOffsetThenDelta),
            11 => Ok(Self::MulDiv),
            12 => Ok(Self::PreviousSnapshotSameKeyResidual),
            13 => Ok(Self::PreviousMutationSameKeyResidual),
            14 => Ok(Self::PreviousOutputByKeyResidual),
            _ => Err(AuraError::InvalidValue("derived expression op")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivedExpressionSource {
    External,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedExpression {
    pub expression_id: u8,
    pub output_slot: u16,
    pub op: DerivedExpressionOp,
    pub input_slots: Vec<u16>,
    pub literals: Vec<i64>,
    pub flags: u8,
}

impl DerivedExpression {
    pub fn new(
        expression_id: u8,
        output_slot: u16,
        op: DerivedExpressionOp,
        input_slots: Vec<u16>,
    ) -> Result<Self> {
        Self::with_literals(expression_id, output_slot, op, input_slots, Vec::new(), 0)
    }

    pub fn with_literals(
        expression_id: u8,
        output_slot: u16,
        op: DerivedExpressionOp,
        input_slots: Vec<u16>,
        literals: Vec<i64>,
        flags: u8,
    ) -> Result<Self> {
        let expression = Self {
            expression_id,
            output_slot,
            op,
            input_slots,
            literals,
            flags,
        };
        expression.validate()?;
        Ok(expression)
    }

    pub fn with_source(mut self, source: DerivedExpressionSource) -> Result<Self> {
        match source {
            DerivedExpressionSource::External => {
                self.flags &= !DERIVED_EXPRESSION_INTERNAL_FLAG;
            }
            DerivedExpressionSource::Internal => {
                self.flags |= DERIVED_EXPRESSION_INTERNAL_FLAG;
            }
        }
        self.validate()?;
        Ok(self)
    }

    pub const fn source(&self) -> DerivedExpressionSource {
        if self.flags & DERIVED_EXPRESSION_INTERNAL_FLAG != 0 {
            DerivedExpressionSource::Internal
        } else {
            DerivedExpressionSource::External
        }
    }

    pub const fn is_internal(&self) -> bool {
        matches!(self.source(), DerivedExpressionSource::Internal)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if !(DERIVED_EXPRESSION_ID_MIN..=DERIVED_EXPRESSION_ID_MAX).contains(&self.expression_id) {
            return Err(AuraError::InvalidValue("derived expression id"));
        }
        if self.flags & !DERIVED_EXPRESSION_KNOWN_FLAGS != 0 {
            return Err(AuraError::InvalidValue("derived expression flags"));
        }
        if self.input_slots.len() > u8::MAX as usize {
            return Err(AuraError::InvalidValue("derived expression inputs"));
        }
        if self.literals.len() > u8::MAX as usize {
            return Err(AuraError::InvalidValue("derived expression literals"));
        }
        Ok(())
    }
}

/// Front Aura file header. The body starts at `header_len`.
///
/// In v3, this front header is authoritative for field relationships, derived
/// expressions, and group membership. It does not define authoritative field
/// names, logical types, roles, scales, or nullability; those belong to the
/// full v3 schema block (encoding tag 4), or canonical external schema JSON
/// before the file is sealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraHeader {
    pub container_version: AuraContainerVersion,
    pub profile: Profile,
    pub stream_id: u16,
    pub dictionary_id: u16,
    pub base_time_ns: i64,
    pub schema_mapping: Vec<u8>,
    pub derived_expressions: Vec<DerivedExpression>,
    pub groups: Vec<GroupDescriptor>,
    pub comment: String,
}

impl AuraHeader {
    pub fn new(profile: Profile) -> Self {
        Self {
            container_version: DEFAULT_CONTAINER_VERSION,
            profile,
            stream_id: 0,
            dictionary_id: 0,
            base_time_ns: 0,
            schema_mapping: Vec::new(),
            derived_expressions: Vec::new(),
            groups: Vec::new(),
            comment: String::new(),
        }
    }

    pub const fn with_container_version(mut self, container_version: AuraContainerVersion) -> Self {
        self.container_version = container_version;
        self
    }

    pub const fn container_version(&self) -> AuraContainerVersion {
        self.container_version
    }

    pub fn with_stream(mut self, stream_id: u16, dictionary_id: u16, base_time_ns: i64) -> Self {
        self.stream_id = stream_id;
        self.dictionary_id = dictionary_id;
        self.base_time_ns = base_time_ns;
        self
    }

    pub fn with_schema_mapping(mut self, schema_mapping: Vec<u8>) -> Result<Self> {
        self.validate_builder_lengths(
            schema_mapping.len(),
            derived_expression_table_len(&self.derived_expressions)?,
            self.comment.len(),
        )?;
        self.schema_mapping = schema_mapping;
        Ok(self)
    }

    pub fn with_derived_expressions(
        mut self,
        derived_expressions: Vec<DerivedExpression>,
    ) -> Result<Self> {
        validate_derived_expressions(&derived_expressions)?;
        self.validate_builder_lengths(
            self.schema_mapping.len(),
            derived_expression_table_len(&derived_expressions)?,
            self.comment.len(),
        )?;
        self.derived_expressions = derived_expressions;
        Ok(self)
    }

    pub fn with_groups(mut self, groups: Vec<GroupDescriptor>) -> Result<Self> {
        // The standalone encoder performs descriptor-only validation without
        // making builder call order significant.
        encode_group_descriptor_table(&groups)?;
        self.groups = groups;
        self.groups.sort_by_key(|group| group.group_id);
        self.validate_builder_lengths(
            self.schema_mapping.len(),
            derived_expression_table_len(&self.derived_expressions)?,
            self.comment.len(),
        )?;
        Ok(self)
    }

    pub fn with_comment(mut self, comment: impl Into<String>) -> Result<Self> {
        let comment = comment.into();
        self.validate_builder_lengths(
            self.schema_mapping.len(),
            derived_expression_table_len(&self.derived_expressions)?,
            comment.len(),
        )?;
        self.comment = comment;
        Ok(self)
    }

    fn validate_builder_lengths(
        &self,
        schema_len: usize,
        expression_len: usize,
        comment_len: usize,
    ) -> Result<()> {
        match self.container_version {
            AuraContainerVersion::V3 => {
                let group_len = encode_group_descriptor_table(&self.groups)?.len();
                for (len, name) in [
                    (schema_len, "schema mapping length"),
                    (expression_len, "derived expression length"),
                    (group_len, "group descriptor length"),
                    (comment_len, "header comment length"),
                ] {
                    u32::try_from(len).map_err(|_| AuraError::InvalidValue(name))?;
                }
                V3_HEADER_PREFIX_SIZE
                    .checked_add(schema_len)
                    .and_then(|len| len.checked_add(expression_len))
                    .and_then(|len| len.checked_add(group_len))
                    .and_then(|len| len.checked_add(comment_len))
                    .and_then(|len| u32::try_from(len).ok())
                    .filter(|len| (*len as usize) <= MAX_V3_HEADER_BYTES)
                    .ok_or(AuraError::InvalidValue("header length"))?;
                Ok(())
            }
            AuraContainerVersion::LegacyV1 | AuraContainerVersion::V2 => {
                validate_header_lengths(schema_len, expression_len, comment_len)
            }
        }
    }

    pub fn header_len(&self) -> usize {
        match self.container_version {
            AuraContainerVersion::V3 => V3_HEADER_PREFIX_SIZE
                .checked_add(self.schema_mapping.len())
                .and_then(|len| {
                    len.checked_add(
                        derived_expression_table_len(&self.derived_expressions)
                            .unwrap_or(usize::MAX),
                    )
                })
                .and_then(|len| {
                    len.checked_add(
                        encode_group_descriptor_table(&self.groups)
                            .map_or(usize::MAX, |table| table.len()),
                    )
                })
                .and_then(|len| len.checked_add(self.comment.len()))
                .unwrap_or(usize::MAX),
            AuraContainerVersion::LegacyV1 | AuraContainerVersion::V2 => {
                HEADER_PREFIX_SIZE
                    + self.schema_mapping.len()
                    + derived_expression_table_len(&self.derived_expressions).unwrap_or(usize::MAX)
                    + self.comment.len()
            }
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        match self.container_version {
            AuraContainerVersion::V2 => self.encode_v2(),
            AuraContainerVersion::V3 => self.encode_v3(),
            AuraContainerVersion::LegacyV1 => Err(AuraError::UnsupportedVersion(
                self.container_version.wire_value(),
            )),
        }
    }

    fn encode_v2(&self) -> Result<Vec<u8>> {
        if !self.groups.is_empty() {
            return Err(AuraError::InvalidValue("v2 header groups"));
        }
        validate_derived_expressions(&self.derived_expressions)?;
        let derived_expression_table = encode_derived_expression_table(&self.derived_expressions)?;
        validate_header_lengths(
            self.schema_mapping.len(),
            derived_expression_table.len(),
            self.comment.len(),
        )?;
        let header_len = u16::try_from(self.header_len())
            .map_err(|_| AuraError::InvalidValue("header length"))?;
        let schema_len = u8::try_from(self.schema_mapping.len())
            .map_err(|_| AuraError::InvalidValue("schema mapping length"))?;
        let comment_len = u8::try_from(self.comment.len())
            .map_err(|_| AuraError::InvalidValue("header comment length"))?;
        let derived_expression_len = u16::try_from(derived_expression_table.len())
            .map_err(|_| AuraError::InvalidValue("derived expression length"))?;
        let mut out = Vec::with_capacity(usize::from(header_len));
        out.extend_from_slice(AURA_MAGIC);
        put_u16_le(&mut out, self.container_version.wire_value());
        put_u8(&mut out, self.profile as u8);
        put_u16_le(&mut out, header_len);
        put_i64_le(&mut out, self.base_time_ns);
        put_u16_le(&mut out, self.stream_id);
        put_u16_le(&mut out, self.dictionary_id);
        put_u8(&mut out, schema_len);
        put_u8(&mut out, comment_len);
        put_u16_le(&mut out, derived_expression_len);
        out.extend_from_slice(&self.schema_mapping);
        out.extend_from_slice(&derived_expression_table);
        out.extend_from_slice(self.comment.as_bytes());
        debug_assert_eq!(usize::from(header_len), out.len());
        Ok(out)
    }

    fn encode_v3(&self) -> Result<Vec<u8>> {
        validate_derived_expressions(&self.derived_expressions)?;
        validate_v3_header_schema(
            &self.schema_mapping,
            &self.groups,
            &self.derived_expressions,
        )?;
        let expression_table = encode_derived_expression_table(&self.derived_expressions)?;
        let group_table = encode_group_descriptor_table(&self.groups)?;
        let header_len = V3_HEADER_PREFIX_SIZE
            .checked_add(self.schema_mapping.len())
            .and_then(|len| len.checked_add(expression_table.len()))
            .and_then(|len| len.checked_add(group_table.len()))
            .and_then(|len| len.checked_add(self.comment.len()))
            .ok_or(AuraError::InvalidValue("header length"))?;
        if header_len > MAX_V3_HEADER_BYTES {
            return Err(AuraError::InvalidValue("header length"));
        }
        let header_len =
            u32::try_from(header_len).map_err(|_| AuraError::InvalidValue("header length"))?;
        let schema_len = u32::try_from(self.schema_mapping.len())
            .map_err(|_| AuraError::InvalidValue("schema mapping length"))?;
        let expression_len = u32::try_from(expression_table.len())
            .map_err(|_| AuraError::InvalidValue("derived expression length"))?;
        let group_len = u32::try_from(group_table.len())
            .map_err(|_| AuraError::InvalidValue("group descriptor length"))?;
        let comment_len = u32::try_from(self.comment.len())
            .map_err(|_| AuraError::InvalidValue("header comment length"))?;
        let mut out = Vec::new();
        out.try_reserve_exact(header_len as usize)
            .map_err(|_| AuraError::InvalidValue("header allocation"))?;
        out.extend_from_slice(AURA_MAGIC);
        put_u16_le(&mut out, self.container_version.wire_value());
        put_u8(&mut out, self.profile as u8);
        put_u32_le(&mut out, header_len);
        put_i64_le(&mut out, self.base_time_ns);
        put_u16_le(&mut out, self.stream_id);
        put_u16_le(&mut out, self.dictionary_id);
        put_u32_le(&mut out, schema_len);
        put_u32_le(&mut out, expression_len);
        put_u32_le(&mut out, group_len);
        put_u32_le(&mut out, comment_len);
        out.extend_from_slice(&self.schema_mapping);
        out.extend_from_slice(&expression_table);
        out.extend_from_slice(&group_table);
        out.extend_from_slice(self.comment.as_bytes());
        debug_assert_eq!(header_len as usize, out.len());
        Ok(out)
    }

    pub fn encoded_len(bytes: &[u8]) -> Result<usize> {
        if bytes.len() < HEADER_LEN_OFFSET + 1 {
            return Err(AuraError::UnexpectedEof);
        }
        if bytes.len() < 6 {
            return Err(AuraError::UnexpectedEof);
        }
        if &bytes[..4] != AURA_MAGIC {
            return Err(AuraError::InvalidMagic { expected: "AURA" });
        }
        let version = AuraContainerVersion::from_wire(u16::from_le_bytes([bytes[4], bytes[5]]))?;
        match version {
            AuraContainerVersion::LegacyV1 => Ok(usize::from(bytes[HEADER_LEN_OFFSET])),
            AuraContainerVersion::V2 => {
                if bytes.len() < HEADER_LEN_END {
                    return Err(AuraError::UnexpectedEof);
                }
                Ok(usize::from(u16::from_le_bytes([
                    bytes[HEADER_LEN_OFFSET],
                    bytes[HEADER_LEN_OFFSET + 1],
                ])))
            }
            AuraContainerVersion::V3 => Self::encoded_len_v3(bytes),
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = ByteReader::new(bytes);
        let magic = reader.read_exact(4)?;
        if magic != AURA_MAGIC {
            return Err(AuraError::InvalidMagic { expected: "AURA" });
        }
        let container_version = AuraContainerVersion::from_wire(reader.read_u16_le()?)?;
        match container_version {
            AuraContainerVersion::LegacyV1 => {
                let profile = Profile::from_byte(reader.read_u8()?)?;
                let header_len = usize::from(reader.read_u8()?);
                Self::decode_supported_layout(
                    reader,
                    bytes,
                    container_version,
                    profile,
                    header_len,
                    LEGACY_HEADER_PREFIX_SIZE,
                    false,
                )
            }
            AuraContainerVersion::V2 => {
                let profile = Profile::from_byte(reader.read_u8()?)?;
                let header_len = usize::from(reader.read_u16_le()?);
                Self::decode_supported_layout(
                    reader,
                    bytes,
                    container_version,
                    profile,
                    header_len,
                    HEADER_PREFIX_SIZE,
                    true,
                )
            }
            AuraContainerVersion::V3 => Self::decode_v3(reader, bytes, container_version),
        }
    }

    fn encoded_len_v3(bytes: &[u8]) -> Result<usize> {
        if bytes.len() < 11 {
            return Err(AuraError::UnexpectedEof);
        }
        let header_len = u32::from_le_bytes(bytes[7..11].try_into().unwrap()) as usize;
        if header_len < V3_HEADER_PREFIX_SIZE {
            return Err(AuraError::InvalidValue("header length"));
        }
        if header_len > MAX_V3_HEADER_BYTES {
            return Err(AuraError::InvalidValue("header length"));
        }
        Ok(header_len)
    }

    fn decode_v3(
        mut reader: ByteReader<'_>,
        bytes: &[u8],
        container_version: AuraContainerVersion,
    ) -> Result<Self> {
        let profile = Profile::from_byte(reader.read_u8()?)?;
        let header_len = reader.read_u32_le()? as usize;
        if header_len != bytes.len()
            || !(V3_HEADER_PREFIX_SIZE..=MAX_V3_HEADER_BYTES).contains(&header_len)
        {
            return Err(AuraError::InvalidValue("header length"));
        }
        let base_time_ns = reader.read_i64_le()?;
        let stream_id = reader.read_u16_le()?;
        let dictionary_id = reader.read_u16_le()?;
        let schema_len = reader.read_u32_le()? as usize;
        let expression_len = reader.read_u32_le()? as usize;
        let group_len = reader.read_u32_le()? as usize;
        let comment_len = reader.read_u32_le()? as usize;
        let payload_len = schema_len
            .checked_add(expression_len)
            .and_then(|len| len.checked_add(group_len))
            .and_then(|len| len.checked_add(comment_len))
            .ok_or(AuraError::InvalidValue("header length"))?;
        if V3_HEADER_PREFIX_SIZE.checked_add(payload_len) != Some(header_len) {
            return Err(AuraError::InvalidValue("header length"));
        }
        // All four lengths are checked against the complete header before any
        // length-controlled owned allocation occurs.
        let schema_bytes = reader.read_exact(schema_len)?;
        let expression_bytes = reader.read_exact(expression_len)?;
        let group_bytes = reader.read_exact(group_len)?;
        let comment_bytes = reader.read_exact(comment_len)?;
        reader.finish()?;
        let derived_expressions = decode_derived_expression_table(expression_bytes)?;
        let groups = decode_group_descriptor_table(group_bytes)?;
        validate_v3_header_schema(schema_bytes, &groups, &derived_expressions)?;
        let comment = std::str::from_utf8(comment_bytes)
            .map_err(|_| AuraError::InvalidValue("header comment"))?
            .to_owned();
        Ok(Self {
            container_version,
            profile,
            stream_id,
            dictionary_id,
            base_time_ns,
            schema_mapping: schema_bytes.to_vec(),
            derived_expressions,
            groups,
            comment,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn decode_supported_layout(
        mut reader: ByteReader<'_>,
        bytes: &[u8],
        container_version: AuraContainerVersion,
        profile: Profile,
        header_len: usize,
        prefix_size: usize,
        has_derived_expressions: bool,
    ) -> Result<Self> {
        if header_len != bytes.len() || bytes.len() < prefix_size {
            return Err(AuraError::InvalidValue("header length"));
        }
        let base_time_ns = reader.read_i64_le()?;
        let stream_id = reader.read_u16_le()?;
        let dictionary_id = reader.read_u16_le()?;
        let schema_len = reader.read_u8()? as usize;
        let comment_len = reader.read_u8()? as usize;
        let derived_expression_len = if has_derived_expressions {
            reader.read_u16_le()? as usize
        } else {
            0
        };
        if prefix_size + schema_len + derived_expression_len + comment_len != header_len {
            return Err(AuraError::InvalidValue("header length"));
        }
        let schema_mapping = reader.read_exact(schema_len)?.to_vec();
        let derived_expressions =
            decode_derived_expression_table(reader.read_exact(derived_expression_len)?)?;
        let comment = std::str::from_utf8(reader.read_exact(comment_len)?)
            .map_err(|_| AuraError::InvalidValue("header comment"))?
            .to_string();
        reader.finish()?;

        Ok(Self {
            container_version,
            profile,
            stream_id,
            dictionary_id,
            base_time_ns,
            schema_mapping,
            derived_expressions,
            groups: Vec::new(),
            comment,
        })
    }
}

fn validate_header_lengths(
    schema_len: usize,
    derived_expression_len: usize,
    comment_len: usize,
) -> Result<()> {
    if schema_len > u8::MAX as usize {
        return Err(AuraError::InvalidValue("schema mapping length"));
    }
    if derived_expression_len > u16::MAX as usize {
        return Err(AuraError::InvalidValue("derived expression length"));
    }
    if comment_len > u8::MAX as usize {
        return Err(AuraError::InvalidValue("header comment length"));
    }
    if HEADER_PREFIX_SIZE + schema_len + derived_expression_len + comment_len > u16::MAX as usize {
        return Err(AuraError::InvalidValue("header length"));
    }
    Ok(())
}

pub(crate) fn validate_derived_expressions(
    derived_expressions: &[DerivedExpression],
) -> Result<()> {
    let mut ids = BTreeSet::new();
    for expression in derived_expressions {
        expression.validate()?;
        if !ids.insert(expression.expression_id) {
            return Err(AuraError::InvalidValue("derived expression id"));
        }
    }
    Ok(())
}

pub(crate) fn derived_expression_table_len(
    derived_expressions: &[DerivedExpression],
) -> Result<usize> {
    validate_derived_expressions(derived_expressions)?;
    let mut len = 1usize;
    for expression in derived_expressions {
        len = len
            .checked_add(7)
            .and_then(|len| len.checked_add(expression.input_slots.len().checked_mul(2)?))
            .and_then(|len| len.checked_add(expression.literals.len().checked_mul(8)?))
            .ok_or(AuraError::InvalidValue("derived expression length"))?;
    }
    if derived_expressions.is_empty() {
        Ok(0)
    } else {
        Ok(len)
    }
}

pub(crate) fn encode_derived_expression_table(
    derived_expressions: &[DerivedExpression],
) -> Result<Vec<u8>> {
    if derived_expressions.is_empty() {
        return Ok(Vec::new());
    }
    validate_derived_expressions(derived_expressions)?;
    if derived_expressions.len() > u8::MAX as usize {
        return Err(AuraError::InvalidValue("derived expression count"));
    }
    let mut out = Vec::with_capacity(derived_expression_table_len(derived_expressions)?);
    put_u8(&mut out, derived_expressions.len() as u8);
    for expression in derived_expressions {
        put_u8(&mut out, expression.expression_id);
        put_u8(&mut out, expression.op as u8);
        put_u16_le(&mut out, expression.output_slot);
        put_u8(&mut out, expression.flags);
        put_u8(&mut out, expression.input_slots.len() as u8);
        for input_slot in &expression.input_slots {
            put_u16_le(&mut out, *input_slot);
        }
        put_u8(&mut out, expression.literals.len() as u8);
        for literal in &expression.literals {
            put_i64_le(&mut out, *literal);
        }
    }
    Ok(out)
}

pub(crate) fn decode_derived_expression_table(bytes: &[u8]) -> Result<Vec<DerivedExpression>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut reader = ByteReader::new(bytes);
    let expression_count = reader.read_u8()? as usize;
    let mut expressions = Vec::with_capacity(expression_count);
    for _ in 0..expression_count {
        let expression_id = reader.read_u8()?;
        let op = DerivedExpressionOp::from_code(reader.read_u8()?)?;
        let output_slot = reader.read_u16_le()?;
        let flags = reader.read_u8()?;
        let input_count = reader.read_u8()? as usize;
        let mut input_slots = Vec::with_capacity(input_count);
        for _ in 0..input_count {
            input_slots.push(reader.read_u16_le()?);
        }
        let literal_count = reader.read_u8()? as usize;
        let mut literals = Vec::with_capacity(literal_count);
        for _ in 0..literal_count {
            literals.push(reader.read_i64_le()?);
        }
        expressions.push(DerivedExpression::with_literals(
            expression_id,
            output_slot,
            op,
            input_slots,
            literals,
            flags,
        )?);
    }
    reader.finish()?;
    validate_derived_expressions(&expressions)?;
    Ok(expressions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_u16_le(bytes: &[u8]) -> u16 {
        u16::from_le_bytes(bytes.try_into().unwrap())
    }

    #[test]
    fn header_round_trips_front_schema_mapping() {
        let open = AuraHeader::new(Profile::Ingest)
            .with_stream(7, 3, 1_725_000_000_000_000_000)
            .with_schema_mapping(vec![100, 0, 2, 2, 2, 0])
            .unwrap();
        let encoded = open.encode().unwrap();
        let decoded_open = AuraHeader::decode(&encoded).unwrap();
        assert_eq!(open, decoded_open);
        assert_eq!(AuraContainerVersion::V2, decoded_open.container_version);
        assert_eq!(7, decoded_open.stream_id);
        assert_eq!(3, decoded_open.dictionary_id);
        assert_eq!(1_725_000_000_000_000_000, decoded_open.base_time_ns);
        assert_eq!(
            &[100, 0, 2, 2, 2, 0],
            decoded_open.schema_mapping.as_slice()
        );
        assert_eq!("", decoded_open.comment);
    }

    #[test]
    fn header_len_includes_schema_mapping() {
        let encoded = AuraHeader::new(Profile::Aura0)
            .with_stream(22, 33, 44)
            .with_schema_mapping(vec![0, 0, 2])
            .unwrap()
            .encode()
            .unwrap();

        assert_eq!(HEADER_PREFIX_SIZE + 3, encoded.len());
        assert_eq!((HEADER_PREFIX_SIZE + 3) as u16, read_u16_le(&encoded[7..9]));
        assert_eq!(3, encoded[21]);
        assert_eq!(0, encoded[22]);
        assert_eq!(0, read_u16_le(&encoded[23..25]));
        assert_eq!(&[0, 0, 2], &encoded[25..28]);
    }

    #[test]
    fn header_len_includes_comment_after_schema_mapping() {
        let encoded = AuraHeader::new(Profile::Aura0)
            .with_stream(22, 33, 44)
            .with_schema_mapping(vec![0, 0, 2])
            .unwrap()
            .with_comment("ts,open,high")
            .unwrap()
            .encode()
            .unwrap();

        assert_eq!(HEADER_PREFIX_SIZE + 3 + 12, encoded.len());
        assert_eq!(
            (HEADER_PREFIX_SIZE + 3 + 12) as u16,
            read_u16_le(&encoded[7..9])
        );
        assert_eq!(3, encoded[21]);
        assert_eq!(12, encoded[22]);
        assert_eq!(0, read_u16_le(&encoded[23..25]));
        assert_eq!(&[0, 0, 2], &encoded[25..28]);
        assert_eq!(b"ts,open,high", &encoded[28..40]);

        let decoded = AuraHeader::decode(&encoded).unwrap();
        assert_eq!("ts,open,high", decoded.comment);
    }

    #[test]
    fn header_round_trips_derived_expression_table() {
        let expressions = vec![
            DerivedExpression::new(2, 2, DerivedExpressionOp::MaxPlusResidual, vec![1, 4]).unwrap(),
            DerivedExpression::new(3, 3, DerivedExpressionOp::MinMinusResidual, vec![1, 4])
                .unwrap(),
        ];
        let encoded = AuraHeader::new(Profile::Ingest)
            .with_schema_mapping(vec![100, 0, 102, 103, 2, 0])
            .unwrap()
            .with_derived_expressions(expressions.clone())
            .unwrap()
            .with_comment("ts,open,high,low,close,volume")
            .unwrap()
            .encode()
            .unwrap();

        assert_eq!(23, read_u16_le(&encoded[23..25]));
        assert_eq!(2, encoded[31]);
        let decoded = AuraHeader::decode(&encoded).unwrap();

        assert_eq!(expressions, decoded.derived_expressions);
        assert_eq!(&[100, 0, 102, 103, 2, 0], decoded.schema_mapping.as_slice());
        assert_eq!("ts,open,high,low,close,volume", decoded.comment);
    }

    #[test]
    fn derived_expression_table_length_is_u16() {
        let expression = DerivedExpression::with_literals(
            1,
            1,
            DerivedExpressionOp::AddResidual,
            vec![0, 2],
            (0..32).map(i64::from).collect(),
            0,
        )
        .unwrap();
        let encoded = AuraHeader::new(Profile::Ingest)
            .with_schema_mapping(vec![100, 101, 0])
            .unwrap()
            .with_derived_expressions(vec![expression.clone()])
            .unwrap()
            .encode()
            .unwrap();

        assert!(read_u16_le(&encoded[23..25]) > u16::from(u8::MAX));
        assert_eq!(
            read_u16_le(&encoded[7..9]) as usize,
            HEADER_PREFIX_SIZE + 3 + read_u16_le(&encoded[23..25]) as usize
        );
        assert_eq!(
            vec![expression],
            AuraHeader::decode(&encoded).unwrap().derived_expressions
        );
    }

    #[test]
    fn header_prefix_reads_version_before_profile_and_length() {
        let encoded = AuraHeader::new(Profile::Aura0).encode().unwrap();

        assert_eq!(b"AURA", &encoded[..4]);
        assert_eq!(crate::format::FORMAT_VERSION.to_le_bytes(), encoded[4..6]);
        assert_eq!(Profile::Aura0 as u8, encoded[6]);
        assert_eq!(HEADER_PREFIX_SIZE as u16, read_u16_le(&encoded[7..9]));
    }

    #[test]
    fn frozen_legacy_v1_header_still_decodes_as_header_only_layout() {
        let frozen = [
            b'A', b'U', b'R', b'A', 1, 0, 0, 27, 42, 0, 0, 0, 0, 0, 0, 0, 7, 0, 3, 0, 2, 3, 100, 0,
            b'o', b'l', b'd',
        ];

        assert_eq!(AuraHeader::encoded_len(&frozen), Ok(frozen.len()));
        let decoded = AuraHeader::decode(&frozen).unwrap();
        assert_eq!(decoded.container_version, AuraContainerVersion::LegacyV1);
        assert_eq!(decoded.profile, Profile::Ingest);
        assert_eq!(decoded.base_time_ns, 42);
        assert_eq!(decoded.stream_id, 7);
        assert_eq!(decoded.dictionary_id, 3);
        assert_eq!(decoded.schema_mapping, vec![100, 0]);
        assert!(decoded.derived_expressions.is_empty());
        assert_eq!(decoded.comment, "old");
        assert_eq!(
            decoded.encode(),
            Err(AuraError::UnsupportedVersion(1)),
            "legacy V1 support is intentionally header-decode-only"
        );
    }

    #[test]
    fn v3_header_layout_is_distinct_from_v2() {
        let encoded = AuraHeader::new(Profile::Aura0)
            .with_container_version(AuraContainerVersion::V3)
            .with_schema_mapping(vec![100, 0])
            .unwrap()
            .encode()
            .unwrap();

        assert_eq!(AuraHeader::encoded_len(&encoded), Ok(encoded.len()));
        assert_eq!(V3_HEADER_PREFIX_SIZE + 2 + 3, encoded.len());
        assert_eq!(
            AuraContainerVersion::V3,
            AuraHeader::decode(&encoded).unwrap().container_version
        );
    }
}
