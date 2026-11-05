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
smb_image="${IRIS_PERF_SAMBA_IMAGE:-dperson/samba}"
smb_port="${IRIS_PERF_SMB_PORT:-1445}"
smb_mount="${IRIS_PERF_SMB_MOUNT:-}"
smb_paging_files="${IRIS_PERF_SMB_PAGING_FILES:-0}"
skip_smb="${IRIS_PERF_SKIP_SMB:-0}"
skip_sftp="${IRIS_PERF_SKIP_SFTP:-0}"
smb_min_protocol="${IRIS_PERF_SMB_MIN_PROTOCOL:-}"
smb_max_protocol="${IRIS_PERF_SMB_MAX_PROTOCOL:-}"
smb_server_signing="${IRIS_PERF_SMB_SERVER_SIGNING:-}"
smb_encryption="${IRIS_PERF_SMB_ENCRYPTION:-}"

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
  IRIS_PERF_SAMBA_IMAGE      Samba container image (default: dperson/samba).
  IRIS_PERF_SMB_PORT         Host SMB port (default: 1445).
  IRIS_PERF_SMB_MOUNT        Existing SMB mount path to benchmark. If unset,
                             macOS is auto-mounted; Linux prints mount guidance.
  IRIS_PERF_SKIP_SMB=1       Run only SFTP benchmarks.
  IRIS_PERF_SKIP_SFTP=1      Run only SMB integration/benchmarks.
  IRIS_PERF_SMB_MIN_PROTOCOL Samba server minimum protocol (for example SMB2_10).
  IRIS_PERF_SMB_MAX_PROTOCOL Samba server maximum protocol (for example SMB3_00).
  IRIS_PERF_SMB_SERVER_SIGNING Samba server signing policy (for example mandatory).
  IRIS_PERF_SMB_ENCRYPTION   Samba encryption policy (off or required).
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

wait_for_port() {
  local host="$1"
  local port="$2"
  local label="$3"
  for _ in $(seq 1 60); do
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
  mkdir -p "$home_dir/.ssh"
  chmod 700 "$home_dir/.ssh"
  ssh-keyscan -p "$sftp_port" 127.0.0.1 >"$home_dir/.ssh/known_hosts" 2>/dev/null
  chmod 600 "$home_dir/.ssh/known_hosts"
  export HOME="$home_dir"
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
    run -d
    --name "$smb_container"
    -p "$smb_port:445"
    -v "$work_root/samba-share:/mount"
    "$smb_image"
    -u "iris;iris"
    -s "bench;/mount;yes;no;no;iris"
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

cleanup() {
  local status=$?
  if [[ "$keep" != "1" ]]; then
    if [[ -n "${mounted_by_script:-}" ]]; then
      umount "$mounted_by_script" >/dev/null 2>&1 || true
    fi
    docker rm -f "$sftp_container" "$smb_container" >/dev/null 2>&1 || true
    if [[ -n "${created_work_root:-}" ]]; then
      rm -rf "$created_work_root"
    fi
  else
    echo "kept perf work root: $work_root" >&2
  fi
  exit "$status"
}

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
