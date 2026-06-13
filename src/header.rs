use crate::bytes::{put_i64_le, put_u16_le, put_u32_le, put_u64_le, put_u8, ByteReader};
use crate::format::{AURA_MAGIC, FORMAT_VERSION};
use crate::{AuraError, Profile, Result};

pub const HEADER_SIZE: usize = 56;

/// Fixed Aura file header. The footer pointer is zero while a writer is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuraHeader {
    pub profile: Profile,
    pub stream_id: u32,
    pub dictionary_id: u32,
    pub base_time_ns: i64,
    pub schema_hash: u64,
    pub footer_offset: u64,
}

impl AuraHeader {
    pub const fn new(profile: Profile) -> Self {
        Self {
            profile,
            stream_id: 0,
            dictionary_id: 0,
            base_time_ns: 0,
            schema_hash: 0,
            footer_offset: 0,
        }
    }

    pub const fn with_stream(
        mut self,
        stream_id: u32,
        dictionary_id: u32,
        base_time_ns: i64,
    ) -> Self {
        self.stream_id = stream_id;
        self.dictionary_id = dictionary_id;
        self.base_time_ns = base_time_ns;
        self
    }

    pub const fn with_schema_hash(mut self, schema_hash: u64) -> Self {
        self.schema_hash = schema_hash;
        self
    }

    pub const fn with_footer_offset(mut self, footer_offset: u64) -> Self {
        self.footer_offset = footer_offset;
        self
    }

    pub fn encode(self) -> [u8; HEADER_SIZE] {
        let mut out = Vec::with_capacity(HEADER_SIZE);
        out.extend_from_slice(AURA_MAGIC);
        put_u8(&mut out, self.profile as u8);
        put_u8(&mut out, HEADER_SIZE as u8);
        put_u16_le(&mut out, FORMAT_VERSION);
        put_u32_le(&mut out, 0);
        put_u32_le(&mut out, 0);
        put_i64_le(&mut out, self.base_time_ns);
        put_u32_le(&mut out, self.stream_id);
        put_u32_le(&mut out, self.dictionary_id);
        put_u64_le(&mut out, self.schema_hash);
        put_u64_le(&mut out, self.footer_offset);
        put_u32_le(&mut out, 0);
        put_u32_le(&mut out, 0);
        debug_assert_eq!(HEADER_SIZE, out.len());

        let mut bytes = [0u8; HEADER_SIZE];
        bytes.copy_from_slice(&out);
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = ByteReader::new(bytes);
        let magic = reader.read_exact(4)?;
        if magic != AURA_MAGIC {
            return Err(AuraError::InvalidMagic { expected: "AURA" });
        }
        let profile_byte = reader.read_u8()?;
        let profile = Profile::from_byte(profile_byte)?;
        let header_len = reader.read_u8()?;
        if usize::from(header_len) != HEADER_SIZE {
            return Err(AuraError::InvalidValue("header length"));
        }
        let version = reader.read_u16_le()?;
        if version != FORMAT_VERSION {
            return Err(AuraError::UnsupportedVersion(version));
        }
        let _reserved0 = reader.read_u32_le()?;
        let _reserved0 = reader.read_u32_le()?;
        let base_time_ns = reader.read_i64_le()?;
        let stream_id = reader.read_u32_le()?;
        let dictionary_id = reader.read_u32_le()?;
        let schema_hash = reader.read_u64_le()?;
        let footer_offset = reader.read_u64_le()?;
        let _reserved1 = reader.read_u32_le()?;
        let _reserved = reader.read_u32_le()?;
        reader.finish()?;

        Ok(Self {
            profile,
            stream_id,
            dictionary_id,
            base_time_ns,
            schema_hash,
            footer_offset,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips_open_and_footer_pointer_states() {
        let open = AuraHeader::new(Profile::Ingest)
            .with_stream(7, 3, 1_725_000_000_000_000_000)
            .with_schema_hash(0xfeed_beef_cafe_babe);
        let decoded_open = AuraHeader::decode(&open.encode()).unwrap();
        assert_eq!(open, decoded_open);
        assert_eq!(7, decoded_open.stream_id);
        assert_eq!(3, decoded_open.dictionary_id);
        assert_eq!(1_725_000_000_000_000_000, decoded_open.base_time_ns);
        assert_eq!(0xfeed_beef_cafe_babe, decoded_open.schema_hash);

        let sealed = open.with_footer_offset(1024);
        let decoded_sealed = AuraHeader::decode(&sealed.encode()).unwrap();
        assert_eq!(sealed, decoded_sealed);
    }

    #[test]
    fn header_size_keeps_schema_slot_reserved() {
        let encoded = AuraHeader::new(Profile::Aura0)
            .with_stream(22, 33, 44)
            .with_schema_hash(55)
            .encode();

        assert_eq!(56, HEADER_SIZE);
        assert_eq!(HEADER_SIZE as u8, encoded[5]);
        assert_eq!(0u32.to_le_bytes(), encoded[8..12]);
    }

    #[test]
    fn footer_pointer_does_not_store_seal_state_in_header() {
        let encoded = AuraHeader::new(Profile::Aura0)
            .with_footer_offset(1024)
            .encode();

        assert_eq!(0u32.to_le_bytes(), encoded[12..16]);
        assert_eq!(0u32.to_le_bytes(), encoded[48..52]);
    }

    #[test]
    fn header_prefix_uses_family_magic_then_profile_and_length() {
        let encoded = AuraHeader::new(Profile::Aura0).encode();

        assert_eq!(b"AURA", &encoded[..4]);
        assert_eq!(Profile::Aura0 as u8, encoded[4]);
        assert_eq!(HEADER_SIZE as u8, encoded[5]);
        assert_eq!(FORMAT_VERSION.to_le_bytes(), encoded[6..8]);
    }
}
