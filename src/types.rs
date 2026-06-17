use crate::{AuraError, Result};

/// Public Aura file profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Profile {
    /// Canonical normalized ingest file with generous logical values.
    Ingest = 0,
    /// Compact storage file compiled from ingest statistics.
    Aura0 = 1,
    /// Replay-optimized file compiled from ingest statistics.
    Aura1 = 2,
}

impl Profile {
    pub fn from_byte(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Ingest),
            1 => Ok(Self::Aura0),
            2 => Ok(Self::Aura1),
            other => Err(AuraError::InvalidProfile(other)),
        }
    }
}
