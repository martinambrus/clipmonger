#!/bin/sh
# win-readclip.sh <alias>: print the clipboard of the user at the Windows VM's screen.
"$(dirname "$0")/win-run.sh" "$1" powershell.exe "-NoProfile -STA -WindowStyle Hidden -ExecutionPolicy Bypass -File C:\\clipmonger-test\\readclip.ps1" >/dev/null
sleep 3
ssh "$1" '[Console]::OutputEncoding = [Text.Encoding]::UTF8; Get-Content C:\clipmonger-test\clip.txt -Encoding UTF8' </dev/null
