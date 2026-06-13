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
pub mod ohlcv;
pub mod plan;
pub mod program;
pub mod records;
pub mod schema;
pub mod stats;
pub mod synthetic;
pub mod types;
pub mod ultra;
pub mod varint;
pub mod warm;

pub use error::{AuraError, Result};
pub use footer::{AuraFooter, CompressionDescriptor, CompressionKind};
pub use header::{AuraHeader, HEADER_PREFIX_SIZE};
pub use plan::{Aura0Plan, Aura1Plan, FieldEncoding, PhysicalFieldPlan};
pub use schema::{
    generic_i64_parent_schema, FieldDescriptor, FieldRelation, FieldRole, FieldTransform,
    FieldType, RelatedFieldMapping, SchemaBuilder, SchemaDescriptor, TransformCandidates,
};
pub use stats::{FieldStats, IngestStats, PhysicalWidth, RunHistogramEntry, ShapeStats};
pub use types::{BookEvent, BookId, LevelChange, Profile};
