//! Aura binary replay codec experiments for sparse book update streams.
//!
//! Aura keeps the public model generic: events contain changed levels for a
//! generic `book_a` or `book_b`, and codecs demonstrate storage/speed tradeoffs.

pub mod bytes;
pub mod chunk;
pub mod cold;
pub mod convert;
pub mod error;
pub mod grouped;
pub mod stats;
pub mod synthetic;
pub mod types;
pub mod ultra;
pub mod varint;
pub mod warm;

pub use error::{AuraError, Result};
pub use types::{BookEvent, BookId, LevelChange, Profile};
