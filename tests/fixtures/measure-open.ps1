param(
    [string]$File = "tests/generated/sample-10mb.log",
    [int]$TimeoutSeconds = 60
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$exe = Join-Path $root "target\release\notepust.exe"
$target = Resolve-Path (Join-Path $root $File)
$watch = [System.Diagnostics.Stopwatch]::StartNew()
$process = Start-Process -FilePath $exe -ArgumentList @($target) -PassThru
$ready = $false
while ($watch.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
    Start-Sleep -Milliseconds 50
    $process.Refresh()
    if ($process.MainWindowTitle -like "*$([System.IO.Path]::GetFileName($target))*") {
        $ready = $true
        break
    }
}
$elapsedMs = [math]::Round($watch.Elapsed.TotalMilliseconds)
Start-Sleep -Seconds 2
$process.Refresh()
Write-Output ("File=" + $target)
Write-Output ("Ready=" + $ready)
Write-Output ("ElapsedMs=" + $elapsedMs)
Write-Output ("Title=" + $process.MainWindowTitle)
Write-Output ("Responding=" + $process.Responding)
Stop-Process -Id $process.Id -Force
