#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/iris-samba-diagnostics.XXXXXX")"
trap 'rm -rf "$test_root"' EXIT
mkdir -p "$test_root/bin" "$test_root/logs"

cat >"$test_root/bin/docker" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-} ${2:-}" in
  'container inspect') [[ "$3" == *samba* ]] ;;
  'exec '* )
    if [[ "$3 $4" == 'sh -c' ]]; then
      # Execute the actual reporter command against the fixture directory.
      command="${5//\/var\/log\/samba/$IRIS_PERF_TEST_LOG_DIR}"
      sh -c "$command"
    fi
    ;;
esac
MOCK
printf '#!/usr/bin/env bash\nexit 0\n' >"$test_root/bin/nc"
printf '#!/usr/bin/env bash\nexit 1\n' >"$test_root/bin/cargo"
chmod +x "$test_root/bin"/*

report_logs() {
  local status=0
  PATH="$test_root/bin:$PATH" \
    IRIS_PERF_TEST_LOG_DIR="$test_root/logs" \
    IRIS_PERF_WORK_ROOT="$test_root/work" \
    IRIS_PERF_KEEP_CONTAINERS=0 \
    IRIS_PERF_SKIP_SFTP=1 \
    IRIS_PERF_SKIP_SMB=0 \
    IRIS_PERF_SMB_LOG_LEVEL=2 \
    IRIS_PERF_BYTES=1 \
    IRIS_PERF_SMALL_FILES=1 \
    IRIS_PERF_SMALL_BYTES=1 \
    bash "$script_dir/perf-transfer-containers.sh" >"$test_root/output" 2>&1 || status=$?
  [[ "$status" == 1 ]]
  grep -Fq 'Container diagnostics after transfer smoke failure:' "$test_root/output"
  sed '1,/^Samba log files /d' "$test_root/output" >"$test_root/report"
}

assert_absent() {
  if grep -Fq "$1" "$test_root/report"; then
    echo "unexpected diagnostic output: $1" >&2
    exit 1
  fi
}

printf 'peer diagnostic\n' >"$test_root/logs/log.172.17.0.1"
printf 'nmbd diagnostic\n' >"$test_root/logs/log.nmbd"
printf 'smbd iris diagnostic\n' >"$test_root/logs/log.smbd"
report_logs
grep -Fq 'smbd [redacted] diagnostic' "$test_root/report"
grep -Fq 'peer diagnostic' "$test_root/report"
assert_absent 'nmbd diagnostic'
assert_absent 'iris'
[[ "$(grep -c '^--- ' "$test_root/report")" == 2 ]]
[[ "$(sed -n '2p' "$test_root/report")" == *'/log.smbd ---' ]]

# Oversized logs prove both the per-file tails and the final combined cap.
for name in log.smbd log.172.17.0.1 log.nmbd; do
  printf 'discarded prefix\n' >"$test_root/logs/$name"
  printf '%6000s' '' | tr ' ' x >>"$test_root/logs/$name"
  printf '\n%s iris diagnostic\n' "$name" >>"$test_root/logs/$name"
done
report_logs
grep -Fq 'log.smbd [redacted] diagnostic' "$test_root/report"
grep -Fq 'log.172.17.0.1 [redacted] diagnostic' "$test_root/report"
assert_absent 'log.nmbd'
assert_absent 'discarded prefix'
[[ "$(wc -c <"$test_root/report")" -eq 8192 ]]
# Redaction adds six bytes per selected file; headers are outside each file tail.
awk '/^--- / || /^$/ {next} {size += length($0) + 1} /diagnostic$/ {
  if (size > 4102) exit 1
  size = 0
}' "$test_root/report"

# Without the primary log, retain the original two-file fallback.
rm "$test_root/logs/log.smbd"
report_logs
grep -Fq 'log.172.17.0.1 [redacted] diagnostic' "$test_root/report"
grep -Fq 'log.nmbd [redacted] diagnostic' "$test_root/report"
[[ "$(wc -c <"$test_root/report")" -le 8192 ]]

# A primary-only directory must not report the explicit path twice.
rm "$test_root/logs/log.172.17.0.1" "$test_root/logs/log.nmbd"
printf 'smbd iris diagnostic\n' >"$test_root/logs/log.smbd"
report_logs
[[ "$(grep -c '^--- ' "$test_root/report")" == 1 ]]
grep -Fq 'smbd [redacted] diagnostic' "$test_root/report"
printf 'Samba diagnostics prioritize smbd, redact credentials, and retain 4096/8192-byte bounds\n'
