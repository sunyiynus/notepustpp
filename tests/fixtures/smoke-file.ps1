param(
    [string]$File = "tests/generated/sample-10mb.log",
    [int]$WaitSeconds = 8
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$exe = Join-Path $root "target\release\notepust.exe"
$target = Resolve-Path (Join-Path $root $File)
$process = Start-Process -FilePath $exe -ArgumentList @($target) -PassThru
Start-Sleep -Seconds $WaitSeconds
$process.Refresh()
Write-Output ("File=" + $target)
Write-Output ("PID=" + $process.Id)
Write-Output ("Title=" + $process.MainWindowTitle)
Write-Output ("Responding=" + $process.Responding)
Stop-Process -Id $process.Id -Force
