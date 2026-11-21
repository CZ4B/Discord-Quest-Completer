//! Verify process lifecycle and normal visible game windows.
use std::{
    fs,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use discord_quest_completer::{
    database::{self, Game},
    process::{ProcessKey, ProcessManager},
    scheduler::SessionQueue,
    session::{SESSION_FILE, SessionState},
};

fn fixture() -> Result<Game> {
    Ok(database::parse(r#"[{"id":"42","name":"Process Test 日本語 & spaces","executables":[{"os":"win32","name":"bin/a/game.exe"},{"os":"win32","name":"bin/b/game.exe"}]}]"#)?.remove(0))
}

fn main() -> Result<()> {
    if !cfg!(windows) {
        println!("process lifecycle: skipped (requires Windows)");
        return Ok(());
    }
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == "--window-test-only") {
        return game_window_lifecycle();
    }
    if args.get(1).is_some_and(|arg| arg == "--ghost") {
        if args.get(2).is_some_and(|arg| arg == "short-lived") {
            thread::sleep(Duration::from_millis(300));
        } else {
            thread::sleep(Duration::from_secs(60));
        }
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "--job-parent") {
        let mut manager = ProcessManager::new()?;
        let game = fixture()?;
        manager.start(&game, &game.executables[0])?;
        let pid = manager.running().values().next().context("no child")?.pid();
        let image = process_image(pid)?;
        let directory = image.ancestors().nth(3).context("missing session root")?;
        fs::write(
            args.get(2).context("missing PID file")?,
            serde_json::to_vec(&serde_json::json!({"pid": pid, "directory": directory}))?,
        )?;
        std::process::exit(0);
    }
    if args.get(1).is_some_and(|arg| arg == "--probe-busy") {
        let mut manager = ProcessManager::new()?;
        let game = fixture()?;
        ensure!(
            manager.start(&game, &game.executables[0]).is_err(),
            "another launcher bypassed the active-game limit"
        );
        return Ok(());
    }

    let game = fixture()?;
    let mut manager = ProcessManager::new()?;
    manager.set_offline(true);
    let mut child_directories = Vec::new();
    for (index, exe) in game.executables.iter().enumerate() {
        manager.start(&game, exe)?;
        ensure!(manager.running().len() == 1);
        ensure!(
            manager.start(&game, &game.executables[1 - index]).is_err(),
            "second process was accepted"
        );
        let running = manager
            .running()
            .values()
            .next()
            .context("no active process")?;
        let path = process_image(running.pid())?;
        ensure!(
            path.ends_with(std::path::Path::new(&running.executable)),
            "nested executable path changed"
        );
        let parent = path.parent().context("missing child directory")?.to_owned();
        ensure!(
            fs::read_to_string(parent.join("dqc.config"))? == game.name,
            "Unicode title/config changed"
        );
        let session = SessionState::read(&parent.join(SESSION_FILE))?;
        ensure!(session.game_id == game.id && session.game_name == game.name);
        ensure!(session.duration_secs.is_none() && session.offline);
        child_directories.push(parent);
        let probe = Command::new(std::env::current_exe()?)
            .arg("--probe-busy")
            .status()?;
        ensure!(probe.success(), "cross-launcher limit failed");
        manager.stop(&ProcessKey::new(&game, exe))?;
        ensure!(manager.running().is_empty());
    }
    ensure!(
        child_directories.iter().all(|dir| !dir.exists()),
        "temporary files not cleaned"
    );

    let mut short = game.clone();
    short.name = "short-lived".into();
    manager.start(&short, &short.executables[0])?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !manager.running().is_empty() && Instant::now() < deadline {
        ensure!(manager.poll().is_empty(), "poll failed");
        thread::sleep(Duration::from_millis(30));
    }
    ensure!(manager.running().is_empty(), "exited child was not reaped");
    scheduler_lifecycle(&game)?;
    game_window_lifecycle()?;

    let result_dir = tempfile::tempdir()?;
    let pid_file = result_dir.path().join("child.pid");
    let status = Command::new(std::env::current_exe()?)
        .arg("--job-parent")
        .arg(&pid_file)
        .status()?;
    ensure!(status.success(), "job parent failed");
    let result: serde_json::Value = serde_json::from_slice(&fs::read(&pid_file)?)?;
    let pid = result["pid"].as_u64().context("missing child PID")? as u32;
    ensure!(
        !is_alive(pid)?,
        "Windows job left an orphan after abrupt parent exit"
    );
    let directory = std::path::PathBuf::from(
        result["directory"]
            .as_str()
            .context("missing session directory")?,
    )
    .canonicalize()?;
    ensure!(
        directory.starts_with(std::env::temp_dir().canonicalize()?)
            && directory
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("discord-quest-")),
        "test cleanup target is outside its expected temporary session"
    );
    fs::remove_dir_all(directory)?;
    println!(
        "process lifecycle: single active game, sequential paths, Unicode config, stop/cleanup, reaping, and crash containment OK"
    );
    Ok(())
}

#[cfg(windows)]
fn game_window_lifecycle() -> Result<()> {
    use std::{os::windows::process::CommandExt, process::Stdio};
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM},
        System::Threading::CREATE_NO_WINDOW,
        UI::WindowsAndMessaging::{
            EnumWindows, GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, GetWindowTextW,
            GetWindowThreadProcessId, IsIconic, IsWindowVisible, PostMessageW, WM_CLOSE,
            WS_CAPTION, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
        },
    };

    if std::env::var_os("CI").is_some()
        && std::env::var_os("DQC_TEST_INTERACTIVE_WINDOWS").is_none()
    {
        println!("game sessions: visual window test skipped on non-interactive CI desktop");
        return Ok(());
    }

    struct TestChild(std::process::Child);
    impl Drop for TestChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[derive(Debug)]
    struct GameWindow {
        pid: u32,
        handle: HWND,
        normal: bool,
    }

    unsafe extern "system" fn find_window(window: HWND, data: LPARAM) -> i32 {
        let state = unsafe { &mut *(data as *mut GameWindow) };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(window, &mut pid) };
        if pid == state.pid {
            let mut title = [0u16; 128];
            let length = unsafe { GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32) };
            if String::from_utf16_lossy(&title[..length as usize]) == "Game window test" {
                state.handle = window;
                let style = unsafe { GetWindowLongPtrW(window, GWL_STYLE) };
                let extended_style = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) };
                state.normal = unsafe { IsWindowVisible(window) } != 0
                    && unsafe { IsIconic(window) } == 0
                    && style & WS_CAPTION as isize == WS_CAPTION as isize
                    && extended_style
                        & (WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT) as isize
                        == 0;
            }
        }
        1
    }

    let directory = tempfile::tempdir()?;
    let executable = directory.path().join("game-window-test.exe");
    fs::copy(env!("CARGO_BIN_EXE_DiscordQuestCompleter"), &executable)?;
    fs::write(directory.path().join("dqc.config"), "Game window test")?;
    for (explicit_flag, compatibility_flag) in [(true, false), (false, false), (true, true)] {
        let mut command = Command::new(&executable);
        if explicit_flag {
            command.args(["--ghost", "Game window test"]);
        }
        if compatibility_flag {
            command.arg("--show-game-window");
        }
        let mut child = TestChild(
            command
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut game_window = std::ptr::null_mut();
        let mut stable_since = None;
        while Instant::now() < deadline {
            ensure!(child.0.try_wait()?.is_none(), "game session exited early");
            let mut state = GameWindow {
                pid: child.0.id(),
                handle: std::ptr::null_mut(),
                normal: false,
            };
            ensure!(
                unsafe { EnumWindows(Some(find_window), &mut state as *mut GameWindow as LPARAM) }
                    != 0
            );
            if state.normal {
                let since = stable_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(500) {
                    game_window = state.handle;
                    break;
                }
            } else {
                stable_since = None;
            }
            thread::sleep(Duration::from_millis(25));
        }
        ensure!(
            !game_window.is_null(),
            "session did not create a normal visible game window"
        );
        ensure!(unsafe { PostMessageW(game_window, WM_CLOSE, 0, 0) } != 0);
        let deadline = Instant::now() + Duration::from_secs(3);
        while child.0.try_wait()?.is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        ensure!(
            child.0.try_wait()?.is_some(),
            "closing the game window did not stop its process"
        );
    }
    println!(
        "game sessions: normal visible window, title, explicit/legacy startup and close behavior OK"
    );
    Ok(())
}

#[cfg(not(windows))]
fn game_window_lifecycle() -> Result<()> {
    Ok(())
}

fn scheduler_lifecycle(game: &Game) -> Result<()> {
    let mut queue = SessionQueue::new(ProcessManager::new()?);
    let a = queue.enqueue(game, &game.executables[0], Duration::from_secs(1))?;
    let b = queue.enqueue(game, &game.executables[1], Duration::from_secs(1))?;
    queue.run()?;
    let mut observed = Vec::new();
    let mut previous_pid = None;
    let deadline = Instant::now() + Duration::from_secs(7);
    while (queue.active().is_some() || !queue.pending().is_empty()) && Instant::now() < deadline {
        ensure!(queue.processes().running().len() <= 1);
        if let Some(active) = queue.active()
            && observed.last() != Some(&active.item.id)
        {
            observed.push(active.item.id);
            if let Some(pid) = previous_pid {
                ensure!(
                    !is_alive(pid)?,
                    "next game started before the previous process exited"
                );
            }
            previous_pid = queue
                .processes()
                .running()
                .values()
                .next()
                .map(|game| game.pid());
        }
        queue.tick()?;
        thread::sleep(Duration::from_millis(25));
    }
    ensure!(
        observed == vec![a, b],
        "FIFO timer advancement failed: {observed:?}"
    );
    ensure!(queue.is_idle() && !queue.is_advancing());
    if let Some(pid) = previous_pid {
        ensure!(!is_alive(pid)?);
    }

    queue.enqueue(game, &game.executables[0], Duration::from_secs(1))?;
    let next = queue.enqueue(game, &game.executables[1], Duration::from_secs(10))?;
    queue.run()?;
    queue.pause();
    let deadline = Instant::now() + Duration::from_secs(4);
    while queue.active().is_some() && Instant::now() < deadline {
        queue.tick()?;
        thread::sleep(Duration::from_millis(25));
    }
    ensure!(
        queue.active().is_none() && queue.pending().len() == 1,
        "pause did not preserve next item"
    );
    queue.run()?;
    ensure!(queue.active().context("resume did not start")?.item.id == next);
    queue.extend_active(Duration::from_secs(300))?;
    let pid = queue
        .processes()
        .running()
        .values()
        .next()
        .context("missing process")?
        .pid();
    let image = process_image(pid)?;
    let session_path = image
        .parent()
        .context("missing session directory")?
        .join(SESSION_FILE);
    let session = SessionState::read(&session_path)?;
    ensure!(
        session.duration_secs == Some(310),
        "game window timer was not extended"
    );
    ensure!(
        queue
            .active()
            .context("missing active")?
            .item
            .duration
            .context("expected a timed session")?
            .as_secs()
            == 310
    );
    queue.enqueue(game, &game.executables[0], Duration::from_secs(10))?;
    queue.stop()?;
    queue.tick()?;
    ensure!(
        queue.active().is_none() && queue.pending().len() == 1,
        "Stop unexpectedly advanced the queue"
    );
    queue.run()?;
    queue.enqueue(game, &game.executables[1], Duration::from_secs(10))?;
    queue.skip()?;
    ensure!(
        queue
            .active()
            .context("skip did not advance")?
            .item
            .executable
            == game.executables[1]
    );
    queue.stop()?;

    let manual = queue.enqueue(game, &game.executables[0], None)?;
    let timed = queue.enqueue(game, &game.executables[1], Duration::from_secs(1))?;
    queue.run()?;
    let deadline = Instant::now() + Duration::from_millis(1200);
    while Instant::now() < deadline {
        queue.tick()?;
        let active = queue
            .active()
            .context("manual session stopped unexpectedly")?;
        ensure!(
            active.item.id == manual && active.remaining().is_none() && active.progress().is_none()
        );
        ensure!(queue.pending().len() == 1 && queue.processes().running().len() == 1);
        thread::sleep(Duration::from_millis(25));
    }
    ensure!(
        queue.extend_active(Duration::from_secs(300)).is_err(),
        "extending a manual session silently enabled its timer"
    );
    queue.stop()?;
    queue.tick()?;
    ensure!(
        queue.active().is_none() && queue.pending().front().is_some_and(|item| item.id == timed)
    );
    queue.run()?;
    let deadline = Instant::now() + Duration::from_secs(4);
    while queue.active().is_some() && Instant::now() < deadline {
        queue.tick()?;
        thread::sleep(Duration::from_millis(25));
    }
    ensure!(
        queue.is_idle(),
        "timed successor did not stop automatically"
    );

    let mut short = game.clone();
    short.name = "short-lived".into();
    queue.enqueue(&short, &short.executables[0], Duration::from_secs(10))?;
    let next = queue.enqueue(game, &game.executables[1], Duration::from_secs(10))?;
    queue.run()?;
    let deadline = Instant::now() + Duration::from_secs(4);
    while queue
        .active()
        .is_some_and(|session| session.item.game.name == "short-lived")
        && Instant::now() < deadline
    {
        queue.tick()?;
        thread::sleep(Duration::from_millis(25));
    }
    ensure!(
        queue
            .active()
            .context("early exit did not advance")?
            .item
            .id
            == next
    );
    queue.stop()?;

    let mut broken = game.executables[0].clone();
    broken.relative_path = "../outside.exe".into();
    let failed = queue.enqueue(game, &broken, Duration::from_secs(10))?;
    ensure!(queue.run().is_err());
    ensure!(
        !queue.is_advancing()
            && queue.active().is_none()
            && queue
                .pending()
                .front()
                .is_some_and(|item| item.id == failed),
        "failed launch lost its queue item"
    );
    queue.clear();
    println!(
        "scheduler: FIFO timers, single active PID, pause/resume, extend, stop, skip, early exit and failure recovery OK"
    );
    Ok(())
}

#[cfg(windows)]
fn process_image(pid: u32) -> Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{Foundation::CloseHandle, System::Threading::*};
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        ensure!(
            !handle.is_null(),
            "OpenProcess failed: {}",
            std::io::Error::last_os_error()
        );
        let mut buffer = vec![0u16; 32768];
        let mut size = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size);
        let error = std::io::Error::last_os_error();
        CloseHandle(handle);
        ensure!(ok != 0, "QueryFullProcessImageNameW: {error}");
        Ok(std::ffi::OsString::from_wide(&buffer[..size as usize]).into())
    }
}

#[cfg(windows)]
fn is_alive(pid: u32) -> Result<bool> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_TIMEOUT},
        System::Threading::*,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return Ok(false);
        }
        let result = WaitForSingleObject(handle, 2000);
        CloseHandle(handle);
        Ok(result == WAIT_TIMEOUT)
    }
}

#[cfg(not(windows))]
fn process_image(_: u32) -> Result<std::path::PathBuf> {
    anyhow::bail!("Windows only")
}
#[cfg(not(windows))]
fn is_alive(_: u32) -> Result<bool> {
    Ok(false)
}
