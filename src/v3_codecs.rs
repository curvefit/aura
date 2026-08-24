//! Shared exact physical codecs for planned V3 containers.

use crate::bytes::ByteReader;
use crate::{AuraError, FieldType, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PlanV2PhysicalCodec {
    FixedWidth = 0,
    UnsignedUleb128 = 1,
    SignedZigZagUleb128 = 2,
    VariableByteDictionaryBitpacked = 3,
}

impl PlanV2PhysicalCodec {
    pub fn from_code(code: u8) -> Result<Self> {
        match code {
            0 => Ok(Self::FixedWidth),
            1 => Ok(Self::UnsignedUleb128),
            2 => Ok(Self::SignedZigZagUleb128),
            3 => Ok(Self::VariableByteDictionaryBitpacked),
            _ => Err(AuraError::InvalidValue("v3 physical codec")),
        }
    }
}

pub const fn integer_varint_codec(field_type: FieldType) -> Option<PlanV2PhysicalCodec> {
    match field_type {
        FieldType::U8 | FieldType::U16 | FieldType::U32 | FieldType::U64 => {
            Some(PlanV2PhysicalCodec::UnsignedUleb128)
        }
        FieldType::I8
        | FieldType::I16
        | FieldType::I32
        | FieldType::I64
        | FieldType::TimestampNs
        | FieldType::TimestampMs => Some(PlanV2PhysicalCodec::SignedZigZagUleb128),
        FieldType::I128 | FieldType::Opaque16 | FieldType::Utf8 | FieldType::DecimalText => None,
    }
}

pub fn decode_canonical_uleb128(reader: &mut ByteReader<'_>) -> Result<u64> {
    let mut value = 0u64;
    for index in 0..10u32 {
        let byte = reader.read_u8()?;
        if index == 9 && (byte & 0xfe) != 0 {
            return Err(AuraError::InvalidValue("v3 varint overflow"));
        }
        value |= u64::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            if usize::try_from(index + 1).unwrap() != uleb128_len(value) {
                return Err(AuraError::InvalidValue("v3 varint noncanonical"));
            }
            return Ok(value);
        }
    }
    Err(AuraError::InvalidValue("v3 varint overflow"))
}

pub fn decode_canonical_zigzag(reader: &mut ByteReader<'_>) -> Result<i64> {
    let value = decode_canonical_uleb128(reader)?;
    Ok(((value >> 1) as i64) ^ (-((value & 1) as i64)))
}

const fn uleb128_len(mut value: u64) -> usize {
    let mut len = 1usize;
    while value >= 0x80 {
        value >>= 7;
        len += 1;
    }
    len
}

pub const fn fixed_width(field_type: FieldType) -> Option<usize> {
    match field_type {
        FieldType::I8 | FieldType::U8 => Some(1),
        FieldType::I16 | FieldType::U16 => Some(2),
        FieldType::I32 | FieldType::U32 => Some(4),
        FieldType::I64 | FieldType::U64 | FieldType::TimestampNs | FieldType::TimestampMs => {
            Some(8)
        }
        FieldType::I128 | FieldType::Opaque16 => Some(16),
        FieldType::Utf8 | FieldType::DecimalText => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_unsigned(value: u64) {
        let mut bytes = Vec::new();
        crate::varint::encode_u64(value, &mut bytes);
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(decode_canonical_uleb128(&mut reader), Ok(value));
        assert_eq!(reader.finish(), Ok(()));
    }

    fn roundtrip_signed(value: i64) {
        let mut bytes = Vec::new();
        crate::varint::encode_i64(value, &mut bytes);
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(decode_canonical_zigzag(&mut reader), Ok(value));
        assert_eq!(reader.finish(), Ok(()));
    }

    #[test]
    fn shared_integer_codec_table_and_exact_boundaries() {
        for field_type in [
            FieldType::U8,
            FieldType::U16,
            FieldType::U32,
            FieldType::U64,
        ] {
            assert_eq!(
                integer_varint_codec(field_type),
                Some(PlanV2PhysicalCodec::UnsignedUleb128)
            );
        }
        for field_type in [
            FieldType::I8,
            FieldType::I16,
            FieldType::I32,
            FieldType::I64,
            FieldType::TimestampNs,
            FieldType::TimestampMs,
        ] {
            assert_eq!(
                integer_varint_codec(field_type),
                Some(PlanV2PhysicalCodec::SignedZigZagUleb128)
            );
        }
        for field_type in [
            FieldType::I128,
            FieldType::Opaque16,
            FieldType::Utf8,
            FieldType::DecimalText,
        ] {
            assert_eq!(integer_varint_codec(field_type), None);
        }

        for value in [
            0,
            u8::MAX.into(),
            u16::MAX.into(),
            u32::MAX.into(),
            u64::MAX,
        ] {
            roundtrip_unsigned(value);
        }
        for value in [
            i64::from(i8::MIN),
            i64::from(i8::MAX),
            i64::from(i16::MIN),
            i64::from(i16::MAX),
            i64::from(i32::MIN),
            i64::from(i32::MAX),
            i64::MIN,
            -1,
            0,
            i64::MAX,
        ] {
            roundtrip_signed(value);
        }
    }

    #[test]
    fn shared_integer_codec_rejects_noncanonical_truncated_and_overflow() {
        for bytes in [
            &[0x80, 0x00][..],
            &[0x80][..],
            &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80][..],
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02][..],
        ] {
            assert!(decode_canonical_uleb128(&mut ByteReader::new(bytes)).is_err());
            assert!(decode_canonical_zigzag(&mut ByteReader::new(bytes)).is_err());
        }
    }
}
