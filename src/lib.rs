//! Aura binary replay codec experiments for sparse book update streams.
//!
//! Aura keeps the public model generic: events contain changed levels for a
//! generic `book_a` or `book_b`, and codecs demonstrate storage/speed tradeoffs.

pub mod error;
pub mod types;

pub use error::{AuraError, Result};
pub use types::{BookEvent, BookId, LevelChange, Profile};
