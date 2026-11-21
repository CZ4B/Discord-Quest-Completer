use std::{
    io::Read,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::database::Game;

pub const SESSION_FILE: &str = "dqc.session.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionState {
    pub game_id: String,
    pub game_name: String,
    pub icon_hash: Option<String>,
    pub started_unix_ms: u64,
    pub duration_secs: Option<u64>,
    pub offline: bool,
}

impl SessionState {
    pub fn new(game: &Game, duration: Option<Duration>, offline: bool) -> Self {
        Self {
            game_id: game.id.clone(),
            game_name: game.name.clone(),
            icon_hash: game.icon_hash.clone(),
            started_unix_ms: unix_ms(SystemTime::now()),
            duration_secs: duration.map(|duration| duration.as_secs()),
            offline,
        }
    }

    pub fn fallback(title: String) -> Self {
        Self {
            game_id: String::new(),
            game_name: title,
            icon_hash: None,
            started_unix_ms: unix_ms(SystemTime::now()),
            duration_secs: None,
            offline: true,
        }
    }

    pub fn elapsed_at(&self, now: SystemTime) -> Duration {
        Duration::from_millis(unix_ms(now).saturating_sub(self.started_unix_ms))
    }

    pub fn remaining(&self, elapsed: Duration) -> Option<Duration> {
        self.duration_secs
            .map(|seconds| Duration::from_secs(seconds).saturating_sub(elapsed))
    }

    pub fn read(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(64 * 1024)
            .read_to_end(&mut bytes)?;
        serde_json::from_slice(&bytes).context("invalid session information")
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let directory = path.parent().context("session path has no parent")?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(&mut file, self)?;
        file.persist(path)
            .context("cannot save session information")?;
        Ok(())
    }
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_and_elapsed_share_the_session_start() {
        let mut state = SessionState::fallback("Game".into());
        state.started_unix_ms = 1_000;
        let elapsed = state.elapsed_at(UNIX_EPOCH + Duration::from_millis(3_500));
        assert_eq!(elapsed, Duration::from_millis(2_500));
        assert_eq!(state.remaining(elapsed), None);
        state.duration_secs = Some(10);
        assert_eq!(state.remaining(elapsed), Some(Duration::from_millis(7_500)));
        assert_eq!(
            state.remaining(Duration::from_secs(11)),
            Some(Duration::ZERO)
        );
        assert_eq!(state.elapsed_at(UNIX_EPOCH), Duration::ZERO);
    }

    #[test]
    fn session_updates_replace_the_file_with_complete_json() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(SESSION_FILE);
        let mut state = SessionState::fallback("Unicode game 日本語".into());
        state.write(&path).unwrap();
        assert_eq!(SessionState::read(&path).unwrap().duration_secs, None);
        state.duration_secs = Some(360);
        state.write(&path).unwrap();
        let loaded = SessionState::read(&path).unwrap();
        assert_eq!(loaded.game_name, state.game_name);
        assert_eq!(loaded.started_unix_ms, state.started_unix_ms);
        assert_eq!(loaded.duration_secs, Some(360));
    }
}
