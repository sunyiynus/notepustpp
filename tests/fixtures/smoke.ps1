param(
    [ValidateSet("debug", "release")]
    [string]$Configuration = "debug"
)

$ErrorActionPreference = "Stop"
$exe = Join-Path $PSScriptRoot "..\..\target\$Configuration\notepust.exe"
$file = Join-Path $PSScriptRoot "hello.txt"
$process = Start-Process -FilePath $exe -ArgumentList @($file) -PassThru
Start-Sleep -Seconds 5
$process.Refresh()
Write-Output ("Configuration=" + $Configuration)
Write-Output ("PID=" + $process.Id)
Write-Output ("Title=" + $process.MainWindowTitle)
Write-Output ("Responding=" + $process.Responding)
Stop-Process -Id $process.Id -Force
