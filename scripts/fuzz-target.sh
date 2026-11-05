#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <fuzz-target>" >&2
  exit 2
fi

target="$1"
case "$target" in
  raw_tiff_parse|raw_bmff|image_sniff|raw_cr3_box|raw_x3f_parse)
    crate_dir="crates/thumb-preview"
    ;;
  config_load)
    crate_dir="crates/ui-shell"
    ;;
  remote_transport_mount_table)
    crate_dir="crates/remote-transport"
    ;;
  segment_parse|ngram_normalize|filter_token_parse)
    crate_dir="crates/search"
    ;;
  smb_packet_parse|smb_negotiate_parse|smb_transform_parse|smb_ntlmssp_parse|smb_directory_parse)
    crate_dir="crates/smb-client"
    ;;
  *)
    echo "unknown fuzz target: $target" >&2
    exit 2
    ;;
esac

max_total_time="${FUZZ_MAX_TOTAL_TIME:-60}"
runs="${FUZZ_RUNS:-}"
outer_timeout="${FUZZ_OUTER_TIMEOUT:-}"
input_timeout="${FUZZ_INPUT_TIMEOUT:-}"
max_len="${FUZZ_MAX_LEN:-}"
build_only="${FUZZ_BUILD_ONLY:-}"
fuzz_toolchain="${FUZZ_TOOLCHAIN:-nightly-2026-07-11}"

run_args=()
label="build"
if [[ -z "$build_only" ]]; then
  if [[ -n "$runs" ]]; then
    run_args=(-runs="$runs")
    label="${runs} runs"
  else
    run_args=(-max_total_time="$max_total_time")
    label="${max_total_time}s"
  fi
fi
if [[ -n "$input_timeout" ]]; then
  run_args+=("-timeout=$input_timeout")
fi
if [[ -n "$max_len" ]]; then
  run_args+=("-max_len=$max_len")
fi

echo "==> ${crate_dir}: ${target} (${label})"
(
  cd "$crate_dir"
  if [[ -n "$build_only" ]]; then
    cargo "+$fuzz_toolchain" fuzz build "$target"
  elif [[ -n "$outer_timeout" ]] && command -v timeout >/dev/null 2>&1; then
    timeout -k 10 "$outer_timeout" cargo "+$fuzz_toolchain" fuzz run "$target" -- "${run_args[@]}"
  else
    cargo "+$fuzz_toolchain" fuzz run "$target" -- "${run_args[@]}"
  fi
)
