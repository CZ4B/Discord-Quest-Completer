#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod session_window;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use discord_quest_completer::{
    APP_NAME, database,
    process::{ProcessManager, legacy_ghost_title},
};
use eframe::egui;

#[derive(Default)]
struct Options {
    offline: bool,
    database: Option<PathBuf>,
    ghost: Option<String>,
    diagnose: bool,
    help: bool,
}

impl Options {
    fn parse() -> Result<Self> {
        let mut options = Self::default();
        let mut args = std::env::args_os().skip(1);
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--offline") => options.offline = true,
                Some("--database") => {
                    options.database = Some(
                        args.next()
                            .context("--database requires a JSON file path")?
                            .into(),
                    )
                }
                Some("--ghost") => {
                    options.ghost = Some(
                        args.next()
                            .context("--ghost requires a window title")?
                            .into_string()
                            .map_err(|_| anyhow::anyhow!("window title must be Unicode"))?,
                    )
                }
                Some("--diagnose") => options.diagnose = true,
                // Accepted for compatibility with earlier launcher builds.
                Some("--show-game-window") => {}
                Some("--help" | "-h") => options.help = true,
                _ => bail!("unknown argument: {}", arg.to_string_lossy()),
            }
        }
        Ok(options)
    }
}

fn icon() -> Result<egui::IconData> {
    let image = image::load_from_memory(include_bytes!("../assets/app-icon.ico"))?.into_rgba8();
    let (width, height) = image.dimensions();
    Ok(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

fn run() -> Result<()> {
    let options = Options::parse()?;
    if options.help {
        println!(
            "DISCORD QUEST COMPLETER {}\n\nUsage: DiscordQuestCompleter [--offline] [--database PATH]\n       DiscordQuestCompleter --diagnose [--database PATH]\n\n--offline       Use the bundled database; disable all network requests\n--database PATH Use a local Discord-format game database\n--diagnose      Validate database and embedded assets without opening the UI\n--help          Show this help",
            discord_quest_completer::VERSION
        );
        return Ok(());
    }
    if options.diagnose {
        let games = match &options.database {
            Some(path) => database::from_file(path)?,
            None => database::bundled()?,
        };
        let icon = icon().context("invalid embedded icon")?;
        println!(
            "DISCORD QUEST COMPLETER {}: {} games, {} Windows executables, icon {}x{}; database OK",
            discord_quest_completer::VERSION,
            games.len(),
            games.iter().map(|g| g.executables.len()).sum::<usize>(),
            icon.width,
            icon.height
        );
        return Ok(());
    }
    let ghost = options.ghost.or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|exe| legacy_ghost_title(&exe))
    });
    let title = ghost.as_deref().unwrap_or(APP_NAME);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size(if ghost.is_some() {
            [800.0, 600.0]
        } else {
            [1040.0, 840.0]
        })
        .with_min_inner_size(if ghost.is_some() {
            [320.0, 240.0]
        } else {
            [760.0, 600.0]
        })
        .with_decorations(ghost.is_some());
    if let Ok(icon) = icon() {
        viewport = viewport.with_icon(icon);
    }
    let native_options = eframe::NativeOptions {
        viewport,
        // WGPU uses DirectX on Windows, avoiding an OpenGL requirement on older
        // drivers, Remote Desktop sessions, and virtual machines.
        renderer: eframe::Renderer::Wgpu,
        centered: true,
        ..Default::default()
    };
    let result = if ghost.is_some() {
        let game_title = title.to_owned();
        eframe::run_native(
            title,
            native_options,
            Box::new(move |cc| Ok(Box::new(session_window::SessionWindow::new(cc, game_title)))),
        )
    } else {
        let processes = ProcessManager::new()?;
        eframe::run_native(
            title,
            native_options,
            Box::new(move |cc| {
                Ok(Box::new(app::Launcher::new(
                    cc,
                    processes,
                    options.offline,
                    options.database,
                )))
            }),
        )
    };
    result.map_err(|error| anyhow::anyhow!("cannot start desktop UI: {error}"))
}

fn main() {
    let headless =
        std::env::args_os().any(|arg| matches!(arg.to_str(), Some("--diagnose" | "--help" | "-h")));
    if let Err(error) = run() {
        eprintln!("{APP_NAME}: {error:#}");
        #[cfg(windows)]
        if !headless {
            let message: Vec<u16> = format!("{error:#}").encode_utf16().chain(Some(0)).collect();
            let title: Vec<u16> = APP_NAME.encode_utf16().chain(Some(0)).collect();
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                    std::ptr::null_mut(),
                    message.as_ptr(),
                    title.as_ptr(),
                    windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                        | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
                );
            }
        }
        std::process::exit(1);
    }
}
