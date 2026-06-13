use crate::bytes::{put_i64_le, put_u16_le, put_u32_le, put_u64_le, ByteReader};
use crate::footer::AuraFooter;
use crate::format::SEAL_MAGIC;
use crate::header::{AuraHeader, HEADER_PREFIX_SIZE};
use crate::plan::{Aura0Plan, Aura1Plan, FieldEncoding};
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

    let aura0_plan = Aura0Plan::from_schema_stats(&input.schema, &stats)?;
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

    let compiled_footer = match target_profile {
        Profile::Ingest => unreachable!(),
        Profile::Aura0 => {
            let plan = decoded.aura0_plan()?;
            let program = DecodeProgram::from_aura0_plan(&plan, decoded.schema.fields.len())?;
            CompiledFooter::new(
                target_profile,
                decoded.schema.clone(),
                decoded.rows.len() as u64,
                1,
                program,
            )?
        }
        Profile::Aura1 => {
            let plan = decoded.aura1_plan()?;
            let block_capacity = plan.block_capacity;
            let program = DecodeProgram::from_aura1_plan(&plan, decoded.schema.fields.len())?;
            CompiledFooter::new(
                target_profile,
                decoded.schema.clone(),
                decoded.rows.len() as u64,
                block_capacity,
                program,
            )?
        }
    };

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
            let plan = footer.program.to_aura0_plan()?;
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
            let plan = footer.program.to_aura1_plan(footer.block_capacity)?;
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
            .program
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
        footer.program.to_aura1_plan(footer.block_capacity)
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
    let mut previous = vec![0i64; field_count];
    for (row_index, row) in rows.iter().enumerate() {
        for field_plan in &plan.fields {
            let field_index = usize::from(field_plan.field_index);
            let value = row[field_index];
            if field_plan.encoding == FieldEncoding::DeltaPrevious && row_index == 0 {
                if value != field_plan.base_value {
                    return Err(AuraError::InvalidValue("delta previous base"));
                }
                continue;
            }
            let encoded = match field_plan.encoding {
                FieldEncoding::Absolute => value,
                FieldEncoding::DeltaBase => value - field_plan.base_value,
                FieldEncoding::DeltaPrevious => value - previous[field_index],
                FieldEncoding::TimestampStep | FieldEncoding::ImplicitFixedStep => {
                    let expected = field_plan.base_value + (row_index as i64) * field_plan.step;
                    if value != expected {
                        return Err(AuraError::InvalidValue("fixed step field"));
                    }
                    0
                }
                FieldEncoding::DeltaRelated => {
                    let reference_index = usize::from(
                        field_plan
                            .reference_field_index
                            .ok_or(AuraError::InvalidValue("reference field"))?,
                    );
                    value - row[reference_index]
                }
            };
            write_i64_width(&mut out, encoded, field_plan.width)?;
        }
        previous.clone_from(row);
    }
    Ok(out)
}

fn decode_aura0_body(
    bytes: &[u8],
    plan: &Aura0Plan,
    record_count: usize,
    field_count: usize,
) -> Result<Vec<Vec<i64>>> {
    let mut reader = ByteReader::new(bytes);
    let mut rows = Vec::with_capacity(record_count);
    let mut previous = vec![0i64; field_count];
    for row_index in 0..record_count {
        let mut row = vec![0i64; field_count];
        for field_plan in &plan.fields {
            let field_index = usize::from(field_plan.field_index);
            let value = match field_plan.encoding {
                FieldEncoding::Absolute => read_i64_width(&mut reader, field_plan.width)?,
                FieldEncoding::DeltaBase => {
                    field_plan.base_value + read_i64_width(&mut reader, field_plan.width)?
                }
                FieldEncoding::DeltaPrevious => {
                    if row_index == 0 {
                        field_plan.base_value
                    } else {
                        previous[field_index] + read_i64_width(&mut reader, field_plan.width)?
                    }
                }
                FieldEncoding::TimestampStep | FieldEncoding::ImplicitFixedStep => {
                    field_plan.base_value + (row_index as i64) * field_plan.step
                }
                FieldEncoding::DeltaRelated => {
                    let reference_index = usize::from(
                        field_plan
                            .reference_field_index
                            .ok_or(AuraError::InvalidValue("reference field"))?,
                    );
                    row[reference_index] + read_i64_width(&mut reader, field_plan.width)?
                }
            };
            row[field_index] = value;
        }
        previous.clone_from(&row);
        rows.push(row);
    }
    reader.finish()?;
    Ok(rows)
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
