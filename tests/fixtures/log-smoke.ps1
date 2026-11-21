param(
    [string]$File = (Join-Path $PSScriptRoot "..\generated\sample-1gb.log"),
    [ValidateSet("debug", "release")]
    [string]$Configuration = "release",
    [int]$TimeoutSeconds = 20
)

$ErrorActionPreference = "Stop"
$exe = Join-Path $PSScriptRoot "..\..\target\$Configuration\notepust.exe"
if (-not (Test-Path $exe)) {
    throw "missing executable: $exe"
}
if (-not (Test-Path $File)) {
    throw "missing log fixture: $File (run cargo run -p xtask --release -- fixtures --out tests/generated --huge)"
}

$watch = [System.Diagnostics.Stopwatch]::StartNew()
$process = Start-Process -FilePath $exe -ArgumentList @($File) -PassThru
$title = ""
while ($watch.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
    Start-Sleep -Milliseconds 100
    $process.Refresh()
    if ($process.HasExited) {
        throw "notepust exited with code $($process.ExitCode)"
    }
    if ($process.MainWindowTitle -like "*- Notepust*") {
        $title = $process.MainWindowTitle
        break
    }
}
$titleMs = [math]::Round($watch.Elapsed.TotalMilliseconds)
Start-Sleep -Seconds 3
$process.Refresh()
$memoryMb = [math]::Round($process.WorkingSet64 / 1MB)

Write-Output ("Configuration=" + $Configuration)
Write-Output ("File=" + $File)
Write-Output ("PID=" + $process.Id)
Write-Output ("Title=" + $title)
Write-Output ("TitleMs=" + $titleMs)
Write-Output ("Responding=" + $process.Responding)
Write-Output ("WorkingSetMB=" + $memoryMb)

Stop-Process -Id $process.Id -Force
