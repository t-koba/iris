#!/usr/bin/env bash
# Run on a fresh Ubuntu runner: prove the packaged native runtimes load from
# /usr, not the build tree. Both ordinary CI and release packaging use this installed smoke.
set -euo pipefail

if [[ $# -ne 3 && $# -ne 5 ]]; then
    echo "usage: $0 <iris.deb> <AppDir> <pdf_thumbnail_smoke executable> [<video_thumbnail_smoke executable> <clip>]" >&2
    exit 1
fi
deb=$(realpath "$1")
appdir=$2
smoke=$3
video=${4:-}
clip=${5:-}
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
test -f "$deb"
test -x "$smoke"
test -f "$appdir/usr/bin/Iris"
test -f "$appdir/usr/lib/iris/libpdfium.so"
# Do not replace a pre-existing package, installation, or smoke executable.
package_status=$(dpkg-query -W -f='${db:Status-Abbrev}' iris 2>/dev/null || :)
if [[ $package_status == i* ]]; then
    echo "package iris is already installed or being installed; refusing to replace it" >&2
    exit 1
fi
test ! -e /usr/bin/Iris
test ! -e /usr/lib/iris
test ! -L /usr/lib/iris
test ! -e /usr/bin/iris-video-smoke
test ! -e /usr/bin/iris-pdfium-smoke

cleanup() {
    local result=$?
    trap - EXIT
    sudo rm -f /usr/bin/iris-pdfium-smoke /usr/bin/iris-video-smoke || result=1
    sudo apt-get remove -y iris || result=1
    if [[ -e /usr/lib/iris ]]; then
        test -z "$(find /usr/lib/iris -mindepth 1 -print -quit)" || result=1
    fi
    test ! -e /usr/bin/iris-video-smoke || result=1
    test ! -e /usr/bin/iris-pdfium-smoke || result=1
    test ! -e /usr/bin/Iris || result=1
    test ! -e /usr/lib/iris/libpdfium.so || result=1
    exit "$result"
}
# Register before apt: even a partially failed installation must be removed.
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
sudo apt-get install -y "$deb"
test -x /usr/bin/Iris
test -f /usr/share/applications/iris.desktop
test -f /usr/share/doc/iris/pdfium/LICENSE
cmp "$appdir/usr/bin/Iris" /usr/bin/Iris
cmp "$appdir/usr/lib/iris/libpdfium.so" /usr/lib/iris/libpdfium.so
python3 "$repo_root/scripts/linux-ffmpeg.py" check / \
    "${IRIS_LINUX_INSTALLED_AUDIT:-$repo_root/dist/linux/installed-ffmpeg-audit.json}"
if [[ -n $video ]]; then
    test -x "$video"
    test -f "$clip"
    sudo cp "$video" /usr/bin/iris-video-smoke
    sudo chmod 0755 /usr/bin/iris-video-smoke
    sudo patchelf --set-rpath '$ORIGIN/../lib/iris' /usr/bin/iris-video-smoke
    /usr/bin/iris-video-smoke "$clip"
fi
sudo cp "$smoke" /usr/bin/iris-pdfium-smoke
sudo chmod 0755 /usr/bin/iris-pdfium-smoke
sudo patchelf --set-rpath '$ORIGIN/../lib/iris' /usr/bin/iris-pdfium-smoke
/usr/bin/iris-pdfium-smoke
