#!/usr/bin/env python3
"""Prepare bounded real GLBX candles for the Aura i64 row encoder."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from datetime import datetime, timezone
from decimal import Decimal
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

COLUMNS = ["ts_event", "rtype", "publisher_id", "instrument_id", "open", "high", "low", "close", "volume", "symbol"]
SCHEMA_HEADER = [100, 0, 0, 0, 0, 5, 5, 5, 0, 0]
DECIMAL_SCALE = 1_000_000_000
DEFAULT_ES = "/home/anton/Downloads/glbx_ohlcv_1m_yearly_parquet/es/year=2010/es_ohlcv_1m_2010.parquet"
DEFAULT_NQ = "/home/anton/Downloads/glbx_ohlcv_1m_yearly_parquet/nq/year=2010/nq_ohlcv_1m_2010.parquet"
DEFAULT_OUT = "/home/anton/Downloads/lean-node-20260907/corpus-w03/candles"
EXPECTED_TYPES = [
    pa.timestamp("ns", tz="UTC"), pa.uint8(), pa.uint16(), pa.uint32(), pa.decimal128(20, 9),
    pa.decimal128(20, 9), pa.decimal128(20, 9), pa.decimal128(20, 9), pa.uint64(), pa.string(),
]
def canonical(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
def digest(value: object) -> str:
    return hashlib.sha256(canonical(value)).hexdigest()
def file_digest(path: Path) -> tuple[str, int]:
    h = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            h.update(block)
            size += len(block)
    return h.hexdigest(), size
def write_json(path: Path, value: object) -> None:
    temporary = path.with_name(f".{path.name}.tmp")
    with temporary.open("w", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    os.replace(temporary, path)
def checked_int(value: object, upper: int, label: str, row: int) -> int:
    if value is None or isinstance(value, bool):
        raise ValueError(f"{label} row {row}: null/non-integer")
    value = int(value)
    if not 0 <= value <= upper:
        raise ValueError(f"{label} row {row}: {value} exceeds unsigned range")
    if value > 2**63 - 1:
        raise ValueError(f"{label} row {row}: value does not fit Aura i64")
    return value
def decimal_value(value: Decimal, row: int, name: str) -> tuple[int, str]:
    if value.as_tuple().exponent != -9:
        raise ValueError(f"{name} row {row}: decimal scale changed")
    scaled = value * DECIMAL_SCALE
    if scaled != scaled.to_integral_value():
        raise ValueError(f"{name} row {row}: non-exact decimal")
    integer = int(scaled)
    if not -(2**63) <= integer <= 2**63 - 1:
        raise ValueError(f"{name} row {row}: scaled decimal does not fit Aura i64")
    text = format(value, "f")
    if Decimal(text) != value:
        raise ValueError(f"{name} row {row}: decimal text mismatch")
    return integer, text
def iso_ns(value: int) -> str:
    seconds, nanos = divmod(value, 1_000_000_000)
    stamp = datetime.fromtimestamp(seconds, timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")
    return f"{stamp}.{nanos:09d}Z"
def prepare(path: Path, dataset: str, limit: int, out_dir: Path) -> dict:
    source_sha256, source_bytes = file_digest(path)
    parquet = pq.ParquetFile(path)
    if parquet.metadata.num_rows < limit or limit <= 0:
        raise ValueError(f"{path}: needs at least {limit} source rows")
    schema = parquet.schema_arrow
    if schema.names != COLUMNS:
        raise ValueError(f"{path}: column order/names changed: {schema.names}")
    for name, actual, expected in zip(COLUMNS, schema.types, EXPECTED_TYPES):
        if actual != expected:
            raise ValueError(f"{path}: {name} has {actual}, expected {expected}")
    batch = next(parquet.iter_batches(batch_size=limit, columns=COLUMNS, use_threads=False), None)
    if batch is None or batch.num_rows != limit:
        raise ValueError(f"{path}: bounded read returned {0 if batch is None else batch.num_rows} rows")
    table = pa.Table.from_batches([batch])
    null_counts = {name: table.column(name).null_count for name in COLUMNS}
    if any(null_counts.values()):
        raise ValueError(f"{path}: selected rows contain nulls: {null_counts}")
    ts = table.column("ts_event").cast(pa.int64()).to_pylist()
    values = {name: table.column(name).to_pylist() for name in COLUMNS[1:]}
    raw_rows, encoded_rows = [], []
    for row in range(limit):
        decimals = [decimal_value(values[name][row], row, name) for name in ("open", "high", "low", "close")]
        raw_rows.append([
            int(ts[row]), int(values["rtype"][row]), int(values["publisher_id"][row]),
            int(values["instrument_id"][row]), *(text for _, text in decimals),
            int(values["volume"][row]), values["symbol"][row],
        ])
        encoded_rows.append([
            int(ts[row]), checked_int(values["rtype"][row], 2**8 - 1, "rtype", row),
            checked_int(values["publisher_id"][row], 2**16 - 1, "publisher_id", row),
            checked_int(values["instrument_id"][row], 2**32 - 1, "instrument_id", row),
            *(integer for integer, _ in decimals),
            checked_int(values["volume"][row], 2**64 - 1, "volume", row),
            None,
        ])
    symbols = sorted({row[-1] for row in raw_rows})
    if any(not isinstance(symbol, str) for symbol in symbols):
        raise ValueError(f"{path}: symbol is not UTF-8 text")
    code_by_symbol = {symbol: code for code, symbol in enumerate(symbols)}
    for raw, encoded in zip(raw_rows, encoded_rows):
        encoded[-1] = code_by_symbol[raw[-1]]
    source_schema = [{"name": name, "type": str(actual), "nullable": field.nullable} for name, actual, field in zip(COLUMNS, schema.types, schema)]
    facts = {"source_sha256": source_sha256, "source_schema": source_schema, "source_row_count": parquet.metadata.num_rows,
             "selected_start_row": 0, "selected_row_count": limit, "schema_header": SCHEMA_HEADER,
             "symbol_dictionary": symbols, "source_rows": raw_rows, "encoded_rows": encoded_rows}
    facts_digests = {"source_rows_sha256": digest(raw_rows), "encoded_rows_sha256": digest(encoded_rows), "all_facts_sha256": digest(facts)}
    name = dataset
    input_path = out_dir / f"{name}.input.json"
    sourcefacts_path = out_dir / f"{name}.sourcefacts.json"
    metadata_path = out_dir / f"{name}.metadata.json"
    write_json(input_path, encoded_rows)
    write_json(sourcefacts_path, {"format": "aura-real-candles-sourcefacts-v1", **facts, "digests": facts_digests})
    timestamps = [row[0] for row in encoded_rows]
    instruments = sorted({row[3] for row in encoded_rows})
    aura_columns = []
    for slot, (name_, source_type) in enumerate(zip(COLUMNS, schema.types)):
        if slot == 0:
            encoding, aura_type, source_scale, aura_scale = "timestamp_ns", "timestamp_ns", 0, 1
        elif slot in (4, 5, 6, 7):
            encoding, aura_type, source_scale, aura_scale = "exact_unscaled_decimal", "i64", 9, 1
        elif slot == 9:
            encoding, aura_type, source_scale, aura_scale = "dictionary_code", "i64", None, 1
        else:
            encoding, aura_type, source_scale, aura_scale = "integer", "i64", 0, 1
        aura_columns.append({"slot": slot, "name": name_, "source_type": str(source_type), "aura_type": aura_type,
                             "encoding": encoding, "source_scale": source_scale, "aura_scale": aura_scale})
    metadata = {
        "format": "aura-real-candles-metadata-v1", "dataset": name,
        "source": {"path": str(path.resolve()), "sha256": source_sha256, "bytes": source_bytes,
                    "row_count": parquet.metadata.num_rows, "schema": source_schema},
        "selection": {"start_row": 0, "row_count": limit, "order": "original parquet row order",
                       "time_range_ns": [min(timestamps), max(timestamps)],
                       "time_range_utc": [iso_ns(min(timestamps)), iso_ns(max(timestamps))],
                       "instrument_ids": instruments},
        "schema": {"header": SCHEMA_HEADER, "header_text": ",".join(map(str, SCHEMA_HEADER)),
                   "field_count": 10, "parent_slots": {"high": 4, "low": 4, "close": 4}},
        "columns": aura_columns,
        "symbol_dictionary": {"slot": 9, "code_type": "i64", "order": "lexicographic_utf8", "codes": symbols},
        "checks": {"all_columns_preserved": True, "row_order_preserved": True, "null_counts": null_counts,
                    "empty_source_rejected": True, "volume_i64_checked": True, "decimal_exact": True},
        "digests": facts_digests,
        "artifacts": {"input_json": {"path": input_path.name, "archive_member": False},
                      "sourcefacts_json": {"path": sourcefacts_path.name, "archive_member": False, "purpose": "independent retained validation reference"},
                      "metadata_json": {"path": metadata_path.name, "archive_member": True}},
    }
    write_json(metadata_path, metadata)
    return {"dataset": name, "metadata": metadata_path.name, "sourcefacts": sourcefacts_path.name,
            "input": input_path.name, "row_count": limit, "source_sha256": source_sha256, "all_facts_sha256": facts_digests["all_facts_sha256"]}
def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, default=Path(DEFAULT_OUT))
    parser.add_argument("--es", type=Path, default=Path(DEFAULT_ES))
    parser.add_argument("--nq", type=Path, default=Path(DEFAULT_NQ))
    parser.add_argument("--rows", type=int, default=16_384)
    parser.add_argument("--tiny-rows", type=int, default=16)
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    es = prepare(args.es, "real-candles-es-16384", args.rows, args.output_dir)
    nq = prepare(args.nq, "real-candles-nq-16384", args.rows, args.output_dir)
    if set(json.loads((args.output_dir / es["metadata"]).read_text())["selection"]["instrument_ids"]) & set(
        json.loads((args.output_dir / nq["metadata"]).read_text())["selection"]["instrument_ids"]
    ):
        raise ValueError("ES and NQ selected instrument ids overlap")
    tiny = prepare(args.es, "real-candles-es-tiny16", args.tiny_rows, args.output_dir)
    manifest = {"format": "aura-real-candles-manifest-v1", "schema_header": SCHEMA_HEADER,
                "datasets": [es, nq, tiny], "temporary_inputs_excluded_from_archive": True}
    write_json(args.output_dir / "manifest.json", manifest)
    for item in manifest["datasets"]:
        print(f"{item['dataset']}: rows={item['row_count']} facts={item['all_facts_sha256']}")
if __name__ == "__main__": main()
