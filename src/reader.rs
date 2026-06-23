use std::io::Read;

use crate::footer::AuraFooter;
use crate::header::AuraHeader;
use crate::options::{AuraFormat, ReaderOptions};
use crate::program::{CompiledAuraPlan, CompiledFooter};
use crate::records::{self, DecodedI64File, DecodedTypedFile};
use crate::schema::{AuraSchema, SchemaDescriptor};
use crate::{AuraError, AuraRecordBatch, AuraTypedValue, Profile, Result};

/// Public SDK reader for Aura files with dynamic schemas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraReader {
    bytes: Vec<u8>,
    schema: AuraSchema,
    profile: Profile,
    rows: Vec<Vec<i64>>,
    compiled_footer: Option<CompiledFooter>,
    compiled_plan: Option<CompiledAuraPlan>,
    cursor: usize,
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
        let decoded = records::decode_i64_file(&bytes)?;
        if decoded.header.profile == Profile::Aura0 {
            match options.use_byte_lane {
                crate::Aura0ByteLaneUse::Always => {
                    let Some(footer) = decoded.compiled_footer.as_ref() else {
                        return Err(AuraError::InvalidValue("aura0 byte lane"));
                    };
                    if footer.aura1_byte_lanes.is_empty() {
                        return Err(AuraError::InvalidValue("aura0 byte lane"));
                    }
                }
                crate::Aura0ByteLaneUse::Auto | crate::Aura0ByteLaneUse::Never => {}
            }
        }
        let compiled_plan = decoded
            .compiled_footer
            .as_ref()
            .map(CompiledAuraPlan::from_footer)
            .transpose()?;
        Ok(Self {
            bytes,
            schema: AuraSchema::from(decoded.schema.clone()),
            profile: decoded.header.profile,
            rows: decoded.rows,
            compiled_footer: decoded.compiled_footer,
            compiled_plan,
            cursor: 0,
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
        Ok(vec![AuraRecordBatch::from_i64_decoded(
            self.schema.clone(),
            self.rows.clone(),
        )?])
    }

    pub fn next_batch(&mut self, batch_size: usize) -> Result<Option<AuraRecordBatch>> {
        if batch_size == 0 {
            return Err(AuraError::InvalidValue("batch size"));
        }
        if self.cursor >= self.rows.len() {
            return Ok(None);
        }
        let end = self.cursor.saturating_add(batch_size).min(self.rows.len());
        let rows = self.rows[self.cursor..end].to_vec();
        self.cursor = end;
        Ok(Some(AuraRecordBatch::from_i64_decoded(
            self.schema.clone(),
            rows,
        )?))
    }

    pub fn reset_batches(&mut self) {
        self.cursor = 0;
    }

    pub fn batches(&self, batch_size: usize) -> Result<AuraBatchIter<'_>> {
        if batch_size == 0 {
            return Err(AuraError::InvalidValue("batch size"));
        }
        Ok(AuraBatchIter {
            reader: self,
            batch_size,
            cursor: 0,
        })
    }

    pub fn replay_i64<F>(&self, mut visitor: F) -> Result<usize>
    where
        F: FnMut(&[i64]) -> Result<()>,
    {
        for row in &self.rows {
            visitor(row)?;
        }
        Ok(self.rows.len())
    }

    pub fn rows_i64(&self) -> &[Vec<i64>] {
        &self.rows
    }

    pub fn into_rows_i64(self) -> Vec<Vec<i64>> {
        self.rows
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

pub struct AuraBatchIter<'a> {
    reader: &'a AuraReader,
    batch_size: usize,
    cursor: usize,
}

impl Iterator for AuraBatchIter<'_> {
    type Item = Result<AuraRecordBatch>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cursor >= self.reader.rows.len() {
            return None;
        }
        let end = self
            .cursor
            .saturating_add(self.batch_size)
            .min(self.reader.rows.len());
        let rows = self.reader.rows[self.cursor..end].to_vec();
        self.cursor = end;
        Some(AuraRecordBatch::from_i64_decoded(
            self.reader.schema.clone(),
            rows,
        ))
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
