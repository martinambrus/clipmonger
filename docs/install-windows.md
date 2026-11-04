# Installing Clipmonger on Windows

> Draft. The Windows prompts below must be checked against a clean, current
> Windows 10 and Windows 11 install before each release
> ([T-1204](../tests/manual/T-1204-bypass-docs-accuracy.md)). Correct this page
> whenever a screen does not match.

Clipmonger 0.1.0 runs on 64-bit Windows 10 and Windows 11. The release has two
Windows downloads:

- `clipmonger-0.1.0-windows-x86_64-setup.exe`: installer for Program Files with
  a Start menu entry. Needed for pasting into windows that run as administrator
  without asking at every start.
- `clipmonger-0.1.0-windows-x86_64.zip`: portable copy. Nothing is installed;
  data is stored beside the program files.

Download `SHA256SUMS` from the same release as well.

## Why Windows warns you

The Clipmonger files are not digitally signed. Windows therefore shows warnings
the first time you download or run them. The steps below get past these
warnings. Check the checksum first, so you only bypass a warning for a file
that matches the release.

## 1. Check the checksum

Put the downloaded files and `SHA256SUMS` in one folder. Open PowerShell in
that folder and run:

```powershell
Get-Content SHA256SUMS | ForEach-Object { $hash, $name = $_ -split '\s+\*?', 2; if (Test-Path -LiteralPath $name) { if ((Get-FileHash -LiteralPath $name -Algorithm SHA256).Hash -eq $hash) { "OK      $name" } else { "FAILED  $name" } } }
```

Every file you downloaded must show `OK`. Files you did not download are
skipped. If any file shows `FAILED`, delete it and download it again; do not
run it.

A matching checksum shows the file was not damaged or changed in transit
relative to `SHA256SUMS`. It is not a signature: someone who can replace both
the file and `SHA256SUMS`, or who already controls your account, defeats it.

## 2. Browser download warning

Some browsers block unsigned downloads that few people have downloaded.

- Microsoft Edge: if the download shows "isn't commonly downloaded", open the
  download's **...** menu, choose **Keep**, then **Show more** and
  **Keep anyway**.
- Other browsers: choose the option to keep the file.

## 3a. Installer

1. Run `clipmonger-0.1.0-windows-x86_64-setup.exe`.
2. If a blue **Windows protected your PC** window appears, click **More info**.
   It shows **Unknown publisher**. Click **Run anyway**.
3. Approve the User Account Control prompt. It also shows an unknown publisher.
4. Finish the installer and start Clipmonger from the Start menu.

## 3b. Portable ZIP

1. Right-click the ZIP, choose **Properties**, tick **Unblock** at the bottom of
   the General tab and click **OK**. If there is no Unblock box, Windows did not
   mark the file and you can continue.
2. Extract the ZIP into a folder you own, for example in Documents. Do not
   extract it into Program Files: portable mode stores its data beside the
   program files.
3. Run `clipmonger.exe`. If **Windows protected your PC** still appears, click
   **More info**, then **Run anyway**.

If you skip step 1, each program in the folder may show the warning on its
first start.

## Smart App Control (Windows 11)

When Smart App Control is on, Windows blocks unsigned programs and offers no
**Run anyway** button. Clipmonger cannot run in that case. You can check the
setting in Windows Security under **App & browser control**, **Smart App
Control**. Turning it off is your decision; on some Windows versions it cannot
be turned back on without reinstalling Windows.

## First paste into an administrator window

Pasting into a program that runs as administrator needs a helper. The first
time, Clipmonger asks whether to start it, and Windows shows a User Account
Control prompt. With the installer this is asked once. With the portable ZIP
the helper is asked for each time Clipmonger starts.
