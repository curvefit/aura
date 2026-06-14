use crate::{AuraError, Result};

pub fn signed_bitpack_width_for_range(min: i64, max: i64) -> u8 {
    if min == 0 && max == 0 {
        return 0;
    }
    for bits in 1..=64 {
        let (lower, upper) = signed_range(bits);
        if i128::from(min) >= lower && i128::from(max) <= upper {
            return bits;
        }
    }
    64
}

pub fn unsigned_bitpack_width(value: u64) -> u8 {
    if value == 0 {
        0
    } else {
        (u64::BITS - value.leading_zeros()) as u8
    }
}

pub fn bitpacked_byte_len(value_count: u64, bit_width: u8) -> u64 {
    (value_count * u64::from(bit_width)).div_ceil(8)
}

pub fn pack_signed_values(values: &[i64], bit_width: u8) -> Result<Vec<u8>> {
    validate_bit_width(bit_width)?;
    if bit_width == 0 {
        if values.iter().any(|value| *value != 0) {
            return Err(AuraError::InvalidValue("bitpacked value"));
        }
        return Ok(Vec::new());
    }

    let (lower, upper) = signed_range(bit_width);
    let mut writer = BitWriter::new();
    for value in values {
        let value = i128::from(*value);
        if value < lower || value > upper {
            return Err(AuraError::InvalidValue("bitpacked value"));
        }
        writer.write_bits(twos_complement_bits(value, bit_width), bit_width);
    }
    Ok(writer.finish())
}

pub fn pack_unsigned_values(values: &[u64], bit_width: u8) -> Result<Vec<u8>> {
    validate_bit_width(bit_width)?;
    if bit_width == 0 {
        if values.iter().any(|value| *value != 0) {
            return Err(AuraError::InvalidValue("bitpacked value"));
        }
        return Ok(Vec::new());
    }

    let upper = unsigned_range_upper(bit_width);
    let mut writer = BitWriter::new();
    for value in values {
        if *value > upper {
            return Err(AuraError::InvalidValue("bitpacked value"));
        }
        writer.write_bits(*value, bit_width);
    }
    Ok(writer.finish())
}

pub fn unpack_signed_values(bytes: &[u8], bit_width: u8, value_count: usize) -> Result<Vec<i64>> {
    validate_bit_width(bit_width)?;
    let expected_len = bitpacked_byte_len(value_count as u64, bit_width) as usize;
    if bytes.len() != expected_len {
        return Err(AuraError::InvalidValue("bitpacked length"));
    }
    if bit_width == 0 {
        return Ok(vec![0; value_count]);
    }

    let mut reader = BitReader::new(bytes);
    let mut values = Vec::with_capacity(value_count);
    for _ in 0..value_count {
        values.push(sign_extend(reader.read_bits(bit_width)?, bit_width)?);
    }
    Ok(values)
}

pub fn unpack_unsigned_values(bytes: &[u8], bit_width: u8, value_count: usize) -> Result<Vec<u64>> {
    validate_bit_width(bit_width)?;
    let expected_len = bitpacked_byte_len(value_count as u64, bit_width) as usize;
    if bytes.len() != expected_len {
        return Err(AuraError::InvalidValue("bitpacked length"));
    }
    if bit_width == 0 {
        return Ok(vec![0; value_count]);
    }

    let mut reader = BitReader::new(bytes);
    let mut values = Vec::with_capacity(value_count);
    for _ in 0..value_count {
        values.push(reader.read_bits(bit_width)?);
    }
    Ok(values)
}

fn validate_bit_width(bit_width: u8) -> Result<()> {
    if bit_width <= 64 {
        Ok(())
    } else {
        Err(AuraError::InvalidValue("bit width"))
    }
}

fn signed_range(bit_width: u8) -> (i128, i128) {
    debug_assert!(bit_width > 0 && bit_width <= 64);
    let lower = -(1i128 << (bit_width - 1));
    let upper = (1i128 << (bit_width - 1)) - 1;
    (lower, upper)
}

fn unsigned_range_upper(bit_width: u8) -> u64 {
    debug_assert!(bit_width > 0 && bit_width <= 64);
    if bit_width == 64 {
        u64::MAX
    } else {
        (1u64 << bit_width) - 1
    }
}

fn twos_complement_bits(value: i128, bit_width: u8) -> u64 {
    if bit_width == 64 {
        value as i64 as u64
    } else {
        let mask = (1u128 << bit_width) - 1;
        (value as u128 & mask) as u64
    }
}

fn sign_extend(raw: u64, bit_width: u8) -> Result<i64> {
    if bit_width == 64 {
        return Ok(raw as i64);
    }
    let sign_bit = 1u64 << (bit_width - 1);
    let value = if raw & sign_bit == 0 {
        i128::from(raw)
    } else {
        i128::from(raw) - (1i128 << bit_width)
    };
    i64::try_from(value).map_err(|_| AuraError::InvalidValue("bitpacked value"))
}

struct BitWriter {
    bytes: Vec<u8>,
    current: u8,
    used_bits: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            used_bits: 0,
        }
    }

    fn write_bits(&mut self, mut value: u64, mut bit_count: u8) {
        while bit_count > 0 {
            let free_bits = 8 - self.used_bits;
            let take = bit_count.min(free_bits);
            let mask = if take == 64 {
                u64::MAX
            } else {
                (1u64 << take) - 1
            };
            self.current |= ((value & mask) as u8) << self.used_bits;
            self.used_bits += take;
            value >>= take;
            bit_count -= take;
            if self.used_bits == 8 {
                self.bytes.push(self.current);
                self.current = 0;
                self.used_bits = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used_bits != 0 {
            self.bytes.push(self.current);
        }
        self.bytes
    }
}

struct BitReader<'a> {
    bytes: &'a [u8],
    byte_index: usize,
    used_bits: u8,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            byte_index: 0,
            used_bits: 0,
        }
    }

    fn read_bits(&mut self, mut bit_count: u8) -> Result<u64> {
        let mut out = 0u64;
        let mut out_shift = 0u8;
        while bit_count > 0 {
            let byte = *self
                .bytes
                .get(self.byte_index)
                .ok_or(AuraError::UnexpectedEof)?;
            let available = 8 - self.used_bits;
            let take = bit_count.min(available);
            let mask = (1u16 << take) - 1;
            let bits = u64::from((u16::from(byte >> self.used_bits) & mask) as u8);
            out |= bits << out_shift;
            self.used_bits += take;
            out_shift += take;
            bit_count -= take;
            if self.used_bits == 8 {
                self.byte_index += 1;
                self.used_bits = 0;
            }
        }
        Ok(out)
    }
}
