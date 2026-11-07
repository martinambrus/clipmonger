#!/bin/sh
# deploy-win.sh <ssh-alias> [dir]: copy the Windows release build of
# Clipmonger into a Windows test VM. The VM downloads the files from a
# short-lived web server on the libvirt network (scp does not work with
# every Windows OpenSSH install) and checks their SHA-256 sums.
set -eu
alias=${1:?usage: deploy-win.sh <ssh-alias> [target dir]}
dest=${2:-C:\\clipmonger-test}
build="$(cd "$(dirname "$0")/../../.." && pwd)/target/x86_64-pc-windows-gnu/release"
port=8766
stage=$(mktemp -d)
trap 'kill $server 2>/dev/null || true; rm -rf "$stage"' EXIT
for f in clipmonger.exe clipmonger-daemon.exe clipmonger-cli.exe; do
    cp "$build/$f" "$stage/"
done
(cd "$stage" && sha256sum *.exe | awk '{print $2" "$1}' > SHA256SUMS)
python3 -m http.server "$port" --bind 192.168.122.1 --directory "$stage" >/dev/null 2>&1 &
server=$!
sleep 1
ssh "$alias" "
\$ErrorActionPreference = 'Stop'
\$dir = '$dest'
\$base = 'http://192.168.122.1:$port'
New-Item -ItemType Directory -Force \$dir | Out-Null
\$wc = New-Object Net.WebClient
\$ok = \$true
foreach (\$line in (\$wc.DownloadString(\"\$base/SHA256SUMS\") -split \"\`n\" | Where-Object { \$_ })) {
    \$name, \$want = \$line.Trim() -split ' '
    \$path = Join-Path \$dir \$name
    \$wc.DownloadFile(\"\$base/\$name\", \$path)
    \$got = (Get-FileHash \$path -Algorithm SHA256).Hash.ToLower()
    if (\$got -eq \$want) { \"\$name ok\" } else { \"\$name CHECKSUM MISMATCH\"; \$ok = \$false }
}
if (-not \$ok) { exit 1 }
" </dev/null
