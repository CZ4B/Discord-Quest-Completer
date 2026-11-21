use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use tempfile::TempDir;

use crate::database::{Executable, Game, executable_path};
use crate::session::{SESSION_FILE, SessionState};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProcessKey {
    pub game_id: String,
    pub relative_path: String,
}

impl ProcessKey {
    pub fn new(game: &Game, exe: &Executable) -> Self {
        Self {
            game_id: game.id.clone(),
            relative_path: exe.relative_path.to_lowercase(),
        }
    }
}

pub struct RunningGame {
    pub game_name: String,
    pub executable: String,
    pub started: Instant,
    child: Child,
    directory: TempDir,
    session: SessionState,
    session_path: PathBuf,
    #[cfg(windows)]
    _lease: WindowsGameLease,
}

impl RunningGame {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    fn stop(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none()
            && let Err(error) = self.child.kill()
            && self.child.try_wait()?.is_none()
        {
            return Err(error.into());
        }
        self.child.wait()?;
        Ok(())
    }
}

impl Drop for RunningGame {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub struct ProcessManager {
    running: HashMap<ProcessKey, RunningGame>,
    source: PathBuf,
    offline: bool,
    #[cfg(windows)]
    job: WindowsJob,
}

impl ProcessManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            running: HashMap::new(),
            source: std::env::current_exe().context("cannot locate launcher executable")?,
            offline: false,
            #[cfg(windows)]
            job: WindowsJob::new()?,
        })
    }

    pub fn running(&self) -> &HashMap<ProcessKey, RunningGame> {
        &self.running
    }

    pub fn start(&mut self, game: &Game, executable: &Executable) -> Result<()> {
        self.start_with_duration(game, executable, None)
    }

    pub fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
    }

    pub fn set_duration(&mut self, key: &ProcessKey, duration: Option<Duration>) -> Result<()> {
        let game = self.running.get_mut(key).context("game is not running")?;
        let mut session = game.session.clone();
        session.duration_secs = duration.map(|duration| duration.as_secs());
        session.write(&game.session_path)?;
        game.session = session;
        Ok(())
    }

    pub fn start_with_duration(
        &mut self,
        game: &Game,
        executable: &Executable,
        duration: Option<Duration>,
    ) -> Result<()> {
        if !cfg!(windows) {
            bail!("game processes are supported on Windows only");
        }
        let key = ProcessKey::new(game, executable);
        if !self.running.is_empty() {
            bail!("only one game can run at a time; add another game to the queue");
        }
        #[cfg(windows)]
        let lease = WindowsGameLease::acquire()?;
        let executable = executable_path(&executable.relative_path)?;
        let directory = tempfile::Builder::new()
            .prefix("discord-quest-")
            .tempdir()
            .context("cannot create temporary game directory")?;
        let destination = directory.path().join(&executable.relative_path);
        let parent = destination
            .parent()
            .context("missing executable directory")?;
        fs::create_dir_all(parent).context("cannot create game subdirectories")?;
        fs::copy(&self.source, &destination).context("cannot copy launcher executable")?;
        fs::write(parent.join("dqc.config"), &game.name)
            .context("cannot write game title configuration")?;
        let session = SessionState::new(game, duration, self.offline);
        let session_path = parent.join(SESSION_FILE);
        session.write(&session_path)?;
        let started = Instant::now();
        let mut child = Command::new(&destination)
            .arg("--ghost")
            .arg(&game.name)
            .current_dir(parent)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("cannot start game process")?;
        #[cfg(windows)]
        if let Err(error) = self.job.assign(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.context("cannot attach game process to launcher job"));
        }
        self.running.insert(
            key,
            RunningGame {
                game_name: game.name.clone(),
                executable: executable.relative_path,
                started,
                child,
                directory,
                session,
                session_path,
                #[cfg(windows)]
                _lease: lease,
            },
        );
        Ok(())
    }

    pub fn stop(&mut self, key: &ProcessKey) -> Result<()> {
        if let Some(game) = self.running.get_mut(key) {
            game.stop()?;
        }
        if let Some(game) = self.running.remove(key) {
            cleanup(game)?;
        }
        Ok(())
    }

    pub fn stop_all(&mut self) -> Result<()> {
        let keys: Vec<_> = self.running.keys().cloned().collect();
        let mut errors = Vec::new();
        for key in keys {
            if let Err(error) = self.stop(&key) {
                errors.push(format!("{error:#}"));
            }
        }
        if !errors.is_empty() {
            bail!("{}", errors.join("; "));
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Vec<String> {
        let mut finished = Vec::new();
        let mut errors = Vec::new();
        for (key, game) in &mut self.running {
            match game.child.try_wait() {
                Ok(Some(_)) => finished.push(key.clone()),
                Ok(None) => {}
                Err(error) => errors.push(format!("{}: {error}", game.game_name)),
            }
        }
        for key in finished {
            if let Err(error) = self.stop(&key) {
                errors.push(format!("{error:#}"));
            }
        }
        errors
    }
}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        let _ = self.stop_all();
    }
}

fn cleanup(mut game: RunningGame) -> Result<()> {
    game.stop()?;
    let path = game.directory.path().to_owned();
    let mut last_error = None;
    for _ in 0..5 {
        match fs::remove_dir_all(&path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err(last_error.context("cleanup failed")?.into())
}

pub fn legacy_ghost_title(exe: &Path) -> Option<String> {
    let config = exe.parent()?.join("dqc.config");
    let title = fs::read_to_string(config).ok()?;
    Some(title.lines().next().unwrap_or("Game").to_owned())
}

#[cfg(windows)]
struct WindowsGameLease(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl WindowsGameLease {
    fn acquire() -> Result<Self> {
        use windows_sys::Win32::{
            Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
            System::Threading::CreateMutexW,
        };
        let name: Vec<u16> = "Local\\DiscordQuestCompleter.ActiveGame"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let lease = Self(handle);
        if existed {
            bail!(
                "another launcher already has an active game; stop it before starting this queue"
            );
        }
        Ok(lease)
    }
}

#[cfg(windows)]
impl Drop for WindowsGameLease {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
struct WindowsJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl WindowsJob {
    fn new() -> Result<Self> {
        use windows_sys::Win32::System::JobObjects::*;
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let job = Self(handle);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let success = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if success == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(job)
    }

    fn assign(&self, child: &Child) -> Result<()> {
        use std::os::windows::io::AsRawHandle;
        if unsafe {
            windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                self.0,
                child.as_raw_handle() as _,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_identity_includes_game_and_entire_path() {
        let game = crate::database::parse(r#"[{"id":"1","name":"Game","executables":[{"os":"win32","name":"a/game.exe"},{"os":"win32","name":"b/game.exe"}]}]"#).unwrap().remove(0);
        assert_ne!(
            ProcessKey::new(&game, &game.executables[0]),
            ProcessKey::new(&game, &game.executables[1])
        );
        let mut other = game.clone();
        other.id = "2".into();
        assert_ne!(
            ProcessKey::new(&game, &game.executables[0]),
            ProcessKey::new(&other, &other.executables[0])
        );
    }

    #[test]
    fn reads_first_line_of_legacy_config() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("game.exe");
        assert_eq!(legacy_ghost_title(&exe), None);
        fs::write(dir.path().join("dqc.config"), "Game Title\nignored").unwrap();
        assert_eq!(legacy_ghost_title(&exe), Some("Game Title".into()));
    }
}
