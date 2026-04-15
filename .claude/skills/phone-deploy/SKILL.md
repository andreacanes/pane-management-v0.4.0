---
name: phone-deploy
description: >
  Connect to Andrea's OnePlus 13 via ADB over Tailscale and push APK updates.
  Use when the user says "deploy to phone", "push APK", "install on phone",
  "connect to phone", "adb install", or after any Android build that should
  land on the device. Also covers ADB troubleshooting.
allowed-tools:
  - Bash
  - Read
---

# Phone deployment

The target device is a OnePlus 13 (oneplus-13) connected **wirelessly via Tailscale**, not USB.
ADB runs from the Windows Android SDK; WSL calls it via its full path.

## ADB path

```
ADB=/mnt/c/Users/Andrea/AppData/Local/Android/Sdk/platform-tools/adb.exe
```

Always assign this at the top of any bash block that uses ADB.

## Tailscale IPs

| Device | Tailscale IP | Role |
|---|---|---|
| Windows desktop | 100.110.47.29 | Runs companion on :8833 |
| OnePlus 13 | 100.83.163.105 | ADB target, runs APK |

## Connect to phone

ADB connects over Tailscale on port 5555 (wireless debugging). **Always connect before any ADB operation** — the connection drops when the phone sleeps or Tailscale reconnects.

```bash
ADB=/mnt/c/Users/Andrea/AppData/Local/Android/Sdk/platform-tools/adb.exe
$ADB connect 100.83.163.105:5555
```

Verify with:
```bash
$ADB devices -l
```

Expected:
```
100.83.163.105:5555    device product:CPH2655 model:CPH2655 ...
```

If connection fails:
1. Check Tailscale is running: `cmd.exe /c "tailscale status"`
2. Check wireless debugging is enabled on the phone (Developer Options → Wireless debugging)
3. `$ADB kill-server && $ADB start-server` then retry connect

## Install APK

The pane-management-mobile APK lives at a fixed path after `gradlew assembleDebug`:

```bash
ADB=/mnt/c/Users/Andrea/AppData/Local/Android/Sdk/platform-tools/adb.exe
$ADB connect 100.83.163.105:5555 2>/dev/null
$ADB install -r 'C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\pane-management-mobile\app\build\outputs\apk\debug\app-debug.apk'
```

`-r` replaces the existing install (keeps data). If install fails with `INSTALL_FAILED_UPDATE_INCOMPATIBLE`, the signing key changed — uninstall first:

```bash
$ADB uninstall com.andreacanes.panemgmt
```

Then re-install without `-r`.

## Launch after install

```bash
$ADB shell am start -n com.andreacanes.panemgmt/.MainActivity
```

## Full deploy (connect + install + launch)

```bash
ADB=/mnt/c/Users/Andrea/AppData/Local/Android/Sdk/platform-tools/adb.exe
$ADB connect 100.83.163.105:5555 2>/dev/null && \
$ADB install -r 'C:\Users\Andrea\Desktop\Botting\pane-management-v0.4.0\pane-management-mobile\app\build\outputs\apk\debug\app-debug.apk' && \
$ADB shell am start -n com.andreacanes.panemgmt/.MainActivity
```

## Logcat (filtered by app)

SurfaceFlinger and system spam is overwhelming. Always filter by app PID:

```bash
ADB=/mnt/c/Users/Andrea/AppData/Local/Android/Sdk/platform-tools/adb.exe
$ADB logcat --pid=$($ADB shell pidof com.andreacanes.panemgmt)
```

## Common failures

| Symptom | Fix |
|---|---|
| `cannot connect to 100.83.163.105:5555` | Phone sleeping, Tailscale down, or wireless debugging off |
| `error: device not found` | Run `$ADB connect 100.83.163.105:5555` first |
| `error: device offline` | `$ADB disconnect 100.83.163.105:5555` then reconnect |
| `error: device unauthorized` | Tap "Allow wireless debugging" on the phone |
| `INSTALL_FAILED_UPDATE_INCOMPATIBLE` | `$ADB uninstall <pkg>` then reinstall |
| `INSTALL_FAILED_INSUFFICIENT_STORAGE` | Free space on phone |
| App crashes on launch | Check logcat filtered by PID for the stack trace |
| `run-as` network calls fail | Known Android restriction — test via the app itself, not `run-as` |

## For other APKs

The paths above are for pane-management-mobile. For any other project's APK, substitute:
- The APK path after `install -r`
- The package name / activity in `am start -n`
- The package name in `pidof` for logcat
