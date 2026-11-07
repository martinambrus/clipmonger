# readclip.ps1: write the session clipboard (formats, text, image size) to
# C:\clipmonger-test\clip.txt as UTF-8 (run with powershell -STA).
Add-Type -AssemblyName System.Windows.Forms
$d = [Windows.Forms.Clipboard]::GetDataObject()
$out = @('formats: ' + (($d.GetFormats()) -join ', '))
if ([Windows.Forms.Clipboard]::ContainsText()) { $out += 'text: ' + [Windows.Forms.Clipboard]::GetText() }
if ([Windows.Forms.Clipboard]::ContainsImage()) { $i = [Windows.Forms.Clipboard]::GetImage(); $out += "image: $($i.Width)x$($i.Height)" }
[IO.File]::WriteAllLines('C:\clipmonger-test\clip.txt', $out, [Text.Encoding]::UTF8)
