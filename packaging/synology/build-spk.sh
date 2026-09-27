#!/bin/bash
# Builds a Synology DSM 7 package (.spk) containing the static tunnel-client
# binaries for every supported CPU, so one file installs on any model.
#
# Usage: packaging/synology/build-spk.sh <version> <bin-dir> [out-dir]
#
# <bin-dir> must contain the release binaries:
#   tunnel-client-linux-amd64  tunnel-client-linux-arm64  tunnel-client-linux-armv7
#
# Also writes synology-feed.json next to the .spk. It is published as a release
# asset and read by the clientproxy.io Package Center source
# (proxy-admin functions/routes/synology-packages.js).
set -euo pipefail

VERSION="${1:?usage: build-spk.sh <version> <bin-dir> [out-dir]}"
BIN_DIR="${2:?usage: build-spk.sh <version> <bin-dir> [out-dir]}"
OUT_DIR="${3:-.}"
SRC="$(cd "$(dirname "$0")" && pwd)"
BUILD_NUM="${SPK_BUILD:-0001}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
export COPYFILE_DISABLE=1  # no AppleDouble ._ files when built on macOS

if tar --version 2>/dev/null | grep -q GNU; then
  TAR_OWNER=(--owner=0 --group=0)
else
  TAR_OWNER=(--uid 0 --gid 0)
fi

# package.tgz — installed to /var/packages/tunnel-client/target
mkdir -p "$WORK/package/bin" "$WORK/spk"
install -m 0755 "$SRC/package/bin/tunnel-client" "$WORK/package/bin/tunnel-client"
for arch in amd64 arm64 armv7; do
  install -m 0755 "$BIN_DIR/tunnel-client-linux-$arch" "$WORK/package/bin/tunnel-client-linux-$arch"
done
tar -czf "$WORK/spk/package.tgz" "${TAR_OWNER[@]}" -C "$WORK/package" .

CHECKSUM=$( (md5sum "$WORK/spk/package.tgz" 2>/dev/null || md5 -r "$WORK/spk/package.tgz") | cut -d' ' -f1)
sed -e "s/@VERSION@/${VERSION}-${BUILD_NUM}/" -e "s/@CHECKSUM@/${CHECKSUM}/" \
  "$SRC/INFO.in" > "$WORK/spk/INFO"

cp -R "$SRC/scripts" "$SRC/conf" "$SRC/WIZARD_UIFILES" "$WORK/spk/"
cp "$SRC/PACKAGE_ICON.PNG" "$SRC/PACKAGE_ICON_256.PNG" "$WORK/spk/"
chmod 0755 "$WORK/spk/scripts/"*
chmod 0644 "$WORK/spk/scripts/common"

mkdir -p "$OUT_DIR"
SPK="$OUT_DIR/tunnel-client-${VERSION}-synology-dsm7.spk"
# An .spk is an uncompressed tar with INFO first.
tar -cf "$SPK" "${TAR_OWNER[@]}" -C "$WORK/spk" \
  INFO package.tgz scripts conf WIZARD_UIFILES PACKAGE_ICON.PNG PACKAGE_ICON_256.PNG

# Feed manifest: the INFO fields plus what Package Center needs to download
# and verify the .spk.
SPK_MD5=$( (md5sum "$SPK" 2>/dev/null || md5 -r "$SPK") | cut -d' ' -f1)
SPK_SIZE=$(wc -c < "$SPK" | tr -d ' ')
python3 - "$WORK/spk/INFO" "$OUT_DIR/synology-feed.json" "$VERSION" "$(basename "$SPK")" "$SPK_SIZE" "$SPK_MD5" <<'PY'
import json, shlex, sys
info_path, out, tag, spk, size, md5 = sys.argv[1:]
info = {}
for line in open(info_path):
    key, _, value = line.strip().partition("=")
    if key:
        info[key] = shlex.split(value)[0] if value else ""
json.dump({
    "package": info["package"],
    "version": info["version"],
    "tag": "v" + tag,
    "dname": info["displayname"],
    "desc": info["description"],
    "maintainer": info["maintainer"],
    "maintainer_url": info["maintainer_url"],
    "distributor": info["distributor"],
    "distributor_url": info["distributor_url"],
    "support_url": info["support_url"],
    "os_min_ver": info["os_min_ver"],
    "spk": spk,
    "size": int(size),
    "md5": md5,
}, open(out, "w"), indent=2)
PY
echo "$SPK"
