//! Aura binary event-file format experiments.
//!
//! Aura keeps ingest facts, logical schemas, seal-time stats, and compiled
//! physical layouts separate so one canonical stream can become compact `.aura0`
//! files or fast replay `.aura1` files.

pub mod bitpack;
pub mod body;
pub mod bytes;
pub mod chunk;
pub mod error;
pub mod footer;
pub mod format;
pub mod generic_planner;
pub mod header;
pub mod instructions;
pub mod legacy;
pub mod ohlcv;
pub mod plan;
pub mod program;
pub mod reader;
pub mod records;
pub mod schema;
pub mod scoped;
pub mod stats;
pub mod types;
pub mod varint;
pub mod writer;

pub use body::{decode_generic_stream_body, encode_generic_stream_body, GenericStreamBodyValue};
pub use error::{AuraDiagnostic, AuraError, Result};
pub use footer::{AuraFooter, CompressionDescriptor, CompressionKind};
pub use generic_planner::{
    decode_generic_i64_rows, decode_generic_i64_rows_body, encode_generic_i64_rows,
    encode_generic_i64_rows_body, encode_generic_i64_rows_with_plan, plan_generic_i64_rows,
    plan_uuid_const_mask_stream, GenericEncodedI64Rows, GenericEncodedStream,
};
pub use header::{
    AuraHeader, DerivedExpression, DerivedExpressionOp, DerivedExpressionSource,
    HEADER_PREFIX_SIZE, LEGACY_HEADER_PREFIX_SIZE,
};
pub use instructions::{
    DerivedOp, GenericGroupInstruction, GenericInstructionPlan, GenericStreamInstruction,
    GenericStreamOp,
};
pub use plan::{Aura0Plan, Aura1Plan, FieldEncoding, PhysicalFieldPlan};
pub use reader::{AuraI64Reader, AuraTypedReader};
pub use records::{
    DecodedI64ColumnsFile, DecodedI64File, DecodedTypedFile, I64FileInput, TypedFileInput,
};
pub use schema::{
    decode_schema_map, generic_i64_parent_schema, schema_parent_mapping, FieldDescriptor,
    FieldRelation, FieldRole, FieldScope, FieldTransform, FieldType, I64SchemaDefinition,
    RelatedFieldMapping, SchemaBuilder, SchemaDescriptor, SchemaMapEntry, SchemaMapHint,
    TransformCandidates,
};
pub use stats::{FieldStats, IngestStats, PhysicalWidth, RunHistogramEntry, ShapeStats};
pub use types::{AuraTypedValue, Profile};
pub use writer::{AuraI64Writer, AuraTypedWriter};
