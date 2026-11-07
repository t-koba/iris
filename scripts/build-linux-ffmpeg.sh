#!/usr/bin/env bash
# Build only the pinned, signature-verified upstream shared runtime. No publishing.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
prefix=${IRIS_FFMPEG_PREFIX:-$root/target/linux-ffmpeg}
[[ $prefix = /* ]] || { echo 'IRIS_FFMPEG_PREFIX must be absolute' >&2; exit 1; }
# A completed prefix is immutable and revalidated before reuse. Failed builds
# leave no cache; the unique scratch tree is owned and removed by this invocation.
if [[ -e $prefix ]]; then
    python3 "$root/scripts/linux-ffmpeg.py" validate "$prefix"
    exit
fi
for tool in curl gpg tar make cc nasm pkg-config python3 patchelf readelf ldd; do command -v "$tool" >/dev/null; done
jobs=${IRIS_FFMPEG_JOBS:-2}
[[ $jobs =~ ^[1-9][0-9]*$ ]] || exit 1
mkdir -p "$(dirname "$prefix")"
work=$(mktemp -d "$(dirname "$prefix")/.ffmpeg-build.XXXXXXXX")
trap 'rm -rf -- "$work"' EXIT
trap 'exit 1' HUP INT TERM
export GNUPGHOME="$work/gnupg"
mkdir -m 700 "$GNUPGHOME"
policy="$root/packaging/linux/ffmpeg.json"
get() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$policy" "$1"; }
fetch() { curl --fail --location --silent --show-error --proto '=https' --proto-redir '=https' --tlsv1.2 "$1" -o "$2"; }
fetch "$(get url)" "$work/source.tar.xz"
fetch "$(get signature_url)" "$work/source.tar.xz.asc"
fetch "$(get key_url)" "$work/signing-key.asc"
echo "$(get sha256)  $work/source.tar.xz" | sha256sum --check --status
gpg --batch --import "$work/signing-key.asc"
# Require the primary key identity before accepting any signature from the keyring.
gpg --batch --with-colons --fingerprint --list-keys "$(get fingerprint)" |
    grep -Fx "fpr:::::::::$(get fingerprint):"
gpg --batch --status-fd 1 --verify "$work/source.tar.xz.asc" "$work/source.tar.xz" > "$work/signature.status"
python3 "$root/scripts/linux-ffmpeg.py" signature "$work/signature.status"
tar -xJf "$work/source.tar.xz" -C "$work"
cd "$work/ffmpeg-$(get version)"
mapfile -t flags < <(python3 -c 'import json,sys; print("\n".join(json.load(open(sys.argv[1]))["configure_flags"]))' "$policy")
# No optional codec/library autodetection or built-in decoder pruning. Enable
# the compatible basic compression libraries and libc iconv explicitly so PNG,
# TIFF and other compressed built-in formats retain their normal support.
# DESTDIR keeps an interrupted installation from ever appearing complete.
./configure --prefix="$prefix" --libdir="$prefix/lib" "${flags[@]}"
python3 "$root/scripts/linux-ffmpeg.py" config config.h
make -j"$jobs"
make DESTDIR="$work/install" install
staged="$work/install$prefix"
mkdir -p "$staged/share/iris-ffmpeg"
cp "$work/"{source.tar.xz,source.tar.xz.asc,signing-key.asc,signature.status} "$staged/share/iris-ffmpeg/"
cp COPYING.LGPLv2.1 config.h config_components.h ffbuild/config.mak "$staged/share/iris-ffmpeg/"
python3 "$root/scripts/linux-ffmpeg.py" record "$staged"
# Refuse races with another builder rather than nesting under/replacing its prefix.
mv -Tn "$staged" "$prefix"
test ! -e "$staged"
