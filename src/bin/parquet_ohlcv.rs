use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use arrow::array::{Array, Float64Array, Int64Array, StringArray};
use arrow::datatypes::SchemaRef;
use aura_codec::ohlcv::{ohlcv_i64_row, DecimalScales, OhlcvF64, DEFAULT_DECIMAL_SCALE};
use aura_codec::records::{
    compile_i64_file, decode_i64_file, encode_ingest_i64_file, I64FileInput,
};
use aura_codec::schema::{generic_i64_schema, RelatedFieldMapping};
use aura_codec::Profile;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

#[derive(Debug, Clone)]
struct Args {
    output: PathBuf,
    stream_id: u32,
    dictionary_id: u32,
    price_scale: i64,
    volume_scale: i64,
    inputs: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
struct ColumnIndices {
    ts: usize,
    symbol: Option<usize>,
    interval: Option<usize>,
    open: usize,
    high: usize,
    low: usize,
    close: usize,
    volume: usize,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let input_paths = collect_parquet_paths(&args.inputs)?;
    if input_paths.is_empty() {
        bail!("no parquet inputs found");
    }

    let mut rows = Vec::new();
    let mut input_bytes = 0u64;
    let mut first_symbol = None;
    let mut first_interval = None;
    for path in &input_paths {
        input_bytes += fs::metadata(path)
            .with_context(|| format!("stat {}", path.display()))?
            .len();
        read_ohlcv_parquet(
            path,
            args.price_scale,
            args.volume_scale,
            &mut rows,
            &mut first_symbol,
            &mut first_interval,
        )?;
    }
    rows.sort_by_key(|row| row[0]);

    let schema = generic_i64_schema(
        "generic_ohlcv_i64_v1",
        5,
        &[
            RelatedFieldMapping::new(2, 1),
            RelatedFieldMapping::new(3, 1),
            RelatedFieldMapping::new(4, 1),
        ],
    )?;
    let aura = encode_ingest_i64_file(I64FileInput {
        schema,
        rows: rows.clone(),
        stream_id: args.stream_id,
        dictionary_id: args.dictionary_id,
    })?;
    let aura0 = compile_i64_file(&aura, Profile::Aura0)?;
    let aura1 = compile_i64_file(&aura, Profile::Aura1)?;

    let decoded_aura0 = decode_i64_file(&aura0)?;
    let decoded_aura1 = decode_i64_file(&aura1)?;
    if decoded_aura0.rows != rows || decoded_aura1.rows != rows {
        bail!("compiled profile roundtrip mismatch");
    }

    fs::write(&args.output, &aura).with_context(|| format!("write {}", args.output.display()))?;
    let aura0_path = args.output.with_extension("aura0");
    let aura1_path = args.output.with_extension("aura1");
    fs::write(&aura0_path, &aura0).with_context(|| format!("write {}", aura0_path.display()))?;
    fs::write(&aura1_path, &aura1).with_context(|| format!("write {}", aura1_path.display()))?;

    println!("inputs={}", input_paths.len());
    println!("rows={}", rows.len());
    println!(
        "symbol={}",
        first_symbol.unwrap_or_else(|| "unknown".to_string())
    );
    println!(
        "interval={}",
        first_interval.unwrap_or_else(|| "unknown".to_string())
    );
    println!("input_bytes={input_bytes}");
    println!("aura_bytes={}", aura.len());
    println!("aura0_bytes={}", aura0.len());
    println!("aura1_bytes={}", aura1.len());
    println!("out={}", args.output.display());
    println!("out_aura0={}", aura0_path.display());
    println!("out_aura1={}", aura1_path.display());
    Ok(())
}

fn parse_args() -> Result<Args> {
    let mut output = None;
    let mut stream_id = 0;
    let mut dictionary_id = 0;
    let mut price_scale = DEFAULT_DECIMAL_SCALE;
    let mut volume_scale = DEFAULT_DECIMAL_SCALE;
    let mut inputs = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => output = Some(next_path(&mut args, "--out")?),
            "--stream-id" => stream_id = next_parse(&mut args, "--stream-id")?,
            "--dictionary-id" => dictionary_id = next_parse(&mut args, "--dictionary-id")?,
            "--price-scale" => price_scale = next_parse(&mut args, "--price-scale")?,
            "--volume-scale" => volume_scale = next_parse(&mut args, "--volume-scale")?,
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            value if value.starts_with('-') => bail!("unknown argument {value}"),
            value => inputs.push(PathBuf::from(value)),
        }
    }

    let output = output.context("missing --out")?;
    if inputs.is_empty() {
        bail!("missing parquet input path");
    }
    Ok(Args {
        output,
        stream_id,
        dictionary_id,
        price_scale,
        volume_scale,
        inputs,
    })
}

fn print_usage() {
    eprintln!(
        "usage: aura-parquet-ohlcv --out <file.aura> [--stream-id N] [--dictionary-id N] [--price-scale N] [--volume-scale N] <file-or-dir>..."
    );
}

fn next_path(args: &mut impl Iterator<Item = String>, name: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(
        args.next()
            .with_context(|| format!("missing value for {name}"))?,
    ))
}

fn next_parse<T>(args: &mut impl Iterator<Item = String>, name: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let value = args
        .next()
        .with_context(|| format!("missing value for {name}"))?;
    value
        .parse::<T>()
        .map_err(|err| anyhow::anyhow!("invalid {name}: {err}"))
}

fn collect_parquet_paths(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for input in inputs {
        collect_one(input, &mut out)?;
    }
    out.sort();
    Ok(out)
}

fn collect_one(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if metadata.is_file() {
        if path.extension().is_some_and(|ext| ext == "parquet") {
            out.push(path.to_path_buf());
        }
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).with_context(|| format!("read_dir {}", path.display()))? {
            collect_one(&entry?.path(), out)?;
        }
    }
    Ok(())
}

fn read_ohlcv_parquet(
    path: &Path,
    price_scale: i64,
    volume_scale: i64,
    rows: &mut Vec<Vec<i64>>,
    first_symbol: &mut Option<String>,
    first_interval: &mut Option<String>,
) -> Result<()> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(|| format!("parquet reader {}", path.display()))?;
    let columns = ColumnIndices::from_schema(builder.schema())?;
    let reader = builder.with_batch_size(8192).build()?;

    for batch in reader {
        let batch = batch?;
        let ts = int64_column(&batch, columns.ts, "ts")?;
        let open = float64_column(&batch, columns.open, "open")?;
        let high = float64_column(&batch, columns.high, "high")?;
        let low = float64_column(&batch, columns.low, "low")?;
        let close = float64_column(&batch, columns.close, "close")?;
        let volume = float64_column(&batch, columns.volume, "volume")?;
        let symbol = optional_string_column(&batch, columns.symbol, "symbol")?;
        let interval = optional_string_column(&batch, columns.interval, "interval")?;

        for row in 0..batch.num_rows() {
            ensure_not_null(ts, row, "ts")?;
            ensure_not_null(open, row, "open")?;
            ensure_not_null(high, row, "high")?;
            ensure_not_null(low, row, "low")?;
            ensure_not_null(close, row, "close")?;
            ensure_not_null(volume, row, "volume")?;
            if first_symbol.is_none() {
                if let Some(symbol) = symbol.filter(|array| !array.is_null(row)) {
                    *first_symbol = Some(symbol.value(row).to_string());
                }
            }
            if first_interval.is_none() {
                if let Some(interval) = interval.filter(|array| !array.is_null(row)) {
                    *first_interval = Some(interval.value(row).to_string());
                }
            }
            rows.push(ohlcv_i64_row(
                OhlcvF64 {
                    ts_seconds: ts.value(row),
                    open: open.value(row),
                    high: high.value(row),
                    low: low.value(row),
                    close: close.value(row),
                    volume: volume.value(row),
                },
                DecimalScales {
                    price: price_scale,
                    volume: volume_scale,
                },
            )?);
        }
    }
    Ok(())
}

impl ColumnIndices {
    fn from_schema(schema: &SchemaRef) -> Result<Self> {
        Ok(Self {
            ts: column_index(schema, "ts")?,
            symbol: maybe_column_index(schema, "symbol"),
            interval: maybe_column_index(schema, "interval"),
            open: column_index(schema, "open")?,
            high: column_index(schema, "high")?,
            low: column_index(schema, "low")?,
            close: column_index(schema, "close")?,
            volume: column_index(schema, "volume")?,
        })
    }
}

fn column_index(schema: &SchemaRef, name: &str) -> Result<usize> {
    maybe_column_index(schema, name).with_context(|| format!("missing column {name}"))
}

fn maybe_column_index(schema: &SchemaRef, name: &str) -> Option<usize> {
    schema
        .fields()
        .iter()
        .position(|field| field.name() == name)
}

fn int64_column<'a>(
    batch: &'a arrow::record_batch::RecordBatch,
    index: usize,
    name: &str,
) -> Result<&'a Int64Array> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<Int64Array>()
        .with_context(|| format!("column {name} is not Int64"))
}

fn float64_column<'a>(
    batch: &'a arrow::record_batch::RecordBatch,
    index: usize,
    name: &str,
) -> Result<&'a Float64Array> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<Float64Array>()
        .with_context(|| format!("column {name} is not Float64"))
}

fn optional_string_column<'a>(
    batch: &'a arrow::record_batch::RecordBatch,
    index: Option<usize>,
    name: &str,
) -> Result<Option<&'a StringArray>> {
    index
        .map(|index| {
            batch
                .column(index)
                .as_any()
                .downcast_ref::<StringArray>()
                .with_context(|| format!("column {name} is not Utf8"))
        })
        .transpose()
}

fn ensure_not_null(array: &dyn Array, row: usize, name: &str) -> Result<()> {
    if array.is_null(row) {
        bail!("column {name} has null at row {row}");
    }
    Ok(())
}
