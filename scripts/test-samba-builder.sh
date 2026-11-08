#!/usr/bin/env bash
# No root, network, Docker daemon, or real package installation in this test.
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
for file in build-test-samba.sh test-samba-entrypoint.sh perf-transfer-containers.sh; do
  bash -n "$script_dir/$file"
done
sh -n "$script_dir/test-samba-entrypoint.sh"
test_root=$(mktemp -d "${TMPDIR:-/tmp}/iris-samba-policy.XXXXXX")
trap 'rm -rf "$test_root"' EXIT
mkdir -p "$test_root/bin" "$test_root/tmp" "$test_root/capture"
printf 'mock archive keyring\n' >"$test_root/keyring"
cat >"$test_root/bin/mock" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "${0##*/}" in
  sudo) exec "$@" ;;
  gpgv) exit 99 ;; # Only debootstrap is responsible for calling the real gpgv.
  debootstrap)
    printf '%s\n' "$@" >"$CAPTURE/bootstrap"
    [[ "$*" == *'--force-check-gpg'* && "$*" == *'--keyring='* ]]
    [[ "$*" != *'--no-check-gpg'* ]]
    [[ "$FAIL_STAGE" != signature ]] || exit 1
    rootfs="${@: -2:1}"
    mkdir -p "$rootfs"/{etc/apt/sources.list.d,etc/samba,usr/share/keyrings,usr/sbin,usr/local/sbin,var/lib/apt/lists}
    printf 'obsolete source\n' >"$rootfs/etc/apt/sources.list.d/obsolete.sources"
    ;;
  chroot)
    rootfs=$1; shift
    printf '%s\n' "$*" >>"$CAPTURE/chroot"
    case "$*" in
      *'apt-get '*update)
        [[ "$FAIL_STAGE" != apt ]] || exit 1
        [[ ! -e "$rootfs/etc/apt/sources.list.d/obsolete.sources" ]]
        cp "$rootfs/etc/apt/sources.list" "$CAPTURE/sources"
        cmp "$IRIS_PERF_DEBIAN_KEYRING" "$rootfs/usr/share/keyrings/iris-debian-archive-keyring.gpg"
        ;;
      dpkg-query*) [[ "$FAIL_STAGE" != package ]] || { echo '2:4.23.8-1'; exit; }; echo '2:4.25.1+dfsg-1' ;;
      'smbd --version') [[ "$FAIL_STAGE" != binary ]] || { echo 'Version 4.23.8'; exit; }; echo 'Version 4.25.1-Debian' ;;
    esac
    ;;
  docker)
    [[ "$1" == import && "$*" == *'ENTRYPOINT ["/usr/local/sbin/iris-test-samba"]'* ]]
    cat >/dev/null
    touch "$CAPTURE/imported"
    printf 'sha256:%064d\n' 0
    ;;
  tar) exit 0 ;; # No rootfs bytes are needed by the mock import.
esac
MOCK
chmod +x "$test_root/bin/mock"
for cmd in sudo debootstrap gpgv chroot docker tar; do ln -s mock "$test_root/bin/$cmd"; done
export PATH="$test_root/bin:$PATH" TMPDIR="$test_root/tmp" CAPTURE="$test_root/capture"
export IRIS_PERF_DEBIAN_KEYRING="$test_root/keyring" FAIL_STAGE=none
run_builder() { bash "$script_dir/build-test-samba.sh" >"$test_root/out" 2>"$test_root/err"; }
run_builder
[[ "$(cat "$test_root/out")" == "sha256:$(printf '%064d' 0)" && -f "$CAPTURE/imported" ]]
grep -Fq 'Built local Samba test image sha256:' "$test_root/err"
grep -Fq 'https://snapshot.debian.org/archive/debian/20261108T000000Z' "$test_root/err"
grep -Fq 'samba=2:4.25.1+dfsg-1' "$test_root/err"
[[ -z "$(ls -A "$TMPDIR")" ]]
grep -Fxq 'https://snapshot.debian.org/archive/debian/20261108T000000Z' "$CAPTURE/bootstrap"
grep -Fxq -- '--force-check-gpg' "$CAPTURE/bootstrap"
grep -Fxq -- "--keyring=$IRIS_PERF_DEBIAN_KEYRING" "$CAPTURE/bootstrap"
grep -Fxq 'deb [signed-by=/usr/share/keyrings/iris-debian-archive-keyring.gpg check-valid-until=no] https://snapshot.debian.org/archive/debian/20261108T000000Z sid main' "$CAPTURE/sources"
grep -Fq 'samba=2:4.25.1+dfsg-1 samba-common-bin=2:4.25.1+dfsg-1' "$CAPTURE/chroot"
for FAIL_STAGE in signature apt package binary; do
  export FAIL_STAGE
  rm "$CAPTURE/imported" 2>/dev/null || true
  if run_builder; then echo "accepted $FAIL_STAGE failure" >&2; exit 1; fi
  [[ ! -e "$CAPTURE/imported" && -z "$(ls -A "$TMPDIR")" ]]
done
export IRIS_PERF_DEBIAN_KEYRING="$test_root/missing"
if run_builder; then echo 'accepted missing keyring' >&2; exit 1; fi
grep -Fq 'Debian archive keyring missing:' "$test_root/err"
# Protect the local server contract, not application or release behavior.
for setting in '[bench]' 'path = /mount' 'valid users = iris' 'guest ok = no' 'map to guest = Never' 'security = user' 'read only = no' 'smb ports = 445' 'log file = /var/log/samba/log.%m'; do
  grep -Fxq "$setting" "$script_dir/test-samba.conf"
done
if grep -E 'docker (pull|push|build)|--no-check-gpg|trusted=yes|allow-unauthenticated' "$script_dir/build-test-samba.sh"; then
  echo 'unsafe test-image build policy' >&2; exit 1
fi
grep -Fq 'run -d --pull=never' "$script_dir/perf-transfer-containers.sh"
printf 'Samba builder policy/config: PASS (signed snapshot, exact versions, fail-closed paths, rootfs cleanup, local import)\n'
