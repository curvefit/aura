use crate::bytes::{put_u16_le, put_u32_le, put_u64_le, put_u8, ByteReader};
use crate::format::{AURA0_MAGIC, AURA1_MAGIC, FORMAT_VERSION, INGEST_MAGIC};
use crate::{AuraError, Profile, Result};

pub const HEADER_SIZE: usize = 32;
pub const FLAG_SEALED: u32 = 1;

/// Fixed Aura file header. The footer pointer is zero while a writer is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuraHeader {
    pub profile: Profile,
    pub schema_id: u32,
    pub flags: u32,
    pub footer_offset: u64,
    pub footer_len: u32,
}

impl AuraHeader {
    pub const fn new(profile: Profile, schema_id: u32) -> Self {
        Self {
            profile,
            schema_id,
            flags: 0,
            footer_offset: 0,
            footer_len: 0,
        }
    }

    pub const fn with_footer(mut self, footer_offset: u64, footer_len: u32) -> Self {
        self.flags |= FLAG_SEALED;
        self.footer_offset = footer_offset;
        self.footer_len = footer_len;
        self
    }

    pub const fn is_sealed(self) -> bool {
        self.flags & FLAG_SEALED != 0
    }

    pub fn encode(self) -> [u8; HEADER_SIZE] {
        let mut out = Vec::with_capacity(HEADER_SIZE);
        out.extend_from_slice(magic_for_profile(self.profile));
        put_u16_le(&mut out, FORMAT_VERSION);
        put_u8(&mut out, self.profile as u8);
        put_u8(&mut out, HEADER_SIZE as u8);
        put_u32_le(&mut out, self.schema_id);
        put_u32_le(&mut out, self.flags);
        put_u64_le(&mut out, self.footer_offset);
        put_u32_le(&mut out, self.footer_len);
        put_u32_le(&mut out, 0);
        debug_assert_eq!(HEADER_SIZE, out.len());

        let mut bytes = [0u8; HEADER_SIZE];
        bytes.copy_from_slice(&out);
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = ByteReader::new(bytes);
        let magic = reader.read_exact(4)?;
        let profile = profile_from_magic(magic)?;
        let version = reader.read_u16_le()?;
        if version != FORMAT_VERSION {
            return Err(AuraError::UnsupportedVersion(version));
        }
        let profile_byte = reader.read_u8()?;
        if Profile::from_byte(profile_byte)? != profile {
            return Err(AuraError::InvalidValue("profile magic mismatch"));
        }
        let header_len = reader.read_u8()?;
        if usize::from(header_len) != HEADER_SIZE {
            return Err(AuraError::InvalidValue("header length"));
        }
        let schema_id = reader.read_u32_le()?;
        let flags = reader.read_u32_le()?;
        let footer_offset = reader.read_u64_le()?;
        let footer_len = reader.read_u32_le()?;
        let _reserved = reader.read_u32_le()?;
        reader.finish()?;

        Ok(Self {
            profile,
            schema_id,
            flags,
            footer_offset,
            footer_len,
        })
    }
}

pub const fn magic_for_profile(profile: Profile) -> &'static [u8; 4] {
    match profile {
        Profile::Ingest => INGEST_MAGIC,
        Profile::Aura0 => AURA0_MAGIC,
        Profile::Aura1 => AURA1_MAGIC,
    }
}

pub fn profile_from_magic(magic: &[u8]) -> Result<Profile> {
    if magic == INGEST_MAGIC {
        Ok(Profile::Ingest)
    } else if magic == AURA0_MAGIC {
        Ok(Profile::Aura0)
    } else if magic == AURA1_MAGIC {
        Ok(Profile::Aura1)
    } else {
        Err(AuraError::InvalidMagic { expected: "AURA" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips_open_and_sealed_states() {
        let open = AuraHeader::new(Profile::Ingest, 42);
        let decoded_open = AuraHeader::decode(&open.encode()).unwrap();
        assert_eq!(open, decoded_open);
        assert!(!decoded_open.is_sealed());

        let sealed = open.with_footer(1024, 128);
        let decoded_sealed = AuraHeader::decode(&sealed.encode()).unwrap();
        assert_eq!(sealed, decoded_sealed);
        assert!(decoded_sealed.is_sealed());
    }

    #[test]
    fn magic_maps_to_public_profiles() {
        assert_eq!(Profile::Ingest, profile_from_magic(b"AURA").unwrap());
        assert_eq!(Profile::Aura0, profile_from_magic(b"AUR0").unwrap());
        assert_eq!(Profile::Aura1, profile_from_magic(b"AUR1").unwrap());
    }
}
