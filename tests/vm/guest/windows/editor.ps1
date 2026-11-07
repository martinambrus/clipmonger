# Test paste target: a text window that saves every change to
# C:\clipmonger-test\editor.txt (and shows whether it runs elevated).
Add-Type -AssemblyName System.Windows.Forms
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$form = New-Object Windows.Forms.Form
$form.Text = "Clipmonger test editor" + $(if ($admin) { " (administrator)" } else { "" })
$form.Width = 640; $form.Height = 360; $form.StartPosition = 'CenterScreen'; $form.TopMost = $true
$box = New-Object Windows.Forms.TextBox
$box.Multiline = $true; $box.Dock = 'Fill'; $box.ScrollBars = 'Vertical'
$box.Font = New-Object Drawing.Font('Consolas', 11)
$box.Add_TextChanged({ Set-Content -Path C:\clipmonger-test\editor.txt -Value $box.Text -Encoding UTF8 })
# WinForms text boxes ignore Ctrl+A; tests use it to clear the box.
$box.Add_KeyDown({ if ($_.Control -and $_.KeyCode -eq 'A') { $box.SelectAll(); $_.SuppressKeyPress = $true } })
$form.Controls.Add($box)
$form.Add_Shown({ $form.Activate(); $box.Focus() })
[void]$form.ShowDialog()
