#!/usr/bin/env bash
# Run from any directory; leave the small synthetic fixture/reports for inspection.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if (( $# != 0 )); then
  echo "usage: bash scripts/check-onboarding.sh" >&2
  exit 2
fi
command -v python3 >/dev/null || { echo "Python 3 is required to check JSON reports" >&2; exit 1; }
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
fixture_dir=$(mktemp -d "${TMPDIR:-/tmp}/aura-onboarding.XXXXXX")
printf 'fixture_dir=%s\n' "$fixture_dir"
cargo run --locked --release --example roundtrip -- "$fixture_dir"
cargo run --locked --release --example order_book_aura0
cargo run --locked --release --bin aura-bench -- \
  --operation transcode-aura0-to-aura1 --dataset sdk-roundtrip \
  --input "$fixture_dir/roundtrip.aura0" --iterations 3 --warmups 1 \
  --preserve-output "$fixture_dir/converted.aura1" --verify-output-decodes \
  --output "$fixture_dir/transcode.json" --format json
cargo run --locked --release --bin aura-bench -- \
  --operation parse-aura1 --dataset sdk-roundtrip \
  --input "$fixture_dir/roundtrip.aura1" --iterations 3 --warmups 1 \
  --output "$fixture_dir/parse.json" --format json
python3 - "$fixture_dir" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
transcode = json.loads((root / "transcode.json").read_text())
parse = json.loads((root / "parse.json").read_text())
for key in ("decoded_row_equality", "record_count_equality", "schema_footer_validation"):
    if transcode.get(key) is not True:
        raise SystemExit(f"transcode verification failed: {key}={transcode.get(key)!r}")
if not transcode.get("output_preserved") or not (root / "converted.aura1").is_file():
    raise SystemExit("transcode output was not preserved")
if transcode.get("record_count", 0) <= 0 or parse.get("record_count") != transcode["record_count"]:
    raise SystemExit("benchmark record counts do not match the nonempty fixture")
print(f"onboarding_ok=true rows={transcode['record_count']} reports={root}")
PY
