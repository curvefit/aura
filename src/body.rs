use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::bitpack::{
    bitpacked_byte_len, pack_signed_values, pack_unsigned_values, signed_bitpack_width_for_range,
    unpack_signed_values, unpack_unsigned_values, unsigned_bitpack_width,
};
use crate::bytes::{put_i64_le, put_u32_le, put_u8, ByteReader};
use crate::instructions::{GenericStreamInstruction, GenericStreamOp};
use crate::{varint, AuraError, Result};

const HUFFMAN_BOUNDED_ROOT_BITS: u8 = 12;
const HUFFMAN_BOUNDED_MAX_SUBTABLE_BITS: u8 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenericStreamBodyValue {
    I64(Vec<i64>),
    U128(Vec<u128>),
}

pub fn encode_generic_stream_body(
    instruction: &GenericStreamInstruction,
    values: &GenericStreamBodyValue,
) -> Result<Vec<u8>> {
    match (&instruction.op, values) {
        (GenericStreamOp::UuidConstMask { .. }, GenericStreamBodyValue::U128(values)) => {
            encode_uuid_const_mask_body(&instruction.op, values)
        }
        (GenericStreamOp::UuidConstMask { .. }, _) => Err(AuraError::InvalidValue("body type")),
        (_, GenericStreamBodyValue::I64(values)) => encode_i64_op(&instruction.op, values),
        (_, GenericStreamBodyValue::U128(_)) => Err(AuraError::InvalidValue("body type")),
    }
}

pub fn decode_generic_stream_body(
    instruction: &GenericStreamInstruction,
    bytes: &[u8],
    value_count: usize,
) -> Result<GenericStreamBodyValue> {
    match instruction.op {
        GenericStreamOp::UuidConstMask { .. } => {
            let values = decode_uuid_const_mask_body(&instruction.op, bytes, value_count)?;
            Ok(GenericStreamBodyValue::U128(values))
        }
        _ => {
            let mut reader = ByteReader::new(bytes);
            let values = decode_i64_op(&instruction.op, &mut reader, value_count)?;
            reader.finish()?;
            Ok(GenericStreamBodyValue::I64(values))
        }
    }
}

pub(crate) fn try_generic_i64_stream_cursor<'a>(
    instruction: &GenericStreamInstruction,
    bytes: &'a [u8],
    value_count: usize,
) -> Result<Option<GenericI64StreamCursor<'a>>> {
    match instruction.op {
        GenericStreamOp::UuidConstMask { .. } | GenericStreamOp::BitplaneRle { .. } => Ok(None),
        _ => GenericI64StreamCursor::try_new(&instruction.op, bytes, value_count),
    }
}

pub(crate) struct GenericI64StreamCursor<'a> {
    inner: GenericI64StreamCursorKind<'a>,
    remaining: usize,
}

impl<'a> GenericI64StreamCursor<'a> {
    fn try_new(
        op: &GenericStreamOp,
        bytes: &'a [u8],
        value_count: usize,
    ) -> Result<Option<Self>> {
        let mut reader = ByteReader::new(bytes);
        let Some(inner) = GenericI64StreamCursorKind::try_new(op, &mut reader, value_count)? else {
            return Ok(None);
        };
        if !inner.keeps_reader() {
            reader.finish()?;
        }
        Ok(Some(Self {
            inner,
            remaining: value_count,
        }))
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.remaining
    }

    pub(crate) fn dictionary_values(&self) -> Option<&[i64]> {
        self.inner.dictionary_values()
    }

    pub(crate) fn next_i64(&mut self) -> Result<i64> {
        if self.remaining == 0 {
            return Err(AuraError::InvalidValue("stream value count"));
        }
        let index = self.inner.produced_count(self.remaining);
        let value = self.inner.next_i64(index)?;
        self.remaining -= 1;
        Ok(value)
    }

    pub(crate) fn finish(&mut self) -> Result<()> {
        if self.remaining != 0 {
            return Err(AuraError::InvalidValue("stream value count"));
        }
        self.inner.finish()
    }
}

enum GenericI64StreamCursorKind<'a> {
    FixedStep {
        base: i64,
        step: i64,
        value_count: usize,
    },
    BaseBitpack {
        base: i64,
        unit: i64,
        values: BitpackUnsignedCursor<'a>,
        value_count: usize,
    },
    PrevDelta {
        unit: i64,
        current: i64,
        first_pending: bool,
        deltas: BitpackSignedCursor<'a>,
        value_count: usize,
    },
    PrevVarint {
        unit: i64,
        current: i64,
        first_pending: bool,
        reader: ByteReader<'a>,
        value_count: usize,
    },
    PatchedBitpack {
        base: i64,
        unit: i64,
        low_width: u8,
        lows: BitpackUnsignedCursor<'a>,
        indexes: BitpackUnsignedCursor<'a>,
        highs: BitpackUnsignedCursor<'a>,
        next_exception: Option<(usize, u64)>,
        exceptions_remaining: usize,
        value_count: usize,
    },
    Rle {
        base: i64,
        unit: i64,
        run_values: BitpackUnsignedCursor<'a>,
        lengths: ByteReader<'a>,
        run_count: usize,
        run_index: usize,
        current_residual: u64,
        current_remaining: usize,
        value_count: usize,
    },
    Dictionary {
        entries: Vec<i64>,
        codes: BitpackUnsignedCursor<'a>,
        value_count: usize,
    },
    PackedDictionary {
        base: i64,
        unit: i64,
        entries: Vec<u64>,
        codes: BitpackUnsignedCursor<'a>,
        value_count: usize,
    },
    HuffmanSingle {
        value: i64,
        value_count: usize,
    },
    HuffmanBounded {
        table: BoundedHuffmanValueDecodeTable,
        bits: HuffmanTableBitReader<'a>,
        value_count: usize,
    },
    BlockLocal {
        block_size: usize,
        block_count: usize,
        body: &'a [u8],
        offset: usize,
        current: Option<Box<GenericI64StreamCursor<'a>>>,
        produced: usize,
        value_count: usize,
    },
}

impl<'a> GenericI64StreamCursorKind<'a> {
    fn try_new(
        op: &GenericStreamOp,
        reader: &mut ByteReader<'a>,
        value_count: usize,
    ) -> Result<Option<Self>> {
        Ok(Some(match *op {
            GenericStreamOp::FixedStep { base, step } => Self::FixedStep {
                base,
                step,
                value_count,
            },
            GenericStreamOp::BaseBitpack {
                base,
                unit,
                bit_width,
            } => Self::BaseBitpack {
                base,
                unit,
                values: read_bitpack_unsigned_cursor(reader, bit_width, value_count)?,
                value_count,
            },
            GenericStreamOp::PrevDelta {
                base,
                unit,
                bit_width,
            } => Self::PrevDelta {
                unit,
                current: base,
                first_pending: value_count != 0,
                deltas: read_bitpack_signed_cursor(
                    reader,
                    bit_width,
                    value_count.saturating_sub(1),
                )?,
                value_count,
            },
            GenericStreamOp::PrevVarint { base, unit } => {
                validate_unit(unit)?;
                let bytes = reader.read_exact(reader.remaining())?;
                Self::PrevVarint {
                    unit,
                    current: base,
                    first_pending: value_count != 0,
                    reader: ByteReader::new(bytes),
                    value_count,
                }
            }
            GenericStreamOp::PatchedBitpack {
                base,
                unit,
                low_width,
                high_width,
                exception_count,
            } => {
                let exception_count = exception_count as usize;
                if exception_count > value_count {
                    return Err(AuraError::InvalidValue("exception count"));
                }
                let lows = read_bitpack_unsigned_cursor(reader, low_width, value_count)?;
                let mut indexes =
                    read_bitpack_unsigned_cursor(reader, index_width(value_count), exception_count)?;
                let mut highs = read_bitpack_unsigned_cursor(reader, high_width, exception_count)?;
                let next_exception = read_next_patch_exception(&mut indexes, &mut highs)?;
                Self::PatchedBitpack {
                    base,
                    unit,
                    low_width,
                    lows,
                    indexes,
                    highs,
                    next_exception,
                    exceptions_remaining: exception_count,
                    value_count,
                }
            }
            GenericStreamOp::Rle {
                base,
                unit,
                bit_width,
                run_count,
            } => {
                let run_count = run_count as usize;
                let run_values = read_bitpack_unsigned_cursor(reader, bit_width, run_count)?;
                let length_bytes = reader.read_exact(reader.remaining())?;
                Self::Rle {
                    base,
                    unit,
                    run_values,
                    lengths: ByteReader::new(length_bytes),
                    run_count,
                    run_index: 0,
                    current_residual: 0,
                    current_remaining: 0,
                    value_count,
                }
            }
            GenericStreamOp::Dictionary {
                unit,
                entry_count,
                code_width,
            } => {
                let entry_count = entry_count as usize;
                let mut entries = Vec::with_capacity(entry_count);
                for _ in 0..entry_count {
                    entries.push(reconstruct_scaled_value(varint::decode_i64(reader)?, unit)?);
                }
                let codes = read_bitpack_unsigned_cursor(reader, code_width, value_count)?;
                Self::Dictionary {
                    entries,
                    codes,
                    value_count,
                }
            }
            GenericStreamOp::PackedDictionary {
                base,
                unit,
                entry_count,
                entry_width,
                code_width,
            } => {
                let entries = read_bitpacked_unsigned(reader, entry_width, entry_count as usize)?;
                let codes = read_bitpack_unsigned_cursor(reader, code_width, value_count)?;
                Self::PackedDictionary {
                    base,
                    unit,
                    entries,
                    codes,
                    value_count,
                }
            }
            GenericStreamOp::HuffmanDictionary {
                base,
                unit,
                entry_count,
                entry_width,
                ref code_lengths,
            } => {
                let Some(cursor) = try_huffman_cursor(
                    base,
                    unit,
                    entry_count,
                    entry_width,
                    code_lengths,
                    reader,
                    value_count,
                )?
                else {
                    return Ok(None);
                };
                cursor
            }
            GenericStreamOp::BlockLocal {
                block_size,
                mode_count,
            } => {
                let body = reader.read_exact(reader.remaining())?;
                let Some(()) =
                    validate_block_local_cursor_body(block_size, mode_count, body, value_count)?
                else {
                    return Ok(None);
                };
                Self::BlockLocal {
                    block_size: usize::from(block_size),
                    block_count: mode_count as usize,
                    body,
                    offset: 0,
                    current: None,
                    produced: 0,
                    value_count,
                }
            }
            GenericStreamOp::BitplaneRle { .. } | GenericStreamOp::UuidConstMask { .. } => {
                return Ok(None);
            }
        }))
    }

    const fn keeps_reader(&self) -> bool {
        matches!(self, Self::PrevVarint { .. } | Self::Rle { .. })
    }

    fn produced_count(&self, remaining: usize) -> usize {
        self.value_count().saturating_sub(remaining)
    }

    const fn value_count(&self) -> usize {
        match self {
            Self::FixedStep { value_count, .. }
            | Self::BaseBitpack { value_count, .. }
            | Self::PrevDelta { value_count, .. }
            | Self::PrevVarint { value_count, .. }
            | Self::PatchedBitpack { value_count, .. }
            | Self::Rle { value_count, .. }
            | Self::Dictionary { value_count, .. }
            | Self::PackedDictionary { value_count, .. }
            | Self::HuffmanSingle { value_count, .. }
            | Self::HuffmanBounded { value_count, .. }
            | Self::BlockLocal { value_count, .. } => *value_count,
        }
    }

    fn dictionary_values(&self) -> Option<&[i64]> {
        match self {
            Self::Dictionary { entries, .. } => Some(entries.as_slice()),
            _ => None,
        }
    }

    fn next_i64(&mut self, index: usize) -> Result<i64> {
        match self {
            Self::FixedStep { base, step, .. } => fixed_step_value(*base, *step, index),
            Self::BaseBitpack {
                base,
                unit,
                values,
                ..
            } => reconstruct_unsigned_offset(*base, *unit, values.next()?),
            Self::PrevDelta {
                unit,
                current,
                first_pending,
                deltas,
                ..
            } => {
                if *first_pending {
                    *first_pending = false;
                    return Ok(*current);
                }
                *current = reconstruct_signed_delta(*current, *unit, deltas.next()?)?;
                Ok(*current)
            }
            Self::PrevVarint {
                unit,
                current,
                first_pending,
                reader,
                ..
            } => {
                if *first_pending {
                    *first_pending = false;
                    return Ok(*current);
                }
                *current = reconstruct_signed_delta(*current, *unit, varint::decode_i64(reader)?)?;
                Ok(*current)
            }
            Self::PatchedBitpack {
                base,
                unit,
                low_width,
                lows,
                indexes,
                highs,
                next_exception,
                exceptions_remaining,
                ..
            } => {
                let mut residual = lows.next()?;
                if let Some((exception_index, high)) = *next_exception {
                    match exception_index.cmp(&index) {
                        Ordering::Less => return Err(AuraError::InvalidValue("exception index")),
                        Ordering::Equal => {
                            let shifted = if *low_width == 64 {
                                if high != 0 {
                                    return Err(AuraError::InvalidValue("exception high"));
                                }
                                0
                            } else {
                                high.checked_shl(u32::from(*low_width))
                                    .ok_or(AuraError::InvalidValue("exception high"))?
                            };
                            residual |= shifted;
                            *exceptions_remaining = exceptions_remaining.saturating_sub(1);
                            *next_exception = read_next_patch_exception(indexes, highs)?;
                        }
                        Ordering::Greater => {}
                    }
                }
                reconstruct_unsigned_offset(*base, *unit, residual)
            }
            Self::Rle {
                base,
                unit,
                run_values,
                lengths,
                run_count,
                run_index,
                current_residual,
                current_remaining,
                ..
            } => {
                if *current_remaining == 0 {
                    if *run_index >= *run_count {
                        return Err(AuraError::InvalidValue("run length"));
                    }
                    *current_residual = run_values.next()?;
                    *current_remaining = usize::try_from(varint::decode_u64(lengths)?)
                        .map_err(|_| AuraError::InvalidValue("run length"))?;
                    if *current_remaining == 0 {
                        return Err(AuraError::InvalidValue("run length"));
                    }
                    *run_index += 1;
                }
                *current_remaining -= 1;
                reconstruct_unsigned_offset(*base, *unit, *current_residual)
            }
            Self::Dictionary { entries, codes, .. } => {
                let code =
                    usize::try_from(codes.next()?).map_err(|_| AuraError::InvalidValue("dictionary code"))?;
                entries
                    .get(code)
                    .copied()
                    .ok_or(AuraError::InvalidValue("dictionary code"))
            }
            Self::PackedDictionary {
                base,
                unit,
                entries,
                codes,
                ..
            } => {
                let code =
                    usize::try_from(codes.next()?).map_err(|_| AuraError::InvalidValue("dictionary code"))?;
                let entry = *entries
                    .get(code)
                    .ok_or(AuraError::InvalidValue("dictionary code"))?;
                reconstruct_unsigned_offset(*base, *unit, entry)
            }
            Self::HuffmanSingle { value, .. } => Ok(*value),
            Self::HuffmanBounded { table, bits, .. } => decode_one_huffman_bounded_value(table, bits),
            Self::BlockLocal {
                block_size,
                block_count,
                body,
                offset,
                current,
                produced,
                value_count,
            } => {
                if current.as_ref().is_none_or(|cursor| cursor.remaining() == 0) {
                    if let Some(cursor) = current {
                        cursor.finish()?;
                    }
                    if *produced >= *value_count {
                        return Err(AuraError::InvalidValue("stream value count"));
                    }
                    let block_index = *produced / *block_size;
                    if block_index >= *block_count {
                        return Err(AuraError::InvalidValue("block count"));
                    }
                    let remaining = *value_count - *produced;
                    let count = remaining.min(*block_size);
                    let (next, next_offset) = read_block_local_cursor(*body, *offset, count)?;
                    *offset = next_offset;
                    *current = Some(Box::new(next));
                }
                let value = current
                    .as_mut()
                    .ok_or(AuraError::InvalidValue("block local body"))?
                    .next_i64()?;
                *produced += 1;
                Ok(value)
            }
        }
    }

    fn finish(&mut self) -> Result<()> {
        match self {
            Self::PrevVarint { reader, .. } => {
                if reader.remaining() == 0 {
                    Ok(())
                } else {
                    Err(AuraError::TrailingBytes(reader.remaining()))
                }
            }
            Self::PatchedBitpack {
                next_exception,
                exceptions_remaining,
                ..
            } => {
                if *exceptions_remaining == 0 && next_exception.is_none() {
                    Ok(())
                } else {
                    Err(AuraError::InvalidValue("exception count"))
                }
            }
            Self::Rle {
                lengths,
                run_count,
                run_index,
                current_remaining,
                ..
            } => {
                if *current_remaining != 0 || *run_index != *run_count {
                    return Err(AuraError::InvalidValue("run length"));
                }
                if lengths.remaining() == 0 {
                    Ok(())
                } else {
                    Err(AuraError::TrailingBytes(lengths.remaining()))
                }
            }
            Self::BlockLocal {
                body,
                offset,
                current,
                ..
            } => {
                if let Some(cursor) = current {
                    cursor.finish()?;
                }
                if *offset == body.len() {
                    Ok(())
                } else {
                    Err(AuraError::TrailingBytes(body.len() - *offset))
                }
            }
            _ => Ok(()),
        }
    }
}

struct BitpackUnsignedCursor<'a> {
    bytes: &'a [u8],
    bit_width: u8,
    value_count: usize,
    index: usize,
    byte_index: usize,
    buffer: u128,
    buffered_bits: u8,
}

struct BitpackSignedCursor<'a> {
    unsigned: BitpackUnsignedCursor<'a>,
}

impl<'a> BitpackUnsignedCursor<'a> {
    fn new(bytes: &'a [u8], bit_width: u8, value_count: usize) -> Result<Self> {
        low_mask(bit_width)?;
        let expected_len = bitpacked_byte_len(value_count as u64, bit_width) as usize;
        if bytes.len() != expected_len {
            return Err(AuraError::InvalidValue("bitpacked length"));
        }
        Ok(Self {
            bytes,
            bit_width,
            value_count,
            index: 0,
            byte_index: 0,
            buffer: 0,
            buffered_bits: 0,
        })
    }

    fn next(&mut self) -> Result<u64> {
        if self.index >= self.value_count {
            return Err(AuraError::InvalidValue("bitpacked length"));
        }
        self.index += 1;
        if self.bit_width == 0 {
            return Ok(0);
        }
        let mask = if self.bit_width == 64 {
            u128::from(u64::MAX)
        } else {
            (1u128 << self.bit_width) - 1
        };
        while self.buffered_bits < self.bit_width {
            let byte = self.bytes.get(self.byte_index).ok_or(AuraError::UnexpectedEof)?;
            self.buffer |= u128::from(*byte) << self.buffered_bits;
            self.buffered_bits += 8;
            self.byte_index += 1;
        }
        let value = (self.buffer & mask) as u64;
        self.buffer >>= self.bit_width;
        self.buffered_bits -= self.bit_width;
        Ok(value)
    }
}

impl<'a> BitpackSignedCursor<'a> {
    fn new(bytes: &'a [u8], bit_width: u8, value_count: usize) -> Result<Self> {
        Ok(Self {
            unsigned: BitpackUnsignedCursor::new(bytes, bit_width, value_count)?,
        })
    }

    fn next(&mut self) -> Result<i64> {
        let raw = self.unsigned.next()?;
        sign_extend_bitpacked(raw, self.unsigned.bit_width)
    }
}

fn read_bitpack_unsigned_cursor<'a>(
    reader: &mut ByteReader<'a>,
    bit_width: u8,
    value_count: usize,
) -> Result<BitpackUnsignedCursor<'a>> {
    let byte_len = bitpacked_byte_len(value_count as u64, bit_width) as usize;
    BitpackUnsignedCursor::new(reader.read_exact(byte_len)?, bit_width, value_count)
}

fn read_bitpack_signed_cursor<'a>(
    reader: &mut ByteReader<'a>,
    bit_width: u8,
    value_count: usize,
) -> Result<BitpackSignedCursor<'a>> {
    let byte_len = bitpacked_byte_len(value_count as u64, bit_width) as usize;
    BitpackSignedCursor::new(reader.read_exact(byte_len)?, bit_width, value_count)
}

fn sign_extend_bitpacked(raw: u64, bit_width: u8) -> Result<i64> {
    match bit_width {
        0 => Ok(0),
        1..=63 => {
            let sign_bit = 1u64 << (bit_width - 1);
            let value = if raw & sign_bit == 0 {
                i128::from(raw)
            } else {
                i128::from(raw) - (1i128 << bit_width)
            };
            i64::try_from(value).map_err(|_| AuraError::InvalidValue("bitpacked value"))
        }
        64 => Ok(raw as i64),
        _ => Err(AuraError::InvalidValue("bit width")),
    }
}

fn read_next_patch_exception(
    indexes: &mut BitpackUnsignedCursor<'_>,
    highs: &mut BitpackUnsignedCursor<'_>,
) -> Result<Option<(usize, u64)>> {
    if indexes.index >= indexes.value_count {
        return Ok(None);
    }
    let index = usize::try_from(indexes.next()?).map_err(|_| AuraError::InvalidValue("index"))?;
    let high = highs.next()?;
    Ok(Some((index, high)))
}

fn try_huffman_cursor<'a>(
    base: i64,
    unit: i64,
    entry_count: u32,
    entry_width: u8,
    code_lengths: &[u8],
    reader: &mut ByteReader<'a>,
    value_count: usize,
) -> Result<Option<GenericI64StreamCursorKind<'a>>> {
    let entry_count = entry_count as usize;
    if code_lengths.len() != entry_count {
        return Err(AuraError::InvalidValue("huffman code lengths"));
    }
    let entries = read_bitpacked_unsigned(reader, entry_width, entry_count)?;
    if entry_count == 1 && matches!(code_lengths, [0] | [1]) {
        let entry = *entries
            .first()
            .ok_or(AuraError::InvalidValue("dictionary entry count"))?;
        let value = reconstruct_unsigned_offset(base, unit, entry)?;
        let code_bytes = reader.read_exact(reader.remaining())?;
        if !code_bytes.is_empty() && code_lengths == [0] {
            return Err(AuraError::InvalidValue("huffman code"));
        }
        return Ok(Some(GenericI64StreamCursorKind::HuffmanSingle {
            value,
            value_count,
        }));
    }

    let decoded_entries = entries
        .iter()
        .map(|entry| reconstruct_unsigned_offset(base, unit, *entry))
        .collect::<Result<Vec<_>>>()?;
    let canonical_codes = canonical_huffman_codes(code_lengths)?;
    let max_len = canonical_codes
        .iter()
        .filter_map(|code| code.map(|code| code.bit_len))
        .max()
        .ok_or(AuraError::InvalidValue("huffman code lengths"))?;
    let code_bytes = reader.read_exact(reader.remaining())?;
    let Ok(table) = huffman_bounded_value_decode_table(
        &canonical_codes,
        &decoded_entries,
        max_len.min(HUFFMAN_BOUNDED_ROOT_BITS),
    ) else {
        return Ok(None);
    };
    Ok(Some(GenericI64StreamCursorKind::HuffmanBounded {
        table,
        bits: HuffmanTableBitReader::new(code_bytes),
        value_count,
    }))
}

fn decode_one_huffman_bounded_value(
    table: &BoundedHuffmanValueDecodeTable,
    bit_reader: &mut HuffmanTableBitReader<'_>,
) -> Result<i64> {
    let key = bit_reader.peek_bits(table.root_bits)? as usize;
    let entry = match table
        .root
        .get(key)
        .ok_or(AuraError::InvalidValue("huffman code"))?
    {
        BoundedHuffmanRootSlot::Empty => return Err(AuraError::InvalidValue("huffman code")),
        BoundedHuffmanRootSlot::Leaf(entry) => *entry,
        BoundedHuffmanRootSlot::Subtable(subtable_index) => {
            let subtable = table
                .subtables
                .get(*subtable_index)
                .ok_or(AuraError::InvalidValue("huffman code"))?;
            let total_bits = table
                .root_bits
                .checked_add(subtable.bits)
                .ok_or(AuraError::InvalidValue("huffman code"))?;
            let suffix_mask = low_mask(subtable.bits)?;
            let suffix = usize::try_from(bit_reader.peek_bits(total_bits)? & suffix_mask)
                .map_err(|_| AuraError::InvalidValue("huffman code"))?;
            subtable
                .slots
                .get(suffix)
                .and_then(|entry| *entry)
                .ok_or(AuraError::InvalidValue("huffman code"))?
        }
    };
    bit_reader.consume_bits(entry.bit_len)?;
    Ok(entry.value)
}

fn validate_block_local_cursor_body(
    block_size: u16,
    mode_count: u32,
    body: &[u8],
    value_count: usize,
) -> Result<Option<()>> {
    let block_size = usize::from(block_size);
    if block_size == 0 {
        return Err(AuraError::InvalidValue("block size"));
    }
    let block_count = value_count.div_ceil(block_size);
    if block_count != mode_count as usize {
        return Err(AuraError::InvalidValue("block count"));
    }
    let mut offset = 0usize;
    for block_index in 0..block_count {
        let count = (value_count - block_index * block_size).min(block_size);
        let mut reader = ByteReader::new(
            body.get(offset..)
                .ok_or(AuraError::InvalidValue("block local body"))?,
        );
        let op = decode_local_op_header(&mut reader)?;
        let header_len = local_op_header_len(&op);
        let Some(body_len) = local_op_body_len(&op, count, reader.read_exact(reader.remaining())?)?
        else {
            return Ok(None);
        };
        offset = offset
            .checked_add(header_len)
            .and_then(|value| value.checked_add(body_len))
            .ok_or(AuraError::InvalidValue("block local body"))?;
        if offset > body.len() {
            return Err(AuraError::UnexpectedEof);
        }
    }
    if offset != body.len() {
        return Err(AuraError::TrailingBytes(body.len() - offset));
    }
    Ok(Some(()))
}

fn read_block_local_cursor<'a>(
    body: &'a [u8],
    offset: usize,
    value_count: usize,
) -> Result<(GenericI64StreamCursor<'a>, usize)> {
    let mut reader = ByteReader::new(
        body.get(offset..)
            .ok_or(AuraError::InvalidValue("block local body"))?,
    );
    let op = decode_local_op_header(&mut reader)?;
    let header_len = local_op_header_len(&op);
    let body_start = offset
        .checked_add(header_len)
        .ok_or(AuraError::InvalidValue("block local body"))?;
    let Some(body_len) = local_op_body_len(&op, value_count, reader.read_exact(reader.remaining())?)?
    else {
        return Err(AuraError::InvalidValue("block local mode"));
    };
    let body_end = body_start
        .checked_add(body_len)
        .ok_or(AuraError::InvalidValue("block local body"))?;
    let local_body = body
        .get(body_start..body_end)
        .ok_or(AuraError::UnexpectedEof)?;
    let cursor = GenericI64StreamCursor::try_new(&op, local_body, value_count)?
        .ok_or(AuraError::InvalidValue("block local mode"))?;
    Ok((cursor, body_end))
}

fn local_op_body_len(op: &GenericStreamOp, value_count: usize, bytes: &[u8]) -> Result<Option<usize>> {
    let len = match *op {
        GenericStreamOp::FixedStep { .. } => 0,
        GenericStreamOp::BaseBitpack { bit_width, .. } => {
            bitpacked_byte_len(value_count as u64, bit_width) as usize
        }
        GenericStreamOp::PrevDelta { bit_width, .. } => {
            bitpacked_byte_len(value_count.saturating_sub(1) as u64, bit_width) as usize
        }
        GenericStreamOp::PatchedBitpack {
            low_width,
            high_width,
            exception_count,
            ..
        } => {
            let exception_count = u64::from(exception_count);
            let lows = bitpacked_byte_len(value_count as u64, low_width);
            let indexes = bitpacked_byte_len(exception_count, index_width(value_count));
            let highs = bitpacked_byte_len(exception_count, high_width);
            usize::try_from(lows + indexes + highs)
                .map_err(|_| AuraError::InvalidValue("block local body"))?
        }
        GenericStreamOp::Rle {
            bit_width,
            run_count,
            ..
        } => {
            let run_count_usize = run_count as usize;
            let run_values_len = bitpacked_byte_len(u64::from(run_count), bit_width) as usize;
            let varint_bytes = bytes
                .get(run_values_len..)
                .ok_or(AuraError::UnexpectedEof)?;
            let mut reader = ByteReader::new(varint_bytes);
            let mut total = 0usize;
            for _ in 0..run_count_usize {
                let len = usize::try_from(varint::decode_u64(&mut reader)?)
                    .map_err(|_| AuraError::InvalidValue("run length"))?;
                if len == 0 {
                    return Err(AuraError::InvalidValue("run length"));
                }
                total = total
                    .checked_add(len)
                    .ok_or(AuraError::InvalidValue("run length"))?;
            }
            if total != value_count {
                return Err(AuraError::InvalidValue("run length"));
            }
            run_values_len
                .checked_add(reader.offset())
                .ok_or(AuraError::InvalidValue("block local body"))?
        }
        _ => return Ok(None),
    };
    if len > bytes.len() {
        return Err(AuraError::UnexpectedEof);
    }
    Ok(Some(len))
}

fn encode_i64_op(op: &GenericStreamOp, values: &[i64]) -> Result<Vec<u8>> {
    match *op {
        GenericStreamOp::FixedStep { base, step } => encode_fixed_step(base, step, values),
        GenericStreamOp::BaseBitpack {
            base,
            unit,
            bit_width,
        } => encode_base_bitpack(base, unit, bit_width, values),
        GenericStreamOp::PrevDelta {
            base,
            unit,
            bit_width,
        } => encode_prev_delta(base, unit, bit_width, values),
        GenericStreamOp::PrevVarint { base, unit } => encode_prev_varint(base, unit, values),
        GenericStreamOp::BlockLocal {
            block_size,
            mode_count,
        } => encode_block_local(block_size, mode_count, values),
        GenericStreamOp::PatchedBitpack {
            base,
            unit,
            low_width,
            high_width,
            exception_count,
        } => encode_patched_bitpack(base, unit, low_width, high_width, exception_count, values),
        GenericStreamOp::Rle {
            base,
            unit,
            bit_width,
            run_count,
        } => encode_rle(base, unit, bit_width, run_count, values),
        GenericStreamOp::BitplaneRle {
            base,
            unit,
            bit_width,
        } => encode_bitplane_rle(base, unit, bit_width, values),
        GenericStreamOp::Dictionary {
            unit,
            entry_count,
            code_width,
        } => encode_dictionary(unit, entry_count, code_width, values),
        GenericStreamOp::PackedDictionary {
            base,
            unit,
            entry_count,
            entry_width,
            code_width,
        } => encode_packed_dictionary(base, unit, entry_count, entry_width, code_width, values),
        GenericStreamOp::HuffmanDictionary {
            base,
            unit,
            entry_count,
            entry_width,
            ref code_lengths,
        } => encode_huffman_dictionary(base, unit, entry_count, entry_width, code_lengths, values),
        GenericStreamOp::UuidConstMask { .. } => Err(AuraError::InvalidValue("body type")),
    }
}

fn decode_i64_op(
    op: &GenericStreamOp,
    reader: &mut ByteReader<'_>,
    value_count: usize,
) -> Result<Vec<i64>> {
    match *op {
        GenericStreamOp::FixedStep { base, step } => decode_fixed_step(base, step, value_count),
        GenericStreamOp::BaseBitpack {
            base,
            unit,
            bit_width,
        } => decode_base_bitpack(base, unit, bit_width, reader, value_count),
        GenericStreamOp::PrevDelta {
            base,
            unit,
            bit_width,
        } => decode_prev_delta(base, unit, bit_width, reader, value_count),
        GenericStreamOp::PrevVarint { base, unit } => {
            decode_prev_varint(base, unit, reader, value_count)
        }
        GenericStreamOp::BlockLocal {
            block_size,
            mode_count,
        } => decode_block_local(block_size, mode_count, reader, value_count),
        GenericStreamOp::PatchedBitpack {
            base,
            unit,
            low_width,
            high_width,
            exception_count,
        } => decode_patched_bitpack(
            base,
            unit,
            low_width,
            high_width,
            exception_count,
            reader,
            value_count,
        ),
        GenericStreamOp::Rle {
            base,
            unit,
            bit_width,
            run_count,
        } => decode_rle(base, unit, bit_width, run_count, reader, value_count),
        GenericStreamOp::BitplaneRle {
            base,
            unit,
            bit_width,
        } => decode_bitplane_rle(base, unit, bit_width, reader, value_count),
        GenericStreamOp::Dictionary {
            unit,
            entry_count,
            code_width,
        } => decode_dictionary(unit, entry_count, code_width, reader, value_count),
        GenericStreamOp::PackedDictionary {
            base,
            unit,
            entry_count,
            entry_width,
            code_width,
        } => decode_packed_dictionary(
            base,
            unit,
            entry_count,
            entry_width,
            code_width,
            reader,
            value_count,
        ),
        GenericStreamOp::HuffmanDictionary {
            base,
            unit,
            entry_count,
            entry_width,
            ref code_lengths,
        } => decode_huffman_dictionary(
            base,
            unit,
            entry_count,
            entry_width,
            code_lengths,
            reader,
            value_count,
        ),
        GenericStreamOp::UuidConstMask { .. } => Err(AuraError::InvalidValue("body type")),
    }
}

fn encode_fixed_step(base: i64, step: i64, values: &[i64]) -> Result<Vec<u8>> {
    for (index, value) in values.iter().enumerate() {
        if *value != fixed_step_value(base, step, index)? {
            return Err(AuraError::InvalidValue("fixed step body"));
        }
    }
    Ok(Vec::new())
}

fn decode_fixed_step(base: i64, step: i64, value_count: usize) -> Result<Vec<i64>> {
    (0..value_count)
        .map(|index| fixed_step_value(base, step, index))
        .collect()
}

fn encode_base_bitpack(base: i64, unit: i64, bit_width: u8, values: &[i64]) -> Result<Vec<u8>> {
    let scaled = values
        .iter()
        .map(|value| scaled_unsigned_offset(*value, base, unit))
        .collect::<Result<Vec<_>>>()?;
    pack_unsigned_values(&scaled, bit_width)
}

fn decode_base_bitpack(
    base: i64,
    unit: i64,
    bit_width: u8,
    reader: &mut ByteReader<'_>,
    value_count: usize,
) -> Result<Vec<i64>> {
    let scaled = read_bitpacked_unsigned(reader, bit_width, value_count)?;
    scaled
        .into_iter()
        .map(|value| reconstruct_unsigned_offset(base, unit, value))
        .collect()
}

fn encode_prev_delta(base: i64, unit: i64, bit_width: u8, values: &[i64]) -> Result<Vec<u8>> {
    if values.is_empty() {
        return Ok(Vec::new());
    }
    if values[0] != base {
        return Err(AuraError::InvalidValue("previous delta base"));
    }
    let deltas = values
        .windows(2)
        .map(|pair| scaled_signed_delta(pair[1], pair[0], unit))
        .collect::<Result<Vec<_>>>()?;
    pack_signed_values(&deltas, bit_width)
}

fn decode_prev_delta(
    base: i64,
    unit: i64,
    bit_width: u8,
    reader: &mut ByteReader<'_>,
    value_count: usize,
) -> Result<Vec<i64>> {
    if value_count == 0 {
        return Ok(Vec::new());
    }
    let deltas = read_bitpacked_signed(reader, bit_width, value_count - 1)?;
    let mut values = Vec::with_capacity(value_count);
    values.push(base);
    for delta in deltas {
        let previous = *values
            .last()
            .ok_or(AuraError::InvalidValue("previous delta body"))?;
        values.push(reconstruct_signed_delta(previous, unit, delta)?);
    }
    Ok(values)
}

fn encode_prev_varint(base: i64, unit: i64, values: &[i64]) -> Result<Vec<u8>> {
    validate_unit(unit)?;
    if values.is_empty() {
        return Ok(Vec::new());
    }
    if values[0] != base {
        return Err(AuraError::InvalidValue("previous varint base"));
    }
    let mut out = Vec::new();
    for pair in values.windows(2) {
        varint::encode_i64(scaled_signed_delta(pair[1], pair[0], unit)?, &mut out);
    }
    Ok(out)
}

fn decode_prev_varint(
    base: i64,
    unit: i64,
    reader: &mut ByteReader<'_>,
    value_count: usize,
) -> Result<Vec<i64>> {
    validate_unit(unit)?;
    if value_count == 0 {
        return Ok(Vec::new());
    }
    let mut values = Vec::with_capacity(value_count);
    values.push(base);
    for _ in 1..value_count {
        let delta = varint::decode_i64(reader)?;
        let previous = *values
            .last()
            .ok_or(AuraError::InvalidValue("previous varint body"))?;
        values.push(reconstruct_signed_delta(previous, unit, delta)?);
    }
    Ok(values)
}

fn encode_patched_bitpack(
    base: i64,
    unit: i64,
    low_width: u8,
    high_width: u8,
    exception_count: u32,
    values: &[i64],
) -> Result<Vec<u8>> {
    let residuals = values
        .iter()
        .map(|value| scaled_unsigned_offset(*value, base, unit))
        .collect::<Result<Vec<_>>>()?;
    let mask = low_mask(low_width)?;
    let mut lows = Vec::with_capacity(residuals.len());
    let mut indexes = Vec::new();
    let mut highs = Vec::new();
    for (index, residual) in residuals.iter().enumerate() {
        lows.push(*residual & mask);
        let high = if low_width == 64 {
            0
        } else {
            *residual >> low_width
        };
        if high != 0 {
            indexes.push(u64::try_from(index).map_err(|_| AuraError::InvalidValue("index"))?);
            highs.push(high);
        }
    }
    if indexes.len() != exception_count as usize {
        return Err(AuraError::InvalidValue("exception count"));
    }

    let index_width = index_width(values.len());
    let mut out = pack_unsigned_values(&lows, low_width)?;
    out.extend(pack_unsigned_values(&indexes, index_width)?);
    out.extend(pack_unsigned_values(&highs, high_width)?);
    Ok(out)
}

fn decode_patched_bitpack(
    base: i64,
    unit: i64,
    low_width: u8,
    high_width: u8,
    exception_count: u32,
    reader: &mut ByteReader<'_>,
    value_count: usize,
) -> Result<Vec<i64>> {
    let mut residuals = read_bitpacked_unsigned(reader, low_width, value_count)?;
    let exception_count = exception_count as usize;
    if exception_count > value_count {
        return Err(AuraError::InvalidValue("exception count"));
    }
    let indexes = read_bitpacked_unsigned(reader, index_width(value_count), exception_count)?;
    let highs = read_bitpacked_unsigned(reader, high_width, exception_count)?;
    for (index, high) in indexes.into_iter().zip(highs) {
        let index = usize::try_from(index).map_err(|_| AuraError::InvalidValue("index"))?;
        let value = residuals
            .get_mut(index)
            .ok_or(AuraError::InvalidValue("exception index"))?;
        let shifted = if low_width == 64 {
            if high != 0 {
                return Err(AuraError::InvalidValue("exception high"));
            }
            0
        } else {
            high.checked_shl(u32::from(low_width))
                .ok_or(AuraError::InvalidValue("exception high"))?
        };
        *value |= shifted;
    }
    residuals
        .into_iter()
        .map(|value| reconstruct_unsigned_offset(base, unit, value))
        .collect()
}

fn encode_rle(
    base: i64,
    unit: i64,
    bit_width: u8,
    run_count: u32,
    values: &[i64],
) -> Result<Vec<u8>> {
    let residuals = values
        .iter()
        .map(|value| scaled_unsigned_offset(*value, base, unit))
        .collect::<Result<Vec<_>>>()?;
    let runs = runs_for(&residuals);
    if runs.len() != run_count as usize {
        return Err(AuraError::InvalidValue("run count"));
    }
    let mut run_values = Vec::with_capacity(runs.len());
    let mut run_lengths = Vec::with_capacity(runs.len());
    for (value, len) in runs {
        run_values.push(value);
        run_lengths.push(len);
    }

    let mut out = pack_unsigned_values(&run_values, bit_width)?;
    for len in run_lengths {
        varint::encode_u64(len as u64, &mut out);
    }
    Ok(out)
}

fn decode_rle(
    base: i64,
    unit: i64,
    bit_width: u8,
    run_count: u32,
    reader: &mut ByteReader<'_>,
    value_count: usize,
) -> Result<Vec<i64>> {
    let run_count = run_count as usize;
    let run_values = read_bitpacked_unsigned(reader, bit_width, run_count)?;
    let mut residuals = Vec::with_capacity(value_count);
    for value in run_values {
        let len = usize::try_from(varint::decode_u64(reader)?)
            .map_err(|_| AuraError::InvalidValue("run length"))?;
        if len == 0 || residuals.len().saturating_add(len) > value_count {
            return Err(AuraError::InvalidValue("run length"));
        }
        residuals.extend(std::iter::repeat_n(value, len));
    }
    if residuals.len() != value_count {
        return Err(AuraError::InvalidValue("run length"));
    }
    residuals
        .into_iter()
        .map(|value| reconstruct_unsigned_offset(base, unit, value))
        .collect()
}

fn encode_bitplane_rle(base: i64, unit: i64, bit_width: u8, values: &[i64]) -> Result<Vec<u8>> {
    let residuals = values
        .iter()
        .map(|value| scaled_unsigned_offset(*value, base, unit))
        .collect::<Result<Vec<_>>>()?;
    for residual in &residuals {
        ensure_unsigned_width(*residual, bit_width, "bitplane value")?;
    }
    let mut out = Vec::new();
    if residuals.is_empty() {
        return Ok(out);
    }
    for bit in 0..bit_width {
        let bits = residuals
            .iter()
            .map(|value| ((*value >> bit) & 1) as u8)
            .collect::<Vec<_>>();
        let runs = runs_for(&bits);
        put_u8(&mut out, bits[0]);
        put_u32_le(
            &mut out,
            u32::try_from(runs.len()).map_err(|_| AuraError::InvalidValue("run count"))?,
        );
        for (_, len) in runs {
            varint::encode_u64(len as u64, &mut out);
        }
    }
    Ok(out)
}

fn decode_bitplane_rle(
    base: i64,
    unit: i64,
    bit_width: u8,
    reader: &mut ByteReader<'_>,
    value_count: usize,
) -> Result<Vec<i64>> {
    let mut residuals = vec![0u64; value_count];
    if value_count == 0 {
        return Ok(Vec::new());
    }
    for bit in 0..bit_width {
        let start = match reader.read_u8()? {
            0 => 0u8,
            1 => 1u8,
            _ => return Err(AuraError::InvalidValue("bitplane run bit")),
        };
        let run_count = reader.read_u32_le()? as usize;
        let mut bit_value = start;
        let mut index = 0usize;
        for _ in 0..run_count {
            let len = usize::try_from(varint::decode_u64(reader)?)
                .map_err(|_| AuraError::InvalidValue("run length"))?;
            if len == 0 || index.saturating_add(len) > value_count {
                return Err(AuraError::InvalidValue("run length"));
            }
            if bit_value == 1 {
                for residual in &mut residuals[index..index + len] {
                    *residual |= 1u64 << bit;
                }
            }
            index += len;
            bit_value ^= 1;
        }
        if index != value_count {
            return Err(AuraError::InvalidValue("run length"));
        }
    }
    residuals
        .into_iter()
        .map(|value| reconstruct_unsigned_offset(base, unit, value))
        .collect()
}

fn encode_dictionary(
    unit: i64,
    entry_count: u32,
    code_width: u8,
    values: &[i64],
) -> Result<Vec<u8>> {
    let scaled_values = values
        .iter()
        .map(|value| scaled_signed_value(*value, unit))
        .collect::<Result<Vec<_>>>()?;
    let mut entries = scaled_values.clone();
    entries.sort_unstable();
    entries.dedup();
    if entries.len() != entry_count as usize {
        return Err(AuraError::InvalidValue("dictionary entry count"));
    }
    let entry_indexes = entries
        .iter()
        .enumerate()
        .map(|(index, value)| (*value, index as u64))
        .collect::<BTreeMap<_, _>>();
    let codes = scaled_values
        .iter()
        .map(|value| {
            entry_indexes
                .get(value)
                .copied()
                .ok_or(AuraError::InvalidValue("dictionary code"))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut out = Vec::new();
    for entry in entries {
        varint::encode_i64(entry, &mut out);
    }
    out.extend(pack_unsigned_values(&codes, code_width)?);
    Ok(out)
}

fn decode_dictionary(
    unit: i64,
    entry_count: u32,
    code_width: u8,
    reader: &mut ByteReader<'_>,
    value_count: usize,
) -> Result<Vec<i64>> {
    let entry_count = entry_count as usize;
    let mut entries = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        entries.push(varint::decode_i64(reader)?);
    }
    let codes = read_bitpacked_unsigned(reader, code_width, value_count)?;
    codes
        .into_iter()
        .map(|code| {
            let entry = *entries
                .get(
                    usize::try_from(code)
                        .map_err(|_| AuraError::InvalidValue("dictionary code"))?,
                )
                .ok_or(AuraError::InvalidValue("dictionary code"))?;
            reconstruct_scaled_value(entry, unit)
        })
        .collect()
}

fn encode_packed_dictionary(
    base: i64,
    unit: i64,
    entry_count: u32,
    entry_width: u8,
    code_width: u8,
    values: &[i64],
) -> Result<Vec<u8>> {
    let scaled_values = values
        .iter()
        .map(|value| scaled_unsigned_offset(*value, base, unit))
        .collect::<Result<Vec<_>>>()?;
    let mut entries = scaled_values.clone();
    entries.sort_unstable();
    entries.dedup();
    if entries.len() != entry_count as usize {
        return Err(AuraError::InvalidValue("dictionary entry count"));
    }
    for entry in &entries {
        ensure_unsigned_width(*entry, entry_width, "dictionary entry")?;
    }
    let entry_indexes = entries
        .iter()
        .enumerate()
        .map(|(index, value)| (*value, index as u64))
        .collect::<BTreeMap<_, _>>();
    let codes = scaled_values
        .iter()
        .map(|value| {
            entry_indexes
                .get(value)
                .copied()
                .ok_or(AuraError::InvalidValue("dictionary code"))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut out = pack_unsigned_values(&entries, entry_width)?;
    out.extend(pack_unsigned_values(&codes, code_width)?);
    Ok(out)
}

fn decode_packed_dictionary(
    base: i64,
    unit: i64,
    entry_count: u32,
    entry_width: u8,
    code_width: u8,
    reader: &mut ByteReader<'_>,
    value_count: usize,
) -> Result<Vec<i64>> {
    let entry_count = entry_count as usize;
    let entries = read_bitpacked_unsigned(reader, entry_width, entry_count)?;
    let codes = read_bitpacked_unsigned(reader, code_width, value_count)?;
    codes
        .into_iter()
        .map(|code| {
            let entry = *entries
                .get(
                    usize::try_from(code)
                        .map_err(|_| AuraError::InvalidValue("dictionary code"))?,
                )
                .ok_or(AuraError::InvalidValue("dictionary code"))?;
            reconstruct_unsigned_offset(base, unit, entry)
        })
        .collect()
}

fn encode_huffman_dictionary(
    base: i64,
    unit: i64,
    entry_count: u32,
    entry_width: u8,
    code_lengths: &[u8],
    values: &[i64],
) -> Result<Vec<u8>> {
    let scaled_values = values
        .iter()
        .map(|value| scaled_unsigned_offset(*value, base, unit))
        .collect::<Result<Vec<_>>>()?;
    let mut entries = scaled_values.clone();
    entries.sort_unstable();
    entries.dedup();
    if entries.len() != entry_count as usize || code_lengths.len() != entries.len() {
        return Err(AuraError::InvalidValue("dictionary entry count"));
    }
    for entry in &entries {
        ensure_unsigned_width(*entry, entry_width, "dictionary entry")?;
    }
    let entry_indexes = entries
        .iter()
        .enumerate()
        .map(|(index, value)| (*value, index))
        .collect::<BTreeMap<_, _>>();
    let canonical_codes = canonical_huffman_codes(code_lengths)?;

    let mut out = pack_unsigned_values(&entries, entry_width)?;
    let mut writer = HuffmanBitWriter::new();
    for value in scaled_values {
        let index = entry_indexes
            .get(&value)
            .copied()
            .ok_or(AuraError::InvalidValue("dictionary code"))?;
        let code = canonical_codes
            .get(index)
            .and_then(|code| *code)
            .ok_or(AuraError::InvalidValue("huffman code"))?;
        writer.write_code(code)?;
    }
    out.extend(writer.finish());
    Ok(out)
}

fn decode_huffman_dictionary(
    base: i64,
    unit: i64,
    entry_count: u32,
    entry_width: u8,
    code_lengths: &[u8],
    reader: &mut ByteReader<'_>,
    value_count: usize,
) -> Result<Vec<i64>> {
    let entry_count = entry_count as usize;
    if code_lengths.len() != entry_count {
        return Err(AuraError::InvalidValue("huffman code lengths"));
    }
    let entries = read_bitpacked_unsigned(reader, entry_width, entry_count)?;
    if entry_count == 1 && matches!(code_lengths, [0] | [1]) {
        let entry = *entries
            .first()
            .ok_or(AuraError::InvalidValue("dictionary entry count"))?;
        let value = reconstruct_unsigned_offset(base, unit, entry)?;
        let code_bytes = reader.read_exact(reader.remaining())?;
        if !code_bytes.is_empty() && code_lengths == [0] {
            return Err(AuraError::InvalidValue("huffman code"));
        }
        return Ok(vec![value; value_count]);
    }

    let decoded_entries = entries
        .iter()
        .map(|entry| reconstruct_unsigned_offset(base, unit, *entry))
        .collect::<Result<Vec<_>>>()?;
    let canonical_codes = canonical_huffman_codes(code_lengths)?;
    let max_len = canonical_codes
        .iter()
        .filter_map(|code| code.map(|code| code.bit_len))
        .max()
        .ok_or(AuraError::InvalidValue("huffman code lengths"))?;
    let code_bytes = reader.read_exact(reader.remaining())?;
    if let Ok(decode_table) = huffman_bounded_value_decode_table(
        &canonical_codes,
        &decoded_entries,
        max_len.min(HUFFMAN_BOUNDED_ROOT_BITS),
    ) {
        return decode_huffman_values_with_bounded_table(&decode_table, code_bytes, value_count);
    }

    if max_len <= 20 {
        let decode_table = huffman_value_decode_table(&canonical_codes, &decoded_entries, max_len)?;
        let mut bit_reader = HuffmanTableBitReader::new(code_bytes);
        let mut out = Vec::with_capacity(value_count);
        for _ in 0..value_count {
            let key = bit_reader.peek_bits(max_len)? as usize;
            let entry = decode_table
                .get(key)
                .and_then(|entry| *entry)
                .ok_or(AuraError::InvalidValue("huffman code"))?;
            bit_reader.consume_bits(entry.bit_len)?;
            out.push(entry.value);
        }
        return Ok(out);
    }

    let decode_map = canonical_codes
        .iter()
        .enumerate()
        .filter_map(|(symbol, code)| code.map(|code| ((code.bit_len, code.bits), symbol)))
        .collect::<BTreeMap<_, _>>();
    let mut bit_reader = HuffmanBitReader::new(code_bytes);
    let mut out = Vec::with_capacity(value_count);
    for _ in 0..value_count {
        let mut bits = 0u64;
        let mut symbol = None;
        for bit_len in 1..=max_len {
            bits = (bits << 1) | u64::from(bit_reader.read_bit()?);
            if let Some(candidate) = decode_map.get(&(bit_len, bits)) {
                symbol = Some(*candidate);
                break;
            }
        }
        let symbol = symbol.ok_or(AuraError::InvalidValue("huffman code"))?;
        out.push(
            *decoded_entries
                .get(symbol)
                .ok_or(AuraError::InvalidValue("dictionary code"))?,
        );
    }
    Ok(out)
}

fn encode_block_local(block_size: u16, mode_count: u32, values: &[i64]) -> Result<Vec<u8>> {
    let block_size = usize::from(block_size);
    if block_size == 0 {
        return Err(AuraError::InvalidValue("block size"));
    }
    let block_count = values.len().div_ceil(block_size);
    if block_count != mode_count as usize {
        return Err(AuraError::InvalidValue("block count"));
    }
    let mut out = Vec::new();
    for block in values.chunks(block_size) {
        let op = choose_local_op(block)?;
        encode_local_op_header(&op, &mut out)?;
        out.extend(encode_i64_op(&op, block)?);
    }
    Ok(out)
}

fn decode_block_local(
    block_size: u16,
    mode_count: u32,
    reader: &mut ByteReader<'_>,
    value_count: usize,
) -> Result<Vec<i64>> {
    let block_size = usize::from(block_size);
    if block_size == 0 {
        return Err(AuraError::InvalidValue("block size"));
    }
    let block_count = value_count.div_ceil(block_size);
    if block_count != mode_count as usize {
        return Err(AuraError::InvalidValue("block count"));
    }
    let mut out = Vec::with_capacity(value_count);
    for block_index in 0..block_count {
        let remaining = value_count - out.len();
        let count = remaining.min(block_size);
        let op = decode_local_op_header(reader)?;
        let values = decode_i64_op(&op, reader, count)?;
        if block_index + 1 == block_count && values.len() != count {
            return Err(AuraError::InvalidValue("block local body"));
        }
        out.extend(values);
    }
    Ok(out)
}

fn choose_local_op(values: &[i64]) -> Result<GenericStreamOp> {
    let mut candidates = vec![derive_fixed_step(values)?, derive_base_bitpack(values)?];
    if let Some(op) = derive_prev_delta(values)? {
        candidates.push(op);
    }
    candidates.push(derive_patched_bitpack(values)?);
    candidates.push(derive_rle(values)?);
    candidates
        .into_iter()
        .map(|op| {
            let size = local_op_header_len(&op) + encode_i64_op(&op, values)?.len();
            Ok((size, op))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .min_by_key(|(size, _)| *size)
        .map(|(_, op)| op)
        .ok_or(AuraError::InvalidValue("block local mode"))
}

fn derive_fixed_step(values: &[i64]) -> Result<GenericStreamOp> {
    let base = *values
        .first()
        .ok_or(AuraError::InvalidValue("block local body"))?;
    let step = match values {
        [first, second, ..] => match second.checked_sub(*first) {
            Some(step) => step,
            None => return derive_base_bitpack(values),
        },
        _ => 0,
    };
    for (index, value) in values.iter().enumerate() {
        let Ok(expected) = fixed_step_value(base, step, index) else {
            return derive_base_bitpack(values);
        };
        if *value != expected {
            return derive_base_bitpack(values);
        }
    }
    Ok(GenericStreamOp::FixedStep { base, step })
}

fn derive_base_bitpack(values: &[i64]) -> Result<GenericStreamOp> {
    let base = *values
        .iter()
        .min()
        .ok_or(AuraError::InvalidValue("block local body"))?;
    let residuals = values
        .iter()
        .map(|value| u64::try_from(i128::from(*value) - i128::from(base)))
        .collect::<core::result::Result<Vec<_>, _>>()
        .map_err(|_| AuraError::InvalidValue("block local residual"))?;
    let unit = storage_unit(&residuals);
    let max_scaled = residuals
        .iter()
        .map(|value| value / unit as u64)
        .max()
        .unwrap_or(0);
    Ok(GenericStreamOp::BaseBitpack {
        base,
        unit,
        bit_width: unsigned_bitpack_width(max_scaled),
    })
}

fn derive_prev_delta(values: &[i64]) -> Result<Option<GenericStreamOp>> {
    let Some(base) = values.first().copied() else {
        return Ok(None);
    };
    if values.len() <= 1 {
        return Ok(None);
    }
    let mut deltas = Vec::with_capacity(values.len().saturating_sub(1));
    for pair in values.windows(2) {
        let delta = match pair[1].checked_sub(pair[0]) {
            Some(delta) => delta,
            None => return Ok(None),
        };
        deltas.push(delta);
    }
    let unit = signed_gcd_unit(&deltas);
    let scaled = deltas.iter().map(|delta| *delta / unit).collect::<Vec<_>>();
    let (min, max) = min_max_i64(&scaled).ok_or(AuraError::InvalidValue("previous delta"))?;
    Ok(Some(GenericStreamOp::PrevDelta {
        base,
        unit,
        bit_width: signed_bitpack_width_for_range(min, max),
    }))
}

fn derive_patched_bitpack(values: &[i64]) -> Result<GenericStreamOp> {
    let GenericStreamOp::BaseBitpack {
        base,
        unit,
        bit_width,
    } = derive_base_bitpack(values)?
    else {
        return Err(AuraError::InvalidValue("block local mode"));
    };
    let residuals = values
        .iter()
        .map(|value| scaled_unsigned_offset(*value, base, unit))
        .collect::<Result<Vec<_>>>()?;
    let mut best: Option<(usize, GenericStreamOp)> = None;
    for low_width in 0..=bit_width {
        let mut exception_count = 0usize;
        let mut max_high = 0u64;
        for residual in &residuals {
            let high = if low_width == 64 {
                0
            } else {
                *residual >> low_width
            };
            if high != 0 {
                exception_count += 1;
                max_high = max_high.max(high);
            }
        }
        let high_width = unsigned_bitpack_width(max_high);
        let op = GenericStreamOp::PatchedBitpack {
            base,
            unit,
            low_width,
            high_width,
            exception_count: u32::try_from(exception_count)
                .map_err(|_| AuraError::InvalidValue("exception count"))?,
        };
        let size = local_op_header_len(&op) + encode_i64_op(&op, values)?.len();
        if best.as_ref().is_none_or(|(best_size, _)| size < *best_size) {
            best = Some((size, op));
        }
    }
    best.map(|(_, op)| op)
        .ok_or(AuraError::InvalidValue("patched bitpack"))
}

fn derive_rle(values: &[i64]) -> Result<GenericStreamOp> {
    let base = *values
        .iter()
        .min()
        .ok_or(AuraError::InvalidValue("block local body"))?;
    let residuals = values
        .iter()
        .map(|value| u64::try_from(i128::from(*value) - i128::from(base)))
        .collect::<core::result::Result<Vec<_>, _>>()
        .map_err(|_| AuraError::InvalidValue("block local residual"))?;
    let unit = storage_unit(&residuals);
    let scaled = residuals
        .iter()
        .map(|value| value / unit as u64)
        .collect::<Vec<_>>();
    let max_scaled = scaled.iter().copied().max().unwrap_or(0);
    Ok(GenericStreamOp::Rle {
        base,
        unit,
        bit_width: unsigned_bitpack_width(max_scaled),
        run_count: u32::try_from(runs_for(&scaled).len())
            .map_err(|_| AuraError::InvalidValue("run count"))?,
    })
}

fn encode_local_op_header(op: &GenericStreamOp, out: &mut Vec<u8>) -> Result<()> {
    match *op {
        GenericStreamOp::FixedStep { base, step } => {
            put_u8(out, 0);
            put_i64_le(out, base);
            put_i64_le(out, step);
        }
        GenericStreamOp::BaseBitpack {
            base,
            unit,
            bit_width,
        } => {
            put_u8(out, 1);
            put_i64_le(out, base);
            put_i64_le(out, unit);
            put_u8(out, bit_width);
        }
        GenericStreamOp::PrevDelta {
            base,
            unit,
            bit_width,
        } => {
            put_u8(out, 2);
            put_i64_le(out, base);
            put_i64_le(out, unit);
            put_u8(out, bit_width);
        }
        GenericStreamOp::PatchedBitpack {
            base,
            unit,
            low_width,
            high_width,
            exception_count,
        } => {
            put_u8(out, 4);
            put_i64_le(out, base);
            put_i64_le(out, unit);
            put_u8(out, low_width);
            put_u8(out, high_width);
            put_u32_le(out, exception_count);
        }
        GenericStreamOp::Rle {
            base,
            unit,
            bit_width,
            run_count,
        } => {
            put_u8(out, 5);
            put_i64_le(out, base);
            put_i64_le(out, unit);
            put_u8(out, bit_width);
            put_u32_le(out, run_count);
        }
        _ => return Err(AuraError::InvalidValue("block local mode")),
    }
    Ok(())
}

fn decode_local_op_header(reader: &mut ByteReader<'_>) -> Result<GenericStreamOp> {
    match reader.read_u8()? {
        0 => Ok(GenericStreamOp::FixedStep {
            base: reader.read_i64_le()?,
            step: reader.read_i64_le()?,
        }),
        1 => Ok(GenericStreamOp::BaseBitpack {
            base: reader.read_i64_le()?,
            unit: reader.read_i64_le()?,
            bit_width: reader.read_u8()?,
        }),
        2 => Ok(GenericStreamOp::PrevDelta {
            base: reader.read_i64_le()?,
            unit: reader.read_i64_le()?,
            bit_width: reader.read_u8()?,
        }),
        4 => Ok(GenericStreamOp::PatchedBitpack {
            base: reader.read_i64_le()?,
            unit: reader.read_i64_le()?,
            low_width: reader.read_u8()?,
            high_width: reader.read_u8()?,
            exception_count: reader.read_u32_le()?,
        }),
        5 => Ok(GenericStreamOp::Rle {
            base: reader.read_i64_le()?,
            unit: reader.read_i64_le()?,
            bit_width: reader.read_u8()?,
            run_count: reader.read_u32_le()?,
        }),
        _ => Err(AuraError::InvalidValue("block local mode")),
    }
}

fn local_op_header_len(op: &GenericStreamOp) -> usize {
    match op {
        GenericStreamOp::FixedStep { .. } => 17,
        GenericStreamOp::BaseBitpack { .. } => 18,
        GenericStreamOp::PrevDelta { .. } => 18,
        GenericStreamOp::PatchedBitpack { .. } => 23,
        GenericStreamOp::Rle { .. } => 22,
        _ => usize::MAX,
    }
}

fn encode_uuid_const_mask_body(op: &GenericStreamOp, values: &[u128]) -> Result<Vec<u8>> {
    let GenericStreamOp::UuidConstMask {
        constant_bits,
        variable_bits,
    } = *op
    else {
        return Err(AuraError::InvalidValue("body type"));
    };
    if u16::from(constant_bits) + u16::from(variable_bits) != 128 {
        return Err(AuraError::InvalidValue("uuid bit mask"));
    }
    let constant_mask = select_uuid_constant_mask(values, constant_bits)?;
    let constant_value = values.first().copied().unwrap_or(0) & constant_mask;

    let mut out = Vec::new();
    out.extend_from_slice(&constant_mask.to_le_bytes());
    out.extend_from_slice(&constant_value.to_le_bytes());
    let mut writer = U128BitWriter::new();
    for value in values {
        for bit in 0..128 {
            if (constant_mask >> bit) & 1 == 0 {
                writer.write_bit((value >> bit) & 1 == 1);
            }
        }
    }
    out.extend(writer.finish());
    Ok(out)
}

fn decode_uuid_const_mask_body(
    op: &GenericStreamOp,
    bytes: &[u8],
    value_count: usize,
) -> Result<Vec<u128>> {
    let GenericStreamOp::UuidConstMask {
        constant_bits,
        variable_bits,
    } = *op
    else {
        return Err(AuraError::InvalidValue("body type"));
    };
    let mut reader = ByteReader::new(bytes);
    let constant_mask = read_u128_le(&mut reader)?;
    let constant_value = read_u128_le(&mut reader)?;
    if u16::from(constant_bits) + u16::from(variable_bits) != 128 {
        return Err(AuraError::InvalidValue("uuid bit mask"));
    }
    if constant_mask.count_ones() != u32::from(constant_bits)
        || 128 - constant_mask.count_ones() != u32::from(variable_bits)
        || constant_value & !constant_mask != 0
    {
        return Err(AuraError::InvalidValue("uuid bit mask"));
    }
    let bit_len = value_count
        .checked_mul(usize::from(variable_bits))
        .ok_or(AuraError::InvalidValue("uuid variable bits"))?;
    let byte_len = bit_len.div_ceil(8);
    let variable_bytes = reader.read_exact(byte_len)?;
    let mut bit_reader = U128BitReader::new(variable_bytes);
    let mut values = Vec::with_capacity(value_count);
    for _ in 0..value_count {
        let mut value = constant_value;
        for bit in 0..128 {
            if (constant_mask >> bit) & 1 == 0 && bit_reader.read_bit()? {
                value |= 1u128 << bit;
            }
        }
        values.push(value);
    }
    reader.finish()?;
    Ok(values)
}

fn fixed_step_value(base: i64, step: i64, index: usize) -> Result<i64> {
    let value = i128::from(base)
        + i128::from(step)
            .checked_mul(i128::try_from(index).map_err(|_| AuraError::InvalidValue("index"))?)
            .ok_or(AuraError::InvalidValue("fixed step"))?;
    i64::try_from(value).map_err(|_| AuraError::InvalidValue("fixed step"))
}

fn scaled_unsigned_offset(value: i64, base: i64, unit: i64) -> Result<u64> {
    validate_unit(unit)?;
    let delta = i128::from(value) - i128::from(base);
    if delta < 0 || delta % i128::from(unit) != 0 {
        return Err(AuraError::InvalidValue("scaled value"));
    }
    u64::try_from(delta / i128::from(unit)).map_err(|_| AuraError::InvalidValue("scaled value"))
}

fn scaled_signed_delta(value: i64, previous: i64, unit: i64) -> Result<i64> {
    validate_unit(unit)?;
    let delta = i128::from(value) - i128::from(previous);
    if delta % i128::from(unit) != 0 {
        return Err(AuraError::InvalidValue("scaled value"));
    }
    i64::try_from(delta / i128::from(unit)).map_err(|_| AuraError::InvalidValue("scaled value"))
}

fn scaled_signed_value(value: i64, unit: i64) -> Result<i64> {
    validate_unit(unit)?;
    if value % unit != 0 {
        return Err(AuraError::InvalidValue("scaled value"));
    }
    Ok(value / unit)
}

fn reconstruct_unsigned_offset(base: i64, unit: i64, value: u64) -> Result<i64> {
    validate_unit(unit)?;
    let out = i128::from(base) + i128::from(unit) * i128::from(value);
    i64::try_from(out).map_err(|_| AuraError::InvalidValue("scaled value"))
}

fn reconstruct_signed_delta(previous: i64, unit: i64, delta: i64) -> Result<i64> {
    validate_unit(unit)?;
    let out = i128::from(previous) + i128::from(unit) * i128::from(delta);
    i64::try_from(out).map_err(|_| AuraError::InvalidValue("scaled value"))
}

fn reconstruct_scaled_value(value: i64, unit: i64) -> Result<i64> {
    validate_unit(unit)?;
    let out = i128::from(value) * i128::from(unit);
    i64::try_from(out).map_err(|_| AuraError::InvalidValue("scaled value"))
}

fn read_bitpacked_unsigned(
    reader: &mut ByteReader<'_>,
    bit_width: u8,
    value_count: usize,
) -> Result<Vec<u64>> {
    let byte_len = bitpacked_byte_len(value_count as u64, bit_width) as usize;
    let bytes = reader.read_exact(byte_len)?;
    unpack_unsigned_values(bytes, bit_width, value_count)
}

fn read_bitpacked_signed(
    reader: &mut ByteReader<'_>,
    bit_width: u8,
    value_count: usize,
) -> Result<Vec<i64>> {
    let byte_len = bitpacked_byte_len(value_count as u64, bit_width) as usize;
    let bytes = reader.read_exact(byte_len)?;
    unpack_signed_values(bytes, bit_width, value_count)
}

fn validate_unit(unit: i64) -> Result<()> {
    if unit <= 0 {
        Err(AuraError::InvalidValue("storage unit"))
    } else {
        Ok(())
    }
}

fn index_width(value_count: usize) -> u8 {
    if value_count <= 1 {
        0
    } else {
        unsigned_bitpack_width((value_count - 1) as u64)
    }
}

fn low_mask(width: u8) -> Result<u64> {
    match width {
        0 => Ok(0),
        1..=63 => Ok((1u64 << width) - 1),
        64 => Ok(u64::MAX),
        _ => Err(AuraError::InvalidValue("bit width")),
    }
}

fn ensure_unsigned_width(value: u64, bit_width: u8, name: &'static str) -> Result<()> {
    let upper = low_mask(bit_width)?;
    if value <= upper {
        Ok(())
    } else {
        Err(AuraError::InvalidValue(name))
    }
}

fn runs_for<T: Copy + Eq>(values: &[T]) -> Vec<(T, usize)> {
    let Some(first) = values.first().copied() else {
        return Vec::new();
    };
    let mut runs = vec![(first, 1usize)];
    for value in &values[1..] {
        let last = runs
            .last_mut()
            .expect("runs always has an entry after first value");
        if last.0 == *value {
            last.1 += 1;
        } else {
            runs.push((*value, 1));
        }
    }
    runs
}

#[derive(Debug, Clone, Copy)]
struct HuffmanCode {
    bits: u64,
    bit_len: u8,
}

#[derive(Clone, Copy)]
struct HuffmanValueDecodeEntry {
    value: i64,
    bit_len: u8,
}

#[derive(Clone)]
enum BoundedHuffmanRootSlot {
    Empty,
    Leaf(HuffmanValueDecodeEntry),
    Subtable(usize),
}

struct BoundedHuffmanSubtable {
    bits: u8,
    slots: Vec<Option<HuffmanValueDecodeEntry>>,
}

struct BoundedHuffmanValueDecodeTable {
    root_bits: u8,
    root: Vec<BoundedHuffmanRootSlot>,
    subtables: Vec<BoundedHuffmanSubtable>,
}

fn huffman_value_decode_table(
    codes: &[Option<HuffmanCode>],
    values: &[i64],
    max_len: u8,
) -> Result<Vec<Option<HuffmanValueDecodeEntry>>> {
    let table_len = 1usize
        .checked_shl(u32::from(max_len))
        .ok_or(AuraError::InvalidValue("huffman code lengths"))?;
    let mut table = vec![None; table_len];
    for (symbol, code) in codes.iter().enumerate() {
        let Some(code) = code else {
            continue;
        };
        if code.bit_len > max_len {
            return Err(AuraError::InvalidValue("huffman code lengths"));
        }
        let suffix_bits = max_len - code.bit_len;
        let start = usize::try_from(code.bits)
            .map_err(|_| AuraError::InvalidValue("huffman code"))?
            .checked_shl(u32::from(suffix_bits))
            .ok_or(AuraError::InvalidValue("huffman code"))?;
        let end = start
            .checked_add(
                1usize
                    .checked_shl(u32::from(suffix_bits))
                    .ok_or(AuraError::InvalidValue("huffman code"))?,
            )
            .ok_or(AuraError::InvalidValue("huffman code"))?;
        let value = *values
            .get(symbol)
            .ok_or(AuraError::InvalidValue("dictionary code"))?;
        for slot in table
            .get_mut(start..end)
            .ok_or(AuraError::InvalidValue("huffman code"))?
        {
            *slot = Some(HuffmanValueDecodeEntry {
                value,
                bit_len: code.bit_len,
            });
        }
    }
    Ok(table)
}

fn huffman_bounded_value_decode_table(
    codes: &[Option<HuffmanCode>],
    values: &[i64],
    root_bits: u8,
) -> Result<BoundedHuffmanValueDecodeTable> {
    if root_bits == 0 || root_bits > 64 || codes.len() != values.len() {
        return Err(AuraError::InvalidValue("huffman code lengths"));
    }
    let root_len = 1usize
        .checked_shl(u32::from(root_bits))
        .ok_or(AuraError::InvalidValue("huffman code lengths"))?;
    let mut root = vec![BoundedHuffmanRootSlot::Empty; root_len];
    let mut long_codes = BTreeMap::<usize, Vec<(usize, HuffmanCode)>>::new();

    for (symbol, code) in codes.iter().enumerate() {
        let Some(code) = code else {
            continue;
        };
        if code.bit_len == 0 || code.bit_len > 64 {
            return Err(AuraError::InvalidValue("huffman code lengths"));
        }
        if code.bit_len <= root_bits {
            let suffix_bits = root_bits - code.bit_len;
            let start = usize::try_from(code.bits)
                .map_err(|_| AuraError::InvalidValue("huffman code"))?
                .checked_shl(u32::from(suffix_bits))
                .ok_or(AuraError::InvalidValue("huffman code"))?;
            let span = 1usize
                .checked_shl(u32::from(suffix_bits))
                .ok_or(AuraError::InvalidValue("huffman code"))?;
            let value = *values
                .get(symbol)
                .ok_or(AuraError::InvalidValue("dictionary code"))?;
            fill_bounded_root_slots(
                &mut root,
                start,
                span,
                BoundedHuffmanRootSlot::Leaf(HuffmanValueDecodeEntry {
                    value,
                    bit_len: code.bit_len,
                }),
            )?;
        } else {
            let prefix = usize::try_from(code.bits >> (code.bit_len - root_bits))
                .map_err(|_| AuraError::InvalidValue("huffman code"))?;
            long_codes.entry(prefix).or_default().push((symbol, *code));
        }
    }

    let mut subtables = Vec::new();
    for (prefix, codes_for_prefix) in long_codes {
        if prefix >= root.len() {
            return Err(AuraError::InvalidValue("huffman code"));
        }
        if !matches!(root[prefix], BoundedHuffmanRootSlot::Empty) {
            return Err(AuraError::InvalidValue("huffman code"));
        }
        let subtable_bits = codes_for_prefix
            .iter()
            .map(|(_, code)| code.bit_len - root_bits)
            .max()
            .ok_or(AuraError::InvalidValue("huffman code lengths"))?;
        if subtable_bits == 0 || subtable_bits > HUFFMAN_BOUNDED_MAX_SUBTABLE_BITS {
            return Err(AuraError::InvalidValue("huffman code lengths"));
        }
        let slot_count = 1usize
            .checked_shl(u32::from(subtable_bits))
            .ok_or(AuraError::InvalidValue("huffman code"))?;
        let mut slots = vec![None; slot_count];
        for (symbol, code) in codes_for_prefix {
            let suffix_len = code.bit_len - root_bits;
            let suffix_mask = low_mask(suffix_len)?;
            let suffix = usize::try_from(code.bits & suffix_mask)
                .map_err(|_| AuraError::InvalidValue("huffman code"))?;
            let fill_bits = subtable_bits - suffix_len;
            let start = suffix
                .checked_shl(u32::from(fill_bits))
                .ok_or(AuraError::InvalidValue("huffman code"))?;
            let span = 1usize
                .checked_shl(u32::from(fill_bits))
                .ok_or(AuraError::InvalidValue("huffman code"))?;
            let value = *values
                .get(symbol)
                .ok_or(AuraError::InvalidValue("dictionary code"))?;
            fill_bounded_subtable_slots(
                &mut slots,
                start,
                span,
                HuffmanValueDecodeEntry {
                    value,
                    bit_len: code.bit_len,
                },
            )?;
        }
        let subtable_index = subtables.len();
        subtables.push(BoundedHuffmanSubtable {
            bits: subtable_bits,
            slots,
        });
        root[prefix] = BoundedHuffmanRootSlot::Subtable(subtable_index);
    }

    Ok(BoundedHuffmanValueDecodeTable {
        root_bits,
        root,
        subtables,
    })
}

fn fill_bounded_root_slots(
    root: &mut [BoundedHuffmanRootSlot],
    start: usize,
    span: usize,
    entry: BoundedHuffmanRootSlot,
) -> Result<()> {
    let end = start
        .checked_add(span)
        .ok_or(AuraError::InvalidValue("huffman code"))?;
    for slot in root
        .get_mut(start..end)
        .ok_or(AuraError::InvalidValue("huffman code"))?
    {
        if !matches!(slot, BoundedHuffmanRootSlot::Empty) {
            return Err(AuraError::InvalidValue("huffman code"));
        }
        *slot = entry.clone();
    }
    Ok(())
}

fn fill_bounded_subtable_slots(
    slots: &mut [Option<HuffmanValueDecodeEntry>],
    start: usize,
    span: usize,
    entry: HuffmanValueDecodeEntry,
) -> Result<()> {
    let end = start
        .checked_add(span)
        .ok_or(AuraError::InvalidValue("huffman code"))?;
    for slot in slots
        .get_mut(start..end)
        .ok_or(AuraError::InvalidValue("huffman code"))?
    {
        if slot.is_some() {
            return Err(AuraError::InvalidValue("huffman code"));
        }
        *slot = Some(entry);
    }
    Ok(())
}

fn decode_huffman_values_with_bounded_table(
    table: &BoundedHuffmanValueDecodeTable,
    code_bytes: &[u8],
    value_count: usize,
) -> Result<Vec<i64>> {
    let mut bit_reader = HuffmanTableBitReader::new(code_bytes);
    let mut out = Vec::with_capacity(value_count);
    for _ in 0..value_count {
        let key = bit_reader.peek_bits(table.root_bits)? as usize;
        let entry = match table
            .root
            .get(key)
            .ok_or(AuraError::InvalidValue("huffman code"))?
        {
            BoundedHuffmanRootSlot::Empty => return Err(AuraError::InvalidValue("huffman code")),
            BoundedHuffmanRootSlot::Leaf(entry) => *entry,
            BoundedHuffmanRootSlot::Subtable(subtable_index) => {
                let subtable = table
                    .subtables
                    .get(*subtable_index)
                    .ok_or(AuraError::InvalidValue("huffman code"))?;
                let total_bits = table
                    .root_bits
                    .checked_add(subtable.bits)
                    .ok_or(AuraError::InvalidValue("huffman code"))?;
                let suffix_mask = low_mask(subtable.bits)?;
                let suffix = usize::try_from(bit_reader.peek_bits(total_bits)? & suffix_mask)
                    .map_err(|_| AuraError::InvalidValue("huffman code"))?;
                subtable
                    .slots
                    .get(suffix)
                    .and_then(|entry| *entry)
                    .ok_or(AuraError::InvalidValue("huffman code"))?
            }
        };
        bit_reader.consume_bits(entry.bit_len)?;
        out.push(entry.value);
    }
    Ok(out)
}

fn canonical_huffman_codes(code_lengths: &[u8]) -> Result<Vec<Option<HuffmanCode>>> {
    if code_lengths.is_empty() {
        return Err(AuraError::InvalidValue("huffman code lengths"));
    }
    if code_lengths.len() == 1 {
        return match code_lengths[0] {
            0 | 1 => Ok(vec![Some(HuffmanCode {
                bits: 0,
                bit_len: code_lengths[0],
            })]),
            _ => Err(AuraError::InvalidValue("huffman code lengths")),
        };
    }
    if code_lengths
        .iter()
        .any(|length| *length == 0 || *length > 64)
    {
        return Err(AuraError::InvalidValue("huffman code lengths"));
    }

    let mut symbols = code_lengths.iter().copied().enumerate().collect::<Vec<_>>();
    symbols.sort_by_key(|(symbol, length)| (*length, *symbol));

    let mut out = vec![None; code_lengths.len()];
    let mut code = 0u128;
    let mut previous_len = 0u8;
    for (ordinal, (symbol, bit_len)) in symbols.into_iter().enumerate() {
        if ordinal > 0 {
            code = code
                .checked_add(1)
                .ok_or(AuraError::InvalidValue("huffman code"))?;
        }
        let shift = bit_len
            .checked_sub(previous_len)
            .ok_or(AuraError::InvalidValue("huffman code lengths"))?;
        code = code
            .checked_shl(u32::from(shift))
            .ok_or(AuraError::InvalidValue("huffman code"))?;
        let limit = 1u128
            .checked_shl(u32::from(bit_len))
            .ok_or(AuraError::InvalidValue("huffman code"))?;
        if code >= limit {
            return Err(AuraError::InvalidValue("huffman code lengths"));
        }
        out[symbol] = Some(HuffmanCode {
            bits: code as u64,
            bit_len,
        });
        previous_len = bit_len;
    }
    Ok(out)
}

struct HuffmanBitWriter {
    bytes: Vec<u8>,
    current: u8,
    used_bits: u8,
}

impl HuffmanBitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            used_bits: 0,
        }
    }

    fn write_code(&mut self, code: HuffmanCode) -> Result<()> {
        if code.bit_len == 0 {
            return Ok(());
        }
        for offset in (0..code.bit_len).rev() {
            let bit = ((code.bits >> offset) & 1) as u8;
            self.current |= bit << (7 - self.used_bits);
            self.used_bits += 1;
            if self.used_bits == 8 {
                self.bytes.push(self.current);
                self.current = 0;
                self.used_bits = 0;
            }
        }
        Ok(())
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used_bits != 0 {
            self.bytes.push(self.current);
        }
        self.bytes
    }
}

struct HuffmanBitReader<'a> {
    bytes: &'a [u8],
    byte_index: usize,
    used_bits: u8,
}

struct HuffmanTableBitReader<'a> {
    bytes: &'a [u8],
    byte_index: usize,
    buffer: u64,
    buffered_bits: u8,
}

impl<'a> HuffmanTableBitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            byte_index: 0,
            buffer: 0,
            buffered_bits: 0,
        }
    }

    fn peek_bits(&mut self, bit_count: u8) -> Result<u64> {
        if bit_count > 64 {
            return Err(AuraError::InvalidValue("huffman code"));
        }
        self.fill_to(bit_count);
        if self.buffered_bits >= bit_count {
            Ok(self.buffer >> (self.buffered_bits - bit_count))
        } else {
            Ok(self.buffer << (bit_count - self.buffered_bits))
        }
    }

    fn consume_bits(&mut self, bit_count: u8) -> Result<()> {
        self.fill_to(bit_count);
        if self.buffered_bits < bit_count {
            return Err(AuraError::UnexpectedEof);
        }
        self.buffered_bits -= bit_count;
        self.buffer = match self.buffered_bits {
            0 => 0,
            64 => self.buffer,
            bits => self.buffer & ((1u64 << bits) - 1),
        };
        Ok(())
    }

    fn fill_to(&mut self, bit_count: u8) {
        while self.buffered_bits < bit_count && self.byte_index < self.bytes.len() {
            self.buffer = (self.buffer << 8) | u64::from(self.bytes[self.byte_index]);
            self.buffered_bits += 8;
            self.byte_index += 1;
        }
    }
}

impl<'a> HuffmanBitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            byte_index: 0,
            used_bits: 0,
        }
    }

    fn read_bit(&mut self) -> Result<u8> {
        let byte = *self
            .bytes
            .get(self.byte_index)
            .ok_or(AuraError::UnexpectedEof)?;
        let bit = (byte >> (7 - self.used_bits)) & 1;
        self.used_bits += 1;
        if self.used_bits == 8 {
            self.byte_index += 1;
            self.used_bits = 0;
        }
        Ok(bit)
    }
}

fn gcd_unit(values: &[u64]) -> u64 {
    let mut out = 0u64;
    for value in values.iter().copied().filter(|value| *value != 0) {
        out = if out == 0 { value } else { gcd(out, value) };
    }
    out.max(1)
}

fn storage_unit(values: &[u64]) -> i64 {
    let unit = gcd_unit(values);
    i64::try_from(unit).unwrap_or(1)
}

fn signed_gcd_unit(values: &[i64]) -> i64 {
    let mut out = 0u64;
    for value in values.iter().copied().filter(|value| *value != 0) {
        let value = value.unsigned_abs();
        out = if out == 0 { value } else { gcd(out, value) };
    }
    i64::try_from(out).unwrap_or(1).max(1)
}

fn min_max_i64(values: &[i64]) -> Option<(i64, i64)> {
    let mut iter = values.iter().copied();
    let first = iter.next()?;
    let mut min = first;
    let mut max = first;
    for value in iter {
        min = min.min(value);
        max = max.max(value);
    }
    Some((min, max))
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    a
}

fn select_uuid_constant_mask(values: &[u128], constant_bits: u8) -> Result<u128> {
    let candidates = uuid_constant_candidates(values);
    if candidates.count_ones() < u32::from(constant_bits) {
        return Err(AuraError::InvalidValue("uuid bit mask"));
    }
    let mut mask = 0u128;
    let mut remaining = constant_bits;
    for bit in (0..128).rev() {
        if remaining == 0 {
            break;
        }
        if (candidates >> bit) & 1 == 1 {
            mask |= 1u128 << bit;
            remaining -= 1;
        }
    }
    Ok(mask)
}

fn uuid_constant_candidates(values: &[u128]) -> u128 {
    if values.is_empty() {
        return u128::MAX;
    }
    let mut all_ones = u128::MAX;
    let mut all_zeroes = u128::MAX;
    for value in values {
        all_ones &= *value;
        all_zeroes &= !*value;
    }
    all_ones | all_zeroes
}

fn read_u128_le(reader: &mut ByteReader<'_>) -> Result<u128> {
    let bytes = reader.read_exact(16)?;
    Ok(u128::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
        bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    ]))
}

struct U128BitWriter {
    bytes: Vec<u8>,
    current: u8,
    used_bits: u8,
}

impl U128BitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            used_bits: 0,
        }
    }

    fn write_bit(&mut self, value: bool) {
        if value {
            self.current |= 1 << self.used_bits;
        }
        self.used_bits += 1;
        if self.used_bits == 8 {
            self.bytes.push(self.current);
            self.current = 0;
            self.used_bits = 0;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used_bits != 0 {
            self.bytes.push(self.current);
        }
        self.bytes
    }
}

struct U128BitReader<'a> {
    bytes: &'a [u8],
    byte_index: usize,
    used_bits: u8,
}

impl<'a> U128BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            byte_index: 0,
            used_bits: 0,
        }
    }

    fn read_bit(&mut self) -> Result<bool> {
        let byte = *self
            .bytes
            .get(self.byte_index)
            .ok_or(AuraError::UnexpectedEof)?;
        let bit = ((byte >> self.used_bits) & 1) == 1;
        self.used_bits += 1;
        if self.used_bits == 8 {
            self.byte_index += 1;
            self.used_bits = 0;
        }
        Ok(bit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_huffman_decode_table_decodes_codes_above_root_width() {
        let code_lengths = (1u8..=17).chain([17]).collect::<Vec<_>>();
        let codes = canonical_huffman_codes(&code_lengths).unwrap();
        let values = (0..code_lengths.len())
            .map(|index| index as i64 * 10)
            .collect::<Vec<_>>();
        let table = huffman_bounded_value_decode_table(&codes, &values, 12).unwrap();
        let symbols = [0usize, 1, 2, 16, 17, 3, 15, 4];
        let mut writer = HuffmanBitWriter::new();
        for symbol in symbols {
            writer.write_code(codes[symbol].unwrap()).unwrap();
        }

        let decoded =
            decode_huffman_values_with_bounded_table(&table, &writer.finish(), symbols.len())
                .unwrap();

        assert_eq!(
            symbols
                .into_iter()
                .map(|symbol| values[symbol])
                .collect::<Vec<_>>(),
            decoded
        );
    }
}
