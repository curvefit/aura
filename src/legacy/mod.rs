//! Quarantined fixed-event prototypes kept for reference.
//!
//! The core Aura format is schema/header driven. These modules predate that
//! boundary and use a fixed `BookEvent` shape, so they are intentionally kept
//! under `legacy` instead of the crate root.

pub mod chunk;
pub mod cold;
pub mod convert;
pub mod grouped;
pub mod stats;
pub mod synthetic;
pub mod types;
pub mod ultra;
pub mod warm;

pub use types::{BookEvent, BookId, LevelChange};
