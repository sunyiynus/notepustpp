param(
    [ValidateSet("debug", "release")]
    [string]$Configuration = "debug"
)

$ErrorActionPreference = "Stop"
$exe = Join-Path $PSScriptRoot "..\..\target\$Configuration\notepust.exe"
$firstFile = Join-Path $PSScriptRoot "hello.txt"
$secondFile = Join-Path $PSScriptRoot "..\generated\small-mixed-eol.txt"
if (-not (Test-Path $secondFile)) {
    throw "missing fixture: $secondFile"
}

Get-Process notepust -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 300

$first = Start-Process -FilePath $exe -ArgumentList @($firstFile) -PassThru
$watch = [System.Diagnostics.Stopwatch]::StartNew()
$firstTitle = ""
while ($watch.Elapsed.TotalSeconds -lt 15) {
    Start-Sleep -Milliseconds 100
    $first.Refresh()
    if ($first.HasExited) { throw "first instance exited early" }
    if ($first.MainWindowTitle -like "*- Notepust*") {
        $firstTitle = $first.MainWindowTitle
        break
    }
}
if (-not $firstTitle) { throw "first instance never showed a document title" }

$secondWatch = [System.Diagnostics.Stopwatch]::StartNew()
$second = Start-Process -FilePath $exe -ArgumentList @($secondFile) -PassThru
if (-not $second.WaitForExit(10000)) {
    Stop-Process -Id $second.Id -Force
    throw "second instance did not forward and exit"
}
$secondCode = $second.ExitCode
$second.ExitCode | Out-Null
$forwardMs = [math]::Round($secondWatch.Elapsed.TotalMilliseconds)

$secondName = [System.IO.Path]::GetFileName($secondFile)
$watch.Restart()
$forwardedTitle = ""
while ($watch.Elapsed.TotalSeconds -lt 10) {
    Start-Sleep -Milliseconds 100
    $first.Refresh()
    if ($first.MainWindowTitle -like "*$secondName*") {
        $forwardedTitle = $first.MainWindowTitle
        break
    }
}
$instances = @(Get-Process notepust -ErrorAction SilentlyContinue).Count

Write-Output ("FirstTitle=" + $firstTitle)
Write-Output ("SecondExitCode=" + $secondCode)
Write-Output ("SecondExitMs=" + $forwardMs)
Write-Output ("ForwardedTitle=" + $forwardedTitle)
Write-Output ("NotepustInstances=" + $instances)

Stop-Process -Id $first.Id -Force
if (-not $forwardedTitle) { throw "the first instance did not open the forwarded file" }
if ($instances -ne 1) { throw "expected exactly one Notepust process" }
