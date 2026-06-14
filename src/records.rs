use crate::bitpack::{
    bitpacked_byte_len, pack_signed_values, pack_unsigned_values, unpack_signed_values,
    unpack_unsigned_values,
};
use crate::bytes::{put_i64_le, put_u16_le, put_u32_le, put_u64_le, ByteReader};
use crate::footer::AuraFooter;
use crate::format::SEAL_MAGIC;
use crate::header::{AuraHeader, HEADER_PREFIX_SIZE};
use crate::plan::{unpack_ref_divisor, unpack_two_refs, Aura0Plan, Aura1Plan, FieldEncoding};
use crate::program::{CompiledFooter, DecodeProgram};
use crate::schema::{FieldRelation, FieldRole, SchemaDescriptor, SCHEMA_MAP_TIME_SLOT};
use crate::stats::IngestStats;
use crate::{AuraError, PhysicalWidth, Profile, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I64FileInput {
    pub schema: SchemaDescriptor,
    pub rows: Vec<Vec<i64>>,
    pub stream_id: u16,
    pub dictionary_id: u16,
    pub header_comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedI64File {
    pub header: AuraHeader,
    pub schema: SchemaDescriptor,
    pub ingest_footer: Option<AuraFooter>,
    pub compiled_footer: Option<CompiledFooter>,
    pub rows: Vec<Vec<i64>>,
}

pub fn encode_ingest_i64_file(input: I64FileInput) -> Result<Vec<u8>> {
    validate_rows(&input.schema, &input.rows)?;
    let mut stats = IngestStats::new_for_schema(&input.schema)?;
    for row in &input.rows {
        stats.observe_i64_record(&input.schema, row)?;
    }
    observe_timestamp_runs(&mut stats, &input.rows);

    let aura0_plan = Aura0Plan::from_schema_rows_stats(&input.schema, &stats, &input.rows)?;
    let aura1_plan = Aura1Plan::from_stats(&stats, 1);
    let footer = AuraFooter::new(input.schema.clone(), stats)
        .with_aura0_plan(aura0_plan)
        .with_aura1_plan(aura1_plan);
    let body = encode_raw_body(input.schema.fields.len(), &input.rows)?;
    let base_time_ns = input
        .rows
        .first()
        .and_then(|row| row.first().copied())
        .unwrap_or(0);
    let header_comment = input.header_comment.as_deref().unwrap_or("");

    encode_file(
        Profile::Ingest,
        input.stream_id,
        input.dictionary_id,
        base_time_ns,
        header_comment,
        body,
        footer,
    )
}

pub fn compile_i64_file(bytes: &[u8], target_profile: Profile) -> Result<Vec<u8>> {
    if target_profile == Profile::Ingest {
        return Err(AuraError::InvalidValue("target profile"));
    }
    let decoded = decode_i64_file(bytes)?;
    let body = match target_profile {
        Profile::Ingest => unreachable!(),
        Profile::Aura0 => {
            let plan = decoded.aura0_plan()?;
            encode_aura0_body(&decoded.rows, &plan)?
        }
        Profile::Aura1 => {
            let plan = decoded.aura1_plan()?;
            encode_aura1_body(&decoded.rows, &plan)?
        }
    };

    let compiled_footer = decoded.compiled_footer_for_compile()?;

    encode_compiled_file(
        target_profile,
        decoded.header.stream_id,
        decoded.header.dictionary_id,
        decoded.header.base_time_ns,
        decoded.header.comment.as_str(),
        body,
        compiled_footer,
    )
}

pub fn decode_i64_file(bytes: &[u8]) -> Result<DecodedI64File> {
    if bytes.len() < HEADER_PREFIX_SIZE + FOOTER_LEN_SIZE + SEAL_MAGIC.len() {
        return Err(AuraError::UnexpectedEof);
    }
    let seal_offset = bytes.len() - SEAL_MAGIC.len();
    if &bytes[seal_offset..] != SEAL_MAGIC {
        return Err(AuraError::InvalidMagic {
            expected: "sealed:)",
        });
    }
    let footer_len_offset = seal_offset - FOOTER_LEN_SIZE;
    let footer_len = read_trailer_footer_len(bytes, footer_len_offset)?;
    let header_len = usize::from(bytes[7]);
    if header_len < HEADER_PREFIX_SIZE || header_len > footer_len_offset {
        return Err(AuraError::UnexpectedEof);
    }
    let header = AuraHeader::decode(&bytes[..header_len])?;
    let footer_start = footer_len_offset
        .checked_sub(footer_len)
        .ok_or(AuraError::UnexpectedEof)?;
    if footer_start < header_len {
        return Err(AuraError::UnexpectedEof);
    }
    let body = &bytes[header_len..footer_start];
    match header.profile {
        Profile::Ingest => {
            let footer = AuraFooter::decode(&bytes[footer_start..footer_len_offset])?;
            let rows = decode_raw_body(body)?;
            validate_rows(&footer.schema, &rows)?;
            Ok(DecodedI64File {
                header,
                schema: footer.schema.clone(),
                ingest_footer: Some(footer),
                compiled_footer: None,
                rows,
            })
        }
        Profile::Aura0 => {
            let footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
            let plan = footer.aura0_program.to_aura0_plan()?;
            let rows = decode_aura0_body(
                body,
                &plan,
                footer.record_count as usize,
                footer.schema.fields.len(),
            )?;
            validate_rows(&footer.schema, &rows)?;
            Ok(DecodedI64File {
                header,
                schema: footer.schema.clone(),
                ingest_footer: None,
                compiled_footer: Some(footer),
                rows,
            })
        }
        Profile::Aura1 => {
            let footer = CompiledFooter::decode(&bytes[footer_start..footer_len_offset])?;
            let plan = footer.aura1_program.to_aura1_plan(footer.block_capacity)?;
            let rows = decode_aura1_body(
                body,
                &plan,
                footer.record_count as usize,
                footer.schema.fields.len(),
            )?;
            validate_rows(&footer.schema, &rows)?;
            Ok(DecodedI64File {
                header,
                schema: footer.schema.clone(),
                ingest_footer: None,
                compiled_footer: Some(footer),
                rows,
            })
        }
    }
}

const FOOTER_LEN_SIZE: usize = 4;

fn read_trailer_footer_len(bytes: &[u8], offset: usize) -> Result<usize> {
    let end = offset
        .checked_add(FOOTER_LEN_SIZE)
        .ok_or(AuraError::UnexpectedEof)?;
    let footer_len_bytes = bytes.get(offset..end).ok_or(AuraError::UnexpectedEof)?;
    Ok(u32::from_le_bytes([
        footer_len_bytes[0],
        footer_len_bytes[1],
        footer_len_bytes[2],
        footer_len_bytes[3],
    ]) as usize)
}

impl DecodedI64File {
    fn aura0_plan(&self) -> Result<Aura0Plan> {
        if let Some(footer) = &self.ingest_footer {
            return footer
                .aura0_plan
                .clone()
                .ok_or(AuraError::InvalidValue("aura0 plan"));
        }
        self.compiled_footer
            .as_ref()
            .ok_or(AuraError::InvalidValue("compiled footer"))?
            .aura0_program
            .to_aura0_plan()
    }

    fn aura1_plan(&self) -> Result<Aura1Plan> {
        if let Some(footer) = &self.ingest_footer {
            return footer
                .aura1_plan
                .clone()
                .ok_or(AuraError::InvalidValue("aura1 plan"));
        }
        let footer = self
            .compiled_footer
            .as_ref()
            .ok_or(AuraError::InvalidValue("compiled footer"))?;
        footer.aura1_program.to_aura1_plan(footer.block_capacity)
    }

    fn compiled_footer_for_compile(&self) -> Result<CompiledFooter> {
        if let Some(footer) = &self.compiled_footer {
            return Ok(footer.clone());
        }
        let aura0_plan = self.aura0_plan()?;
        let aura1_plan = self.aura1_plan()?;
        let block_capacity = aura1_plan.block_capacity;
        let field_count = self.schema.fields.len();
        CompiledFooter::new(
            self.schema.clone(),
            self.rows.len() as u64,
            block_capacity,
            DecodeProgram::from_aura0_plan(&aura0_plan, field_count)?,
            DecodeProgram::from_aura1_plan(&aura1_plan, field_count)?,
        )
    }
}

fn encode_file(
    profile: Profile,
    stream_id: u16,
    dictionary_id: u16,
    base_time_ns: i64,
    header_comment: &str,
    body: Vec<u8>,
    footer: AuraFooter,
) -> Result<Vec<u8>> {
    let footer_bytes = footer.encode()?;
    let footer_len =
        u32::try_from(footer_bytes.len()).map_err(|_| AuraError::InvalidValue("footer length"))?;
    let header = AuraHeader::new(profile)
        .with_stream(stream_id, dictionary_id, base_time_ns)
        .with_schema_mapping(schema_parent_mapping(&footer.schema)?)?
        .with_comment(header_comment)?;
    let header_bytes = header.encode()?;

    let mut out = Vec::with_capacity(
        header_bytes.len() + body.len() + footer_bytes.len() + FOOTER_LEN_SIZE + SEAL_MAGIC.len(),
    );
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&body);
    out.extend_from_slice(&footer_bytes);
    put_u32_le(&mut out, footer_len);
    out.extend_from_slice(SEAL_MAGIC);
    Ok(out)
}

fn encode_compiled_file(
    profile: Profile,
    stream_id: u16,
    dictionary_id: u16,
    base_time_ns: i64,
    header_comment: &str,
    body: Vec<u8>,
    footer: CompiledFooter,
) -> Result<Vec<u8>> {
    let footer_bytes = footer.encode()?;
    let footer_len =
        u32::try_from(footer_bytes.len()).map_err(|_| AuraError::InvalidValue("footer length"))?;
    let header = AuraHeader::new(profile)
        .with_stream(stream_id, dictionary_id, base_time_ns)
        .with_schema_mapping(schema_parent_mapping(&footer.schema)?)?
        .with_comment(header_comment)?;
    let header_bytes = header.encode()?;

    let mut out = Vec::with_capacity(
        header_bytes.len() + body.len() + footer_bytes.len() + FOOTER_LEN_SIZE + SEAL_MAGIC.len(),
    );
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&body);
    out.extend_from_slice(&footer_bytes);
    put_u32_le(&mut out, footer_len);
    out.extend_from_slice(SEAL_MAGIC);
    Ok(out)
}

fn schema_parent_mapping(schema: &SchemaDescriptor) -> Result<Vec<u8>> {
    let mut mapping = Vec::with_capacity(schema.fields.len());
    for (position, field) in schema.fields.iter().enumerate() {
        if usize::from(field.index) != position {
            return Err(AuraError::InvalidValue("schema field index"));
        }
        let parent = match (field.role, field.relation) {
            (FieldRole::Timestamp, FieldRelation::None) => SCHEMA_MAP_TIME_SLOT,
            (FieldRole::Timestamp, FieldRelation::DeltaFromField(_)) => {
                return Err(AuraError::InvalidValue("schema time mapping"));
            }
            (_, FieldRelation::None) => 0,
            (_, FieldRelation::DeltaFromField(parent_index)) => {
                let parent_slot = parent_index
                    .checked_add(1)
                    .ok_or(AuraError::InvalidValue("schema parent mapping"))?;
                if parent_slot >= u16::from(SCHEMA_MAP_TIME_SLOT) {
                    return Err(AuraError::InvalidValue("schema parent mapping"));
                }
                parent_slot as u8
            }
        };
        mapping.push(parent);
    }
    Ok(mapping)
}

fn encode_raw_body(field_count: usize, rows: &[Vec<i64>]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    put_u64_le(&mut out, rows.len() as u64);
    put_u16_len(&mut out, field_count, "field count")?;
    for row in rows {
        for value in row {
            put_i64_le(&mut out, *value);
        }
    }
    Ok(out)
}

fn decode_raw_body(bytes: &[u8]) -> Result<Vec<Vec<i64>>> {
    let mut reader = ByteReader::new(bytes);
    let record_count = reader.read_u64_le()? as usize;
    let field_count = reader.read_u16_le()? as usize;
    let mut rows = Vec::with_capacity(record_count);
    for _ in 0..record_count {
        let mut row = Vec::with_capacity(field_count);
        for _ in 0..field_count {
            row.push(reader.read_i64_le()?);
        }
        rows.push(row);
    }
    reader.finish()?;
    Ok(rows)
}

fn encode_aura0_body(rows: &[Vec<i64>], plan: &Aura0Plan) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let field_count = rows.first().map(|row| row.len()).unwrap_or(0);
    for field_plan in &plan.fields {
        let field_index = usize::from(field_plan.field_index);
        if field_index >= field_count {
            return Err(AuraError::InvalidValue("field index"));
        }
        encode_aura0_column(&mut out, rows, field_plan)?;
    }
    Ok(out)
}

fn encode_aura0_column(
    out: &mut Vec<u8>,
    rows: &[Vec<i64>],
    field_plan: &crate::PhysicalFieldPlan,
) -> Result<()> {
    let field_index = usize::from(field_plan.field_index);
    match field_plan.encoding {
        FieldEncoding::Absolute => {
            for row in rows {
                write_i64_width(out, row[field_index], field_plan.width)?;
            }
        }
        FieldEncoding::DeltaBase => {
            for row in rows {
                write_i64_width(
                    out,
                    checked_delta(row[field_index], field_plan.base_value)?,
                    field_plan.width,
                )?;
            }
        }
        FieldEncoding::DeltaPrevious => {
            if let Some(first) = rows.first() {
                if first[field_index] != field_plan.base_value {
                    return Err(AuraError::InvalidValue("delta previous base"));
                }
            }
            for pair in rows.windows(2) {
                write_i64_width(
                    out,
                    checked_delta(pair[1][field_index], pair[0][field_index])?,
                    field_plan.width,
                )?;
            }
        }
        FieldEncoding::TimestampStep | FieldEncoding::ImplicitFixedStep => {
            for (row_index, row) in rows.iter().enumerate() {
                let expected =
                    checked_step_value(field_plan.base_value, row_index, field_plan.step)?;
                if row[field_index] != expected {
                    return Err(AuraError::InvalidValue("fixed step field"));
                }
            }
        }
        FieldEncoding::DeltaRelated => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            for row in rows {
                write_i64_width(
                    out,
                    checked_delta(row[field_index], row[reference_index])?,
                    field_plan.width,
                )?;
            }
        }
        FieldEncoding::DerivedOffset => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            for row in rows {
                let expected = checked_sum(row[reference_index], field_plan.base_value)?;
                if row[field_index] != expected {
                    return Err(AuraError::InvalidValue("derived offset field"));
                }
            }
        }
        FieldEncoding::BitpackedDeltaPreviousFieldOffset => {
            let reference_index = field_reference_index(field_plan, rows, field_index)?;
            if let Some(first) = rows.first() {
                if first[field_index] != field_plan.base_value {
                    return Err(AuraError::InvalidValue("delta previous base"));
                }
            }
            let values = rows
                .iter()
                .skip(1)
                .zip(rows)
                .map(|(row, previous_row)| {
                    checked_biased_delta(
                        row[field_index],
                        previous_row[reference_index],
                        field_plan.step,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_unsigned_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedDeltaPrevious => {
            if let Some(first) = rows.first() {
                if first[field_index] != field_plan.base_value {
                    return Err(AuraError::InvalidValue("delta previous base"));
                }
            }
            let values = rows
                .windows(2)
                .map(|pair| checked_delta(pair[1][field_index], pair[0][field_index]))
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_signed_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedDeltaBase => {
            let values = rows
                .iter()
                .map(|row| checked_unsigned_delta(row[field_index], field_plan.base_value))
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_unsigned_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedDeltaRelated => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            let values = rows
                .iter()
                .map(|row| checked_delta(row[field_index], row[reference_index]))
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_signed_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedDeltaRelatedOffset => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            let values = rows
                .iter()
                .map(|row| {
                    checked_biased_delta(
                        row[field_index],
                        row[reference_index],
                        field_plan.base_value,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_unsigned_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedDeltaPreviousOffset => {
            if let Some(first) = rows.first() {
                if first[field_index] != field_plan.base_value {
                    return Err(AuraError::InvalidValue("delta previous base"));
                }
            }
            let values = rows
                .windows(2)
                .map(|pair| {
                    checked_biased_delta(
                        pair[1][field_index],
                        pair[0][field_index],
                        field_plan.step,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_unsigned_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedCandleMaxOffset | FieldEncoding::BitpackedCandleMinOffset => {
            let first_reference_index = field_reference_index(field_plan, rows, field_index)?;
            let second_reference_index = step_reference_index(field_plan, rows)?;
            let values = rows
                .iter()
                .map(|row| {
                    let reference = match field_plan.encoding {
                        FieldEncoding::BitpackedCandleMaxOffset => {
                            row[first_reference_index].max(row[second_reference_index])
                        }
                        FieldEncoding::BitpackedCandleMinOffset => {
                            row[first_reference_index].min(row[second_reference_index])
                        }
                        _ => unreachable!(),
                    };
                    match field_plan.encoding {
                        FieldEncoding::BitpackedCandleMaxOffset => {
                            checked_biased_delta(row[field_index], reference, field_plan.base_value)
                        }
                        FieldEncoding::BitpackedCandleMinOffset => {
                            checked_biased_delta(reference, row[field_index], field_plan.base_value)
                        }
                        _ => unreachable!(),
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_unsigned_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedProductResidual => {
            let quantity_index = field_reference_index(field_plan, rows, field_index)?;
            let (price_index, divisor) = product_args(field_plan, rows)?;
            let values = rows
                .iter()
                .map(|row| {
                    let predicted =
                        checked_product_div(row[quantity_index], row[price_index], divisor)?;
                    checked_biased_i128_delta(row[field_index], predicted, field_plan.base_value)
                })
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_unsigned_values(&values, field_plan.bit_width)?);
        }
        FieldEncoding::BitpackedProportionalResidual => {
            let total_value_index = field_reference_index(field_plan, rows, field_index)?;
            let (child_quantity_index, total_quantity_index) = proportional_args(field_plan, rows)?;
            let values = rows
                .iter()
                .map(|row| {
                    let predicted = checked_product_div(
                        row[total_value_index],
                        row[child_quantity_index],
                        row[total_quantity_index],
                    )?;
                    checked_biased_i128_delta(row[field_index], predicted, field_plan.base_value)
                })
                .collect::<Result<Vec<_>>>()?;
            out.extend_from_slice(&pack_unsigned_values(&values, field_plan.bit_width)?);
        }
    }
    Ok(())
}

fn decode_aura0_body(
    bytes: &[u8],
    plan: &Aura0Plan,
    record_count: usize,
    field_count: usize,
) -> Result<Vec<Vec<i64>>> {
    let mut reader = ByteReader::new(bytes);
    let mut rows = Vec::with_capacity(record_count);
    rows.resize_with(record_count, || vec![0i64; field_count]);
    let mut pending = Vec::new();
    for field_plan in &plan.fields {
        let field_index = usize::from(field_plan.field_index);
        if field_index >= field_count {
            return Err(AuraError::InvalidValue("field index"));
        }
        match field_plan.encoding {
            FieldEncoding::BitpackedDeltaPreviousFieldOffset => {
                let values = read_bitpacked_unsigned_values(
                    &mut reader,
                    field_plan.bit_width,
                    rows.len().saturating_sub(1),
                )?;
                pending.push((*field_plan, values));
            }
            FieldEncoding::BitpackedDeltaRelatedOffset
            | FieldEncoding::BitpackedProductResidual
            | FieldEncoding::BitpackedProportionalResidual => {
                let values =
                    read_bitpacked_unsigned_values(&mut reader, field_plan.bit_width, rows.len())?;
                pending.push((*field_plan, values));
            }
            FieldEncoding::BitpackedCandleMaxOffset | FieldEncoding::BitpackedCandleMinOffset => {
                let values =
                    read_bitpacked_unsigned_values(&mut reader, field_plan.bit_width, rows.len())?;
                pending.push((*field_plan, values));
            }
            _ => decode_aura0_column(&mut reader, &mut rows, field_plan)?,
        }
    }
    if rows.is_empty() {
        reader.finish()?;
        return Ok(rows);
    }
    let mut consumed = vec![false; pending.len()];
    for previous_index in 0..pending.len() {
        let (previous_plan, previous_values) = &pending[previous_index];
        if previous_plan.encoding != FieldEncoding::BitpackedDeltaPreviousFieldOffset {
            continue;
        }
        let Some(related_index) = pending.iter().position(|(related_plan, _)| {
            related_plan.encoding == FieldEncoding::BitpackedDeltaRelatedOffset
                && Some(related_plan.field_index) == previous_plan.reference_field_index
                && related_plan.reference_field_index == Some(previous_plan.field_index)
        }) else {
            decode_pending_previous_field_offset(&mut rows, previous_plan, previous_values)?;
            consumed[previous_index] = true;
            continue;
        };
        let (related_plan, related_values) = &pending[related_index];
        decode_pending_previous_related_pair(
            &mut rows,
            previous_plan,
            previous_values,
            related_plan,
            related_values,
        )?;
        consumed[previous_index] = true;
        consumed[related_index] = true;
    }
    for (index, (field_plan, values)) in pending.iter().enumerate() {
        if consumed[index] {
            continue;
        }
        match field_plan.encoding {
            FieldEncoding::BitpackedDeltaPreviousFieldOffset => {
                decode_pending_previous_field_offset(&mut rows, field_plan, values)?;
                consumed[index] = true;
            }
            FieldEncoding::BitpackedDeltaRelatedOffset => {
                decode_pending_related_offset(&mut rows, field_plan, values)?;
                consumed[index] = true;
            }
            _ => {}
        }
    }
    for (index, (field_plan, values)) in pending.iter().enumerate() {
        if consumed[index] {
            continue;
        }
        if field_plan.encoding == FieldEncoding::BitpackedProductResidual {
            decode_pending_product_residual(&mut rows, field_plan, values)?;
            consumed[index] = true;
        }
    }
    for (index, (field_plan, values)) in pending.iter().enumerate() {
        if consumed[index] {
            continue;
        }
        if field_plan.encoding == FieldEncoding::BitpackedProportionalResidual {
            decode_pending_proportional_residual(&mut rows, field_plan, values)?;
            consumed[index] = true;
        }
    }
    for (index, (field_plan, values)) in pending.iter().enumerate() {
        if consumed[index] {
            continue;
        }
        if matches!(
            field_plan.encoding,
            FieldEncoding::BitpackedCandleMaxOffset | FieldEncoding::BitpackedCandleMinOffset
        ) {
            decode_pending_aura0_column(&mut rows, field_plan, values)?;
            consumed[index] = true;
        }
    }
    reader.finish()?;
    Ok(rows)
}

fn decode_aura0_column(
    reader: &mut ByteReader<'_>,
    rows: &mut [Vec<i64>],
    field_plan: &crate::PhysicalFieldPlan,
) -> Result<()> {
    let field_index = usize::from(field_plan.field_index);
    match field_plan.encoding {
        FieldEncoding::Absolute => {
            for row in rows {
                row[field_index] = read_i64_width(reader, field_plan.width)?;
            }
        }
        FieldEncoding::DeltaBase => {
            for row in rows {
                row[field_index] = checked_sum(
                    field_plan.base_value,
                    read_i64_width(reader, field_plan.width)?,
                )?;
            }
        }
        FieldEncoding::DeltaPrevious => {
            if let Some(first) = rows.first_mut() {
                first[field_index] = field_plan.base_value;
            }
            for row_index in 1..rows.len() {
                let delta = read_i64_width(reader, field_plan.width)?;
                rows[row_index][field_index] =
                    checked_sum(rows[row_index - 1][field_index], delta)?;
            }
        }
        FieldEncoding::TimestampStep | FieldEncoding::ImplicitFixedStep => {
            for (row_index, row) in rows.iter_mut().enumerate() {
                row[field_index] =
                    checked_step_value(field_plan.base_value, row_index, field_plan.step)?;
            }
        }
        FieldEncoding::DeltaRelated => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            for row in rows {
                row[field_index] = checked_sum(
                    row[reference_index],
                    read_i64_width(reader, field_plan.width)?,
                )?;
            }
        }
        FieldEncoding::DerivedOffset => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            for row in rows {
                row[field_index] = checked_sum(row[reference_index], field_plan.base_value)?;
            }
        }
        FieldEncoding::BitpackedDeltaPreviousFieldOffset => {
            let reference_index = field_reference_index_for_count(field_plan, rows[0].len())?;
            if let Some(first) = rows.first_mut() {
                first[field_index] = field_plan.base_value;
            }
            let deltas = read_bitpacked_unsigned_values(
                reader,
                field_plan.bit_width,
                rows.len().saturating_sub(1),
            )?;
            for (offset, delta) in deltas.into_iter().enumerate() {
                let row_index = offset + 1;
                let delta = checked_sum_unsigned(field_plan.step, delta)?;
                rows[row_index][field_index] =
                    checked_sum(rows[row_index - 1][reference_index], delta)?;
            }
        }
        FieldEncoding::BitpackedDeltaPrevious => {
            if let Some(first) = rows.first_mut() {
                first[field_index] = field_plan.base_value;
            }
            let deltas =
                read_bitpacked_values(reader, field_plan.bit_width, rows.len().saturating_sub(1))?;
            for (offset, delta) in deltas.into_iter().enumerate() {
                let row_index = offset + 1;
                rows[row_index][field_index] =
                    checked_sum(rows[row_index - 1][field_index], delta)?;
            }
        }
        FieldEncoding::BitpackedDeltaBase => {
            let deltas = read_bitpacked_unsigned_values(reader, field_plan.bit_width, rows.len())?;
            for (row, delta) in rows.iter_mut().zip(deltas) {
                row[field_index] = checked_sum_unsigned(field_plan.base_value, delta)?;
            }
        }
        FieldEncoding::BitpackedDeltaRelated => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            let deltas = read_bitpacked_values(reader, field_plan.bit_width, rows.len())?;
            for (row, delta) in rows.iter_mut().zip(deltas) {
                row[field_index] = checked_sum(row[reference_index], delta)?;
            }
        }
        FieldEncoding::BitpackedDeltaRelatedOffset => {
            let reference_index = related_reference_index(field_plan, field_index)?;
            let deltas = read_bitpacked_unsigned_values(reader, field_plan.bit_width, rows.len())?;
            for (row, delta) in rows.iter_mut().zip(deltas) {
                let delta = checked_sum_unsigned(field_plan.base_value, delta)?;
                row[field_index] = checked_sum(row[reference_index], delta)?;
            }
        }
        FieldEncoding::BitpackedDeltaPreviousOffset => {
            if let Some(first) = rows.first_mut() {
                first[field_index] = field_plan.base_value;
            }
            let deltas = read_bitpacked_unsigned_values(
                reader,
                field_plan.bit_width,
                rows.len().saturating_sub(1),
            )?;
            for (offset, delta) in deltas.into_iter().enumerate() {
                let row_index = offset + 1;
                let delta = checked_sum_unsigned(field_plan.step, delta)?;
                rows[row_index][field_index] =
                    checked_sum(rows[row_index - 1][field_index], delta)?;
            }
        }
        FieldEncoding::BitpackedCandleMaxOffset | FieldEncoding::BitpackedCandleMinOffset => {
            return Err(AuraError::InvalidValue("pending field"));
        }
        FieldEncoding::BitpackedProductResidual => {
            let quantity_index = field_reference_index_for_count(field_plan, rows[0].len())?;
            let (price_index, divisor) = product_args_for_count(field_plan, rows[0].len())?;
            let residuals =
                read_bitpacked_unsigned_values(reader, field_plan.bit_width, rows.len())?;
            for (row, residual) in rows.iter_mut().zip(residuals) {
                let predicted =
                    checked_product_div(row[quantity_index], row[price_index], divisor)?;
                row[field_index] = checked_i128_sum_unsigned(
                    predicted + i128::from(field_plan.base_value),
                    residual,
                )?;
            }
        }
        FieldEncoding::BitpackedProportionalResidual => {
            let total_value_index = field_reference_index_for_count(field_plan, rows[0].len())?;
            let (child_quantity_index, total_quantity_index) =
                proportional_args_for_count(field_plan, rows[0].len())?;
            let residuals =
                read_bitpacked_unsigned_values(reader, field_plan.bit_width, rows.len())?;
            for (row, residual) in rows.iter_mut().zip(residuals) {
                let predicted = checked_product_div(
                    row[total_value_index],
                    row[child_quantity_index],
                    row[total_quantity_index],
                )?;
                row[field_index] = checked_i128_sum_unsigned(
                    predicted + i128::from(field_plan.base_value),
                    residual,
                )?;
            }
        }
    }
    Ok(())
}

fn decode_pending_aura0_column(
    rows: &mut [Vec<i64>],
    field_plan: &crate::PhysicalFieldPlan,
    values: &[u64],
) -> Result<()> {
    let field_index = usize::from(field_plan.field_index);
    let first_reference_index = field_reference_index_for_count(field_plan, rows[0].len())?;
    let second_reference_index = step_reference_index_for_count(field_plan, rows[0].len())?;
    for (row, residual) in rows.iter_mut().zip(values.iter().copied()) {
        let reference = match field_plan.encoding {
            FieldEncoding::BitpackedCandleMaxOffset => {
                row[first_reference_index].max(row[second_reference_index])
            }
            FieldEncoding::BitpackedCandleMinOffset => {
                row[first_reference_index].min(row[second_reference_index])
            }
            _ => return Err(AuraError::InvalidValue("pending field")),
        };
        let delta = checked_sum_unsigned(field_plan.base_value, residual)?;
        row[field_index] = match field_plan.encoding {
            FieldEncoding::BitpackedCandleMaxOffset => checked_sum(reference, delta)?,
            FieldEncoding::BitpackedCandleMinOffset => reference
                .checked_sub(delta)
                .ok_or(AuraError::InvalidValue("delta value"))?,
            _ => unreachable!(),
        };
    }
    Ok(())
}

fn decode_pending_previous_field_offset(
    rows: &mut [Vec<i64>],
    field_plan: &crate::PhysicalFieldPlan,
    values: &[u64],
) -> Result<()> {
    let field_index = usize::from(field_plan.field_index);
    let reference_index = field_reference_index_for_count(field_plan, rows[0].len())?;
    if let Some(first) = rows.first_mut() {
        first[field_index] = field_plan.base_value;
    }
    for (offset, residual) in values.iter().copied().enumerate() {
        let row_index = offset + 1;
        let delta = checked_sum_unsigned(field_plan.step, residual)?;
        rows[row_index][field_index] = checked_sum(rows[row_index - 1][reference_index], delta)?;
    }
    Ok(())
}

fn decode_pending_previous_related_pair(
    rows: &mut [Vec<i64>],
    previous_plan: &crate::PhysicalFieldPlan,
    previous_values: &[u64],
    related_plan: &crate::PhysicalFieldPlan,
    related_values: &[u64],
) -> Result<()> {
    let previous_field_index = usize::from(previous_plan.field_index);
    let related_field_index = usize::from(related_plan.field_index);
    if related_values.len() != rows.len()
        || previous_values.len() != rows.len().saturating_sub(1)
        || previous_plan.reference_field_index != Some(related_plan.field_index)
        || related_plan.reference_field_index != Some(previous_plan.field_index)
    {
        return Err(AuraError::InvalidValue("candle pair"));
    }
    if let Some(first) = rows.first_mut() {
        first[previous_field_index] = previous_plan.base_value;
    }
    for row_index in 0..rows.len() {
        if row_index > 0 {
            let delta = checked_sum_unsigned(previous_plan.step, previous_values[row_index - 1])?;
            rows[row_index][previous_field_index] =
                checked_sum(rows[row_index - 1][related_field_index], delta)?;
        }
        let related_delta =
            checked_sum_unsigned(related_plan.base_value, related_values[row_index])?;
        rows[row_index][related_field_index] =
            checked_sum(rows[row_index][previous_field_index], related_delta)?;
    }
    Ok(())
}

fn decode_pending_related_offset(
    rows: &mut [Vec<i64>],
    field_plan: &crate::PhysicalFieldPlan,
    values: &[u64],
) -> Result<()> {
    let field_index = usize::from(field_plan.field_index);
    let reference_index = field_reference_index_for_count(field_plan, rows[0].len())?;
    if values.len() != rows.len() {
        return Err(AuraError::InvalidValue("pending field"));
    }
    for (row, residual) in rows.iter_mut().zip(values.iter().copied()) {
        let delta = checked_sum_unsigned(field_plan.base_value, residual)?;
        row[field_index] = checked_sum(row[reference_index], delta)?;
    }
    Ok(())
}

fn decode_pending_product_residual(
    rows: &mut [Vec<i64>],
    field_plan: &crate::PhysicalFieldPlan,
    values: &[u64],
) -> Result<()> {
    let field_index = usize::from(field_plan.field_index);
    let quantity_index = field_reference_index_for_count(field_plan, rows[0].len())?;
    let (price_index, divisor) = product_args_for_count(field_plan, rows[0].len())?;
    if values.len() != rows.len() {
        return Err(AuraError::InvalidValue("pending field"));
    }
    for (row, residual) in rows.iter_mut().zip(values.iter().copied()) {
        let predicted = checked_product_div(row[quantity_index], row[price_index], divisor)?;
        row[field_index] =
            checked_i128_sum_unsigned(predicted + i128::from(field_plan.base_value), residual)?;
    }
    Ok(())
}

fn decode_pending_proportional_residual(
    rows: &mut [Vec<i64>],
    field_plan: &crate::PhysicalFieldPlan,
    values: &[u64],
) -> Result<()> {
    let field_index = usize::from(field_plan.field_index);
    let total_value_index = field_reference_index_for_count(field_plan, rows[0].len())?;
    let (child_quantity_index, total_quantity_index) =
        proportional_args_for_count(field_plan, rows[0].len())?;
    if values.len() != rows.len() {
        return Err(AuraError::InvalidValue("pending field"));
    }
    for (row, residual) in rows.iter_mut().zip(values.iter().copied()) {
        let predicted = checked_product_div(
            row[total_value_index],
            row[child_quantity_index],
            row[total_quantity_index],
        )?;
        row[field_index] =
            checked_i128_sum_unsigned(predicted + i128::from(field_plan.base_value), residual)?;
    }
    Ok(())
}

fn read_bitpacked_values(
    reader: &mut ByteReader<'_>,
    bit_width: u8,
    value_count: usize,
) -> Result<Vec<i64>> {
    let byte_len = bitpacked_byte_len(value_count as u64, bit_width) as usize;
    unpack_signed_values(reader.read_exact(byte_len)?, bit_width, value_count)
}

fn read_bitpacked_unsigned_values(
    reader: &mut ByteReader<'_>,
    bit_width: u8,
    value_count: usize,
) -> Result<Vec<u64>> {
    let byte_len = bitpacked_byte_len(value_count as u64, bit_width) as usize;
    unpack_unsigned_values(reader.read_exact(byte_len)?, bit_width, value_count)
}

fn related_reference_index(
    field_plan: &crate::PhysicalFieldPlan,
    field_index: usize,
) -> Result<usize> {
    let reference_index = usize::from(
        field_plan
            .reference_field_index
            .ok_or(AuraError::InvalidValue("reference field"))?,
    );
    if reference_index >= field_index {
        return Err(AuraError::InvalidValue("reference field order"));
    }
    Ok(reference_index)
}

fn field_reference_index(
    field_plan: &crate::PhysicalFieldPlan,
    rows: &[Vec<i64>],
    field_index: usize,
) -> Result<usize> {
    let field_count = rows.first().map(|row| row.len()).unwrap_or(0);
    let reference_index = field_reference_index_for_count(field_plan, field_count)?;
    if reference_index == field_index {
        return Err(AuraError::InvalidValue("reference field"));
    }
    Ok(reference_index)
}

fn field_reference_index_for_count(
    field_plan: &crate::PhysicalFieldPlan,
    field_count: usize,
) -> Result<usize> {
    let reference_index = usize::from(
        field_plan
            .reference_field_index
            .ok_or(AuraError::InvalidValue("reference field"))?,
    );
    if reference_index >= field_count {
        return Err(AuraError::InvalidValue("reference field"));
    }
    Ok(reference_index)
}

fn step_reference_index(field_plan: &crate::PhysicalFieldPlan, rows: &[Vec<i64>]) -> Result<usize> {
    let field_count = rows.first().map(|row| row.len()).unwrap_or(0);
    step_reference_index_for_count(field_plan, field_count)
}

fn step_reference_index_for_count(
    field_plan: &crate::PhysicalFieldPlan,
    field_count: usize,
) -> Result<usize> {
    let index =
        usize::try_from(field_plan.step).map_err(|_| AuraError::InvalidValue("reference field"))?;
    if index >= field_count {
        return Err(AuraError::InvalidValue("reference field"));
    }
    Ok(index)
}

fn product_args(field_plan: &crate::PhysicalFieldPlan, rows: &[Vec<i64>]) -> Result<(usize, i64)> {
    let field_count = rows.first().map(|row| row.len()).unwrap_or(0);
    product_args_for_count(field_plan, field_count)
}

fn product_args_for_count(
    field_plan: &crate::PhysicalFieldPlan,
    field_count: usize,
) -> Result<(usize, i64)> {
    let (reference, divisor) =
        unpack_ref_divisor(field_plan.step).ok_or(AuraError::InvalidValue("product args"))?;
    let reference = usize::from(reference);
    if reference >= field_count {
        return Err(AuraError::InvalidValue("reference field"));
    }
    Ok((reference, i64::from(divisor)))
}

fn proportional_args(
    field_plan: &crate::PhysicalFieldPlan,
    rows: &[Vec<i64>],
) -> Result<(usize, usize)> {
    let field_count = rows.first().map(|row| row.len()).unwrap_or(0);
    proportional_args_for_count(field_plan, field_count)
}

fn proportional_args_for_count(
    field_plan: &crate::PhysicalFieldPlan,
    field_count: usize,
) -> Result<(usize, usize)> {
    let (first, second) =
        unpack_two_refs(field_plan.step).ok_or(AuraError::InvalidValue("proportional args"))?;
    let first = usize::from(first);
    let second = usize::from(second);
    if first >= field_count || second >= field_count {
        return Err(AuraError::InvalidValue("reference field"));
    }
    Ok((first, second))
}

fn checked_delta(value: i64, reference: i64) -> Result<i64> {
    value
        .checked_sub(reference)
        .ok_or(AuraError::InvalidValue("delta value"))
}

fn checked_unsigned_delta(value: i64, reference: i64) -> Result<u64> {
    let delta = i128::from(value) - i128::from(reference);
    u64::try_from(delta).map_err(|_| AuraError::InvalidValue("delta value"))
}

fn checked_biased_delta(value: i64, reference: i64, bias: i64) -> Result<u64> {
    let delta = i128::from(value) - i128::from(reference) - i128::from(bias);
    u64::try_from(delta).map_err(|_| AuraError::InvalidValue("delta value"))
}

fn checked_biased_i128_delta(value: i64, reference: i128, bias: i64) -> Result<u64> {
    let delta = i128::from(value) - reference - i128::from(bias);
    u64::try_from(delta).map_err(|_| AuraError::InvalidValue("delta value"))
}

fn checked_sum(value: i64, delta: i64) -> Result<i64> {
    value
        .checked_add(delta)
        .ok_or(AuraError::InvalidValue("delta value"))
}

fn checked_sum_unsigned(value: i64, delta: u64) -> Result<i64> {
    let sum = i128::from(value) + i128::from(delta);
    i64::try_from(sum).map_err(|_| AuraError::InvalidValue("delta value"))
}

fn checked_i128_sum_unsigned(value: i128, delta: u64) -> Result<i64> {
    let sum = value
        .checked_add(i128::from(delta))
        .ok_or(AuraError::InvalidValue("delta value"))?;
    i64::try_from(sum).map_err(|_| AuraError::InvalidValue("delta value"))
}

fn checked_product_div(left: i64, right: i64, divisor: i64) -> Result<i128> {
    if divisor == 0 {
        return Err(AuraError::InvalidValue("product divisor"));
    }
    i128::from(left)
        .checked_mul(i128::from(right))
        .and_then(|value| value.checked_div(i128::from(divisor)))
        .ok_or(AuraError::InvalidValue("product value"))
}

fn checked_step_value(base: i64, row_index: usize, step: i64) -> Result<i64> {
    let offset = step
        .checked_mul(i64::try_from(row_index).map_err(|_| AuraError::InvalidValue("row index"))?)
        .ok_or(AuraError::InvalidValue("fixed step field"))?;
    checked_sum(base, offset)
}

fn encode_aura1_body(rows: &[Vec<i64>], plan: &Aura1Plan) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for row in rows {
        for field_plan in &plan.fields {
            write_i64_width(
                &mut out,
                row[usize::from(field_plan.field_index)],
                field_plan.width,
            )?;
        }
    }
    Ok(out)
}

fn decode_aura1_body(
    bytes: &[u8],
    plan: &Aura1Plan,
    record_count: usize,
    field_count: usize,
) -> Result<Vec<Vec<i64>>> {
    let mut reader = ByteReader::new(bytes);
    let mut rows = Vec::with_capacity(record_count);
    for _ in 0..record_count {
        let mut row = vec![0i64; field_count];
        for field_plan in &plan.fields {
            row[usize::from(field_plan.field_index)] =
                read_i64_width(&mut reader, field_plan.width)?;
        }
        rows.push(row);
    }
    reader.finish()?;
    Ok(rows)
}

fn write_i64_width(out: &mut Vec<u8>, value: i64, width: PhysicalWidth) -> Result<()> {
    match width {
        PhysicalWidth::Zero => {
            if value == 0 {
                Ok(())
            } else {
                Err(AuraError::InvalidValue("zero-width value"))
            }
        }
        PhysicalWidth::I8 => {
            let value = i8::try_from(value).map_err(|_| AuraError::InvalidValue("i8 value"))?;
            out.push(value as u8);
            Ok(())
        }
        PhysicalWidth::I16 => {
            let value = i16::try_from(value).map_err(|_| AuraError::InvalidValue("i16 value"))?;
            out.extend_from_slice(&value.to_le_bytes());
            Ok(())
        }
        PhysicalWidth::I32 => {
            let value = i32::try_from(value).map_err(|_| AuraError::InvalidValue("i32 value"))?;
            out.extend_from_slice(&value.to_le_bytes());
            Ok(())
        }
        PhysicalWidth::I64 => {
            put_i64_le(out, value);
            Ok(())
        }
        PhysicalWidth::I128 => {
            out.extend_from_slice(&i128::from(value).to_le_bytes());
            Ok(())
        }
    }
}

fn read_i64_width(reader: &mut ByteReader<'_>, width: PhysicalWidth) -> Result<i64> {
    match width {
        PhysicalWidth::Zero => Ok(0),
        PhysicalWidth::I8 => Ok(reader.read_u8()? as i8 as i64),
        PhysicalWidth::I16 => {
            let bytes = reader.read_exact(2)?;
            Ok(i16::from_le_bytes([bytes[0], bytes[1]]) as i64)
        }
        PhysicalWidth::I32 => {
            let bytes = reader.read_exact(4)?;
            Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i64)
        }
        PhysicalWidth::I64 => reader.read_i64_le(),
        PhysicalWidth::I128 => {
            let bytes = reader.read_exact(16)?;
            let value = i128::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]);
            i64::try_from(value).map_err(|_| AuraError::InvalidValue("i128 value"))
        }
    }
}

fn validate_rows(schema: &SchemaDescriptor, rows: &[Vec<i64>]) -> Result<()> {
    for row in rows {
        if row.len() != schema.fields.len() {
            return Err(AuraError::InvalidValue("record field count"));
        }
    }
    Ok(())
}

fn observe_timestamp_runs(stats: &mut IngestStats, rows: &[Vec<i64>]) {
    let mut previous_ts = None;
    let mut run_len = 0u32;
    for row in rows {
        let ts = row.first().copied();
        if ts == previous_ts {
            run_len += 1;
        } else {
            stats.observe_timestamp_run(run_len);
            previous_ts = ts;
            run_len = 1;
        }
    }
    stats.observe_timestamp_run(run_len);
}

fn put_u16_len(out: &mut Vec<u8>, len: usize, name: &'static str) -> Result<()> {
    let len = u16::try_from(len).map_err(|_| AuraError::InvalidValue(name))?;
    put_u16_le(out, len);
    Ok(())
}
