//! Bounded seekable reader for grouped Aura0 V3 exact-event containers.
//!
//! A generic `Read + Seek` source cannot be locked by this crate. Callers must
//! provide an immutable snapshot for each open/verify/read operation. Envelope
//! checks around a second full-body hash detect mutations observed during the
//! passes without claiming categorical concurrent-mutation exclusion.

use std::io::{Read, Seek, SeekFrom};

use sha2::Digest;

use crate::format::SEAL_MAGIC;
use crate::v3_container::zero_stats;
use crate::v3_events::{AuraV3EventBatch, CanonicalV3EventHasher};
use crate::v3_grouped_container::{
    accumulate_grouped_stats, decode_v3_grouped_footer_with_limits, v3_grouped_body_sha256_hasher,
    v3_grouped_header_sha256, validate_grouped_header_footer, verify_grouped_chunk,
    V3GroupedChunkDescriptor, V3GroupedFooter, V3GroupedLimits,
};
use crate::{AuraError, AuraHeader, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V3GroupedReaderState {
    Opened,
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3GroupedVerifySummary {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3GroupedReadAll {
    pub summary: V3GroupedVerifySummary,
    pub batches: Vec<AuraV3EventBatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3GroupedChunkRead {
    pub descriptor: V3GroupedChunkDescriptor,
    pub batch: AuraV3EventBatch,
}

pub struct V3GroupedAura0Reader<R: Read + Seek> {
    inner: R,
    limits: V3GroupedLimits,
    header: AuraHeader,
    footer: V3GroupedFooter,
    header_len: u64,
    body_start: u64,
    body_end: u64,
    footer_len: u32,
    file_len_at_open: u64,
    state: V3GroupedReaderState,
}

pub type AuraV3GroupedReader<R> = V3GroupedAura0Reader<R>;

impl<R: Read + Seek> V3GroupedAura0Reader<R> {
    pub fn open(inner: R) -> Result<Self> {
        Self::open_with_limits(inner, V3GroupedLimits::default())
    }

    pub fn open_with_limits(mut inner: R, limits: V3GroupedLimits) -> Result<Self> {
        let limits = limits.effective();
        let file_len = seek(&mut inner, SeekFrom::End(0))?;
        if file_len < 12 + crate::V3_HEADER_PREFIX_SIZE as u64 {
            return Err(AuraError::UnexpectedEof);
        }
        seek(&mut inner, SeekFrom::End(-12))?;
        let mut trailer = [0u8; 12];
        read_exact(&mut inner, &mut trailer)?;
        if &trailer[4..] != SEAL_MAGIC {
            return Err(AuraError::InvalidMagic {
                expected: "sealed:)",
            });
        }
        let footer_len = u32::from_le_bytes(trailer[..4].try_into().unwrap()) as u64;
        let footer_len_usize = usize::try_from(footer_len)
            .map_err(|_| AuraError::InvalidValue("v3 grouped footer length"))?;
        if footer_len_usize > limits.max_footer_bytes {
            return Err(AuraError::InvalidValue("v3 grouped footer length"));
        }
        let footer_start = file_len
            .checked_sub(12)
            .and_then(|offset| offset.checked_sub(footer_len))
            .ok_or(AuraError::InvalidValue("v3 grouped footer length"))?;
        let mut footer_bytes = Vec::new();
        footer_bytes
            .try_reserve_exact(footer_len_usize)
            .map_err(|_| AuraError::InvalidValue("v3 grouped reader allocation"))?;
        footer_bytes.resize(footer_len_usize, 0);
        seek(&mut inner, SeekFrom::Start(footer_start))?;
        read_exact(&mut inner, &mut footer_bytes)?;
        let footer = decode_v3_grouped_footer_with_limits(&footer_bytes, limits)?;
        seek(&mut inner, SeekFrom::Start(0))?;
        let mut prefix = [0u8; 11];
        read_exact(&mut inner, &mut prefix)?;
        let header_len = AuraHeader::encoded_len(&prefix)? as u64;
        let header_len_usize = usize::try_from(header_len)
            .map_err(|_| AuraError::InvalidValue("v3 grouped header length"))?;
        if header_len > footer_start || header_len > crate::MAX_V3_HEADER_BYTES as u64 {
            return Err(AuraError::InvalidValue("v3 grouped file ranges"));
        }
        let mut header_bytes = Vec::new();
        header_bytes
            .try_reserve_exact(header_len_usize)
            .map_err(|_| AuraError::InvalidValue("v3 grouped reader allocation"))?;
        header_bytes.resize(header_len_usize, 0);
        seek(&mut inner, SeekFrom::Start(0))?;
        read_exact(&mut inner, &mut header_bytes)?;
        let header = AuraHeader::decode(&header_bytes)?;
        validate_grouped_header_footer(&header, &footer)?;
        if v3_grouped_header_sha256(&header_bytes)? != footer.header_sha256 {
            return Err(AuraError::InvalidValue("v3 grouped header hash"));
        }
        let body_end = footer_start;
        if body_end.checked_sub(header_len) != Some(footer.body_len) {
            return Err(AuraError::InvalidValue("v3 grouped body length"));
        }
        Ok(Self {
            inner,
            limits,
            header,
            footer,
            header_len,
            body_start: header_len,
            body_end,
            footer_len: footer_len as u32,
            file_len_at_open: file_len,
            state: V3GroupedReaderState::Opened,
        })
    }

    pub const fn state(&self) -> V3GroupedReaderState {
        self.state
    }

    pub const fn is_fully_verified(&self) -> bool {
        matches!(self.state, V3GroupedReaderState::Verified)
    }

    /// Metadata is structurally checked at open; body-dependent claims remain
    /// provisional until a complete verification succeeds.
    pub const fn embedded_footer(&self) -> &V3GroupedFooter {
        &self.footer
    }

    pub const fn header(&self) -> &AuraHeader {
        &self.header
    }

    pub const fn body_range(&self) -> (u64, u64) {
        (self.body_start, self.body_end)
    }

    pub const fn header_len(&self) -> u64 {
        self.header_len
    }

    pub fn chunk_for_event(&self, global_event: u64) -> Option<&V3GroupedChunkDescriptor> {
        if global_event >= self.footer.event_count {
            return None;
        }
        let end = self
            .footer
            .chunks
            .partition_point(|chunk| chunk.first_global_event <= global_event);
        end.checked_sub(1).and_then(|index| {
            let chunk = &self.footer.chunks[index];
            (global_event < chunk.first_global_event + u64::from(chunk.event_count))
                .then_some(chunk)
        })
    }

    pub fn chunk_for_child(&self, global_child: u64) -> Option<&V3GroupedChunkDescriptor> {
        if global_child >= self.footer.child_count {
            return None;
        }
        let index = self.footer.chunks.partition_point(|chunk| {
            chunk.first_global_child + u64::from(chunk.child_count) <= global_child
        });
        self.footer.chunks.get(index).filter(|chunk| {
            chunk.first_global_child <= global_child
                && global_child < chunk.first_global_child + u64::from(chunk.child_count)
        })
    }

    /// Read and verify one chunk. This does not make a whole-file verification
    /// claim and does not change [`Self::state`].
    pub fn read_chunk(&mut self, chunk_id: u32) -> Result<V3GroupedChunkRead> {
        let descriptor = self
            .footer
            .chunks
            .get(chunk_id as usize)
            .filter(|chunk| chunk.chunk_id == chunk_id)
            .cloned()
            .ok_or(AuraError::InvalidValue("v3 grouped chunk id"))?;
        let block = self.read_stored_chunk(&descriptor)?;
        let batch =
            verify_grouped_chunk(&self.footer, &descriptor, &block, self.limits.event_limits)?;
        Ok(V3GroupedChunkRead { descriptor, batch })
    }

    pub fn verify_all(&mut self) -> Result<V3GroupedVerifySummary> {
        self.state = V3GroupedReaderState::Opened;
        let summary = self.verification_pass(|_| Ok(()))?;
        self.commit_verification()?;
        Ok(summary)
    }

    pub fn read_all(&mut self) -> Result<V3GroupedReadAll> {
        let mut batches = Vec::new();
        batches
            .try_reserve_exact(self.footer.chunks.len())
            .map_err(|_| AuraError::InvalidValue("v3 grouped reader allocation"))?;
        self.state = V3GroupedReaderState::Opened;
        let summary = self.verification_pass(|batch| {
            batches.push(batch.clone());
            Ok(())
        })?;
        self.commit_verification()?;
        Ok(V3GroupedReadAll { summary, batches })
    }

    /// Verify completely before replaying checked batches to the callback.
    pub fn verify_with(
        &mut self,
        mut callback: impl FnMut(&AuraV3EventBatch) -> Result<()>,
    ) -> Result<V3GroupedVerifySummary> {
        let summary = self.verify_all()?;
        if let Err(error) = self.verification_pass(|batch| callback(batch)) {
            self.state = V3GroupedReaderState::Opened;
            return Err(error);
        }
        if let Err(error) = self.commit_verification() {
            self.state = V3GroupedReaderState::Opened;
            return Err(error);
        }
        Ok(summary)
    }

    fn verification_pass(
        &mut self,
        mut callback: impl FnMut(&AuraV3EventBatch) -> Result<()>,
    ) -> Result<V3GroupedVerifySummary> {
        let total_events = u32::try_from(self.footer.event_count)
            .map_err(|_| AuraError::InvalidValue("v3 grouped event count"))?;
        let total_children = u32::try_from(self.footer.child_count)
            .map_err(|_| AuraError::InvalidValue("v3 grouped child count"))?;
        let mut global = CanonicalV3EventHasher::new(
            &self.footer.schema,
            total_events,
            total_children,
            self.limits.event_limits,
        )?;
        let mut stats = zero_stats(&self.footer.schema);
        let mut body_hasher = v3_grouped_body_sha256_hasher(self.footer.body_len);
        for index in 0..self.footer.chunks.len() {
            let chunk = self.footer.chunks[index].clone();
            let block = self.read_stored_chunk(&chunk)?;
            body_hasher.update(&block);
            let batch =
                verify_grouped_chunk(&self.footer, &chunk, &block, self.limits.event_limits)?;
            accumulate_grouped_stats(&self.footer.schema, &mut stats, &batch)?;
            global.update_batch(&self.footer.schema, &batch)?;
            callback(&batch)?;
        }
        let body_hash: [u8; 32] = body_hasher.finalize().into();
        if body_hash != self.footer.body_sha256 {
            return Err(AuraError::InvalidValue("v3 grouped body hash"));
        }
        if stats != self.footer.stats {
            return Err(AuraError::InvalidValue("v3 grouped stats"));
        }
        if global.finalize()? != self.footer.global_logical_sha256 {
            return Err(AuraError::InvalidValue("v3 grouped global logical hash"));
        }
        Ok(V3GroupedVerifySummary {
            event_count: self.footer.event_count,
            child_count: self.footer.child_count,
            chunk_count: self.footer.chunks.len() as u32,
            header_bytes: self.header_len,
            body_bytes: self.footer.body_len,
            footer_bytes: self.footer_len,
            file_bytes: self.file_len_at_open,
            schema_fingerprint: self.footer.schema_fingerprint,
            global_logical_sha256: self.footer.global_logical_sha256,
        })
    }

    fn read_stored_chunk(&mut self, chunk: &V3GroupedChunkDescriptor) -> Result<Vec<u8>> {
        let absolute = self
            .body_start
            .checked_add(chunk.body_relative_offset)
            .ok_or(AuraError::InvalidValue("v3 grouped chunk range"))?;
        seek(&mut self.inner, SeekFrom::Start(absolute))?;
        let stored_len = usize::try_from(chunk.stored_len)
            .map_err(|_| AuraError::InvalidValue("v3 grouped chunk range"))?;
        let mut block = Vec::new();
        block
            .try_reserve_exact(stored_len)
            .map_err(|_| AuraError::InvalidValue("v3 grouped reader allocation"))?;
        block.resize(stored_len, 0);
        read_exact(&mut self.inner, &mut block)?;
        Ok(block)
    }

    fn recheck_envelope(&mut self) -> Result<()> {
        if seek(&mut self.inner, SeekFrom::End(0))? != self.file_len_at_open {
            return Err(AuraError::InvalidValue(
                "v3 grouped reader file length changed",
            ));
        }
        seek(&mut self.inner, SeekFrom::End(-12))?;
        let mut trailer = [0u8; 12];
        read_exact(&mut self.inner, &mut trailer)?;
        if u32::from_le_bytes(trailer[..4].try_into().unwrap()) != self.footer_len
            || &trailer[4..] != SEAL_MAGIC
        {
            return Err(AuraError::InvalidValue(
                "v3 grouped reader envelope changed",
            ));
        }
        let footer_len = usize::try_from(self.footer_len)
            .map_err(|_| AuraError::InvalidValue("v3 grouped footer length"))?;
        let mut footer_bytes = Vec::new();
        footer_bytes
            .try_reserve_exact(footer_len)
            .map_err(|_| AuraError::InvalidValue("v3 grouped reader allocation"))?;
        footer_bytes.resize(footer_len, 0);
        seek(&mut self.inner, SeekFrom::Start(self.body_end))?;
        read_exact(&mut self.inner, &mut footer_bytes)?;
        if decode_v3_grouped_footer_with_limits(&footer_bytes, self.limits)? != self.footer {
            return Err(AuraError::InvalidValue(
                "v3 grouped reader envelope changed",
            ));
        }
        let header_len = usize::try_from(self.header_len)
            .map_err(|_| AuraError::InvalidValue("v3 grouped header length"))?;
        let mut header_bytes = Vec::new();
        header_bytes
            .try_reserve_exact(header_len)
            .map_err(|_| AuraError::InvalidValue("v3 grouped reader allocation"))?;
        header_bytes.resize(header_len, 0);
        seek(&mut self.inner, SeekFrom::Start(0))?;
        read_exact(&mut self.inner, &mut header_bytes)?;
        if AuraHeader::decode(&header_bytes)? != self.header
            || v3_grouped_header_sha256(&header_bytes)? != self.footer.header_sha256
        {
            return Err(AuraError::InvalidValue(
                "v3 grouped reader envelope changed",
            ));
        }
        Ok(())
    }

    fn commit_verification(&mut self) -> Result<()> {
        self.recheck_envelope()?;
        self.rehash_body()?;
        self.recheck_envelope()?;
        self.state = V3GroupedReaderState::Verified;
        Ok(())
    }

    fn rehash_body(&mut self) -> Result<()> {
        seek(&mut self.inner, SeekFrom::Start(self.body_start))?;
        let mut remaining = self.footer.body_len;
        let mut hasher = v3_grouped_body_sha256_hasher(self.footer.body_len);
        let mut scratch = [0u8; 64 * 1024];
        while remaining != 0 {
            let request = usize::try_from(remaining.min(scratch.len() as u64))
                .map_err(|_| AuraError::InvalidValue("v3 grouped body length"))?;
            read_exact(&mut self.inner, &mut scratch[..request])?;
            hasher.update(&scratch[..request]);
            remaining -= request as u64;
        }
        let hash: [u8; 32] = hasher.finalize().into();
        if hash != self.footer.body_sha256 {
            return Err(AuraError::InvalidValue("v3 grouped body hash"));
        }
        Ok(())
    }

    pub fn into_inner(self) -> R {
        self.inner
    }
}

fn seek(stream: &mut impl Seek, position: SeekFrom) -> Result<u64> {
    stream
        .seek(position)
        .map_err(|_| AuraError::InvalidValue("v3 grouped reader io"))
}

fn read_exact(stream: &mut impl Read, bytes: &mut [u8]) -> Result<()> {
    stream
        .read_exact(bytes)
        .map_err(|_| AuraError::InvalidValue("v3 grouped reader io"))
}
