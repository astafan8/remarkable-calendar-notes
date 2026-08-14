<#
.SYNOPSIS
Install an official Calendar Notes ZIP using one SSH password prompt.
.DESCRIPTION
Works with the all-in-one remarkable-calendar-notes-<ver>.zip release
archive. The app is always installed; the optional xochitl
sidebar launcher is installed only with -Sidebar.
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$Bundle,
    [string]$Device = "10.11.99.1",
    [switch]$Sidebar
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false
if (-not (Test-Path $Bundle -PathType Leaf)) {
    throw "Bundle not found: $Bundle"
}
if (-not (Get-Command ssh -ErrorAction SilentlyContinue)) {
    throw "ssh is required. Install Windows OpenSSH Client in Settings > Optional Features."
}

$wantSidebar = if ($Sidebar) { "1" } else { "0" }
$remoteInstall = (@"
want_sidebar=$wantSidebar
set -eu
archive=/tmp/remarkable-calendar-notes-install.zip
stage=/tmp/remarkable-calendar-notes-install
rm -rf "`$stage"
mkdir -p "`$stage"
cat >"`$archive"
unzip -oq "`$archive" -d "`$stage"
app="`$(find "`$stage" -type f -name external.manifest.json -path '*/remarkable-calendar-notes/*' | head -n 1)"
[ -n "`$app" ] || { echo "Calendar Notes app not found in bundle" >&2; exit 1; }
app_dir="`$(dirname "`$app")"
destination=/home/root/xovi/exthome/appload/remarkable-calendar-notes
incoming=/home/root/xovi/exthome/appload/.remarkable-calendar-notes.new.`$`$
backup=/home/root/xovi/exthome/appload/.remarkable-calendar-notes.previous
restore_previous() {
    if [ ! -d "`$destination" ] && [ -d "`$backup" ]; then
        mv "`$backup" "`$destination"
    fi
}
trap restore_previous EXIT HUP INT TERM
restore_previous
rm -rf "`$incoming"
cp -a "`$app_dir" "`$incoming"
chmod 755 "`$incoming/remarkable-calendar-notes"
"`$incoming/remarkable-calendar-notes" --help >/dev/null 2>&1
rm -rf "`$backup"
if [ -d "`$destination" ]; then
    mv "`$destination" "`$backup"
fi
if mv "`$incoming" "`$destination"; then
    rm -rf "`$backup"
else
    [ ! -d "`$backup" ] || mv "`$backup" "`$destination"
    exit 1
fi
if [ "`$want_sidebar" = "1" ]; then
    qmd="`$(find "`$stage" -type f -name 'calendarNotesSidebar.qmd' | head -n 1)"
    rcc="`$(find "`$stage" -type f -name 'calendarNotesSidebar.rcc' | head -n 1)"
    if [ -n "`$qmd" ]; then
        install -m 644 "`$qmd" /home/root/xovi/exthome/qt-resource-rebuilder/calendarNotesSidebar.qmd
    fi
    if [ -n "`$rcc" ]; then
        install -m 644 "`$rcc" /home/root/xovi/exthome/qt-resource-rebuilder/calendarNotesSidebar.rcc
    fi
    if [ -n "`$qmd" ]; then
        xovi/rebuild_hashtable
        echo "Sidebar launcher installed; restart XOVI/xochitl to see the icon."
    else
        echo "warning: -Sidebar requested but the bundle has no sidebar files." >&2
    fi
fi
rm -rf "`$stage" "`$archive"
rm -f /home/root/.local/share/remarkable-calendar-notes/process-started.txt
echo "Calendar Notes installed; executable permissions verified."
"@) -replace "`r`n", "`n"

Write-Host "Connecting to root@$Device (one SSH password prompt)..."
# Stream the raw ZIP to ssh's stdin as bytes and save it on the device with
# `cat` (no `base64` is required on the tablet -- it isn't installed there).
# PowerShell's pipeline is text-oriented and would corrupt binary, so drive
# ssh via a process whose stdin we write raw bytes to.
$psi = [System.Diagnostics.ProcessStartInfo]::new()
$psi.FileName = "ssh"
foreach ($arg in @("-o", "ConnectTimeout=30", "root@$Device", $remoteInstall)) {
    $psi.ArgumentList.Add($arg)
}
$psi.RedirectStandardInput = $true
$psi.UseShellExecute = $false
$proc = [System.Diagnostics.Process]::Start($psi)
$bytes = [IO.File]::ReadAllBytes((Resolve-Path $Bundle))
$proc.StandardInput.BaseStream.Write($bytes, 0, $bytes.Length)
$proc.StandardInput.BaseStream.Flush()
$proc.StandardInput.Close()
$proc.WaitForExit()
if ($proc.ExitCode -ne 0) {
    Write-Host ""
    Write-Host "If this failed at the SSH connection (e.g. 'Timeout during banner" -ForegroundColor Yellow
    Write-Host "exchange' with no password prompt), the address is almost certainly" -ForegroundColor Yellow
    Write-Host "wrong -- not the transfer:" -ForegroundColor Yellow
    Write-Host "  * Over USB: keep the default 10.11.99.1 and check the cable" -ForegroundColor Yellow
    Write-Host "    (http://10.11.99.1 should open the reMarkable USB web UI)."   -ForegroundColor Yellow
    Write-Host "  * Over Wi-Fi: pass -Device <tablet IP>, e.g. -Device 192.168.1.100." -ForegroundColor Yellow
    throw "Installation failed."
}

