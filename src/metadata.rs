use crate::{AuraError, Result};

const METADATA_PREFIX: &str = "AURAMETA1|";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SymbolMap {
    entries: Vec<(u64, String)>,
}

impl SymbolMap {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn insert(mut self, id: u64, symbol: impl Into<String>) -> Self {
        self.entries.push((id, symbol.into()));
        self.entries.sort_by_key(|(id, _)| *id);
        self.entries.dedup_by_key(|(id, _)| *id);
        self
    }

    pub fn resolve(&self, id: u64) -> Option<&str> {
        self.entries
            .iter()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, symbol)| symbol.as_str())
    }

    pub fn entries(&self) -> &[(u64, String)] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuraMetadata {
    dataset: Option<String>,
    source: Option<String>,
    venue: Option<String>,
    writer_version: Option<String>,
    symbol_map: SymbolMap,
    custom: Vec<(String, String)>,
}

impl AuraMetadata {
    pub const fn new() -> Self {
        Self {
            dataset: None,
            source: None,
            venue: None,
            writer_version: None,
            symbol_map: SymbolMap::new(),
            custom: Vec::new(),
        }
    }

    pub fn with_dataset(mut self, dataset: impl Into<String>) -> Self {
        self.dataset = Some(dataset.into());
        self
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn with_venue(mut self, venue: impl Into<String>) -> Self {
        self.venue = Some(venue.into());
        self
    }

    pub fn with_writer_version(mut self, writer_version: impl Into<String>) -> Self {
        self.writer_version = Some(writer_version.into());
        self
    }

    pub fn with_symbol_map(mut self, symbol_map: SymbolMap) -> Self {
        self.symbol_map = symbol_map;
        self
    }

    pub fn custom(mut self, key: impl Into<String>, value: impl Into<String>) -> Result<Self> {
        let key = key.into();
        if key.is_empty() {
            return Err(AuraError::InvalidValue("metadata key"));
        }
        self.custom.push((key, value.into()));
        self.custom.sort_by(|left, right| left.0.cmp(&right.0));
        self.custom.dedup_by(|left, right| left.0 == right.0);
        Ok(self)
    }

    pub fn dataset(&self) -> Option<&str> {
        self.dataset.as_deref()
    }

    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    pub fn venue(&self) -> Option<&str> {
        self.venue.as_deref()
    }

    pub fn writer_version(&self) -> Option<&str> {
        self.writer_version.as_deref()
    }

    pub fn symbol_map(&self) -> &SymbolMap {
        &self.symbol_map
    }

    pub fn custom_value(&self, key: &str) -> Option<&str> {
        self.custom
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }

    pub fn custom_entries(&self) -> &[(String, String)] {
        &self.custom
    }

    pub fn is_empty(&self) -> bool {
        self.dataset.is_none()
            && self.source.is_none()
            && self.venue.is_none()
            && self.writer_version.is_none()
            && self.symbol_map.is_empty()
            && self.custom.is_empty()
    }

    pub(crate) fn encode_header_comment(&self) -> Result<String> {
        if self.is_empty() {
            return Ok(String::new());
        }
        let mut parts = Vec::new();
        if let Some(dataset) = &self.dataset {
            parts.push(format!("dataset={}", escape_component(dataset)));
        }
        if let Some(source) = &self.source {
            parts.push(format!("source={}", escape_component(source)));
        }
        if let Some(venue) = &self.venue {
            parts.push(format!("venue={}", escape_component(venue)));
        }
        if let Some(writer_version) = &self.writer_version {
            parts.push(format!("writer={}", escape_component(writer_version)));
        }
        if !self.symbol_map.is_empty() {
            let entries = self
                .symbol_map
                .entries()
                .iter()
                .map(|(id, symbol)| format!("{id}:{}", escape_component(symbol)))
                .collect::<Vec<_>>()
                .join(",");
            parts.push(format!("symbols={entries}"));
        }
        if !self.custom.is_empty() {
            let entries = self
                .custom
                .iter()
                .map(|(key, value)| {
                    format!("{}:{}", escape_component(key), escape_component(value))
                })
                .collect::<Vec<_>>()
                .join(",");
            parts.push(format!("custom={entries}"));
        }
        let comment = format!("{METADATA_PREFIX}{}", parts.join(";"));
        if comment.len() > u8::MAX as usize {
            return Err(AuraError::InvalidValue("metadata header length"));
        }
        Ok(comment)
    }

    pub(crate) fn decode_header_comment(comment: &str) -> Self {
        let Some(payload) = comment.strip_prefix(METADATA_PREFIX) else {
            return Self::new();
        };
        let mut metadata = Self::new();
        for part in payload.split(';').filter(|part| !part.is_empty()) {
            let Some((key, value)) = part.split_once('=') else {
                return Self::new();
            };
            match key {
                "dataset" => metadata.dataset = unescape_component(value),
                "source" => metadata.source = unescape_component(value),
                "venue" => metadata.venue = unescape_component(value),
                "writer" => metadata.writer_version = unescape_component(value),
                "symbols" => {
                    let mut map = SymbolMap::new();
                    for entry in value.split(',').filter(|entry| !entry.is_empty()) {
                        let Some((id, symbol)) = entry.split_once(':') else {
                            return Self::new();
                        };
                        let Ok(id) = id.parse::<u64>() else {
                            return Self::new();
                        };
                        let Some(symbol) = unescape_component(symbol) else {
                            return Self::new();
                        };
                        map = map.insert(id, symbol);
                    }
                    metadata.symbol_map = map;
                }
                "custom" => {
                    let mut custom = Vec::new();
                    for entry in value.split(',').filter(|entry| !entry.is_empty()) {
                        let Some((key, value)) = entry.split_once(':') else {
                            return Self::new();
                        };
                        let Some(key) = unescape_component(key) else {
                            return Self::new();
                        };
                        let Some(value) = unescape_component(value) else {
                            return Self::new();
                        };
                        custom.push((key, value));
                    }
                    custom.sort_by(|left, right| left.0.cmp(&right.0));
                    metadata.custom = custom;
                }
                _ => return Self::new(),
            }
        }
        metadata
    }
}

fn escape_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'%' | b'|' | b';' | b'=' | b',' | b':' => {
                out.push('%');
                out.push(hex_digit(byte >> 4));
                out.push(hex_digit(byte & 0x0f));
            }
            _ => out.push(byte as char),
        }
    }
    out
}

fn unescape_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut index = 0usize;
    let mut out = Vec::with_capacity(bytes.len());
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let high = hex_value(bytes[index + 1])?;
            let low = hex_value(bytes[index + 2])?;
            out.push((high << 4) | low);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'A' + value - 10) as char,
    }
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}
