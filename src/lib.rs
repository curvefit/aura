//! Exact, versioned binary event storage and replay.
//! Start with [`sdk`] for ordinary use and [`experimental`] for V3 research.
//!
//! Aura keeps ingest facts, logical schemas, seal-time stats, and compiled
//! physical layouts separate so one canonical stream can become compact `.aura0`
//! files or fast replay `.aura1` files.

#[doc(hidden)]
pub mod bitpack;
#[doc(hidden)]
pub mod body;
#[doc(hidden)]
pub mod bytes;
#[doc(hidden)]
pub mod chunk;
#[doc(hidden)]
pub mod convert;
#[doc(hidden)]
pub mod error;
#[doc(hidden)]
pub mod execution;
mod expressions;
mod fixed_width;
#[doc(hidden)]
pub mod footer;
#[doc(hidden)]
pub mod format;
#[doc(hidden)]
pub mod generic_planner;
#[doc(hidden)]
pub mod header;
#[doc(hidden)]
pub mod instructions;
#[doc(hidden)]
pub mod legacy;
#[doc(hidden)]
pub mod metadata;
#[doc(hidden)]
pub mod ohlcv;
#[doc(hidden)]
pub mod options;
#[doc(hidden)]
pub mod orderbook;
#[doc(hidden)]
pub mod plan;
#[doc(hidden)]
pub mod program;
#[doc(hidden)]
pub mod random_verify;
#[doc(hidden)]
pub mod reader;
#[doc(hidden)]
pub mod records;
#[doc(hidden)]
pub mod schema;
#[doc(hidden)]
pub mod schema_json;
#[doc(hidden)]
pub mod scoped;
#[doc(hidden)]
pub mod shadow_protocol;
#[doc(hidden)]
pub mod shadow_protocol_v2;
#[doc(hidden)]
pub mod source;
#[doc(hidden)]
pub mod stats;
#[doc(hidden)]
pub mod types;
#[doc(hidden)]
pub mod v3_codecs;
#[doc(hidden)]
pub mod v3_container;
#[doc(hidden)]
pub mod v3_events;
#[doc(hidden)]
pub mod v3_flat_plan_v2;
#[doc(hidden)]
pub mod v3_grouped_container;
#[doc(hidden)]
pub mod v3_grouped_reader;
#[doc(hidden)]
pub mod v3_grouped_writer;
#[doc(hidden)]
pub mod v3_plan_v2;
#[doc(hidden)]
pub mod v3_planned_flat;
#[doc(hidden)]
pub mod v3_planned_grouped;
#[doc(hidden)]
pub mod v3_planned_grouped_writer;
#[doc(hidden)]
pub mod v3_reader;
#[doc(hidden)]
pub mod v3_values;
#[doc(hidden)]
pub mod v3_writer;
#[doc(hidden)]
pub mod varint;
#[doc(hidden)]
pub mod writer;

pub use body::{decode_generic_stream_body, encode_generic_stream_body, GenericStreamBodyValue};
pub use convert::{convert_aura, ConversionSummary};
pub use error::{AuraDiagnostic, AuraError, Result};
pub use execution::{
    Aura0ColumnPath, Aura1BodyPath, Aura1EffectiveBodyPath, Aura1ExecutionOptions,
    Aura1ExecutionTrace, UnsupportedPathBehavior,
};
pub use footer::{AuraFooter, CompressionDescriptor, CompressionKind};
pub use format::{
    AuraContainerVersion, AURA_CHUNK_DESCRIPTOR_SIZE, MAX_AURA_CHUNK_COUNT, MAX_V3_HEADER_BYTES,
};
pub use generic_planner::{
    decode_generic_i64_rows, decode_generic_i64_rows_body, encode_generic_i64_rows,
    encode_generic_i64_rows_body, encode_generic_i64_rows_with_plan, plan_generic_i64_rows,
    plan_uuid_const_mask_stream, GenericEncodedI64Rows, GenericEncodedStream,
};
pub use header::{
    AuraHeader, DerivedExpression, DerivedExpressionOp, DerivedExpressionSource,
    HEADER_PREFIX_SIZE, LEGACY_HEADER_PREFIX_SIZE, V3_HEADER_PREFIX_SIZE,
};
pub use instructions::{
    DerivedOp, GenericGroupInstruction, GenericInstructionPlan, GenericStreamInstruction,
    GenericStreamOp,
};
pub use metadata::{AuraMetadata, SymbolMap};
pub use options::{AuraFormat, AuraProfile, ConvertOptions, ReaderOptions, WriterOptions};
pub use orderbook::{
    BookApplyBreakdown, BookApplyStats, BookLevel, BookSide, BookStateHash, OrderBookApplyMode,
    OrderBookApplyPlan, OrderBookDelta, OrderBookEngine, OrderBookEngineBuffers,
    OrderBookEngineKind, OrderBookLifecycleMode, OrderBookReplaySession,
    PreparedOrderBookApplyPlan, PreparedOrderBookEngine,
};
pub use plan::{Aura0Plan, Aura1Plan, FieldEncoding, PhysicalFieldPlan};
pub use program::{
    CompiledAuraField, CompiledAuraPlan, AURA1_BYTE_LANE_DESCRIPTOR_SIZE,
    MAX_AURA1_BYTE_LANES_TOTAL_OUTPUT_BYTES, MAX_AURA1_BYTE_LANE_COMPRESSED_BYTES,
    MAX_AURA1_BYTE_LANE_COUNT, MAX_AURA1_BYTE_LANE_OUTPUT_BYTES,
};
pub use reader::{
    Aura1FieldI64Iter, Aura1FixedBatchView, Aura1RowView, Aura1SelectedRowView, AuraBatchIter,
    AuraEventGroup, AuraGroupKey, AuraGroupStats, AuraI64EventReader, AuraI64Reader, AuraReader,
    AuraReaderSourceKind, AuraReaderStats, AuraReplayBackend, AuraTypedReader,
    FusedOrderBookReplayStats, GroupBy, OrderBookDeltaBatch, OrderBookDeltaSpec,
    OrderBookDeltaSpecBuilder,
};
pub use records::{
    compile_aura0_to_aura1_materialized, Aura0ByteLaneCodec, Aura0ByteLaneUse,
    DecodedI64ColumnsFile, DecodedI64EventFile, DecodedI64File, DecodedTypedFile, I64Event,
    I64EventFileInput, I64FileInput, MaterializedAura1CompileOutput, TypedFileInput,
    MAX_V2_I64_DECODE_BODY_BYTES, MAX_V2_I64_DECODE_FIELDS, MAX_V2_I64_DECODE_ROWS,
    MAX_V2_I64_DECODE_VALUES,
};
pub use schema::{
    decode_group_descriptor_table, decode_schema_descriptor, decode_schema_map,
    decode_v3_schema_map, encode_group_descriptor_table, encode_schema_descriptor,
    generic_i64_parent_schema, schema_parent_mapping, validate_schema_container_compatibility,
    AuraField, AuraSchema, AuraSchemaBuilder, AuraType, DualDomainDescriptor, FieldDescriptor,
    FieldRelation, FieldRole, FieldScope, FieldTransform, FieldType, GroupDescriptor, GroupKind,
    I64SchemaDefinition, RelatedFieldMapping, RelationshipPermissions, SchemaBuilder,
    SchemaDescriptor, SchemaEncodingVersion, SchemaMapEntry, SchemaMapHint, TransformCandidates,
    AURA_V3_GROUP_DESCRIPTOR_TABLE_VERSION, MAX_DUAL_DOMAIN_COUNT,
};
pub use schema_json::{canonicalize_schema_json, parse_schema_json, MAX_SCHEMA_JSON_BYTES};
pub use source::{
    AuraEventBatch, AuraEventSource, AuraEventSourceStats, AuraFileSource, AuraLiveFrameSource,
    AuraLiveSource, AuraMemorySource,
};
pub use stats::{FieldStats, IngestStats, PhysicalWidth, RunHistogramEntry, ShapeStats};
pub use types::{
    AuraBatch, AuraColumn, AuraColumnBatch, AuraColumnBatchBuilder, AuraRecordBatch,
    AuraTypedValue, AuraValue, Profile,
};
pub use writer::{
    AuraI64EventWriter, AuraI64Writer, AuraTypedWriter, AuraWriteSummary, AuraWriter,
};

/// Explicit development/experimental formats and protocol adapters.
pub mod experimental;
/// Supported V2 facade. Root reexports remain compatible with existing consumers.
pub mod sdk;
// Existing implementation references use the crate root internally.
pub(crate) use experimental::*;
// Retained flat-data consumer contract used by Grimoire. Keep these exact paths.
pub use experimental::{
    decode_v3_selected_flat, AuraV3ValueRef, DecodedV3SelectedFlat, V3FlatLimits,
};
