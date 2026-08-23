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
pub mod schema_json;
pub mod scoped;
pub mod shadow_protocol;
pub mod source;
pub mod stats;
pub mod types;
pub mod v3_container;
pub mod v3_values;
pub mod varint;
pub mod writer;

pub use body::{decode_generic_stream_body, encode_generic_stream_body, GenericStreamBodyValue};
pub use convert::{convert_aura, ConversionSummary};
pub use error::{AuraDiagnostic, AuraError, Result};
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
    Aura0ByteLaneCodec, Aura0ByteLaneUse, DecodedI64ColumnsFile, DecodedI64EventFile,
    DecodedI64File, DecodedTypedFile, I64Event, I64EventFileInput, I64FileInput, TypedFileInput,
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
pub use shadow_protocol::{
    arrow_rust_version, build_provenance, cargo_lock_sha256, encode_shadow_arrow_ipc,
    BuildProvenance, ShadowEncodeResult, ShadowProtocolLimits, DEFAULT_SHADOW_ARROW_IPC_BYTES,
    DEFAULT_SHADOW_RECORD_BATCHES, DEFAULT_SHADOW_VALUE_BLOCK_BYTES, DEFAULT_SHADOW_VALUE_ROWS,
    MAX_SHADOW_ARROW_IPC_BYTES, MAX_SHADOW_RECORD_BATCHES, SHADOW_ARROW_PROTOCOL,
    SHADOW_ARTIFACT_KIND, SHADOW_HANDSHAKE_SCHEMA, SHADOW_PROTOCOL, SHADOW_RESULT_SCHEMA,
    SHADOW_SCHEMA_FORMAT, SHADOW_VERIFY_RESULT_SCHEMA,
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
pub use v3_container::{
    decode_any_compiled_footer, decode_v3_aura0_file, decode_v3_aura0_footer, decode_v3_flat_aura0,
    decode_v3_flat_aura0_with_limits, decode_v3_flat_footer, decode_v3_flat_footer_with_limits,
    encode_v3_aura0_footer, encode_v3_flat_footer, v3_flat_body_sha256, v3_flat_header_sha256,
    AnyCompiledFooter, DecodedV3Aura0File, DecodedV3FlatAura0, V3Aura0ChunkDescriptor,
    V3Aura0ColumnStats, V3Aura0Footer, V3FlatChunkDescriptor, V3FlatColumnStats, V3FlatFooter,
    V3FlatLimits, DEFAULT_V3_FLAT_IN_MEMORY_BODY_BYTES, DEFAULT_V3_FLAT_IN_MEMORY_CHUNKS,
    DEFAULT_V3_FLAT_IN_MEMORY_ROWS, DEFAULT_V3_FLAT_VALUE_BLOCK_BYTES, MAX_V3_FLAT_BODY_BYTES,
    MAX_V3_FLAT_CHUNKS, MAX_V3_FLAT_FOOTER_BYTES, MAX_V3_FLAT_ROWS, MAX_V3_FLAT_SCHEMA_BYTES,
    V3_FLAT_BODY_ENCODING_EXACT_BLOCKS, V3_FLAT_CHUNK_DESCRIPTOR_BYTES,
    V3_FLAT_FOOTER_LAYOUT_VERSION, V3_FLAT_FOOTER_PREFIX_BYTES, V3_FLAT_STATS_DESCRIPTOR_BYTES,
};
pub use v3_values::{
    canonical_v3_batch_sha256, canonical_v3_schema_fingerprint, decode_v3_value_block,
    encode_v3_value_block, validate_decimal_text_v1, validate_v3_batch, AuraV3Batch, AuraV3Column,
    AuraV3ColumnValues, AuraV3ValueRef, AuraV3VariableColumn, CanonicalV3RowHasher,
    V3CanonicalRowHasher, V3ValueLimits, MAX_V3_SCHEMA_DESCRIPTOR_BYTES, MAX_V3_VALUE_BLOCK_BYTES,
    MAX_V3_VALUE_ROWS, MAX_V3_VARIABLE_VALUE_BYTES,
};
pub use writer::{
    AuraI64EventWriter, AuraI64Writer, AuraTypedWriter, AuraWriteSummary, AuraWriter,
};
