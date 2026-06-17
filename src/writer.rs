use crate::records::{self, I64FileInput};
use crate::schema::SchemaDescriptor;
use crate::{AuraError, Profile, Result};

/// In-memory writer for positional i64 Aura ingest files.
///
/// This is the public ownership boundary for sealing `.aura` files and
/// compiling them to `.aura0`/`.aura1`. The existing record implementation
/// remains the compatibility layer behind this facade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraI64Writer {
    schema: SchemaDescriptor,
    rows: Vec<Vec<i64>>,
    stream_id: u16,
    dictionary_id: u16,
    header_comment: Option<String>,
}

impl AuraI64Writer {
    pub fn new(schema: SchemaDescriptor) -> Self {
        Self {
            schema,
            rows: Vec::new(),
            stream_id: 0,
            dictionary_id: 0,
            header_comment: None,
        }
    }

    pub fn from_input(input: I64FileInput) -> Self {
        Self {
            schema: input.schema,
            rows: input.rows,
            stream_id: input.stream_id,
            dictionary_id: input.dictionary_id,
            header_comment: input.header_comment,
        }
    }

    pub fn with_stream(mut self, stream_id: u16, dictionary_id: u16) -> Self {
        self.stream_id = stream_id;
        self.dictionary_id = dictionary_id;
        self
    }

    pub fn with_header_comment(mut self, comment: impl Into<String>) -> Self {
        self.header_comment = Some(comment.into());
        self
    }

    pub fn push_row(&mut self, row: impl Into<Vec<i64>>) -> Result<&mut Self> {
        let row = row.into();
        if row.len() != self.schema.fields.len() {
            return Err(AuraError::InvalidValue("record field count"));
        }
        self.rows.push(row);
        Ok(self)
    }

    pub fn extend_rows<I, R>(&mut self, rows: I) -> Result<&mut Self>
    where
        I: IntoIterator<Item = R>,
        R: Into<Vec<i64>>,
    {
        for row in rows {
            self.push_row(row)?;
        }
        Ok(self)
    }

    pub fn schema(&self) -> &SchemaDescriptor {
        &self.schema
    }

    pub fn rows(&self) -> &[Vec<i64>] {
        &self.rows
    }

    pub fn into_input(self) -> I64FileInput {
        I64FileInput {
            schema: self.schema,
            rows: self.rows,
            stream_id: self.stream_id,
            dictionary_id: self.dictionary_id,
            header_comment: self.header_comment,
        }
    }

    pub fn finish(self) -> Result<Vec<u8>> {
        encode_i64(self.into_input())
    }

    pub fn compile_profile(bytes: &[u8], target_profile: Profile) -> Result<Vec<u8>> {
        compile_i64(bytes, target_profile)
    }
}

pub fn encode_i64(input: I64FileInput) -> Result<Vec<u8>> {
    records::encode_ingest_i64_file_inner(input)
}

pub fn compile_i64(bytes: &[u8], target_profile: Profile) -> Result<Vec<u8>> {
    records::compile_i64_file_inner(bytes, target_profile)
}
