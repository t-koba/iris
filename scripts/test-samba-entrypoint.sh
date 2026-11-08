#!/bin/sh
# Synthetic iris/iris credentials are confined to this local integration server.
set -eu
[ "$(dpkg-query -W '-f=${Version}' samba)" = '2:4.25.1+dfsg-1' ] \
  || { echo 'unexpected Samba package version in test image' >&2; exit 1; }
expected_smbd_version='Version 4.25.1-Debian-4.25.1+dfsg-1'
[ "$(smbd --version)" = "$expected_smbd_version" ] \
  || { echo 'unexpected smbd version in test image' >&2; exit 1; }
# Keep the effective config on disk so testparm failure diagnostics see it.
sed '/^\[bench\]/,$d' /etc/samba/smb.conf >/etc/samba/smb.conf.tmp
while [ "$#" -gt 0 ]; do
  [ "$1" = -g ] && [ "$#" -ge 2 ] || exit 2
  case "$2" in
    *'
'*|*"$(printf '\r')"*) exit 2 ;;
    'server min protocol = '*|'server max protocol = '*|'server signing = '*|'smb encrypt = '*|'workgroup = '*|'log level = '*) ;;
    *) exit 2 ;;
  esac
  printf '%s\n' "$2" >>/etc/samba/smb.conf.tmp
  shift 2
done
sed -n '/^\[bench\]/,$p' /etc/samba/smb.conf >>/etc/samba/smb.conf.tmp
mv /etc/samba/smb.conf.tmp /etc/samba/smb.conf
# Match the bind-mounted directory owner so uploads remain removable by the
# harness on the host, without granting anonymous access or chmodding the share.
groupadd --non-unique --gid "$(stat -c %g /mount)" iris
useradd --non-unique --uid "$(stat -c %u /mount)" --gid iris --no-create-home --shell /usr/sbin/nologin iris
printf 'iris\niris\n' | smbpasswd -a -s iris
mkdir -p /run/samba /var/log/samba
testparm -s /etc/samba/smb.conf >/dev/null
exec smbd --foreground --no-process-group --configfile=/etc/samba/smb.conf
