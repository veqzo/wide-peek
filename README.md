# widepeek

widepeek starts Valorant in **true stretched** resolution and always restores
your display afterwards. It is one small `widepeek.exe` for Windows, without a
runtime, a service or a GUI framework.

## What is true stretched?

Valorant normally shows a 4:3 or 5:4 resolution with black bars or a
letterbox, even when the GPU scaling is set to "Full-screen". The game asks the
monitor (through its EDID) which modes it really supports and keeps the native
aspect ratio.

True stretched works like this:

1. A custom resolution with a narrower width (for example 1568 x 1080 on a
   1920 x 1080 monitor) exists in the graphics driver.
2. The monitor device is disabled in the Device Manager. Windows then uses the
   display without the monitor's EDID data, so the game accepts the custom
   mode as a "real" desktop resolution.
3. The desktop is set to the custom mode before the game starts. The GPU
   stretches the picture to the full panel.

widepeek does steps 2 and 3 for each game session and undoes them when the
game ends. Setup does step 1 once.

## Recommended resolution

widepeek suggests **width = native height x 1.45**, rounded to a multiple of 8.
The height and the refresh rate stay native.

| Native        | Suggested stretch |
|---------------|-------------------|
| 1920 x 1080   | 1568 x 1080       |
| 2560 x 1440   | 2088 x 1440       |
| 3440 x 1440   | 2088 x 1440       |

You can set another width with `widepeek setup --width <px>`. The refresh rate
can never be higher than the native refresh rate.

## Setup in 3 steps

1. Download `widepeek.exe` from the [releases](../../releases) and run
   `widepeek setup`. The wizard runs unelevated.
   Setup:
   - asks which monitor to use and stores its device ID,
   - creates the custom resolution (NVIDIA, automatic; see below),
   - copies the exe to `C:\Program Files\widepeek\` and creates the scheduled
     tasks `\widepeek\MonitorOff`, `\widepeek\MonitorOn` and
     `\widepeek\RestoreAtLogon`. Only this step needs administrator rights,
     so Windows shows one UAC prompt for it,
   - creates the shortcut **Valorant (widepeek)** on the desktop and in the
     Start menu.
2. In Valorant, set once: **Display Mode: Fullscreen**,
   **Resolution: the stretched mode**, **Aspect Ratio Method: Fill**.
   widepeek never edits `GameUserSettings.ini`.
3. Start the game with the **Valorant (widepeek)** shortcut.

### Custom resolution

- **NVIDIA:** setup uses NVAPI. It tries the new mode with standard CVT-RB
  timings, shows a dialog, and saves the mode only when you click *Yes*. If you
  do not answer within 15 seconds, the old mode comes back. Setup rejects
  timings with a higher refresh rate or a higher pixel clock than the native
  mode.
- **AMD / Intel:** setup shows the manual steps for the driver panel and then
  only checks that Windows lists the mode. Run `widepeek setup` again after you
  created it.

## Commands

| Command               | What it does                                                        |
|-----------------------|---------------------------------------------------------------------|
| `widepeek` / `launch` | Stretch, start Valorant, wait for the game to end, restore (default) |
| `widepeek tray`       | Stay in the tray, stretch when `VALORANT.exe` starts (optional)     |
| `widepeek setup`      | One-time setup; options `--width`, `--height`, `--hz`, `--display`, `--yes` |
| `widepeek restore`    | Restore the native mode and enable the monitor. Safe to run any time |
| `widepeek uninstall`  | Restore, then remove tasks, shortcuts, autostart, files and config  |

`monitor-off` and `monitor-on` are internal. The scheduled tasks call them.

## How the launcher works

1. If the last session did not end cleanly, restore it first.
2. Write a state file with the native mode.
3. Disable the monitor (scheduled task with highest privileges).
4. Test the stretched mode with `CDS_TEST`, then apply it to the registry.
5. Start the Riot Client with `--launch-product=valorant --launch-patchline=live`.
6. Wait for `VALORANT-Win64-Shipping.exe` (process list every 1 s, timeout 5 min).
7. Wait for the game to end: check the process list every 5 s.
8. Restore the native mode, enable the monitor, delete the state file.

widepeek and the Riot Client run **unelevated**. Only the monitor toggle runs
elevated, through the scheduled tasks.

### Anti-cheat (Vanguard)

widepeek never opens a handle to the game process. It never reads, writes or
injects into the game and sends no input to it. It only reads the system
process list (Toolhelp), like the Task Manager does. Everything else is the
normal Windows display API and the Device Manager API, and widepeek uses them
before the game starts and after it ended.

## Restore guarantees

Your PC must never stay in the stretched mode or with a disabled monitor.

| Event                                     | Restore path                                       |
|-------------------------------------------|----------------------------------------------------|
| Game ends normally                        | Launcher restores within about 5 s                 |
| Error or panic in widepeek                | Drop guard restores during unwind                  |
| Console window closed, logoff, shutdown   | Console handler / `WM_ENDSESSION` restores         |
| widepeek is killed hard, PC crashes, power loss | State file stays; `RestoreAtLogon` restores at the next logon, and the next widepeek start restores first |
| Anything else                             | Run `widepeek restore` or the tray item *Restore now* |

The display mode is stored in the registry, so after a reboot Windows also
starts in the stretched mode until `RestoreAtLogon` runs (about 5 s after
logon). `restore` works without a config and without a state file: then it
enables all disabled monitor devices and sets each display to its best mode
(highest resolution, then highest refresh rate).

Only one session can run at a time (named mutexes).

## Security

- The elevated steps (`install-system`, `uninstall-system`) only touch
  `C:\Program Files\widepeek` and the `\widepeek` task folder. They never
  write the config, the state file or the log in your user profile.
- Setup copies the running image, not the file at its path, so a file that
  another process puts at that path during setup is not installed.
- The administrator prompt must use the same Windows account as the wizard.
- The exe links the C runtime statically and loads DLLs only from `System32`,
  so a planted DLL next to the download is not loaded.

## Side effects while the monitor is disabled

While the game runs, Windows sees the display without its EDID. This has
side effects:

- **G-Sync / FreeSync** are off.
- **ICC color profiles** of the monitor do not apply.
- **HDR** is not available.
- The monitor name in Windows changes to a generic name.

All of this comes back when widepeek restores.

## Tray mode

`widepeek tray` stays in the notification area. It checks the process list
every 2 s and stretches when `VALORANT.exe` starts. The menu has a status
line, *Restore now* and *Quit*. Setup can add it to the autostart (unelevated,
`HKCU\...\Run`).

**The launcher is the recommended way.** The tray stretches only after
`VALORANT.exe` started, so the game can already read the native resolution.
Then you must restart the game or change the resolution in the game once.

## Measured overhead

Release build, Windows 11, 12 logical cores, about 210 processes.

| Item                        | Result                                                     |
|-----------------------------|------------------------------------------------------------|
| One process-list lookup (Toolhelp) | 2.2 ms CPU                                          |
| Tray, idle, 120 s           | 0.01–0.02 % of total CPU (0.17–0.25 % of one core)         |
| Tray memory                 | 1.5 MB private, 0.6 MB working set (10 MB before the trim) |
| Launcher                    | Not measured live (needs a real display change). Before the game starts it does one Toolhelp lookup per second (about 0.2 % of one core); while the game runs, one every 5 s (about 0.04 % of one core). Its memory is the same as the tray's or lower. |

The CPU time of Windows has a granularity of 15.6 ms, so the tray numbers are
noisy.

**Why Toolhelp and not WMI:** `Win32_ProcessStartTrace` needs administrator
rights, and the tray runs unelevated. `__InstanceCreationEvent WITHIN 2`
polls the process list inside the WMI service, so it does the same work in
another process and adds the WMI service as a dependency. A measurement of the
WMI service with and without such a subscription showed no difference above
the noise. Toolhelp gives the same result with fewer parts.

## Files

| Path                                       | Content                    |
|--------------------------------------------|----------------------------|
| `C:\Program Files\widepeek\widepeek.exe`   | Installed exe              |
| `%AppData%\widepeek\config.toml`           | Configuration              |
| `%LocalAppData%\widepeek\widepeek.log`     | Log (rotates at about 1 MB) |
| `%LocalAppData%\widepeek\state.json`       | Only while a session runs  |

The config has a `riot_client_path` option. If it is set and the file exists,
widepeek uses it. Otherwise it reads `RiotClientInstalls.json`, then it uses
the default install path.

## Known limitations

- **cmd does not wait for GUI apps.** `widepeek.exe` is a GUI subsystem
  program. If you start it from `cmd`, the prompt comes back at once, while
  widepeek still runs and writes to the console. Use
  `start /wait widepeek launch` in cmd. PowerShell users can use
  `Start-Process widepeek -ArgumentList launch -Wait`.
- Only Valorant. Only NVIDIA for the automatic custom resolution.
- One monitor is stretched. On multi-monitor systems, choose the game monitor
  in setup.

## Build

```powershell
cargo build --release
```

The exe is at `target\release\widepeek.exe`. CI builds, lints and tests every
push and attaches the exe to tagged releases.

## Disclaimer

widepeek is not made by, endorsed by or associated with Riot Games. Valorant
and Riot Client are trademarks of Riot Games, Inc. widepeek changes display
settings and disables a device in the Device Manager. Use it at your own risk.
The authors are not responsible for bans, display problems or other damage.
Riot can change what is allowed at any time.

## License

MIT, see [LICENSE](LICENSE).
