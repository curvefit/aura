use std::cell::Cell;
use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::footer::AuraFooter;
use crate::format::SEAL_MAGIC;
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
    pub source_bytes_read_at_open: usize,
    pub source_bytes_read_total: usize,
    pub streaming_reader_used: bool,
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
            AuraReaderSource::Memory(bytes) => records::visit_i64_rows_file(bytes, visitor),
            AuraReaderSource::FileRange(_) => {
                let plan = self
                    .compiled_plan
                    .as_ref()
                    .ok_or(AuraError::InvalidValue("compiled plan"))?;
                let rows_per_chunk = rows_per_file_chunk(plan.aura1_record_width);
                let mut rows_seen = 0usize;
                while rows_seen < plan.record_count {
                    let rows_to_read = rows_per_chunk.min(plan.record_count - rows_seen);
                    let (body, rows_to_visit) =
                        self.read_aura1_body_range(rows_seen, rows_to_read)?;
                    records::visit_aura1_body(
                        &body,
                        &plan.aura1_plan,
                        rows_to_visit,
                        plan.field_count,
                        &mut visitor,
                    )?;
                    rows_seen = rows_seen.saturating_add(rows_to_visit);
                }
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
