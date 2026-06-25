use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;

use crate::reader::{Aura1FixedBatchView, AuraReader, AuraReaderStats};
use crate::{AuraError, AuraFormat, AuraSchema, CompiledAuraPlan, ReaderOptions, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuraEventSourceStats {
    pub source_kind: &'static str,
    pub buffer_reuse_enabled: bool,
    pub bytes_copied: usize,
    pub bytes_borrowed_or_ranged: usize,
    pub allocations_proxy: usize,
    pub rows_materialized: usize,
    pub full_file_materialized: bool,
    pub full_file_bytes_copied: usize,
}

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
    bytes_copied: usize,
    batch_count: usize,
}

#[derive(Debug, Clone)]
enum AuraLiveFrameStorage {
    Owned(Vec<Vec<u8>>),
    OwnedBody(Vec<u8>),
}

/// Live Aura1 source backed by already-framed fixed-width message chunks.
#[derive(Debug, Clone)]
pub struct AuraLiveFrameSource {
    storage: AuraLiveFrameStorage,
    frame_index: usize,
    cursor: usize,
    frame_bytes: usize,
    schema: AuraSchema,
    compiled_plan: CompiledAuraPlan,
    bytes_borrowed: usize,
    allocations_proxy: usize,
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

    pub fn source_stats(&self) -> AuraEventSourceStats {
        source_stats_from_reader(self.reader.stats(), "memory", true)
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

    pub fn source_stats(&self) -> AuraEventSourceStats {
        source_stats_from_reader(self.reader.stats(), "file_range", false)
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
            bytes_copied: 0,
            batch_count: 0,
        })
    }

    pub fn into_inner(self) -> R {
        self.input
    }

    pub fn source_stats(&self) -> AuraEventSourceStats {
        AuraEventSourceStats {
            source_kind: "live_read",
            buffer_reuse_enabled: true,
            bytes_copied: self.bytes_copied,
            bytes_borrowed_or_ranged: self.bytes_copied,
            allocations_proxy: usize::from(self.batch_bytes.capacity() > 0),
            rows_materialized: 0,
            full_file_materialized: false,
            full_file_bytes_copied: 0,
        }
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
                    self.bytes_copied = self.bytes_copied.saturating_add(read);
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
        self.batch_count = self.batch_count.saturating_add(1);
        Ok(Some(Aura1FixedBatchView::new(
            &self.batch_bytes,
            &self.compiled_plan,
            row_count,
        )?))
    }
}

impl AuraLiveFrameSource {
    pub fn try_new(frames: Vec<Vec<u8>>, schema: AuraSchema) -> Result<Self> {
        let compiled_plan = CompiledAuraPlan::from_schema(&schema)?;
        Self::with_plan(frames, schema, compiled_plan)
    }

    pub fn with_plan(
        frames: Vec<Vec<u8>>,
        schema: AuraSchema,
        compiled_plan: CompiledAuraPlan,
    ) -> Result<Self> {
        if schema.hash() != compiled_plan.schema_hash {
            return Err(AuraError::InvalidValue("live frame source schema"));
        }
        if compiled_plan.aura1_record_width == 0 {
            return Err(AuraError::InvalidValue("live frame record width"));
        }
        for frame in &frames {
            if frame.is_empty() || frame.len() % compiled_plan.aura1_record_width != 0 {
                return Err(AuraError::UnexpectedEof);
            }
        }
        let allocations_proxy = frames.len();
        Ok(Self {
            storage: AuraLiveFrameStorage::Owned(frames),
            frame_index: 0,
            cursor: 0,
            frame_bytes: 0,
            schema,
            compiled_plan,
            bytes_borrowed: 0,
            allocations_proxy,
        })
    }

    pub fn from_body_chunks(
        body: Vec<u8>,
        schema: AuraSchema,
        compiled_plan: CompiledAuraPlan,
        batch_size: usize,
    ) -> Result<Self> {
        validate_batch_size(batch_size)?;
        if schema.hash() != compiled_plan.schema_hash {
            return Err(AuraError::InvalidValue("live frame source schema"));
        }
        let record_width = compiled_plan.aura1_record_width;
        if record_width == 0 {
            return Err(AuraError::InvalidValue("live frame record width"));
        }
        if body.is_empty() || body.len() % record_width != 0 {
            return Err(AuraError::UnexpectedEof);
        }
        let frame_bytes = batch_size
            .checked_mul(record_width)
            .ok_or(AuraError::InvalidValue("batch size"))?;
        Ok(Self {
            storage: AuraLiveFrameStorage::OwnedBody(body),
            frame_index: 0,
            cursor: 0,
            frame_bytes,
            schema,
            compiled_plan,
            bytes_borrowed: 0,
            allocations_proxy: 0,
        })
    }

    pub fn source_stats(&self) -> AuraEventSourceStats {
        AuraEventSourceStats {
            source_kind: "live_frame",
            buffer_reuse_enabled: true,
            bytes_copied: 0,
            bytes_borrowed_or_ranged: self.bytes_borrowed,
            allocations_proxy: self.allocations_proxy,
            rows_materialized: 0,
            full_file_materialized: false,
            full_file_bytes_copied: 0,
        }
    }
}

impl AuraEventSource for AuraLiveFrameSource {
    type Batch<'a>
        = Aura1FixedBatchView<'a>
    where
        Self: 'a;

    fn schema(&self) -> &AuraSchema {
        &self.schema
    }

    fn compiled_plan(&self) -> &CompiledAuraPlan {
        &self.compiled_plan
    }

    fn next_batch(&mut self) -> Result<Option<Self::Batch<'_>>> {
        let record_width = self.compiled_plan.aura1_record_width;
        let frame = match &self.storage {
            AuraLiveFrameStorage::Owned(frames) => {
                if self.frame_index >= frames.len() {
                    return Ok(None);
                }
                let index = self.frame_index;
                self.frame_index = self.frame_index.saturating_add(1);
                frames[index].as_slice()
            }
            AuraLiveFrameStorage::OwnedBody(body) => {
                if self.cursor >= body.len() {
                    return Ok(None);
                }
                let remaining = body.len() - self.cursor;
                let frame_len = self.frame_bytes.min(remaining);
                if frame_len == 0 || frame_len % record_width != 0 {
                    return Err(AuraError::UnexpectedEof);
                }
                let start = self.cursor;
                let end = start + frame_len;
                self.cursor = end;
                &body[start..end]
            }
        };
        if frame.is_empty() || frame.len() % record_width != 0 {
            return Err(AuraError::UnexpectedEof);
        }
        let row_count = frame.len() / record_width;
        self.bytes_borrowed = self.bytes_borrowed.saturating_add(frame.len());
        Aura1FixedBatchView::new(frame, &self.compiled_plan, row_count).map(Some)
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

fn source_stats_from_reader(
    stats: AuraReaderStats,
    source_kind: &'static str,
    buffer_reuse_enabled: bool,
) -> AuraEventSourceStats {
    AuraEventSourceStats {
        source_kind,
        buffer_reuse_enabled,
        bytes_copied: stats.full_file_bytes_copied,
        bytes_borrowed_or_ranged: stats.bytes_read_during_replay,
        allocations_proxy: stats.temp_row_buffers_allocated,
        rows_materialized: stats.max_rows_materialized_at_once,
        full_file_materialized: stats.full_file_materialized,
        full_file_bytes_copied: stats.full_file_bytes_copied,
    }
}
