#!/bin/sh
# Calendar Notes — on-device diagnostic.
#
# Run this DIRECTLY ON the reMarkable (it is read-only and changes nothing).
# The easiest way, from the computer connected to the tablet:
#
#     ssh root@10.11.99.1 'sh -s' < diagnose-on-device.sh
#
# ...then copy the whole terminal output and paste it back. It answers the
# single most important question — can the app binary even execute on this
# device? — which the log-file collectors cannot, because if the binary
# fails to start there is no log to collect.

echo "===== Calendar Notes on-device diagnostic ====="
echo "date: $(date 2>/dev/null)"
echo

echo "----- reMarkable OS / kernel -----"
# The OS version matters: a firmware update replaces the root filesystem
# (and its C library / dynamic loader), and wipes XOVI/AppLoad. That is the
# classic "worked once, then never again" cause.
cat /etc/version 2>/dev/null
sed -n 's/^REMARKABLE_RELEASE_VERSION=/os version: /p' /usr/share/remarkable/update.conf 2>/dev/null
grep -E 'PRETTY_NAME|VERSION_ID' /etc/os-release 2>/dev/null
uname -a
echo

echo "----- XOVI / AppLoad state -----"
ls -la /home/root/xovi 2>/dev/null || echo "  /home/root/xovi does NOT exist (XOVI not installed / wiped)"
echo "appload apps:"
ls -la /home/root/xovi/exthome/appload 2>/dev/null || echo "  no appload directory"
echo "qt-resource-rebuilder patches (sidebar etc.):"
ls -la /home/root/xovi/exthome/qt-resource-rebuilder 2>/dev/null || echo "  none"
echo "running processes of interest:"
ps ax 2>/dev/null | grep -iE 'xovi|appload|xochitl|remarkable-cal' | grep -v grep || \
  ps 2>/dev/null | grep -iE 'xovi|appload|xochitl|remarkable-cal' | grep -v grep
echo "qtfb socket:"
ls -la /tmp/qtfb.sock 2>/dev/null || echo "  /tmp/qtfb.sock NOT present (AppLoad QTFB host not running)"
echo

APP_DIR=$(dirname "$(find /home/root/xovi/exthome/appload -name external.manifest.json 2>/dev/null | grep -i remarkable-calendar-notes | head -n 1)" 2>/dev/null)
echo "----- App directory -----"
echo "app dir: ${APP_DIR:-NOT FOUND}"
if [ -n "$APP_DIR" ]; then
    ls -la "$APP_DIR"
fi
BIN="$APP_DIR/remarkable-calendar-notes"
echo

echo "----- Manifest -----"
if [ -f "$APP_DIR/external.manifest.json" ]; then
    cat "$APP_DIR/external.manifest.json"
else
    echo "  no external.manifest.json found"
fi
echo

echo "----- Binary details -----"
if [ -f "$BIN" ]; then
    ls -la "$BIN"
    file "$BIN" 2>/dev/null || echo "  (no 'file' command on device)"
    echo "program interpreter (empty = static, which is good):"
    readelf -l "$BIN" 2>/dev/null | grep -i 'interpreter' || echo "  (none reported / no readelf)"
    echo "dynamically needed libraries (empty = static):"
    readelf -d "$BIN" 2>/dev/null | grep 'NEEDED' || echo "  (none / no readelf)"
    echo "ldd:"
    ldd "$BIN" 2>&1 || echo "  (ldd failed — expected for a static binary)"
    echo "presence of the loader/libs a dynamic build needs:"
    for L in /lib/ld-linux-armhf.so.3 /lib/libc.so.6 /lib/libgcc_s.so.1 \
             /usr/lib/libc.so.6 /usr/lib/libgcc_s.so.1; do
        if [ -e "$L" ]; then echo "  present: $L"; else echo "  MISSING: $L"; fi
    done
else
    echo "  BINARY NOT FOUND at ${BIN:-<unknown>}"
fi
echo

echo "----- Direct execution test -----"
# This is the decisive check. If the binary can run at all, --help prints
# usage and exits 0 without needing QTFB. A crash, 'not found', or a loader
# error here means the binary cannot start on this device — which is exactly
# why no log is ever written and the window stays blank.
if [ -f "$BIN" ]; then
    chmod +x "$BIN" 2>/dev/null
    echo "\$ $BIN --help"
    "$BIN" --help
    echo "exit code: $?"
else
    echo "  skipped: binary not found"
fi
echo

echo "----- Existing logs / markers -----"
LOGDIR=/home/root/.local/share/remarkable-calendar-notes
ls -la "$LOGDIR" 2>/dev/null || echo "  $LOGDIR does not exist"
echo "--- calendar-notes.log (last 40 lines) ---"
tail -n 40 "$LOGDIR/calendar-notes.log" 2>/dev/null || echo "  no calendar-notes.log"
echo "--- /tmp/calendar-notes.log fallback (last 40 lines) ---"
tail -n 40 /tmp/calendar-notes.log 2>/dev/null || echo "  none"
echo "--- process-started marker ---"
cat "$LOGDIR/process-started.txt" 2>/dev/null || echo "  no marker (process may never have started)"
echo

echo "----- Kernel messages (crashes) -----"
dmesg 2>/dev/null | grep -iE 'segfault|trap|undefined instruction|remarkable-cal' | tail -n 20 || \
    echo "  (none, or dmesg unavailable)"
echo

echo "===== end of report ====="
