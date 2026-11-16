#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"
cat >"$tmp/bin/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" >"$FUZZ_TEST_ARGS"
MOCK
chmod +x "$tmp/bin/cargo"

assert_args() {
  local path="$1" expected="$2"
  python3 - "$path" "$expected" <<'PY'
from pathlib import Path
import sys
actual = Path(sys.argv[1]).read_text().splitlines()
expected = sys.argv[2].split("|")
if actual != expected:
    raise SystemExit(f"cargo-fuzz args mismatch: {actual!r} != {expected!r}")
PY
}

args="$tmp/args"
PATH="$tmp/bin:$PATH" \
  FUZZ_TEST_ARGS="$args" \
  FUZZ_TOOLCHAIN=nightly-test \
  FUZZ_BUILD_ONLY=1 \
  IRIS_FUZZ_TARGET=x86_64-unknown-linux-gnu \
  bash "$repo_root/scripts/fuzz-target.sh" raw_tiff_parse >/dev/null
assert_args "$args" '+nightly-test|fuzz|build|--target|x86_64-unknown-linux-gnu|raw_tiff_parse'

PATH="$tmp/bin:$PATH" \
  FUZZ_TEST_ARGS="$args" \
  FUZZ_TOOLCHAIN=nightly-test \
  FUZZ_RUNS=1000 \
  FUZZ_OUTER_TIMEOUT=10 \
  IRIS_FUZZ_TARGET=x86_64-unknown-linux-gnu \
  bash "$repo_root/scripts/fuzz-target.sh" raw_tiff_parse >/dev/null
assert_args "$args" '+nightly-test|fuzz|run|--target|x86_64-unknown-linux-gnu|raw_tiff_parse|--|-runs=1000'

printf '%s\n' 'fuzz target selection passed'
