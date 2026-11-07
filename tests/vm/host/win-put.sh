#!/bin/sh
# win-put.sh <alias> <local file> <windows path>: copy one file into a
# Windows VM. The VM downloads it from a short-lived web server on the
# libvirt network (SSH stdin stops after 64 KB with Win32-OpenSSH, and scp
# does not work with every install), then checks its SHA-256.
set -eu
alias=${1:?usage: win-put.sh <alias> <local file> <windows path>}
src=${2:?local file}
dest=${3:?windows path}
port=8767
stage=$(mktemp -d)
trap 'kill $server 2>/dev/null || true; rm -rf "$stage"' EXIT
cp "$src" "$stage/payload"
want=$(sha256sum "$src" | cut -d' ' -f1)
python3 -m http.server "$port" --bind 192.168.122.1 --directory "$stage" >/dev/null 2>&1 &
server=$!
sleep 1
ssh "$alias" "
(New-Object Net.WebClient).DownloadFile('http://192.168.122.1:$port/payload', '$dest')
\$got = (Get-FileHash '$dest' -Algorithm SHA256).Hash.ToLower()
if (\$got -eq '$want') { '$dest ok (' + (Get-Item '$dest').Length + ' bytes)' } else { '$dest CHECKSUM MISMATCH'; exit 1 }
" </dev/null
