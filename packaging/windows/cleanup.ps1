# Machine-wide uninstall cleanup and separately selected default-profile removal.
param([Parameter(Mandatory=$true)][string]$InstallDir, [ValidateSet("Cleanup", "ProfileInfo", "ProfileDelete", "TaskExceptionClassifier")][string]$Mode = "Cleanup", [string]$Expected = "")
$ErrorActionPreference = 'Stop'
# Accept the missing-task HRESULT, including through the wrappers used by the
# PowerShell COM adapter. Unknown wrappers and other HRESULTs remain failures.
function Test-MissingTaskException([Exception]$exception) {
    while ($exception -is [Management.Automation.MethodInvocationException] -or
           $exception -is [Reflection.TargetInvocationException]) {
        $exception = $exception.InnerException
    }
    return ($null -ne $exception -and $exception.HResult -eq -2147024894)
}
# This read-only mode exposes the exact classifier to focused policy-respecting tests.
if ($Mode -eq 'TaskExceptionClassifier') { return ${function:Test-MissingTaskException} }
if ($Mode -ne 'Cleanup') {
    Add-Type -Path (Join-Path $InstallDir 'profile-removal.cs')
    if ($Mode -eq 'ProfileInfo') {
        $d = [ClipmongerProfileRemoval]::Discover()
        $path = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($d.Path))
        $result = "$path`:$($d.Sid)`:$($d.AnchorId)`:$($d.TargetId)"
        if ($result.Length -gt 900) { throw 'Profile information exceeds the supported length' }
        [Console]::Out.Write($result)
    } else {
        $parts = $Expected.Split(':')
        if ($parts.Length -ne 4 -or $Expected.Length -gt 900) { throw 'Invalid confirmation' }
        $d = New-Object 'ClipmongerProfileRemoval+Discovery'
        $d.Path = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String($parts[0]))
        $d.Sid = $parts[1]; $d.AnchorId = $parts[2]; $d.TargetId = $parts[3]
        [ClipmongerProfileRemoval]::DeleteConfirmed($d)
    }
    exit 0
}
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
public static class ClipmongerCleanup {
  [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool QueryFullProcessImageName(IntPtr h, uint flags, StringBuilder name, ref int length);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool TerminateProcess(IntPtr h, uint exitCode);
  [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(IntPtr h, uint milliseconds);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [DllImport("shell32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern IntPtr CommandLineToArgvW(string command, out int count);
  [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr p);
  public static string Executable(string command) {
    int count; IntPtr args = CommandLineToArgvW(command, out count);
    if (args == IntPtr.Zero || count == 0) throw new Win32Exception(Marshal.GetLastWin32Error());
    try { return Marshal.PtrToStringUni(Marshal.ReadIntPtr(args)); } finally { LocalFree(args); }
  }
  public static bool StopIfInstalled(uint pid, string ui, string daemon, string cli) {
    IntPtr h = OpenProcess(0x1000 | 0x0001 | 0x00100000, false, pid);
    if (h == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
    try {
      var buf = new StringBuilder(32768); int n = buf.Capacity;
      if (!QueryFullProcessImageName(h, 0, buf, ref n)) throw new Win32Exception(Marshal.GetLastWin32Error());
      string path = buf.ToString();
      if (!String.Equals(path, ui, StringComparison.OrdinalIgnoreCase) &&
          !String.Equals(path, daemon, StringComparison.OrdinalIgnoreCase) &&
          !String.Equals(path, cli, StringComparison.OrdinalIgnoreCase)) return false;
      if (!TerminateProcess(h, 0)) throw new Win32Exception(Marshal.GetLastWin32Error());
      if (WaitForSingleObject(h, 10000) != 0) throw new Exception("Process did not stop");
      return true;
    } finally { CloseHandle(h); }
  }
}
'@
$root = [IO.Path]::GetFullPath($InstallDir).TrimEnd('\')
$ui = Join-Path $root 'clipmonger.exe'
$daemon = Join-Path $root 'clipmonger-daemon.exe'
$cli = Join-Path $root 'clipmonger-cli.exe'

# Task Scheduler gives structured actions, not marker text. An unrelated task
# with the same name is preserved; any query or shape uncertainty stops cleanup.
$service = New-Object -ComObject 'Schedule.Service'
$service.Connect()
$folder = $service.GetFolder('\')
$task = $null
try { $task = $folder.GetTask('ClipmongerHelper') }
catch {
    if (-not (Test-MissingTaskException $_.Exception)) { throw }
}
if ($null -ne $task) {
    $actions = $task.Definition.Actions
    if ($actions.Count -ne 1) { throw 'ClipmongerHelper has an ambiguous action list' }
    $action = $actions.Item(1)
    if ($action.Type -ne 0 -or [string]::IsNullOrWhiteSpace($action.Path)) { throw 'ClipmongerHelper is not a single executable action' }
    if (-not [IO.Path]::IsPathRooted($action.Path)) { throw 'ClipmongerHelper has a relative action path' }
    $taskPath = [IO.Path]::GetFullPath($action.Path)
    if ([string]::Equals($taskPath, $daemon, [StringComparison]::OrdinalIgnoreCase)) {
        $folder.DeleteTask('ClipmongerHelper', 0)
    }
}

# Only loaded user hives can be examined. Retain any entry whose parsed command
# does not name this installation; never remove an entire Run key.
$users = [Microsoft.Win32.Registry]::Users
foreach ($sid in $users.GetSubKeyNames()) {
    if ($sid -notmatch '^S-1-5-21(-[0-9]+){4}$') { continue }
    $run = $users.OpenSubKey("$sid\Software\Microsoft\Windows\CurrentVersion\Run", $true)
    if ($null -eq $run) { continue }
    try {
        $value = $run.GetValue('clipmonger', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        if ($null -eq $value) { continue }
        if ($value -isnot [string]) { throw "Unrecognized Run entry in $sid" }
        $exe = [ClipmongerCleanup]::Executable($value)
        if (-not [IO.Path]::IsPathRooted($exe)) { throw "Ambiguous Run entry in $sid" }
        if ([string]::Equals([IO.Path]::GetFullPath($exe), $ui, [StringComparison]::OrdinalIgnoreCase)) {
            $run.DeleteValue('clipmonger', $false)
        }
    } finally { $run.Dispose() }
}

# Snapshot names narrow the candidates; image identity is re-read from an open
# handle and that same handle is used for termination and wait.
foreach ($process in Get-Process -Name 'clipmonger','clipmonger-daemon','clipmonger-cli' -ErrorAction SilentlyContinue) {
    [void][ClipmongerCleanup]::StopIfInstalled([uint32]$process.Id, $ui, $daemon, $cli)
}
