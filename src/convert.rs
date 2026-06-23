use std::io::{Read, Write};

use crate::options::{AuraFormat, AuraProfile, ConvertOptions};
use crate::records::{self, I64FileInput};
use crate::{AuraError, Profile, Result};

/// Summary returned by the SDK conversion helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionSummary {
    pub source_format: AuraFormat,
    pub target_format: AuraFormat,
    pub record_count: usize,
    pub output_bytes: usize,
    pub schema_hash: u32,
    pub verified: bool,
}

pub fn convert_aura<R: Read, W: Write>(
    mut input: R,
    mut output: W,
    options: ConvertOptions,
) -> Result<ConversionSummary> {
    let mut input_bytes = Vec::new();
    input
        .read_to_end(&mut input_bytes)
        .map_err(|_| AuraError::InvalidValue("converter input"))?;
    let decoded = records::decode_i64_file(&input_bytes)?;
    let source_format = profile_format(decoded.header.profile);
    let record_count = decoded.rows.len();
    let schema_hash = decoded.schema.schema_id;

    let output_bytes = match options.target_format {
        AuraFormat::Aura => records::encode_ingest_i64_file(I64FileInput {
            schema: decoded.schema,
            rows: decoded.rows,
            stream_id: decoded.header.stream_id,
            dictionary_id: decoded.header.dictionary_id,
            header_comment: if decoded.header.comment.is_empty() {
                None
            } else {
                Some(decoded.header.comment)
            },
        })?,
        AuraFormat::Aura1 => {
            if source_format == AuraFormat::Aura0 {
                records::compile_aura0_to_aura1_bytes_with_lane(
                    &input_bytes,
                    options.use_byte_lane,
                    options.verify,
                )?
            } else {
                records::compile_i64_file(&input_bytes, Profile::Aura1)?
            }
        }
        AuraFormat::Aura0 => {
            if options.aura0_profile == AuraProfile::Compact {
                records::compile_i64_file(&input_bytes, Profile::Aura0)?
            } else {
                records::compile_i64_file_with_aura0_profile(
                    &input_bytes,
                    options.aura0_profile.aura0_file_profile(),
                    options.byte_lane_codec,
                )?
            }
        }
    };

    if options.verify {
        let output_decoded = records::decode_i64_file(&output_bytes)?;
        if output_decoded.rows != records::decode_i64_file(&input_bytes)?.rows {
            return Err(AuraError::InvalidValue("conversion verification"));
        }
    }

    output
        .write_all(&output_bytes)
        .map_err(|_| AuraError::InvalidValue("converter output"))?;
    Ok(ConversionSummary {
        source_format,
        target_format: options.target_format,
        record_count,
        output_bytes: output_bytes.len(),
        schema_hash,
        verified: options.verify,
    })
}

const fn profile_format(profile: Profile) -> AuraFormat {
    match profile {
        Profile::Ingest => AuraFormat::Aura,
        Profile::Aura0 => AuraFormat::Aura0,
        Profile::Aura1 => AuraFormat::Aura1,
    }
}
