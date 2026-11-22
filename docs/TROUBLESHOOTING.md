# Troubleshooting

This guide lists the common problems and their fixes. Find your symptom in
the table, then read the section.

| Symptom | Section |
|---|---|
| Black bars, or the picture is not stretched | [Not stretched](#the-picture-is-not-stretched) |
| A part of the picture is cut off, for example the abilities at the bottom | [Cut-off picture](#a-part-of-the-picture-is-cut-off) |
| The mouse cursor does not hit the buttons in the menus | [Cursor offset](#the-cursor-does-not-match-the-menus) |
| The game opens on the wrong display | [Wrong display](#the-game-opens-on-the-wrong-display) |
| Setup cannot create the custom resolution | [Custom resolution](#setup-cannot-create-the-custom-resolution) |
| The display stays stretched, or a monitor stays disabled | [Stuck display](#the-display-stays-stretched-or-a-monitor-stays-disabled) |
| widepeek cannot find or start the Riot Client | [Riot Client](#widepeek-cannot-find-or-start-the-riot-client) |
| A UAC prompt comes at every start | [UAC prompt](#a-uac-prompt-comes-at-every-start) |
| More input lag, or no G-Sync | [Input lag](#more-input-lag-or-no-g-sync) |
| The update check or the update install fails | [Updates](#the-update-fails) |

Before you start, open the log. Most errors are there with a clear text:

```powershell
notepad "$env:LOCALAPPDATA\widepeek\widepeek.log"
```

---

## Recommended settings

Most problems come from one wrong setting. Compare your settings with this
list first.

**In Valorant** (Settings > Video > General):

| Setting | Value |
|---|---|
| Display Mode | **Windowed Fullscreen** |
| Monitor | The display that you chose in setup (the main display) |
| Resolution | The stretched mode, for example `2088x1440 144Hz` |
| Aspect Ratio Method | **Fill** |

**In the NVIDIA Control Panel** (Display > Adjust desktop size and position),
for the stretched display:

| Setting | Value |
|---|---|
| Scaling mode | **Full-screen** |
| Perform scaling on | **GPU** |
| Override the scaling mode set by games and programs | **On** |

**In Windows** (Settings > System > Display > Graphics):

- **Optimizations for windowed games:** on, if your Windows version has this
  switch.

Why Windowed Fullscreen? widepeek sets the desktop to the stretched mode, so
Windowed Fullscreen renders in the stretched mode too. Exclusive Fullscreen
changes the display mode again and often computes the cursor position wrong
with a custom resolution. On Windows 11, the input lag of both modes is almost
the same (see [Input lag](#more-input-lag-or-no-g-sync)).

---

## The picture is not stretched

You see black bars at the sides, or the picture has the native aspect ratio.

1. **Start the game with the shortcut "Valorant (widepeek)".** If you start
   Valorant from the Riot Client directly, nothing is stretched.
2. **Check the Valorant settings.** Aspect Ratio Method must be **Fill**.
3. **Check the NVIDIA scaling.** Scaling mode must be **Full-screen** on the
   **GPU**, with the override on. Some drivers do not accept the stretch flag
   from Windows. The log then has this line:

   ```text
   stretched scaling is not accepted, using the GPU scaling setting
   ```

   In that case, the NVIDIA setting alone decides the scaling.
4. **Check that the monitors were disabled.** The log must show
   `disable N monitor device(s)` and `N monitor device(s) are now Disabled`.
   If not, see [UAC prompt](#a-uac-prompt-comes-at-every-start).
5. **Tray mode:** the tray stretches only after `VALORANT.exe` started, so the
   game can already read the native resolution. Restart the game once, or use
   the launcher shortcut.

## A part of the picture is cut off

The abilities, the minimap or the chat are partly outside the screen.

- **The game resolution does not match the stretched mode.** Valorant keeps
  its own resolution in its config. If that is, for example, `1920x1080`, but
  the desktop is `2088x1440`, a part of the picture is outside the screen.
  Set the resolution in Valorant to the stretched mode.
- **The game window is on another display.** Valorant also stores the window
  position. If the window was on a second display before, it can open with an
  offset. Set **Monitor** to the main display in Valorant.
- **The NVIDIA scaling is not Full-screen.** See
  [Recommended settings](#recommended-settings).

widepeek never edits `GameUserSettings.ini`. You can check the values there
(read only, the game overwrites the file):

```text
%LocalAppData%\VALORANT\Saved\Config\<account-id>\Windows\GameUserSettings.ini
```

| Key | Meaning |
|---|---|
| `ResolutionSizeX`, `ResolutionSizeY` | Game resolution |
| `LastUserConfirmedResolutionSizeX`, `LastUserConfirmedResolutionSizeY` | Game resolution |
| `LastUserConfirmedDesiredScreenWidth`, `LastUserConfirmedDesiredScreenHeight` | Game resolution |
| `DesiredScreenWidth`, `DesiredScreenHeight` | Game resolution |
| `FullscreenMode` | `0` = Fullscreen, `1` = Windowed Fullscreen, `2` = Windowed |
| `WindowPosX`, `WindowPosY` | Window position on the desktop |

Making this file read-only is not necessary. In Windowed Fullscreen the game
uses the desktop resolution anyway, and a read-only file stops Valorant from
saving any other setting.

## The cursor does not match the menus

In the menus, you click next to a button, or the button reacts at another
position.

1. **Use Windowed Fullscreen.** This is the main fix. Exclusive Fullscreen
   with a custom resolution often maps the cursor wrong. The stretched picture
   stays the same in Windowed Fullscreen.
2. **Close overlays that hook into the game**, for example RivaTuner
   Statistics Server (RTSS). Test without them.
3. **Check the Windows scaling.** Settings > System > Display > Scale should be
   100 % on the stretched display.

## The game opens on the wrong display

- Set **Monitor** in the Valorant video settings to the main display.
- Run `widepeek setup` again and choose the display that you play on.
- widepeek disables **all** monitor devices during a session. If the game
  still opens on another display, that display is probably not a Monitor-class
  device (for example, a virtual display of a streaming tool). Disconnect it
  for a test.

## Setup cannot create the custom resolution

| Message in the console or the log | Fix |
|---|---|
| `the NVIDIA driver rejected ... (NVAPI_ERROR)` | Create the mode once by hand: NVIDIA Control Panel > Change resolution > Customize > Create Custom Resolution. Use the timing **CVT reduced blank**. Then run `widepeek setup` again. |
| `the driver saved ..., but Windows does not list it yet` | Reboot, then run `widepeek setup` again. |
| `the custom resolution was not confirmed, the old mode is back` | You did not click *Yes* within 15 seconds. Run setup again. |
| `... Hz is above the native ... Hz` | widepeek never sets a higher refresh rate than the native one. Use `--hz` with the native rate or lower. |
| `the computed pixel clock ... is above the native ...` | The chosen mode needs more bandwidth than the native mode. Use a smaller width or a lower refresh rate. |
| AMD or Intel GPU | Setup shows the manual steps for the driver panel. Create the mode there, then run `widepeek setup` again. |

If the picture stays black during the 15-second test, do nothing. The old mode
comes back automatically.

## The display stays stretched or a monitor stays disabled

widepeek restores the display when the game ends, when widepeek crashes, at
logoff and at the next logon. If the display is still wrong, run:

```powershell
widepeek restore
```

`restore` is safe to run at any time. It works without a config and without a
state file.

If `widepeek` itself does not start:

1. Open the Device Manager (`devmgmt.msc`).
2. Open **Monitors**. Right-click each disabled monitor, then click
   **Enable device**.
3. Set the resolution in Settings > System > Display.

After a reboot, Windows can start in the stretched mode for about 5 seconds.
Then the logon task `\widepeek\RestoreAtLogon` restores it. That is normal.

## widepeek cannot find or start the Riot Client

| Message | Fix |
|---|---|
| `cannot find RiotClientServices.exe. Set riot_client_path in ...` | Add the full path to `%AppData%\widepeek\config.toml`, for example `riot_client_path = 'D:\Riot Games\Riot Client\RiotClientServices.exe'`. |
| `VALORANT-Win64-Shipping.exe did not start within 300 s` | The Riot Client waited for a login or an update. Start again after the update, or increase `start_timeout_secs` in the config (10 to 3600). |
| `Valorant is already running` | Close Valorant, then start widepeek again. widepeek must change the display before the game starts. |

## A UAC prompt comes at every start

The monitor toggle needs administrator rights. Setup creates the scheduled
tasks `\widepeek\MonitorOff` and `\widepeek\MonitorOn`, so a normal start needs
no prompt. If the tasks are missing or broken, widepeek asks for administrator
rights as a fallback.

1. Open the Task Scheduler (`taskschd.msc`) and check the folder `\widepeek`.
   It must have `MonitorOff`, `MonitorOn` and `RestoreAtLogon`.
2. If a task is missing, run `widepeek setup` again.
3. The administrator prompt during setup must use the **same Windows account**
   that runs the wizard.

## More input lag or no G-Sync

- **Overlays** (RTSS, Discord, Xbox Game Bar) can stop Windows from giving the
  frames directly to the display. Then each frame waits for the desktop
  compositor. Turn the overlays off.
- **G-Sync and FreeSync are off during a session.** The monitors are disabled,
  so Windows does not see their EDID and their variable refresh rate. This is
  a side effect of true stretched and not a bug. ICC profiles and HDR are also
  not available. Everything comes back after the game.

## The update fails

The automatic check writes its errors only to the log. A manual check
(`widepeek update` or *Check for updates*) shows them.

| Error | Fix |
|---|---|
| `widepeek or Valorant is running` | Close Valorant and wait until the display is native. Then update. |
| `widepeek is not installed` | Run `widepeek setup` first. The update replaces `C:\Program Files\widepeek\widepeek.exe`. |
| A network or `HTTP` error | Check the internet connection, a proxy or a firewall. widepeek uses the Windows proxy settings. The GitHub API also has a rate limit per IP address. Try again later. |
| `does not match its SHA-256 file` or `does not match the SHA-256 of the release` | The download is damaged or changed. Try again. If it stays, open an issue. |
| `is not a GitHub download host` or `too many redirects` | A proxy or a captive portal sent widepeek to another server. widepeek does not follow it. Check the network, then try again. |
| `the download took longer than 5 minutes` | The connection is too slow or stalls. Try again later. |
| `the update is signed by ...` or `is not signed` | The new exe has another signer than the installed one. widepeek does not install it. Open an issue. |
| `Windows cannot check if the certificate ... is revoked` | Windows cannot reach the revocation server of the certificate. Check the internet connection, a proxy or a firewall. Then try again. |
| `is not newer than the installed` | The installed widepeek is already this version or newer. Nothing to do. |
| `the update file is not on a local disk` or `is not in ...` | The update folder in `%LocalAppData%` is on a network drive or is a link to one. Use a local profile folder, or install by hand (below). |
| `the elevated update step failed` | You declined the UAC prompt, or the prompt used another Windows account. See the log for details. |

To install by hand, download `widepeek.exe` from the release page and run
`widepeek setup` with it.

## SmartScreen or the antivirus blocks widepeek

The release exe is not signed. SmartScreen and some antivirus programs warn
about unsigned exes, most of the time as a false positive.

1. Compare `(Get-FileHash .\widepeek.exe).Hash` with `widepeek.exe.sha256` of
   the release.
2. Scan the exe on [VirusTotal](https://www.virustotal.com), or search for its
   SHA-256 there.
3. SmartScreen: click *More info* > *Run anyway*. Antivirus: restore the exe
   from quarantine and add an exception for it.

See [Unsigned releases](../README.md#unsigned-releases).

---

## Still stuck?

Open an issue and add:

- your GPU and driver version,
- the native and the stretched resolution,
- the Valorant display settings,
- the last session from `%LocalAppData%\widepeek\widepeek.log`.

Check the log before you post it and remove anything private.
