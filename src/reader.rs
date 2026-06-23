use std::io::Read;

use crate::footer::AuraFooter;
use crate::header::AuraHeader;
use crate::options::{AuraFormat, ReaderOptions};
use crate::program::{CompiledAuraPlan, CompiledFooter};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuraReaderStats {
    pub open_decoded_row_count: usize,
    pub full_file_materialized: bool,
    pub batches_read: usize,
    pub rows_decoded_in_last_batch: usize,
    pub max_rows_materialized_at_once: usize,
    pub source_bytes_read_at_open: usize,
    pub source_bytes_read_total: usize,
    pub streaming_reader_used: bool,
}

/// Public SDK reader for Aura files with dynamic schemas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraReader {
    bytes: Vec<u8>,
    schema: AuraSchema,
    profile: Profile,
    compiled_footer: Option<CompiledFooter>,
    compiled_plan: Option<CompiledAuraPlan>,
    cursor: usize,
    record_count: usize,
    state: AuraReaderState,
    stats: AuraReaderStats,
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
        Ok(Self {
            stats: AuraReaderStats {
                open_decoded_row_count: 0,
                full_file_materialized: false,
                batches_read: 0,
                rows_decoded_in_last_batch: 0,
                max_rows_materialized_at_once: 0,
                source_bytes_read_at_open: bytes.len(),
                source_bytes_read_total: bytes.len(),
                streaming_reader_used: true,
            },
            bytes,
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
        let rows_to_read = batch_size.min(self.record_count - self.cursor);
        let end = self.cursor.saturating_add(rows_to_read);
        let rows = match self.profile {
            Profile::Aura1 => self.read_aura1_batch(rows_to_read)?,
            Profile::Aura0 => self.read_aura0_batch(rows_to_read)?,
            Profile::Ingest => self.read_lazy_rows_batch(rows_to_read)?,
        };
        self.cursor = end;
        self.stats.batches_read = self.stats.batches_read.saturating_add(1);
        self.stats.rows_decoded_in_last_batch = rows.len();
        self.stats.max_rows_materialized_at_once =
            self.stats.max_rows_materialized_at_once.max(rows.len());
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
        let rows_to_read = batch_size.min(self.record_count - self.cursor);
        let end = self.cursor.saturating_add(rows_to_read);
        let batch = match self.profile {
            Profile::Aura1 => read_aura1_column_batch(
                &self.bytes,
                self.schema.clone(),
                self.cursor,
                rows_to_read,
            )?,
            Profile::Aura0 => self.read_aura0_column_batch(rows_to_read)?,
            Profile::Ingest => {
                let rows = self.read_lazy_rows_batch(rows_to_read)?;
                AuraColumnBatch::from_i64_columns(self.schema.clone(), rows_to_columns(&rows)?)?
            }
        };
        self.cursor = end;
        self.stats.batches_read = self.stats.batches_read.saturating_add(1);
        self.stats.rows_decoded_in_last_batch = batch.row_count();
        Ok(Some(batch))
    }

    pub fn reset_batches(&mut self) {
        self.cursor = 0;
        self.stats.rows_decoded_in_last_batch = 0;
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
            Profile::Aura1 => records::visit_i64_rows_file(&self.bytes, visitor),
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

    pub fn grouped_replay<F>(&self, group_by: &GroupBy, mut visitor: F) -> Result<AuraGroupStats>
    where
        F: FnMut(&AuraEventGroup) -> Result<()>,
    {
        let group_indexes = self.group_indexes(group_by)?;
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

    pub fn stats(&self) -> AuraReaderStats {
        self.stats
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
        &self.bytes
    }

    fn read_aura1_batch(&self, rows_to_read: usize) -> Result<Vec<Vec<i64>>> {
        let mut rows = Vec::with_capacity(rows_to_read);
        records::visit_i64_rows_file_range(&self.bytes, self.cursor, rows_to_read, |row| {
            rows.push(row.to_vec());
            Ok(())
        })?;
        Ok(rows)
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
            if let Some(decoded) = records::decode_i64_columns_file(&self.bytes)? {
                self.state = AuraReaderState::Aura0Columns {
                    columns: Some(decoded.columns),
                };
            } else {
                let aura1 = records::compile_aura0_to_aura1_bytes_with_lane(
                    &self.bytes,
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
            let decoded = records::decode_i64_file(&self.bytes)?;
            self.stats.full_file_materialized = true;
            self.stats.max_rows_materialized_at_once = self
                .stats
                .max_rows_materialized_at_once
                .max(decoded.rows.len());
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
