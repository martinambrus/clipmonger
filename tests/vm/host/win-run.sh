#!/bin/sh
# win-run.sh <ssh-alias> <program> [arguments]: start a program in the
# desktop session of the user logged in at the Windows VM's screen (an SSH
# login runs in session 0, without a desktop or that user's clipboard).
# Uses a one-off interactive scheduled task, deleted again right away.
# RUNLEVEL=Highest starts it elevated (an administrator account, no UAC prompt).
set -eu
alias=${1:?usage: win-run.sh <ssh-alias> <program> [arguments]}
prog=${2:?program}
shift 2
args="$*"
ssh "$alias" "
\$user = (Get-CimInstance Win32_ComputerSystem).UserName
if (-not \$user) { 'nobody is logged in at the screen'; exit 1 }
\$action = New-ScheduledTaskAction -Execute '$prog' $( [ -n "$args" ] && printf "%s" "-Argument '$args'" )
\$principal = New-ScheduledTaskPrincipal -UserId \$user -LogonType Interactive -RunLevel ${RUNLEVEL:-Limited}
Register-ScheduledTask -TaskName cm-run -Action \$action -Principal \$principal -Force | Out-Null
Start-ScheduledTask -TaskName cm-run
Start-Sleep -Milliseconds 800
Unregister-ScheduledTask -TaskName cm-run -Confirm:\$false
\"started for \${user}: $prog $args\"
" </dev/null
