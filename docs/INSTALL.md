# Install

These steps are for a **reMarkable 2 running OS 3.26 or 3.27**. The app
has not yet been exercised on the author's physical tablet, so treat the
first install as experimental.

## 1. Connect to the tablet

1. On the tablet, open **Settings → Help → Copyrights and licenses** and
   find the SSH password under **General information**.
2. Connect the tablet to the computer with USB.
3. In PowerShell or a terminal, connect and accept the host-key prompt:

   ```sh
   ssh root@10.11.99.1
   ```

   Enter the password shown on the tablet. If the USB address does not
   respond, use the tablet's Wi-Fi IP address instead.

## 2. Install Vellum, XOVI, and AppLoad

Run these commands inside the SSH session. The bootstrap checksum below
is the one published by `vellum-dev/vellum-cli` when these instructions
were written; compare it with the current
[official Vellum instructions](https://github.com/vellum-dev/vellum-cli#installation)
before running it.

```sh
wget --no-check-certificate -O bootstrap.sh https://github.com/vellum-dev/vellum-cli/releases/latest/download/bootstrap.sh
echo "7b0deebc81b28a7d74d95c85e99a4a0a0f6ecaa5b9edb6b858ac61405978ebb9  bootstrap.sh" | sha256sum -c
bash bootstrap.sh

vellum update
vellum add appload tripletap
xovi/rebuild_hashtable
```

`appload` pulls in XOVI and the required Qt resource extension.
`tripletap` lets you start XOVI without reconnecting over SSH after every
reboot.

## 3. Download and verify Calendar Notes

On the computer—not inside the SSH session—download these two files from
the latest public
[release](https://github.com/astafan8/remarkable-calendar-notes-releases/releases):

- `remarkable-calendar-notes-<version>-armv7.zip`
- `remarkable-calendar-notes-<version>-armv7.zip.sha256`

On Windows, verify the downloaded ZIP:

```powershell
(Get-FileHash .\remarkable-calendar-notes-<version>-armv7.zip -Algorithm SHA256).Hash.ToLower()
Get-Content .\remarkable-calendar-notes-<version>-armv7.zip.sha256
```

The two hashes must match. Then extract the ZIP. It contains one
`remarkable-calendar-notes` folder with the binary, `icon.png`, and
`external.manifest.json`.

## 4. Copy the app to AppLoad

From the directory containing the extracted folder, run:

```sh
scp -r remarkable-calendar-notes root@10.11.99.1:/home/root/xovi/exthome/appload/
```

Use the Wi-Fi IP instead of `10.11.99.1` if that is how SSH worked in
step 1.

## 5. Start and try the app

1. Triple-press the tablet's power button to start XOVI. The normal
   reMarkable interface restarts with an **AppLoad** item in its sidebar.
2. Open **AppLoad**, tap **Reload**, then tap **Calendar Notes**.
3. Tap **SET** in Calendar Notes to configure the UTC offset and a
   calendar source.
4. For the quickest test, choose **+ ICS URL** and enter an HTTPS `.ics`
   subscription URL. Tap AppLoad's keyboard button in the window chrome
   whenever a text field needs input.
5. Return to the calendar and write in a day cell with the Marker. Change
   between Day, Week, Work Week, Two Weeks, and Month to confirm that the
   same date's note follows it.

Google and iCloud setup need additional provider-specific details; see
[SOURCES.md](SOURCES.md).

## Troubleshooting

- **No AppLoad sidebar item:** XOVI is not running. Triple-press power or
  SSH in and run `xovi/start`.
- **Calendar Notes is missing in AppLoad:** tap **Reload** and verify that
  `/home/root/xovi/exthome/appload/remarkable-calendar-notes/` contains
  the binary and `external.manifest.json`.
- **Nothing happens after an OS update:** reconnect over SSH, run
  `vellum reenable`, `vellum upgrade`, and `xovi/rebuild_hashtable`.
- **Firmware compatibility error:** this build supports OS 3.26-3.27
  only. Do not force-install it on another version.
- **Need logs:** over SSH, run `journalctl -u xochitl -f`, then launch the
  app again.

## Vellum package status

The app itself is not yet available through `vellum add`. The source is
public, but Vellum's maintainers require the repository owner to review,
clean up, and personally submit the package contribution. Until that PR
is accepted and published to the testing feed, use the manual
release-bundle installation above.
