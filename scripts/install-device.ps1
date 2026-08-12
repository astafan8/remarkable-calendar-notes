<#
.SYNOPSIS
Install an official Calendar Notes ZIP using one SSH password prompt.
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$Bundle,
    [string]$Device = "10.11.99.1"
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false
if (-not (Test-Path $Bundle -PathType Leaf)) {
    throw "Bundle not found: $Bundle"
}
if (-not (Get-Command ssh -ErrorAction SilentlyContinue)) {
    throw "ssh is required. Install Windows OpenSSH Client in Settings > Optional Features."
}

$encoded = [Convert]::ToBase64String([IO.File]::ReadAllBytes((Resolve-Path $Bundle)))
$remoteInstall = (@'
set -eu
archive=/tmp/remarkable-calendar-notes-install.zip
stage=/tmp/remarkable-calendar-notes-install
rm -rf "$stage"
mkdir -p "$stage"
base64 -d >"$archive"
unzip -oq "$archive" -d "$stage"
app="$(find "$stage" -type f -name external.manifest.json -path '*/remarkable-calendar-notes/*' | head -n 1)"
[ -n "$app" ] || { echo "Calendar Notes app not found in bundle" >&2; exit 1; }
app_dir="$(dirname "$app")"
destination=/home/root/xovi/exthome/appload/remarkable-calendar-notes
incoming=/home/root/xovi/exthome/appload/.remarkable-calendar-notes.new.$$
backup=/home/root/xovi/exthome/appload/.remarkable-calendar-notes.previous
restore_previous() {
    if [ ! -d "$destination" ] && [ -d "$backup" ]; then
        mv "$backup" "$destination"
    fi
}
trap restore_previous EXIT HUP INT TERM
restore_previous
rm -rf "$incoming"
cp -a "$app_dir" "$incoming"
chmod 755 "$incoming/remarkable-calendar-notes"
"$incoming/remarkable-calendar-notes" --help >/dev/null 2>&1
rm -rf "$backup"
if [ -d "$destination" ]; then
    mv "$destination" "$backup"
fi
if mv "$incoming" "$destination"; then
    rm -rf "$backup"
else
    [ ! -d "$backup" ] || mv "$backup" "$destination"
    exit 1
fi
qmd="$(find "$stage" -type f -name 'calendarNotesSidebar.qmd' | head -n 1)"
rcc="$(find "$stage" -type f -name 'calendarNotesSidebar.rcc' | head -n 1)"
if [ -n "$qmd" ]; then
    install -m 644 "$qmd" /home/root/xovi/exthome/qt-resource-rebuilder/calendarNotesSidebar.qmd
fi
if [ -n "$rcc" ]; then
    install -m 644 "$rcc" /home/root/xovi/exthome/qt-resource-rebuilder/calendarNotesSidebar.rcc
fi
rm -rf "$stage" "$archive"
rm -f /home/root/.local/share/remarkable-calendar-notes/process-started.txt
if [ -n "$qmd" ]; then
    xovi/rebuild_hashtable
fi
echo "Calendar Notes installed; executable permissions verified."
'@) -replace "`r`n", "`n"

Write-Host "Connecting to root@$Device (one SSH password prompt)..."
$encoded | & ssh -o ConnectTimeout=10 "root@$Device" $remoteInstall
if ($LASTEXITCODE -ne 0) {
    throw "Installation failed."
}
