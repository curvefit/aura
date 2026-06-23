use crate::records::{Aura0ByteLaneCodec, Aura0ByteLaneUse, Aura0FileProfile};
use crate::Profile;

/// Public Aura file role for SDK writers, readers, and converters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AuraFormat {
    Aura,
    Aura0,
    Aura1,
}

impl AuraFormat {
    pub const fn profile(self) -> Profile {
        match self {
            Self::Aura => Profile::Ingest,
            Self::Aura0 => Profile::Aura0,
            Self::Aura1 => Profile::Aura1,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aura => "aura",
            Self::Aura0 => "aura0",
            Self::Aura1 => "aura1",
        }
    }
}

/// Aura0 storage profile selected by the SDK writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AuraProfile {
    Compact,
    Fast,
    Hybrid,
}

impl AuraProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Fast => "fast",
            Self::Hybrid => "hybrid",
        }
    }

    pub const fn aura0_file_profile(self) -> Aura0FileProfile {
        match self {
            Self::Compact => Aura0FileProfile::Compact,
            Self::Fast => Aura0FileProfile::Fast,
            Self::Hybrid => Aura0FileProfile::Hybrid,
        }
    }
}

/// Writer options for the public SDK writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriterOptions {
    pub format: AuraFormat,
    pub aura0_profile: AuraProfile,
    pub byte_lane_codec: Aura0ByteLaneCodec,
    pub stream_id: u16,
    pub dictionary_id: u16,
}

impl WriterOptions {
    pub const fn new(format: AuraFormat) -> Self {
        Self {
            format,
            aura0_profile: AuraProfile::Compact,
            byte_lane_codec: Aura0ByteLaneCodec::Lz4,
            stream_id: 0,
            dictionary_id: 0,
        }
    }

    pub const fn aura0_compact() -> Self {
        Self::new(AuraFormat::Aura0)
    }

    pub const fn aura1() -> Self {
        Self::new(AuraFormat::Aura1)
    }

    pub const fn profile(mut self, profile: AuraProfile) -> Self {
        self.aura0_profile = profile;
        self
    }

    pub const fn byte_lane_codec(mut self, codec: Aura0ByteLaneCodec) -> Self {
        self.byte_lane_codec = codec;
        self
    }

    pub const fn stream(mut self, stream_id: u16, dictionary_id: u16) -> Self {
        self.stream_id = stream_id;
        self.dictionary_id = dictionary_id;
        self
    }
}

impl Default for WriterOptions {
    fn default() -> Self {
        Self::aura0_compact()
    }
}

/// Reader options for the public SDK reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaderOptions {
    pub use_byte_lane: Aura0ByteLaneUse,
}

impl Default for ReaderOptions {
    fn default() -> Self {
        Self {
            use_byte_lane: Aura0ByteLaneUse::Auto,
        }
    }
}

/// Converter options for the public SDK conversion helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConvertOptions {
    pub target_format: AuraFormat,
    pub aura0_profile: AuraProfile,
    pub byte_lane_codec: Aura0ByteLaneCodec,
    pub use_byte_lane: Aura0ByteLaneUse,
    pub verify: bool,
}

impl ConvertOptions {
    pub const fn new(target_format: AuraFormat) -> Self {
        Self {
            target_format,
            aura0_profile: AuraProfile::Compact,
            byte_lane_codec: Aura0ByteLaneCodec::Lz4,
            use_byte_lane: Aura0ByteLaneUse::Auto,
            verify: false,
        }
    }

    pub const fn profile(mut self, profile: AuraProfile) -> Self {
        self.aura0_profile = profile;
        self
    }

    pub const fn byte_lane_codec(mut self, codec: Aura0ByteLaneCodec) -> Self {
        self.byte_lane_codec = codec;
        self
    }

    pub const fn use_byte_lane(mut self, use_byte_lane: Aura0ByteLaneUse) -> Self {
        self.use_byte_lane = use_byte_lane;
        self
    }

    pub const fn verify(mut self, verify: bool) -> Self {
        self.verify = verify;
        self
    }
}
