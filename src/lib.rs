//! Aura binary event-file format experiments.
//!
//! Aura keeps ingest facts, logical schemas, seal-time stats, and compiled
//! physical layouts separate so one canonical stream can become compact `.aura0`
//! files or fast replay `.aura1` files.

pub mod bitpack;
pub mod body;
pub mod bytes;
pub mod chunk;
pub mod cold;
pub mod convert;
pub mod error;
pub mod footer;
pub mod format;
pub mod generic_planner;
pub mod grouped;
pub mod header;
pub mod instructions;
pub mod ohlcv;
pub mod plan;
pub mod program;
pub mod records;
pub mod schema;
pub mod scoped;
pub mod stats;
pub mod synthetic;
pub mod types;
pub mod ultra;
pub mod varint;
pub mod warm;

pub use body::{decode_generic_stream_body, encode_generic_stream_body, GenericStreamBodyValue};
pub use error::{AuraError, Result};
pub use footer::{AuraFooter, CompressionDescriptor, CompressionKind};
pub use generic_planner::{
    decode_generic_i64_rows, encode_generic_i64_rows, plan_generic_i64_rows,
    plan_uuid_const_mask_stream, GenericEncodedI64Rows, GenericEncodedStream,
};
pub use header::{AuraHeader, HEADER_PREFIX_SIZE};
pub use instructions::{
    DerivedOp, GenericGroupInstruction, GenericInstructionPlan, GenericStreamInstruction,
    GenericStreamOp,
};
pub use plan::{Aura0Plan, Aura1Plan, FieldEncoding, PhysicalFieldPlan};
pub use schema::{
    generic_i64_parent_schema, schema_parent_mapping, FieldDescriptor, FieldRelation, FieldRole,
    FieldScope, FieldTransform, FieldType, I64SchemaDefinition, RelatedFieldMapping, SchemaBuilder,
    SchemaDescriptor, TransformCandidates,
};
pub use stats::{FieldStats, IngestStats, PhysicalWidth, RunHistogramEntry, ShapeStats};
pub use types::{BookEvent, BookId, LevelChange, Profile};
