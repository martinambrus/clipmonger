# saveclipimg.ps1: save the clipboard's formats and image (as PNG, and the raw DIB) for inspection.
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$d = [Windows.Forms.Clipboard]::GetDataObject()
$out = @('formats: ' + (($d.GetFormats()) -join ', '))
$img = [Windows.Forms.Clipboard]::GetImage()
if ($img) { $img.Save('C:\clipmonger-test\clip.png', [Drawing.Imaging.ImageFormat]::Png); $out += "image: $($img.Width)x$($img.Height) $($img.PixelFormat)" }
foreach ($f in 'DeviceIndependentBitmap', 'Format17', 'PNG') {
    $s = $d.GetData($f)
    if ($s -is [IO.MemoryStream]) { [IO.File]::WriteAllBytes("C:\clipmonger-test\clip-$f.bin", $s.ToArray()); $out += "$f bytes: $($s.Length)" }
}
[IO.File]::WriteAllLines('C:\clipmonger-test\clip.txt', $out)
