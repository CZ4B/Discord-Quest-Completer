use std::{
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};

use discord_quest_completer::{
    APP_NAME,
    game_icons::GameIcons,
    scheduler::format_duration,
    session::{SESSION_FILE, SessionState},
};
use eframe::egui::{self, Color32, FontId, RichText, Vec2};

const BG: Color32 = Color32::from_rgb(13, 16, 23);
const TEXT: Color32 = Color32::from_rgb(235, 239, 249);
const MUTED: Color32 = Color32::from_rgb(139, 150, 173);
const ACCENT: Color32 = Color32::from_rgb(151, 132, 255);

pub struct SessionWindow {
    state: SessionState,
    path: Option<PathBuf>,
    initial_elapsed: Duration,
    opened_at: Instant,
    last_read: Instant,
    icons: GameIcons,
    fallback_icon: egui::TextureHandle,
}

impl SessionWindow {
    pub fn new(cc: &eframe::CreationContext<'_>, title: String) -> Self {
        let path = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|directory| directory.join(SESSION_FILE)));
        let state = path
            .as_deref()
            .and_then(|path| SessionState::read(path).ok())
            .unwrap_or_else(|| SessionState::fallback(title));
        let initial_elapsed = state.elapsed_at(SystemTime::now());
        let icon = crate::icon().expect("embedded application icon must be valid");
        let fallback_icon = cc.egui_ctx.load_texture(
            "session-fallback-icon",
            egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            ),
            egui::TextureOptions::LINEAR,
        );
        Self {
            icons: GameIcons::new(&cc.egui_ctx, state.offline),
            state,
            path,
            initial_elapsed,
            opened_at: Instant::now(),
            last_read: Instant::now(),
            fallback_icon,
        }
    }
}

impl eframe::App for SessionWindow {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.last_read.elapsed() >= Duration::from_millis(200) {
            if let Some(path) = &self.path
                && let Ok(state) = SessionState::read(path)
            {
                self.state = state;
            }
            self.last_read = Instant::now();
        }
        self.icons.receive(ctx);
        let texture = self
            .icons
            .texture_for(&self.state.game_id, self.state.icon_hash.as_deref())
            .unwrap_or_else(|| self.fallback_icon.id());
        let elapsed = self.initial_elapsed + self.opened_at.elapsed();
        let remaining = self.state.remaining(elapsed);
        let timer =
            format_duration(remaining.unwrap_or_else(|| Duration::from_secs(elapsed.as_secs())));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(0))
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let center = rect.center();
                let compact = rect.height() < 400.0;
                let icon_size = if compact { 48.0 } else { 112.0 };
                let icon_offset = if compact { -75.0 } else { -132.0 };
                let name_offset = if compact { -37.0 } else { -56.0 };
                let timer_offset = if compact { 24.0 } else { 35.0 };
                let timer_size =
                    (rect.width() * 0.105).clamp(34.0, if compact { 44.0 } else { 76.0 });
                let width = (rect.width() - 36.0).max(1.0);
                let painter = ui.painter();
                painter.rect_filled(
                    egui::Rect::from_min_size(rect.min, Vec2::new(rect.width(), 3.0)),
                    0,
                    ACCENT,
                );
                if !compact {
                    painter.text(
                        rect.left_top() + Vec2::new(24.0, 25.0),
                        egui::Align2::LEFT_CENTER,
                        APP_NAME,
                        FontId::proportional(11.0),
                        MUTED,
                    );
                    painter.text(
                        rect.right_top() + Vec2::new(-24.0, 25.0),
                        egui::Align2::RIGHT_CENTER,
                        "SESSION ACTIVE",
                        FontId::proportional(10.0),
                        Color32::from_rgb(94, 218, 173),
                    );
                }
                let icon_rect = egui::Rect::from_center_size(
                    center + Vec2::new(0.0, icon_offset),
                    Vec2::splat(icon_size),
                );
                painter.rect_filled(icon_rect.expand(8.0), 24, Color32::from_rgb(26, 29, 43));
                ui.put(
                    icon_rect,
                    egui::Image::new((texture, Vec2::splat(icon_size))),
                );
                ui.put(
                    egui::Rect::from_center_size(
                        center + Vec2::new(0.0, name_offset),
                        Vec2::new(width, 30.0),
                    ),
                    egui::Label::new(
                        RichText::new(&self.state.game_name)
                            .size(if compact { 17.0 } else { 24.0 })
                            .strong()
                            .color(TEXT),
                    )
                    .truncate(),
                );
                let painter = ui.painter();
                painter.text(
                    center + Vec2::new(0.0, -12.0),
                    egui::Align2::CENTER_CENTER,
                    if remaining.is_some() {
                        "TIME REMAINING"
                    } else {
                        "TIME ELAPSED"
                    },
                    FontId::proportional(11.0),
                    MUTED,
                );
                painter.text(
                    center + Vec2::new(0.0, timer_offset),
                    egui::Align2::CENTER_CENTER,
                    timer,
                    FontId::monospace(timer_size),
                    TEXT,
                );
                let detail_y = if compact { 81.0 } else { 126.0 };
                let detail = if let Some(total) = self.state.duration_secs {
                    let progress =
                        (elapsed.as_secs_f64() / (total.max(1) as f64)).clamp(0.0, 1.0) as f32;
                    let bar = egui::Rect::from_center_size(
                        center + Vec2::new(0.0, if compact { 61.0 } else { 101.0 }),
                        Vec2::new(width.min(240.0), 4.0),
                    );
                    painter.rect_filled(bar, 2, Color32::from_rgb(39, 46, 62));
                    if progress > 0.0 {
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                bar.min,
                                Vec2::new(bar.width() * progress, bar.height()),
                            ),
                            2,
                            ACCENT,
                        );
                    }
                    format!("of {}", format_duration(Duration::from_secs(total)))
                } else {
                    "No time limit".into()
                };
                painter.text(
                    center + Vec2::new(0.0, detail_y),
                    egui::Align2::CENTER_CENTER,
                    detail,
                    FontId::proportional(11.0),
                    MUTED,
                );
                ui.put(
                    egui::Rect::from_center_size(
                        egui::pos2(center.x, rect.bottom() - 18.0),
                        Vec2::new(width, 20.0),
                    ),
                    egui::Label::new(
                        RichText::new(
                            "Use Stop in the launcher or close this window to end the session.",
                        )
                        .size(10.0)
                        .color(MUTED),
                    )
                    .truncate(),
                );
            });
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}
