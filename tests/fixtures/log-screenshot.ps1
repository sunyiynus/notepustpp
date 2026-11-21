param(
    [string[]]$File = @((Join-Path $PSScriptRoot "..\generated\sample-1gb.log")),
    [string]$Out = (Join-Path $PSScriptRoot "..\generated\log-view.png"),
    [ValidateSet("debug", "release")]
    [string]$Configuration = "release",
    [int]$TimeoutSeconds = 20
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public struct Rect { public int Left; public int Top; public int Right; public int Bottom; }
public static class Win32Rect {
    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out Rect rect);
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")]
    public static extern bool SetProcessDPIAware();
}
"@

$exe = Join-Path $PSScriptRoot "..\..\target\$Configuration\notepust.exe"
$filesToOpen = @($File | ForEach-Object { $_ -split '[;,]' } | Where-Object { $_ })
$process = Start-Process -FilePath $exe -ArgumentList $filesToOpen -PassThru
$watch = [System.Diagnostics.Stopwatch]::StartNew()
$title = ""
while ($watch.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
    Start-Sleep -Milliseconds 100
    $process.Refresh()
    if ($process.HasExited) { throw "notepust exited with code $($process.ExitCode)" }
    if ($process.MainWindowTitle -like "*- Notepust*") {
        $title = $process.MainWindowTitle
        break
    }
}
if (-not $title) { throw "window title never became a document title" }
Start-Sleep -Seconds 2
$process.Refresh()
[void][Win32Rect]::SetProcessDPIAware()
[void][Win32Rect]::SetForegroundWindow($process.MainWindowHandle)
Start-Sleep -Milliseconds 400
$rect = New-Object Rect
if (-not [Win32Rect]::GetWindowRect($process.MainWindowHandle, [ref]$rect)) {
    throw "could not read the window rectangle"
}
$width = $rect.Right - $rect.Left
$height = $rect.Bottom - $rect.Top
$bitmap = New-Object System.Drawing.Bitmap $width, $height
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
$bitmap.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$graphics.Dispose()
$bitmap.Dispose()
Write-Output ("Title=" + $title)
Write-Output ("Screenshot=" + $Out)
Write-Output ("Size=" + $width + "x" + $height)
Stop-Process -Id $process.Id -Force
