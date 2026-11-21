//! Single active game with per-item durations and a FIFO queue.
use crate::{
    database::{Executable, Game},
    process::{ProcessKey, ProcessManager},
};
use anyhow::{Result, bail};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub const MAX_DURATION: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone)]
pub struct QueuedGame {
    pub id: u64,
    pub game: Game,
    pub executable: Executable,
    pub duration: Option<Duration>,
}

pub struct ActiveSession {
    pub item: QueuedGame,
    pub started: Instant,
}

impl ActiveSession {
    pub fn remaining(&self) -> Option<Duration> {
        self.item
            .duration
            .map(|duration| duration.saturating_sub(self.started.elapsed()))
    }
    pub fn progress(&self) -> Option<f32> {
        self.item.duration.map(|duration| {
            (self.started.elapsed().as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
        })
    }
}

pub struct SessionQueue {
    processes: ProcessManager,
    pending: VecDeque<QueuedGame>,
    active: Option<ActiveSession>,
    auto_advance: bool,
    next_id: u64,
    status: String,
}

impl SessionQueue {
    pub fn new(processes: ProcessManager) -> Self {
        Self {
            processes,
            pending: VecDeque::new(),
            active: None,
            auto_advance: false,
            next_id: 1,
            status: "Ready to build your queue".into(),
        }
    }
    pub fn processes(&self) -> &ProcessManager {
        &self.processes
    }
    pub fn active(&self) -> Option<&ActiveSession> {
        self.active.as_ref()
    }
    pub fn pending(&self) -> &VecDeque<QueuedGame> {
        &self.pending
    }
    pub fn is_advancing(&self) -> bool {
        self.auto_advance
    }
    pub fn status(&self) -> &str {
        &self.status
    }
    pub fn is_idle(&self) -> bool {
        self.active.is_none() && self.pending.is_empty() && self.processes.running().is_empty()
    }

    pub fn enqueue(
        &mut self,
        game: &Game,
        executable: &Executable,
        duration: impl Into<Option<Duration>>,
    ) -> Result<u64> {
        let duration = duration.into();
        if let Some(duration) = duration {
            validate_duration(duration)?;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.pending.push_back(QueuedGame {
            id,
            game: game.clone(),
            executable: executable.clone(),
            duration,
        });
        if self.active.is_none() && !self.auto_advance {
            self.status = "Queue ready — press Start queue".into();
        }
        Ok(id)
    }

    pub fn start_now(
        &mut self,
        game: &Game,
        executable: &Executable,
        duration: impl Into<Option<Duration>>,
    ) -> Result<()> {
        if !self.is_idle() {
            bail!("add this game to the queue; only one game can run at a time");
        }
        self.enqueue(game, executable, duration)?;
        self.run()
    }

    pub fn run(&mut self) -> Result<()> {
        self.auto_advance = true;
        if self.active.is_none() {
            self.launch_next()?;
        } else {
            self.status = "Queue running".into();
        }
        Ok(())
    }

    pub fn pause(&mut self) {
        self.auto_advance = false;
        self.status = if self.active.is_some() {
            "Queue paused — current game keeps running"
        } else {
            "Queue paused"
        }
        .into();
    }

    pub fn stop(&mut self) -> Result<()> {
        self.auto_advance = false;
        if let Some(active) = &self.active {
            self.processes
                .stop(&ProcessKey::new(&active.item.game, &active.item.executable))?;
        }
        self.active = None;
        self.status = "Stopped — queued games are kept".into();
        Ok(())
    }

    pub fn skip(&mut self) -> Result<()> {
        self.stop()?;
        self.run()
    }

    pub fn extend_active(&mut self, extra: Duration) -> Result<()> {
        if let Some(active) = &mut self.active {
            let Some(current) = active.item.duration else {
                bail!("this session has no timer");
            };
            let duration = current.saturating_add(extra);
            validate_duration(duration)?;
            self.processes.set_duration(
                &ProcessKey::new(&active.item.game, &active.item.executable),
                Some(duration),
            )?;
            active.item.duration = Some(duration);
        }
        Ok(())
    }

    pub fn remove(&mut self, id: u64) {
        self.pending.retain(|item| item.id != id);
    }
    pub fn clear(&mut self) {
        self.pending.clear();
    }
    pub fn move_up(&mut self, id: u64) {
        if let Some(index) = self.pending.iter().position(|item| item.id == id)
            && index > 0
        {
            self.pending.swap(index - 1, index);
        }
    }
    pub fn set_duration(&mut self, id: u64, duration: impl Into<Option<Duration>>) -> Result<()> {
        let duration = duration.into();
        if let Some(duration) = duration {
            validate_duration(duration)?;
        }
        if let Some(item) = self.pending.iter_mut().find(|item| item.id == id) {
            item.duration = duration;
        }
        Ok(())
    }

    pub fn tick(&mut self) -> Result<()> {
        let errors = self.processes.poll();
        if !errors.is_empty() {
            self.auto_advance = false;
            bail!("{}", errors.join("\n"));
        }
        if let Some(active) = &self.active {
            let key = ProcessKey::new(&active.item.game, &active.item.executable);
            if !self.processes.running().contains_key(&key) {
                self.active = None;
                self.status = "Game window closed".into();
            } else if active
                .remaining()
                .is_some_and(|remaining| remaining.is_zero())
            {
                if let Err(error) = self.processes.stop(&key) {
                    self.auto_advance = false;
                    return Err(error);
                }
                self.active = None;
                self.status = "Session completed".into();
            }
        }
        if self.active.is_none() && self.auto_advance {
            self.launch_next()?;
        }
        Ok(())
    }

    fn launch_next(&mut self) -> Result<()> {
        let Some(item) = self.pending.front() else {
            self.auto_advance = false;
            self.status = "Queue completed".into();
            return Ok(());
        };
        if let Err(error) =
            self.processes
                .start_with_duration(&item.game, &item.executable, item.duration)
        {
            self.auto_advance = false;
            self.status = format!("Could not start {} — queue paused", item.game.name);
            return Err(error);
        }
        let item = self.pending.pop_front().expect("front was present");
        let started = self
            .processes
            .running()
            .values()
            .next()
            .map(|game| game.started)
            .unwrap_or_else(Instant::now);
        self.active = Some(ActiveSession { item, started });
        self.status = "Queue running".into();
        Ok(())
    }
}

pub fn validate_duration(duration: Duration) -> Result<()> {
    if duration < Duration::from_secs(1) || duration > MAX_DURATION {
        bail!("choose a duration between 1 second and 24 hours");
    }
    Ok(())
}

pub fn parse_duration(minutes: &str, seconds: &str) -> Result<Option<Duration>> {
    let parse = |value: &str| -> Result<u64> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(0);
        }
        if !value.bytes().all(|byte| byte.is_ascii_digit()) {
            bail!("use whole numbers for minutes and seconds");
        }
        Ok(value.parse()?)
    };
    let minutes = parse(minutes)?;
    let seconds = parse(seconds)?;
    if minutes > 1440 || seconds > 59 {
        bail!("use up to 1440 minutes and 0–59 seconds");
    }
    let duration = Duration::from_secs(minutes * 60 + seconds);
    if duration.is_zero() {
        return Ok(None);
    }
    validate_duration(duration)?;
    Ok(Some(duration))
}

pub fn format_limit(duration: Option<Duration>) -> String {
    duration
        .map(format_duration)
        .unwrap_or_else(|| "No limit".into())
}

pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs() + u64::from(duration.subsec_nanos() > 0);
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_bounded_and_countdown_rounds_up() {
        assert!(validate_duration(Duration::ZERO).is_err());
        assert!(validate_duration(Duration::from_millis(999)).is_err());
        assert!(validate_duration(Duration::from_secs(1)).is_ok());
        assert!(validate_duration(MAX_DURATION).is_ok());
        assert!(validate_duration(MAX_DURATION + Duration::from_secs(1)).is_err());
        assert_eq!(format_duration(Duration::from_millis(1)), "00:01");
        assert_eq!(format_duration(Duration::from_secs(900)), "15:00");
        assert_eq!(format_duration(Duration::from_secs(3661)), "1:01:01");
    }

    #[test]
    fn queue_reordering_edits_and_removal_keep_per_item_durations() {
        let game = crate::database::parse(
            r#"[{"id":"1","name":"Test","executables":[{"os":"win32","name":"test.exe"}]}]"#,
        )
        .unwrap()
        .remove(0);
        let mut queue = SessionQueue::new(ProcessManager::new().unwrap());
        let a = queue
            .enqueue(&game, &game.executables[0], Duration::from_secs(60))
            .unwrap();
        let b = queue
            .enqueue(&game, &game.executables[0], Duration::from_secs(120))
            .unwrap();
        assert!(!queue.is_advancing());
        assert!(queue.active().is_none());
        queue.move_up(b);
        assert_eq!(queue.pending()[0].id, b);
        queue.set_duration(a, Duration::from_secs(90)).unwrap();
        assert!(queue.set_duration(b, Duration::ZERO).is_err());
        assert_eq!(queue.pending()[0].duration.unwrap().as_secs(), 120);
        assert_eq!(queue.pending()[1].duration.unwrap().as_secs(), 90);
        queue.set_duration(a, None).unwrap();
        assert_eq!(queue.pending()[1].duration, None);
        queue.remove(b);
        assert_eq!(queue.pending()[0].id, a);
        queue.clear();
        assert!(queue.is_idle());
    }

    #[test]
    fn optional_timer_parses_empty_zero_and_explicit_durations() {
        for (minutes, seconds) in [("", ""), (" ", " "), ("0", ""), ("0", "0")] {
            assert_eq!(parse_duration(minutes, seconds).unwrap(), None);
        }
        assert_eq!(
            parse_duration("15", "").unwrap(),
            Some(Duration::from_secs(900))
        );
        assert_eq!(
            parse_duration("", "5").unwrap(),
            Some(Duration::from_secs(5))
        );
        assert_eq!(parse_duration("1440", "").unwrap(), Some(MAX_DURATION));
        for (minutes, seconds) in [("-1", ""), ("x", ""), ("", "60"), ("1440", "1")] {
            assert!(parse_duration(minutes, seconds).is_err());
        }
        assert_eq!(format_limit(None), "No limit");
    }
}
