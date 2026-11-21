use discord_quest_completer::{
    APP_NAME, VERSION,
    database::{self, Game},
    game_icons::GameIcons,
    process::{ProcessKey, ProcessManager},
    scheduler::{SessionQueue, format_duration, format_limit, parse_duration},
    updates,
};
use eframe::egui::{self, Color32, FontFamily, FontId, RichText};
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

const BG: Color32 = Color32::from_rgb(13, 16, 23);
const SURFACE: Color32 = Color32::from_rgb(20, 24, 34);
const RAISED: Color32 = Color32::from_rgb(27, 32, 45);
const BORDER: Color32 = Color32::from_rgb(39, 46, 62);
const TEXT: Color32 = Color32::from_rgb(235, 239, 249);
const MUTED: Color32 = Color32::from_rgb(139, 150, 173);
const ACCENT: Color32 = Color32::from_rgb(151, 132, 255);
const GREEN: Color32 = Color32::from_rgb(94, 218, 173);
const RED: Color32 = Color32::from_rgb(255, 139, 154);

enum Event {
    Database {
        games: Vec<Game>,
        status: String,
        refreshing: bool,
    },
    DatabaseError(String),
    Update(Result<updates::CheckResult, String>),
}

pub struct Launcher {
    games: Vec<Game>,
    filtered: Vec<usize>,
    query: String,
    selected_id: Option<String>,
    database_status: String,
    loading: bool,
    offline: bool,
    local_database: Option<PathBuf>,
    scheduler: SessionQueue,
    duration_minutes: String,
    duration_seconds: String,
    error: Option<String>,
    update_status: String,
    update_available: Option<updates::AvailableRelease>,
    checking_updates: bool,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    last_poll: Instant,
    application_icon: egui::TextureHandle,
    game_icons: GameIcons,
}

impl Launcher {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        mut processes: ProcessManager,
        offline: bool,
        local_database: Option<PathBuf>,
    ) -> Self {
        configure_style(&cc.egui_ctx);
        processes.set_offline(offline);
        let icon = crate::icon().expect("embedded application icon must be a valid ICO");
        let application_icon = cc.egui_ctx.load_texture(
            "application-icon",
            egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            ),
            egui::TextureOptions::LINEAR,
        );
        let (tx, rx) = mpsc::channel();
        let game_icons = GameIcons::new(&cc.egui_ctx, offline);
        let mut app = Self {
            games: Vec::new(),
            filtered: Vec::new(),
            query: String::new(),
            selected_id: None,
            database_status: "Loading library".into(),
            loading: false,
            offline,
            local_database,
            scheduler: SessionQueue::new(processes),
            duration_minutes: String::new(),
            duration_seconds: String::new(),
            error: None,
            update_status: if offline {
                "Release checks disabled in offline mode".into()
            } else {
                String::new()
            },
            update_available: None,
            checking_updates: false,
            tx,
            rx,
            last_poll: Instant::now(),
            application_icon,
            game_icons,
        };
        app.load_database(&cc.egui_ctx);
        if !offline {
            app.check_updates(&cc.egui_ctx);
        }
        app
    }

    fn load_database(&mut self, ctx: &egui::Context) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.game_icons.retry_failed();
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        let offline = self.offline;
        let path = self.local_database.clone();
        std::thread::spawn(move || {
            let send = |event| {
                let _ = tx.send(event);
                ctx.request_repaint();
            };
            if let Some(path) = path {
                match database::from_file(&path) {
                    Ok(games) => send(Event::Database {
                        games,
                        status: "Local library".into(),
                        refreshing: false,
                    }),
                    Err(error) => send(Event::DatabaseError(format!("{error:#}"))),
                }
                return;
            }
            match database::bundled() {
                Ok(games) => send(Event::Database {
                    games,
                    status: if offline {
                        "Offline library"
                    } else {
                        "Refreshing library"
                    }
                    .into(),
                    refreshing: !offline,
                }),
                Err(error) => send(Event::DatabaseError(format!("{error:#}"))),
            }
            if !offline {
                match database::download() {
                    Ok(games) => send(Event::Database {
                        games,
                        status: "Library up to date".into(),
                        refreshing: false,
                    }),
                    Err(error) => send(Event::DatabaseError(format!(
                        "Could not refresh the library. Your offline games are still available.\n{error:#}"
                    ))),
                }
            }
        });
    }

    fn check_updates(&mut self, ctx: &egui::Context) {
        if self.checking_updates || self.offline {
            return;
        }
        self.checking_updates = true;
        self.update_available = None;
        self.update_status = "Checking releases...".into();
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Event::Update(
                updates::check().map_err(|e| format!("{e:#}")),
            ));
            ctx.request_repaint();
        });
    }

    fn receive(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Database {
                    games,
                    status,
                    refreshing,
                } => {
                    if self
                        .selected_id
                        .as_ref()
                        .is_some_and(|id| !games.iter().any(|g| &g.id == id))
                    {
                        self.selected_id = None;
                    }
                    self.games = games;
                    self.loading = refreshing;
                    self.database_status = status;
                    self.filter();
                }
                Event::DatabaseError(error) => {
                    self.loading = false;
                    self.database_status = if self.games.is_empty() {
                        "Library unavailable"
                    } else {
                        "Using offline library"
                    }
                    .into();
                    self.error = Some(error);
                }
                Event::Update(result) => {
                    self.checking_updates = false;
                    match result {
                        Ok(updates::CheckResult::Available(release)) => {
                            self.update_status = format!("Version {} available", release.version);
                            self.update_available = Some(release);
                        }
                        Ok(updates::CheckResult::UpToDate) => {
                            self.update_status = "No newer release available".into();
                            self.update_available = None;
                        }
                        Ok(updates::CheckResult::NoPublishedRelease) => {
                            self.update_status = "No published stable releases yet".into();
                            self.update_available = None;
                        }
                        Err(error) => {
                            self.update_status = format!("Release check unavailable: {error}");
                            self.update_available = None;
                        }
                    }
                }
            }
        }
    }

    fn filter(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.filtered = self
            .games
            .iter()
            .enumerate()
            .filter_map(|(i, game)| game.matches(&query).then_some(i))
            .collect();
    }

    fn header(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2((ui.available_width() - 140.0).max(0.0), 54.0),
                egui::Sense::click_and_drag(),
            );
            let p = ui.painter();
            p.image(
                self.application_icon.id(),
                egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 5.0), egui::vec2(44.0, 44.0)),
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            let x = rect.left() + 60.0;
            draw_text(
                p,
                egui::pos2(x, rect.top() + 4.0),
                APP_NAME,
                22.0,
                TEXT,
                rect.width() - 60.0,
                true,
            );
            draw_text(
                p,
                egui::pos2(x, rect.top() + 34.0),
                "Your game library. Your sessions. One place.",
                12.0,
                MUTED,
                rect.width() - 60.0,
                false,
            );
            if response.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if response.double_clicked() {
                let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            }
            if ui
                .add_sized([36.0, 32.0], egui::Button::new("—").frame(false))
                .on_hover_text("Minimize")
                .clicked()
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            if ui
                .add_sized([36.0, 32.0], egui::Button::new("□").frame(false))
                .on_hover_text("Maximize / restore")
                .clicked()
            {
                let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            }
            if ui
                .add_sized(
                    [36.0, 32.0],
                    egui::Button::new(RichText::new("×").size(22.0)).frame(false),
                )
                .on_hover_text("Close and stop all sessions")
                .clicked()
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }

    fn library(&mut self, ui: &mut egui::Ui) {
        surface().show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.horizontal(|ui| {
                ui.label(heading("Game library", 19.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    badge(ui, &number(self.games.len()), RAISED, MUTED);
                });
            });
            ui.add_space(8.0);
            let response = ui.add_sized(
                [ui.available_width(), 40.0],
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Search games...")
                    .margin(egui::vec2(12.0, 11.0)),
            );
            if response.changed() {
                self.filter();
            }
            if ui.input(|input| input.key_pressed(egui::Key::Escape)) && response.has_focus() {
                self.query.clear();
                self.filter();
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(caption(if self.query.trim().is_empty() {
                    "ALL GAMES"
                } else {
                    "SEARCH RESULTS"
                }));
                if !self.query.trim().is_empty() {
                    ui.label(
                        RichText::new(format!("{} found", number(self.filtered.len())))
                            .size(11.0)
                            .color(MUTED),
                    );
                }
            });
            ui.add_space(5.0);
            let height = ui.available_height().max(40.0);
            if self.filtered.is_empty() {
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), height),
                    egui::Layout::centered_and_justified(egui::Direction::TopDown),
                    |ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(heading(
                                if self.loading {
                                    "Loading your library"
                                } else {
                                    "No games found"
                                },
                                17.0,
                            ));
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(if self.query.is_empty() {
                                    "Refresh the library to try again."
                                } else {
                                    "Try a different game name."
                                })
                                .color(MUTED),
                            );
                        });
                    },
                );
            } else {
                egui::ScrollArea::vertical()
                    .id_salt("library")
                    .auto_shrink([false, false])
                    .max_height(height)
                    .show_rows(ui, 68.0, self.filtered.len(), |ui, range| {
                        for row in range {
                            let game = &self.games[self.filtered[row]];
                            let selected = self.selected_id.as_ref() == Some(&game.id);
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), 68.0),
                                egui::Sense::click(),
                            );
                            let p = ui.painter();
                            let fill = if selected {
                                Color32::from_rgb(43, 38, 68)
                            } else if response.hovered() {
                                RAISED
                            } else {
                                SURFACE
                            };
                            p.rect_filled(rect, 9.0, fill);
                            if selected {
                                p.rect_stroke(
                                    rect,
                                    9.0,
                                    egui::Stroke::new(1.0_f32, Color32::from_rgb(85, 69, 137)),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            let tile = egui::Rect::from_min_size(
                                rect.min + egui::vec2(10.0, 13.0),
                                egui::vec2(42.0, 42.0),
                            );
                            game_tile(p, tile, self.game_icons.texture(game));
                            let x = rect.left() + 64.0;
                            draw_text(
                                p,
                                egui::pos2(x, rect.top() + 15.0),
                                &game.name,
                                14.0,
                                if selected {
                                    TEXT
                                } else {
                                    Color32::from_rgb(209, 216, 232)
                                },
                                rect.width() - 94.0,
                                true,
                            );
                            let running = self
                                .scheduler
                                .processes()
                                .running()
                                .keys()
                                .any(|key| key.game_id == game.id);
                            let subtitle = if running {
                                "Session active".to_owned()
                            } else {
                                format!(
                                    "{} launch option{}",
                                    game.executables.len(),
                                    if game.executables.len() == 1 { "" } else { "s" }
                                )
                            };
                            draw_text(
                                p,
                                egui::pos2(x, rect.top() + 39.0),
                                &subtitle,
                                11.0,
                                if running { GREEN } else { MUTED },
                                rect.width() - 94.0,
                                false,
                            );
                            p.text(
                                egui::pos2(rect.right() - 15.0, rect.center().y),
                                egui::Align2::CENTER_CENTER,
                                if selected { "•" } else { "›" },
                                FontId::proportional(19.0),
                                if selected { ACCENT } else { MUTED },
                            );
                            if response.on_hover_text(&game.name).clicked() {
                                self.selected_id = Some(game.id.clone());
                            }
                        }
                    });
            }
        });
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        surface().show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let selected = self
                .selected_id
                .as_ref()
                .and_then(|id| self.games.iter().find(|g| &g.id == id))
                .cloned();
            if let Some(game) = selected {
                egui::ScrollArea::vertical()
                    .id_salt("selected_game")
                    .auto_shrink([false, false])
                    .max_height(ui.available_height())
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(52.0, 52.0), egui::Sense::hover());
                            game_tile(ui.painter(), rect, self.game_icons.texture(&game));
                            ui.vertical(|ui| {
                                ui.label(caption("SELECTED GAME"));
                                ui.add(egui::Label::new(heading(&game.name, 22.0)).truncate())
                                    .on_hover_text(&game.name);
                                ui.label(
                                    RichText::new("One active game at a time")
                                        .size(11.0)
                                        .color(MUTED),
                                );
                            });
                        });
                        ui.add_space(12.0);
                        ui.separator();
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(heading("Run for", 13.0));
                            ui.add_sized(
                                [48.0, 24.0],
                                egui::TextEdit::singleline(&mut self.duration_minutes)
                                    .hint_text("—")
                                    .char_limit(4),
                            );
                            ui.label(RichText::new("min").size(11.0).color(MUTED));
                            ui.add_sized(
                                [42.0, 24.0],
                                egui::TextEdit::singleline(&mut self.duration_seconds)
                                    .hint_text("—")
                                    .char_limit(2),
                            );
                            ui.label(RichText::new("sec").size(11.0).color(MUTED));
                            if ui.available_width() > 115.0 {
                                if ui.small_button("15m").clicked() {
                                    self.duration_minutes = "15".into();
                                    self.duration_seconds.clear();
                                }
                                if ui.small_button("30m").clicked() {
                                    self.duration_minutes = "30".into();
                                    self.duration_seconds.clear();
                                }
                            }
                            if (!self.duration_minutes.is_empty()
                                || !self.duration_seconds.is_empty())
                                && ui.small_button("Clear").clicked()
                            {
                                self.duration_minutes.clear();
                                self.duration_seconds.clear();
                            }
                        });
                        let parsed = parse_duration(&self.duration_minutes, &self.duration_seconds);
                        let valid = parsed.is_ok();
                        let duration = parsed.as_ref().ok().copied().flatten();
                        let timer_hint = match &parsed {
                            Ok(None) => "No time limit — runs until you stop it.".into(),
                            Ok(Some(duration)) => format!(
                                "Timer enabled — stops after {}.",
                                format_duration(*duration)
                            ),
                            Err(error) => error.to_string(),
                        };
                        ui.label(RichText::new(timer_hint).size(11.0).color(if valid {
                            MUTED
                        } else {
                            RED
                        }));
                        ui.add_space(8.0);
                        ui.label(heading("Launch options", 14.0));
                        for exe in &game.executables {
                            let key = ProcessKey::new(&game, exe);
                            let running = self.scheduler.processes().running().contains_key(&key);
                            egui::Frame::new()
                                .fill(if running {
                                    Color32::from_rgb(22, 41, 39)
                                } else {
                                    RAISED
                                })
                                .corner_radius(10)
                                .inner_margin(10)
                                .stroke(egui::Stroke::new(1.0_f32, BORDER))
                                .show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        let width = (ui.available_width()
                                            - 146.0
                                            - ui.spacing().item_spacing.x * 2.0)
                                            .max(30.0);
                                        ui.allocate_ui_with_layout(
                                            egui::vec2(width, 44.0),
                                            egui::Layout::top_down(egui::Align::LEFT),
                                            |ui| {
                                                ui.set_min_width(width);
                                                ui.add(
                                                    egui::Label::new(heading(&exe.name, 13.0))
                                                        .truncate(),
                                                );
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(&exe.relative_path)
                                                            .size(10.0)
                                                            .color(MUTED),
                                                    )
                                                    .truncate(),
                                                )
                                                .on_hover_text(&exe.relative_path);
                                            },
                                        );
                                        let can_start =
                                            running || (valid && self.scheduler.is_idle());
                                        if ui
                                            .add_enabled_ui(can_start, |ui| {
                                                ui.add_sized(
                                                    [68.0, 32.0],
                                                    action_button(
                                                        if running { "Stop" } else { "Start" },
                                                        running,
                                                    ),
                                                )
                                            })
                                            .inner
                                            .clicked()
                                        {
                                            let result = if running {
                                                self.scheduler.stop()
                                            } else {
                                                self.scheduler.start_now(&game, exe, duration)
                                            };
                                            if let Err(error) = result {
                                                self.error = Some(format!("{error:#}"));
                                            }
                                        }
                                        if ui
                                            .add_enabled_ui(valid, |ui| {
                                                ui.add_sized(
                                                    [78.0, 32.0],
                                                    egui::Button::new(
                                                        RichText::new("+ Queue")
                                                            .size(12.0)
                                                            .color(ACCENT),
                                                    )
                                                    .corner_radius(8),
                                                )
                                            })
                                            .inner
                                            .clicked()
                                            && let Err(error) =
                                                self.scheduler.enqueue(&game, exe, duration)
                                        {
                                            self.error = Some(format!("{error:#}"));
                                        }
                                    });
                                });
                            ui.add_space(5.0);
                        }
                    });
            } else {
                let height = ui.available_height();
                ui.vertical_centered(|ui| {
                    ui.add_space(((height - 180.0) * 0.5).max(0.0));
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(64.0, 64.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 18.0, Color32::from_rgb(35, 32, 55));
                    gamepad(ui.painter(), rect.shrink(15.0), ACCENT);
                    ui.add_space(14.0);
                    ui.label(heading("Build your game queue", 22.0));
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Start without a time limit, or set an optional timer.")
                            .size(12.0)
                            .color(MUTED),
                    );
                });
            }
        });
    }

    fn sessions(&mut self, ui: &mut egui::Ui) {
        surface().show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.horizontal(|ui| {
                ui.label(heading("Session & queue", 16.0));
                if self.scheduler.active().is_some() {
                    badge(ui, "1 active", Color32::from_rgb(26, 53, 45), GREEN);
                }
            });
            let status = self.scheduler.status().to_owned();
            ui.add(egui::Label::new(RichText::new(&status).size(10.0).color(MUTED)).truncate())
                .on_hover_text(status);
            if let Some((item, remaining, progress, elapsed)) =
                self.scheduler.active().map(|active| {
                    (
                        active.item.clone(),
                        active.remaining(),
                        active.progress(),
                        Duration::from_secs(active.started.elapsed().as_secs()),
                    )
                })
            {
                egui::Frame::new()
                    .fill(Color32::from_rgb(22, 36, 37))
                    .corner_radius(9)
                    .inner_margin(10)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let width = (ui.available_width() - 112.0).max(30.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(width, 35.0),
                                egui::Layout::top_down(egui::Align::LEFT),
                                |ui| {
                                    ui.set_min_width(width);
                                    ui.add(
                                        egui::Label::new(heading(&item.game.name, 14.0)).truncate(),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(format!(
                                                "{} / {}",
                                                item.executable.name,
                                                format_limit(item.duration)
                                            ))
                                            .size(10.0)
                                            .color(MUTED),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(&item.executable.relative_path);
                                },
                            );
                            ui.label(
                                RichText::new(
                                    remaining
                                        .map(format_duration)
                                        .unwrap_or_else(|| format_duration(elapsed)),
                                )
                                .monospace()
                                .size(22.0)
                                .color(GREEN),
                            );
                        });
                        if let Some(progress) = progress {
                            ui.add(
                                egui::ProgressBar::new(progress)
                                    .desired_width(ui.available_width())
                                    .desired_height(4.0)
                                    .fill(GREEN),
                            );
                        }
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    item.duration.is_some(),
                                    egui::Button::new("+5 min").small(),
                                )
                                .clicked()
                                && let Err(error) =
                                    self.scheduler.extend_active(Duration::from_secs(300))
                            {
                                self.error = Some(format!("{error:#}"));
                            }
                            if ui
                                .small_button("Skip")
                                .on_hover_text("Stop this game and start the next queued game")
                                .clicked()
                                && let Err(error) = self.scheduler.skip()
                            {
                                self.error = Some(format!("{error:#}"));
                            }
                            if ui
                                .add(egui::Button::new(RichText::new("Stop").color(RED)).small())
                                .on_hover_text("Stop the active game and pause the queue")
                                .clicked()
                                && let Err(error) = self.scheduler.stop()
                            {
                                self.error = Some(format!("{error:#}"));
                            }
                        });
                    });
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                ui.label(heading(
                    &format!("Up next ({})", self.scheduler.pending().len()),
                    13.0,
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            !self.scheduler.pending().is_empty(),
                            egui::Button::new(RichText::new("Clear").size(11.0).color(MUTED))
                                .frame(false),
                        )
                        .clicked()
                    {
                        self.scheduler.clear();
                    }
                    if self.scheduler.is_advancing() {
                        if ui.small_button("Pause queue").clicked() {
                            self.scheduler.pause();
                        }
                    } else {
                        let label = if self.scheduler.active().is_some() {
                            "Resume queue"
                        } else {
                            "Start queue"
                        };
                        if ui
                            .add_enabled(
                                !self.scheduler.pending().is_empty()
                                    || self.scheduler.active().is_some(),
                                egui::Button::new(RichText::new(label).size(11.0).color(ACCENT)),
                            )
                            .clicked()
                            && let Err(error) = self.scheduler.run()
                        {
                            self.error = Some(format!("{error:#}"));
                        }
                    }
                });
            });
            if self.scheduler.pending().is_empty() {
                ui.label(
                    RichText::new("Add games with + Queue. They will run one after another.")
                        .size(11.0)
                        .color(MUTED),
                );
            } else {
                let pending: Vec<_> = self.scheduler.pending().iter().cloned().collect();
                egui::ScrollArea::vertical()
                    .id_salt("queue_items")
                    .auto_shrink([false, false])
                    .max_height(ui.available_height().max(24.0))
                    .show_rows(ui, 42.0, pending.len(), |ui, range| {
                        for index in range {
                            let item = &pending[index];
                            ui.push_id(item.id, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(format!("{:02}", index + 1))
                                            .monospace()
                                            .size(10.0)
                                            .color(MUTED),
                                    );
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(30.0, 30.0),
                                        egui::Sense::hover(),
                                    );
                                    game_tile(
                                        ui.painter(),
                                        rect,
                                        self.game_icons.texture(&item.game),
                                    );
                                    let width = (ui.available_width() - 109.0).max(25.0);
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(width, 38.0),
                                        egui::Layout::top_down(egui::Align::LEFT),
                                        |ui| {
                                            ui.set_min_width(width);
                                            ui.add(
                                                egui::Label::new(heading(&item.game.name, 12.0))
                                                    .truncate(),
                                            );
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&item.executable.name)
                                                        .size(9.0)
                                                        .color(MUTED),
                                                )
                                                .truncate(),
                                            )
                                            .on_hover_text(&item.executable.relative_path);
                                        },
                                    );
                                    ui.menu_button(
                                        RichText::new(format_limit(item.duration))
                                            .monospace()
                                            .size(11.0),
                                        |ui| {
                                            ui.label(heading("Run duration", 13.0));
                                            let mut minutes = item
                                                .duration
                                                .map(|duration| duration.as_secs() / 60)
                                                .unwrap_or(0);
                                            let mut seconds = item
                                                .duration
                                                .map(|duration| duration.as_secs() % 60)
                                                .unwrap_or(0);
                                            let changed = ui
                                                .horizontal(|ui| {
                                                    let a = ui
                                                        .add(
                                                            egui::DragValue::new(&mut minutes)
                                                                .range(0..=1440)
                                                                .suffix(" min"),
                                                        )
                                                        .changed();
                                                    let b = ui
                                                        .add(
                                                            egui::DragValue::new(&mut seconds)
                                                                .range(0..=59)
                                                                .suffix(" sec"),
                                                        )
                                                        .changed();
                                                    a || b
                                                })
                                                .inner;
                                            if changed
                                                && let Err(error) = self.scheduler.set_duration(
                                                    item.id,
                                                    (minutes * 60 + seconds > 0).then(|| {
                                                        Duration::from_secs(minutes * 60 + seconds)
                                                    }),
                                                )
                                            {
                                                self.error = Some(format!("{error:#}"));
                                            }
                                            if ui.button("No time limit").clicked() {
                                                let _ = self.scheduler.set_duration(item.id, None);
                                                ui.close();
                                            }
                                            if ui
                                                .add_enabled(
                                                    index > 0,
                                                    egui::Button::new("Move up"),
                                                )
                                                .clicked()
                                            {
                                                self.scheduler.move_up(item.id);
                                                ui.close();
                                            }
                                        },
                                    );
                                    if ui
                                        .small_button("×")
                                        .on_hover_text("Remove from queue")
                                        .clicked()
                                    {
                                        self.scheduler.remove(item.id);
                                    }
                                });
                            });
                        }
                    });
            }
        });
    }

    fn footer(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            if self.loading {
                ui.add(egui::Spinner::new().size(12.0));
            } else {
                ui.label(RichText::new("•").color(
                    if self.database_status == "Library up to date" {
                        GREEN
                    } else {
                        MUTED
                    },
                ));
            }
            ui.label(RichText::new(&self.database_status).size(11.0).color(MUTED));
            ui.label(
                RichText::new(format!("/  {} games", number(self.games.len())))
                    .size(11.0)
                    .color(MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button(RichText::new("•••").color(MUTED), |ui| {
                    ui.label(heading(APP_NAME, 13.0));
                    ui.label(
                        RichText::new(format!("Version {VERSION}"))
                            .size(11.0)
                            .color(MUTED),
                    );
                    ui.label(RichText::new("Dev by CZ4B").size(11.0).color(MUTED));
                    ui.separator();
                    if ui
                        .add_enabled(!self.loading, egui::Button::new("Refresh library"))
                        .clicked()
                    {
                        self.load_database(ctx);
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            !self.offline && !self.checking_updates,
                            egui::Button::new("Check releases"),
                        )
                        .clicked()
                    {
                        self.check_updates(ctx);
                    }
                    if !self.update_status.is_empty() {
                        ui.label(RichText::new(&self.update_status).size(11.0).color(MUTED));
                    }
                    if let Some(release) = &self.update_available {
                        ui.hyperlink_to("View release", &release.url);
                    }
                });
                let active = self.scheduler.processes().running().len();
                ui.label(
                    RichText::new(format!(
                        "{active} active / {} queued",
                        self.scheduler.pending().len()
                    ))
                    .size(11.0)
                    .color(if active > 0 { GREEN } else { MUTED }),
                );
            });
        });
    }
}

impl eframe::App for Launcher {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive();
        self.game_icons.receive(ctx);
        if let Some(game) = self
            .selected_id
            .as_ref()
            .and_then(|id| self.games.iter().find(|game| &game.id == id))
        {
            self.game_icons.texture(game);
        }
        if self.last_poll.elapsed() >= Duration::from_millis(200) {
            if let Err(error) = self.scheduler.tick() {
                self.error = Some(format!("{error:#}"));
            }
            self.last_poll = Instant::now();
        }
        if self.scheduler.active().is_some() || self.scheduler.is_advancing() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(26, 20)),
            )
            .show(ctx, |ui| self.header(ui, ctx));
        egui::TopBottomPanel::bottom("footer")
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(26, 12)),
            )
            .show(ctx, |ui| self.footer(ui, ctx));
        if let Some(error) = self.error.clone() {
            egui::TopBottomPanel::top("notice")
                .frame(
                    egui::Frame::new()
                        .fill(BG)
                        .inner_margin(egui::Margin::symmetric(26, 4)),
                )
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(Color32::from_rgb(53, 39, 33))
                        .corner_radius(9)
                        .inner_margin(12)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.set_max_width((ui.available_width() - 75.0).max(80.0));
                                    ui.label(
                                        RichText::new("Something needs your attention")
                                            .color(Color32::from_rgb(255, 202, 151))
                                            .size(12.0),
                                    );
                                    ui.label(RichText::new(error).size(11.0).color(MUTED));
                                });
                                if ui.small_button("Dismiss").clicked() {
                                    self.error = None;
                                }
                            });
                        });
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin {
                left: 26,
                right: 26,
                top: 4,
                bottom: 4,
            }))
            .show(ctx, |ui| {
                let available = ui.available_size();
                let gap = 18.0;
                let left_width = (available.x * 0.36).clamp(260.0, 360.0);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    ui.allocate_ui_with_layout(
                        egui::vec2(left_width, available.y),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            self.library(ui);
                        },
                    );
                    let right_width = (available.x - left_width - gap).max(200.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(right_width, available.y),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            let session_height = if self.scheduler.active().is_none()
                                && self.scheduler.pending().is_empty()
                            {
                                174.0_f32.min(available.y * 0.42)
                            } else {
                                (available.y * 0.49).clamp(170.0, 340.0)
                            };
                            let details_height = (available.y - session_height - 16.0).max(160.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(right_width, details_height),
                                egui::Layout::top_down(egui::Align::LEFT),
                                |ui| self.details(ui),
                            );
                            ui.add_space(6.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(right_width, session_height),
                                egui::Layout::top_down(egui::Align::LEFT),
                                |ui| self.sessions(ui),
                            );
                        },
                    );
                });
            });
        if !ctx.input(|input| input.viewport().maximized.unwrap_or(false)) {
            egui::Area::new(egui::Id::new("resize_grip"))
                .fixed_pos(ctx.content_rect().right_bottom() - egui::vec2(20.0, 20.0))
                .order(egui::Order::Foreground)
                .movable(false)
                .constrain(false)
                .show(ctx, |ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::drag());
                    let corner = rect.right_bottom() - egui::vec2(2.0, 2.0);
                    for length in [5.0, 10.0] {
                        ui.painter().line_segment(
                            [
                                corner - egui::vec2(length, 0.0),
                                corner - egui::vec2(0.0, length),
                            ],
                            egui::Stroke::new(1.0_f32, MUTED),
                        );
                    }
                    let response = response.on_hover_cursor(egui::CursorIcon::ResizeNwSe);
                    if (response.dragged() || response.drag_stopped())
                        && let Some(pointer) = ctx.pointer_interact_pos()
                    {
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                            (pointer.x + 8.0).max(760.0),
                            (pointer.y + 8.0).max(600.0),
                        )));
                    }
                });
        }
    }
}

fn heading(text: &str, size: f32) -> RichText {
    RichText::new(text)
        .font(FontId::new(size, FontFamily::Name("heading".into())))
        .color(TEXT)
}
fn caption(text: &str) -> RichText {
    RichText::new(text)
        .size(10.0)
        .color(MUTED)
        .extra_letter_spacing(1.0)
}
fn surface() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .corner_radius(14)
        .inner_margin(20)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
}
fn badge(ui: &mut egui::Ui, text: &str, bg: Color32, color: Color32) {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(10.0).color(color));
        });
}
fn action_button(text: &str, stop: bool) -> egui::Button<'_> {
    egui::Button::new(
        RichText::new(text)
            .size(12.0)
            .color(if stop { RED } else { Color32::WHITE }),
    )
    .fill(if stop {
        Color32::from_rgb(61, 36, 46)
    } else {
        Color32::from_rgb(119, 96, 226)
    })
    .stroke(egui::Stroke::new(
        1.0_f32,
        if stop {
            Color32::from_rgb(91, 49, 61)
        } else {
            Color32::from_rgb(145, 123, 250)
        },
    ))
    .corner_radius(8)
}
fn draw_text(
    p: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    size: f32,
    color: Color32,
    width: f32,
    strong: bool,
) {
    let font = FontId::new(
        size,
        if strong {
            FontFamily::Name("heading".into())
        } else {
            FontFamily::Proportional
        },
    );
    let mut job = egui::text::LayoutJob::simple(text.into(), font, color, width.max(1.0));
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    p.galley(pos, p.layout_job(job), color);
}
fn game_tile(p: &egui::Painter, rect: egui::Rect, texture: Option<egui::TextureId>) {
    if let Some(texture) = texture {
        p.image(
            texture,
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        p.rect_filled(rect, 11.0, RAISED);
        gamepad(p, rect.shrink(rect.width() * 0.24), MUTED);
    }
}
fn gamepad(p: &egui::Painter, rect: egui::Rect, color: Color32) {
    let center = rect.center();
    let body = egui::Rect::from_center_size(center, egui::vec2(rect.width(), rect.height() * 0.65));
    p.rect_stroke(
        body,
        9.0,
        egui::Stroke::new(2.0_f32, color),
        egui::StrokeKind::Inside,
    );
    let left = center - egui::vec2(rect.width() * 0.23, 0.0);
    p.line_segment(
        [left - egui::vec2(4.0, 0.0), left + egui::vec2(4.0, 0.0)],
        egui::Stroke::new(2.0_f32, color),
    );
    p.line_segment(
        [left - egui::vec2(0.0, 4.0), left + egui::vec2(0.0, 4.0)],
        egui::Stroke::new(2.0_f32, color),
    );
    p.circle_filled(center + egui::vec2(rect.width() * 0.25, -3.0), 2.0, color);
    p.circle_filled(center + egui::vec2(rect.width() * 0.15, 3.0), 2.0, color);
}
fn number(value: usize) -> String {
    let digits = value.to_string();
    let mut result = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(ch);
    }
    result
}
fn configure_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Ok(data) = std::fs::read("C:/Windows/Fonts/segoeui.ttf") {
        fonts
            .font_data
            .insert("system-ui".into(), egui::FontData::from_owned(data).into());
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "system-ui".into());
    }
    let mut headings = fonts.families[&FontFamily::Proportional].clone();
    if let Ok(data) = std::fs::read("C:/Windows/Fonts/seguisb.ttf") {
        fonts.font_data.insert(
            "system-semibold".into(),
            egui::FontData::from_owned(data).into(),
        );
        headings.insert(0, "system-semibold".into());
    }
    fonts
        .families
        .insert(FontFamily::Name("heading".into()), headings);
    ctx.set_fonts(fonts);
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = SURFACE;
    style.visuals.extreme_bg_color = BG;
    style.visuals.faint_bg_color = RAISED;
    style.visuals.hyperlink_color = ACCENT;
    style.visuals.selection.bg_fill = Color32::from_rgb(76, 61, 128);
    style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    style.visuals.widgets.inactive.bg_fill = RAISED;
    style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(52, 47, 77);
    style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(74, 59, 121);
    for widget in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
    ] {
        widget.corner_radius = 8.into();
    }
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.scroll.bar_width = 5.0;
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(12.0));
    ctx.set_style(style);
}
