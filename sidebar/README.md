# Optional xochitl sidebar launcher

`3.27/calendarNotesSidebar.qmd` plus the generated
`calendarNotesSidebar.rcc` add a **Calendar Notes** icon to the
normal reMarkable sidebar and launch the existing AppLoad/QTFB app
directly. It does not replace the Rust app or change its pen latency.

This companion is intentionally pinned to reMarkable OS **3.27.x**.
QMLDiff patches run inside xochitl and can break when its QML structure
changes. After installing or updating it, run:

```sh
xovi/rebuild_hashtable
```

If xochitl fails to load correctly, SSH in, remove
`/home/root/xovi/exthome/qt-resource-rebuilder/calendarNotesSidebar.qmd`,
and `/home/root/xovi/exthome/qt-resource-rebuilder/calendarNotesSidebar.rcc`,
then restart xochitl/XOVI.

The QMD is derived from rm-appload's GPL-3.0-only sidebar patch and is
therefore GPL-3.0-only. The rest of Calendar Notes remains MIT licensed.
