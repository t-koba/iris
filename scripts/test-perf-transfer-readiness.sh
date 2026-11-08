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
  IRIS_PERF_READY_TIMEOUT_SECONDS=2 \
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
if (( elapsed > 4 )); then
  echo "two-second readiness deadline took ${elapsed}s" >&2
  cat "$test_root/output" >&2
  exit 1
fi
grep -Fq 'SFTP server did not provide an SSH host key within 2s' "$test_root/output"
printf 'stalled key scan respected readiness budget (elapsed=%ss)\n' "$elapsed"

# Exercise the actual helper with a deterministic monotonic clock: no real
# sleeping or scheduler timing is involved in the fractional-budget assertions.
python3 - "$repo_root/scripts/perf-transfer-containers.sh" "$test_root" <<'PY'
import pathlib
import subprocess
import sys
from unittest.mock import patch

source = pathlib.Path(sys.argv[1]).read_text().split("<<'PY'\n", 1)[1].split("\nPY\n", 1)[0]
root = pathlib.Path(sys.argv[2])


def check_case(name, duration, budget, ceiling, success=False, stalled=False, succeed_first=False):
    now = 0.0
    scans = []
    delays = []
    output = root / (name + ".out")
    error = root / (name + ".err")

    def scan(args, *, stdout, stderr, timeout, check):
        nonlocal now
        requested = int(args[args.index("-T") + 1])
        remaining = budget - now
        assert remaining > 0, (name, remaining, now)
        assert 1 <= requested <= ceiling, (name, requested, now)
        assert requested == min(ceiling, max(1, int(remaining))), (name, requested, remaining)
        assert timeout == remaining, (name, timeout, remaining)
        scans.append(requested)
        if stalled:
            now += timeout
            raise subprocess.TimeoutExpired(args, timeout)
        now += duration
        if succeed_first and len(scans) == 1:
            stdout.write(b"synthetic-host-key\n")
            return subprocess.CompletedProcess(args, 0)
        if success and len(scans) == 2:
            stdout.write(b"synthetic-host-key\n")
            return subprocess.CompletedProcess(args, 0)
        stderr.write(b"synthetic scan failure\n")
        return subprocess.CompletedProcess(args, 1)

    def sleep(delay):
        nonlocal now
        assert 0 < delay <= min(1, budget - now), (name, delay, now)
        delays.append(delay)
        now += delay

    with patch.object(sys, "argv", ["-", str(budget), str(ceiling), "2222", str(output), str(error)]), \
            patch("time.monotonic", side_effect=lambda: now), \
            patch("time.sleep", side_effect=sleep), \
            patch("subprocess.run", side_effect=scan):
        try:
            exec(compile(source, "readiness-helper", "exec"), {})
        except SystemExit as result:
            assert result.code == (0 if (success or succeed_first) else 1), (name, result.code)
        else:
            raise AssertionError("helper must return a status")
    assert now <= budget, (name, now)
    return scans, delays


# A quick failure late in the budget leaves only a fractional retry delay.
assert check_case("short-delay", 0.25, 1, 3) == ([1], [0.75])
# The final fractional second still gets a scan: the integer -T stays at
# least 1 while the fractional deadline is enforced via the subprocess timeout.
assert check_case("fractional-retry", 0.25, 2, 3, success=True) == ([2, 1], [1])
# A one-second budget still allows an immediate success.
assert check_case("immediate", 0.1, 1, 3, succeed_first=True) == ([1], [])
# Retain transient success, the per-scan ceiling, and an outer scan deadline.
assert check_case("transient", 0.25, 4, 1, success=True) == ([1, 1], [1])
assert check_case("stalled", 0, 2, 3, stalled=True) == ([2], [])
print("deterministic retry, integer scan budget, transient success and scan deadline passed")
PY
