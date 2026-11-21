//! Lazy game artwork loading: bounded background requests, disk cache and GPU LRU.
use std::{
    collections::HashMap,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use eframe::egui::{self, ColorImage, TextureHandle, TextureId};

use crate::database::Game;

const MAX_IMAGE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_TEXTURES: usize = 192;
const ICON_SIZE: u32 = 128;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct IconKey {
    application_id: String,
    hash: String,
}

impl IconKey {
    fn new(id: &str, hash: &str) -> Option<Self> {
        let hex = hash.strip_prefix("a_").unwrap_or(hash);
        if id.is_empty()
            || id.len() > 20
            || !id.bytes().all(|b| b.is_ascii_digit())
            || hex.len() != 32
            || !hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return None;
        }
        Some(Self {
            application_id: id.into(),
            hash: hash.to_ascii_lowercase(),
        })
    }

    fn filename(&self) -> String {
        format!("{}-{}.png", self.application_id, self.hash)
    }
    fn url(&self) -> String {
        format!(
            "https://cdn.discordapp.com/app-icons/{}/{}.png?size={ICON_SIZE}",
            self.application_id, self.hash
        )
    }
}

enum Entry {
    Loading,
    Ready { texture: TextureHandle, used: u64 },
    Failed { retry_at: Instant },
}

pub struct GameIcons {
    requests: SyncSender<IconKey>,
    results: Receiver<(IconKey, Option<ColorImage>)>,
    entries: HashMap<IconKey, Entry>,
    clock: u64,
}

impl GameIcons {
    pub fn new(ctx: &egui::Context, offline: bool) -> Self {
        let (requests, receiver) = mpsc::sync_channel::<IconKey>(32);
        let (sender, results) = mpsc::channel();
        let receiver = Arc::new(Mutex::new(receiver));
        let directory = cache_directory();
        for _ in 0..4 {
            let (receiver, sender, ctx, directory) = (
                receiver.clone(),
                sender.clone(),
                ctx.clone(),
                directory.clone(),
            );
            std::thread::spawn(move || {
                let client = (!offline)
                    .then(|| {
                        reqwest::blocking::Client::builder()
                            .user_agent(concat!(
                                "DiscordQuestCompleter/",
                                env!("CARGO_PKG_VERSION")
                            ))
                            .connect_timeout(Duration::from_secs(4))
                            .timeout(Duration::from_secs(10))
                            .redirect(reqwest::redirect::Policy::none())
                            .build()
                            .ok()
                    })
                    .flatten();
                loop {
                    let request = match receiver.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => break,
                    };
                    let Ok(key) = request else {
                        break;
                    };
                    let image = load_image(&key, &directory, client.as_ref()).ok();
                    if sender.send((key, image)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
        }
        Self {
            requests,
            results,
            entries: HashMap::new(),
            clock: 0,
        }
    }

    pub fn receive(&mut self, ctx: &egui::Context) {
        while let Ok((key, image)) = self.results.try_recv() {
            let entry = if let Some(image) = image {
                let texture = ctx.load_texture(
                    format!("game-icon-{}", key.filename()),
                    image,
                    egui::TextureOptions::LINEAR,
                );
                Entry::Ready {
                    texture,
                    used: self.clock,
                }
            } else {
                Entry::Failed {
                    retry_at: Instant::now() + Duration::from_secs(300),
                }
            };
            self.entries.insert(key, entry);
        }
        while self
            .entries
            .values()
            .filter(|entry| matches!(entry, Entry::Ready { .. }))
            .count()
            > MAX_TEXTURES
        {
            let oldest = self
                .entries
                .iter()
                .filter_map(|(key, entry)| match entry {
                    Entry::Ready { used, .. } => Some((key.clone(), *used)),
                    _ => None,
                })
                .min_by_key(|(_, used)| *used)
                .map(|(key, _)| key);
            if let Some(key) = oldest {
                self.entries.remove(&key);
            } else {
                break;
            }
        }
    }

    pub fn texture(&mut self, game: &Game) -> Option<TextureId> {
        self.texture_for(&game.id, game.icon_hash.as_deref())
    }

    pub fn texture_for(&mut self, game_id: &str, icon_hash: Option<&str>) -> Option<TextureId> {
        let key = IconKey::new(game_id, icon_hash?)?;
        self.clock = self.clock.wrapping_add(1);
        match self.entries.get_mut(&key) {
            Some(Entry::Ready { texture, used }) => {
                *used = self.clock;
                return Some(texture.id());
            }
            Some(Entry::Loading) => return None,
            Some(Entry::Failed { retry_at }) if *retry_at > Instant::now() => return None,
            _ => {}
        }
        if self.requests.try_send(key.clone()).is_ok() {
            self.entries.insert(key, Entry::Loading);
        }
        None
    }

    pub fn retry_failed(&mut self) {
        self.entries
            .retain(|_, entry| !matches!(entry, Entry::Failed { .. }));
    }
}

fn cache_directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("DiscordQuestCompleter")
        .join("cache")
        .join("game-icons")
}

fn load_image(
    key: &IconKey,
    directory: &Path,
    client: Option<&reqwest::blocking::Client>,
) -> Result<ColorImage> {
    let cache_path = directory.join(key.filename());
    if let Ok(file) = std::fs::File::open(&cache_path) {
        let mut bytes = Vec::new();
        if file
            .take(MAX_IMAGE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_ok()
            && let Ok(image) = decode(&bytes)
        {
            return Ok(image);
        }
    }
    let client = client.context("icon is not cached; networking is disabled")?;
    let response = client.get(key.url()).send()?.error_for_status()?;
    let mut bytes = Vec::new();
    response.take(MAX_IMAGE_BYTES + 1).read_to_end(&mut bytes)?;
    let image = decode(&bytes)?;
    if std::fs::create_dir_all(directory).is_ok()
        && let Ok(mut file) = tempfile::NamedTempFile::new_in(directory)
        && file.write_all(&bytes).is_ok()
    {
        let _ = file.persist(&cache_path);
    }
    Ok(image)
}

fn decode(bytes: &[u8]) -> Result<ColorImage> {
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        bail!("icon exceeds 2 MiB");
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1024);
    limits.max_image_height = Some(1024);
    limits.max_alloc = Some(8 * 1024 * 1024);
    reader.limits(limits);
    let thumbnail = reader
        .decode()?
        .thumbnail(ICON_SIZE, ICON_SIZE)
        .into_rgba8();
    let mut canvas = image::RgbaImage::new(ICON_SIZE, ICON_SIZE);
    image::imageops::overlay(
        &mut canvas,
        &thumbnail,
        ((ICON_SIZE - thumbnail.width()) / 2) as i64,
        ((ICON_SIZE - thumbnail.height()) / 2) as i64,
    );
    for (x, y, pixel) in canvas.enumerate_pixels_mut() {
        let dx = (x as f32 + 0.5 - 64.0).abs() - 40.0;
        let dy = (y as f32 + 0.5 - 64.0).abs() - 40.0;
        let coverage = (24.5 - dx.max(0.0).hypot(dy.max(0.0))).clamp(0.0, 1.0);
        pixel[3] = (pixel[3] as f32 * coverage) as u8;
    }
    Ok(ColorImage::from_rgba_unmultiplied(
        [ICON_SIZE as usize, ICON_SIZE as usize],
        canvas.as_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_discord_asset_keys_can_form_urls_and_cache_paths() {
        let hash = "d20f9f39f2eec584dcdbc7b206786124";
        let key = IconKey::new("356875221078245376", hash).unwrap();
        assert_eq!(
            key.url(),
            format!("https://cdn.discordapp.com/app-icons/356875221078245376/{hash}.png?size=128")
        );
        for (id, hash) in [
            ("../other", hash),
            ("42", "../icon"),
            ("42", "https://other/image.png"),
            ("42", ""),
        ] {
            assert!(IconKey::new(id, hash).is_none());
        }
        assert!(IconKey::new("42", &format!("a_{hash}")).is_some());
    }

    #[test]
    fn cached_icons_work_without_network_and_missing_icons_fail_cleanly() {
        let directory = tempfile::tempdir().unwrap();
        let key = IconKey::new("42", "0123456789abcdef0123456789abcdef").unwrap();
        assert!(load_image(&key, directory.path(), None).is_err());
        image::RgbaImage::from_pixel(32, 32, image::Rgba([12, 120, 240, 255]))
            .save(directory.path().join(key.filename()))
            .unwrap();
        let image = load_image(&key, directory.path(), None).unwrap();
        assert_eq!(image.size, [128, 128]);
        assert_eq!(image.pixels[64 * 128 + 64].to_array(), [12, 120, 240, 255]);
        assert_eq!(image.pixels[0].a(), 0);
        std::fs::write(directory.path().join(key.filename()), "not an image").unwrap();
        assert!(load_image(&key, directory.path(), None).is_err());
    }

    #[test]
    fn invalid_or_oversized_images_are_rejected() {
        assert!(decode(b"<html>not an icon</html>").is_err());
        assert!(decode(&vec![0; MAX_IMAGE_BYTES as usize + 1]).is_err());
        let mut bytes = Cursor::new(Vec::new());
        image::RgbaImage::new(1025, 1)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        assert!(decode(bytes.get_ref()).is_err());
    }
}
