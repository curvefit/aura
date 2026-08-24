use crate::{AuraError, Profile, Result};

pub const AURA_NAME: &str = "Aura";
pub const AURA_MAGIC: &[u8; 4] = b"AURA";
pub const INGEST_MAGIC: &[u8; 4] = b"AURA";
pub const AURA0_MAGIC: &[u8; 4] = b"AUR0";
pub const AURA1_MAGIC: &[u8; 4] = b"AUR1";
pub const SEAL_MAGIC: &[u8; 8] = b"sealed:)";
/// Aura container versions recognized by this implementation.
///
/// Recognition and layout support are intentionally separate. V3 has an
/// authoritative front header and explicit flat/grouped Aura0 SDK containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum AuraContainerVersion {
    /// Historical header layout. Aura only supports decoding this header; a
    /// complete V1 container/footer contract is not supported.
    LegacyV1 = 1,
    V2 = 2,
    V3 = 3,
}

impl AuraContainerVersion {
    pub const fn wire_value(self) -> u16 {
        self as u16
    }

    pub fn from_wire(value: u16) -> Result<Self> {
        match value {
            1 => Ok(Self::LegacyV1),
            2 => Ok(Self::V2),
            3 => Ok(Self::V3),
            _ => Err(AuraError::UnsupportedVersion(value)),
        }
    }

    /// Confirms that this crate can decode the selected front-header layout.
    pub fn require_supported_header_layout(self) -> Result<()> {
        match self {
            Self::LegacyV1 | Self::V2 | Self::V3 => Ok(()),
        }
    }

    /// Confirms support in the established generic reader/writer container
    /// paths. Explicit V3 flat/grouped Aura0 APIs are intentionally separate.
    pub fn require_supported_container_layout(self) -> Result<()> {
        match self {
            Self::V2 => Ok(()),
            Self::LegacyV1 | Self::V3 => Err(AuraError::UnsupportedVersion(self.wire_value())),
        }
    }
}

pub const DEFAULT_CONTAINER_VERSION: AuraContainerVersion = AuraContainerVersion::V2;
pub const AURA_V2_WIRE_VERSION: u16 = AuraContainerVersion::V2.wire_value();
pub const AURA_V3_WIRE_VERSION: u16 = AuraContainerVersion::V3.wire_value();
/// Normative maximum encoded size of an Aura v3 front header (16 MiB).
pub const MAX_V3_HEADER_BYTES: usize = 16 * 1024 * 1024;

/// Exact V2+ wire size of one chunk descriptor in ingest or compiled footers.
pub const AURA_CHUNK_DESCRIPTOR_SIZE: usize = 76;
/// Normative V2+ supported-subset security ceiling for chunk tables.
///
/// Aura's current readers materialize footer metadata in memory. Files above
/// this ceiling are unsupported rather than eligible for an unsafe override.
pub const MAX_AURA_CHUNK_COUNT: usize = 65_536;

/// Compatibility alias for code that needs the current default emitted wire
/// number. It remains pinned to V2; explicit V3 APIs use `AURA_V3_WIRE_VERSION`.
pub const FORMAT_VERSION: u16 = AURA_V2_WIRE_VERSION;

pub fn profile_extension(profile: Profile) -> &'static str {
    match profile {
        Profile::Ingest => ".aura",
        Profile::Aura0 => ".aura0",
        Profile::Aura1 => ".aura1",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_public_profiles() {
        assert_eq!(".aura", profile_extension(Profile::Ingest));
        assert_eq!(".aura0", profile_extension(Profile::Aura0));
        assert_eq!(".aura1", profile_extension(Profile::Aura1));
    }

    #[test]
    fn container_version_recognition_is_separate_from_layout_support() {
        assert_eq!(
            AuraContainerVersion::from_wire(1),
            Ok(AuraContainerVersion::LegacyV1)
        );
        assert_eq!(
            AuraContainerVersion::from_wire(AURA_V2_WIRE_VERSION),
            Ok(AuraContainerVersion::V2)
        );
        assert_eq!(
            AuraContainerVersion::from_wire(AURA_V3_WIRE_VERSION),
            Ok(AuraContainerVersion::V3)
        );
        assert_eq!(
            AuraContainerVersion::LegacyV1.require_supported_header_layout(),
            Ok(())
        );
        assert_eq!(
            AuraContainerVersion::LegacyV1.require_supported_container_layout(),
            Err(AuraError::UnsupportedVersion(1))
        );
        assert_eq!(
            AuraContainerVersion::V2.require_supported_header_layout(),
            Ok(())
        );
        assert_eq!(
            AuraContainerVersion::V2.require_supported_container_layout(),
            Ok(())
        );
        assert_eq!(
            AuraContainerVersion::V3.require_supported_header_layout(),
            Ok(())
        );
        assert_eq!(
            AuraContainerVersion::V3.require_supported_container_layout(),
            Err(AuraError::UnsupportedVersion(AURA_V3_WIRE_VERSION))
        );
        assert_eq!(
            AuraContainerVersion::from_wire(99),
            Err(AuraError::UnsupportedVersion(99))
        );
        assert_eq!(DEFAULT_CONTAINER_VERSION, AuraContainerVersion::V2);
        assert_eq!(FORMAT_VERSION, AURA_V2_WIRE_VERSION);
    }
}
