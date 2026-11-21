# Discord Quest Completer

A Windows desktop application for managing local game activity sessions, built with Rust and egui.

The application reads Discord's game database and creates temporary local processes using the selected game's executable name and folder structure. You can start a session without a time limit, set a timer, or queue several sessions to run one after another. Activity detection and quest progress are handled by Discord.

Developed by [CZ4B](https://github.com/CZ4B).

## Features

- Searchable game library with an embedded database for offline use.
- Game artwork loaded on demand and cached locally.
- Optional session timers, from one second to 24 hours.
- Game windows with artwork and a centered countdown or elapsed-time display.
- A sequential queue with individual durations, editing, and reordering.
- Pause, resume, skip, stop, and extend controls.
- One active game session at a time, including across launcher instances.
- Automatic child-process shutdown when the launcher exits.
- Stable release checks with links to the available version.
- A standalone Windows executable with the database and application icon embedded.

## Getting started

1. Open the [Releases page](https://github.com/CZ4B/Discord-Quest-Completer/releases).
2. Download `DiscordQuestCompleter.exe`, or download and extract the Windows x64 ZIP.
3. Run `DiscordQuestCompleter.exe` on Windows x64.
4. Search for a game and select it from the library.
5. Leave **Run for** empty for an unlimited session, or enter a duration in minutes and seconds.
6. Click **Start** beside an executable to start a session immediately, or use **+ Queue** to add it to the queue.

The release executable runs without a Rust installation. On startup, the launcher loads its embedded game library and attempts to refresh it online. If the refresh fails, the embedded library remains available.

## Sessions and queue

Click **Start queue** to begin processing queued games in order. Each session ends when its timer expires or its game window closes, and the next game starts when the queue is running.

The game window displays its icon, name, and a large timer. Timed sessions show the time remaining, including changes made with **+5 min** in the launcher. Sessions without a time limit show elapsed time. Artwork uses the same local cache as the library; unavailable icons use the application icon instead.

| Control | Behavior |
| --- | --- |
| **Pause queue** | Keeps the current session running and prevents the next game from starting automatically. |
| **Resume queue** | Enables automatic progression again. |
| **Skip** | Stops the current session and starts the next queued game. |
| **Stop** | Stops the current session and pauses the queue, keeping waiting items. |
| **+5 min** | Adds five minutes to a timed session, within the 24-hour limit. |
| Duration menu | Changes a waiting item's duration, removes its time limit, or moves it up in the queue. |
| Remove / **Clear** | Removes an individual waiting item or clears all waiting items. |

A session without a timer stays active until you stop it, skip it, or close its game window. It holds up the rest of the queue while it is running. The queue is kept in memory for the current launcher session; closing the launcher stops the active game and discards waiting items.

The **...** menu contains the application version, developer credit, **Refresh library**, and **Check releases**. Refreshing the library also retries game icons that previously failed to load.

## Build from source

You need Windows x64, Rust 1.88 or newer with the MSVC toolchain, Visual Studio Build Tools with **Desktop development with C++**, and the Windows SDK.

From the project root:

```powershell
cargo build --release --locked
.\target\release\DiscordQuestCompleter.exe
```

The Windows build embeds the application icon and uses the static CRT configuration in [.cargo/config.toml](.cargo/config.toml). The interface uses egui's OpenGL renderer.

To run the project checks:

```powershell
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Tests cover database parsing, executable paths, icon handling, release comparisons, timers, queue operations, and Windows process lifecycle behavior.

## License

Licensed under the [MIT License](LICENSE).
