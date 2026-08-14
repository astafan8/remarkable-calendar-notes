#!/usr/bin/env sh
# Install an official Calendar Notes ZIP using one SSH password prompt.
#
# Works with either the all-in-one `remarkable-calendar-notes-<ver>.zip` or
# the app-only `-armv7.zip`. The app is always installed; the optional
# xochitl sidebar launcher is installed only when --sidebar is given.
set -eu

device="10.11.99.1"
bundle=""
sidebar=0

usage() {
    echo "Usage: install-device.sh --bundle RELEASE.zip [--device IP] [--sidebar]"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --bundle)
            [ "$#" -ge 2 ] || { usage >&2; exit 2; }
            bundle="$2"
            shift 2
            ;;
        --device)
            [ "$#" -ge 2 ] || { usage >&2; exit 2; }
            device="$2"
            shift 2
            ;;
        --sidebar)
            sidebar=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

[ -r "$bundle" ] || { echo "error: bundle not found: $bundle" >&2; exit 1; }
for command in ssh base64; do
    command -v "$command" >/dev/null 2>&1 ||
        { echo "error: $command is required" >&2; exit 1; }
done

remote_body='
set -eu
archive=/tmp/remarkable-calendar-notes-install.zip
stage=/tmp/remarkable-calendar-notes-install
rm -rf "$stage"
mkdir -p "$stage"
base64 -d >"$archive"
unzip -oq "$archive" -d "$stage"
app="$(find "$stage" -type f -name external.manifest.json -path "*/remarkable-calendar-notes/*" | head -n 1)"
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
if [ "$want_sidebar" = "1" ]; then
    qmd="$(find "$stage" -type f -name calendarNotesSidebar.qmd | head -n 1)"
    rcc="$(find "$stage" -type f -name calendarNotesSidebar.rcc | head -n 1)"
    if [ -n "$qmd" ]; then
        install -m 644 "$qmd" /home/root/xovi/exthome/qt-resource-rebuilder/calendarNotesSidebar.qmd
    fi
    if [ -n "$rcc" ]; then
        install -m 644 "$rcc" /home/root/xovi/exthome/qt-resource-rebuilder/calendarNotesSidebar.rcc
    fi
    if [ -n "$qmd" ]; then
        xovi/rebuild_hashtable
        echo "Sidebar launcher installed; restart XOVI/xochitl to see the icon."
    else
        echo "warning: --sidebar requested but the bundle has no sidebar files." >&2
    fi
fi
rm -rf "$stage" "$archive"
rm -f /home/root/.local/share/remarkable-calendar-notes/process-started.txt
echo "Calendar Notes installed; executable permissions verified."
'

remote_install="want_sidebar=$sidebar
$remote_body"

echo "Connecting to root@$device (one SSH password prompt)..."
base64 <"$bundle" | ssh -o ConnectTimeout=10 "root@$device" "$remote_install"
