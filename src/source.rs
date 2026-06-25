use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;

use crate::reader::{Aura1FixedBatchView, AuraReader};
use crate::{AuraError, AuraFormat, AuraSchema, CompiledAuraPlan, ReaderOptions, Result};

/// Common borrowed fixed-width batch surface for Aura event sources.
pub trait AuraEventBatch {
    fn row_count(&self) -> usize;
    fn record_width(&self) -> usize;
    fn field_count(&self) -> usize;
    fn value_i64(&self, row_index: usize, field_index: usize) -> Result<i64>;
    fn checksum_all_fields(&self) -> Result<u64>;
}

impl AuraEventBatch for Aura1FixedBatchView<'_> {
    fn row_count(&self) -> usize {
        Aura1FixedBatchView::row_count(self)
    }

    fn record_width(&self) -> usize {
        Aura1FixedBatchView::record_width(self)
    }

    fn field_count(&self) -> usize {
        Aura1FixedBatchView::field_count(self)
    }

    fn value_i64(&self, row_index: usize, field_index: usize) -> Result<i64> {
        Aura1FixedBatchView::value_i64(self, row_index, field_index)
    }

    fn checksum_all_fields(&self) -> Result<u64> {
        Aura1FixedBatchView::checksum_all_fields(self)
    }
}

/// Shared historical/live interface for fixed-width Aura event streams.
pub trait AuraEventSource {
    type Batch<'a>: AuraEventBatch
    where
        Self: 'a;

    fn schema(&self) -> &AuraSchema;
    fn compiled_plan(&self) -> &CompiledAuraPlan;
    fn next_batch(&mut self) -> Result<Option<Self::Batch<'_>>>;
}

/// Historical Aura1 source backed by owned in-memory bytes.
#[derive(Debug, Clone)]
pub struct AuraMemorySource {
    reader: AuraReader,
    batch_size: usize,
}

/// Historical Aura1 source backed by range reads from a file.
#[derive(Debug, Clone)]
pub struct AuraFileSource {
    reader: AuraReader,
    batch_size: usize,
}

/// Live Aura1 body source backed by a byte stream of fixed-width records.
#[derive(Debug)]
pub struct AuraLiveSource<R> {
    input: R,
    schema: AuraSchema,
    compiled_plan: CompiledAuraPlan,
    batch_size: usize,
    batch_bytes: Vec<u8>,
    finished: bool,
}

impl AuraMemorySource {
    pub fn try_new(bytes: Vec<u8>, batch_size: usize) -> Result<Self> {
        validate_batch_size(batch_size)?;
        let reader = AuraReader::open_bytes(bytes)?;
        validate_aura1_reader(&reader)?;
        Ok(Self { reader, batch_size })
    }

    pub fn with_options(bytes: Vec<u8>, batch_size: usize, options: ReaderOptions) -> Result<Self> {
        validate_batch_size(batch_size)?;
        let reader = AuraReader::open_bytes_with_options(bytes, options)?;
        validate_aura1_reader(&reader)?;
        Ok(Self { reader, batch_size })
    }

    pub fn into_reader(self) -> AuraReader {
        self.reader
    }
}

impl AuraEventSource for AuraMemorySource {
    type Batch<'a> = Aura1FixedBatchView<'a>;

    fn schema(&self) -> &AuraSchema {
        self.reader.schema()
    }

    fn compiled_plan(&self) -> &CompiledAuraPlan {
        self.reader
            .compiled_plan()
            .expect("AuraMemorySource validates Aura1 compiled plan")
    }

    fn next_batch(&mut self) -> Result<Option<Self::Batch<'_>>> {
        self.reader.next_fixed_batch(self.batch_size)
    }
}

impl AuraFileSource {
    pub fn open_path(path: impl AsRef<Path>, batch_size: usize) -> Result<Self> {
        Self::open_path_with_options(path, batch_size, ReaderOptions::default())
    }

    pub fn open_path_with_options(
        path: impl AsRef<Path>,
        batch_size: usize,
        options: ReaderOptions,
    ) -> Result<Self> {
        validate_batch_size(batch_size)?;
        let reader = AuraReader::open_path_with_options(path, options)?;
        validate_aura1_reader(&reader)?;
        Ok(Self { reader, batch_size })
    }

    pub fn open_file(file: File, batch_size: usize) -> Result<Self> {
        Self::open_file_with_options(file, batch_size, ReaderOptions::default())
    }

    pub fn open_file_with_options(
        file: File,
        batch_size: usize,
        options: ReaderOptions,
    ) -> Result<Self> {
        validate_batch_size(batch_size)?;
        let reader = AuraReader::open_file_with_options(file, options)?;
        validate_aura1_reader(&reader)?;
        Ok(Self { reader, batch_size })
    }

    pub fn into_reader(self) -> AuraReader {
        self.reader
    }
}

impl AuraEventSource for AuraFileSource {
    type Batch<'a> = Aura1FixedBatchView<'a>;

    fn schema(&self) -> &AuraSchema {
        self.reader.schema()
    }

    fn compiled_plan(&self) -> &CompiledAuraPlan {
        self.reader
            .compiled_plan()
            .expect("AuraFileSource validates Aura1 compiled plan")
    }

    fn next_batch(&mut self) -> Result<Option<Self::Batch<'_>>> {
        self.reader.next_fixed_batch(self.batch_size)
    }
}

impl<R: Read> AuraLiveSource<R> {
    pub fn try_new(input: R, schema: AuraSchema, batch_size: usize) -> Result<Self> {
        let compiled_plan = CompiledAuraPlan::from_schema(&schema)?;
        Self::with_plan(input, schema, compiled_plan, batch_size)
    }

    pub fn with_plan(
        input: R,
        schema: AuraSchema,
        compiled_plan: CompiledAuraPlan,
        batch_size: usize,
    ) -> Result<Self> {
        validate_batch_size(batch_size)?;
        if schema.hash() != compiled_plan.schema_hash {
            return Err(AuraError::InvalidValue("live source schema"));
        }
        if compiled_plan.aura1_record_width == 0 {
            return Err(AuraError::InvalidValue("live source record width"));
        }
        Ok(Self {
            input,
            schema,
            compiled_plan,
            batch_size,
            batch_bytes: Vec::new(),
            finished: false,
        })
    }

    pub fn into_inner(self) -> R {
        self.input
    }
}

impl<R: Read> AuraEventSource for AuraLiveSource<R> {
    type Batch<'a>
        = Aura1FixedBatchView<'a>
    where
        R: 'a;

    fn schema(&self) -> &AuraSchema {
        &self.schema
    }

    fn compiled_plan(&self) -> &CompiledAuraPlan {
        &self.compiled_plan
    }

    fn next_batch(&mut self) -> Result<Option<Self::Batch<'_>>> {
        if self.finished {
            return Ok(None);
        }
        let record_width = self.compiled_plan.aura1_record_width;
        let target_len = self
            .batch_size
            .checked_mul(record_width)
            .ok_or(AuraError::InvalidValue("batch size"))?;
        self.batch_bytes.clear();

        while self.batch_bytes.len() < target_len {
            let start = self.batch_bytes.len();
            self.batch_bytes.resize(target_len, 0);
            match self.input.read(&mut self.batch_bytes[start..target_len]) {
                Ok(0) => {
                    self.batch_bytes.truncate(start);
                    self.finished = true;
                    break;
                }
                Ok(read) => {
                    self.batch_bytes.truncate(start + read);
                    if self.batch_bytes.len() >= record_width
                        && self.batch_bytes.len() % record_width == 0
                    {
                        break;
                    }
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {
                    self.batch_bytes.truncate(start);
                    continue;
                }
                Err(error) if error.kind() == ErrorKind::UnexpectedEof => {
                    return Err(AuraError::UnexpectedEof);
                }
                Err(_) => return Err(AuraError::InvalidValue("live source input")),
            }
        }

        if self.batch_bytes.is_empty() {
            return Ok(None);
        }
        if self.batch_bytes.len() % record_width != 0 {
            return Err(AuraError::UnexpectedEof);
        }
        let row_count = self.batch_bytes.len() / record_width;
        Ok(Some(Aura1FixedBatchView::new(
            &self.batch_bytes,
            &self.compiled_plan,
            row_count,
        )?))
    }
}

fn validate_batch_size(batch_size: usize) -> Result<()> {
    if batch_size == 0 {
        return Err(AuraError::InvalidValue("batch size"));
    }
    Ok(())
}

fn validate_aura1_reader(reader: &AuraReader) -> Result<()> {
    if reader.format() != AuraFormat::Aura1 {
        return Err(AuraError::InvalidValue("aura1 event source"));
    }
    let _ = reader
        .compiled_plan()
        .ok_or(AuraError::InvalidValue("compiled plan"))?;
    Ok(())
}
