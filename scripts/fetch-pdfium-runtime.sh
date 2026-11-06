#!/usr/bin/env bash
# Stages the pinned PDFium runtime, its licenses, and its build metadata into a
# Linux AppDir root. Fails closed when the archive does not match the pin.
set -euo pipefail

usage="usage: $0 <AppDir root>"
if [[ $# -ne 1 || -z ${1:-} ]]; then
    echo "$usage" >&2
    exit 1
fi
dest=$1

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

# Pinned from https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium%2F8138
# (PDFium 159.0.8138.0). These are GitHub's digests for the immutable release;
# bump the tag, source commit, asset sizes, digests, and attestation digest together.
pdfium_release="chromium/8138"
pdfium_version="159.0.8138.0"
pdfium_commit="0eb94b28c2252d2baae3dfe15dc6b237131d8cd8"
pdfium_attestation_sha256="7526f4b934ab498d6206ac7f3d78dd4ce8e0f7d3175227ab3f4ecc255d950ad7"

for tool in curl tar sha256sum mktemp; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "$tool is required to stage the PDFium runtime" >&2
        exit 1
    }
done

case "$(uname -m)" in
    x86_64|amd64)
        asset="pdfium-linux-x64.tgz"
        asset_bytes=3945444
        asset_sha256="d6b1e28950a0392590d79bbdbb7eb19686872b688f8a36f170acb19e41473113"
        ;;
    aarch64|arm64)
        asset="pdfium-linux-arm64.tgz"
        asset_bytes=3811232
        asset_sha256="c70b1ce4d13257f741af19e0aeee575890875995eb81370a65be942e601a1862"
        ;;
    *)
        echo "no verified PDFium runtime for architecture $(uname -m)" >&2
        exit 1
        ;;
esac

url="https://github.com/bblanchon/pdfium-binaries/releases/download/$pdfium_release/$asset"

matches_pin() {
    [[ -f $1 ]] &&
        [[ $(wc -c < "$1") -eq $asset_bytes ]] &&
        [[ $(sha256sum "$1" | cut -d' ' -f1) == "$asset_sha256" ]]
}

if [[ -n ${IRIS_PDFIUM_TARBALL:-} ]]; then
    tarball=$IRIS_PDFIUM_TARBALL
    if ! matches_pin "$tarball"; then
        echo "IRIS_PDFIUM_TARBALL does not match the pinned $asset size/sha256" >&2
        exit 1
    fi
else
    cache_dir=${IRIS_PDFIUM_CACHE_DIR:-"$repo_root/target/pdfium"}
    tarball="$cache_dir/$asset"
    if ! matches_pin "$tarball"; then
        mkdir -p "$cache_dir"
        partial="$tarball.part"
        rm -f -- "$partial"
        echo "downloading $url"
        curl --fail --location --silent --show-error \
            --retry 3 --retry-delay 2 --proto '=https' --tlsv1.2 \
            --output "$partial" "$url"
        if ! matches_pin "$partial"; then
            echo "downloaded $asset does not match the pinned size/sha256" >&2
            rm -f -- "$partial"
            exit 1
        fi
        mv -f -- "$partial" "$tarball"
    fi
fi

verify_provenance=${IRIS_VERIFY_PDFIUM_PROVENANCE:-0}
case "$verify_provenance" in
    0|1) ;;
    *)
        echo "IRIS_VERIFY_PDFIUM_PROVENANCE must be 0 or 1" >&2
        exit 1
        ;;
esac

attestation_status=not_attempted
if [[ $verify_provenance == 1 ]]; then
    [[ -n ${GH_TOKEN:-} ]] || {
        echo "GH_TOKEN is required to verify PDFium build provenance" >&2
        exit 1
    }
    command -v gh >/dev/null 2>&1 || {
        echo "gh is required to verify PDFium build provenance" >&2
        exit 1
    }
    # With set -e, a failed verification exits before any successful status is recorded.
    gh attestation verify "$tarball" --repo bblanchon/pdfium-binaries
    attestation_status=verified
fi

extract=$(mktemp -d)
trap 'rm -rf -- "$extract"' EXIT
tar -xzf "$tarball" -C "$extract" --no-same-owner --no-same-permissions

for member in LICENSE VERSION args.gn licenses/pdfium.txt lib/libpdfium.so; do
    if [[ ! -f "$extract/$member" ]]; then
        echo "PDFium archive is missing $member" >&2
        exit 1
    fi
done
if [[ -L "$extract/lib/libpdfium.so" ]]; then
    echo "PDFium archive contains a symlinked library" >&2
    exit 1
fi
grep -qx 'MAJOR=159' "$extract/VERSION"
grep -qx 'BUILD=8138' "$extract/VERSION"

lib_dir="$dest/usr/lib/iris"
doc_dir="$dest/usr/share/doc/iris/pdfium"
install -d -m 0755 "$lib_dir" "$doc_dir"
install -m 0755 "$extract/lib/libpdfium.so" "$lib_dir/libpdfium.so"
install -m 0644 "$extract/LICENSE" "$doc_dir/LICENSE"
install -m 0644 "$extract/VERSION" "$doc_dir/VERSION"
install -m 0644 "$extract/args.gn" "$doc_dir/args.gn"
cp -R "$extract/licenses" "$doc_dir/licenses"
chmod -R u+rwX,go+rX,go-w "$doc_dir/licenses"

library_sha256=$(sha256sum "$lib_dir/libpdfium.so" | cut -d' ' -f1)
cat > "$doc_dir/PROVENANCE" <<PROVENANCE
PDFium runtime bundled with Iris for Linux.

Package:        PDFium $pdfium_version
Upstream tag:   $pdfium_release
Source commit:  $pdfium_commit
Asset:          $asset
Asset bytes:    $asset_bytes
Asset sha256:   $asset_sha256
Download:       $url
Release:        immutable GitHub release by bblanchon/pdfium-binaries
Attestation digest pin: $pdfium_attestation_sha256
Attestation verification: $attestation_status (subject sha256 $asset_sha256)
Library sha256: $library_sha256
License:        see LICENSE and licenses/ in this directory
PROVENANCE
cat > "$doc_dir/PROVENANCE.json" <<PROVENANCE_JSON
{
  "schema": 1,
  "archive": {
    "asset": "$asset",
    "bytes": $asset_bytes,
    "sha256": "$asset_sha256"
  },
  "runtime": {
    "path": "usr/lib/iris/libpdfium.so",
    "sha256": "$library_sha256"
  },
  "attestation": {
    "status": "$attestation_status",
    "pin_sha256": "$pdfium_attestation_sha256",
    "subject_sha256": "$asset_sha256"
  }
}
PROVENANCE_JSON
echo "staged PDFium $pdfium_version in $lib_dir"
