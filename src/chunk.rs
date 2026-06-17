/// Directory entry for an independently encoded/compressed chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkDescriptor {
    pub chunk_id: u32,
    pub first_event_index: u64,
    pub event_count: u32,
    pub compressed_offset: u64,
    pub compressed_len: u64,
    pub uncompressed_len: u64,
    pub first_ts_event: u64,
    pub last_ts_event: u64,
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub checksum: u32,
}

/// Deterministic non-cryptographic checksum for examples and tests.
pub fn checksum32(bytes: &[u8]) -> u32 {
    let mut hash = 0x811c9dc5u32;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_is_stable() {
        assert_eq!(0x811c9dc5, checksum32(&[]));
        assert_eq!(0x7830701e, checksum32(b"aura"));
    }
}
