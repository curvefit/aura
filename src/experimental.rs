//! Development-only V3 and Arrow/shadow protocol entrypoints.
//!
//! These are not the default V2 SDK. Readers remain compatible with the
//! documented retained fixtures; planner APIs may change with explicit migration.
//! [`GroupedSearch`] selects a research boundary; [`compile_v3_grouped_with_plan`]
//! encodes an explicit plan for format tests. See `docs/COMPATIBILITY.md`.

pub use crate::shadow_protocol::{
    arrow_rust_version, build_provenance, cargo_lock_sha256, decode_shadow_arrow_ipc_batch,
    encode_shadow_arrow_ipc, BuildProvenance, ShadowEncodeResult, ShadowProtocolLimits,
    DEFAULT_SHADOW_ARROW_IPC_BYTES, DEFAULT_SHADOW_RECORD_BATCHES,
    DEFAULT_SHADOW_VALUE_BLOCK_BYTES, DEFAULT_SHADOW_VALUE_ROWS, MAX_SHADOW_ARROW_IPC_BYTES,
    MAX_SHADOW_RECORD_BATCHES, SHADOW_ARROW_PROTOCOL, SHADOW_ARTIFACT_KIND,
    SHADOW_HANDSHAKE_SCHEMA, SHADOW_PROTOCOL, SHADOW_RESULT_SCHEMA, SHADOW_SCHEMA_FORMAT,
    SHADOW_VERIFY_RESULT_SCHEMA,
};
pub use crate::shadow_protocol_v2::{
    compile_shadow_grouped_arrow_ipc, decode_shadow_grouped_arrow_ipc_batch,
    encode_shadow_grouped_arrow_ipc, shadow_grouped_arrow_protocol, shadow_grouped_schema_format,
    ShadowGroupedEncodeResult, ShadowGroupedProtocolLimits, DEFAULT_SHADOW_GROUPED_ARROW_IPC_BYTES,
    DEFAULT_SHADOW_GROUPED_RECORD_BATCHES, MAX_SHADOW_GROUPED_ARROW_IPC_BYTES,
    MAX_SHADOW_GROUPED_RECORD_BATCHES, SHADOW_ARTIFACT_KIND_V2, SHADOW_PROTOCOL_V2,
    SHADOW_REPEATED_FIELD_V2, SHADOW_RESULT_SCHEMA_V2, SHADOW_VERIFY_RESULT_SCHEMA_V2,
};
pub use crate::v3_codecs::{fixed_width as v3_physical_fixed_width, integer_varint_codec};
pub use crate::v3_container::{
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
pub use crate::v3_events::{
    canonical_v3_event_batch_sha256, decode_v3_event_block, encode_v3_event_block,
    validate_v3_event_batch, validate_v3_grouped_exact_subset, AuraV3EventBatch,
    CanonicalV3EventHasher, V3EventLimits, DEFAULT_V3_EVENT_BLOCK_BYTES, DEFAULT_V3_EVENT_CHILDREN,
    DEFAULT_V3_EVENT_EVENTS, DEFAULT_V3_EVENT_VALUES, MAX_V3_EVENT_BLOCK_BYTES,
    MAX_V3_EVENT_CHILDREN, MAX_V3_EVENT_EVENTS, MAX_V3_EVENT_OFFSETS_BYTES, MAX_V3_EVENT_VALUES,
    V3_EVENT_BLOCK_VERSION,
};
pub use crate::v3_flat_plan_v2::{
    FlatAuraPlanV2, FLAT_PLAN_V2_DICTIONARY_REGISTRY_VERSION, FLAT_PLAN_V2_MAGIC,
    FLAT_PLAN_V2_PREFIX_SUFFIX_REGISTRY_VERSION, FLAT_PLAN_V2_REGISTRY_VERSION,
    FLAT_PLAN_V2_TEMPORAL_REGISTRY_VERSION, FLAT_PLAN_V2_VERSION, MAX_FLAT_PLAN_V2_BYTES,
};
pub use crate::v3_grouped_container::{
    decode_v3_grouped_aura0, decode_v3_grouped_aura0_with_limits, decode_v3_grouped_footer,
    decode_v3_grouped_footer_with_limits, encode_v3_grouped_footer, v3_grouped_body_sha256,
    v3_grouped_header_sha256, DecodedV3GroupedAura0, V3GroupedChunkDescriptor,
    V3GroupedColumnStats, V3GroupedFooter, V3GroupedLimits,
    DEFAULT_V3_GROUPED_IN_MEMORY_BODY_BYTES, DEFAULT_V3_GROUPED_IN_MEMORY_CHILDREN,
    DEFAULT_V3_GROUPED_IN_MEMORY_CHUNKS, DEFAULT_V3_GROUPED_IN_MEMORY_EVENTS,
    MAX_V3_GROUPED_BODY_BYTES, MAX_V3_GROUPED_CHILDREN, MAX_V3_GROUPED_CHUNKS,
    MAX_V3_GROUPED_EVENTS, MAX_V3_GROUPED_FOOTER_BYTES, MAX_V3_GROUPED_SCHEMA_BYTES,
    V3_GROUPED_BODY_ENCODING_EXACT_EVENTS, V3_GROUPED_BODY_LAYOUT_VERSION,
    V3_GROUPED_CHUNK_DESCRIPTOR_BYTES, V3_GROUPED_EVENT_BLOCK_VERSION,
    V3_GROUPED_FOOTER_LAYOUT_VERSION, V3_GROUPED_FOOTER_PREFIX_BYTES,
    V3_GROUPED_STATS_DESCRIPTOR_BYTES,
};
pub use crate::v3_grouped_reader::{
    AuraV3GroupedReader, V3GroupedAura0Reader, V3GroupedChunkRead, V3GroupedReadAll,
    V3GroupedReaderState, V3GroupedVerifySummary,
};
pub use crate::v3_grouped_writer::{
    AuraV3GroupedWriter, V3GroupedAura0Writer, V3GroupedWriteSummary, V3GroupedWriterOptions,
    V3GroupedWriterState,
};
pub use crate::v3_plan_v2::{
    AuraPlanV2, PlanV2Inspection, PlanV2PhysicalCodec, PlanV2Selection, PlanV2StreamDescriptor,
    AURA_PLAN_V2_AUTHORITATIVE_SOURCE_ORDER, AURA_PLAN_V2_CROSS_DOMAIN_REGISTRY_VERSION,
    AURA_PLAN_V2_DIRECT_OP, AURA_PLAN_V2_DOMAIN0_FROM_DOMAIN1_OP,
    AURA_PLAN_V2_DOMAIN1_FROM_DOMAIN0_OP, AURA_PLAN_V2_INTEGER_CODEC_REGISTRY_VERSION,
    AURA_PLAN_V2_MAGIC, AURA_PLAN_V2_PREVIOUS_WITHIN_DOMAIN_OP, AURA_PLAN_V2_REGISTRY_VERSION,
    AURA_PLAN_V2_SAME_CHILD_PARENT_OP, AURA_PLAN_V2_SAME_CHILD_PARENT_REGISTRY_VERSION,
    AURA_PLAN_V2_SPLIT_DOMAIN_DIRECT_OP, AURA_PLAN_V2_SPLIT_REGISTRY_VERSION, AURA_PLAN_V2_VERSION,
    AURA_PLAN_V2_WITHIN_DOMAIN_REGISTRY_VERSION, MAX_AURA_PLAN_V2_BYTES,
    MAX_AURA_PLAN_V2_DEPENDENCIES, MAX_AURA_PLAN_V2_STREAMS,
};
pub use crate::v3_planned_flat::{
    compile_v3_planned_flat, decode_v3_planned_flat, decode_v3_planned_flat_footer,
    decode_v3_selected_flat, encode_v3_planned_flat_footer, DecodedV3PlannedFlat,
    DecodedV3SelectedFlat, V3PlannedFlatArtifact, V3PlannedFlatCandidateInspection,
    V3PlannedFlatChunkDescriptor, V3PlannedFlatCodecInspection, V3PlannedFlatFooter,
    V3PlannedFlatInspection, V3PlannedFlatPrefixSuffixZstdInspection, V3PlannedFlatSummary,
    V3PlannedFlatZstdInspection, MAX_V3_PLANNED_FLAT_FOOTER_BYTES, V3_PLANNED_FLAT_BLOCK_VERSION,
    V3_PLANNED_FLAT_BODY_ENCODING, V3_PLANNED_FLAT_BODY_LAYOUT_VERSION,
    V3_PLANNED_FLAT_CHUNK_DESCRIPTOR_BYTES, V3_PLANNED_FLAT_DICTIONARY_BLOCK_VERSION,
    V3_PLANNED_FLAT_DICTIONARY_BODY_LAYOUT_VERSION, V3_PLANNED_FLAT_FOOTER_LAYOUT_VERSION,
    V3_PLANNED_FLAT_FOOTER_PREFIX_BYTES, V3_PLANNED_FLAT_PREFIX_SUFFIX_BLOCK_VERSION,
    V3_PLANNED_FLAT_PREFIX_SUFFIX_BODY_LAYOUT_VERSION,
    V3_PLANNED_FLAT_PREFIX_SUFFIX_ZSTD_BLOCK_VERSION,
    V3_PLANNED_FLAT_PREFIX_SUFFIX_ZSTD_BODY_LAYOUT_VERSION,
    V3_PLANNED_FLAT_PREFIX_SUFFIX_ZSTD_WRAPPER_VERSION, V3_PLANNED_FLAT_TEMPORAL_BLOCK_VERSION,
    V3_PLANNED_FLAT_TEMPORAL_BODY_LAYOUT_VERSION, V3_PLANNED_FLAT_TEMPORAL_ZSTD_BLOCK_VERSION,
    V3_PLANNED_FLAT_TEMPORAL_ZSTD_BODY_LAYOUT_VERSION,
    V3_PLANNED_FLAT_TEMPORAL_ZSTD_WRAPPER_VERSION, V3_PLANNED_FLAT_ZSTD_BLOCK_VERSION,
    V3_PLANNED_FLAT_ZSTD_BODY_LAYOUT_VERSION, V3_PLANNED_FLAT_ZSTD_LEVEL,
    V3_PLANNED_FLAT_ZSTD_WINDOW_LOG, V3_PLANNED_FLAT_ZSTD_WRAPPER_VERSION,
};
pub use crate::v3_planned_grouped::{
    compile_v3_grouped_with_plan, compile_v3_planned_grouped, decode_v3_planned_grouped,
    decode_v3_planned_grouped_footer, encode_v3_planned_grouped_footer, DecodedV3PlannedGrouped,
    GroupedSearch, V3PlannedGroupedArtifact, V3PlannedGroupedCandidateInspection,
    V3PlannedGroupedChunkDescriptor, V3PlannedGroupedCodecInspection,
    V3PlannedGroupedCrossInspection, V3PlannedGroupedFooter, V3PlannedGroupedInspection,
    V3PlannedGroupedParentInspection, V3PlannedGroupedSummary, V3PlannedGroupedWithinInspection,
    MAX_V3_PLANNED_GROUPED_FOOTER_BYTES, V3_PLANNED_GROUPED_BODY_ENCODING,
    V3_PLANNED_GROUPED_BODY_LAYOUT_VERSION, V3_PLANNED_GROUPED_CHUNK_DESCRIPTOR_BYTES,
    V3_PLANNED_GROUPED_COMPACT_BLOCK_VERSION, V3_PLANNED_GROUPED_COMPACT_BODY_LAYOUT_VERSION,
    V3_PLANNED_GROUPED_CROSS_DOMAIN_BLOCK_VERSION,
    V3_PLANNED_GROUPED_CROSS_DOMAIN_BODY_LAYOUT_VERSION, V3_PLANNED_GROUPED_DIRECT_BLOCK_VERSION,
    V3_PLANNED_GROUPED_FOOTER_LAYOUT_VERSION, V3_PLANNED_GROUPED_FOOTER_PREFIX_BYTES,
    V3_PLANNED_GROUPED_INTEGER_CODEC_BLOCK_VERSION,
    V3_PLANNED_GROUPED_INTEGER_CODEC_BODY_LAYOUT_VERSION,
    V3_PLANNED_GROUPED_SAME_CHILD_PARENT_BLOCK_VERSION,
    V3_PLANNED_GROUPED_SAME_CHILD_PARENT_BODY_LAYOUT_VERSION,
    V3_PLANNED_GROUPED_WITHIN_DOMAIN_BLOCK_VERSION,
    V3_PLANNED_GROUPED_WITHIN_DOMAIN_BODY_LAYOUT_VERSION,
};
pub use crate::v3_planned_grouped_writer::{
    V3PlannedGroupedCreateOnceWriter, V3PlannedGroupedIngestWriter,
    V3PlannedGroupedPublicationOutcome, V3PlannedGroupedWriteReceipt,
    V3PlannedGroupedWriterOptions, V3PlannedGroupedWriterState,
    DEFAULT_V3_PLANNED_GROUPED_SCRATCH_BYTES, MAX_V3_PLANNED_GROUPED_SCRATCH_BYTES,
};
pub use crate::v3_reader::{
    AuraV3FlatReader, V3FlatAura0Reader, V3FlatReadAll, V3FlatReaderState, V3FlatVerifySummary,
};
pub use crate::v3_values::{
    canonical_v3_batch_sha256, canonical_v3_schema_fingerprint, decode_v3_value_block,
    encode_v3_value_block, validate_decimal_text_v1, validate_v3_batch, AuraV3Batch, AuraV3Column,
    AuraV3ColumnValues, AuraV3ValueRef, AuraV3VariableColumn, CanonicalV3RowHasher,
    V3CanonicalRowHasher, V3ValueLimits, MAX_V3_SCHEMA_DESCRIPTOR_BYTES, MAX_V3_VALUE_BLOCK_BYTES,
    MAX_V3_VALUE_ROWS, MAX_V3_VARIABLE_VALUE_BYTES,
};
pub use crate::v3_writer::{
    AuraV3FlatWriter, V3FlatAura0Writer, V3FlatWriteSummary, V3FlatWriterOptions, V3FlatWriterState,
};
