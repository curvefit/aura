//! Seekable writer for the frozen flat Aura0 V3 container.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use sha2::Digest;

use crate::format::{AuraContainerVersion, SEAL_MAGIC};
use crate::v3_container::{
    accumulate_stats, decode_v3_flat_footer_with_limits, encode_v3_flat_footer, plain_sha256,
    primary_sequence_slot, primary_timestamp_slot, v3_flat_body_sha256_hasher,
    v3_flat_header_sha256, validate_chunk_bounds, validate_flat_schema, zero_stats,
    V3FlatChunkDescriptor, V3FlatColumnStats, V3FlatFooter, V3FlatLimits,
};
use crate::v3_values::{
    canonical_v3_batch_sha256, canonical_v3_schema_fingerprint, decode_v3_value_block,
    encode_v3_value_block, AuraV3Batch, CanonicalV3RowHasher,
};
use crate::{AuraError, AuraHeader, AuraV3ValueRef, Profile, Result, SchemaDescriptor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V3FlatWriterState {
    Open,
    Sealing,
    Finished,
    Poisoned,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct V3FlatWriterOptions {
    pub limits: V3FlatLimits,
    pub comment: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3FlatWriteSummary {
    pub record_count: u64,
    pub chunk_count: u32,
    pub header_bytes: u64,
    pub body_bytes: u64,
    pub footer_bytes: u32,
    pub file_bytes: u64,
    pub schema_fingerprint: [u8; 32],
    pub global_logical_sha256: [u8; 32],
}

pub struct V3FlatAura0Writer<W: Read + Write + Seek> {
    inner: W,
    schema: SchemaDescriptor,
    limits: V3FlatLimits,
    header_bytes: Vec<u8>,
    state: V3FlatWriterState,
    record_count: u64,
    body_len: u64,
    chunks: Vec<V3FlatChunkDescriptor>,
    ingest_stats: Vec<V3FlatColumnStats>,
}

pub type AuraV3FlatWriter<W> = V3FlatAura0Writer<W>;

impl<W: Read + Write + Seek> V3FlatAura0Writer<W> {
    pub fn try_new(
        mut inner: W,
        schema: SchemaDescriptor,
        options: V3FlatWriterOptions,
    ) -> Result<Self> {
        let limits = options.limits.effective();
        validate_flat_schema(&schema)?;
        if inner
            .seek(SeekFrom::End(0))
            .map_err(|_| AuraError::InvalidValue("v3 writer io"))?
            != 0
        {
            return Err(AuraError::InvalidValue("v3 writer output length"));
        }
        inner
            .seek(SeekFrom::Start(0))
            .map_err(|_| AuraError::InvalidValue("v3 writer io"))?;
        let header = AuraHeader::new(Profile::Aura0)
            .with_container_version(AuraContainerVersion::V3)
            .with_schema_mapping(
                schema
                    .compact_schema_map
                    .clone()
                    .ok_or(AuraError::InvalidValue("v3 flat schema map"))?,
            )?
            .with_comment(options.comment)?;
        let header_bytes = header.encode()?;
        inner
            .write_all(&header_bytes)
            .map_err(|_| AuraError::InvalidValue("v3 writer io"))?;
        Ok(Self {
            inner,
            ingest_stats: zero_stats(&schema),
            schema,
            limits,
            header_bytes,
            state: V3FlatWriterState::Open,
            record_count: 0,
            body_len: 0,
            chunks: Vec::new(),
        })
    }

    pub const fn state(&self) -> V3FlatWriterState {
        self.state
    }

    pub const fn record_count(&self) -> u64 {
        self.record_count
    }

    pub const fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn write_batch(&mut self, batch: &AuraV3Batch) -> Result<()> {
        self.require_open()?;
        if batch.row_count == 0 {
            return Err(AuraError::InvalidValue("v3 writer empty batch"));
        }
        let next_rows = self
            .record_count
            .checked_add(u64::from(batch.row_count))
            .filter(|rows| *rows <= self.limits.max_rows)
            .ok_or(AuraError::InvalidValue("v3 flat record count"))?;
        if self.chunks.len() >= self.limits.max_chunks {
            return Err(AuraError::InvalidValue("v3 flat chunk count"));
        }
        let block = encode_v3_value_block(&self.schema, batch, self.limits.value_limits)?;
        let next_body = self
            .body_len
            .checked_add(block.len() as u64)
            .filter(|bytes| *bytes <= self.limits.max_body_bytes)
            .ok_or(AuraError::InvalidValue("v3 flat body length"))?;
        let chunk = build_chunk(
            &self.schema,
            batch,
            self.chunks.len() as u32,
            self.record_count,
            self.body_len,
            &block,
            self.limits,
        )?;
        let mut next_stats = self.ingest_stats.clone();
        accumulate_stats(&mut next_stats, batch)?;
        if self.inner.write_all(&block).is_err() {
            self.state = V3FlatWriterState::Poisoned;
            return Err(AuraError::InvalidValue("v3 writer io"));
        }
        self.record_count = next_rows;
        self.body_len = next_body;
        self.chunks.push(chunk);
        self.ingest_stats = next_stats;
        Ok(())
    }

    pub fn finish(mut self) -> Result<(W, V3FlatWriteSummary)> {
        self.require_open()?;
        self.state = V3FlatWriterState::Sealing;
        let result = self.finish_inner();
        if result.is_err() {
            self.state = V3FlatWriterState::Poisoned;
        }
        let summary = result?;
        self.state = V3FlatWriterState::Finished;
        Ok((self.inner, summary))
    }

    fn finish_inner(&mut self) -> Result<V3FlatWriteSummary> {
        self.io_flush()?;
        let body_len = usize::try_from(self.body_len)
            .map_err(|_| AuraError::InvalidValue("v3 writer output length"))?;
        let expected_end =
            self.header_bytes
                .len()
                .checked_add(body_len)
                .ok_or(AuraError::InvalidValue("v3 writer output length"))? as u64;
        if self.io_seek(SeekFrom::End(0))? != expected_end {
            return Err(AuraError::InvalidValue("v3 writer output length"));
        }
        let total_rows = u32::try_from(self.record_count)
            .map_err(|_| AuraError::InvalidValue("v3 flat record count"))?;
        let mut global =
            CanonicalV3RowHasher::new(&self.schema, total_rows, self.limits.value_limits)?;
        let mut stats = zero_stats(&self.schema);
        let mut chunks = Vec::new();
        chunks
            .try_reserve_exact(self.chunks.len())
            .map_err(|_| AuraError::InvalidValue("v3 writer allocation"))?;
        let mut body_hasher = v3_flat_body_sha256_hasher(self.body_len);
        for index in 0..self.chunks.len() {
            let expected = self.chunks[index].clone();
            let absolute = self.header_bytes.len() as u64 + expected.body_relative_offset;
            self.io_seek(SeekFrom::Start(absolute))?;
            let stored_len = usize::try_from(expected.stored_len)
                .map_err(|_| AuraError::InvalidValue("v3 writer allocation"))?;
            let mut block = Vec::new();
            block
                .try_reserve_exact(stored_len)
                .map_err(|_| AuraError::InvalidValue("v3 writer allocation"))?;
            block.resize(stored_len, 0);
            self.io_read_exact(&mut block)?;
            body_hasher.update(&block);
            let batch = decode_v3_value_block(&self.schema, &block, self.limits.value_limits)?;
            let actual = build_chunk(
                &self.schema,
                &batch,
                expected.chunk_id,
                expected.first_global_row,
                expected.body_relative_offset,
                &block,
                self.limits,
            )?;
            if actual != expected {
                return Err(AuraError::InvalidValue("v3 writer second pass"));
            }
            accumulate_stats(&mut stats, &batch)?;
            global.update_batch(&self.schema, &batch)?;
            chunks.push(actual);
        }
        if stats != self.ingest_stats {
            return Err(AuraError::InvalidValue("v3 writer second pass stats"));
        }
        let schema_fingerprint = canonical_v3_schema_fingerprint(&self.schema)?;
        let global_logical_sha256 = global.finalize()?;
        let footer = V3FlatFooter {
            record_count: self.record_count,
            body_len: self.body_len,
            schema: self.schema.clone(),
            primary_timestamp_slot: primary_timestamp_slot(
                self.schema
                    .compact_schema_map
                    .as_deref()
                    .unwrap_or_default(),
            ),
            primary_sequence_slot: primary_sequence_slot(&self.schema)?,
            schema_fingerprint,
            header_sha256: v3_flat_header_sha256(&self.header_bytes)?,
            body_sha256: body_hasher.finalize().into(),
            global_logical_sha256,
            stats,
            chunks,
        };
        let footer_bytes = encode_v3_flat_footer(&footer)?;
        if decode_v3_flat_footer_with_limits(&footer_bytes, self.limits)? != footer {
            return Err(AuraError::InvalidValue("v3 writer footer verification"));
        }
        let footer_len = u32::try_from(footer_bytes.len())
            .map_err(|_| AuraError::InvalidValue("v3 flat footer length"))?;
        self.io_seek(SeekFrom::Start(expected_end))?;
        self.io_write_all(&footer_bytes)?;
        self.io_write_all(&footer_len.to_le_bytes())?;
        self.io_write_all(SEAL_MAGIC)?;
        self.io_flush()?;
        let file_bytes = expected_end
            .checked_add(footer_bytes.len() as u64)
            .and_then(|bytes| bytes.checked_add(12))
            .ok_or(AuraError::InvalidValue("v3 writer output length"))?;
        Ok(V3FlatWriteSummary {
            record_count: self.record_count,
            chunk_count: footer.chunks.len() as u32,
            header_bytes: self.header_bytes.len() as u64,
            body_bytes: self.body_len,
            footer_bytes: footer_len,
            file_bytes,
            schema_fingerprint,
            global_logical_sha256,
        })
    }

    fn require_open(&self) -> Result<()> {
        if self.state == V3FlatWriterState::Open {
            Ok(())
        } else {
            Err(AuraError::InvalidValue("v3 writer state"))
        }
    }

    fn io_seek(&mut self, position: SeekFrom) -> Result<u64> {
        self.inner.seek(position).map_err(|_| {
            self.state = V3FlatWriterState::Poisoned;
            AuraError::InvalidValue("v3 writer io")
        })
    }

    fn io_read_exact(&mut self, bytes: &mut [u8]) -> Result<()> {
        self.inner.read_exact(bytes).map_err(|_| {
            self.state = V3FlatWriterState::Poisoned;
            AuraError::InvalidValue("v3 writer io")
        })
    }

    fn io_write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.inner.write_all(bytes).map_err(|_| {
            self.state = V3FlatWriterState::Poisoned;
            AuraError::InvalidValue("v3 writer io")
        })
    }

    fn io_flush(&mut self) -> Result<()> {
        self.inner.flush().map_err(|_| {
            self.state = V3FlatWriterState::Poisoned;
            AuraError::InvalidValue("v3 writer io")
        })
    }
}

impl V3FlatAura0Writer<File> {
    pub fn finish_and_sync(self) -> Result<(File, V3FlatWriteSummary)> {
        let (file, summary) = self.finish()?;
        file.sync_all()
            .map_err(|_| AuraError::InvalidValue("v3 writer sync"))?;
        Ok((file, summary))
    }
}

fn build_chunk(
    schema: &SchemaDescriptor,
    batch: &AuraV3Batch,
    chunk_id: u32,
    first_global_row: u64,
    body_relative_offset: u64,
    block: &[u8],
    limits: V3FlatLimits,
) -> Result<V3FlatChunkDescriptor> {
    let mut chunk = V3FlatChunkDescriptor {
        chunk_id,
        flags: 0,
        first_global_row,
        row_count: batch.row_count,
        body_relative_offset,
        stored_len: block.len() as u64,
        stored_sha256: plain_sha256(block),
        chunk_logical_sha256: canonical_v3_batch_sha256(schema, batch, limits.value_limits)?,
        first_timestamp: 0,
        last_timestamp: 0,
        first_sequence: 0,
        last_sequence: 0,
    };
    let primary_timestamp =
        primary_timestamp_slot(schema.compact_schema_map.as_deref().unwrap_or_default());
    if primary_timestamp != u16::MAX {
        let mut bounds = None;
        for row in 0..batch.row_count as usize {
            if let Some(value) = batch.columns[usize::from(primary_timestamp)].value_ref(row)? {
                let value = match value {
                    AuraV3ValueRef::TimestampMs(value)
                    | AuraV3ValueRef::TimestampNs(value)
                    | AuraV3ValueRef::I64(value) => value,
                    _ => return Err(AuraError::InvalidValue("v3 flat timestamp bounds")),
                };
                bounds.get_or_insert((value, value)).1 = value;
            }
        }
        if let Some((first, last)) = bounds {
            chunk.flags |= 1;
            chunk.first_timestamp = first;
            chunk.last_timestamp = last;
        }
    }
    if let Some(slot) = primary_sequence_slot(schema)? {
        let mut bounds = None;
        for row in 0..batch.row_count as usize {
            if let Some(AuraV3ValueRef::U64(value)) =
                batch.columns[usize::from(slot)].value_ref(row)?
            {
                bounds.get_or_insert((value, value)).1 = value;
            }
        }
        if let Some((first, last)) = bounds {
            chunk.flags |= 2;
            chunk.first_sequence = first;
            chunk.last_sequence = last;
        }
    }
    let footer = V3FlatFooter {
        record_count: u64::from(batch.row_count),
        body_len: block.len() as u64,
        schema: schema.clone(),
        primary_timestamp_slot: primary_timestamp,
        primary_sequence_slot: primary_sequence_slot(schema)?,
        schema_fingerprint: canonical_v3_schema_fingerprint(schema)?,
        header_sha256: [0; 32],
        body_sha256: [0; 32],
        global_logical_sha256: [0; 32],
        stats: zero_stats(schema),
        chunks: vec![V3FlatChunkDescriptor {
            chunk_id: 0,
            first_global_row: 0,
            body_relative_offset: 0,
            ..chunk.clone()
        }],
    };
    validate_chunk_bounds(&footer, &footer.chunks[0], batch)?;
    Ok(chunk)
}
