use crate::{AuraError, Result};

#[derive(Debug, Clone)]
pub struct ByteReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ByteReader<'a> {
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub const fn offset(&self) -> usize {
        self.offset
    }

    pub const fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    pub fn finish(self) -> Result<()> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(AuraError::TrailingBytes(self.remaining()))
        }
    }

    pub fn read_exact(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(AuraError::UnexpectedEof)?;
        if end > self.bytes.len() {
            return Err(AuraError::UnexpectedEof);
        }
        let out = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(out)
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        Ok(self.read_exact(1)?[0])
    }

    pub fn read_u16_le(&mut self) -> Result<u16> {
        let bytes = self.read_exact(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub fn read_u32_le(&mut self) -> Result<u32> {
        let bytes = self.read_exact(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn read_u64_le(&mut self) -> Result<u64> {
        let bytes = self.read_exact(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    pub fn read_i64_le(&mut self) -> Result<i64> {
        Ok(self.read_u64_le()? as i64)
    }
}

pub fn put_u8(out: &mut Vec<u8>, value: u8) {
    out.push(value);
}

pub fn put_u16_le(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

pub fn put_u32_le(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

pub fn put_u64_le(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

pub fn put_i64_le(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_reports_trailing_bytes() {
        let reader = ByteReader::new(&[1, 2, 3]);

        assert_eq!(Err(AuraError::TrailingBytes(3)), reader.finish());
    }

    #[test]
    fn reader_parses_little_endian_values() {
        let mut bytes = Vec::new();
        put_u8(&mut bytes, 7);
        put_u16_le(&mut bytes, 513);
        put_u32_le(&mut bytes, 1025);
        put_i64_le(&mut bytes, -9);

        let mut reader = ByteReader::new(&bytes);
        assert_eq!(7, reader.read_u8().unwrap());
        assert_eq!(513, reader.read_u16_le().unwrap());
        assert_eq!(1025, reader.read_u32_le().unwrap());
        assert_eq!(-9, reader.read_i64_le().unwrap());
        assert_eq!(Ok(()), reader.finish());
    }
}
