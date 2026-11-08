#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"

bytes="${IRIS_PERF_BYTES:-67108864}"
small_files="${IRIS_PERF_SMALL_FILES:-1000}"
small_bytes="${IRIS_PERF_SMALL_BYTES:-16384}"
work_root="${IRIS_PERF_WORK_ROOT:-}"
keep="${IRIS_PERF_KEEP_CONTAINERS:-0}"
sftp_image="${IRIS_PERF_SFTP_IMAGE:-atmoz/sftp:alpine}"
sftp_port="${IRIS_PERF_SFTP_PORT:-2222}"
ready_timeout="${IRIS_PERF_READY_TIMEOUT_SECONDS:-60}"
ssh_keyscan_timeout="${IRIS_PERF_SSH_KEYSCAN_TIMEOUT_SECONDS:-5}"
smb_image="${IRIS_PERF_SAMBA_IMAGE:-}"
owned_smb_image=""
smb_port="${IRIS_PERF_SMB_PORT:-1445}"
smb_mount="${IRIS_PERF_SMB_MOUNT:-}"
smb_paging_files="${IRIS_PERF_SMB_PAGING_FILES:-0}"
skip_smb="${IRIS_PERF_SKIP_SMB:-0}"
skip_sftp="${IRIS_PERF_SKIP_SFTP:-0}"
smb_min_protocol="${IRIS_PERF_SMB_MIN_PROTOCOL:-}"
smb_max_protocol="${IRIS_PERF_SMB_MAX_PROTOCOL:-}"
smb_server_signing="${IRIS_PERF_SMB_SERVER_SIGNING:-}"
smb_encryption="${IRIS_PERF_SMB_ENCRYPTION:-}"
smb_log_level="${IRIS_PERF_SMB_LOG_LEVEL:-2}"
smb_auth_audit_level="${IRIS_PERF_SMB_AUTH_AUDIT_LEVEL:-}"
smb_log_config="$smb_log_level"
if [[ -n "$smb_auth_audit_level" ]]; then
  smb_log_config+=" auth_audit:$smb_auth_audit_level"
fi
if ! read -r smb_debug_level _ <<<"$smb_log_level"; then
  smb_debug_level=""
fi

usage() {
  cat <<'USAGE'
Usage: scripts/perf-transfer-containers.sh

Runs the v0.5 P0 transfer benchmark harness against local containers.
The script prints measurements to stdout and keeps all generated payloads under
a temporary directory unless IRIS_PERF_WORK_ROOT is set.

Environment:
  IRIS_PERF_BYTES            Large-file size in bytes (default: 67108864).
  IRIS_PERF_SMALL_FILES      Number of small files (default: 1000).
  IRIS_PERF_SMALL_BYTES      Small-file size in bytes (default: 16384).
  IRIS_PERF_SFTP_IMAGE       SFTP container image (default: atmoz/sftp:alpine).
  IRIS_PERF_SFTP_PORT        Host SFTP port (default: 2222).
  IRIS_PERF_READY_TIMEOUT_SECONDS Readiness deadline (default: 60).
  IRIS_PERF_SSH_KEYSCAN_TIMEOUT_SECONDS Per-scan timeout (default: 5).
  IRIS_PERF_SAMBA_IMAGE      Local Samba sha256 image ID (never a registry tag).
                             If unset, build a signed Debian snapshot test rootfs
                             (requires Linux, sudo, debootstrap, archive keyring).
  IRIS_PERF_DEBIAN_KEYRING   Host archive keyring for that build (default:
                             /usr/share/keyrings/debian-archive-keyring.gpg).
  IRIS_PERF_SMB_PORT         Host SMB port (default: 1445).
  IRIS_PERF_SMB_MOUNT        Existing SMB mount path to benchmark. If unset,
                             macOS is auto-mounted; Linux prints mount guidance.
  IRIS_PERF_SKIP_SMB=1       Run only SFTP benchmarks.
  IRIS_PERF_SKIP_SFTP=1      Run only SMB integration/benchmarks.
  IRIS_PERF_SMB_MIN_PROTOCOL Samba server minimum protocol (for example SMB2_10).
  IRIS_PERF_SMB_MAX_PROTOCOL Samba server maximum protocol (for example SMB3_00).
  IRIS_PERF_SMB_SERVER_SIGNING Samba server signing policy (for example mandatory).
  IRIS_PERF_SMB_ENCRYPTION   Samba encryption policy (off or required).
  IRIS_PERF_SMB_LOG_LEVEL    Samba log level (default: 2);
                             numeric prefix must be 0-10 and sets smbd's base level.
  IRIS_PERF_SMB_AUTH_AUDIT_LEVEL
                             Optional auth_audit class level (0-10; unset by default).
  IRIS_PERF_KEEP_CONTAINERS=1 Keep containers and generated payloads.
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "required command is missing: $1" >&2
    exit 1
  }
}

validate_timeout() {
  local name="$1"
  local value="$2"
  if [[ ! "$value" =~ ^[1-9][0-9]{0,17}$ ]]; then
    echo "$name must be a positive integer" >&2
    exit 2
  fi
}

wait_for_port() {
  local host="$1"
  local port="$2"
  local label="$3"
  local deadline=$((SECONDS + ready_timeout))
  while (( SECONDS < deadline )); do
    if nc -z "$host" "$port" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  echo "$label did not become reachable at $host:$port" >&2
  return 1
}

make_payloads() {
  mkdir -p "$payload_root/small-files"
  truncate -s "$bytes" "$payload_root/large.bin"
  local index
  for index in $(seq 1 "$small_files"); do
    truncate -s "$small_bytes" "$payload_root/small-files/file-$(printf '%04d' "$index").bin"
  done
}

start_sftp() {
  docker rm -f "$sftp_container" >/dev/null 2>&1 || true
  docker run -d \
    --name "$sftp_container" \
    -p "$sftp_port:22" \
    "$sftp_image" \
    "iris:iris:1000:1000:upload" >/dev/null
  wait_for_port 127.0.0.1 "$sftp_port" "SFTP container"
}

prepare_known_hosts() {
  local home_dir="$work_root/home"
  local known_hosts="$home_dir/.ssh/known_hosts"
  local scan_output="$work_root/ssh-keyscan.out"
  local scan_error="$work_root/ssh-keyscan.err"
  local deadline=$((SECONDS + ready_timeout))
  local remaining
  local scan_timeout
  mkdir -p "$home_dir/.ssh"
  chmod 700 "$home_dir/.ssh"
  while (( SECONDS < deadline )); do
    remaining=$((deadline - SECONDS))
    scan_timeout="$ssh_keyscan_timeout"
    if (( scan_timeout > remaining )); then
      scan_timeout="$remaining"
    fi
    if ssh-keyscan -T "$scan_timeout" -p "$sftp_port" 127.0.0.1 >"$scan_output" 2>"$scan_error" && [[ -s "$scan_output" ]]; then
      mv "$scan_output" "$known_hosts"
      chmod 600 "$known_hosts"
      export HOME="$home_dir"
      return 0
    fi
    if (( SECONDS < deadline )); then
      sleep 1
    fi
  done
  echo "SFTP server did not provide an SSH host key within ${ready_timeout}s" >&2
  if [[ -s "$scan_error" ]]; then
    tail -c 2048 "$scan_error" >&2
  fi
  return 1
}

run_sftp_benchmarks() {
  prepare_known_hosts
  (
    cd "$repo_root"
    cargo run -p iris-remote-transport --release --example sftp_perf_transfer -- \
      --host 127.0.0.1 --port "$sftp_port" --user iris --password iris \
      --direction upload \
      --local-path "$payload_root/large.bin" \
      --remote-path /upload/large.bin
    cargo run -p iris-remote-transport --release --example sftp_perf_transfer -- \
      --host 127.0.0.1 --port "$sftp_port" --user iris --password iris \
      --direction download \
      --local-path "$work_root/sftp-download/large.bin" \
      --remote-path /upload/large.bin
    cargo run -p iris-remote-transport --release --example sftp_perf_transfer -- \
      --host 127.0.0.1 --port "$sftp_port" --user iris --password iris \
      --direction upload --dir \
      --local-path "$payload_root/small-files" \
      --remote-path /upload/small-files
    cargo run -p iris-remote-transport --release --example sftp_perf_transfer -- \
      --host 127.0.0.1 --port "$sftp_port" --user iris --password iris \
      --direction download --dir \
      --local-path "$work_root/sftp-download/small-files" \
      --remote-path /upload/small-files
    cmp "$payload_root/large.bin" "$work_root/sftp-download/large.bin"
    diff -r "$payload_root/small-files" "$work_root/sftp-download/small-files"
  )
}

start_samba() {
  if [[ -z "$smb_image" ]]; then
    smb_image="$(bash "$script_dir/build-test-samba.sh")"
    owned_smb_image="$smb_image"
  fi
  if [[ ! "$smb_image" =~ ^sha256:[0-9a-f]{64}$ ]]; then
    echo "IRIS_PERF_SAMBA_IMAGE must be a local sha256 image ID" >&2
    return 2
  fi
  mkdir -p "$work_root/samba-share"
  if (( smb_paging_files > 0 )); then
    local suffix
    suffix="$(printf 'x%.0s' {1..180})"
    for ((index = 0; index < smb_paging_files; index++)); do
      : >"$work_root/samba-share/paging-$(printf '%06d' "$index")-$suffix"
    done
  fi
  docker rm -f "$smb_container" >/dev/null 2>&1 || true
  local samba_args=(
    run -d --pull=never
    --name "$smb_container"
    -p "$smb_port:445"
    -v "$work_root/samba-share:/mount"
    "$smb_image"
  )
  if [[ -n "$smb_min_protocol" ]]; then
    samba_args+=(-g "server min protocol = $smb_min_protocol")
  fi
  if [[ -n "$smb_max_protocol" ]]; then
    samba_args+=(-g "server max protocol = $smb_max_protocol")
  fi
  if [[ -n "$smb_server_signing" ]]; then
    samba_args+=(-g "server signing = $smb_server_signing")
  fi
  if [[ -n "$smb_encryption" ]]; then
    samba_args+=(-g "smb encrypt = $smb_encryption")
  fi
  samba_args+=(-g "workgroup = WORKGROUP")
  samba_args+=(-g "log level = $smb_log_config")
  docker "${samba_args[@]}" >/dev/null
  wait_for_port 127.0.0.1 "$smb_port" "Samba container"
}

mount_smb_if_possible() {
  if [[ -n "$smb_mount" ]]; then
    echo "$smb_mount"
    return 0
  fi

  local mount_dir="$work_root/smb-mount"
  mkdir -p "$mount_dir"
  case "$(uname -s)" in
    Darwin)
      mount_smbfs "//iris:iris@127.0.0.1:$smb_port/bench" "$mount_dir"
      mounted_by_script="$mount_dir"
      echo "$mount_dir"
      ;;
    Linux)
      cat >&2 <<EOF
SMB container is running, but Linux CIFS mounting usually requires elevated
privileges. Mount it separately, then rerun with:

  sudo mount -t cifs //127.0.0.1/bench "$mount_dir" -o port=$smb_port,username=iris,password=iris,vers=3.0,noserverino
  IRIS_PERF_SMB_MOUNT="$mount_dir" scripts/perf-transfer-containers.sh

Skipping SMB measurements in this run.
EOF
      return 1
      ;;
    *)
      echo "automatic SMB mount is unsupported on this platform; set IRIS_PERF_SMB_MOUNT" >&2
      return 1
      ;;
  esac
}

run_smb_benchmarks() {
  start_samba
  (
    cd "$repo_root"
    IRIS_SMB_HOST=127.0.0.1 \
      IRIS_SMB_PORT="$smb_port" \
      IRIS_SMB_SHARE=bench \
      IRIS_SMB_USER=iris \
      IRIS_SMB_PASSWORD=iris \
      IRIS_SMB_SMOKE_PAYLOAD="$payload_root/smb-smoke.bin" \
      IRIS_SMB_SMOKE_BYTES="$bytes" \
      IRIS_SMB_PAGING_FILES="$smb_paging_files" \
      cargo run -p iris-backends --release --example smb_native_smoke
  )
  local mount_dir
  if ! mount_dir="$(mount_smb_if_possible)"; then
    return 0
  fi

  cp "$payload_root/large.bin" "$mount_dir/large.bin"
  rm -rf "$mount_dir/small-files"
  cp -R "$payload_root/small-files" "$mount_dir/small-files"

  (
    cd "$repo_root"
    cargo run -p iris-transfer --release --example backend_copy_perf -- \
      --src-kind smb --src-root "$mount_dir" --src-path large.bin \
      --dst-kind local --dst-root "$work_root/smb-download" --dst-path large.bin \
      --mode file
    cargo run -p iris-transfer --release --example backend_copy_perf -- \
      --src-kind local --src-root "$payload_root" --src-path large.bin \
      --dst-kind smb --dst-root "$mount_dir" --dst-path upload-large.bin \
      --mode file
    cargo run -p iris-transfer --release --example backend_copy_perf -- \
      --src-kind smb --src-root "$mount_dir" --src-path small-files \
      --dst-kind local --dst-root "$work_root/smb-download" --dst-path small-files \
      --mode directory
    cargo run -p iris-transfer --release --example backend_copy_perf -- \
      --src-kind local --src-root "$payload_root" --src-path small-files \
      --dst-kind smb --dst-root "$mount_dir" --dst-path upload-small-files \
      --mode directory
  )
}

report_samba_setting() {
  local section="$1"
  local parameter="$2"
  local value
  if value=$(docker exec "$smb_container" testparm -s \
      --section-name="$section" --parameter-name="$parameter" 2>/dev/null \
      | tail -c 512 | tail -n 1 | sed 's/iris/[redacted]/g' | tail -c 512); then
    [[ -n "$value" ]] || value="<empty>"
  else
    value="<unavailable>"
  fi
  printf '  [%s] %s=%s\n' "$section" "$parameter" "$value" >&2
}

report_container() {
  local name="$1"
  if ! docker container inspect "$name" >/dev/null 2>&1; then
    return 0
  fi
  docker inspect --format 'container={{.Name}} state={{.State.Status}} exit_code={{.State.ExitCode}} error={{.State.Error}}' "$name" 2>&1 \
    | tail -c 2048 | sed 's/iris/[redacted]/g' | tail -c 2048 >&2 || true
  docker logs --tail 100 "$name" 2>&1 \
    | tail -c 8192 | sed 's/iris/[redacted]/g' | tail -c 8192 >&2 || true
  if [[ "$name" == "$smb_container" ]]; then
    echo "Samba effective configuration (allowlisted settings):" >&2
    report_samba_setting global 'log level'
    report_samba_setting global 'log file'
    report_samba_setting global 'server min protocol'
    report_samba_setting global 'server max protocol'
    report_samba_setting global 'server signing'
    report_samba_setting global 'smb encrypt'
    report_samba_setting global 'map to guest'
    report_samba_setting bench 'guest ok'
    echo "Samba runtime debug levels:" >&2
    docker exec "$name" smbcontrol smbd debuglevel 2>&1 \
      | tail -c 2048 | sed 's/iris/[redacted]/g' | tail -c 2048 >&2 || true
    echo "Samba log files (last 4096 bytes each; combined output capped at 8192 bytes):" >&2
    docker exec "$name" sh -c 'found=0; count=0; for file in /var/log/samba/log.smbd /var/log/samba/log.*; do
      [ -f "$file" ] || continue
      # The explicit primary log may also appear in the glob; report it only once.
      if [ "$file" = /var/log/samba/log.smbd ] && [ "$found" -eq 1 ]; then continue; fi
      [ "$count" -lt 2 ] || break
      found=1
      count=$((count + 1))
      printf "\n--- %s ---\n" "$file"
      tail -c 4096 "$file"
    done; [ "$found" -eq 1 ] || echo "no matching /var/log/samba/log.* files found"' \
      2>&1 | tail -c 8192 | sed 's/iris/[redacted]/g' | tail -c 8192 >&2 || true
  fi
}

cleanup() {
  local status=$?
  if (( status != 0 )); then
    echo "Container diagnostics after transfer smoke failure:" >&2
    report_container "$sftp_container"
    report_container "$smb_container"
  fi
  if [[ "$keep" != "1" ]]; then
    if [[ -n "${mounted_by_script:-}" ]]; then
      umount "$mounted_by_script" >/dev/null 2>&1 || true
    fi
    docker rm -f "$sftp_container" "$smb_container" >/dev/null 2>&1 || true
    if [[ -n "$owned_smb_image" ]]; then
      docker image rm "$owned_smb_image" >/dev/null 2>&1 || true
    fi
    if [[ -n "${created_work_root:-}" ]]; then
      rm -rf "$created_work_root"
    fi
  else
    echo "kept perf work root: $work_root" >&2
  fi
  exit "$status"
}

validate_timeout IRIS_PERF_READY_TIMEOUT_SECONDS "$ready_timeout"
validate_timeout IRIS_PERF_SSH_KEYSCAN_TIMEOUT_SECONDS "$ssh_keyscan_timeout"
if [[ "$skip_smb" != "1" && ! "$smb_debug_level" =~ ^(10|[0-9])$ ]]; then
  echo "IRIS_PERF_SMB_LOG_LEVEL must start with an integer from 0 to 10" >&2
  exit 2
fi
if [[ "$skip_smb" != "1" && -n "$smb_auth_audit_level" && ! "$smb_auth_audit_level" =~ ^(10|[0-9])$ ]]; then
  echo "IRIS_PERF_SMB_AUTH_AUDIT_LEVEL must be an integer from 0 to 10" >&2
  exit 2
fi

require_cmd docker
require_cmd nc
require_cmd cargo
if [[ "$skip_sftp" != "1" ]]; then
  require_cmd ssh-keyscan
fi

if [[ -z "$work_root" ]]; then
  work_root="$(mktemp -d "${TMPDIR:-/tmp}/iris-perf-transfer.XXXXXX")"
  created_work_root="$work_root"
else
  mkdir -p "$work_root"
fi

payload_root="$work_root/payloads"
sftp_container="iris-perf-sftp-$$"
smb_container="iris-perf-samba-$$"
mounted_by_script=""

trap cleanup EXIT

make_payloads
if [[ "$skip_sftp" != "1" ]]; then
  start_sftp
  run_sftp_benchmarks
fi

if [[ "$skip_smb" != "1" ]]; then
  run_smb_benchmarks
fi
