//! Aura binary event-file format experiments.
//!
//! Aura keeps ingest facts, logical schemas, seal-time stats, and compiled
//! physical layouts separate so one canonical stream can become compact `.aura0`
//! files or fast replay `.aura1` files.

pub mod bitpack;
pub mod body;
pub mod bytes;
pub mod chunk;
pub mod convert;
pub mod error;
mod fixed_width;
pub mod footer;
pub mod format;
pub mod generic_planner;
pub mod header;
pub mod instructions;
pub mod legacy;
pub mod metadata;
pub mod ohlcv;
pub mod options;
pub mod orderbook;
pub mod plan;
pub mod program;
pub mod random_verify;
pub mod reader;
pub mod records;
pub mod schema;
pub mod scoped;
pub mod source;
pub mod stats;
pub mod types;
pub mod varint;
pub mod writer;

pub use body::{decode_generic_stream_body, encode_generic_stream_body, GenericStreamBodyValue};
pub use convert::{convert_aura, ConversionSummary};
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
pub use metadata::{AuraMetadata, SymbolMap};
pub use options::{AuraFormat, AuraProfile, ConvertOptions, ReaderOptions, WriterOptions};
pub use orderbook::{
    BookApplyBreakdown, BookApplyStats, BookLevel, BookSide, BookStateHash, OrderBookApplyMode,
    OrderBookApplyPlan, OrderBookDelta, OrderBookEngine, OrderBookEngineBuffers,
    OrderBookEngineKind, OrderBookLifecycleMode, OrderBookReplaySession,
    PreparedOrderBookApplyPlan, PreparedOrderBookEngine,
};
pub use plan::{Aura0Plan, Aura1Plan, FieldEncoding, PhysicalFieldPlan};
pub use program::{CompiledAuraField, CompiledAuraPlan};
pub use reader::{
    Aura1FieldI64Iter, Aura1FixedBatchView, Aura1RowView, Aura1SelectedRowView, AuraBatchIter,
    AuraEventGroup, AuraGroupKey, AuraGroupStats, AuraI64EventReader, AuraI64Reader, AuraReader,
    AuraReaderSourceKind, AuraReaderStats, AuraReplayBackend, AuraTypedReader,
    FusedOrderBookReplayStats, GroupBy, OrderBookDeltaBatch, OrderBookDeltaSpec,
    OrderBookDeltaSpecBuilder,
};
pub use records::{
    Aura0ByteLaneCodec, Aura0ByteLaneUse, DecodedI64ColumnsFile, DecodedI64EventFile,
    DecodedI64File, DecodedTypedFile, I64Event, I64EventFileInput, I64FileInput, TypedFileInput,
};
pub use schema::{
    decode_schema_map, generic_i64_parent_schema, schema_parent_mapping, AuraField, AuraSchema,
    AuraSchemaBuilder, AuraType, FieldDescriptor, FieldRelation, FieldRole, FieldScope,
    FieldTransform, FieldType, I64SchemaDefinition, RelatedFieldMapping, SchemaBuilder,
    SchemaDescriptor, SchemaMapEntry, SchemaMapHint, TransformCandidates,
};
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
