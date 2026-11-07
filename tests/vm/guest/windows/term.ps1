# term.ps1: a console window that saves one pasted line to C:\clipmonger-test\term.txt.
$host.UI.RawUI.WindowTitle = 'Clipmonger test console'
$x = Read-Host 'paste here'
Set-Content -Path C:\clipmonger-test\term.txt -Value $x -Encoding UTF8
