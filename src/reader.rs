use crate::footer::AuraFooter;
use crate::header::AuraHeader;
use crate::program::CompiledFooter;
use crate::records::{self, DecodedI64File, DecodedTypedFile};
use crate::schema::SchemaDescriptor;
use crate::{AuraTypedValue, Profile, Result};

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
