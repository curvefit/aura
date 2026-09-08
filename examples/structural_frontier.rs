//! Paired structural/no-Huffman/Huffman experiment on one complete V2 file.
//!
//! The input must be a compact explicit-event Aura0 file with a generic plan.
//! One high-volume stream whose current operation has a nested structural
//! residual (`PreviousValueDelta`, `DeltaOfDelta`, or `FixedStrideDelta`) is
//! selected when one is available. Its structural transform is kept as an
//! operation; only the nested residual codec changes between
//! PackedDictionary and HuffmanDictionary. If no nested structural stream is
//! suitable, the largest low-cardinality ordinary stream is used as the
//! bounded `identity_stream_values` fallback: it introduces no relationship
//! and keeps the existing schema/group plan while changing only that stream's
//! physical value codec. Every other stream, the header, and footer fields are
//! retained. Both rebuilt files are decoded through the public event decoder
//! before their results are printed.
//!
//! This is a bounded experiment, not a new codec framework or a production
//! default. It deliberately chooses one stream and caps the dictionary at
//! 4096 entries; it does not search combinations of streams.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aura_codec::bytes::ByteReader;
use aura_codec::format::SEAL_MAGIC;
use aura_codec::instructions::{GenericInstructionPlan, GenericStreamOp};
use aura_codec::records::{self, DecodedI64FileMetadata};
use aura_codec::{
    decode_generic_stream_body, encode_generic_stream_body, GenericStreamBodyValue, Profile,
};
use sha2::{Digest, Sha256};

const MAX_PROBE_DICTIONARY_ENTRIES: usize = 4096;

#[derive(Debug, Clone)]
struct StreamFrame {
    stream_id: u16,
    value_count: usize,
    body: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
enum StructuralShape {
    PreviousValue,
    DeltaOfDelta,
    FixedStride(u16),
    Identity,
}

impl StructuralShape {
    const fn label(self) -> &'static str {
        match self {
            Self::PreviousValue => "previous_value_delta",
            Self::DeltaOfDelta => "delta_of_delta",
            Self::FixedStride(_) => "fixed_stride_delta",
            Self::Identity => "identity_stream_values",
        }
    }
}

#[derive(Debug, Clone)]
struct Candidate {
    frame_index: usize,
    instruction_index: usize,
    stream_id: u16,
    value_count: usize,
    shape: StructuralShape,
    headroom_bits: i128,
    logical_values: Vec<i64>,
    residual_values: Vec<i64>,
}

#[derive(Debug, Clone)]
struct DictionaryShape {
    base: i64,
    unit: i64,
    entry_count: u32,
    entry_width: u8,
    code_width: u8,
    entries: Vec<u64>,
    frequencies: Vec<u64>,
}

#[derive(Debug, Clone)]
struct VariantResult {
    mode: &'static str,
    output_bytes: usize,
    header_bytes: usize,
    body_bytes: usize,
    footer_bytes: usize,
    trailer_bytes: usize,
    plan_bytes: usize,
    selected_stream_body_bytes_before: usize,
    selected_stream_body_bytes_after: usize,
    encode_us: u128,
    decode_us: u128,
    header_preserved: bool,
    footer_preserved_except_plan: bool,
    output_path: Option<String>,
    sha256: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct HuffmanHeapNode {
    frequency: u64,
    min_symbol: usize,
    node_index: usize,
}

impl Ord for HuffmanHeapNode {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .frequency
            .cmp(&self.frequency)
            .then_with(|| other.min_symbol.cmp(&self.min_symbol))
            .then_with(|| other.node_index.cmp(&self.node_index))
    }
}

impl PartialOrd for HuffmanHeapNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, Copy)]
struct HuffmanTreeNode {
    symbol: Option<usize>,
    left: Option<usize>,
    right: Option<usize>,
}

fn main() -> Result<(), aura_codec::AuraError> {
    let mut args = env::args().skip(1);
    let input_path = args.next().ok_or(aura_codec::AuraError::InvalidValue(
        "usage: structural_frontier <aura0> [output_dir]",
    ))?;
    let output_dir = args.next().map(PathBuf::from);
    if args.next().is_some() {
        return Err(aura_codec::AuraError::InvalidValue(
            "usage: structural_frontier <aura0> [output_dir]",
        ));
    }
    if let Some(output_dir) = &output_dir {
        fs::create_dir_all(output_dir)
            .map_err(|_| aura_codec::AuraError::InvalidValue("structural probe output dir"))?;
    }
    let input = fs::read(&input_path)
        .map_err(|_| aura_codec::AuraError::InvalidValue("structural probe input"))?;
    let metadata = records::decode_i64_file_metadata(&input)?;
    if metadata.header.profile != Profile::Aura0 {
        return Err(aura_codec::AuraError::InvalidValue(
            "structural probe profile",
        ));
    }
    let footer = metadata
        .compiled_footer
        .as_ref()
        .ok_or(aura_codec::AuraError::InvalidValue(
            "structural probe footer",
        ))?;
    if !footer.aura1_byte_lanes.is_empty() || !footer.chunks.is_empty() {
        return Err(aura_codec::AuraError::InvalidValue(
            "structural probe requires compact Aura0 without byte lanes/chunks",
        ));
    }
    let plan = footer
        .generic_aura0_plan
        .as_ref()
        .ok_or(aura_codec::AuraError::InvalidValue(
            "structural probe generic plan",
        ))?;
    let selection_start = Instant::now();
    let frames = read_stream_frames(&input[metadata.header_len..metadata.footer_start])?;
    let candidate = choose_candidate(plan, &frames)?;
    let shape = dictionary_shape(&candidate.residual_values)?;
    let (packed_op, huffman_op) = dictionary_ops(&shape)?;
    let shared_selection_ns = selection_start.elapsed().as_nanos();
    let packed_path = output_dir.as_ref().map(|dir| dir.join("packed.aura0"));
    let huffman_path = output_dir.as_ref().map(|dir| dir.join("huffman.aura0"));

    let packed = rebuild_variant(
        &input,
        &metadata,
        plan,
        &frames,
        &candidate,
        &packed_op,
        "structural+packed_dictionary",
        packed_path.as_deref(),
    )?;
    let huffman = rebuild_variant(
        &input,
        &metadata,
        plan,
        &frames,
        &candidate,
        &huffman_op,
        "structural+huffman_dictionary",
        huffman_path.as_deref(),
    )?;

    let source_events = records::decode_i64_events_file(&input)?.events;
    let source_footer =
        metadata
            .compiled_footer
            .as_ref()
            .ok_or(aura_codec::AuraError::InvalidValue(
                "structural probe footer",
            ))?;
    let packed_meta = records::decode_i64_file_metadata(&packed.0)?;
    let huffman_meta = records::decode_i64_file_metadata(&huffman.0)?;
    let packed_footer = packed_meta
        .compiled_footer
        .as_ref()
        .ok_or(aura_codec::AuraError::InvalidValue("packed probe footer"))?;
    let huffman_footer = huffman_meta
        .compiled_footer
        .as_ref()
        .ok_or(aura_codec::AuraError::InvalidValue("huffman probe footer"))?;
    let packed_events = records::decode_i64_events_file(&packed.0)?.events;
    let huffman_events = records::decode_i64_events_file(&huffman.0)?.events;
    let packed_footer_preserved = footer_preserved_except_plan(source_footer, packed_footer);
    let huffman_footer_preserved = footer_preserved_except_plan(source_footer, huffman_footer);
    let packed_exact = packed_events == source_events;
    let huffman_exact = huffman_events == source_events;
    if !packed_exact || !huffman_exact || !packed_footer_preserved || !huffman_footer_preserved {
        return Err(aura_codec::AuraError::InvalidValue(
            "structural probe parity",
        ));
    }
    if let Some(path) = &packed_path {
        fs::write(path, &packed.0)
            .map_err(|_| aura_codec::AuraError::InvalidValue("structural probe packed output"))?;
    }
    if let Some(path) = &huffman_path {
        fs::write(path, &huffman.0)
            .map_err(|_| aura_codec::AuraError::InvalidValue("structural probe huffman output"))?;
    }
    let candidate_note = if matches!(candidate.shape, StructuralShape::Identity) {
        "The identity_stream_values fallback adds no relationship; the selected stream's logical values and existing schema/group plan are retained while its physical codec changes."
    } else {
        "The selected structural transform is unchanged; only its residual entropy coding differs."
    };

    println!(
        "{}",
        serde_json::json!({
            "probe": "structural_frontier_v2",
            "status": "complete_container_pair",
            "input": input_path,
            "output_dir": output_dir.as_ref().map(|path| path.display().to_string()),
            "input_bytes": input.len(),
            "schema_id": metadata.schema.schema_id,
            "schema_map": metadata.schema.compact_schema_map,
            "record_count": metadata.record_count,
            "stream_id": candidate.stream_id,
            "stream_value_count": candidate.value_count,
            "structural_transform": candidate.shape.label(),
            "residual_value_count": candidate.residual_values.len(),
            "dictionary_entries": shape.entries.len(),
            "estimated_entropy_headroom_bits": candidate.headroom_bits,
            "shared_selection_ns": shared_selection_ns,
            "original_stream_body_bytes": frames[candidate.frame_index].body.len(),
            "source_events_decoded": source_events.len(),
            "packed": variant_json(&packed.1, true),
            "huffman": variant_json(&huffman.1, true),
            "notes": [
                "Both outputs preserve the source header and every footer field except the selected stream operation.",
                candidate_note,
                "Receipt files and external source archives are not part of Aura0 file_bytes."
            ]
        })
    );
    Ok(())
}

fn read_stream_frames(body: &[u8]) -> Result<Vec<StreamFrame>, aura_codec::AuraError> {
    let mut reader = ByteReader::new(body);
    let stream_count = usize::from(reader.read_u16_le()?);
    let mut frames = Vec::new();
    frames
        .try_reserve_exact(stream_count)
        .map_err(|_| aura_codec::AuraError::InvalidValue("structural probe frames"))?;
    for _ in 0..stream_count {
        let stream_id = reader.read_u16_le()?;
        let value_count = usize::try_from(reader.read_u64_le()?)
            .map_err(|_| aura_codec::AuraError::InvalidValue("structural probe values"))?;
        let body_len = usize::try_from(reader.read_u32_le()?)
            .map_err(|_| aura_codec::AuraError::InvalidValue("structural probe body"))?;
        frames.push(StreamFrame {
            stream_id,
            value_count,
            body: reader.read_exact(body_len)?.to_vec(),
        });
    }
    reader.finish()?;
    Ok(frames)
}

fn encode_stream_frames(frames: &[StreamFrame]) -> Result<Vec<u8>, aura_codec::AuraError> {
    let count = u16::try_from(frames.len())
        .map_err(|_| aura_codec::AuraError::InvalidValue("structural probe frames"))?;
    let mut out = Vec::new();
    out.extend_from_slice(&count.to_le_bytes());
    for frame in frames {
        let body_len = u32::try_from(frame.body.len())
            .map_err(|_| aura_codec::AuraError::InvalidValue("structural probe body"))?;
        out.extend_from_slice(&frame.stream_id.to_le_bytes());
        out.extend_from_slice(&(frame.value_count as u64).to_le_bytes());
        out.extend_from_slice(&body_len.to_le_bytes());
        out.extend_from_slice(&frame.body);
    }
    Ok(out)
}

// Screen entropy headroom without trial encoding. Constants are excluded:
// their existing zero-payload representation cannot benefit from Huffman.
fn dictionary_headroom(shape: &DictionaryShape) -> Result<i128, aura_codec::AuraError> {
    let lengths = huffman_code_lengths(&shape.frequencies)?;
    let bits = shape
        .frequencies
        .iter()
        .zip(lengths)
        .map(|(frequency, length)| {
            i128::from(*frequency) * (i128::from(shape.code_width) - i128::from(length))
        })
        .sum::<i128>();
    Ok(bits - 8 * shape.entries.len() as i128)
}

fn choose_candidate(
    plan: &GenericInstructionPlan,
    frames: &[StreamFrame],
) -> Result<Candidate, aura_codec::AuraError> {
    let mut best: Option<Candidate> = None;
    for (frame_index, frame) in frames.iter().enumerate() {
        let Some((instruction_index, instruction)) = plan
            .streams
            .iter()
            .enumerate()
            .find(|(_, instruction)| instruction.stream_id == frame.stream_id)
        else {
            continue;
        };
        let Some(shape) = structural_shape(&instruction.op) else {
            continue;
        };
        let logical_values =
            match decode_generic_stream_body(instruction, &frame.body, frame.value_count)? {
                GenericStreamBodyValue::I64(values) => values,
                GenericStreamBodyValue::U128(_) => continue,
            };
        let residual_values = residual_values(shape, &logical_values)?;
        let dictionary = match dictionary_shape(&residual_values) {
            Ok(dictionary) => dictionary,
            Err(_) => continue,
        };
        if dictionary.entries.len() < 2 || dictionary.entries.len() > MAX_PROBE_DICTIONARY_ENTRIES {
            continue;
        }
        let candidate = Candidate {
            frame_index,
            instruction_index,
            stream_id: frame.stream_id,
            value_count: frame.value_count,
            shape,
            headroom_bits: dictionary_headroom(&dictionary)?,
            logical_values,
            residual_values,
        };
        if best.as_ref().is_none_or(|current| {
            (candidate.headroom_bits, candidate.value_count)
                > (current.headroom_bits, current.value_count)
        }) {
            best = Some(candidate);
        }
    }
    if let Some(best) = best {
        return Ok(best);
    }

    // Some production plans have no nested structural operation. In that
    // case, probe the largest ordinary I64 stream without inventing a
    // relationship or changing the surrounding schema/group plan.
    for (frame_index, frame) in frames.iter().enumerate() {
        let Some((instruction_index, instruction)) = plan
            .streams
            .iter()
            .enumerate()
            .find(|(_, instruction)| instruction.stream_id == frame.stream_id)
        else {
            continue;
        };
        if structural_shape(&instruction.op).is_some() {
            continue;
        }
        let logical_values =
            match decode_generic_stream_body(instruction, &frame.body, frame.value_count)? {
                GenericStreamBodyValue::I64(values) => values,
                GenericStreamBodyValue::U128(_) => continue,
            };
        let dictionary = match dictionary_shape(&logical_values) {
            Ok(dictionary) => dictionary,
            Err(_) => continue,
        };
        if dictionary.entries.len() < 2 || dictionary.entries.len() > MAX_PROBE_DICTIONARY_ENTRIES {
            continue;
        }
        let candidate = Candidate {
            frame_index,
            instruction_index,
            stream_id: frame.stream_id,
            value_count: frame.value_count,
            shape: StructuralShape::Identity,
            headroom_bits: dictionary_headroom(&dictionary)?,
            residual_values: logical_values.clone(),
            logical_values,
        };
        if best.as_ref().is_none_or(|current| {
            (candidate.headroom_bits, candidate.value_count)
                > (current.headroom_bits, current.value_count)
        }) {
            best = Some(candidate);
        }
    }
    best.ok_or(aura_codec::AuraError::InvalidValue(
        "no low-cardinality stream",
    ))
}

fn structural_shape(op: &GenericStreamOp) -> Option<StructuralShape> {
    match op {
        GenericStreamOp::PreviousValueDelta { .. } => Some(StructuralShape::PreviousValue),
        GenericStreamOp::DeltaOfDelta { .. } => Some(StructuralShape::DeltaOfDelta),
        GenericStreamOp::FixedStrideDelta { stride, .. } => {
            Some(StructuralShape::FixedStride(*stride))
        }
        _ => None,
    }
}

fn residual_values(
    shape: StructuralShape,
    values: &[i64],
) -> Result<Vec<i64>, aura_codec::AuraError> {
    match shape {
        StructuralShape::PreviousValue => previous_value_residuals(values),
        StructuralShape::DeltaOfDelta => delta_of_delta_residuals(values),
        StructuralShape::FixedStride(stride) => fixed_stride_residuals(values, usize::from(stride)),
        StructuralShape::Identity => Ok(values.to_vec()),
    }
}

fn previous_value_residuals(values: &[i64]) -> Result<Vec<i64>, aura_codec::AuraError> {
    let mut residuals = Vec::with_capacity(values.len());
    let Some(first) = values.first().copied() else {
        return Ok(residuals);
    };
    residuals.push(first);
    for pair in values.windows(2) {
        residuals.push(
            pair[1]
                .checked_sub(pair[0])
                .ok_or(aura_codec::AuraError::InvalidValue(
                    "structural probe residual",
                ))?,
        );
    }
    Ok(residuals)
}

fn delta_of_delta_residuals(values: &[i64]) -> Result<Vec<i64>, aura_codec::AuraError> {
    let mut residuals = Vec::with_capacity(values.len());
    let Some(first) = values.first().copied() else {
        return Ok(residuals);
    };
    residuals.push(first);
    let Some(second) = values.get(1).copied() else {
        return Ok(residuals);
    };
    let mut previous_delta =
        second
            .checked_sub(first)
            .ok_or(aura_codec::AuraError::InvalidValue(
                "structural probe residual",
            ))?;
    residuals.push(previous_delta);
    for pair in values.windows(2).skip(1) {
        let delta = pair[1]
            .checked_sub(pair[0])
            .ok_or(aura_codec::AuraError::InvalidValue(
                "structural probe residual",
            ))?;
        residuals.push(delta.checked_sub(previous_delta).ok_or(
            aura_codec::AuraError::InvalidValue("structural probe residual"),
        )?);
        previous_delta = delta;
    }
    Ok(residuals)
}

fn fixed_stride_residuals(
    values: &[i64],
    stride: usize,
) -> Result<Vec<i64>, aura_codec::AuraError> {
    if stride == 0 {
        return Err(aura_codec::AuraError::InvalidValue(
            "structural probe stride",
        ));
    }
    values
        .iter()
        .copied()
        .enumerate()
        .map(|(index, value)| {
            if index < stride {
                Ok(value)
            } else {
                value.checked_sub(values[index - stride]).ok_or(
                    aura_codec::AuraError::InvalidValue("structural probe residual"),
                )
            }
        })
        .collect()
}

fn dictionary_shape(values: &[i64]) -> Result<DictionaryShape, aura_codec::AuraError> {
    let base = *values
        .iter()
        .min()
        .ok_or(aura_codec::AuraError::InvalidValue(
            "structural probe values",
        ))?;
    let mut offsets = values
        .iter()
        .map(|value| {
            u64::try_from(i128::from(*value) - i128::from(base))
                .map_err(|_| aura_codec::AuraError::InvalidValue("structural probe offsets"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut unit = 0u64;
    for offset in &offsets {
        unit = gcd(unit, *offset);
    }
    let unit = i64::try_from(unit.max(1)).unwrap_or(1);
    let mut frequencies = BTreeMap::<u64, u64>::new();
    for offset in offsets.drain(..) {
        let scaled = offset / unit as u64;
        *frequencies.entry(scaled).or_default() += 1;
    }
    let entries = frequencies.keys().copied().collect::<Vec<_>>();
    let frequencies = frequencies.values().copied().collect::<Vec<_>>();
    let entry_count = u32::try_from(entries.len())
        .map_err(|_| aura_codec::AuraError::InvalidValue("structural probe dictionary"))?;
    let max_entry = entries.last().copied().unwrap_or(0);
    Ok(DictionaryShape {
        base,
        unit,
        entry_count,
        entry_width: bit_width(max_entry),
        code_width: bit_width(u64::from(entry_count.saturating_sub(1))),
        entries,
        frequencies,
    })
}

fn dictionary_ops(
    shape: &DictionaryShape,
) -> Result<(GenericStreamOp, GenericStreamOp), aura_codec::AuraError> {
    if shape.entries.is_empty() || shape.entries.len() != shape.frequencies.len() {
        return Err(aura_codec::AuraError::InvalidValue(
            "structural probe dictionary",
        ));
    }
    let lengths = huffman_code_lengths(&shape.frequencies)?;
    let packed = GenericStreamOp::PackedDictionary {
        base: shape.base,
        unit: shape.unit,
        entry_count: shape.entry_count,
        entry_width: shape.entry_width,
        code_width: shape.code_width,
    };
    let huffman = GenericStreamOp::HuffmanDictionary {
        base: shape.base,
        unit: shape.unit,
        entry_count: shape.entry_count,
        entry_width: shape.entry_width,
        code_lengths: lengths,
    };
    Ok((packed, huffman))
}

fn huffman_code_lengths(frequencies: &[u64]) -> Result<Vec<u8>, aura_codec::AuraError> {
    if frequencies.is_empty() || frequencies.contains(&0) {
        return Err(aura_codec::AuraError::InvalidValue(
            "structural probe Huffman",
        ));
    }
    if frequencies.len() == 1 {
        return Ok(vec![0]);
    }
    let mut nodes = Vec::with_capacity(frequencies.len().saturating_mul(2));
    let mut heap = BinaryHeap::new();
    for (symbol, frequency) in frequencies.iter().copied().enumerate() {
        let node_index = nodes.len();
        nodes.push(HuffmanTreeNode {
            symbol: Some(symbol),
            left: None,
            right: None,
        });
        heap.push(HuffmanHeapNode {
            frequency,
            min_symbol: symbol,
            node_index,
        });
    }
    while heap.len() > 1 {
        let left = heap.pop().ok_or(aura_codec::AuraError::InvalidValue(
            "structural probe Huffman",
        ))?;
        let right = heap.pop().ok_or(aura_codec::AuraError::InvalidValue(
            "structural probe Huffman",
        ))?;
        let node_index = nodes.len();
        nodes.push(HuffmanTreeNode {
            symbol: None,
            left: Some(left.node_index),
            right: Some(right.node_index),
        });
        heap.push(HuffmanHeapNode {
            frequency: left.frequency.saturating_add(right.frequency),
            min_symbol: left.min_symbol.min(right.min_symbol),
            node_index,
        });
    }
    let root = heap.pop().ok_or(aura_codec::AuraError::InvalidValue(
        "structural probe Huffman",
    ))?;
    let mut lengths = vec![0u8; frequencies.len()];
    let mut stack = vec![(root.node_index, 0u8)];
    while let Some((node_index, depth)) = stack.pop() {
        let node = *nodes
            .get(node_index)
            .ok_or(aura_codec::AuraError::InvalidValue(
                "structural probe Huffman",
            ))?;
        if let Some(symbol) = node.symbol {
            lengths[symbol] = depth;
            continue;
        }
        let depth = depth
            .checked_add(1)
            .ok_or(aura_codec::AuraError::InvalidValue(
                "structural probe Huffman",
            ))?;
        if depth > 64 {
            return Err(aura_codec::AuraError::InvalidValue(
                "structural probe Huffman",
            ));
        }
        if let Some(right) = node.right {
            stack.push((right, depth));
        }
        if let Some(left) = node.left {
            stack.push((left, depth));
        }
    }
    Ok(lengths)
}

#[allow(clippy::too_many_arguments)]
fn rebuild_variant(
    input: &[u8],
    metadata: &DecodedI64FileMetadata,
    old_plan: &GenericInstructionPlan,
    old_frames: &[StreamFrame],
    candidate: &Candidate,
    residual_op: &GenericStreamOp,
    mode: &'static str,
    output_path: Option<&Path>,
) -> Result<(Vec<u8>, VariantResult), aura_codec::AuraError> {
    let encode_start = Instant::now();
    let new_op = wrap_structural(candidate.shape, residual_op.clone());
    let mut new_plan = old_plan.clone();
    new_plan.streams[candidate.instruction_index].op = new_op.clone();
    let plan_bytes = new_plan.encode()?.len();
    let mut frames = old_frames.to_vec();
    let new_body = encode_generic_stream_body(
        &new_plan.streams[candidate.instruction_index],
        &GenericStreamBodyValue::I64(candidate.logical_values.clone()),
    )?;
    frames[candidate.frame_index].body = new_body;
    let body = encode_stream_frames(&frames)?;
    let footer = metadata
        .compiled_footer
        .as_ref()
        .ok_or(aura_codec::AuraError::InvalidValue(
            "structural probe footer",
        ))?
        .clone()
        .with_generic_aura0_plan(new_plan);
    let footer_bytes = footer.encode()?;
    let mut output = Vec::with_capacity(
        metadata
            .header_len
            .saturating_add(body.len())
            .saturating_add(footer_bytes.len())
            .saturating_add(12),
    );
    output.extend_from_slice(&input[..metadata.header_len]);
    output.extend_from_slice(&body);
    output.extend_from_slice(&footer_bytes);
    output.extend_from_slice(&(footer_bytes.len() as u32).to_le_bytes());
    output.extend_from_slice(SEAL_MAGIC);
    let encode_us = encode_start.elapsed().as_micros();
    let decode_start = Instant::now();
    let decoded = records::decode_i64_events_file(&output)?;
    let decode_us = decode_start.elapsed().as_micros();
    let decoded_footer =
        decoded
            .compiled_footer
            .as_ref()
            .ok_or(aura_codec::AuraError::InvalidValue(
                "structural probe footer",
            ))?;
    let footer_preserved_except_plan = footer_preserved_except_plan(
        metadata
            .compiled_footer
            .as_ref()
            .ok_or(aura_codec::AuraError::InvalidValue(
                "structural probe footer",
            ))?,
        decoded_footer,
    );
    Ok((
        output.clone(),
        VariantResult {
            mode,
            output_bytes: output.len(),
            header_bytes: metadata.header_len,
            body_bytes: body.len(),
            footer_bytes: footer_bytes.len(),
            trailer_bytes: 12,
            plan_bytes,
            selected_stream_body_bytes_before: old_frames[candidate.frame_index].body.len(),
            selected_stream_body_bytes_after: frames[candidate.frame_index].body.len(),
            encode_us,
            decode_us,
            header_preserved: output[..metadata.header_len] == input[..metadata.header_len],
            footer_preserved_except_plan,
            output_path: output_path.map(|path| path.display().to_string()),
            sha256: sha256_hex(&output),
        },
    ))
}

fn wrap_structural(shape: StructuralShape, residual_op: GenericStreamOp) -> GenericStreamOp {
    match shape {
        StructuralShape::PreviousValue => GenericStreamOp::PreviousValueDelta {
            residual_op: Box::new(residual_op),
        },
        StructuralShape::DeltaOfDelta => GenericStreamOp::DeltaOfDelta {
            residual_op: Box::new(residual_op),
        },
        StructuralShape::FixedStride(stride) => GenericStreamOp::FixedStrideDelta {
            stride,
            residual_op: Box::new(residual_op),
        },
        StructuralShape::Identity => residual_op,
    }
}

fn footer_preserved_except_plan(
    source: &aura_codec::program::CompiledFooter,
    output: &aura_codec::program::CompiledFooter,
) -> bool {
    source.container_version == output.container_version
        && source.schema == output.schema
        && source.compression == output.compression
        && source.record_count == output.record_count
        && source.block_capacity == output.block_capacity
        && source.aura0_program == output.aura0_program
        && source.aura1_program == output.aura1_program
        && source.chunks == output.chunks
        && source.aura1_byte_lanes == output.aura1_byte_lanes
}

fn variant_json(result: &VariantResult, exact_round_trip: bool) -> serde_json::Value {
    serde_json::json!({
        "mode": result.mode,
        "output_bytes": result.output_bytes,
        "header_bytes": result.header_bytes,
        "body_bytes": result.body_bytes,
        "footer_bytes": result.footer_bytes,
        "trailer_bytes": result.trailer_bytes,
        "plan_bytes": result.plan_bytes,
        "selected_stream_body_bytes_before": result.selected_stream_body_bytes_before,
        "selected_stream_body_bytes_after": result.selected_stream_body_bytes_after,
        "encode_us": result.encode_us,
        "decode_us": result.decode_us,
        "header_preserved": result.header_preserved,
        "footer_preserved_except_plan": result.footer_preserved_except_plan,
        "output_path": result.output_path,
        "sha256": result.sha256,
        "exact_round_trip": exact_round_trip,
        "complete_archive": true,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn gcd(left: u64, right: u64) -> u64 {
    let mut left = left;
    let mut right = right;
    while right != 0 {
        let next = left % right;
        left = right;
        right = next;
    }
    left
}

fn bit_width(value: u64) -> u8 {
    if value == 0 {
        0
    } else {
        (64 - value.leading_zeros()) as u8
    }
}
