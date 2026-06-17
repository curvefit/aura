use crate::bytes::{put_i64_le, put_u16_le, put_u8, ByteReader};
use crate::format::{AURA_MAGIC, FORMAT_VERSION};
use crate::{AuraError, Profile, Result};

pub const HEADER_PREFIX_SIZE: usize = 22;

/// Front Aura file header. The body starts at `header_len`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraHeader {
    pub profile: Profile,
    pub stream_id: u16,
    pub dictionary_id: u16,
    pub base_time_ns: i64,
    pub schema_mapping: Vec<u8>,
    pub comment: String,
}

impl AuraHeader {
    pub fn new(profile: Profile) -> Self {
        Self {
            profile,
            stream_id: 0,
            dictionary_id: 0,
            base_time_ns: 0,
            schema_mapping: Vec::new(),
            comment: String::new(),
        }
    }

    pub fn with_stream(mut self, stream_id: u16, dictionary_id: u16, base_time_ns: i64) -> Self {
        self.stream_id = stream_id;
        self.dictionary_id = dictionary_id;
        self.base_time_ns = base_time_ns;
        self
    }

    pub fn with_schema_mapping(mut self, schema_mapping: Vec<u8>) -> Result<Self> {
        validate_header_lengths(schema_mapping.len(), self.comment.len())?;
        self.schema_mapping = schema_mapping;
        Ok(self)
    }

    pub fn with_comment(mut self, comment: impl Into<String>) -> Result<Self> {
        let comment = comment.into();
        validate_header_lengths(self.schema_mapping.len(), comment.len())?;
        self.comment = comment;
        Ok(self)
    }

    pub fn header_len(&self) -> usize {
        HEADER_PREFIX_SIZE + self.schema_mapping.len() + self.comment.len()
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        validate_header_lengths(self.schema_mapping.len(), self.comment.len())?;
        let header_len = u8::try_from(self.header_len())
            .map_err(|_| AuraError::InvalidValue("header length"))?;
        let schema_len = u8::try_from(self.schema_mapping.len())
            .map_err(|_| AuraError::InvalidValue("schema mapping length"))?;
        let comment_len = u8::try_from(self.comment.len())
            .map_err(|_| AuraError::InvalidValue("header comment length"))?;
        let mut out = Vec::with_capacity(usize::from(header_len));
        out.extend_from_slice(AURA_MAGIC);
        put_u16_le(&mut out, FORMAT_VERSION);
        put_u8(&mut out, self.profile as u8);
        put_u8(&mut out, header_len);
        put_i64_le(&mut out, self.base_time_ns);
        put_u16_le(&mut out, self.stream_id);
        put_u16_le(&mut out, self.dictionary_id);
        put_u8(&mut out, schema_len);
        put_u8(&mut out, comment_len);
        out.extend_from_slice(&self.schema_mapping);
        out.extend_from_slice(self.comment.as_bytes());
        debug_assert_eq!(usize::from(header_len), out.len());
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = ByteReader::new(bytes);
        let magic = reader.read_exact(4)?;
        if magic != AURA_MAGIC {
            return Err(AuraError::InvalidMagic { expected: "AURA" });
        }
        let version = reader.read_u16_le()?;
        if version != FORMAT_VERSION {
            return Err(AuraError::UnsupportedVersion(version));
        }
        let profile_byte = reader.read_u8()?;
        let profile = Profile::from_byte(profile_byte)?;
        let header_len = reader.read_u8()?;
        if usize::from(header_len) != bytes.len() || bytes.len() < HEADER_PREFIX_SIZE {
            return Err(AuraError::InvalidValue("header length"));
        }
        let base_time_ns = reader.read_i64_le()?;
        let stream_id = reader.read_u16_le()?;
        let dictionary_id = reader.read_u16_le()?;
        let schema_len = reader.read_u8()? as usize;
        let comment_len = reader.read_u8()? as usize;
        if HEADER_PREFIX_SIZE + schema_len + comment_len != usize::from(header_len) {
            return Err(AuraError::InvalidValue("header length"));
        }
        let schema_mapping = reader.read_exact(schema_len)?.to_vec();
        let comment = std::str::from_utf8(reader.read_exact(comment_len)?)
            .map_err(|_| AuraError::InvalidValue("header comment"))?
            .to_string();
        reader.finish()?;

        Ok(Self {
            profile,
            stream_id,
            dictionary_id,
            base_time_ns,
            schema_mapping,
            comment,
        })
    }
}

fn validate_header_lengths(schema_len: usize, comment_len: usize) -> Result<()> {
    if schema_len > u8::MAX as usize {
        return Err(AuraError::InvalidValue("schema mapping length"));
    }
    if comment_len > u8::MAX as usize {
        return Err(AuraError::InvalidValue("header comment length"));
    }
    if HEADER_PREFIX_SIZE + schema_len + comment_len > u8::MAX as usize {
        return Err(AuraError::InvalidValue("header length"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips_front_schema_mapping() {
        let open = AuraHeader::new(Profile::Ingest)
            .with_stream(7, 3, 1_725_000_000_000_000_000)
            .with_schema_mapping(vec![100, 0, 2, 2, 2, 0])
            .unwrap();
        let encoded = open.encode().unwrap();
        let decoded_open = AuraHeader::decode(&encoded).unwrap();
        assert_eq!(open, decoded_open);
        assert_eq!(7, decoded_open.stream_id);
        assert_eq!(3, decoded_open.dictionary_id);
        assert_eq!(1_725_000_000_000_000_000, decoded_open.base_time_ns);
        assert_eq!(
            &[100, 0, 2, 2, 2, 0],
            decoded_open.schema_mapping.as_slice()
        );
        assert_eq!("", decoded_open.comment);
    }

    #[test]
    fn header_len_includes_schema_mapping() {
        let encoded = AuraHeader::new(Profile::Aura0)
            .with_stream(22, 33, 44)
            .with_schema_mapping(vec![0, 0, 2])
            .unwrap()
            .encode()
            .unwrap();

        assert_eq!(HEADER_PREFIX_SIZE + 3, encoded.len());
        assert_eq!((HEADER_PREFIX_SIZE + 3) as u8, encoded[7]);
        assert_eq!(3, encoded[20]);
        assert_eq!(0, encoded[21]);
        assert_eq!(&[0, 0, 2], &encoded[22..25]);
    }

    #[test]
    fn header_len_includes_comment_after_schema_mapping() {
        let encoded = AuraHeader::new(Profile::Aura0)
            .with_stream(22, 33, 44)
            .with_schema_mapping(vec![0, 0, 2])
            .unwrap()
            .with_comment("ts,open,high")
            .unwrap()
            .encode()
            .unwrap();

        assert_eq!(HEADER_PREFIX_SIZE + 3 + 12, encoded.len());
        assert_eq!((HEADER_PREFIX_SIZE + 3 + 12) as u8, encoded[7]);
        assert_eq!(3, encoded[20]);
        assert_eq!(12, encoded[21]);
        assert_eq!(&[0, 0, 2], &encoded[22..25]);
        assert_eq!(b"ts,open,high", &encoded[25..37]);

        let decoded = AuraHeader::decode(&encoded).unwrap();
        assert_eq!("ts,open,high", decoded.comment);
    }

    #[test]
    fn header_prefix_reads_version_before_profile_and_length() {
        let encoded = AuraHeader::new(Profile::Aura0).encode().unwrap();

        assert_eq!(b"AURA", &encoded[..4]);
        assert_eq!(FORMAT_VERSION.to_le_bytes(), encoded[4..6]);
        assert_eq!(Profile::Aura0 as u8, encoded[6]);
        assert_eq!(HEADER_PREFIX_SIZE as u8, encoded[7]);
    }
}
