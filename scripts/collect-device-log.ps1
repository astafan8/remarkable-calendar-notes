<#
.SYNOPSIS
Collect Calendar Notes diagnostics using one SSH password prompt.

.DESCRIPTION
Run this script on the Windows computer connected to the reMarkable. It
streams one diagnostics archive through one SSH session and still collects
AppLoad/xochitl launch errors when the application created no log.
#>
param(
    [string]$Device = "10.11.99.1",
    [string]$OutputDirectory = ".",
    [switch]$IncludeSystemLog
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

foreach ($command in @("ssh", "tar")) {
    if (-not (Get-Command $command -ErrorAction SilentlyContinue)) {
        throw "$command is required. Install Windows OpenSSH Client in Settings > Optional Features."
    }
}

$payloadPath = Join-Path $PSScriptRoot "device-diagnostics-remote.sh"
if (-not (Test-Path $payloadPath)) {
    throw "Missing $payloadPath. Keep the diagnostics bundle files together."
}

$payloadText = (Get-Content $payloadPath -Raw) -replace "`r`n", "`n"
$include = if ($IncludeSystemLog) { "1" } else { "0" }

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$name = "calendar-notes-diagnostics-$stamp"
$destination = Join-Path $OutputDirectory $name
$archive = Join-Path $OutputDirectory "$name.tar.gz"
New-Item -ItemType Directory -Force -Path $destination | Out-Null

Write-Host "Connecting to root@$Device (one SSH password prompt)..."
# The tablet has no `base64`. Feed the diagnostic script to a remote
# `sh -s` over ssh's stdin, and read the raw .tar.gz it streams back on
# stdout straight into a file (PowerShell's text pipeline would corrupt
# binary, so drive ssh via a process and copy the raw streams).
$psi = [System.Diagnostics.ProcessStartInfo]::new()
$psi.FileName = "ssh"
foreach ($arg in @("-o", "ConnectTimeout=30", "root@$Device", "INCLUDE_SYSTEM_LOG=$include sh -s")) {
    $psi.ArgumentList.Add($arg)
}
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.UseShellExecute = $false
$proc = [System.Diagnostics.Process]::Start($psi)
$scriptBytes = [Text.Encoding]::UTF8.GetBytes($payloadText)
$proc.StandardInput.BaseStream.Write($scriptBytes, 0, $scriptBytes.Length)
$proc.StandardInput.BaseStream.Flush()
$proc.StandardInput.Close()
$fileStream = [IO.File]::Open($archive, [IO.FileMode]::Create, [IO.FileAccess]::Write)
$proc.StandardOutput.BaseStream.CopyTo($fileStream)
$fileStream.Close()
$proc.WaitForExit()
if ($proc.ExitCode -ne 0) {
    throw "Diagnostics collection failed. Verify USB/Wi-Fi connectivity and the tablet SSH password."
}
if ((Get-Item $archive).Length -eq 0) {
    throw "The tablet returned an empty diagnostics archive."
}

& tar -xzf $archive -C $destination
if ($LASTEXITCODE -ne 0) {
    throw "The archive was downloaded but could not be extracted: $archive"
}

Write-Host "Diagnostics copied to $destination"
Write-Host "Shareable archive created at $archive"
