use crate::bytes::ByteReader;
use crate::{AuraError, Result};

pub fn encode_u64(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

pub fn decode_u64(reader: &mut ByteReader<'_>) -> Result<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        if shift >= 64 {
            return Err(AuraError::InvalidValue("varint"));
        }
        let byte = reader.read_u8()?;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
    }
}

pub const fn zigzag_encode(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

pub const fn zigzag_decode(value: u64) -> i64 {
    ((value >> 1) as i64) ^ (-((value & 1) as i64))
}

pub fn encode_i64(value: i64, out: &mut Vec<u8>) {
    encode_u64(zigzag_encode(value), out);
}

pub fn decode_i64(reader: &mut ByteReader<'_>) -> Result<i64> {
    Ok(zigzag_decode(decode_u64(reader)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trips_boundaries() {
        for value in [0, 1, 2, 127, 128, 255, 16_384, u32::MAX as u64, u64::MAX] {
            let mut bytes = Vec::new();
            encode_u64(value, &mut bytes);
            let mut reader = ByteReader::new(&bytes);
            assert_eq!(value, decode_u64(&mut reader).unwrap());
            assert_eq!(Ok(()), reader.finish());
        }
    }

    #[test]
    fn zigzag_round_trips_signed_values() {
        for value in [i64::MIN + 1, -1000, -1, 0, 1, 1000, i64::MAX] {
            let mut bytes = Vec::new();
            encode_i64(value, &mut bytes);
            let mut reader = ByteReader::new(&bytes);
            assert_eq!(value, decode_i64(&mut reader).unwrap());
        }
    }
}
