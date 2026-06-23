use std::io::Read;

use crate::footer::AuraFooter;
use crate::header::AuraHeader;
use crate::options::{AuraFormat, ReaderOptions};
use crate::program::{CompiledAuraPlan, CompiledFooter};
use crate::records::{self, DecodedI64File, DecodedTypedFile};
use crate::schema::{AuraSchema, SchemaDescriptor};
use crate::{AuraError, AuraRecordBatch, AuraTypedValue, Profile, Result};

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
