//! Seekable streaming writer for grouped Aura0 V3 exact-event containers.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use sha2::Digest;

use crate::format::{AuraContainerVersion, SEAL_MAGIC};
use crate::v3_container::{plain_sha256, primary_timestamp_slot, zero_stats};
use crate::v3_events::{
    canonical_v3_event_batch_sha256, decode_v3_event_block, encode_v3_event_block,
    validate_v3_grouped_exact_subset, AuraV3EventBatch, CanonicalV3EventHasher,
};
use crate::v3_grouped_container::{
    accumulate_grouped_stats, decode_v3_grouped_footer_with_limits, encode_v3_grouped_footer,
    grouped_primary_sequence_slot, v3_grouped_body_sha256_hasher, v3_grouped_header_sha256,
    validate_grouped_chunk_bounds, V3GroupedChunkDescriptor, V3GroupedColumnStats, V3GroupedFooter,
    V3GroupedLimits,
};
use crate::v3_values::canonical_v3_schema_fingerprint;
use crate::{AuraError, AuraHeader, AuraV3ValueRef, FieldScope, Profile, Result, SchemaDescriptor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V3GroupedWriterState {
    Open,
    Sealing,
    Finished,
    Poisoned,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct V3GroupedWriterOptions {
    pub limits: V3GroupedLimits,
    pub comment: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3GroupedWriteSummary {
    pub event_count: u64,
    pub child_count: u64,
    pub chunk_count: u32,
    pub header_bytes: u64,
    pub body_bytes: u64,
    pub footer_bytes: u32,
    pub file_bytes: u64,
    pub schema_fingerprint: [u8; 32],
    pub global_logical_sha256: [u8; 32],
}

pub struct V3GroupedAura0Writer<W: Read + Write + Seek> {
    inner: W,
    schema: SchemaDescriptor,
    limits: V3GroupedLimits,
    header_bytes: Vec<u8>,
    state: V3GroupedWriterState,
    event_count: u64,
    child_count: u64,
    body_len: u64,
    chunks: Vec<V3GroupedChunkDescriptor>,
    ingest_stats: Vec<V3GroupedColumnStats>,
}

pub type AuraV3GroupedWriter<W> = V3GroupedAura0Writer<W>;

impl<W: Read + Write + Seek> V3GroupedAura0Writer<W> {
    pub fn try_new(
        mut inner: W,
        schema: SchemaDescriptor,
        options: V3GroupedWriterOptions,
    ) -> Result<Self> {
        let limits = options.limits.effective();
        validate_v3_grouped_exact_subset(&schema)?;
        if inner
            .seek(SeekFrom::End(0))
            .map_err(|_| AuraError::InvalidValue("v3 grouped writer io"))?
            != 0
        {
            return Err(AuraError::InvalidValue("v3 grouped writer output length"));
        }
        inner
            .seek(SeekFrom::Start(0))
            .map_err(|_| AuraError::InvalidValue("v3 grouped writer io"))?;
        let header = AuraHeader::new(Profile::Aura0)
            .with_container_version(AuraContainerVersion::V3)
            .with_schema_mapping(
                schema
                    .compact_schema_map
                    .clone()
                    .ok_or(AuraError::InvalidValue("v3 grouped schema map"))?,
            )?
            .with_groups(schema.groups.clone())?
            .with_comment(options.comment)?;
        let header_bytes = header.encode()?;
        inner
            .write_all(&header_bytes)
            .map_err(|_| AuraError::InvalidValue("v3 grouped writer io"))?;
        Ok(Self {
            inner,
            ingest_stats: zero_stats(&schema),
            schema,
            limits,
            header_bytes,
            state: V3GroupedWriterState::Open,
            event_count: 0,
            child_count: 0,
            body_len: 0,
            chunks: Vec::new(),
        })
    }

    pub const fn state(&self) -> V3GroupedWriterState {
        self.state
    }

    pub const fn event_count(&self) -> u64 {
        self.event_count
    }

    pub const fn child_count(&self) -> u64 {
        self.child_count
    }

    pub const fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn write_batch(&mut self, batch: &AuraV3EventBatch) -> Result<()> {
        self.require_open()?;
        if batch.event_count == 0 {
            return Err(AuraError::InvalidValue("v3 grouped writer empty batch"));
        }
        let next_events = self
            .event_count
            .checked_add(u64::from(batch.event_count))
            .filter(|count| *count <= self.limits.max_events)
            .ok_or(AuraError::InvalidValue("v3 grouped event count"))?;
        let next_children = self
            .child_count
            .checked_add(u64::from(batch.child_count()))
            .filter(|count| *count <= self.limits.max_children)
            .ok_or(AuraError::InvalidValue("v3 grouped child count"))?;
        if self.chunks.len() >= self.limits.max_chunks {
            return Err(AuraError::InvalidValue("v3 grouped chunk count"));
        }
        let block = encode_v3_event_block(&self.schema, batch, self.limits.event_limits)?;
        let next_body = self
            .body_len
            .checked_add(block.len() as u64)
            .filter(|bytes| *bytes <= self.limits.max_body_bytes)
            .ok_or(AuraError::InvalidValue("v3 grouped body length"))?;
        let chunk = build_chunk(
            &self.schema,
            batch,
            self.chunks.len() as u32,
            self.event_count,
            self.child_count,
            self.body_len,
            &block,
            self.limits,
        )?;
        let mut next_stats = self.ingest_stats.clone();
        accumulate_grouped_stats(&self.schema, &mut next_stats, batch)?;
        if self.inner.write_all(&block).is_err() {
            self.state = V3GroupedWriterState::Poisoned;
            return Err(AuraError::InvalidValue("v3 grouped writer io"));
        }
        self.event_count = next_events;
        self.child_count = next_children;
        self.body_len = next_body;
        self.chunks.push(chunk);
        self.ingest_stats = next_stats;
        Ok(())
    }

    pub fn finish(mut self) -> Result<(W, V3GroupedWriteSummary)> {
        self.require_open()?;
        self.state = V3GroupedWriterState::Sealing;
        let result = self.finish_inner();
        if result.is_err() {
            self.state = V3GroupedWriterState::Poisoned;
        }
        let summary = result?;
        self.state = V3GroupedWriterState::Finished;
        Ok((self.inner, summary))
    }

    fn finish_inner(&mut self) -> Result<V3GroupedWriteSummary> {
        self.io_flush()?;
        let expected_end = (self.header_bytes.len() as u64)
            .checked_add(self.body_len)
            .ok_or(AuraError::InvalidValue("v3 grouped writer output length"))?;
        if self.io_seek(SeekFrom::End(0))? != expected_end {
            return Err(AuraError::InvalidValue("v3 grouped writer output length"));
        }
        let total_events = u32::try_from(self.event_count)
            .map_err(|_| AuraError::InvalidValue("v3 grouped event count"))?;
        let total_children = u32::try_from(self.child_count)
            .map_err(|_| AuraError::InvalidValue("v3 grouped child count"))?;
        let mut global = CanonicalV3EventHasher::new(
            &self.schema,
            total_events,
            total_children,
            self.limits.event_limits,
        )?;
        let mut stats = zero_stats(&self.schema);
        let mut chunks = Vec::new();
        chunks
            .try_reserve_exact(self.chunks.len())
            .map_err(|_| AuraError::InvalidValue("v3 grouped writer allocation"))?;
        let mut body_hasher = v3_grouped_body_sha256_hasher(self.body_len);
        for index in 0..self.chunks.len() {
            let expected = self.chunks[index].clone();
            let absolute = (self.header_bytes.len() as u64)
                .checked_add(expected.body_relative_offset)
                .ok_or(AuraError::InvalidValue("v3 grouped chunk range"))?;
            self.io_seek(SeekFrom::Start(absolute))?;
            let stored_len = usize::try_from(expected.stored_len)
                .map_err(|_| AuraError::InvalidValue("v3 grouped writer allocation"))?;
            let mut block = Vec::new();
            block
                .try_reserve_exact(stored_len)
                .map_err(|_| AuraError::InvalidValue("v3 grouped writer allocation"))?;
            block.resize(stored_len, 0);
            self.io_read_exact(&mut block)?;
            body_hasher.update(&block);
            let batch = decode_v3_event_block(&self.schema, &block, self.limits.event_limits)?;
            let actual = build_chunk(
                &self.schema,
                &batch,
                expected.chunk_id,
                expected.first_global_event,
                expected.first_global_child,
                expected.body_relative_offset,
                &block,
                self.limits,
            )?;
            if actual != expected {
                return Err(AuraError::InvalidValue("v3 grouped writer second pass"));
            }
            accumulate_grouped_stats(&self.schema, &mut stats, &batch)?;
            global.update_batch(&self.schema, &batch)?;
            chunks.push(actual);
        }
        if stats != self.ingest_stats {
            return Err(AuraError::InvalidValue(
                "v3 grouped writer second pass stats",
            ));
        }
        let schema_fingerprint = canonical_v3_schema_fingerprint(&self.schema)?;
        let global_logical_sha256 = global.finalize()?;
        let footer = V3GroupedFooter {
            event_count: self.event_count,
            child_count: self.child_count,
            body_len: self.body_len,
            schema: self.schema.clone(),
            primary_timestamp_slot: primary_timestamp_slot(
                self.schema
                    .compact_schema_map
                    .as_deref()
                    .unwrap_or_default(),
            ),
            primary_sequence_slot: grouped_primary_sequence_slot(&self.schema)?,
            schema_fingerprint,
            header_sha256: v3_grouped_header_sha256(&self.header_bytes)?,
            body_sha256: body_hasher.finalize().into(),
            global_logical_sha256,
            stats,
            chunks,
        };
        let footer_bytes = encode_v3_grouped_footer(&footer)?;
        if decode_v3_grouped_footer_with_limits(&footer_bytes, self.limits)? != footer {
            return Err(AuraError::InvalidValue(
                "v3 grouped writer footer verification",
            ));
        }
        let footer_len = u32::try_from(footer_bytes.len())
            .map_err(|_| AuraError::InvalidValue("v3 grouped footer length"))?;
        self.io_seek(SeekFrom::Start(expected_end))?;
        self.io_write_all(&footer_bytes)?;
        self.io_write_all(&footer_len.to_le_bytes())?;
        self.io_write_all(SEAL_MAGIC)?;
        self.io_flush()?;
        let file_bytes = expected_end
            .checked_add(footer_bytes.len() as u64)
            .and_then(|bytes| bytes.checked_add(12))
            .ok_or(AuraError::InvalidValue("v3 grouped writer output length"))?;
        Ok(V3GroupedWriteSummary {
            event_count: self.event_count,
            child_count: self.child_count,
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
        if self.state == V3GroupedWriterState::Open {
            Ok(())
        } else {
            Err(AuraError::InvalidValue("v3 grouped writer state"))
        }
    }

    fn io_seek(&mut self, position: SeekFrom) -> Result<u64> {
        self.inner.seek(position).map_err(|_| {
            self.state = V3GroupedWriterState::Poisoned;
            AuraError::InvalidValue("v3 grouped writer io")
        })
    }

    fn io_read_exact(&mut self, bytes: &mut [u8]) -> Result<()> {
        self.inner.read_exact(bytes).map_err(|_| {
            self.state = V3GroupedWriterState::Poisoned;
            AuraError::InvalidValue("v3 grouped writer io")
        })
    }

    fn io_write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.inner.write_all(bytes).map_err(|_| {
            self.state = V3GroupedWriterState::Poisoned;
            AuraError::InvalidValue("v3 grouped writer io")
        })
    }

    fn io_flush(&mut self) -> Result<()> {
        self.inner.flush().map_err(|_| {
            self.state = V3GroupedWriterState::Poisoned;
            AuraError::InvalidValue("v3 grouped writer io")
        })
    }
}

impl V3GroupedAura0Writer<File> {
    pub fn finish_and_sync(self) -> Result<(File, V3GroupedWriteSummary)> {
        let (file, summary) = self.finish()?;
        file.sync_all()
            .map_err(|_| AuraError::InvalidValue("v3 grouped writer sync"))?;
        Ok((file, summary))
    }
}

#[allow(clippy::too_many_arguments)]
fn build_chunk(
    schema: &SchemaDescriptor,
    batch: &AuraV3EventBatch,
    chunk_id: u32,
    first_global_event: u64,
    first_global_child: u64,
    body_relative_offset: u64,
    block: &[u8],
    limits: V3GroupedLimits,
) -> Result<V3GroupedChunkDescriptor> {
    let mut chunk = V3GroupedChunkDescriptor {
        chunk_id,
        flags: 0,
        first_global_event,
        event_count: batch.event_count,
        first_global_child,
        child_count: batch.child_count(),
        body_relative_offset,
        stored_len: block.len() as u64,
        stored_sha256: plain_sha256(block),
        chunk_logical_sha256: canonical_v3_event_batch_sha256(schema, batch, limits.event_limits)?,
        first_timestamp: 0,
        last_timestamp: 0,
        first_sequence: 0,
        last_sequence: 0,
    };
    let primary_timestamp =
        primary_timestamp_slot(schema.compact_schema_map.as_deref().unwrap_or_default());
    if primary_timestamp != u16::MAX {
        let column = event_column(schema, batch, primary_timestamp)?;
        let mut bounds = None;
        for row in 0..batch.event_count as usize {
            if let Some(value) = column.value_ref(row)? {
                let value = match value {
                    AuraV3ValueRef::TimestampMs(value)
                    | AuraV3ValueRef::TimestampNs(value)
                    | AuraV3ValueRef::I64(value) => value,
                    _ => return Err(AuraError::InvalidValue("v3 grouped timestamp bounds")),
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
    let primary_sequence = grouped_primary_sequence_slot(schema)?;
    if let Some(slot) = primary_sequence {
        let column = event_column(schema, batch, slot)?;
        let mut bounds = None;
        for row in 0..batch.event_count as usize {
            if let Some(AuraV3ValueRef::U64(value)) = column.value_ref(row)? {
                bounds.get_or_insert((value, value)).1 = value;
            }
        }
        if let Some((first, last)) = bounds {
            chunk.flags |= 2;
            chunk.first_sequence = first;
            chunk.last_sequence = last;
        }
    }
    let footer = V3GroupedFooter {
        event_count: u64::from(batch.event_count),
        child_count: u64::from(batch.child_count()),
        body_len: block.len() as u64,
        schema: schema.clone(),
        primary_timestamp_slot: primary_timestamp,
        primary_sequence_slot: primary_sequence,
        schema_fingerprint: canonical_v3_schema_fingerprint(schema)?,
        header_sha256: [0; 32],
        body_sha256: [0; 32],
        global_logical_sha256: [0; 32],
        stats: zero_stats(schema),
        chunks: vec![V3GroupedChunkDescriptor {
            chunk_id: 0,
            first_global_event: 0,
            first_global_child: 0,
            body_relative_offset: 0,
            ..chunk.clone()
        }],
    };
    validate_grouped_chunk_bounds(&footer, &footer.chunks[0], batch)?;
    Ok(chunk)
}

fn event_column<'a>(
    schema: &SchemaDescriptor,
    batch: &'a AuraV3EventBatch,
    slot: u16,
) -> Result<&'a crate::AuraV3Column> {
    schema
        .fields
        .iter()
        .filter(|field| field.scope == FieldScope::Event)
        .position(|field| field.index == slot)
        .and_then(|position| batch.event_columns.get(position))
        .ok_or(AuraError::InvalidValue("v3 grouped event column"))
}
