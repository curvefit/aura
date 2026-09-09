//! Held-file ingest writer for the planned-grouped Aura0 V3 development route.
//!
//! The open file is deliberately an incomplete canonical V3 header followed by
//! exact event blocks. Seal rereads and verifies those blocks, invokes the
//! finite registry-1..5 planner, then replaces the held bytes with the selected
//! complete artifact. No logical batches are retained between writes.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::format::AuraContainerVersion;
use crate::v3_container::zero_stats;
use crate::v3_events::{
    canonical_v3_event_batch_sha256, decode_v3_event_block, encode_v3_event_block,
    validate_v3_event_batch, validate_v3_grouped_exact_subset, AuraV3EventBatch,
};
use crate::v3_grouped_container::{
    accumulate_grouped_stats, V3GroupedColumnStats, V3GroupedLimits,
};
use crate::v3_planned_grouped::{
    decode_v3_planned_grouped, GroupedSearch, V3PlannedGroupedArtifact, V3PlannedGroupedInspection,
    V3PlannedGroupedSummary,
};
use crate::{AuraError, AuraHeader, Profile, Result, SchemaDescriptor};

pub const MAX_V3_PLANNED_GROUPED_SCRATCH_BYTES: u64 = u32::MAX as u64;
pub const DEFAULT_V3_PLANNED_GROUPED_SCRATCH_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V3PlannedGroupedWriterState {
    Open,
    Sealing,
    Finished,
    Poisoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V3PlannedGroupedWriterOptions {
    pub limits: V3GroupedLimits,
    pub max_scratch_bytes: u64,
    pub max_output_bytes: u64,
}

impl Default for V3PlannedGroupedWriterOptions {
    fn default() -> Self {
        Self {
            limits: V3GroupedLimits::DEFAULT_IN_MEMORY,
            max_scratch_bytes: DEFAULT_V3_PLANNED_GROUPED_SCRATCH_BYTES,
            max_output_bytes: DEFAULT_V3_PLANNED_GROUPED_SCRATCH_BYTES,
        }
    }
}

impl V3PlannedGroupedWriterOptions {
    fn effective(self) -> Self {
        Self {
            limits: self.limits.effective(),
            max_scratch_bytes: self
                .max_scratch_bytes
                .min(MAX_V3_PLANNED_GROUPED_SCRATCH_BYTES),
            max_output_bytes: self
                .max_output_bytes
                .min(MAX_V3_PLANNED_GROUPED_SCRATCH_BYTES),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScratchChunk {
    event_count: u32,
    child_count: u32,
    body_offset: u64,
    stored_len: u64,
    stored_sha256: [u8; 32],
    logical_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3PlannedGroupedWriteReceipt {
    pub summary: V3PlannedGroupedSummary,
    pub inspection: V3PlannedGroupedInspection,
    pub artifact_sha256: [u8; 32],
    pub stale_temp_cleanup_required: bool,
}

pub struct V3PlannedGroupedIngestWriter<W: Read + Write + Seek> {
    inner: W,
    schema: SchemaDescriptor,
    options: V3PlannedGroupedWriterOptions,
    header_bytes: Vec<u8>,
    chunks: Vec<ScratchChunk>,
    stats: Vec<V3GroupedColumnStats>,
    event_count: u64,
    child_count: u64,
    body_len: u64,
    state: V3PlannedGroupedWriterState,
}

impl<W: Read + Write + Seek> V3PlannedGroupedIngestWriter<W> {
    pub fn try_new(
        mut inner: W,
        schema: SchemaDescriptor,
        options: V3PlannedGroupedWriterOptions,
    ) -> Result<Self> {
        let options = options.effective();
        validate_v3_grouped_exact_subset(&schema)?;
        if options.max_scratch_bytes == 0 || options.max_output_bytes == 0 {
            return Err(AuraError::InvalidValue("v3 planned writer byte limit"));
        }
        if inner
            .seek(SeekFrom::End(0))
            .map_err(|_| AuraError::InvalidValue("v3 planned writer io"))?
            != 0
        {
            return Err(AuraError::InvalidValue("v3 planned writer scratch length"));
        }
        let header = AuraHeader::new(Profile::Aura0)
            .with_container_version(AuraContainerVersion::V3)
            .with_schema_mapping(
                schema
                    .compact_schema_map
                    .clone()
                    .ok_or(AuraError::InvalidValue("v3 planned writer schema map"))?,
            )?
            .with_groups(schema.groups.clone())?;
        let header_bytes = header.encode()?;
        if header_bytes.len() as u64 > options.max_scratch_bytes {
            return Err(AuraError::InvalidValue("v3 planned writer scratch length"));
        }
        inner
            .seek(SeekFrom::Start(0))
            .and_then(|_| inner.write_all(&header_bytes))
            .map_err(|_| AuraError::InvalidValue("v3 planned writer io"))?;
        Ok(Self {
            inner,
            stats: zero_stats(&schema),
            schema,
            options,
            header_bytes,
            chunks: Vec::new(),
            event_count: 0,
            child_count: 0,
            body_len: 0,
            state: V3PlannedGroupedWriterState::Open,
        })
    }

    pub const fn state(&self) -> V3PlannedGroupedWriterState {
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
        validate_v3_event_batch(&self.schema, batch, self.options.limits.event_limits)?;
        if batch.event_count == 0 {
            return Err(AuraError::InvalidValue("v3 planned writer empty batch"));
        }
        if self.chunks.len() >= self.options.limits.max_chunks {
            return Err(AuraError::InvalidValue("v3 planned writer chunk count"));
        }
        let next_events = self
            .event_count
            .checked_add(u64::from(batch.event_count))
            .filter(|value| *value <= self.options.limits.max_events)
            .ok_or(AuraError::InvalidValue("v3 planned writer event count"))?;
        let next_children = self
            .child_count
            .checked_add(u64::from(batch.child_count()))
            .filter(|value| *value <= self.options.limits.max_children)
            .ok_or(AuraError::InvalidValue("v3 planned writer child count"))?;
        let block = encode_v3_event_block(&self.schema, batch, self.options.limits.event_limits)?;
        let next_body = self
            .body_len
            .checked_add(block.len() as u64)
            .filter(|value| *value <= self.options.limits.max_body_bytes)
            .ok_or(AuraError::InvalidValue("v3 planned writer body length"))?;
        let scratch_end = (self.header_bytes.len() as u64)
            .checked_add(next_body)
            .filter(|value| *value <= self.options.max_scratch_bytes)
            .ok_or(AuraError::InvalidValue("v3 planned writer scratch length"))?;
        let mut next_stats = self.stats.clone();
        accumulate_grouped_stats(&self.schema, &mut next_stats, batch)?;
        let chunk = ScratchChunk {
            event_count: batch.event_count,
            child_count: batch.child_count(),
            body_offset: self.body_len,
            stored_len: block.len() as u64,
            stored_sha256: Sha256::digest(&block).into(),
            logical_sha256: canonical_v3_event_batch_sha256(
                &self.schema,
                batch,
                self.options.limits.event_limits,
            )?,
        };
        if self.inner.write_all(&block).is_err() {
            self.state = V3PlannedGroupedWriterState::Poisoned;
            return Err(AuraError::InvalidValue("v3 planned writer io"));
        }
        debug_assert_eq!(scratch_end, self.header_bytes.len() as u64 + next_body);
        self.event_count = next_events;
        self.child_count = next_children;
        self.body_len = next_body;
        self.stats = next_stats;
        self.chunks.push(chunk);
        Ok(())
    }

    pub fn finish(mut self) -> Result<(W, V3PlannedGroupedArtifact)> {
        self.require_open()?;
        self.state = V3PlannedGroupedWriterState::Sealing;
        let artifact = self.compile_from_scratch()?;
        self.state = V3PlannedGroupedWriterState::Finished;
        Ok((self.inner, artifact))
    }

    fn compile_from_scratch(&mut self) -> Result<V3PlannedGroupedArtifact> {
        self.io_flush()?;
        let expected_end = (self.header_bytes.len() as u64)
            .checked_add(self.body_len)
            .ok_or(AuraError::InvalidValue("v3 planned writer scratch length"))?;
        if self.io_seek(SeekFrom::End(0))? != expected_end {
            return Err(AuraError::InvalidValue("v3 planned writer scratch length"));
        }
        let mut batches = Vec::new();
        batches
            .try_reserve_exact(self.chunks.len())
            .map_err(|_| AuraError::InvalidValue("v3 planned writer allocation"))?;
        let mut reread_stats = zero_stats(&self.schema);
        for index in 0..self.chunks.len() {
            let chunk = self.chunks[index].clone();
            let absolute = (self.header_bytes.len() as u64)
                .checked_add(chunk.body_offset)
                .ok_or(AuraError::InvalidValue("v3 planned writer chunk range"))?;
            self.io_seek(SeekFrom::Start(absolute))?;
            let len = usize::try_from(chunk.stored_len)
                .map_err(|_| AuraError::InvalidValue("v3 planned writer allocation"))?;
            let mut block = Vec::new();
            block
                .try_reserve_exact(len)
                .map_err(|_| AuraError::InvalidValue("v3 planned writer allocation"))?;
            block.resize(len, 0);
            self.io_read_exact(&mut block)?;
            if Sha256::digest(&block).as_slice() != chunk.stored_sha256 {
                return Err(AuraError::InvalidValue("v3 planned writer stored hash"));
            }
            let batch =
                decode_v3_event_block(&self.schema, &block, self.options.limits.event_limits)?;
            if batch.event_count != chunk.event_count
                || batch.child_count() != chunk.child_count
                || canonical_v3_event_batch_sha256(
                    &self.schema,
                    &batch,
                    self.options.limits.event_limits,
                )? != chunk.logical_sha256
            {
                return Err(AuraError::InvalidValue("v3 planned writer second pass"));
            }
            accumulate_grouped_stats(&self.schema, &mut reread_stats, &batch)?;
            batches.push(batch);
        }
        if reread_stats != self.stats {
            return Err(AuraError::InvalidValue(
                "v3 planned writer second pass stats",
            ));
        }
        let artifact =
            GroupedSearch::CrossDomain.compile(&self.schema, &batches, self.options.limits)?;
        if artifact.summary.file_bytes > self.options.max_output_bytes {
            return Err(AuraError::InvalidValue("v3 planned writer output length"));
        }
        Ok(artifact)
    }

    fn require_open(&self) -> Result<()> {
        if self.state == V3PlannedGroupedWriterState::Open {
            Ok(())
        } else {
            Err(AuraError::InvalidValue("v3 planned writer state"))
        }
    }

    fn io_seek(&mut self, position: SeekFrom) -> Result<u64> {
        self.inner.seek(position).map_err(|_| {
            self.state = V3PlannedGroupedWriterState::Poisoned;
            AuraError::InvalidValue("v3 planned writer io")
        })
    }

    fn io_read_exact(&mut self, bytes: &mut [u8]) -> Result<()> {
        self.inner.read_exact(bytes).map_err(|_| {
            self.state = V3PlannedGroupedWriterState::Poisoned;
            AuraError::InvalidValue("v3 planned writer io")
        })
    }

    fn io_flush(&mut self) -> Result<()> {
        self.inner.flush().map_err(|_| {
            self.state = V3PlannedGroupedWriterState::Poisoned;
            AuraError::InvalidValue("v3 planned writer io")
        })
    }
}

impl V3PlannedGroupedIngestWriter<File> {
    pub fn finish_and_sync(self) -> Result<(File, V3PlannedGroupedWriteReceipt)> {
        self.finish_and_sync_with(|file| file.sync_all())
    }

    fn finish_and_sync_with(
        self,
        sync: impl FnOnce(&File) -> std::io::Result<()>,
    ) -> Result<(File, V3PlannedGroupedWriteReceipt)> {
        let (mut file, artifact) = self.finish()?;
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.write_all(&artifact.bytes))
            .and_then(|_| file.set_len(artifact.bytes.len() as u64))
            .and_then(|_| file.flush())
            .map_err(|_| AuraError::InvalidValue("v3 planned writer sync"))?;
        sync(&file).map_err(|_| AuraError::InvalidValue("v3 planned writer sync"))?;
        verify_held_file(&mut file, &artifact)?;
        let receipt = V3PlannedGroupedWriteReceipt {
            artifact_sha256: Sha256::digest(&artifact.bytes).into(),
            summary: artifact.summary,
            inspection: artifact.inspection,
            stale_temp_cleanup_required: false,
        };
        Ok((file, receipt))
    }
}

#[cfg(target_os = "linux")]
fn verify_expected_file(
    parent: &HeldParent,
    name: &std::ffi::OsStr,
    receipt: &V3PlannedGroupedWriteReceipt,
) -> Result<()> {
    let mut file = parent
        .open_regular(name)
        .map_err(|_| AuraError::InvalidValue("v3 planned writer adoption"))?;
    if file
        .metadata()
        .map_err(|_| AuraError::InvalidValue("v3 planned writer adoption"))?
        .len()
        != receipt.summary.file_bytes
    {
        return Err(AuraError::InvalidValue("v3 planned writer adoption"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| AuraError::InvalidValue("v3 planned writer adoption"))?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    if <[u8; 32]>::from(hash.finalize()) != receipt.artifact_sha256 {
        return Err(AuraError::InvalidValue("v3 planned writer adoption"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn remove_if_same(parent: &HeldParent, name: &std::ffi::OsStr, expected: &fs::Metadata) -> bool {
    remove_if_identity(parent, name, file_identity(expected))
}

#[cfg(target_os = "linux")]
fn file_identity(metadata: &fs::Metadata) -> HeldFileIdentity {
    use std::os::unix::fs::MetadataExt;
    HeldFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(target_os = "linux")]
fn remove_if_identity(
    parent: &HeldParent,
    name: &std::ffi::OsStr,
    expected: HeldFileIdentity,
) -> bool {
    parent
        .open_regular(name)
        .and_then(|file| file.metadata())
        .is_ok_and(|actual| {
            let actual = file_identity(&actual);
            actual.device == expected.device && actual.inode == expected.inode
        })
        && parent.remove(name).is_ok()
}

#[cfg(target_os = "linux")]
fn exact_recovery_temp(
    parent: &HeldParent,
    name: &std::ffi::OsStr,
    identity: HeldFileIdentity,
    path: &Path,
    receipt: &V3PlannedGroupedWriteReceipt,
) -> Option<PathBuf> {
    let matches = parent
        .open_regular(name)
        .and_then(|file| file.metadata())
        .is_ok_and(|metadata| {
            let actual = file_identity(&metadata);
            actual.device == identity.device && actual.inode == identity.inode
        });
    (matches && verify_expected_file(parent, name, receipt).is_ok()).then(|| path.to_path_buf())
}

fn verify_held_file(file: &mut File, artifact: &V3PlannedGroupedArtifact) -> Result<()> {
    if file
        .seek(SeekFrom::End(0))
        .map_err(|_| AuraError::InvalidValue("v3 planned writer io"))?
        != artifact.bytes.len() as u64
    {
        return Err(AuraError::InvalidValue("v3 planned writer output length"));
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| AuraError::InvalidValue("v3 planned writer io"))?;
    let mut offset = 0usize;
    let mut buffer = [0u8; 64 * 1024];
    while offset < artifact.bytes.len() {
        let take = buffer.len().min(artifact.bytes.len() - offset);
        file.read_exact(&mut buffer[..take])
            .map_err(|_| AuraError::InvalidValue("v3 planned writer io"))?;
        if buffer[..take] != artifact.bytes[offset..offset + take] {
            return Err(AuraError::InvalidValue("v3 planned writer verification"));
        }
        offset += take;
    }
    decode_v3_planned_grouped(&artifact.bytes, V3GroupedLimits::HARD)?;
    Ok(())
}

#[cfg(target_os = "linux")]
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V3PlannedGroupedPublicationOutcome {
    Committed { stale_temp_cleanup_required: bool },
    AdoptedExact,
    LinkedNotDurable { recovery_temp: Option<PathBuf> },
    PublicationAmbiguous { recovery_temp: Option<PathBuf> },
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct HeldFileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(target_os = "linux")]
struct HeldParent {
    directory: File,
    display: PathBuf,
}

#[cfg(target_os = "linux")]
impl HeldParent {
    fn open(output: &Path) -> Result<(Self, std::ffi::OsString)> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let name = output
            .file_name()
            .ok_or(AuraError::InvalidValue("v3 planned writer output path"))?
            .to_os_string();
        let parent = output.parent().unwrap_or_else(|| Path::new("."));
        let mut directory = if parent.is_absolute() {
            File::open("/")
        } else {
            File::open(".")
        }
        .map_err(|_| AuraError::InvalidValue("v3 planned writer output parent"))?;
        for component in parent.components() {
            use std::path::Component;
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(value) => directory = openat_directory(&directory, value)?,
                _ => return Err(AuraError::InvalidValue("v3 planned writer output parent")),
            }
        }
        let metadata = directory
            .metadata()
            .map_err(|_| AuraError::InvalidValue("v3 planned writer output parent"))?;
        if !metadata.is_dir()
            || metadata.permissions().mode() & 0o022 != 0
            || metadata.uid() != unsafe { geteuid() }
        {
            return Err(AuraError::InvalidValue("v3 planned writer output parent"));
        }
        let display = fs::read_link(format!("/proc/self/fd/{}", directory.as_raw_fd()))
            .map_err(|_| AuraError::InvalidValue("v3 planned writer output parent"))?;
        if !display.is_absolute() {
            return Err(AuraError::InvalidValue("v3 planned writer output parent"));
        }
        Ok((Self { directory, display }, name))
    }

    fn create_temp(&self, name: &std::ffi::OsStr) -> std::io::Result<File> {
        openat_file(&self.directory, name, true)
    }

    fn open_file(&self, name: &std::ffi::OsStr) -> std::io::Result<File> {
        openat_file(&self.directory, name, false)
    }

    fn open_regular(&self, name: &std::ffi::OsStr) -> std::io::Result<File> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let file = self.open_file(name)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { geteuid() }
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "untrusted publication leaf",
            ));
        }
        Ok(file)
    }

    fn link(&self, source: &std::ffi::OsStr, output: &std::ffi::OsStr) -> std::io::Result<()> {
        linkat_file(&self.directory, source, output)
    }

    fn remove(&self, name: &std::ffi::OsStr) -> std::io::Result<()> {
        unlinkat_file(&self.directory, name)
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn openat(dirfd: i32, path: *const i8, flags: i32, mode: u32) -> i32;
    fn linkat(
        olddirfd: i32,
        oldpath: *const i8,
        newdirfd: i32,
        newpath: *const i8,
        flags: i32,
    ) -> i32;
    fn unlinkat(dirfd: i32, path: *const i8, flags: i32) -> i32;
    fn geteuid() -> u32;
}

#[cfg(target_os = "linux")]
fn c_name(name: &std::ffi::OsStr) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(name.as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "nul filename"))
}

#[cfg(target_os = "linux")]
fn openat_directory(parent: &File, name: &std::ffi::OsStr) -> Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name =
        c_name(name).map_err(|_| AuraError::InvalidValue("v3 planned writer output parent"))?;
    let fd = unsafe {
        openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            0o200000 | 0o400000 | 0o2000000,
            0,
        )
    };
    if fd < 0 {
        return Err(AuraError::InvalidValue("v3 planned writer output parent"));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn openat_file(parent: &File, name: &std::ffi::OsStr, create: bool) -> std::io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_name(name)?;
    let flags = (if create { 0o2 | 0o300 } else { 0 }) | 0o4000 | 0o400000 | 0o2000000;
    let fd = unsafe { openat(parent.as_raw_fd(), name.as_ptr(), flags, 0o600) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

#[cfg(target_os = "linux")]
fn linkat_file(
    parent: &File,
    source: &std::ffi::OsStr,
    output: &std::ffi::OsStr,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let source = c_name(source)?;
    let output = c_name(output)?;
    let result = unsafe {
        linkat(
            parent.as_raw_fd(),
            source.as_ptr(),
            parent.as_raw_fd(),
            output.as_ptr(),
            0,
        )
    };
    (result == 0)
        .then_some(())
        .ok_or_else(std::io::Error::last_os_error)
}

#[cfg(target_os = "linux")]
fn unlinkat_file(parent: &File, name: &std::ffi::OsStr) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let name = c_name(name)?;
    let result = unsafe { unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) };
    (result == 0)
        .then_some(())
        .ok_or_else(std::io::Error::last_os_error)
}

pub struct V3PlannedGroupedCreateOnceWriter {
    writer: Option<V3PlannedGroupedIngestWriter<File>>,
    #[cfg(target_os = "linux")]
    parent: HeldParent,
    #[cfg(target_os = "linux")]
    temp_name: std::ffi::OsString,
    #[cfg(target_os = "linux")]
    output_name: std::ffi::OsString,
    temp_path: PathBuf,
    #[cfg(target_os = "linux")]
    temp_identity: HeldFileIdentity,
    #[cfg(all(test, target_os = "linux"))]
    fault_before_link: bool,
    #[cfg(all(test, target_os = "linux"))]
    fault_after_link: bool,
    #[cfg(all(test, target_os = "linux"))]
    fault_verify_after_link: bool,
    #[cfg(all(test, target_os = "linux"))]
    fault_sync: bool,
    #[cfg(all(test, target_os = "linux"))]
    fault_replace_before_finish: bool,
}

impl V3PlannedGroupedCreateOnceWriter {
    #[cfg(target_os = "linux")]
    pub fn recover_exact(
        output: impl AsRef<Path>,
        receipt: &V3PlannedGroupedWriteReceipt,
    ) -> Result<V3PlannedGroupedPublicationOutcome> {
        let (parent, output_name) = HeldParent::open(output.as_ref())?;
        verify_expected_file(&parent, &output_name, receipt)?;
        parent
            .directory
            .sync_all()
            .map_err(|_| AuraError::InvalidValue("v3 planned writer directory sync"))?;
        Ok(V3PlannedGroupedPublicationOutcome::AdoptedExact)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn recover_exact(
        _output: impl AsRef<Path>,
        _receipt: &V3PlannedGroupedWriteReceipt,
    ) -> Result<V3PlannedGroupedPublicationOutcome> {
        Err(AuraError::InvalidValue(
            "v3 planned writer publication platform",
        ))
    }

    #[cfg(target_os = "linux")]
    pub fn create(
        output: impl AsRef<Path>,
        schema: SchemaDescriptor,
        options: V3PlannedGroupedWriterOptions,
    ) -> Result<Self> {
        let output_path = output.as_ref().to_path_buf();
        let (parent, output_name) = HeldParent::open(&output_path)?;
        if let Err(error) = parent.open_regular(&output_name) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(AuraError::InvalidValue("v3 planned writer output path"));
            }
        }
        let display_name = output_name.to_string_lossy();
        let mut created = None;
        for _ in 0..64 {
            let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let temp_name = std::ffi::OsString::from(format!(
                ".{display_name}.planned-held-{}-{id}",
                std::process::id()
            ));
            match parent.create_temp(&temp_name) {
                Ok(file) => {
                    created = Some((temp_name, file));
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(AuraError::InvalidValue("v3 planned writer temp create")),
            }
        }
        let (temp_name, file) =
            created.ok_or(AuraError::InvalidValue("v3 planned writer temp collision"))?;
        use std::os::unix::fs::PermissionsExt;
        let temp_metadata = file
            .metadata()
            .map_err(|_| AuraError::InvalidValue("v3 planned writer temp mode"))?;
        let temp_identity = file_identity(&temp_metadata);
        if temp_metadata.permissions().mode() & 0o777 != 0o600 {
            let _ = remove_if_identity(&parent, &temp_name, temp_identity);
            return Err(AuraError::InvalidValue("v3 planned writer temp mode"));
        }
        let writer = match V3PlannedGroupedIngestWriter::try_new(file, schema, options) {
            Ok(writer) => writer,
            Err(error) => {
                let _ = remove_if_identity(&parent, &temp_name, temp_identity);
                return Err(error);
            }
        };
        let temp_path = parent.display.join(&temp_name);
        Ok(Self {
            writer: Some(writer),
            parent,
            temp_name,
            output_name,
            temp_path,
            temp_identity,
            #[cfg(all(test, target_os = "linux"))]
            fault_before_link: false,
            #[cfg(all(test, target_os = "linux"))]
            fault_after_link: false,
            #[cfg(all(test, target_os = "linux"))]
            fault_verify_after_link: false,
            #[cfg(all(test, target_os = "linux"))]
            fault_sync: false,
            #[cfg(all(test, target_os = "linux"))]
            fault_replace_before_finish: false,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn create(
        _output: impl AsRef<Path>,
        _schema: SchemaDescriptor,
        _options: V3PlannedGroupedWriterOptions,
    ) -> Result<Self> {
        Err(AuraError::InvalidValue(
            "v3 planned writer publication platform",
        ))
    }

    pub fn write_batch(&mut self, batch: &AuraV3EventBatch) -> Result<()> {
        self.writer
            .as_mut()
            .ok_or(AuraError::InvalidValue("v3 planned writer state"))?
            .write_batch(batch)
    }

    pub fn temp_path(&self) -> &Path {
        &self.temp_path
    }

    #[cfg(target_os = "linux")]
    pub fn finish_and_publish(
        mut self,
    ) -> Result<(
        V3PlannedGroupedWriteReceipt,
        V3PlannedGroupedPublicationOutcome,
    )> {
        let writer = self
            .writer
            .take()
            .ok_or(AuraError::InvalidValue("v3 planned writer state"))?;
        #[cfg(test)]
        if self.fault_replace_before_finish {
            let _ = remove_if_identity(&self.parent, &self.temp_name, self.temp_identity);
            let mut replacement = self.parent.create_temp(&self.temp_name).unwrap();
            replacement.write_all(b"replacement-before-finish").unwrap();
        }
        let (file, mut receipt) = match writer.finish_and_sync() {
            Ok(value) => value,
            Err(error) => {
                let _ = remove_if_identity(&self.parent, &self.temp_name, self.temp_identity);
                return Err(error);
            }
        };
        let held = file
            .metadata()
            .map_err(|_| AuraError::InvalidValue("v3 planned writer held identity"))?;
        #[cfg(test)]
        if self.fault_before_link {
            let _ = remove_if_identity(&self.parent, &self.temp_name, self.temp_identity);
            let mut replacement = self.parent.create_temp(&self.temp_name).unwrap();
            replacement.write_all(b"replacement").unwrap();
        }
        let path_before = match self
            .parent
            .open_regular(&self.temp_name)
            .and_then(|file| file.metadata())
        {
            Ok(metadata) => metadata,
            Err(_) => {
                let recovery_temp = exact_recovery_temp(
                    &self.parent,
                    &self.temp_name,
                    self.temp_identity,
                    &self.temp_path,
                    &receipt,
                );
                return Ok((
                    receipt,
                    V3PlannedGroupedPublicationOutcome::PublicationAmbiguous { recovery_temp },
                ));
            }
        };
        if !path_before.file_type().is_file()
            || !same_file_identity(&held, &path_before)
            || held.len() != receipt.summary.file_bytes
        {
            let recovery_temp = exact_recovery_temp(
                &self.parent,
                &self.temp_name,
                self.temp_identity,
                &self.temp_path,
                &receipt,
            );
            return Ok((
                receipt,
                V3PlannedGroupedPublicationOutcome::PublicationAmbiguous { recovery_temp },
            ));
        }
        if let Err(error) = self.parent.link(&self.temp_name, &self.output_name) {
            if error.kind() == std::io::ErrorKind::AlreadyExists
                && verify_expected_file(&self.parent, &self.output_name, &receipt).is_ok()
            {
                let adoption_sync_failed = self.parent.directory.sync_all().is_err();
                #[cfg(test)]
                let adoption_sync_failed = adoption_sync_failed || self.fault_sync;
                if adoption_sync_failed {
                    let recovery_temp = exact_recovery_temp(
                        &self.parent,
                        &self.temp_name,
                        self.temp_identity,
                        &self.temp_path,
                        &receipt,
                    );
                    return Ok((
                        receipt,
                        V3PlannedGroupedPublicationOutcome::LinkedNotDurable { recovery_temp },
                    ));
                }
                let _ = remove_if_same(&self.parent, &self.temp_name, &held);
                return Ok((receipt, V3PlannedGroupedPublicationOutcome::AdoptedExact));
            }
            let _ = remove_if_same(&self.parent, &self.temp_name, &held);
            return Err(AuraError::InvalidValue("v3 planned writer create once"));
        }
        #[cfg(test)]
        if self.fault_after_link {
            let _ = remove_if_same(&self.parent, &self.output_name, &held);
            let mut replacement = self.parent.create_temp(&self.output_name).unwrap();
            replacement.write_all(b"replacement").unwrap();
        }
        let output_identity = match self
            .parent
            .open_regular(&self.output_name)
            .and_then(|file| file.metadata())
        {
            Ok(metadata) => metadata,
            Err(_) => {
                let recovery_temp = exact_recovery_temp(
                    &self.parent,
                    &self.temp_name,
                    self.temp_identity,
                    &self.temp_path,
                    &receipt,
                );
                return Ok((
                    receipt,
                    V3PlannedGroupedPublicationOutcome::PublicationAmbiguous { recovery_temp },
                ));
            }
        };
        if !same_file_identity(&held, &output_identity) {
            let recovery_temp = exact_recovery_temp(
                &self.parent,
                &self.temp_name,
                self.temp_identity,
                &self.temp_path,
                &receipt,
            );
            return Ok((
                receipt,
                V3PlannedGroupedPublicationOutcome::PublicationAmbiguous { recovery_temp },
            ));
        }
        let verification_failed =
            verify_expected_file(&self.parent, &self.output_name, &receipt).is_err();
        #[cfg(test)]
        let verification_failed = verification_failed || self.fault_verify_after_link;
        if verification_failed {
            let recovery_temp = exact_recovery_temp(
                &self.parent,
                &self.temp_name,
                self.temp_identity,
                &self.temp_path,
                &receipt,
            );
            return Ok((
                receipt,
                V3PlannedGroupedPublicationOutcome::PublicationAmbiguous { recovery_temp },
            ));
        }
        let sync_failed = self.parent.directory.sync_all().is_err();
        #[cfg(test)]
        let sync_failed = sync_failed || self.fault_sync;
        if sync_failed {
            let recovery_temp = exact_recovery_temp(
                &self.parent,
                &self.temp_name,
                self.temp_identity,
                &self.temp_path,
                &receipt,
            );
            return Ok((
                receipt,
                V3PlannedGroupedPublicationOutcome::LinkedNotDurable { recovery_temp },
            ));
        }
        receipt.stale_temp_cleanup_required = !remove_if_same(&self.parent, &self.temp_name, &held);
        let _ = self.parent.directory.sync_all();
        Ok((
            receipt.clone(),
            V3PlannedGroupedPublicationOutcome::Committed {
                stale_temp_cleanup_required: receipt.stale_temp_cleanup_required,
            },
        ))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn finish_and_publish(
        self,
    ) -> Result<(
        V3PlannedGroupedWriteReceipt,
        V3PlannedGroupedPublicationOutcome,
    )> {
        Err(AuraError::InvalidValue(
            "v3 planned writer publication platform",
        ))
    }
}

#[cfg(target_os = "linux")]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::{FieldRole, FieldType, RelationshipPermissions, SchemaBuilder};
    use std::fs::OpenOptions;
    use std::os::unix::fs::PermissionsExt;

    fn publication_schema() -> SchemaDescriptor {
        let schema = SchemaBuilder::new("planned_publication_fault")
            .field("ts", FieldType::TimestampMs, FieldRole::Timestamp)
            .repeated_field("side", FieldType::U8, FieldRole::Side)
            .repeated_field("value", FieldType::I64, FieldRole::Value)
            .dual_domain_repeated_group(
                1,
                vec![1, 2],
                1,
                RelationshipPermissions::none().with_across_domain_same_field(),
            )
            .finish()
            .unwrap();
        let groups = schema.groups.clone();
        schema.with_v3_groups(groups).unwrap()
    }

    fn publication_output(tag: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "aura-planned-publication-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        directory.join("out.aura0")
    }

    #[test]
    fn injected_sync_failure_is_reported_before_publication() {
        let schema = publication_schema();
        let path =
            std::env::temp_dir().join(format!("aura-planned-sync-fault-{}", std::process::id()));
        let _ = fs::remove_file(&path);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let writer =
            V3PlannedGroupedIngestWriter::try_new(file, schema, Default::default()).unwrap();
        assert!(writer
            .finish_and_sync_with(|_| Err(std::io::Error::other("injected sync")))
            .is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn before_after_link_and_directory_sync_faults_have_recoverable_states() {
        let output = publication_output("before");
        let mut before = V3PlannedGroupedCreateOnceWriter::create(
            &output,
            publication_schema(),
            Default::default(),
        )
        .unwrap();
        before.fault_before_link = true;
        let temp = before.temp_path.clone();
        assert_eq!(
            before.finish_and_publish().unwrap().1,
            V3PlannedGroupedPublicationOutcome::PublicationAmbiguous {
                recovery_temp: None
            }
        );
        assert!(!output.exists());
        assert_eq!(fs::read(&temp).unwrap(), b"replacement");
        fs::remove_dir_all(output.parent().unwrap()).unwrap();

        let output = publication_output("after");
        let mut after = V3PlannedGroupedCreateOnceWriter::create(
            &output,
            publication_schema(),
            Default::default(),
        )
        .unwrap();
        after.fault_after_link = true;
        let (_, outcome) = after.finish_and_publish().unwrap();
        assert!(matches!(
            outcome,
            V3PlannedGroupedPublicationOutcome::PublicationAmbiguous {
                recovery_temp: Some(_)
            }
        ));
        assert_eq!(fs::read(&output).unwrap(), b"replacement");
        fs::remove_dir_all(output.parent().unwrap()).unwrap();

        let output = publication_output("verify");
        let mut verify = V3PlannedGroupedCreateOnceWriter::create(
            &output,
            publication_schema(),
            Default::default(),
        )
        .unwrap();
        verify.fault_verify_after_link = true;
        let (receipt, outcome) = verify.finish_and_publish().unwrap();
        assert!(matches!(
            outcome,
            V3PlannedGroupedPublicationOutcome::PublicationAmbiguous {
                recovery_temp: Some(_)
            }
        ));
        assert_eq!(
            V3PlannedGroupedCreateOnceWriter::recover_exact(&output, &receipt).unwrap(),
            V3PlannedGroupedPublicationOutcome::AdoptedExact
        );
        fs::remove_dir_all(output.parent().unwrap()).unwrap();

        let output = publication_output("sync");
        let mut sync = V3PlannedGroupedCreateOnceWriter::create(
            &output,
            publication_schema(),
            Default::default(),
        )
        .unwrap();
        sync.fault_sync = true;
        let (receipt, outcome) = sync.finish_and_publish().unwrap();
        assert!(matches!(
            outcome,
            V3PlannedGroupedPublicationOutcome::LinkedNotDurable {
                recovery_temp: Some(_)
            }
        ));
        assert_eq!(
            V3PlannedGroupedCreateOnceWriter::recover_exact(&output, &receipt).unwrap(),
            V3PlannedGroupedPublicationOutcome::AdoptedExact
        );
        fs::remove_dir_all(output.parent().unwrap()).unwrap();

        let output = publication_output("finish-substitution");
        let mut finish = V3PlannedGroupedCreateOnceWriter::create(
            &output,
            publication_schema(),
            Default::default(),
        )
        .unwrap();
        finish.fault_replace_before_finish = true;
        let temp = finish.temp_path.clone();
        assert_eq!(
            finish.finish_and_publish().unwrap().1,
            V3PlannedGroupedPublicationOutcome::PublicationAmbiguous {
                recovery_temp: None
            }
        );
        assert_eq!(fs::read(temp).unwrap(), b"replacement-before-finish");
        fs::remove_dir_all(output.parent().unwrap()).unwrap();

        let output = publication_output("adoption-sync");
        let first = V3PlannedGroupedCreateOnceWriter::create(
            &output,
            publication_schema(),
            Default::default(),
        )
        .unwrap();
        let _ = first.finish_and_publish().unwrap();
        let mut restart = V3PlannedGroupedCreateOnceWriter::create(
            &output,
            publication_schema(),
            Default::default(),
        )
        .unwrap();
        restart.fault_sync = true;
        assert!(matches!(
            restart.finish_and_publish().unwrap().1,
            V3PlannedGroupedPublicationOutcome::LinkedNotDurable {
                recovery_temp: Some(_)
            }
        ));
        fs::remove_dir_all(output.parent().unwrap()).unwrap();
    }

    #[test]
    fn relative_output_reports_absolute_recovery_temp() {
        let directory = PathBuf::from(format!(".relative-planned-writer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let output = directory.join("out.aura0");
        let writer = V3PlannedGroupedCreateOnceWriter::create(
            &output,
            publication_schema(),
            Default::default(),
        )
        .unwrap();
        let temp = writer.temp_path().to_path_buf();
        assert!(temp.is_absolute());
        drop(writer);
        fs::remove_file(temp).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
