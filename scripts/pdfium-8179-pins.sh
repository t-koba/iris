#!/usr/bin/env bash
# Opt-in validation pins for the date-later chromium/8179 PDFium candidate.
# Sourced by fetch-pdfium-runtime.sh only when IRIS_PDFIUM_PROFILE=8179;
# the production chromium/8138 defaults and this opt-in never change each other.
# Values are GitHub's API-reported digests for the immutable release.
pdfium_release="chromium/8179"
pdfium_version="160.0.8179.0"
pdfium_commit="02feff9f75486f5caac18147db45087b6ffbbec6"
pdfium_attestation_bytes=18083
pdfium_attestation_sha256="f9d4bb0be84c30c98b98b2550ed2f81efc4024cd172511a5e786163c9ae8deb6"
pdfium_major=160
pdfium_build=8179

case "$(uname -m)" in
    x86_64|amd64)
        asset="pdfium-linux-x64.tgz"
        asset_bytes=3953118
        asset_sha256="b4766a6bf2856efbcc6c3cc7d6ae9022bdc6bb36ddc1acdd92d1f1873f773f4b"
        ;;
    aarch64|arm64)
        asset="pdfium-linux-arm64.tgz"
        asset_bytes=3819293
        asset_sha256="18bb6d568257b28d14f6c3b8bfb48caf73d4615b319ca4d43b424c2a03431d9f"
        ;;
    *)
        echo "no validated PDFium 8179 candidate for architecture $(uname -m)" >&2
        return 1
        ;;
esac

# Keep the candidate tarball out of the production PDFium cache unless a cache
# directory is explicitly supplied by the caller.
pdfium_cache_dir="$repo_root/target/pdfium-8179"
