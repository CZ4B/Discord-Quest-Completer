use std::{collections::HashSet, io::Read, path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const DATABASE_URL: &str = "https://cdn.discordapp.com/detectables/games.json";
const BUNDLED_DATABASE: &str = include_str!("../gamelist.json");
const MAX_DATABASE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Executable {
    pub name: String,
    pub relative_path: String,
}

#[derive(Clone, Debug)]
pub struct Game {
    pub id: String,
    pub name: String,
    pub icon_hash: Option<String>,
    pub executables: Vec<Executable>,
    search_name: String,
}

impl Game {
    pub fn matches(&self, query: &str) -> bool {
        self.search_name.contains(query)
    }
}

#[derive(Deserialize)]
struct RawGame {
    #[serde(default)]
    id: serde_json::Value,
    #[serde(default)]
    name: String,
    #[serde(default)]
    icon_hash: Option<String>,
    #[serde(default)]
    executables: Option<Vec<RawExecutable>>,
}

#[derive(Deserialize)]
struct RawExecutable {
    #[serde(default)]
    os: String,
    #[serde(default)]
    name: String,
}

pub fn executable_path(value: &str) -> Result<Executable> {
    let value = value.strip_prefix('>').unwrap_or(value);
    let normalized = value.replace('\\', "/");
    let parts: Vec<_> = normalized.split('/').collect();
    if parts.is_empty() || parts.iter().any(|part| !valid_component(part)) {
        bail!("invalid executable path: {value}");
    }
    let name = parts.last().context("missing executable filename")?;
    if !name.to_ascii_lowercase().ends_with(".exe") {
        bail!("not a Windows executable: {value}");
    }
    Ok(Executable {
        name: (*name).to_owned(),
        relative_path: normalized,
    })
}

fn valid_component(part: &str) -> bool {
    if part.is_empty()
        || part == "."
        || part == ".."
        || part.ends_with([' ', '.'])
        || part
            .chars()
            .any(|c| c.is_control() || "<>:\"|?*".contains(c))
    {
        return false;
    }
    let base = part.split('.').next().unwrap_or("").to_ascii_uppercase();
    !matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !(base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && matches!(base.as_bytes()[3], b'1'..=b'9'))
}

pub fn parse(data: &str) -> Result<Vec<Game>> {
    let raw: Vec<RawGame> = serde_json::from_str(data).context("invalid game database JSON")?;
    let mut games = Vec::new();
    let mut game_ids = HashSet::new();
    for entry in raw {
        let id = match entry.id {
            serde_json::Value::String(id) => id,
            serde_json::Value::Number(id) => id.to_string(),
            _ => continue,
        };
        if id.is_empty() || entry.name.trim().is_empty() {
            continue;
        }
        let mut seen = HashSet::new();
        let executables: Vec<_> = entry
            .executables
            .unwrap_or_default()
            .into_iter()
            .filter(|exe| exe.os == "win32")
            .filter_map(|exe| executable_path(&exe.name).ok())
            .filter(|exe| seen.insert(exe.relative_path.to_lowercase()))
            .collect();
        if !executables.is_empty() && game_ids.insert(id.clone()) {
            games.push(Game {
                id,
                search_name: entry.name.to_lowercase(),
                name: entry.name,
                icon_hash: entry.icon_hash,
                executables,
            });
        }
    }
    games.sort_by(|a, b| a.search_name.cmp(&b.search_name).then(a.id.cmp(&b.id)));
    if games.is_empty() {
        bail!("database contains no supported Windows games");
    }
    Ok(games)
}

pub fn bundled() -> Result<Vec<Game>> {
    parse(BUNDLED_DATABASE)
}

pub fn from_file(path: &Path) -> Result<Vec<Game>> {
    let data =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    parse(&data)
}

pub fn download() -> Result<Vec<Game>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("DiscordQuestCompleter/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(30))
        .build()?;
    let response = client.get(DATABASE_URL).send()?.error_for_status()?;
    let mut body = String::new();
    response
        .take(MAX_DATABASE_BYTES + 1)
        .read_to_string(&mut body)?;
    if body.len() as u64 > MAX_DATABASE_BYTES {
        bail!("database exceeds the 64 MiB download limit");
    }
    parse(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_paths_cannot_escape_or_use_device_names() {
        for path in [
            "../game.exe",
            "a/../../game.exe",
            "C:\\game.exe",
            "\\\\host\\game.exe",
            "/game.exe",
            "a//game.exe",
            "a/./game.exe",
            "NUL.exe",
            "con/game.exe",
            "a/com1.exe",
            "game.exe:stream",
            "a. /game.exe",
            "bad\0.exe",
        ] {
            assert!(executable_path(path).is_err(), "accepted {path:?}");
        }
        assert_eq!(
            executable_path("bin\\win64\\game.exe")
                .unwrap()
                .relative_path,
            "bin/win64/game.exe"
        );
        assert_eq!(executable_path(">hl2.exe").unwrap().name, "hl2.exe");
    }

    #[test]
    fn parses_filters_deduplicates_and_searches() {
        let games = parse(
            r#"[
          {"id":123,"name":"Zelda","icon_hash":"0123456789abcdef0123456789abcdef","executables":[
            {"os":"linux","name":"game"},
            {"os":"win32","name":"bin/game.exe"},
            {"os":"win32","name":"BIN/GAME.EXE"},
            {"os":"win32","name":"../escape.exe"}]},
          {"id":"2","name":"Alpha","executables":[{"os":"win32","name":"alpha.exe"}]},
          {"id":"3","name":"Empty","executables":null},
          {"id":"4","name":"No executable"}
        ]"#,
        )
        .unwrap();
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].name, "Alpha");
        assert_eq!(games[1].id, "123");
        assert_eq!(
            games[1].icon_hash.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(games[0].icon_hash, None);
        assert_eq!(games[1].executables.len(), 1);
        assert!(games[1].matches("zel"));
        assert!(parse("{}").is_err());
        assert!(parse("[]").is_err());
    }

    #[test]
    fn bundled_database_is_usable_offline() {
        let games = bundled().unwrap();
        assert!(games.len() > 1000);
        assert!(games.iter().any(|g| g.matches("overwatch")));
    }
}
