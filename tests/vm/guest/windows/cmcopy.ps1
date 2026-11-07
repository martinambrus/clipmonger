# cmcopy.ps1 -Kind text|file|imgfile|image|noise|secret -Value <text, UTF-8 file path or WxH>
# Puts synthetic test content on the clipboard (run with powershell -STA).
param([string]$Kind = 'text', [string]$Value = 'test')
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
switch ($Kind) {
    'text' { [Windows.Forms.Clipboard]::SetText($Value) }
    'file' { [Windows.Forms.Clipboard]::SetText([IO.File]::ReadAllText($Value, [Text.Encoding]::UTF8)) }
    'imgfile' { [Windows.Forms.Clipboard]::SetImage([Drawing.Image]::FromFile($Value)) }
    'secret' {
        # Password managers add this format; clipboard tools must not keep the copy.
        $data = New-Object Windows.Forms.DataObject
        $data.SetText($Value)
        $data.SetData('ExcludeClipboardContentFromMonitorProcessing', $false, (New-Object IO.MemoryStream(,[byte[]](0,0,0,0))))
        [Windows.Forms.Clipboard]::SetDataObject($data, $true)
    }
    default {
        $w, $h = $Value -split 'x' | ForEach-Object { [int]$_ }
        $bmp = New-Object Drawing.Bitmap $w, $h
        $rng = New-Object Random 7
        for ($y = 0; $y -lt $h; $y += 4) { for ($x = 0; $x -lt $w; $x += 4) {
            $c = if ($Kind -eq 'noise') { [Drawing.Color]::FromArgb($rng.Next(256), $rng.Next(256), $rng.Next(256)) } else { [Drawing.Color]::FromArgb(200, ($x * 3) % 256, ($y * 5) % 256) }
            for ($dy = 0; $dy -lt 4 -and $y + $dy -lt $h; $dy++) { for ($dx = 0; $dx -lt 4 -and $x + $dx -lt $w; $dx++) { $bmp.SetPixel($x + $dx, $y + $dy, $c) } }
        } }
        [Windows.Forms.Clipboard]::SetImage($bmp)
    }
}
