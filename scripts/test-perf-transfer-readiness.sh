#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/iris-sftp-readiness.XXXXXX")"
trap 'rm -rf "$test_root"' EXIT
mkdir -p "$test_root/bin" "$test_root/work"

cat >"$test_root/bin/docker" <<'MOCK'
#!/usr/bin/env bash
case "${1:-} ${2:-}" in
  'run -d') echo mock-container-id ;;
  'container inspect') exit 1 ;;
  'rm -f') exit 0 ;;
esac
exit 0
MOCK
cat >"$test_root/bin/nc" <<'MOCK'
#!/usr/bin/env bash
exit 0
MOCK
cat >"$test_root/bin/cargo" <<'MOCK'
#!/usr/bin/env bash
exit 0
MOCK
cat >"$test_root/bin/ssh-keyscan" <<'MOCK'
#!/usr/bin/env bash
timeout=1
while (($#)); do
  if [[ "$1" == "-T" ]]; then
    timeout="$2"
    shift 2
  else
    shift
  fi
done
printf '%s\n' "$timeout" >"$IRIS_PERF_TEST_SCAN_TIMEOUT_FILE"
sleep "$timeout"
echo 'mock key scan timed out' >&2
exit 1
MOCK
chmod +x "$test_root/bin"/*

started=$SECONDS
set +e
PATH="$test_root/bin:$PATH" \
  IRIS_PERF_WORK_ROOT="$test_root/work" \
  IRIS_PERF_KEEP_CONTAINERS=1 \
  IRIS_PERF_SKIP_SMB=1 \
  IRIS_PERF_BYTES=1 \
  IRIS_PERF_SMALL_FILES=1 \
  IRIS_PERF_SMALL_BYTES=1 \
  IRIS_PERF_READY_TIMEOUT_SECONDS=1 \
  IRIS_PERF_SSH_KEYSCAN_TIMEOUT_SECONDS=3 \
  IRIS_PERF_TEST_SCAN_TIMEOUT_FILE="$test_root/scan-timeout" \
  bash "$repo_root/scripts/perf-transfer-containers.sh" >"$test_root/output" 2>&1
status=$?
set -e
elapsed=$((SECONDS - started))

if (( status == 0 )); then
  echo "expected the stalled SFTP key scan to fail" >&2
  exit 1
fi
if [[ "$(cat "$test_root/scan-timeout")" != "1" ]]; then
  echo "key scan timeout was not capped to the remaining readiness budget" >&2
  cat "$test_root/output" >&2
  exit 1
fi
if (( elapsed > 3 )); then
  echo "one-second readiness deadline took ${elapsed}s" >&2
  cat "$test_root/output" >&2
  exit 1
fi
grep -Fq 'SFTP server did not provide an SSH host key within 1s' "$test_root/output"
printf 'stalled key scan respected readiness budget (elapsed=%ss)\n' "$elapsed"
