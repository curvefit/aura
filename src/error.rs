use core::fmt;

/// Error type used by the small dependency-free Aura prototypes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuraError {
    UnexpectedEof,
    InvalidMagic { expected: &'static str },
    UnsupportedVersion(u16),
    InvalidBookId(u8),
    InvalidProfile(u8),
    InvalidBlockSize(u16),
    InvalidValue(&'static str),
    TrailingBytes(usize),
}

impl fmt::Display for AuraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => write!(f, "unexpected end of input"),
            Self::InvalidMagic { expected } => write!(f, "invalid magic, expected {expected}"),
            Self::UnsupportedVersion(version) => write!(f, "unsupported version {version}"),
            Self::InvalidBookId(value) => write!(f, "invalid book id {value}"),
            Self::InvalidProfile(value) => write!(f, "invalid profile {value}"),
            Self::InvalidBlockSize(value) => write!(f, "invalid block size {value}"),
            Self::InvalidValue(name) => write!(f, "invalid value for {name}"),
            Self::TrailingBytes(bytes) => write!(f, "{bytes} trailing bytes after decode"),
        }
    }
}

impl std::error::Error for AuraError {}

pub type Result<T> = core::result::Result<T, AuraError>;
