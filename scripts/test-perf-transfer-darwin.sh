#!/usr/bin/env bash
# Bounded mocked Darwin startup: no server installation, sockets or containers.
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
test_root=$(mktemp -d "${TMPDIR:-/tmp}/iris-samba-darwin.XXXXXX")
trap 'rm -rf "$test_root"' EXIT
mkdir -p "$test_root"/{bin,brew/sbin,brew/bin,capture,repo/scripts}
cp "$script_dir/perf-transfer-containers.sh" "$test_root/repo/scripts/"
cat >"$test_root/bin/mock" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "${0##*/}" in
  uname) echo Darwin ;;
  id) case "$1" in -u) echo 501 ;; -un) echo benchuser ;; *) exit 99 ;; esac ;;
  brew) [[ "$*" == '--prefix samba' ]]; echo "$TEST_ROOT/brew" ;;
  docker|sudo|debootstrap|build-test-samba.sh|truncate)
    echo "${0##*/} $*" >>"$CAPTURE/forbidden"; exit 99 ;;
  testparm)
    [[ "$1" == -s ]]
    config=$2
    native=${config%/*}
    [[ "$(ls -ld "$native" | cut -c1-10)" == drwx------ ]]
    [[ "$(ls -l "$native/private/smbpasswd" | cut -c1-10)" == -rw------- ]]
    for dir in private state cache lock pid logs ncalrpc winbind; do
      [[ "$(ls -ld "$native/$dir" | cut -c1-10)" == drwx------ ]]
    done
    cp "$config" "$CAPTURE/config"
    cp "$native/private/smbpasswd" "$CAPTURE/passdb"
    echo "$native" >"$CAPTURE/native-dir"
    ;;
  samba-dot-org-smbd)
    if [[ "$*" == --version ]]; then echo "Version ${TEST_VERSION:-4.25.1}"; exit; fi
    [[ "$1 $2" == '--foreground --no-process-group' && "$3" == --configfile=* ]]
    echo "synthetic iris credential diagnostic"
    echo $$ >"$CAPTURE/pid"
    echo started >>"$CAPTURE/lifecycle"
    trap 'echo stopped >>"$CAPTURE/lifecycle"; exit 0' TERM
    while :; do sleep 0.1; done
    ;;
  nc) [[ -s "$CAPTURE/pid" ]] ;;
  cargo)
    if [[ "$*" == *smb_native_smoke* ]]; then
      [[ "$IRIS_SMB_USER:$IRIS_SMB_PASSWORD:$IRIS_SMB_PORT:$IRIS_SMB_SHARE" == benchuser:iris:1445:bench ]]
      touch "$CAPTURE/smoke"
      exit "${TEST_SMOKE_STATUS:-0}"
    fi
    [[ "$*" == *backend_copy_perf* ]]
    echo benchmark >>"$CAPTURE/benchmarks"
    ;;
  mount_smbfs)
    [[ "$1" == '//benchuser:iris@127.0.0.1:1445/bench' && "$2" == "$IRIS_PERF_WORK_ROOT/smb-mount" ]]
    echo mounted >>"$CAPTURE/lifecycle"
    ;;
  umount)
    [[ "$1" == "$IRIS_PERF_WORK_ROOT/smb-mount" ]]
    echo unmounted >>"$CAPTURE/lifecycle"
    ;;
esac
MOCK
chmod +x "$test_root/bin/mock"
for cmd in uname id brew docker sudo debootstrap truncate nc cargo mount_smbfs umount; do
  ln -s mock "$test_root/bin/$cmd"
done
ln -s "$test_root/bin/mock" "$test_root/brew/sbin/samba-dot-org-smbd"
ln -s "$test_root/bin/mock" "$test_root/brew/bin/testparm"
ln -s "$test_root/bin/mock" "$test_root/repo/scripts/build-test-samba.sh"
export TEST_ROOT="$test_root" CAPTURE="$test_root/capture" PATH="$test_root/bin:$PATH"
export IRIS_PERF_WORK_ROOT="$test_root/work with spaces"
export IRIS_PERF_SKIP_SFTP=1 IRIS_PERF_SKIP_SMB=0 IRIS_PERF_SAMBA_IMAGE=''
export IRIS_PERF_BYTES=1 IRIS_PERF_SMALL_FILES=1 IRIS_PERF_SMALL_BYTES=1
export IRIS_PERF_SMB_PORT=1445 IRIS_PERF_READY_TIMEOUT_SECONDS=2
export IRIS_PERF_SMB_PAGING_FILES=0 IRIS_PERF_SSH_KEYSCAN_TIMEOUT_SECONDS=1
export TEST_VERSION=4.25.1 TEST_SMOKE_STATUS=0
export IRIS_PERF_KEEP_CONTAINERS=0 IRIS_PERF_SMB_MOUNT=''
export IRIS_PERF_SMB_MIN_PROTOCOL='' IRIS_PERF_SMB_MAX_PROTOCOL=''
export IRIS_PERF_SMB_SERVER_SIGNING='' IRIS_PERF_SMB_ENCRYPTION=''
export IRIS_PERF_SMB_LOG_LEVEL=2 IRIS_PERF_SMB_AUTH_AUDIT_LEVEL=''
report_harness_failure() {
  local stream file
  for stream in stdout stderr; do
    file="$test_root/out"
    [[ "$stream" == stdout ]] || file="$test_root/err"
    [[ -s "$file" ]] || continue
    echo "--- Darwin harness $stream (last at most 4096 bytes) ---" >&2
    tail -c 4096 "$file" | sed \
      -e 's/benchuser:iris/[redacted]/g' \
      -e 's/benchuser/[redacted]/g' \
      -e 's/iris/[redacted]/g' \
      -e 's/A224A29E508493ECD70572422203EC18/[redacted]/g' >&2
  done
}

# Exercise the diagnostic reader itself with oversized byte streams.
python3 - "$test_root/out" "$test_root/err" <<'PY'
from pathlib import Path
import sys
Path(sys.argv[1]).write_bytes(b"x" * 6000 + b" benchuser:iris\n")
Path(sys.argv[2]).write_bytes(b"y" * 7000 + b" A224A29E508493ECD70572422203EC18\n")
PY
report_harness_failure >"$test_root/bounded-report" 2>&1
[[ "$(wc -c <"$test_root/bounded-report")" -le 9000 ]]
grep -Fq 'stdout (last at most 4096 bytes)' "$test_root/bounded-report"
grep -Fq 'stderr (last at most 4096 bytes)' "$test_root/bounded-report"
! grep -Fq 'benchuser:iris' "$test_root/bounded-report"
! grep -Fq 'benchuser' "$test_root/bounded-report"
! grep -Fq 'iris' "$test_root/bounded-report"
! grep -Fq 'A224A29E508493ECD70572422203EC18' "$test_root/bounded-report"

run_harness() {
  # A process-group deadline also bounds regressions in daemon cleanup.
  local status=0
  python3 - "$test_root/repo/scripts/perf-transfer-containers.sh" "$test_root" <<'PY' || status=$?
import os, signal, subprocess, sys
with open(sys.argv[2] + '/out', 'w') as out, open(sys.argv[2] + '/err', 'w') as err:
    p = subprocess.Popen(['bash', sys.argv[1]], stdout=out, stderr=err, start_new_session=True)
    try:
        sys.exit(p.wait(timeout=10))
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGKILL)
        p.wait()
        sys.exit('Darwin harness exceeded 10s test deadline')
PY
  cat "$test_root/out" "$test_root/err" >>"$test_root/all-output"
  if (( status != 0 )); then
    echo "Darwin harness failed (exit=$status); bounded sanitized output follows:" >&2
    report_harness_failure
  fi
  return "$status"
}
run_harness
[[ ! -e "$CAPTURE/forbidden" && -e "$CAPTURE/smoke" ]]
[[ "$(wc -l <"$CAPTURE/benchmarks" | tr -d ' ')" == 4 ]]
[[ "$(cat "$CAPTURE/lifecycle")" == $'started\nmounted\nunmounted\nstopped' ]]
! kill -0 "$(cat "$CAPTURE/pid")" 2>/dev/null
[[ ! -e "$(cat "$CAPTURE/native-dir")" ]]
config="$CAPTURE/config"
for setting in 'security = user' 'server role = standalone server' 'map to guest = Never' \
    'guest ok = no' 'valid users = benchuser' 'read only = no' 'interfaces = 127.0.0.1' \
    'bind interfaces only = yes' 'disable netbios = yes' 'smb ports = 1445' \
    'server min protocol = SMB2_10' 'server max protocol = SMB3_11' \
    'server signing = mandatory' 'smb encrypt = required' "path = $IRIS_PERF_WORK_ROOT/samba-share"; do
  grep -Fxq "$setting" "$config"
done
native=$(cat "$CAPTURE/native-dir")
for setting in "private dir = $native/private" "state directory = $native/state" \
    "cache directory = $native/cache" "lock directory = $native/lock" \
    "pid directory = $native/pid" "ncalrpc dir = $native/ncalrpc" \
    "winbindd socket directory = $native/winbind" "log file = $native/logs/log.%m" \
    "passdb backend = smbpasswd:$native/private/smbpasswd"; do
  grep -Fxq "$setting" "$config"
done
grep -Eq '^benchuser:501:XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX:A224A29E508493ECD70572422203EC18:\[U          \]:LCT-[0-9A-F]{8}:$' "$CAPTURE/passdb"
# Failures still stop the daemon; retention keeps only the private on-disk state.
rm -f "$CAPTURE"/{pid,smoke,lifecycle,benchmarks}
export TEST_SMOKE_STATUS=42 IRIS_PERF_KEEP_CONTAINERS=1
status=0
run_harness >"$test_root/failure-report" 2>&1 || status=$?
[[ "$status" == 42 && ! -e "$CAPTURE/forbidden" ]]
grep -Fq 'Darwin harness failed (exit=42)' "$test_root/failure-report"
grep -Fq 'synthetic [redacted] credential diagnostic' "$test_root/failure-report"
! grep -Fq 'synthetic iris credential diagnostic' "$test_root/failure-report"
! grep -Fq 'A224A29E508493ECD70572422203EC18' "$test_root/failure-report"
[[ "$(wc -c <"$test_root/failure-report")" -le 9000 ]]
[[ "$(cat "$CAPTURE/lifecycle")" == $'started\nstopped' ]]
! kill -0 "$(cat "$CAPTURE/pid")" 2>/dev/null
[[ -d "$(cat "$CAPTURE/native-dir")" ]]
grep -Fq 'synthetic [redacted] credential diagnostic' "$test_root/err"
! grep -Fq 'synthetic iris credential diagnostic' "$test_root/err"
# Security downgrades, old/unrecognised binaries, config injection and privileged
# ports must fail before a daemon, mount, builder or container is started.
rm -f "$CAPTURE"/{pid,smoke,lifecycle}
unset TEST_SMOKE_STATUS
export IRIS_PERF_KEEP_CONTAINERS=0
for setting in 'TEST_VERSION=4.25.0' 'TEST_VERSION=unknown' 'IRIS_PERF_SMB_PORT=445' \
    'IRIS_PERF_SMB_SERVER_SIGNING=auto' 'IRIS_PERF_SMB_ENCRYPTION=off' \
    'IRIS_PERF_SMB_MIN_PROTOCOL=NT1' 'IRIS_PERF_SMB_LOG_LEVEL=2;guest ok = yes' \
    $'IRIS_PERF_SMB_LOG_LEVEL=2\nguest ok = yes'; do
  if (export "$setting"; run_harness >"$test_root/expected-failure" 2>&1); then
    echo "accepted invalid native setting" >&2; exit 1
  fi
  [[ ! -e "$CAPTURE/pid" && ! -e "$CAPTURE/smoke" && ! -e "$CAPTURE/forbidden" ]]
done
if grep -E 'benchuser:iris|A224A29E508493ECD70572422203EC18' "$test_root/all-output"; then
  echo 'credential leaked in harness output' >&2; exit 1
fi
# Skipping SMB entirely must not select the native server or require Docker.
rm -rf "$test_root/skip-work"
export IRIS_PERF_WORK_ROOT="$test_root/skip-work"
export IRIS_PERF_SKIP_SFTP=1 IRIS_PERF_SKIP_SMB=1
rm -f "$CAPTURE"/{pid,smoke,lifecycle,benchmarks,forbidden}
run_harness
[[ ! -e "$CAPTURE/forbidden" && ! -e "$CAPTURE/pid" && ! -e "$CAPTURE/lifecycle" ]]
printf 'Darwin Samba startup: PASS (private authenticated server, strict security, no builder/container, cleanup)\n'
