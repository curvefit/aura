use std::cell::Cell;
use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::fixed_width::{
    fixed_field_loads, parse_checksum_value, read_i64_checked, read_i64_unchecked,
    validate_fixed_body, FixedFieldLoad, FixedLoadKind,
};
use crate::footer::AuraFooter;
use crate::format::SEAL_MAGIC;
use crate::header::AuraHeader;
use crate::options::{AuraFormat, ReaderOptions};
use crate::program::{CompiledAuraField, CompiledAuraPlan, CompiledFooter};
use crate::records::{self, DecodedI64File, DecodedTypedFile};
use crate::schema::{AuraSchema, SchemaDescriptor};
use crate::{
    AuraColumnBatch, AuraError, AuraRecordBatch, AuraTypedValue, AuraValue, Profile, Result,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum GroupByField {
    Name(String),
    Id(u16),
}

/// Consecutive-run grouping recipe for Aura replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupBy {
    fields: Vec<GroupByField>,
}

impl GroupBy {
    pub fn fields(fields: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            fields: fields
                .into_iter()
                .map(|field| GroupByField::Name(field.into()))
                .collect(),
        }
    }

    pub fn field_ids(fields: impl IntoIterator<Item = u16>) -> Self {
        Self {
            fields: fields.into_iter().map(GroupByField::Id).collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

/// Shared key values for a consecutive Aura replay group.
#[derive(Debug, Clone, PartialEq)]
pub struct AuraGroupKey {
    field_names: Vec<String>,
    values: Vec<AuraValue>,
}

impl AuraGroupKey {
    pub fn field_names(&self) -> &[String] {
        &self.field_names
    }

    pub fn values(&self) -> &[AuraValue] {
        &self.values
    }
}

/// Consecutive run of rows with identical selected key fields.
#[derive(Debug, Clone, PartialEq)]
pub struct AuraEventGroup {
    row_start: usize,
    row_count: usize,
    key: AuraGroupKey,
}

impl AuraEventGroup {
    pub const fn row_start(&self) -> usize {
        self.row_start
    }

    pub const fn row_count(&self) -> usize {
        self.row_count
    }

    pub const fn key(&self) -> &AuraGroupKey {
        &self.key
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AuraGroupStats {
    pub row_count: usize,
    pub group_count: usize,
    pub callback_count: usize,
    pub rows_per_group_avg: f64,
    pub rows_per_group_p95: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AuraReaderState {
    Aura1Fixed,
    Aura0Columns { columns: Option<Vec<Vec<i64>>> },
    Aura0ByteLane { aura1: Option<Vec<u8>> },
    LazyRows { rows: Option<Vec<Vec<i64>>> },
}

#[derive(Debug, Clone)]
enum AuraReaderSource {
    Memory(Vec<u8>),
    FileRange(FileBackedAuraInput),
}

impl AuraReaderSource {
    fn as_memory(&self) -> Option<&[u8]> {
        match self {
            Self::Memory(bytes) => Some(bytes),
            Self::FileRange(_) => None,
        }
    }

    fn file_range(&self) -> Option<&FileBackedAuraInput> {
        match self {
            Self::Memory(_) => None,
            Self::FileRange(input) => Some(input),
        }
    }
}

#[derive(Debug, Clone)]
struct FileBackedAuraInput {
    file: Arc<Mutex<File>>,
    file_len: usize,
    body_offset: usize,
    footer_offset: usize,
    body_bytes: usize,
}

impl FileBackedAuraInput {
    fn read_at(&self, offset: usize, len: usize) -> Result<Vec<u8>> {
        let end = offset
            .checked_add(len)
            .ok_or(AuraError::InvalidValue("file range"))?;
        if end > self.file_len {
            return Err(AuraError::UnexpectedEof);
        }
        let mut file = self
            .file
            .lock()
            .map_err(|_| AuraError::InvalidValue("reader input"))?;
        read_file_exact_at(&mut file, offset, len)
    }
}

#[derive(Debug, Clone)]
struct GroupKeyRecipe {
    field_name: String,
    aura_type: crate::AuraType,
    offset: usize,
    width: usize,
}

#[derive(Debug, Default)]
struct RawGroupState {
    key: Vec<u8>,
    row_start: usize,
    row_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuraReaderSourceKind {
    Memory,
    FileRange,
}

impl AuraReaderSourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::FileRange => "file_range",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuraReplayBackend {
    Memory,
    FileRange,
}

impl AuraReplayBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::FileRange => "file_range",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuraReaderStats {
    pub source_kind: AuraReaderSourceKind,
    pub replay_backend: AuraReplayBackend,
    pub file_len: usize,
    pub open_decoded_row_count: usize,
    pub full_file_materialized: bool,
    pub batches_read: usize,
    pub rows_decoded_in_last_batch: usize,
    pub max_rows_materialized_at_once: usize,
    pub bytes_read_at_open: usize,
    pub body_bytes_read_at_open: usize,
    pub footer_bytes_read_at_open: usize,
    pub bytes_read_during_replay: usize,
    pub bytes_read_in_last_batch: usize,
    pub full_file_bytes_copied: usize,
    pub row_width_from_plan: usize,
    pub body_offset_from_header: usize,
    pub footer_offset_from_trailer: usize,
    pub record_count_from_footer: usize,
    pub rows_scanned: usize,
    pub temp_row_buffers_allocated: usize,
    pub visitor_calls: usize,
    pub field_decode_count: usize,
    pub endian_load_count: usize,
    pub compiled_plan_used: bool,
    pub source_bytes_read_at_open: usize,
    pub source_bytes_read_total: usize,
    pub streaming_reader_used: bool,
}

#[derive(Debug, Clone)]
pub struct Aura1FixedBatchView<'a> {
    body: &'a [u8],
    record_width: usize,
    row_count: usize,
    field_offsets: Vec<CompiledAuraField>,
    field_loads: Option<Vec<FixedFieldLoad>>,
}

#[derive(Debug, Clone, Copy)]
pub struct Aura1RowView<'a> {
    row_bytes: &'a [u8],
    field_offsets: &'a [CompiledAuraField],
    field_loads: Option<&'a [FixedFieldLoad]>,
}

#[derive(Debug, Clone, Copy)]
pub struct Aura1SelectedRowView<'a> {
    row_bytes: &'a [u8],
    selected_loads: &'a [FixedFieldLoad],
}

impl<'a> Aura1FixedBatchView<'a> {
    fn new(body: &'a [u8], plan: &CompiledAuraPlan, row_count: usize) -> Result<Self> {
        let expected_len = row_count
            .checked_mul(plan.aura1_record_width)
            .ok_or(AuraError::InvalidValue("body length"))?;
        if body.len() != expected_len {
            return Err(AuraError::UnexpectedEof);
        }
        let field_offsets = aura1_field_slots(plan)?;
        let field_loads = fixed_field_loads(&field_offsets).ok();
        if field_loads.is_some() {
            validate_fixed_body(body, plan.aura1_record_width, row_count, &field_offsets)?;
        }
        Ok(Self {
            body,
            record_width: plan.aura1_record_width,
            row_count,
            field_offsets,
            field_loads,
        })
    }

    pub const fn row_count(&self) -> usize {
        self.row_count
    }

    pub const fn record_width(&self) -> usize {
        self.record_width
    }

    pub fn field_count(&self) -> usize {
        self.field_offsets.len()
    }

    pub fn value_i64(&self, row_index: usize, field_index: usize) -> Result<i64> {
        if row_index >= self.row_count {
            return Err(AuraError::InvalidValue("row index"));
        }
        let field = self
            .field_offsets
            .get(field_index)
            .ok_or(AuraError::InvalidValue("field index"))?;
        let row_offset = row_index
            .checked_mul(self.record_width)
            .ok_or(AuraError::InvalidValue("row index"))?;
        let start = row_offset
            .checked_add(field.offset)
            .ok_or(AuraError::InvalidValue("field offset"))?;
        let end = start
            .checked_add(field.width)
            .ok_or(AuraError::InvalidValue("field offset"))?;
        let bytes = self.body.get(start..end).ok_or(AuraError::UnexpectedEof)?;
        read_i64_fixed_width(bytes)
    }

    pub fn row_view(&self, row_index: usize) -> Result<Aura1RowView<'_>> {
        if row_index >= self.row_count {
            return Err(AuraError::InvalidValue("row index"));
        }
        let start = row_index
            .checked_mul(self.record_width)
            .ok_or(AuraError::InvalidValue("row index"))?;
        let end = start
            .checked_add(self.record_width)
            .ok_or(AuraError::InvalidValue("row index"))?;
        let row_bytes = self.body.get(start..end).ok_or(AuraError::UnexpectedEof)?;
        Ok(Aura1RowView {
            row_bytes,
            field_offsets: &self.field_offsets,
            field_loads: self.field_loads.as_deref(),
        })
    }

    pub fn row_i64_values(&self, row_index: usize, out: &mut Vec<i64>) -> Result<()> {
        out.clear();
        out.resize(self.field_offsets.len(), 0);
        let row = self.row_view(row_index)?;
        for index in 0..self.field_offsets.len() {
            out[index] = row.get_i64(index)?;
        }
        Ok(())
    }

    pub fn checksum_field(&self, field_index: usize) -> Result<u64> {
        let mut checksum = 0u64;
        for row_index in 0..self.row_count {
            checksum = mix_checksum(checksum, self.value_i64(row_index, field_index)?);
        }
        Ok(checksum)
    }

    pub fn checksum_all_fields(&self) -> Result<u64> {
        let mut checksum = 0u64;
        for row_index in 0..self.row_count {
            let row = self.row_view(row_index)?;
            for field_index in 0..self.field_offsets.len() {
                checksum = mix_checksum(checksum, row.get_i64(field_index)?);
            }
        }
        Ok(checksum)
    }

    pub fn checksum_selected_fields(&self, field_indices: &[usize]) -> Result<u64> {
        self.checksum_selected_fields_type_kernel(field_indices)
    }

    pub fn checksum_selected_fields_field_major(&self, field_indices: &[usize]) -> Result<u64> {
        let fields = select_field_slots(&self.field_offsets, field_indices)?;
        checksum_field_major_checked(self.body, self.record_width, self.row_count, &fields)
    }

    pub fn checksum_selected_fields_type_kernel(&self, field_indices: &[usize]) -> Result<u64> {
        let fields = select_field_slots(&self.field_offsets, field_indices)?;
        checksum_type_kernel(self.body, self.record_width, self.row_count, &fields)
    }

    pub fn checksum_all_fields_field_major(&self) -> Result<u64> {
        checksum_field_major_checked(
            self.body,
            self.record_width,
            self.row_count,
            &self.field_offsets,
        )
    }

    pub fn checksum_all_fields_checked_once(&self) -> Result<u64> {
        checksum_row_major_unchecked(
            self.body,
            self.record_width,
            self.row_count,
            &self.field_offsets,
        )
    }

    pub fn checksum_all_fields_type_kernel(&self) -> Result<u64> {
        checksum_type_kernel(
            self.body,
            self.record_width,
            self.row_count,
            &self.field_offsets,
        )
    }

    pub fn checksum_all_fields_parse_program(&self) -> Result<u64> {
        let ops = fixed_field_loads(&self.field_offsets)?;
        checksum_parse_program(self.body, self.record_width, self.row_count, &ops)
    }
}

impl<'a> Aura1RowView<'a> {
    pub fn field_count(&self) -> usize {
        self.field_offsets.len()
    }

    pub fn get_i64(&self, field_index: usize) -> Result<i64> {
        if let Some(loads) = self.field_loads {
            let load = loads
                .get(field_index)
                .ok_or(AuraError::InvalidValue("field index"))?;
            return Ok(unsafe { read_i64_unchecked(self.row_bytes.as_ptr(), *load) });
        }
        let field = self
            .field_offsets
            .get(field_index)
            .ok_or(AuraError::InvalidValue("field index"))?;
        let end = field
            .offset
            .checked_add(field.width)
            .ok_or(AuraError::InvalidValue("field offset"))?;
        let bytes = self
            .row_bytes
            .get(field.offset..end)
            .ok_or(AuraError::UnexpectedEof)?;
        read_i64_fixed_width(bytes)
    }

    pub fn get_u64(&self, field_index: usize) -> Result<u64> {
        Ok(self.get_i64(field_index)? as u64)
    }

    pub fn get_i32(&self, field_index: usize) -> Result<i32> {
        i32::try_from(self.get_i64(field_index)?).map_err(|_| AuraError::InvalidValue("i32 value"))
    }

    pub fn get_u32(&self, field_index: usize) -> Result<u32> {
        u32::try_from(self.get_i64(field_index)?).map_err(|_| AuraError::InvalidValue("u32 value"))
    }

    pub fn get_u8(&self, field_index: usize) -> Result<u8> {
        u8::try_from(self.get_i64(field_index)?).map_err(|_| AuraError::InvalidValue("u8 value"))
    }

    pub fn get_timestamp_nanos(&self, field_index: usize) -> Result<i64> {
        self.get_i64(field_index)
    }

    pub fn get_price_scaled(&self, field_index: usize) -> Result<i64> {
        self.get_i64(field_index)
    }

    pub fn checksum_all_fields(&self) -> Result<u64> {
        let mut checksum = 0u64;
        for field_index in 0..self.field_offsets.len() {
            checksum = mix_checksum(checksum, self.get_i64(field_index)?);
        }
        Ok(checksum)
    }
}

impl<'a> Aura1SelectedRowView<'a> {
    pub fn field_count(&self) -> usize {
        self.selected_loads.len()
    }

    pub fn get_i64(&self, selected_index: usize) -> Result<i64> {
        let load = self
            .selected_loads
            .get(selected_index)
            .ok_or(AuraError::InvalidValue("field index"))?;
        Ok(unsafe { read_i64_unchecked(self.row_bytes.as_ptr(), *load) })
    }
}

pub struct Aura1FieldI64Iter<'a> {
    body: &'a [u8],
    record_width: usize,
    field: CompiledAuraField,
    row_count: usize,
    row_index: usize,
}

impl<'a> Iterator for Aura1FieldI64Iter<'a> {
    type Item = Result<i64>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.row_index >= self.row_count {
            return None;
        }
        let start = match self
            .row_index
            .checked_mul(self.record_width)
            .and_then(|row| row.checked_add(self.field.offset))
        {
            Some(start) => start,
            None => return Some(Err(AuraError::InvalidValue("field offset"))),
        };
        let end = match start.checked_add(self.field.width) {
            Some(end) => end,
            None => return Some(Err(AuraError::InvalidValue("field offset"))),
        };
        self.row_index = self.row_index.saturating_add(1);
        Some(
            self.body
                .get(start..end)
                .ok_or(AuraError::UnexpectedEof)
                .and_then(read_i64_fixed_width),
        )
    }
}

fn select_field_slots(
    fields: &[CompiledAuraField],
    field_indices: &[usize],
) -> Result<Vec<CompiledAuraField>> {
    field_indices
        .iter()
        .map(|index| {
            fields
                .get(*index)
                .copied()
                .ok_or(AuraError::InvalidValue("field index"))
        })
        .collect()
}

fn checksum_field_major_checked(
    body: &[u8],
    record_width: usize,
    row_count: usize,
    fields: &[CompiledAuraField],
) -> Result<u64> {
    validate_fixed_body(body, record_width, row_count, fields)?;
    let mut checksum = 0u64;
    for field in fields {
        for row_index in 0..row_count {
            let row_start = row_index
                .checked_mul(record_width)
                .ok_or(AuraError::InvalidValue("row offset"))?;
            let row = body
                .get(row_start..row_start + record_width)
                .ok_or(AuraError::UnexpectedEof)?;
            checksum = parse_checksum_value(
                checksum,
                row_index,
                field.field_index,
                read_i64_checked(row, *field)?,
            );
        }
    }
    Ok(checksum)
}

fn checksum_row_major_unchecked(
    body: &[u8],
    record_width: usize,
    row_count: usize,
    fields: &[CompiledAuraField],
) -> Result<u64> {
    validate_fixed_body(body, record_width, row_count, fields)?;
    let loads = fixed_field_loads(fields)?;
    let mut checksum = 0u64;
    let base = body.as_ptr();
    for row_index in 0..row_count {
        let row_offset = row_index
            .checked_mul(record_width)
            .ok_or(AuraError::InvalidValue("row offset"))?;
        let row_ptr = unsafe { base.add(row_offset) };
        for load in &loads {
            let value = unsafe { read_i64_unchecked(row_ptr, *load) };
            checksum = parse_checksum_value(checksum, row_index, load.field_index, value);
        }
    }
    Ok(checksum)
}

fn checksum_type_kernel(
    body: &[u8],
    record_width: usize,
    row_count: usize,
    fields: &[CompiledAuraField],
) -> Result<u64> {
    validate_fixed_body(body, record_width, row_count, fields)?;
    let loads = fixed_field_loads(fields)?;
    let mut checksum = 0u64;
    checksum = checksum_load_kind_group(
        checksum,
        body,
        record_width,
        row_count,
        &loads,
        FixedLoadKind::I64,
    );
    checksum = checksum_load_kind_group(
        checksum,
        body,
        record_width,
        row_count,
        &loads,
        FixedLoadKind::I32,
    );
    checksum = checksum_load_kind_group(
        checksum,
        body,
        record_width,
        row_count,
        &loads,
        FixedLoadKind::I16,
    );
    checksum = checksum_load_kind_group(
        checksum,
        body,
        record_width,
        row_count,
        &loads,
        FixedLoadKind::I8,
    );
    Ok(checksum)
}

fn checksum_load_kind_group(
    mut checksum: u64,
    body: &[u8],
    record_width: usize,
    row_count: usize,
    loads: &[FixedFieldLoad],
    kind: FixedLoadKind,
) -> u64 {
    let base = body.as_ptr();
    for load in loads.iter().copied().filter(|load| load.kind == kind) {
        for row_index in 0..row_count {
            let row_ptr = unsafe { base.add(row_index * record_width) };
            let value = unsafe { read_i64_unchecked(row_ptr, load) };
            checksum = parse_checksum_value(checksum, row_index, load.field_index, value);
        }
    }
    checksum
}

fn checksum_parse_program(
    body: &[u8],
    record_width: usize,
    row_count: usize,
    ops: &[FixedFieldLoad],
) -> Result<u64> {
    let fields = ops
        .iter()
        .map(|op| CompiledAuraField {
            field_index: op.field_index,
            offset: op.offset,
            width: match op.kind {
                FixedLoadKind::I8 => 1,
                FixedLoadKind::I16 => 2,
                FixedLoadKind::I32 => 4,
                FixedLoadKind::I64 => 8,
            },
        })
        .collect::<Vec<_>>();
    validate_fixed_body(body, record_width, row_count, &fields)?;
    let mut checksum = 0u64;
    let base = body.as_ptr();
    for row_index in 0..row_count {
        let row_ptr = unsafe { base.add(row_index * record_width) };
        for op in ops {
            let value = unsafe { read_i64_unchecked(row_ptr, *op) };
            checksum = parse_checksum_value(checksum, row_index, op.field_index, value);
        }
    }
    Ok(checksum)
}

impl<'a> Aura1FixedBatchView<'a> {
    pub fn field_i64(&'a self, field_index: usize) -> Result<Aura1FieldI64Iter<'a>> {
        let field = *self
            .field_offsets
            .get(field_index)
            .ok_or(AuraError::InvalidValue("field index"))?;
        Ok(Aura1FieldI64Iter {
            body: self.body,
            record_width: self.record_width,
            field,
            row_count: self.row_count,
            row_index: 0,
        })
    }

    pub fn field_u64(
        &'a self,
        field_index: usize,
    ) -> Result<impl Iterator<Item = Result<u64>> + 'a> {
        Ok(self
            .field_i64(field_index)?
            .map(|value| value.map(|value| value as u64)))
    }

    pub fn field_u32(
        &'a self,
        field_index: usize,
    ) -> Result<impl Iterator<Item = Result<u32>> + 'a> {
        Ok(self.field_i64(field_index)?.map(|value| {
            value.and_then(|value| {
                u32::try_from(value).map_err(|_| AuraError::InvalidValue("u32 value"))
            })
        }))
    }

    pub fn field_u8(&'a self, field_index: usize) -> Result<impl Iterator<Item = Result<u8>> + 'a> {
        Ok(self.field_i64(field_index)?.map(|value| {
            value.and_then(|value| {
                u8::try_from(value).map_err(|_| AuraError::InvalidValue("u8 value"))
            })
        }))
    }
}

fn mix_checksum(checksum: u64, value: i64) -> u64 {
    checksum.wrapping_mul(0x9E37_79B1_85EB_CA87).rotate_left(7)
        ^ (value as u64).wrapping_add(0xC2B2_AE3D_27D4_EB4F)
}

fn aura1_field_slots(plan: &CompiledAuraPlan) -> Result<Vec<CompiledAuraField>> {
    let mut slots = vec![
        CompiledAuraField {
            field_index: 0,
            offset: 0,
            width: 0,
        };
        plan.field_count
    ];
    for field in plan.aura1_field_offsets() {
        let index = usize::from(field.field_index);
        let slot = slots
            .get_mut(index)
            .ok_or(AuraError::InvalidValue("field index"))?;
        *slot = field;
    }
    for (index, field) in slots.iter().enumerate() {
        if usize::from(field.field_index) != index {
            return Err(AuraError::InvalidValue("field index"));
        }
    }
    Ok(slots)
}

/// Public SDK reader for Aura files with dynamic schemas.
#[derive(Debug, Clone)]
pub struct AuraReader {
    source: AuraReaderSource,
    schema: AuraSchema,
    profile: Profile,
    compiled_footer: Option<CompiledFooter>,
    compiled_plan: Option<CompiledAuraPlan>,
    cursor: usize,
    record_count: usize,
    state: AuraReaderState,
    stats: Cell<AuraReaderStats>,
    use_byte_lane: crate::Aura0ByteLaneUse,
}

impl AuraReader {
    pub fn open<R: Read>(mut input: R) -> Result<Self> {
        Self::open_with_options(&mut input, ReaderOptions::default())
    }

    pub fn open_with_options<R: Read>(input: &mut R, options: ReaderOptions) -> Result<Self> {
        let mut bytes = Vec::new();
        input
            .read_to_end(&mut bytes)
            .map_err(|_| crate::AuraError::InvalidValue("reader input"))?;
        Self::open_memory(bytes, options)
    }

    pub fn open_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_path_with_options(path, ReaderOptions::default())
    }

    pub fn open_path_with_options(path: impl AsRef<Path>, options: ReaderOptions) -> Result<Self> {
        let file = File::open(path).map_err(|_| AuraError::InvalidValue("reader input"))?;
        Self::open_file_with_options(file, options)
    }

    pub fn open_file(file: File) -> Result<Self> {
        Self::open_file_with_options(file, ReaderOptions::default())
    }

    pub fn open_file_with_options(mut file: File, options: ReaderOptions) -> Result<Self> {
        let parsed = match parse_file_backed_metadata(&mut file) {
            Ok(parsed) => parsed,
            Err(error) => return Err(error),
        };
        if parsed.header.profile != Profile::Aura1 {
            let bytes = read_file_exact_at(&mut file, 0, parsed.file_len)?;
            return Self::open_memory(bytes, options);
        }
        let footer = parsed
            .compiled_footer
            .ok_or(AuraError::InvalidValue("compiled footer"))?;
        match options.use_byte_lane {
            crate::Aura0ByteLaneUse::Always => {
                return Err(AuraError::InvalidValue("aura0 byte lane"));
            }
            crate::Aura0ByteLaneUse::Auto | crate::Aura0ByteLaneUse::Never => {}
        }
        let record_count = records::validate_compiled_i64_metadata(&parsed.header, &footer)?;
        let compiled_plan = CompiledAuraPlan::from_footer(&footer)?;
        let body_bytes = parsed
            .footer_offset
            .checked_sub(parsed.header_len)
            .ok_or(AuraError::UnexpectedEof)?;
        if body_bytes != compiled_plan.aura1_body_size {
            return Err(AuraError::InvalidValue("aura1 body length"));
        }
        let state = AuraReaderState::Aura1Fixed;
        let source = AuraReaderSource::FileRange(FileBackedAuraInput {
            file: Arc::new(Mutex::new(file)),
            file_len: parsed.file_len,
            body_offset: parsed.header_len,
            footer_offset: parsed.footer_offset,
            body_bytes,
        });
        Ok(Self {
            stats: Cell::new(AuraReaderStats {
                source_kind: AuraReaderSourceKind::FileRange,
                replay_backend: AuraReplayBackend::FileRange,
                file_len: parsed.file_len,
                open_decoded_row_count: 0,
                full_file_materialized: false,
                batches_read: 0,
                rows_decoded_in_last_batch: 0,
                max_rows_materialized_at_once: 0,
                bytes_read_at_open: parsed.bytes_read_at_open,
                body_bytes_read_at_open: 0,
                footer_bytes_read_at_open: parsed.footer_len,
                bytes_read_during_replay: 0,
                bytes_read_in_last_batch: 0,
                full_file_bytes_copied: 0,
                row_width_from_plan: compiled_plan.aura1_record_width,
                body_offset_from_header: parsed.header_len,
                footer_offset_from_trailer: parsed.footer_offset,
                record_count_from_footer: record_count,
                rows_scanned: 0,
                temp_row_buffers_allocated: 0,
                visitor_calls: 0,
                field_decode_count: 0,
                endian_load_count: 0,
                compiled_plan_used: true,
                source_bytes_read_at_open: parsed.bytes_read_at_open,
                source_bytes_read_total: parsed.bytes_read_at_open,
                streaming_reader_used: true,
            }),
            source,
            schema: AuraSchema::from(footer.schema.clone()),
            profile: parsed.header.profile,
            compiled_footer: Some(footer),
            compiled_plan: Some(compiled_plan),
            cursor: 0,
            record_count,
            state,
            use_byte_lane: options.use_byte_lane,
        })
    }

    fn open_memory(bytes: Vec<u8>, options: ReaderOptions) -> Result<Self> {
        let metadata = records::decode_i64_file_metadata(&bytes)?;
        if metadata.header.profile == Profile::Aura0 {
            match options.use_byte_lane {
                crate::Aura0ByteLaneUse::Always => {
                    let Some(footer) = metadata.compiled_footer.as_ref() else {
                        return Err(AuraError::InvalidValue("aura0 byte lane"));
                    };
                    if footer.aura1_byte_lanes.is_empty() {
                        return Err(AuraError::InvalidValue("aura0 byte lane"));
                    }
                }
                crate::Aura0ByteLaneUse::Auto | crate::Aura0ByteLaneUse::Never => {}
            }
        }
        let compiled_plan = metadata
            .compiled_footer
            .as_ref()
            .map(CompiledAuraPlan::from_footer)
            .transpose()?;
        let state = match metadata.header.profile {
            Profile::Aura1 => AuraReaderState::Aura1Fixed,
            Profile::Aura0 => AuraReaderState::Aura0Columns { columns: None },
            Profile::Ingest => AuraReaderState::LazyRows { rows: None },
        };
        let body_bytes = metadata.footer_start.saturating_sub(metadata.header_len);
        let replay_backend = if metadata.header.profile == Profile::Aura1 {
            AuraReplayBackend::Memory
        } else {
            AuraReplayBackend::Memory
        };
        let row_width_from_plan = compiled_plan
            .as_ref()
            .map(|plan| plan.aura1_record_width)
            .unwrap_or(0);
        Ok(Self {
            stats: Cell::new(AuraReaderStats {
                source_kind: AuraReaderSourceKind::Memory,
                replay_backend,
                file_len: bytes.len(),
                open_decoded_row_count: 0,
                full_file_materialized: false,
                batches_read: 0,
                rows_decoded_in_last_batch: 0,
                max_rows_materialized_at_once: 0,
                bytes_read_at_open: bytes.len(),
                body_bytes_read_at_open: body_bytes,
                footer_bytes_read_at_open: metadata.footer_len_offset - metadata.footer_start,
                bytes_read_during_replay: 0,
                bytes_read_in_last_batch: 0,
                full_file_bytes_copied: bytes.len(),
                row_width_from_plan,
                body_offset_from_header: metadata.header_len,
                footer_offset_from_trailer: metadata.footer_start,
                record_count_from_footer: metadata.record_count,
                rows_scanned: 0,
                temp_row_buffers_allocated: 0,
                visitor_calls: 0,
                field_decode_count: 0,
                endian_load_count: 0,
                compiled_plan_used: compiled_plan.is_some(),
                source_bytes_read_at_open: bytes.len(),
                source_bytes_read_total: bytes.len(),
                streaming_reader_used: true,
            }),
            source: AuraReaderSource::Memory(bytes),
            schema: AuraSchema::from(metadata.schema.clone()),
            profile: metadata.header.profile,
            compiled_footer: metadata.compiled_footer,
            compiled_plan,
            cursor: 0,
            record_count: metadata.record_count,
            state,
            use_byte_lane: options.use_byte_lane,
        })
    }

    pub fn schema(&self) -> &AuraSchema {
        &self.schema
    }

    pub const fn profile(&self) -> Profile {
        self.profile
    }

    pub const fn format(&self) -> AuraFormat {
        match self.profile {
            Profile::Ingest => AuraFormat::Aura,
            Profile::Aura0 => AuraFormat::Aura0,
            Profile::Aura1 => AuraFormat::Aura1,
        }
    }

    pub fn compiled_footer(&self) -> Option<&CompiledFooter> {
        self.compiled_footer.as_ref()
    }

    pub fn compiled_plan(&self) -> Option<&CompiledAuraPlan> {
        self.compiled_plan.as_ref()
    }

    pub fn read_batches(&self) -> Result<Vec<AuraRecordBatch>> {
        let mut reader = self.clone();
        reader.reset_batches();
        let batch_size = reader.record_count.max(1);
        let mut batches = Vec::new();
        while let Some(batch) = reader.next_batch(batch_size)? {
            batches.push(batch);
        }
        Ok(batches)
    }

    pub fn next_batch(&mut self, batch_size: usize) -> Result<Option<AuraRecordBatch>> {
        if batch_size == 0 {
            return Err(AuraError::InvalidValue("batch size"));
        }
        if self.cursor >= self.record_count {
            return Ok(None);
        }
        self.update_stats(|stats| stats.bytes_read_in_last_batch = 0);
        let rows_to_read = batch_size.min(self.record_count - self.cursor);
        let end = self.cursor.saturating_add(rows_to_read);
        let rows = match self.profile {
            Profile::Aura1 => self.read_aura1_batch(rows_to_read)?,
            Profile::Aura0 => self.read_aura0_batch(rows_to_read)?,
            Profile::Ingest => self.read_lazy_rows_batch(rows_to_read)?,
        };
        self.cursor = end;
        self.update_stats(|stats| {
            stats.batches_read = stats.batches_read.saturating_add(1);
            stats.rows_decoded_in_last_batch = rows.len();
            stats.max_rows_materialized_at_once =
                stats.max_rows_materialized_at_once.max(rows.len());
        });
        Ok(Some(AuraRecordBatch::from_i64_decoded(
            self.schema.clone(),
            rows,
        )?))
    }

    pub fn next_column_batch(&mut self, batch_size: usize) -> Result<Option<AuraColumnBatch>> {
        if batch_size == 0 {
            return Err(AuraError::InvalidValue("batch size"));
        }
        if self.cursor >= self.record_count {
            return Ok(None);
        }
        self.update_stats(|stats| stats.bytes_read_in_last_batch = 0);
        let rows_to_read = batch_size.min(self.record_count - self.cursor);
        let end = self.cursor.saturating_add(rows_to_read);
        let batch = match self.profile {
            Profile::Aura1 => self.read_aura1_column_batch(rows_to_read)?,
            Profile::Aura0 => self.read_aura0_column_batch(rows_to_read)?,
            Profile::Ingest => {
                let rows = self.read_lazy_rows_batch(rows_to_read)?;
                AuraColumnBatch::from_i64_columns(self.schema.clone(), rows_to_columns(&rows)?)?
            }
        };
        self.cursor = end;
        self.update_stats(|stats| {
            stats.batches_read = stats.batches_read.saturating_add(1);
            stats.rows_decoded_in_last_batch = batch.row_count();
        });
        Ok(Some(batch))
    }

    pub fn reset_batches(&mut self) {
        self.cursor = 0;
        self.update_stats(|stats| {
            stats.rows_decoded_in_last_batch = 0;
            stats.bytes_read_in_last_batch = 0;
        });
    }

    pub fn batches(&self, batch_size: usize) -> Result<AuraBatchIter<'_>> {
        if batch_size == 0 {
            return Err(AuraError::InvalidValue("batch size"));
        }
        let mut reader = self.clone();
        reader.reset_batches();
        Ok(AuraBatchIter {
            reader,
            batch_size,
            _marker: std::marker::PhantomData,
        })
    }

    pub fn replay_i64<F>(&self, mut visitor: F) -> Result<usize>
    where
        F: FnMut(&[i64]) -> Result<()>,
    {
        match self.profile {
            Profile::Aura1 => self.replay_aura1(visitor),
            Profile::Aura0 | Profile::Ingest => {
                let mut reader = self.clone();
                reader.reset_batches();
                let mut count = 0usize;
                while let Some(batch) = reader.next_batch(8192)? {
                    let rows = batch.to_i64_rows()?;
                    for row in &rows {
                        visitor(row)?;
                        count = count.saturating_add(1);
                    }
                }
                Ok(count)
            }
        }
    }

    pub fn replay_fixed_batches<F>(&self, batch_size: usize, mut visitor: F) -> Result<usize>
    where
        F: FnMut(Aura1FixedBatchView<'_>) -> Result<()>,
    {
        if batch_size == 0 {
            return Err(AuraError::InvalidValue("batch size"));
        }
        if self.profile != Profile::Aura1 {
            return Err(AuraError::InvalidValue("aura1 replay profile"));
        }
        let plan = self
            .compiled_plan
            .as_ref()
            .ok_or(AuraError::InvalidValue("compiled plan"))?;
        let mut rows_seen = 0usize;
        match &self.source {
            AuraReaderSource::Memory(bytes) => {
                let stats = self.stats.get();
                let body = bytes
                    .get(stats.body_offset_from_header..stats.footer_offset_from_trailer)
                    .ok_or(AuraError::UnexpectedEof)?;
                while rows_seen < plan.record_count {
                    let rows_to_visit = batch_size.min(plan.record_count - rows_seen);
                    let byte_start = rows_seen
                        .checked_mul(plan.aura1_record_width)
                        .ok_or(AuraError::InvalidValue("row range"))?;
                    let byte_len = rows_to_visit
                        .checked_mul(plan.aura1_record_width)
                        .ok_or(AuraError::InvalidValue("row range"))?;
                    let byte_end = byte_start
                        .checked_add(byte_len)
                        .ok_or(AuraError::InvalidValue("row range"))?;
                    let range = body
                        .get(byte_start..byte_end)
                        .ok_or(AuraError::UnexpectedEof)?;
                    visitor(Aura1FixedBatchView::new(range, plan, rows_to_visit)?)?;
                    rows_seen = rows_seen.saturating_add(rows_to_visit);
                }
                self.update_stats(|stats| {
                    stats.rows_scanned = stats.rows_scanned.saturating_add(rows_seen);
                    stats.visitor_calls = stats
                        .visitor_calls
                        .saturating_add(rows_seen.div_ceil(batch_size));
                    stats.bytes_read_during_replay = stats
                        .bytes_read_during_replay
                        .saturating_add(plan.aura1_body_size);
                    stats.compiled_plan_used = true;
                });
                Ok(rows_seen)
            }
            AuraReaderSource::FileRange(_) => {
                while rows_seen < plan.record_count {
                    let rows_to_read = batch_size.min(plan.record_count - rows_seen);
                    let (body, rows_to_visit) =
                        self.read_aura1_body_range(rows_seen, rows_to_read)?;
                    visitor(Aura1FixedBatchView::new(&body, plan, rows_to_visit)?)?;
                    rows_seen = rows_seen.saturating_add(rows_to_visit);
                }
                self.update_stats(|stats| {
                    stats.rows_scanned = stats.rows_scanned.saturating_add(rows_seen);
                    stats.visitor_calls = stats
                        .visitor_calls
                        .saturating_add(rows_seen.div_ceil(batch_size));
                    stats.compiled_plan_used = true;
                });
                Ok(rows_seen)
            }
        }
    }

    pub fn replay_row_views<F>(&self, mut visitor: F) -> Result<usize>
    where
        F: FnMut(Aura1RowView<'_>) -> Result<()>,
    {
        if self.profile != Profile::Aura1 {
            return Err(AuraError::InvalidValue("aura1 replay profile"));
        }
        let plan = self
            .compiled_plan
            .as_ref()
            .ok_or(AuraError::InvalidValue("compiled plan"))?;
        let field_offsets = aura1_field_slots(plan)?;
        let field_loads = fixed_field_loads(&field_offsets).ok();
        let mut rows_seen = 0usize;
        match &self.source {
            AuraReaderSource::Memory(bytes) => {
                let stats = self.stats.get();
                let body = bytes
                    .get(stats.body_offset_from_header..stats.footer_offset_from_trailer)
                    .ok_or(AuraError::UnexpectedEof)?;
                if body.len() != plan.aura1_body_size {
                    return Err(AuraError::UnexpectedEof);
                }
                if field_loads.is_some() {
                    validate_fixed_body(
                        body,
                        plan.aura1_record_width,
                        plan.record_count,
                        &field_offsets,
                    )?;
                }
                while rows_seen < plan.record_count {
                    let row_bytes = if field_loads.is_some() {
                        unsafe { prevalidated_row_bytes(body, rows_seen, plan.aura1_record_width) }
                    } else {
                        let row_start = rows_seen
                            .checked_mul(plan.aura1_record_width)
                            .ok_or(AuraError::InvalidValue("row range"))?;
                        let row_end = row_start
                            .checked_add(plan.aura1_record_width)
                            .ok_or(AuraError::InvalidValue("row range"))?;
                        body.get(row_start..row_end)
                            .ok_or(AuraError::UnexpectedEof)?
                    };
                    visitor(Aura1RowView {
                        row_bytes,
                        field_offsets: &field_offsets,
                        field_loads: field_loads.as_deref(),
                    })?;
                    rows_seen = rows_seen.saturating_add(1);
                }
                self.update_stats(|stats| {
                    stats.rows_scanned = stats.rows_scanned.saturating_add(rows_seen);
                    stats.visitor_calls = stats.visitor_calls.saturating_add(rows_seen);
                    stats.bytes_read_during_replay = stats
                        .bytes_read_during_replay
                        .saturating_add(plan.aura1_body_size);
                    stats.compiled_plan_used = true;
                });
                Ok(rows_seen)
            }
            AuraReaderSource::FileRange(_) => {
                let rows_per_chunk = rows_per_file_chunk(plan.aura1_record_width);
                while rows_seen < plan.record_count {
                    let rows_to_read = rows_per_chunk.min(plan.record_count - rows_seen);
                    let (body, rows_to_visit) =
                        self.read_aura1_body_range(rows_seen, rows_to_read)?;
                    if field_loads.is_some() {
                        validate_fixed_body(
                            &body,
                            plan.aura1_record_width,
                            rows_to_visit,
                            &field_offsets,
                        )?;
                    }
                    for local_row in 0..rows_to_visit {
                        let row_bytes = if field_loads.is_some() {
                            unsafe {
                                prevalidated_row_bytes(&body, local_row, plan.aura1_record_width)
                            }
                        } else {
                            let row_start = local_row
                                .checked_mul(plan.aura1_record_width)
                                .ok_or(AuraError::InvalidValue("row range"))?;
                            let row_end = row_start
                                .checked_add(plan.aura1_record_width)
                                .ok_or(AuraError::InvalidValue("row range"))?;
                            body.get(row_start..row_end)
                                .ok_or(AuraError::UnexpectedEof)?
                        };
                        visitor(Aura1RowView {
                            row_bytes,
                            field_offsets: &field_offsets,
                            field_loads: field_loads.as_deref(),
                        })?;
                    }
                    rows_seen = rows_seen.saturating_add(rows_to_visit);
                }
                self.update_stats(|stats| {
                    stats.rows_scanned = stats.rows_scanned.saturating_add(rows_seen);
                    stats.visitor_calls = stats.visitor_calls.saturating_add(rows_seen);
                    stats.compiled_plan_used = true;
                });
                Ok(rows_seen)
            }
        }
    }

    pub fn replay_selected_row_views<F>(
        &self,
        field_indices: &[usize],
        mut visitor: F,
    ) -> Result<usize>
    where
        F: FnMut(Aura1SelectedRowView<'_>) -> Result<()>,
    {
        if self.profile != Profile::Aura1 {
            return Err(AuraError::InvalidValue("aura1 replay profile"));
        }
        let plan = self
            .compiled_plan
            .as_ref()
            .ok_or(AuraError::InvalidValue("compiled plan"))?;
        let field_offsets = aura1_field_slots(plan)?;
        let selected_fields = select_field_slots(&field_offsets, field_indices)?;
        let selected_loads = fixed_field_loads(&selected_fields)?;
        let mut rows_seen = 0usize;
        match &self.source {
            AuraReaderSource::Memory(bytes) => {
                let stats = self.stats.get();
                let body = bytes
                    .get(stats.body_offset_from_header..stats.footer_offset_from_trailer)
                    .ok_or(AuraError::UnexpectedEof)?;
                validate_fixed_body(
                    body,
                    plan.aura1_record_width,
                    plan.record_count,
                    &selected_fields,
                )?;
                while rows_seen < plan.record_count {
                    let row_bytes =
                        unsafe { prevalidated_row_bytes(body, rows_seen, plan.aura1_record_width) };
                    visitor(Aura1SelectedRowView {
                        row_bytes,
                        selected_loads: &selected_loads,
                    })?;
                    rows_seen = rows_seen.saturating_add(1);
                }
                self.update_stats(|stats| {
                    stats.rows_scanned = stats.rows_scanned.saturating_add(rows_seen);
                    stats.visitor_calls = stats.visitor_calls.saturating_add(rows_seen);
                    stats.bytes_read_during_replay = stats
                        .bytes_read_during_replay
                        .saturating_add(plan.aura1_body_size);
                    stats.compiled_plan_used = true;
                });
                Ok(rows_seen)
            }
            AuraReaderSource::FileRange(_) => {
                let rows_per_chunk = rows_per_file_chunk(plan.aura1_record_width);
                while rows_seen < plan.record_count {
                    let rows_to_read = rows_per_chunk.min(plan.record_count - rows_seen);
                    let (body, rows_to_visit) =
                        self.read_aura1_body_range(rows_seen, rows_to_read)?;
                    validate_fixed_body(
                        &body,
                        plan.aura1_record_width,
                        rows_to_visit,
                        &selected_fields,
                    )?;
                    for local_row in 0..rows_to_visit {
                        let row_bytes = unsafe {
                            prevalidated_row_bytes(&body, local_row, plan.aura1_record_width)
                        };
                        visitor(Aura1SelectedRowView {
                            row_bytes,
                            selected_loads: &selected_loads,
                        })?;
                    }
                    rows_seen = rows_seen.saturating_add(rows_to_visit);
                }
                self.update_stats(|stats| {
                    stats.rows_scanned = stats.rows_scanned.saturating_add(rows_seen);
                    stats.visitor_calls = stats.visitor_calls.saturating_add(rows_seen);
                    stats.compiled_plan_used = true;
                });
                Ok(rows_seen)
            }
        }
    }

    pub fn grouped_replay<F>(&self, group_by: &GroupBy, visitor: F) -> Result<AuraGroupStats>
    where
        F: FnMut(&AuraEventGroup) -> Result<()>,
    {
        let group_indexes = self.group_indexes(group_by)?;
        if self.profile == Profile::Aura1 {
            return self.grouped_replay_aura1_raw(&group_indexes, visitor);
        }
        self.grouped_replay_i64(&group_indexes, visitor)
    }

    fn grouped_replay_i64<F>(
        &self,
        group_indexes: &[usize],
        mut visitor: F,
    ) -> Result<AuraGroupStats>
    where
        F: FnMut(&AuraEventGroup) -> Result<()>,
    {
        let mut current_key = Vec::<i64>::new();
        let mut current_start = 0usize;
        let mut current_len = 0usize;
        let mut rows_seen = 0usize;
        let mut group_sizes = Vec::<usize>::new();

        self.replay_i64(|row| {
            let key = group_indexes
                .iter()
                .map(|index| row[*index])
                .collect::<Vec<_>>();
            if current_len == 0 {
                current_key = key;
                current_start = rows_seen;
                current_len = 1;
            } else if key == current_key {
                current_len = current_len.saturating_add(1);
            } else {
                let group = self.group_from_i64_key(
                    &group_indexes,
                    &current_key,
                    current_start,
                    current_len,
                )?;
                visitor(&group)?;
                group_sizes.push(current_len);
                current_key = key;
                current_start = rows_seen;
                current_len = 1;
            }
            rows_seen = rows_seen.saturating_add(1);
            Ok(())
        })?;

        if current_len > 0 {
            let group =
                self.group_from_i64_key(&group_indexes, &current_key, current_start, current_len)?;
            visitor(&group)?;
            group_sizes.push(current_len);
        }
        let group_count = group_sizes.len();
        let rows_per_group_avg = if group_count == 0 {
            0.0
        } else {
            rows_seen as f64 / group_count as f64
        };
        Ok(AuraGroupStats {
            row_count: rows_seen,
            group_count,
            callback_count: group_count,
            rows_per_group_avg,
            rows_per_group_p95: percentile_usize(&group_sizes, 0.95),
        })
    }

    fn grouped_replay_aura1_raw<F>(
        &self,
        group_indexes: &[usize],
        mut visitor: F,
    ) -> Result<AuraGroupStats>
    where
        F: FnMut(&AuraEventGroup) -> Result<()>,
    {
        let plan = self
            .compiled_plan
            .as_ref()
            .ok_or(AuraError::InvalidValue("compiled plan"))?;
        let recipes = self.group_key_recipes(group_indexes, plan)?;
        let key_width = recipes.iter().try_fold(0usize, |acc, recipe| {
            acc.checked_add(recipe.width)
                .ok_or(AuraError::InvalidValue("group key"))
        })?;
        let mut state = RawGroupState {
            key: Vec::with_capacity(key_width),
            row_start: 0,
            row_count: 0,
        };
        let mut group_sizes = Vec::<usize>::new();
        let mut rows_seen = 0usize;
        match &self.source {
            AuraReaderSource::Memory(bytes) => {
                let stats = self.stats.get();
                let body = bytes
                    .get(stats.body_offset_from_header..stats.footer_offset_from_trailer)
                    .ok_or(AuraError::UnexpectedEof)?;
                self.process_group_body(
                    body,
                    0,
                    plan.record_count,
                    &recipes,
                    &mut state,
                    &mut group_sizes,
                    &mut visitor,
                )?;
                rows_seen = plan.record_count;
                self.update_stats(|stats| {
                    stats.rows_scanned = stats.rows_scanned.saturating_add(rows_seen);
                    stats.bytes_read_during_replay = stats
                        .bytes_read_during_replay
                        .saturating_add(plan.aura1_body_size);
                    stats.compiled_plan_used = true;
                });
            }
            AuraReaderSource::FileRange(_) => {
                let rows_per_chunk = rows_per_file_chunk(plan.aura1_record_width);
                while rows_seen < plan.record_count {
                    let rows_to_read = rows_per_chunk.min(plan.record_count - rows_seen);
                    let (body, rows_to_visit) =
                        self.read_aura1_body_range(rows_seen, rows_to_read)?;
                    self.process_group_body(
                        &body,
                        rows_seen,
                        rows_to_visit,
                        &recipes,
                        &mut state,
                        &mut group_sizes,
                        &mut visitor,
                    )?;
                    rows_seen = rows_seen.saturating_add(rows_to_visit);
                }
                self.update_stats(|stats| {
                    stats.rows_scanned = stats.rows_scanned.saturating_add(rows_seen);
                    stats.compiled_plan_used = true;
                });
            }
        }
        if state.row_count > 0 {
            self.emit_raw_group(&recipes, &state, &mut visitor)?;
            group_sizes.push(state.row_count);
        }
        let group_count = group_sizes.len();
        self.update_stats(|stats| {
            stats.visitor_calls = stats.visitor_calls.saturating_add(group_count);
            stats.field_decode_count = stats
                .field_decode_count
                .saturating_add(group_count.saturating_mul(recipes.len()));
            stats.endian_load_count = stats
                .endian_load_count
                .saturating_add(group_count.saturating_mul(recipes.len()));
            stats.compiled_plan_used = true;
        });
        let rows_per_group_avg = if group_count == 0 {
            0.0
        } else {
            rows_seen as f64 / group_count as f64
        };
        Ok(AuraGroupStats {
            row_count: rows_seen,
            group_count,
            callback_count: group_count,
            rows_per_group_avg,
            rows_per_group_p95: percentile_usize(&group_sizes, 0.95),
        })
    }

    fn group_key_recipes(
        &self,
        group_indexes: &[usize],
        plan: &CompiledAuraPlan,
    ) -> Result<Vec<GroupKeyRecipe>> {
        let offsets = plan.aura1_field_offsets();
        group_indexes
            .iter()
            .map(|index| {
                let field = self
                    .schema
                    .fields()
                    .get(*index)
                    .ok_or(AuraError::InvalidValue("group field"))?;
                let field_index =
                    u16::try_from(*index).map_err(|_| AuraError::InvalidValue("field index"))?;
                let offset = offsets
                    .iter()
                    .find(|offset| offset.field_index == field_index)
                    .ok_or(AuraError::InvalidValue("field offset"))?;
                Ok(GroupKeyRecipe {
                    field_name: field.name.clone(),
                    aura_type: field.aura_type,
                    offset: offset.offset,
                    width: offset.width,
                })
            })
            .collect()
    }

    fn process_group_body<F>(
        &self,
        body: &[u8],
        row_start: usize,
        row_count: usize,
        recipes: &[GroupKeyRecipe],
        state: &mut RawGroupState,
        group_sizes: &mut Vec<usize>,
        visitor: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&AuraEventGroup) -> Result<()>,
    {
        let record_width = self
            .compiled_plan
            .as_ref()
            .ok_or(AuraError::InvalidValue("compiled plan"))?
            .aura1_record_width;
        let expected_len = row_count
            .checked_mul(record_width)
            .ok_or(AuraError::InvalidValue("body length"))?;
        if body.len() != expected_len {
            return Err(AuraError::UnexpectedEof);
        }
        for local_row in 0..row_count {
            let absolute_row = row_start.saturating_add(local_row);
            let row_offset = local_row
                .checked_mul(record_width)
                .ok_or(AuraError::InvalidValue("row range"))?;
            let row = body
                .get(row_offset..row_offset + record_width)
                .ok_or(AuraError::UnexpectedEof)?;
            if state.row_count == 0 {
                copy_group_key(row, recipes, &mut state.key)?;
                state.row_start = absolute_row;
                state.row_count = 1;
            } else if row_key_matches(row, recipes, &state.key)? {
                state.row_count = state.row_count.saturating_add(1);
            } else {
                self.emit_raw_group(recipes, state, visitor)?;
                group_sizes.push(state.row_count);
                copy_group_key(row, recipes, &mut state.key)?;
                state.row_start = absolute_row;
                state.row_count = 1;
            }
        }
        Ok(())
    }

    fn emit_raw_group<F>(
        &self,
        recipes: &[GroupKeyRecipe],
        state: &RawGroupState,
        visitor: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&AuraEventGroup) -> Result<()>,
    {
        let mut key_offset = 0usize;
        let mut field_names = Vec::with_capacity(recipes.len());
        let mut values = Vec::with_capacity(recipes.len());
        for recipe in recipes {
            let end = key_offset
                .checked_add(recipe.width)
                .ok_or(AuraError::InvalidValue("group key"))?;
            let bytes = state
                .key
                .get(key_offset..end)
                .ok_or(AuraError::UnexpectedEof)?;
            let value = read_i64_fixed_width(bytes)?;
            field_names.push(recipe.field_name.clone());
            values.push(AuraValue::from_i64_for_type(value, recipe.aura_type));
            key_offset = end;
        }
        let group = AuraEventGroup {
            row_start: state.row_start,
            row_count: state.row_count,
            key: AuraGroupKey {
                field_names,
                values,
            },
        };
        visitor(&group)
    }

    pub fn stats(&self) -> AuraReaderStats {
        self.stats.get()
    }

    pub fn rows_i64(&self) -> Result<Vec<Vec<i64>>> {
        let mut rows = Vec::new();
        self.replay_i64(|row| {
            rows.push(row.to_vec());
            Ok(())
        })?;
        Ok(rows)
    }

    pub fn into_rows_i64(self) -> Result<Vec<Vec<i64>>> {
        self.rows_i64()
    }

    pub fn bytes(&self) -> &[u8] {
        self.source.as_memory().unwrap_or(&[])
    }

    fn read_aura1_batch(&self, rows_to_read: usize) -> Result<Vec<Vec<i64>>> {
        match &self.source {
            AuraReaderSource::Memory(bytes) => {
                let mut rows = Vec::with_capacity(rows_to_read);
                records::visit_i64_rows_file_range(bytes, self.cursor, rows_to_read, |row| {
                    rows.push(row.to_vec());
                    Ok(())
                })?;
                Ok(rows)
            }
            AuraReaderSource::FileRange(_) => {
                let (body, rows_to_visit) =
                    self.read_aura1_body_range(self.cursor, rows_to_read)?;
                let plan = self
                    .compiled_plan
                    .as_ref()
                    .ok_or(AuraError::InvalidValue("compiled plan"))?;
                let mut rows = Vec::with_capacity(rows_to_visit);
                records::visit_aura1_body(
                    &body,
                    &plan.aura1_plan,
                    rows_to_visit,
                    plan.field_count,
                    &mut |row| {
                        rows.push(row.to_vec());
                        Ok(())
                    },
                )?;
                Ok(rows)
            }
        }
    }

    fn read_aura1_column_batch(&self, rows_to_read: usize) -> Result<AuraColumnBatch> {
        match &self.source {
            AuraReaderSource::Memory(bytes) => {
                read_aura1_column_batch(bytes, self.schema.clone(), self.cursor, rows_to_read)
            }
            AuraReaderSource::FileRange(_) => {
                let (body, rows_to_visit) =
                    self.read_aura1_body_range(self.cursor, rows_to_read)?;
                let plan = self
                    .compiled_plan
                    .as_ref()
                    .ok_or(AuraError::InvalidValue("compiled plan"))?;
                read_aura1_column_batch_from_body(&body, self.schema.clone(), plan, rows_to_visit)
            }
        }
    }

    fn replay_aura1<F>(&self, mut visitor: F) -> Result<usize>
    where
        F: FnMut(&[i64]) -> Result<()>,
    {
        match &self.source {
            AuraReaderSource::Memory(bytes) => {
                let rows = records::visit_i64_rows_file(bytes, visitor)?;
                let plan = self
                    .compiled_plan
                    .as_ref()
                    .ok_or(AuraError::InvalidValue("compiled plan"))?;
                self.update_stats(|stats| {
                    stats.rows_scanned = stats.rows_scanned.saturating_add(rows);
                    stats.visitor_calls = stats.visitor_calls.saturating_add(rows);
                    stats.field_decode_count = stats
                        .field_decode_count
                        .saturating_add(rows.saturating_mul(plan.field_count));
                    stats.endian_load_count = stats
                        .endian_load_count
                        .saturating_add(rows.saturating_mul(plan.field_count));
                    stats.temp_row_buffers_allocated =
                        stats.temp_row_buffers_allocated.saturating_add(1);
                    stats.bytes_read_during_replay = stats
                        .bytes_read_during_replay
                        .saturating_add(plan.aura1_body_size);
                    stats.compiled_plan_used = true;
                });
                Ok(rows)
            }
            AuraReaderSource::FileRange(_) => {
                let plan = self
                    .compiled_plan
                    .as_ref()
                    .ok_or(AuraError::InvalidValue("compiled plan"))?;
                let rows_per_chunk = rows_per_file_chunk(plan.aura1_record_width);
                let mut rows_seen = 0usize;
                let mut chunk_count = 0usize;
                while rows_seen < plan.record_count {
                    let rows_to_read = rows_per_chunk.min(plan.record_count - rows_seen);
                    let (body, rows_to_visit) =
                        self.read_aura1_body_range(rows_seen, rows_to_read)?;
                    chunk_count = chunk_count.saturating_add(1);
                    records::visit_aura1_body(
                        &body,
                        &plan.aura1_plan,
                        rows_to_visit,
                        plan.field_count,
                        &mut visitor,
                    )?;
                    rows_seen = rows_seen.saturating_add(rows_to_visit);
                }
                self.update_stats(|stats| {
                    stats.rows_scanned = stats.rows_scanned.saturating_add(rows_seen);
                    stats.visitor_calls = stats.visitor_calls.saturating_add(rows_seen);
                    stats.field_decode_count = stats
                        .field_decode_count
                        .saturating_add(rows_seen.saturating_mul(plan.field_count));
                    stats.endian_load_count = stats
                        .endian_load_count
                        .saturating_add(rows_seen.saturating_mul(plan.field_count));
                    stats.temp_row_buffers_allocated =
                        stats.temp_row_buffers_allocated.saturating_add(chunk_count);
                    stats.compiled_plan_used = true;
                });
                Ok(rows_seen)
            }
        }
    }

    fn read_aura1_body_range(&self, start_row: usize, max_rows: usize) -> Result<(Vec<u8>, usize)> {
        let plan = self
            .compiled_plan
            .as_ref()
            .ok_or(AuraError::InvalidValue("compiled plan"))?;
        if start_row > plan.record_count {
            return Err(AuraError::InvalidValue("row range"));
        }
        let rows_to_read = max_rows.min(plan.record_count - start_row);
        if rows_to_read == 0 {
            return Ok((Vec::new(), 0));
        }
        let byte_start = start_row
            .checked_mul(plan.aura1_record_width)
            .ok_or(AuraError::InvalidValue("row range"))?;
        let byte_len = rows_to_read
            .checked_mul(plan.aura1_record_width)
            .ok_or(AuraError::InvalidValue("row range"))?;
        let byte_end = byte_start
            .checked_add(byte_len)
            .ok_or(AuraError::InvalidValue("row range"))?;
        let source = self
            .source
            .file_range()
            .ok_or(AuraError::InvalidValue("reader source"))?;
        if byte_end > source.body_bytes {
            return Err(AuraError::UnexpectedEof);
        }
        let offset = source
            .body_offset
            .checked_add(byte_start)
            .ok_or(AuraError::InvalidValue("file range"))?;
        if offset
            .checked_add(byte_len)
            .ok_or(AuraError::InvalidValue("file range"))?
            > source.footer_offset
        {
            return Err(AuraError::UnexpectedEof);
        }
        let body = source.read_at(offset, byte_len)?;
        self.update_stats(|stats| {
            stats.bytes_read_during_replay =
                stats.bytes_read_during_replay.saturating_add(byte_len);
            stats.bytes_read_in_last_batch =
                stats.bytes_read_in_last_batch.saturating_add(byte_len);
            stats.source_bytes_read_total = stats.source_bytes_read_total.saturating_add(byte_len);
        });
        Ok((body, rows_to_read))
    }

    fn read_aura0_batch(&mut self, rows_to_read: usize) -> Result<Vec<Vec<i64>>> {
        self.ensure_aura0_columns()?;
        let end = self.cursor.saturating_add(rows_to_read);
        match &self.state {
            AuraReaderState::Aura0Columns {
                columns: Some(columns),
            } => rows_from_columns(columns, self.cursor, end),
            AuraReaderState::Aura0ByteLane { aura1: Some(aura1) } => {
                let mut rows = Vec::with_capacity(rows_to_read);
                records::visit_i64_rows_file_range(aura1, self.cursor, rows_to_read, |row| {
                    rows.push(row.to_vec());
                    Ok(())
                })?;
                Ok(rows)
            }
            _ => Err(AuraError::InvalidValue("aura0 stream state")),
        }
    }

    fn read_aura0_column_batch(&mut self, rows_to_read: usize) -> Result<AuraColumnBatch> {
        self.ensure_aura0_columns()?;
        let end = self.cursor.saturating_add(rows_to_read);
        match &self.state {
            AuraReaderState::Aura0Columns {
                columns: Some(columns),
            } => AuraColumnBatch::from_i64_columns(
                self.schema.clone(),
                slice_columns(columns, self.cursor, end)?,
            ),
            AuraReaderState::Aura0ByteLane { aura1: Some(aura1) } => {
                read_aura1_column_batch(aura1, self.schema.clone(), self.cursor, rows_to_read)
            }
            _ => Err(AuraError::InvalidValue("aura0 stream state")),
        }
    }

    fn read_lazy_rows_batch(&mut self, rows_to_read: usize) -> Result<Vec<Vec<i64>>> {
        self.ensure_lazy_rows()?;
        let end = self.cursor.saturating_add(rows_to_read);
        match &self.state {
            AuraReaderState::LazyRows { rows: Some(rows) } => rows
                .get(self.cursor..end)
                .map(<[Vec<i64>]>::to_vec)
                .ok_or(AuraError::UnexpectedEof),
            _ => Err(AuraError::InvalidValue("reader row state")),
        }
    }

    fn ensure_aura0_columns(&mut self) -> Result<()> {
        let needs_decode = matches!(
            self.state,
            AuraReaderState::Aura0Columns { columns: None }
                | AuraReaderState::Aura0ByteLane { aura1: None }
        );
        if needs_decode {
            let bytes = self
                .source
                .as_memory()
                .ok_or(AuraError::InvalidValue("reader source"))?;
            if let Some(decoded) = records::decode_i64_columns_file(bytes)? {
                self.state = AuraReaderState::Aura0Columns {
                    columns: Some(decoded.columns),
                };
            } else {
                let aura1 = records::compile_aura0_to_aura1_bytes_with_lane(
                    bytes,
                    self.use_byte_lane,
                    false,
                )?;
                self.state = AuraReaderState::Aura0ByteLane { aura1: Some(aura1) };
            }
        }
        Ok(())
    }

    fn ensure_lazy_rows(&mut self) -> Result<()> {
        let needs_decode = matches!(self.state, AuraReaderState::LazyRows { rows: None });
        if needs_decode {
            let bytes = self
                .source
                .as_memory()
                .ok_or(AuraError::InvalidValue("reader source"))?;
            let decoded = records::decode_i64_file(bytes)?;
            self.update_stats(|stats| {
                stats.full_file_materialized = true;
                stats.max_rows_materialized_at_once =
                    stats.max_rows_materialized_at_once.max(decoded.rows.len());
            });
            self.state = AuraReaderState::LazyRows {
                rows: Some(decoded.rows),
            };
        }
        Ok(())
    }

    fn group_indexes(&self, group_by: &GroupBy) -> Result<Vec<usize>> {
        if group_by.is_empty() {
            return Err(AuraError::InvalidValue("group fields"));
        }
        let mut indexes = Vec::with_capacity(group_by.len());
        for field in &group_by.fields {
            let index = match field {
                GroupByField::Name(name) => self
                    .schema
                    .fields()
                    .iter()
                    .position(|field| field.name == *name)
                    .ok_or(AuraError::InvalidValue("group field"))?,
                GroupByField::Id(id) => self
                    .schema
                    .fields()
                    .iter()
                    .position(|field| field.id == *id)
                    .ok_or(AuraError::InvalidValue("group field"))?,
            };
            if indexes.contains(&index) {
                return Err(AuraError::InvalidValue("group field"));
            }
            let field = &self.schema.fields()[index];
            if !field.aura_type.is_supported() || field.aura_type.byte_width().is_none() {
                return Err(AuraError::InvalidValue("group field type"));
            }
            indexes.push(index);
        }
        Ok(indexes)
    }

    fn group_from_i64_key(
        &self,
        indexes: &[usize],
        values: &[i64],
        row_start: usize,
        row_count: usize,
    ) -> Result<AuraEventGroup> {
        if indexes.len() != values.len() {
            return Err(AuraError::InvalidValue("group key"));
        }
        let mut field_names = Vec::with_capacity(indexes.len());
        let mut key_values = Vec::with_capacity(indexes.len());
        for (index, value) in indexes.iter().zip(values) {
            let field = &self.schema.fields()[*index];
            field_names.push(field.name.clone());
            key_values.push(AuraValue::from_i64_for_type(*value, field.aura_type));
        }
        Ok(AuraEventGroup {
            row_start,
            row_count,
            key: AuraGroupKey {
                field_names,
                values: key_values,
            },
        })
    }

    fn update_stats(&self, update: impl FnOnce(&mut AuraReaderStats)) {
        let mut stats = self.stats.get();
        update(&mut stats);
        self.stats.set(stats);
    }
}

pub struct AuraBatchIter<'a> {
    reader: AuraReader,
    batch_size: usize,
    _marker: std::marker::PhantomData<&'a AuraReader>,
}

impl Iterator for AuraBatchIter<'_> {
    type Item = Result<AuraRecordBatch>;

    fn next(&mut self) -> Option<Self::Item> {
        self.reader.next_batch(self.batch_size).transpose()
    }
}

fn rows_from_columns(columns: &[Vec<i64>], start: usize, end: usize) -> Result<Vec<Vec<i64>>> {
    let field_count = columns.len();
    for column in columns {
        if end > column.len() {
            return Err(AuraError::UnexpectedEof);
        }
    }
    let mut rows = Vec::with_capacity(end.saturating_sub(start));
    for row_index in start..end {
        let mut row = Vec::with_capacity(field_count);
        for column in columns {
            row.push(column[row_index]);
        }
        rows.push(row);
    }
    Ok(rows)
}

fn read_aura1_column_batch(
    bytes: &[u8],
    schema: AuraSchema,
    start_row: usize,
    rows_to_read: usize,
) -> Result<AuraColumnBatch> {
    let field_count = schema.field_count();
    let mut columns = (0..field_count)
        .map(|_| Vec::with_capacity(rows_to_read))
        .collect::<Vec<_>>();
    records::visit_i64_rows_file_range(bytes, start_row, rows_to_read, |row| {
        if row.len() != field_count {
            return Err(AuraError::InvalidValue("record field count"));
        }
        for (column, value) in columns.iter_mut().zip(row) {
            column.push(*value);
        }
        Ok(())
    })?;
    AuraColumnBatch::from_i64_columns(schema, columns)
}

fn read_aura1_column_batch_from_body(
    body: &[u8],
    schema: AuraSchema,
    plan: &CompiledAuraPlan,
    rows_to_read: usize,
) -> Result<AuraColumnBatch> {
    let field_count = schema.field_count();
    let mut columns = (0..field_count)
        .map(|_| Vec::with_capacity(rows_to_read))
        .collect::<Vec<_>>();
    records::visit_aura1_body(
        body,
        &plan.aura1_plan,
        rows_to_read,
        plan.field_count,
        &mut |row| {
            if row.len() != field_count {
                return Err(AuraError::InvalidValue("record field count"));
            }
            for (column, value) in columns.iter_mut().zip(row) {
                column.push(*value);
            }
            Ok(())
        },
    )?;
    AuraColumnBatch::from_i64_columns(schema, columns)
}

fn read_i64_fixed_width(bytes: &[u8]) -> Result<i64> {
    match bytes.len() {
        0 => Ok(0),
        1 => Ok(bytes[0] as i8 as i64),
        2 => Ok(i16::from_le_bytes([bytes[0], bytes[1]]) as i64),
        4 => Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i64),
        8 => Ok(i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])),
        16 => {
            let value = i128::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]);
            i64::try_from(value).map_err(|_| AuraError::InvalidValue("i128 value"))
        }
        _ => Err(AuraError::InvalidValue("field width")),
    }
}

/// Return one Aura1 row slice after `validate_fixed_body` has established that
/// `body` contains `row_count * record_width` bytes.
///
/// # Safety
///
/// `row_index * record_width + record_width` must be within `body`. The replay
/// callers satisfy this by validating the body once per memory/file-range chunk
/// and iterating `row_index < row_count` for that validated chunk.
unsafe fn prevalidated_row_bytes(body: &[u8], row_index: usize, record_width: usize) -> &[u8] {
    let row_start = row_index * record_width;
    unsafe { std::slice::from_raw_parts(body.as_ptr().add(row_start), record_width) }
}

fn copy_group_key(row: &[u8], recipes: &[GroupKeyRecipe], out: &mut Vec<u8>) -> Result<()> {
    out.clear();
    let key_width = recipes.iter().try_fold(0usize, |acc, recipe| {
        acc.checked_add(recipe.width)
            .ok_or(AuraError::InvalidValue("group key"))
    })?;
    if out.capacity() < key_width {
        out.reserve(key_width);
    }
    for recipe in recipes {
        let end = recipe
            .offset
            .checked_add(recipe.width)
            .ok_or(AuraError::InvalidValue("group key"))?;
        let bytes = row
            .get(recipe.offset..end)
            .ok_or(AuraError::UnexpectedEof)?;
        out.extend_from_slice(bytes);
    }
    Ok(())
}

fn row_key_matches(row: &[u8], recipes: &[GroupKeyRecipe], key: &[u8]) -> Result<bool> {
    let mut key_offset = 0usize;
    for recipe in recipes {
        let row_end = recipe
            .offset
            .checked_add(recipe.width)
            .ok_or(AuraError::InvalidValue("group key"))?;
        let key_end = key_offset
            .checked_add(recipe.width)
            .ok_or(AuraError::InvalidValue("group key"))?;
        let row_bytes = row
            .get(recipe.offset..row_end)
            .ok_or(AuraError::UnexpectedEof)?;
        let key_bytes = key
            .get(key_offset..key_end)
            .ok_or(AuraError::UnexpectedEof)?;
        if row_bytes != key_bytes {
            return Ok(false);
        }
        key_offset = key_end;
    }
    Ok(true)
}

fn rows_per_file_chunk(record_width: usize) -> usize {
    const TARGET_BYTES: usize = 1024 * 1024;
    TARGET_BYTES
        .checked_div(record_width.max(1))
        .unwrap_or(1)
        .max(1)
}

fn slice_columns(columns: &[Vec<i64>], start: usize, end: usize) -> Result<Vec<Vec<i64>>> {
    columns
        .iter()
        .map(|column| {
            column
                .get(start..end)
                .map(<[i64]>::to_vec)
                .ok_or(AuraError::UnexpectedEof)
        })
        .collect()
}

fn rows_to_columns(rows: &[Vec<i64>]) -> Result<Vec<Vec<i64>>> {
    let Some(first) = rows.first() else {
        return Ok(Vec::new());
    };
    let field_count = first.len();
    let mut columns = (0..field_count)
        .map(|_| Vec::with_capacity(rows.len()))
        .collect::<Vec<_>>();
    for row in rows {
        if row.len() != field_count {
            return Err(AuraError::InvalidValue("record field count"));
        }
        for (column, value) in columns.iter_mut().zip(row) {
            column.push(*value);
        }
    }
    Ok(columns)
}

fn percentile_usize(values: &[usize], percentile: f64) -> usize {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len().saturating_sub(1));
    sorted[index]
}

struct ParsedFileMetadata {
    header: AuraHeader,
    compiled_footer: Option<CompiledFooter>,
    file_len: usize,
    header_len: usize,
    footer_offset: usize,
    footer_len: usize,
    bytes_read_at_open: usize,
}

fn parse_file_backed_metadata(file: &mut File) -> Result<ParsedFileMetadata> {
    let file_len_u64 = file
        .seek(SeekFrom::End(0))
        .map_err(|_| AuraError::InvalidValue("reader input"))?;
    let file_len =
        usize::try_from(file_len_u64).map_err(|_| AuraError::InvalidValue("file len"))?;
    let trailer_len = 4usize
        .checked_add(SEAL_MAGIC.len())
        .ok_or(AuraError::InvalidValue("trailer length"))?;
    if file_len < crate::header::HEADER_PREFIX_SIZE + trailer_len {
        return Err(AuraError::UnexpectedEof);
    }

    let prefix = read_file_exact_at(file, 0, crate::header::HEADER_PREFIX_SIZE)?;
    let header_len = AuraHeader::encoded_len(&prefix)?;
    if header_len > file_len {
        return Err(AuraError::UnexpectedEof);
    }
    let header_bytes = read_file_exact_at(file, 0, header_len)?;
    let header = AuraHeader::decode(&header_bytes)?;

    let trailer_offset = file_len
        .checked_sub(trailer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    let trailer = read_file_exact_at(file, trailer_offset, trailer_len)?;
    if &trailer[4..] != SEAL_MAGIC {
        return Err(AuraError::InvalidMagic {
            expected: "sealed:)",
        });
    }
    let footer_len = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]) as usize;
    let footer_offset = trailer_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_offset < header_len {
        return Err(AuraError::UnexpectedEof);
    }
    let footer_bytes = read_file_exact_at(file, footer_offset, footer_len)?;
    let compiled_footer = match header.profile {
        Profile::Aura0 | Profile::Aura1 => Some(CompiledFooter::decode(&footer_bytes)?),
        Profile::Ingest => None,
    };

    Ok(ParsedFileMetadata {
        header,
        compiled_footer,
        file_len,
        header_len,
        footer_offset,
        footer_len,
        bytes_read_at_open: prefix
            .len()
            .saturating_add(header_bytes.len())
            .saturating_add(trailer.len())
            .saturating_add(footer_bytes.len()),
    })
}

fn read_file_exact_at(file: &mut File, offset: usize, len: usize) -> Result<Vec<u8>> {
    let offset = u64::try_from(offset).map_err(|_| AuraError::InvalidValue("file offset"))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| AuraError::InvalidValue("reader input"))?;
    let mut bytes = vec![0u8; len];
    match file.read_exact(&mut bytes) {
        Ok(()) => Ok(bytes),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Err(AuraError::UnexpectedEof),
        Err(_) => Err(AuraError::InvalidValue("reader input")),
    }
}

/// In-memory reader for sealed Aura i64 files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraI64Reader {
    decoded: DecodedI64File,
}

/// In-memory reader for sealed Aura typed files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraTypedReader {
    decoded: DecodedTypedFile,
}

impl AuraI64Reader {
    pub fn open(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            decoded: decode_i64(bytes)?,
        })
    }

    pub fn header(&self) -> &AuraHeader {
        &self.decoded.header
    }

    pub fn profile(&self) -> Profile {
        self.decoded.header.profile
    }

    pub fn schema(&self) -> &SchemaDescriptor {
        &self.decoded.schema
    }

    pub fn rows(&self) -> &[Vec<i64>] {
        &self.decoded.rows
    }

    pub fn ingest_footer(&self) -> Option<&AuraFooter> {
        self.decoded.ingest_footer.as_ref()
    }

    pub fn compiled_footer(&self) -> Option<&CompiledFooter> {
        self.decoded.compiled_footer.as_ref()
    }

    pub fn into_rows(self) -> Vec<Vec<i64>> {
        self.decoded.rows
    }

    pub fn into_decoded(self) -> DecodedI64File {
        self.decoded
    }
}

impl AuraTypedReader {
    pub fn open(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            decoded: decode_typed(bytes)?,
        })
    }

    pub fn header(&self) -> &AuraHeader {
        &self.decoded.header
    }

    pub fn profile(&self) -> Profile {
        self.decoded.header.profile
    }

    pub fn schema(&self) -> &SchemaDescriptor {
        &self.decoded.schema
    }

    pub fn rows(&self) -> &[Vec<AuraTypedValue>] {
        &self.decoded.rows
    }

    pub fn ingest_footer(&self) -> Option<&AuraFooter> {
        self.decoded.ingest_footer.as_ref()
    }

    pub fn compiled_footer(&self) -> Option<&CompiledFooter> {
        self.decoded.compiled_footer.as_ref()
    }

    pub fn into_rows(self) -> Vec<Vec<AuraTypedValue>> {
        self.decoded.rows
    }

    pub fn into_decoded(self) -> DecodedTypedFile {
        self.decoded
    }
}

pub fn decode_i64(bytes: &[u8]) -> Result<DecodedI64File> {
    records::decode_i64_file_inner(bytes)
}

pub fn decode_typed(bytes: &[u8]) -> Result<DecodedTypedFile> {
    records::decode_typed_file_inner(bytes)
}
