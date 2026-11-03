# Run only with the system execution policy permitting both reviewed scripts.
param([Parameter(Mandatory=$true)][string]$CleanupPath)
$ErrorActionPreference = 'Stop'
$classifier = & $CleanupPath -InstallDir 'C:\Temp' -Mode TaskExceptionClassifier
if ($classifier -isnot [scriptblock]) { throw 'Classifier unavailable' }
$fileMissing = [IO.FileNotFoundException]::new('synthetic missing')
$fileWrapped = [Management.Automation.MethodInvocationException]::new('wrapper', $fileMissing)
$fileNested = [Management.Automation.MethodInvocationException]::new('wrapper', [Reflection.TargetInvocationException]::new($fileMissing))
$otherIo = [IO.IOException]::new('synthetic IO failure', -2147024891)
$missing = [Runtime.InteropServices.COMException]::new('synthetic missing', -2147024894)
$denied = [Runtime.InteropServices.COMException]::new('synthetic denied', -2147024891)
$wrappedMissing = [Management.Automation.MethodInvocationException]::new('wrapper', $missing)
$wrappedDenied = [Management.Automation.MethodInvocationException]::new('wrapper', $denied)
$target = [Reflection.TargetInvocationException]::new($missing)
$nested = [Management.Automation.MethodInvocationException]::new('wrapper', $target)
$unknown = [Exception]::new('unknown wrapper', $missing)
$empty = [Management.Automation.MethodInvocationException]::new('without inner')
if (-not (& $classifier $fileMissing)) { throw 'direct FileNotFound not accepted' }
if (-not (& $classifier $fileWrapped)) { throw 'wrapped FileNotFound not accepted' }
if (-not (& $classifier $fileNested)) { throw 'target-wrapped FileNotFound not accepted' }
if (& $classifier $otherIo) { throw 'other IO failure incorrectly accepted' }
if (-not (& $classifier $missing)) { throw 'direct missing not accepted' }
if (-not (& $classifier $wrappedMissing)) { throw 'wrapped missing not accepted' }
if (-not (& $classifier $nested)) { throw 'target-wrapped missing not accepted' }
if ((& $classifier $denied) -or (& $classifier $wrappedDenied)) { throw 'access denied incorrectly accepted' }
if ((& $classifier $unknown) -or (& $classifier $empty) -or (& $classifier $null)) { throw 'unknown wrapper incorrectly accepted' }
# Exercise the same catch decision: nonmissing and unknown errors must propagate.
foreach ($exception in @($wrappedDenied, $otherIo, $unknown, $empty)) {
    $propagated = $false
    try {
        try { throw $exception } catch { if (-not (& $classifier $_.Exception)) { throw } }
    } catch { $propagated = $true }
    if (-not $propagated) { throw 'nonmissing exception was swallowed' }
}
Write-Output 'Task exception controls: PASS'
