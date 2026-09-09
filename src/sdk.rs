//! Supported schema, write, read, event, and conversion API.
//!
//! No private fixtures or research checkout are required. See the `roundtrip`
//! and `order_book_aura0` examples. The caller supplies exact scales and source
//! semantics; conversion cannot recreate omitted facts.

pub use crate::{
    convert_aura, AuraColumn, AuraColumnBatch, AuraError, AuraEventBatch, AuraEventSource,
    AuraField, AuraFileSource, AuraFormat, AuraI64EventReader, AuraI64EventWriter,
    AuraLiveFrameSource, AuraLiveSource, AuraMemorySource, AuraMetadata, AuraProfile, AuraReader,
    AuraRecordBatch, AuraSchema, AuraSchemaBuilder, AuraType, AuraValue, AuraWriter,
    CompiledAuraField, CompiledAuraPlan, ConversionSummary, ConvertOptions, I64Event,
    ReaderOptions, Result, SymbolMap, WriterOptions,
};
