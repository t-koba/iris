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

fetch_retries=${IRIS_PDFIUM_FETCH_RETRIES:-3}
fetch_retry_delay=${IRIS_PDFIUM_FETCH_RETRY_DELAY_SECONDS:-2}
[[ $fetch_retries =~ ^[0-9]+$ ]] || { echo 'IRIS_PDFIUM_FETCH_RETRIES must be non-negative' >&2; exit 1; }
[[ $fetch_retry_delay =~ ^[0-9]+$ ]] || { echo 'IRIS_PDFIUM_FETCH_RETRY_DELAY_SECONDS must be non-negative' >&2; exit 1; }

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

profile=${IRIS_PDFIUM_PROFILE:-8138}
case "$profile" in
    8138|8179) ;;
    *)
        echo "IRIS_PDFIUM_PROFILE must be 8138 or 8179" >&2
        exit 1
        ;;
esac

# Pinned from https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium%2F8138
# (PDFium 159.0.8138.0). These are GitHub's digests for the immutable release;
# bump the tag, source commit, asset sizes, digests, and attestation digest together.
pdfium_release="chromium/8138"
pdfium_version="159.0.8138.0"
pdfium_commit="0eb94b28c2252d2baae3dfe15dc6b237131d8cd8"
pdfium_attestation_bytes=18267
pdfium_repo="bblanchon/pdfium-binaries"
pdfium_signer_workflow="github.com/bblanchon/pdfium-binaries/.github/workflows/build-all.yml"
pdfium_source_ref="refs/heads/master"
pdfium_attestation_sha256="7526f4b934ab498d6206ac7f3d78dd4ce8e0f7d3175227ab3f4ecc255d950ad7"
pdfium_major=159
pdfium_build=8138

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

# Opt-in candidate profile: overrides the defaults above before any gate runs.
if [[ $profile == 8179 ]]; then
    # shellcheck source=pdfium-8179-pins.sh
    source "$repo_root/scripts/pdfium-8179-pins.sh"
fi

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
    if [[ -n ${IRIS_PDFIUM_CACHE_DIR:-} ]]; then
        cache_dir=$IRIS_PDFIUM_CACHE_DIR
    else
        cache_dir=${pdfium_cache_dir:-"$repo_root/target/pdfium"}
    fi
    tarball="$cache_dir/$asset"
    if ! matches_pin "$tarball"; then
        mkdir -p "$cache_dir"
        partial="$tarball.part"
        rm -f -- "$partial"
        echo "downloading $url"
        curl --fail --location --silent --show-error \
            --retry "$fetch_retries" --retry-delay "$fetch_retry_delay" --proto '=https' --tlsv1.2 \
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

# One disposable directory owns the bundle and extracted files, even on failure.
extract=$(mktemp -d)
trap 'rm -rf -- "$extract"' EXIT
attestation_status=not_attempted
bundle_sha256=null
bundle_bytes=null
verifier_version_json=null
verifier_sha256_json=null
verifier_version=not_attempted
verifier_sha256=not_attempted
if [[ $verify_provenance == 1 ]]; then
    [[ -n ${GH_TOKEN:-} ]] || {
        echo "GH_TOKEN is required to verify PDFium build provenance" >&2
        exit 1
    }
    gh_command=$(command -v gh) || {
        echo "gh is required to verify PDFium build provenance" >&2
        exit 1
    }
    # Resolve symlinks once: version, digest and verification must use one executable,
    # not a PATH lookup (or a symlink target) that can differ between invocations.
    gh_executable=$(realpath -e -- "$gh_command")
    [[ $gh_command == */* && -f $gh_executable && -x $gh_executable ]] || {
        echo "cannot resolve the gh verifier executable" >&2
        exit 1
    }
    gh_version_output=$("$gh_executable" version)
    gh_version_line=${gh_version_output%%$'\n'*}
    # Provenance stores canonical stable X.Y.Z, at most 64 characters, without
    # a leading v, leading zeroes, build metadata or the CLI's date/help suffix.
    version_pattern='^gh version ((0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*))( \([^()]*\))?$'
    if [[ ! $gh_version_line =~ $version_pattern || ${#BASH_REMATCH[1]} -gt 64 ]]; then
        echo "cannot establish a canonical gh verifier version" >&2
        exit 1
    fi
    verifier_version=${BASH_REMATCH[1]}
    verifier_sha256=$(sha256sum "$gh_executable" | cut -d' ' -f1)
    [[ $verifier_sha256 =~ ^[0-9a-f]{64}$ ]] || {
        echo "cannot establish the gh verifier executable sha256" >&2
        exit 1
    }
    verifier_version_json="\"$verifier_version\""
    verifier_sha256_json="\"$verifier_sha256\""
    bundle="$extract/pdfium-attestation.json"
    curl --fail --location --silent --show-error \
        --retry "$fetch_retries" --retry-delay "$fetch_retry_delay" --proto '=https' --tlsv1.2 \
        --output "$bundle" \
        "https://github.com/$pdfium_repo/releases/download/$pdfium_release/pdfium-attestation.json"
    bundle_bytes=$(wc -c < "$bundle")
    bundle_sha256=$(sha256sum "$bundle" | cut -d' ' -f1)
    if [[ $bundle_bytes -ne $pdfium_attestation_bytes || $bundle_sha256 != "$pdfium_attestation_sha256" ]]; then
        echo "PDFium attestation bundle does not match the pinned size/sha256" >&2
        exit 1
    fi
    # gh binds the exact archive subject to the pinned bundle and upstream identity.
    # With set -e, any verification failure exits before recording success or staging.
    "$gh_executable" attestation verify "$tarball" --repo "$pdfium_repo" --bundle "$bundle" \
        --signer-workflow "$pdfium_signer_workflow" --signer-digest "$pdfium_commit" \
        --source-digest "$pdfium_commit" --source-ref "$pdfium_source_ref"
    bundle_sha256="\"$bundle_sha256\""
    attestation_status=verified
fi

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
grep -qx "MAJOR=$pdfium_major" "$extract/VERSION"
grep -qx "BUILD=$pdfium_build" "$extract/VERSION"

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
Attestation size pin: $pdfium_attestation_bytes
Attestation bundle sha256: $bundle_sha256
Attestation bundle bytes: $bundle_bytes
Attestation repository: $pdfium_repo
Attestation signer workflow: $pdfium_signer_workflow
Attestation signer/source digest: $pdfium_commit
Attestation source ref: $pdfium_source_ref
Attestation verifier gh version: $verifier_version
Attestation verifier executable sha256: $verifier_sha256
Attestation verification: $attestation_status (subject sha256 $asset_sha256)
Library sha256: $library_sha256
License:        see LICENSE and licenses/ in this directory
PROVENANCE
cat > "$doc_dir/PROVENANCE.json" <<PROVENANCE_JSON
{
  "schema": 2,
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
    "verifier_version": $verifier_version_json,
    "verifier_sha256": $verifier_sha256_json,
    "pin_sha256": "$pdfium_attestation_sha256",
    "bundle_sha256": $bundle_sha256,
    "bundle_bytes": $bundle_bytes,
    "repo": "$pdfium_repo",
    "signer_workflow": "$pdfium_signer_workflow",
    "signer_digest": "$pdfium_commit",
    "source_digest": "$pdfium_commit",
    "source_ref": "$pdfium_source_ref",
    "subject_sha256": "$asset_sha256"
  }
}
PROVENANCE_JSON
echo "staged PDFium $pdfium_version in $lib_dir"
