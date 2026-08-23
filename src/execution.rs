//! Explicit, process-local execution strategy selection.
//!
//! These options select implementation paths only. They are never serialized
//! and cannot change the Aura wire format or planner output.

/// Complete-body implementation used while producing Aura1 bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Aura1BodyPath {
    /// The stable production choice. Unsupported specializations fall back to
    /// the established materialized implementation.
    #[default]
    StableAuto,
    /// Write the complete Aura1 file while reconstructing its body.
    DirectFile,
    /// Decode supported Aura0 streams through bounded cursors.
    StreamingCursor,
    /// Materialize encoded streams and write rows directly from them.
    DirectStreams,
    /// Materialize logical columns before writing fixed-width Aura1 rows.
    Columns,
}

/// Actual implementation that produced Aura1 bytes. Unlike
/// [`Aura1BodyPath`], this can describe automatic and compatibility-only
/// outcomes that callers cannot request directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aura1EffectiveBodyPath {
    EmbeddedByteLane,
    DirectFile,
    StreamingCursor,
    DirectStreams,
    Columns,
    Materialized,
    MaterializedFallback,
}

impl Aura1EffectiveBodyPath {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EmbeddedByteLane => "embedded-byte-lane",
            Self::DirectFile => "direct-file",
            Self::StreamingCursor => "streaming-cursor",
            Self::DirectStreams => "direct-streams",
            Self::Columns => "columns",
            Self::Materialized => "materialized",
            Self::MaterializedFallback => "materialized-fallback",
        }
    }
}

impl Aura1BodyPath {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StableAuto => "stable-auto",
            Self::DirectFile => "direct-file",
            Self::StreamingCursor => "streaming-cursor",
            Self::DirectStreams => "direct-streams",
            Self::Columns => "columns",
        }
    }
}

/// Implementation used when an Aura0 body must be reconstructed as columns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Aura0ColumnPath {
    #[default]
    Materialized,
    PartitionedSparseCursor,
}

impl Aura0ColumnPath {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Materialized => "materialized",
            Self::PartitionedSparseCursor => "partitioned-sparse-cursor",
        }
    }
}

/// Policy applied when a specifically requested execution path cannot handle
/// the input plan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UnsupportedPathBehavior {
    /// Return a typed Aura error. No other implementation is tried.
    #[default]
    Error,
    /// Use [`Aura1BodyPath::StableAuto`] and report why fallback occurred.
    FallbackToStable,
}

impl UnsupportedPathBehavior {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::FallbackToStable => "fallback-to-stable",
        }
    }
}

/// Explicit Aura0-to-Aura1 execution controls.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Aura1ExecutionOptions {
    pub body_path: Aura1BodyPath,
    pub column_path: Aura0ColumnPath,
    pub unsupported_path: UnsupportedPathBehavior,
}

/// Auditable execution-only dispatch result. This is intentionally separate
/// from the stable profiled compile result so adding strategy details does not
/// break existing callers that destructure that public result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aura1ExecutionTrace {
    pub requested_body_path: Aura1BodyPath,
    pub effective_body_path: Aura1EffectiveBodyPath,
    pub requested_column_path: Aura0ColumnPath,
    pub effective_column_path: Option<Aura0ColumnPath>,
    pub unsupported_path: UnsupportedPathBehavior,
    pub fallback_reason: Option<&'static str>,
}
