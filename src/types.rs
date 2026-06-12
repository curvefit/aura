use crate::{AuraError, Result};

/// Generic source-book label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum BookId {
    BookA = 1,
    BookB = 2,
}

impl BookId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BookA => "book_a",
            Self::BookB => "book_b",
        }
    }

    pub fn from_byte(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::BookA),
            2 => Ok(Self::BookB),
            other => Err(AuraError::InvalidBookId(other)),
        }
    }
}

/// Aura storage/replay profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Profile {
    Cold = 0,
    Warm = 1,
    GroupedHot = 2,
    UltraHot = 3,
}

impl Profile {
    pub fn from_byte(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Cold),
            1 => Ok(Self::Warm),
            2 => Ok(Self::GroupedHot),
            3 => Ok(Self::UltraHot),
            other => Err(AuraError::InvalidProfile(other)),
        }
    }
}

/// Changed level stored as scaled integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelChange {
    pub price: i64,
    pub qty_a: i64,
    pub qty_b: i64,
}

impl LevelChange {
    pub const fn new(price: i64, qty_a: i64, qty_b: i64) -> Self {
        Self {
            price,
            qty_a,
            qty_b,
        }
    }

    pub const fn delete(price: i64) -> Self {
        Self {
            price,
            qty_a: 0,
            qty_b: 0,
        }
    }
}

/// One replay event containing changed bid and ask levels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookEvent {
    pub ts_event: u64,
    pub sequence: u64,
    pub book: BookId,
    pub bids: Vec<LevelChange>,
    pub asks: Vec<LevelChange>,
}

impl BookEvent {
    pub fn new(
        ts_event: u64,
        sequence: u64,
        book: BookId,
        bids: Vec<LevelChange>,
        asks: Vec<LevelChange>,
    ) -> Self {
        Self {
            ts_event,
            sequence,
            book,
            bids,
            asks,
        }
    }

    pub fn level_count(&self) -> usize {
        self.bids.len() + self.asks.len()
    }

    pub fn validate_counts(&self) -> Result<()> {
        if self.bids.len() > u32::MAX as usize {
            return Err(AuraError::InvalidValue("bid count"));
        }
        if self.asks.len() > u32::MAX as usize {
            return Err(AuraError::InvalidValue("ask count"));
        }
        Ok(())
    }
}
