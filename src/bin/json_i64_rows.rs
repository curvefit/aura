use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use aura_codec::records::{
    compile_i64_file, decode_i64_file, encode_ingest_i64_file, I64FileInput,
};
use aura_codec::schema::generic_i64_parent_schema;
use aura_codec::Profile;
use serde_json::Value;

const DEFAULT_TIMESTAMP_MULTIPLIER: i64 = 1_000_000;

#[derive(Debug, Clone)]
struct Args {
    input: PathBuf,
    output: PathBuf,
    schema_header: Vec<u8>,
    decimal_scale: Option<i64>,
    timestamp_multiplier: i64,
    stream_id: u16,
    dictionary_id: u16,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let input_bytes = fs::metadata(&args.input)
        .with_context(|| format!("stat {}", args.input.display()))?
        .len();
    let schema = generic_i64_parent_schema("json_i64_rows_v1", &args.schema_header)?;
    let (rows, decimal_scales) = read_positional_rows(
        &args.input,
        &args.schema_header,
        args.decimal_scale,
        args.timestamp_multiplier,
    )?;

    let aura = encode_ingest_i64_file(I64FileInput {
        schema,
        rows: rows.clone(),
        stream_id: args.stream_id,
        dictionary_id: args.dictionary_id,
        header_comment: None,
    })?;
    let aura0 = compile_i64_file(&aura, Profile::Aura0)?;
    let aura1 = compile_i64_file(&aura, Profile::Aura1)?;

    verify_round_trip(&aura, &rows, &args.schema_header, Profile::Ingest)?;
    verify_round_trip(&aura0, &rows, &args.schema_header, Profile::Aura0)?;
    verify_round_trip(&aura1, &rows, &args.schema_header, Profile::Aura1)?;

    fs::write(&args.output, &aura).with_context(|| format!("write {}", args.output.display()))?;
    let aura0_path = args.output.with_extension("aura0");
    let aura1_path = args.output.with_extension("aura1");
    fs::write(&aura0_path, &aura0).with_context(|| format!("write {}", aura0_path.display()))?;
    fs::write(&aura1_path, &aura1).with_context(|| format!("write {}", aura1_path.display()))?;

    println!("input={}", args.input.display());
    println!("rows={}", rows.len());
    println!("slots={}", args.schema_header.len());
    println!("schema_header={:?}", args.schema_header);
    println!(
        "decimal_scale={}",
        args.decimal_scale
            .map(|scale| scale.to_string())
            .unwrap_or_else(|| "auto".to_string())
    );
    println!("decimal_scales={decimal_scales:?}");
    println!("timestamp_multiplier={}", args.timestamp_multiplier);
    println!("input_bytes={input_bytes}");
    println!("aura_bytes={}", aura.len());
    println!("aura0_bytes={}", aura0.len());
    println!("aura1_bytes={}", aura1.len());
    println!("decoded_ingest_match=true");
    println!("decoded_aura0_match=true");
    println!("decoded_aura1_match=true");
    println!("out={}", args.output.display());
    println!("out_aura0={}", aura0_path.display());
    println!("out_aura1={}", aura1_path.display());
    Ok(())
}

fn parse_args() -> Result<Args> {
    let mut input = None;
    let mut output = None;
    let mut schema_header = None;
    let mut decimal_scale = None;
    let mut timestamp_multiplier = DEFAULT_TIMESTAMP_MULTIPLIER;
    let mut stream_id = 0;
    let mut dictionary_id = 0;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => output = Some(next_path(&mut args, "--out")?),
            "--schema" => {
                schema_header = Some(parse_schema_header(&next_string(&mut args, "--schema")?)?)
            }
            "--decimal-scale" => decimal_scale = Some(next_parse(&mut args, "--decimal-scale")?),
            "--timestamp-multiplier" => {
                timestamp_multiplier = next_parse(&mut args, "--timestamp-multiplier")?
            }
            "--stream-id" => stream_id = next_parse(&mut args, "--stream-id")?,
            "--dictionary-id" => dictionary_id = next_parse(&mut args, "--dictionary-id")?,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            value if value.starts_with('-') => bail!("unknown argument {value}"),
            value => {
                if input.replace(PathBuf::from(value)).is_some() {
                    bail!("multiple input paths");
                }
            }
        }
    }

    if let Some(decimal_scale) = decimal_scale {
        if decimal_scale <= 0 || !is_power_of_ten(decimal_scale) {
            bail!("--decimal-scale must be a positive power of ten");
        }
    }
    if timestamp_multiplier <= 0 {
        bail!("--timestamp-multiplier must be positive");
    }

    Ok(Args {
        input: input.context("missing input path")?,
        output: output.context("missing --out")?,
        schema_header: schema_header.context("missing --schema")?,
        decimal_scale,
        timestamp_multiplier,
        stream_id,
        dictionary_id,
    })
}

fn read_positional_rows(
    path: &PathBuf,
    schema_header: &[u8],
    decimal_scale: Option<i64>,
    timestamp_multiplier: i64,
) -> Result<(Vec<Vec<i64>>, Vec<i64>)> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let value: Value =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    let source_rows = value.as_array().context("top-level JSON array")?;
    let field_count = schema_header.len();
    let decimal_scales = infer_decimal_scales(source_rows, field_count, decimal_scale)?;
    let timestamp_slots = timestamp_slots(schema_header)?;
    let mut rows = Vec::with_capacity(source_rows.len());
    for (row_index, source_row) in source_rows.iter().enumerate() {
        let values = source_row
            .as_array()
            .with_context(|| format!("row {row_index} is not an array"))?;
        if values.len() < field_count {
            bail!(
                "row {row_index} has {} slots, schema header needs {field_count}",
                values.len()
            );
        }
        let mut row = Vec::with_capacity(field_count);
        for (slot_index, value) in values.iter().take(field_count).enumerate() {
            let multiplier = if timestamp_slots[slot_index] {
                timestamp_multiplier
            } else {
                1
            };
            row.push(
                value_to_i64(value, decimal_scales[slot_index], multiplier)
                    .with_context(|| format!("row {row_index} slot {slot_index}"))?,
            );
        }
        rows.push(row);
    }
    Ok((rows, decimal_scales))
}

fn value_to_i64(value: &Value, decimal_scale: i64, multiplier: i64) -> Result<i64> {
    let parsed = match value {
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                Ok(value)
            } else if let Some(value) = number.as_u64() {
                i64::try_from(value).context("integer value exceeds i64")
            } else {
                parse_scaled_decimal(&number.to_string(), decimal_scale)
            }
        }
        Value::String(value) => parse_scaled_decimal(value, decimal_scale),
        _ => bail!("expected integer number or decimal string"),
    }?;
    parsed
        .checked_mul(multiplier)
        .context("scaled value exceeds i64")
}

fn parse_scaled_decimal(raw: &str, scale: i64) -> Result<i64> {
    let value = raw.trim();
    if value.is_empty() {
        bail!("empty decimal");
    }

    let (negative, value) = match value.as_bytes()[0] {
        b'-' => (true, &value[1..]),
        b'+' => (false, &value[1..]),
        _ => (false, value),
    };
    if value.is_empty() {
        bail!("empty decimal");
    }

    let mut parts = value.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    if parts.next().is_some() {
        bail!("invalid decimal");
    }
    if whole.is_empty() && fraction.unwrap_or_default().is_empty() {
        bail!("invalid decimal");
    }

    let mut scaled = if whole.is_empty() {
        0i128
    } else {
        parse_digits(whole)?
            .checked_mul(i128::from(scale))
            .context("scaled decimal overflow")?
    };

    if let Some(fraction) = fraction {
        let mut place = scale / 10;
        for digit in fraction.bytes() {
            if !digit.is_ascii_digit() {
                bail!("invalid decimal digit");
            }
            let digit_value = i128::from(digit - b'0');
            if place > 0 {
                scaled = scaled
                    .checked_add(digit_value * i128::from(place))
                    .context("scaled decimal overflow")?;
                place /= 10;
            } else if digit_value != 0 {
                bail!("decimal exceeds scale precision");
            }
        }
    }

    if negative {
        scaled = -scaled;
    }
    i64::try_from(scaled).context("scaled decimal exceeds i64")
}

fn parse_digits(value: &str) -> Result<i128> {
    let mut parsed = 0i128;
    for digit in value.bytes() {
        if !digit.is_ascii_digit() {
            bail!("invalid decimal digit");
        }
        parsed = parsed
            .checked_mul(10)
            .and_then(|current| current.checked_add(i128::from(digit - b'0')))
            .context("decimal overflow")?;
    }
    Ok(parsed)
}

fn infer_decimal_scales(
    rows: &[Value],
    field_count: usize,
    decimal_scale: Option<i64>,
) -> Result<Vec<i64>> {
    let mut max_fraction_digits = vec![0usize; field_count];
    let mut has_decimal = vec![false; field_count];
    for (row_index, source_row) in rows.iter().enumerate() {
        let values = source_row
            .as_array()
            .with_context(|| format!("row {row_index} is not an array"))?;
        if values.len() < field_count {
            bail!(
                "row {row_index} has {} slots, schema header needs {field_count}",
                values.len()
            );
        }
        for (slot_index, value) in values.iter().take(field_count).enumerate() {
            if let Some(fraction_digits) = decimal_fraction_digits(value)
                .with_context(|| format!("row {row_index} slot {slot_index}"))?
            {
                has_decimal[slot_index] = true;
                max_fraction_digits[slot_index] =
                    max_fraction_digits[slot_index].max(fraction_digits);
            }
        }
    }

    max_fraction_digits
        .into_iter()
        .enumerate()
        .map(|(slot_index, fraction_digits)| {
            if let Some(decimal_scale) = decimal_scale {
                return Ok(if has_decimal[slot_index] {
                    decimal_scale
                } else {
                    1
                });
            }
            power_of_ten(fraction_digits)
        })
        .collect()
}

fn decimal_fraction_digits(value: &Value) -> Result<Option<usize>> {
    match value {
        Value::String(value) => decimal_fraction_digits_str(value).map(Some),
        Value::Number(number) => {
            if number.is_i64() || number.is_u64() {
                Ok(None)
            } else {
                decimal_fraction_digits_str(&number.to_string()).map(Some)
            }
        }
        _ => bail!("expected integer number or decimal string"),
    }
}

fn decimal_fraction_digits_str(raw: &str) -> Result<usize> {
    let value = raw.trim();
    if value.is_empty() {
        bail!("empty decimal");
    }
    let value = value
        .strip_prefix('-')
        .or_else(|| value.strip_prefix('+'))
        .unwrap_or(value);
    let mut parts = value.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next().unwrap_or_default();
    if parts.next().is_some() || (whole.is_empty() && fraction.is_empty()) {
        bail!("invalid decimal");
    }
    if !whole.bytes().all(|digit| digit.is_ascii_digit())
        || !fraction.bytes().all(|digit| digit.is_ascii_digit())
    {
        bail!("invalid decimal digit");
    }
    Ok(fraction.trim_end_matches('0').len())
}

fn power_of_ten(exponent: usize) -> Result<i64> {
    let mut out = 1i64;
    for _ in 0..exponent {
        out = out.checked_mul(10).context("decimal scale exceeds i64")?;
    }
    Ok(out)
}

fn timestamp_slots(schema_header: &[u8]) -> Result<Vec<bool>> {
    let mut slots = vec![false; schema_header.len()];
    for (slot, is_timestamp) in slots.iter_mut().enumerate() {
        *is_timestamp = timestamp_slot(schema_header, slot)?;
    }
    Ok(slots)
}

fn timestamp_slot(schema_header: &[u8], slot: usize) -> Result<bool> {
    let value = schema_header[slot];
    if value == 255 {
        return Ok(true);
    }
    let Some(parent) = parent_slot(value) else {
        return Ok(false);
    };
    if parent >= slot {
        bail!("schema parent must refer to an earlier slot");
    }
    timestamp_slot(schema_header, parent)
}

fn parent_slot(value: u8) -> Option<usize> {
    match value {
        1..=127 => Some(usize::from(value - 1)),
        129..=254 => Some(usize::from(value - 129)),
        _ => None,
    }
}

fn parse_schema_header(raw: &str) -> Result<Vec<u8>> {
    let trimmed = raw.trim();
    let trimmed = trimmed
        .strip_prefix('[')
        .unwrap_or(trimmed)
        .strip_suffix(']')
        .unwrap_or(trimmed)
        .trim();
    if trimmed.is_empty() {
        bail!("empty schema header");
    }
    trimmed
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<u8>()
                .with_context(|| format!("invalid schema byte {}", part.trim()))
        })
        .collect()
}

fn verify_round_trip(
    bytes: &[u8],
    rows: &[Vec<i64>],
    schema_header: &[u8],
    profile: Profile,
) -> Result<()> {
    let decoded = decode_i64_file(bytes)?;
    if decoded.header.profile != profile {
        bail!("decoded profile mismatch");
    }
    if decoded.header.schema_mapping != schema_header {
        bail!("decoded schema header mismatch");
    }
    if decoded.rows != rows {
        bail!("decoded rows mismatch");
    }
    Ok(())
}

fn is_power_of_ten(value: i64) -> bool {
    let mut value = value;
    while value > 1 && value % 10 == 0 {
        value /= 10;
    }
    value == 1
}

fn next_string(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String> {
    args.next()
        .with_context(|| format!("missing value for {name}"))
}

fn next_path(args: &mut impl Iterator<Item = String>, name: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(next_string(args, name)?))
}

fn next_parse<T>(args: &mut impl Iterator<Item = String>, name: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    next_string(args, name)?
        .parse()
        .with_context(|| format!("invalid value for {name}"))
}

fn print_usage() {
    println!(
        "usage: aura-json-i64 --schema <bytes> --out <file.aura> [--decimal-scale N] [--timestamp-multiplier N] [--stream-id N] [--dictionary-id N] <rows.json>"
    );
}
