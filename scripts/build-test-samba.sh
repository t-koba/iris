#!/usr/bin/env bash
# Build only the disposable native-test server; no registry or release artifacts.
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
snapshot=https://snapshot.debian.org/archive/debian/20261108T000000Z
version='2:4.25.1+dfsg-1'
keyring="${IRIS_PERF_DEBIAN_KEYRING:-/usr/share/keyrings/debian-archive-keyring.gpg}"
for cmd in debootstrap gpgv sudo docker tar; do
  command -v "$cmd" >/dev/null || { echo "required command is missing: $cmd" >&2; exit 1; }
done
[[ -s "$keyring" && -r "$keyring" ]] || { echo "Debian archive keyring missing: $keyring" >&2; exit 1; }
rootfs=$(mktemp -d "${TMPDIR:-/tmp}/iris-test-samba.XXXXXX")
trap 'sudo rm -rf -- "$rootfs"' EXIT
# --force-check-gpg refuses to bootstrap if archive signature verification is
# unavailable. The host keyring is installed by the CI package manager.
sudo debootstrap --variant=minbase --components=main --include=ca-certificates \
  --keyring="$keyring" --force-check-gpg sid "$rootfs" "$snapshot" >&2
sudo install -m 0644 "$keyring" "$rootfs/usr/share/keyrings/iris-debian-archive-keyring.gpg"
# Only Valid-Until is relaxed for this immutable historical snapshot, never
# signature or package-hash verification. No other apt sources may be used.
sudo rm -f "$rootfs/etc/apt/sources.list.d/"*
printf 'deb [signed-by=/usr/share/keyrings/iris-debian-archive-keyring.gpg check-valid-until=no] %s sid main\n' "$snapshot" \
  | sudo tee "$rootfs/etc/apt/sources.list" >/dev/null
printf '#!/bin/sh\nexit 101\n' | sudo tee "$rootfs/usr/sbin/policy-rc.d" >/dev/null
sudo chmod 0755 "$rootfs/usr/sbin/policy-rc.d"
sudo chroot "$rootfs" env DEBIAN_FRONTEND=noninteractive apt-get -o APT::Update::Error-Mode=any update >&2
sudo chroot "$rootfs" env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
  "samba=$version" "samba-common-bin=$version" >&2
[[ "$(sudo chroot "$rootfs" dpkg-query -W '-f=${Version}' samba)" == "$version" ]] \
  || { echo "unexpected Samba package version in test rootfs (expected $version)" >&2; exit 1; }
case "$(sudo chroot "$rootfs" smbd --version)" in
  'Version 4.25.1'|'Version 4.25.1-Debian') ;;
  *) echo 'unexpected smbd version in test rootfs' >&2; exit 1 ;;
esac
sudo install -m 0755 "$script_dir/test-samba-entrypoint.sh" "$rootfs/usr/local/sbin/iris-test-samba"
sudo install -m 0644 "$script_dir/test-samba.conf" "$rootfs/etc/samba/smb.conf"
sudo chroot "$rootfs" apt-get clean >&2
sudo rm -rf "$rootfs/var/lib/apt/lists/"*
# Import a rootfs stream directly, with no Dockerfile base or mutable image tag.
image_id=$(sudo tar --numeric-owner --exclude='./dev/*' --exclude='./proc/*' --exclude='./sys/*' -C "$rootfs" -c . \
  | docker import --change 'ENTRYPOINT ["/usr/local/sbin/iris-test-samba"]' -)
[[ "$image_id" =~ ^sha256:[0-9a-f]{64}$ ]] || { echo 'docker import did not return a local sha256 image ID' >&2; exit 1; }
printf 'Built local Samba test image %s from Debian snapshot %s; archive signature verification required; samba=%s\n' \
  "$image_id" "$snapshot" "$version" >&2
printf '%s\n' "$image_id"
