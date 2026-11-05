#!/usr/bin/env bash
set -euo pipefail

max_total_time="${FUZZ_MAX_TOTAL_TIME:-60}"
runs="${FUZZ_RUNS:-}"
outer_timeout="${FUZZ_OUTER_TIMEOUT:-}"
input_timeout="${FUZZ_INPUT_TIMEOUT:-}"
max_len="${FUZZ_MAX_LEN:-}"
build_only="${FUZZ_BUILD_ONLY:-}"

run_target() {
  FUZZ_MAX_TOTAL_TIME="$max_total_time" \
    FUZZ_RUNS="$runs" \
    FUZZ_OUTER_TIMEOUT="$outer_timeout" \
    FUZZ_INPUT_TIMEOUT="$input_timeout" \
    FUZZ_MAX_LEN="$max_len" \
    FUZZ_BUILD_ONLY="$build_only" \
    "$(dirname "$0")/fuzz-target.sh" "$1"
}

run_target raw_tiff_parse
run_target raw_bmff
run_target image_sniff
run_target raw_cr3_box
run_target raw_x3f_parse
run_target config_load
run_target remote_transport_mount_table
run_target segment_parse
run_target ngram_normalize
run_target filter_token_parse
run_target smb_packet_parse
run_target smb_negotiate_parse
run_target smb_transform_parse
run_target smb_ntlmssp_parse
run_target smb_directory_parse
