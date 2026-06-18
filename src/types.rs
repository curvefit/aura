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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuraTypedValue {
    I64(i64),
    I128(i128),
    Opaque16([u8; 16]),
}

impl From<i64> for AuraTypedValue {
    fn from(value: i64) -> Self {
        Self::I64(value)
    }
}

impl AuraTypedValue {
    pub const fn observed_type(&self) -> &'static str {
        match self {
            Self::I64(_) => "i64",
            Self::I128(_) => "i128",
            Self::Opaque16(_) => "opaque16",
        }
    }

    pub fn observed_value_class(&self) -> &'static str {
        match self {
            Self::I64(value) if *value < 0 => "negative integer",
            Self::I64(_) => "integer",
            Self::I128(value) if *value < i64::MIN as i128 || *value > i64::MAX as i128 => {
                "wide integer"
            }
            Self::I128(_) => "integer",
            Self::Opaque16(_) => "fixed bytes",
        }
    }
}
