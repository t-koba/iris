#!/usr/bin/env bash
# Run on a fresh Ubuntu runner: prove the packaged PDFium loads from /usr, not
# the build tree. Both ordinary CI and release packaging use this installed smoke.
set -euo pipefail

if [[ $# -ne 3 ]]; then
    echo "usage: $0 <iris.deb> <AppDir> <pdf_thumbnail_smoke executable>" >&2
    exit 1
fi
deb=$(realpath "$1")
appdir=$2
smoke=$3
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
test ! -e /usr/lib/iris/libpdfium.so
test ! -e /usr/bin/iris-pdfium-smoke

cleanup() {
    local result=$?
    trap - EXIT
    sudo rm -f /usr/bin/iris-pdfium-smoke || result=1
    sudo apt-get remove -y iris || result=1
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
sudo cp "$smoke" /usr/bin/iris-pdfium-smoke
sudo chmod 0755 /usr/bin/iris-pdfium-smoke
/usr/bin/iris-pdfium-smoke
