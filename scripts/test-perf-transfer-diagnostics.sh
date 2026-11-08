#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/iris-samba-diagnostics.XXXXXX")"
trap 'rm -rf "$test_root"' EXIT
mkdir -p "$test_root/bin" "$test_root/logs" "$test_root/filter-markers" "$test_root/filter-temp"
real_sed=$(command -v sed)

cat >"$test_root/bin/docker" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
mark_stream() {
  local name="$1"
  local bytes="$2"
  [[ "$bytes" =~ ^[0-9]+$ ]] && (( bytes > 0 )) || return 0
  : >"$IRIS_PERF_TEST_FILTER_MARKER_DIR/$name"
  head -c "$bytes" /dev/zero | tr '\0' x
  printf '\n'
}
case "${1:-} ${2:-}" in
  'container inspect') [[ "$3" == *samba* ]] ;;
  'inspect --format') mark_stream inspect "${IRIS_PERF_TEST_INSPECT_BYTES:-}" ;;
  'logs --tail') mark_stream logs "${IRIS_PERF_TEST_DOCKER_LOG_BYTES:-}" ;;
  'run '* )
    if [[ -n "${IRIS_PERF_TEST_DOCKER_ARGS:-}" ]]; then
      printf '<%s>\n' "$@" >>"$IRIS_PERF_TEST_DOCKER_ARGS"
    fi
    ;;
  'exec '* )
    if [[ "$3 $4" == 'sh -c' ]]; then
      # Execute the actual reporter command against the fixture directory.
      command="${5//\/var\/log\/samba/$IRIS_PERF_TEST_LOG_DIR}"
      sh -c "$command"
    elif [[ "${3:-}" == testparm ]]; then
      mark_stream "testparm-$$" "${IRIS_PERF_TEST_TESTPARM_BYTES:-}"
    elif [[ "${3:-}" == smbcontrol ]]; then
      mark_stream smbcontrol "${IRIS_PERF_TEST_SMBCONTROL_BYTES:-}"
    fi
    ;;
esac
MOCK
cat >"$test_root/bin/sed" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == 's/iris/[redacted]/g' && -n "${IRIS_PERF_TEST_FILTER_MARKER_DIR:-}" ]]; then
  input=$(mktemp "$IRIS_PERF_TEST_FILTER_TEMP_DIR/input.XXXXXX")
  cat >"$input"
  for marker in "$IRIS_PERF_TEST_FILTER_MARKER_DIR"/*; do
    [[ -e "$marker" ]] || continue
    printf '%s %s\n' "$(basename "$marker")" "$(wc -c <"$input")" \
      >>"$IRIS_PERF_TEST_FILTER_OBSERVATIONS"
    rm -f "$marker"
  done
  "$IRIS_PERF_TEST_REAL_SED" "$@" <"$input"
  rm -f "$input"
else
  exec "$IRIS_PERF_TEST_REAL_SED" "$@"
fi
MOCK
printf '#!/usr/bin/env bash\nexit 0\n' >"$test_root/bin/nc"
printf '#!/usr/bin/env bash\nexit 1\n' >"$test_root/bin/cargo"
chmod +x "$test_root/bin"/*

report_logs() {
  local auth_audit_level="${1:-}"
  local inspect_bytes="${2:-}"
  local docker_log_bytes="${3:-}"
  local testparm_bytes="${4:-}"
  local smbcontrol_bytes="${5:-}"
  local status=0
  : >"$test_root/docker-args"
  : >"$test_root/filter-observations"
  rm -f "$test_root/filter-markers"/*
  PATH="$test_root/bin:$PATH" \
    IRIS_PERF_TEST_DOCKER_ARGS="$test_root/docker-args" \
    IRIS_PERF_TEST_LOG_DIR="$test_root/logs" \
    IRIS_PERF_TEST_FILTER_MARKER_DIR="$test_root/filter-markers" \
    IRIS_PERF_TEST_FILTER_TEMP_DIR="$test_root/filter-temp" \
    IRIS_PERF_TEST_FILTER_OBSERVATIONS="$test_root/filter-observations" \
    IRIS_PERF_TEST_REAL_SED="$real_sed" \
    IRIS_PERF_TEST_INSPECT_BYTES="$inspect_bytes" \
    IRIS_PERF_TEST_DOCKER_LOG_BYTES="$docker_log_bytes" \
    IRIS_PERF_TEST_TESTPARM_BYTES="$testparm_bytes" \
    IRIS_PERF_TEST_SMBCONTROL_BYTES="$smbcontrol_bytes" \
    IRIS_PERF_WORK_ROOT="$test_root/work" \
    IRIS_PERF_KEEP_CONTAINERS=0 \
    IRIS_PERF_SAMBA_IMAGE="sha256:$(printf '%064d' 0)" \
    IRIS_PERF_SKIP_SFTP=1 \
    IRIS_PERF_SKIP_SMB=0 \
    IRIS_PERF_SMB_LOG_LEVEL=2 \
    IRIS_PERF_SMB_AUTH_AUDIT_LEVEL="$auth_audit_level" \
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
printf 'smbd iris diagnostic\nAuth: user [iris] with [NTLMv2] status [NT_STATUS_LOGON_FAILURE]\n' >"$test_root/logs/log.smbd"
report_logs
grep -Fxq '<--pull=never>' "$test_root/docker-args"
grep -Fxq "<sha256:$(printf '%064d' 0)>" "$test_root/docker-args"
grep -Fxq '<log level = 2>' "$test_root/docker-args"
! grep -Fq 'auth_audit' "$test_root/docker-args"
grep -Fq 'smbd [redacted] diagnostic' "$test_root/report"
grep -Fq 'NTLMv2] status [NT_STATUS_LOGON_FAILURE]' "$test_root/report"
grep -Fq 'peer diagnostic' "$test_root/report"
assert_absent 'nmbd diagnostic'
assert_absent 'iris'

report_logs 3
grep -Fxq '<log level = 2 auth_audit:3>' "$test_root/docker-args"
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

# Oversized single-line container output must be capped before sed sees it.
report_logs "" 8192 16384 4096 16384
awk '
  $1 == "inspect" { inspect = 1; if ($2 > 2048) bad = 1 }
  $1 == "logs" { logs = 1; if ($2 > 8192) bad = 1 }
  $1 ~ /^testparm-/ { testparm++; if ($2 > 512) bad = 1 }
  $1 == "smbcontrol" { smbcontrol = 1; if ($2 > 2048) bad = 1 }
  END { if (!inspect || !logs || !testparm || !smbcontrol || bad) exit 1 }
' "$test_root/filter-observations"
printf 'Samba diagnostics bound inspect/log/testparm/smbcontrol streams before redaction filters\n'
printf 'Samba diagnostics configure auth_audit selectively, prioritize smbd, redact credentials, and retain 4096/8192-byte bounds\n'
