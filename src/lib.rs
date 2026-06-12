//! Aura binary event-file format experiments.
//!
//! Aura keeps ingest facts, logical schemas, seal-time stats, and compiled
//! physical layouts separate so one canonical stream can become compact `.aura0`
//! files or fast replay `.aura1` files.

pub mod bytes;
pub mod chunk;
pub mod cold;
pub mod convert;
pub mod error;
pub mod footer;
pub mod format;
pub mod grouped;
pub mod header;
pub mod plan;
pub mod schema;
pub mod stats;
pub mod synthetic;
pub mod types;
pub mod ultra;
pub mod varint;
pub mod warm;

pub use error::{AuraError, Result};
pub use footer::{AuraFooter, CompressionDescriptor, CompressionKind};
pub use header::{AuraHeader, FLAG_SEALED, HEADER_SIZE};
pub use plan::{Aura0Plan, Aura1Plan, FieldEncoding, PhysicalFieldPlan};
pub use schema::{
    FieldDescriptor, FieldRelation, FieldRole, FieldType, SchemaBuilder, SchemaDescriptor,
};
pub use stats::{FieldStats, IngestStats, PhysicalWidth, RunHistogramEntry, ShapeStats};
pub use types::{BookEvent, BookId, LevelChange, Profile};
