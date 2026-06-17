use core::fmt;

/// Structured writer diagnostic for declared-layout errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuraDiagnostic {
    pub row_index: Option<usize>,
    pub slot_index: Option<u16>,
    pub declared_type: &'static str,
    pub observed_type: &'static str,
    pub observed_value_class: &'static str,
    pub suggested_upgrade: Option<&'static str>,
    pub reason: &'static str,
}

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
    Diagnostic(AuraDiagnostic),
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
            Self::Diagnostic(diagnostic) => {
                write!(f, "writer diagnostic: {}", diagnostic.reason)?;
                if let Some(row_index) = diagnostic.row_index {
                    write!(f, ", row {row_index}")?;
                }
                if let Some(slot_index) = diagnostic.slot_index {
                    write!(f, ", slot {slot_index}")?;
                }
                write!(
                    f,
                    ", declared {}, observed {} ({})",
                    diagnostic.declared_type,
                    diagnostic.observed_type,
                    diagnostic.observed_value_class
                )?;
                if let Some(suggested_upgrade) = diagnostic.suggested_upgrade {
                    write!(f, ", suggested {suggested_upgrade}")?;
                }
                Ok(())
            }
            Self::TrailingBytes(bytes) => write!(f, "{bytes} trailing bytes after decode"),
        }
    }
}

impl std::error::Error for AuraError {}

pub type Result<T> = core::result::Result<T, AuraError>;
