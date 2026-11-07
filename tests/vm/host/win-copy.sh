#!/bin/sh
# win-copy.sh <alias> <kind> <value>: put test content on the clipboard of
# the user logged in at the Windows VM's screen (via win-run.sh).
exec "$(dirname "$0")/win-run.sh" "$1" powershell.exe "-NoProfile -STA -WindowStyle Hidden -ExecutionPolicy Bypass -File C:\\clipmonger-test\\cmcopy.ps1 -Kind $2 -Value $3"
