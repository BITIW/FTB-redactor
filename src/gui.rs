use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, ComboBox, DragValue, FontId, Id, PointerButton, Pos2, Rect, RichText,
    Sense, Shape, Stroke, StrokeKind, TextEdit, TextureHandle, TextureId, TextureOptions, Vec2,
    vec2,
};
use serde::{Deserialize, Serialize};

use crate::diagnostics::{
    DependencyCycle, cycle_after_adding_dependency, find_broken_dependencies,
    find_dependency_cycles,
};
use crate::editor::{
    EditableBook, IdGenerator, Quest, Reward, Task, Tristate, snap_coordinate,
    translation_display_text, translation_storage_text,
};
use crate::resources::{ResourceIndex, ResourceKind, minecraft_jar_path};
use crate::snbt;

const FTB_UNIT: f32 = 64.0;
const QUEST_SIZE: f32 = 44.0;
const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(5 * 60);
const MAX_LOG_ENTRIES: usize = 1_000;
const MAX_SEARCH_RESULTS: usize = 250;
const APP_STATE_KEY: &str = "ftbgui.app_preferences.v1";

pub fn run(destination: PathBuf) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("FTB Quests GUI Editor")
            .with_inner_size([1400.0, 850.0])
            .with_min_inner_size([980.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "FTB Quests GUI Editor",
        options,
        Box::new(move |creation_context| {
            creation_context.egui_ctx.set_theme(egui::Theme::Dark);
            Ok(Box::new(QuestEditorApp::new(
                destination,
                creation_context.storage,
            )))
        }),
    )
}

struct QuestEditorApp {
    book: EditableBook,
    ids: IdGenerator,
    selected_chapter: usize,
    selected_quest: Option<String>,
    selected_quests: HashSet<String>,
    scene_rect: Rect,
    destination: String,
    saved_destination: Option<PathBuf>,
    status: String,
    cursor_world: Option<(f64, f64)>,
    right_tab: RightTab,
    search_query: String,
    logs: Vec<LogEntry>,
    session_started: Instant,
    autosave_enabled: bool,
    last_autosave: Instant,
    multilingual_mode: bool,
    new_locale: String,
    collapsed_groups: HashSet<String>,
    canvas_tool: CanvasTool,
    snap_enabled: bool,
    connection_start: Option<String>,
    quest_clipboard: Vec<Quest>,
    delete_confirmation: Option<Vec<String>>,
    drag_origins: Option<HashMap<String, (f64, f64)>>,
    drag_accumulated: Vec2,
    selection_drag_start: Option<Pos2>,
    resource_index: ResourceIndex,
    resource_project_root: PathBuf,
    resource_textures: HashMap<String, TextureHandle>,
    failed_resource_textures: HashSet<String>,
    resource_picker: Option<ResourcePickerTarget>,
    resource_query: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum RightTab {
    Inspector,
    Search,
    Logs,
    Languages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum CanvasTool {
    Cursor,
    Connect,
    Select,
    Delete,
}

impl CanvasTool {
    fn title(self) -> &'static str {
        match self {
            Self::Cursor => "Курсор",
            Self::Connect => "Связи",
            Self::Select => "Выделение",
            Self::Delete => "Удаление",
        }
    }
}

#[derive(Debug, Clone)]
enum ResourcePickerTarget {
    QuestIcon { quest_id: String },
    TaskItem { quest_id: String, index: usize },
    TaskEntity { quest_id: String, index: usize },
    RewardItem { quest_id: String, index: usize },
}

impl ResourcePickerTarget {
    fn kind(&self) -> ResourceKind {
        match self {
            Self::TaskEntity { .. } => ResourceKind::Entity,
            _ => ResourceKind::Item,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct AppPreferences {
    destination: String,
    autosave_enabled: bool,
    multilingual_mode: bool,
    active_locale: String,
    selected_chapter_id: Option<String>,
    selected_quest_id: Option<String>,
    scene: [f32; 4],
    right_tab: RightTab,
    collapsed_groups: Vec<String>,
    canvas_tool: CanvasTool,
    snap_enabled: bool,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            destination: String::new(),
            autosave_enabled: true,
            multilingual_mode: false,
            active_locale: "en_us".to_owned(),
            selected_chapter_id: None,
            selected_quest_id: None,
            scene: [0.0, 0.0, 1280.0, 720.0],
            right_tab: RightTab::Inspector,
            collapsed_groups: Vec::new(),
            canvas_tool: CanvasTool::Cursor,
            snap_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum LogLevel {
    Info,
    Warning,
    Error,
}

struct LogEntry {
    elapsed: Duration,
    level: LogLevel,
    message: String,
}

impl QuestEditorApp {
    fn new(destination: PathBuf, storage: Option<&dyn eframe::Storage>) -> Self {
        let preferences = storage
            .and_then(|storage| storage.get_string(APP_STATE_KEY))
            .and_then(|value| serde_json::from_str::<AppPreferences>(&value).ok())
            .unwrap_or_default();
        let destination = if preferences.destination.trim().is_empty() {
            destination
        } else {
            PathBuf::from(&preferences.destination)
        };
        let mut ids = IdGenerator::new();
        let now = Instant::now();
        let loaded = EditableBook::load(&destination).ok().map(|(book, _)| book);
        let restored_project = loaded.is_some();
        let mut book = loaded.unwrap_or_else(|| EditableBook::blank(&mut ids));
        if book
            .translations
            .languages
            .contains_key(&preferences.active_locale)
        {
            book.translations.active_locale = preferences.active_locale.clone();
        }
        let selected_chapter = preferences
            .selected_chapter_id
            .as_deref()
            .and_then(|id| book.chapters.iter().position(|chapter| chapter.id == id))
            .unwrap_or(0);
        let selected_quest = preferences.selected_quest_id.filter(|id| {
            book.chapters
                .get(selected_chapter)
                .is_some_and(|chapter| chapter.quests.iter().any(|quest| quest.id == *id))
        });
        let scene_rect = Rect::from_center_size(
            Pos2::new(preferences.scene[0], preferences.scene[1]),
            vec2(
                preferences.scene[2].max(100.0),
                preferences.scene[3].max(100.0),
            ),
        );
        let resource_project_root = project_root(&destination).to_path_buf();
        let resource_index = ResourceIndex::scan_project(&resource_project_root);
        let selected_quests = selected_quest.iter().cloned().collect();
        let resource_count = resource_index.entries.len();
        let source_count = resource_index.scanned_sources.len();
        Self {
            book,
            ids,
            selected_chapter,
            selected_quest,
            selected_quests,
            scene_rect,
            destination: destination.display().to_string(),
            saved_destination: restored_project.then_some(destination.clone()),
            status: "Двойной клик по полотну создаёт квест.".to_owned(),
            cursor_world: None,
            search_query: String::new(),
            logs: vec![LogEntry {
                elapsed: Duration::ZERO,
                level: LogLevel::Info,
                message: format!(
                    "Редактор запущен. В справочнике {resource_count} записей из {source_count} источников."
                ),
            }],
            session_started: now,
            autosave_enabled: preferences.autosave_enabled,
            last_autosave: now,
            multilingual_mode: preferences.multilingual_mode,
            new_locale: String::new(),
            collapsed_groups: preferences.collapsed_groups.into_iter().collect(),
            canvas_tool: preferences.canvas_tool,
            snap_enabled: preferences.snap_enabled,
            connection_start: None,
            quest_clipboard: Vec::new(),
            delete_confirmation: None,
            drag_origins: None,
            drag_accumulated: Vec2::ZERO,
            selection_drag_start: None,
            resource_index,
            resource_project_root,
            resource_textures: HashMap::new(),
            failed_resource_textures: HashSet::new(),
            resource_picker: None,
            resource_query: String::new(),
            right_tab: preferences.right_tab,
        }
    }

    fn selected_quest_index(&self) -> Option<usize> {
        let id = self.selected_quest.as_deref()?;
        self.book
            .chapters
            .get(self.selected_chapter)?
            .quests
            .iter()
            .position(|quest| quest.id == id)
    }

    fn preferences(&self) -> AppPreferences {
        AppPreferences {
            destination: self.destination.clone(),
            autosave_enabled: self.autosave_enabled,
            multilingual_mode: self.multilingual_mode,
            active_locale: self.book.translations.active_locale.clone(),
            selected_chapter_id: self
                .book
                .chapters
                .get(self.selected_chapter)
                .map(|chapter| chapter.id.clone()),
            selected_quest_id: self.selected_quest.clone(),
            scene: [
                self.scene_rect.center().x,
                self.scene_rect.center().y,
                self.scene_rect.width(),
                self.scene_rect.height(),
            ],
            right_tab: self.right_tab,
            collapsed_groups: self.collapsed_groups.iter().cloned().collect(),
            canvas_tool: self.canvas_tool,
            snap_enabled: self.snap_enabled,
        }
    }

    fn clear_quest_selection(&mut self) {
        self.selected_quest = None;
        self.selected_quests.clear();
        self.drag_origins = None;
        self.drag_accumulated = Vec2::ZERO;
    }

    fn select_only(&mut self, quest_id: String) {
        self.selected_quests.clear();
        self.selected_quests.insert(quest_id.clone());
        self.selected_quest = Some(quest_id);
    }

    fn toggle_selection(&mut self, quest_id: String) {
        if !self.selected_quests.remove(&quest_id) {
            self.selected_quests.insert(quest_id.clone());
            self.selected_quest = Some(quest_id);
        } else if self.selected_quest.as_deref() == Some(&quest_id) {
            self.selected_quest = self.selected_quests.iter().next().cloned();
        }
    }

    fn copy_selected_quests(&mut self) {
        let Some(chapter) = self.book.chapters.get(self.selected_chapter) else {
            return;
        };
        self.quest_clipboard = chapter
            .quests
            .iter()
            .filter(|quest| self.selected_quests.contains(&quest.id))
            .cloned()
            .collect();
        self.status = format!("Скопировано квестов: {}.", self.quest_clipboard.len());
    }

    fn paste_quests(&mut self) {
        if self.quest_clipboard.is_empty() {
            self.status = "Буфер квестов пуст.".to_owned();
            return;
        }
        let min_x = self
            .quest_clipboard
            .iter()
            .map(|quest| quest.x)
            .fold(f64::INFINITY, f64::min);
        let min_y = self
            .quest_clipboard
            .iter()
            .map(|quest| quest.y)
            .fold(f64::INFINITY, f64::min);
        let offset = self
            .cursor_world
            .map(|(x, y)| (x - min_x, y - min_y))
            .unwrap_or((self.book.grid_scale.max(0.5), self.book.grid_scale.max(0.5)));
        let new_ids = self.book.duplicate_quests(
            self.selected_chapter,
            &self.quest_clipboard,
            offset,
            &mut self.ids,
        );
        self.selected_quests = new_ids.iter().cloned().collect();
        self.selected_quest = new_ids.last().cloned();
        self.snap_selected();
        self.status = format!("Вставлено квестов: {}.", new_ids.len());
    }

    fn request_delete_selected(&mut self) {
        if !self.selected_quests.is_empty() {
            self.delete_confirmation = Some(self.selected_quests.iter().cloned().collect());
        }
    }

    fn delete_quests(&mut self, ids: &[String]) {
        let removed = ids.iter().cloned().collect::<HashSet<_>>();
        for chapter in &mut self.book.chapters {
            chapter.quests.retain(|quest| !removed.contains(&quest.id));
            for quest in &mut chapter.quests {
                quest
                    .dependencies
                    .retain(|dependency| !removed.contains(dependency));
            }
        }
        self.clear_quest_selection();
        self.connection_start = None;
        self.status = format!("Удалено квестов: {}.", removed.len());
    }

    fn snap_selected(&mut self) {
        let Some(chapter) = self.book.chapters.get_mut(self.selected_chapter) else {
            return;
        };
        for quest in &mut chapter.quests {
            if self.selected_quests.contains(&quest.id) {
                quest.x = snap_coordinate(quest.x, self.book.grid_scale, quest.size);
                quest.y = snap_coordinate(quest.y, self.book.grid_scale, quest.size);
            }
        }
    }

    fn handle_shortcuts(&mut self, context: &egui::Context) {
        if context.egui_wants_keyboard_input() {
            return;
        }
        let mut copy = false;
        let mut paste = false;
        let mut delete = false;
        context.input_mut(|input| {
            copy = input.consume_key(egui::Modifiers::COMMAND, egui::Key::C);
            paste = input.consume_key(egui::Modifiers::COMMAND, egui::Key::V);
            delete = input.consume_key(egui::Modifiers::NONE, egui::Key::Delete);
            for (key, tool) in [
                (egui::Key::Num1, CanvasTool::Cursor),
                (egui::Key::Num2, CanvasTool::Connect),
                (egui::Key::Num3, CanvasTool::Select),
                (egui::Key::Num4, CanvasTool::Delete),
            ] {
                if input.consume_key(egui::Modifiers::COMMAND, key) {
                    self.canvas_tool = tool;
                    self.connection_start = None;
                }
            }
        });
        if copy {
            self.copy_selected_quests();
        }
        if paste {
            self.paste_quests();
        }
        if delete {
            self.request_delete_selected();
        }
    }

    fn reset_project(&mut self) {
        self.book = EditableBook::blank(&mut self.ids);
        self.selected_chapter = 0;
        self.clear_quest_selection();
        self.scene_rect = Rect::from_center_size(Pos2::ZERO, vec2(1280.0, 720.0));
        self.saved_destination = None;
        self.resource_project_root =
            project_root(std::path::Path::new(&self.destination)).to_path_buf();
        self.resource_index = ResourceIndex::scan_project(&self.resource_project_root);
        self.clear_resource_texture_cache();
        self.status = "Создан новый проект.".to_owned();
        self.last_autosave = Instant::now();
        self.push_log(LogLevel::Info, "Создан новый проект.");
    }

    fn import_project(&mut self, source: PathBuf) {
        match EditableBook::load(&source) {
            Ok((book, report)) => {
                let quest_count = book
                    .chapters
                    .iter()
                    .map(|chapter| chapter.quests.len())
                    .sum::<usize>();
                let destination = import_destination(&source);
                self.book = book;
                self.selected_chapter = 0;
                self.clear_quest_selection();
                self.scene_rect = Rect::from_center_size(Pos2::ZERO, vec2(1280.0, 720.0));
                self.destination = destination.display().to_string();
                self.saved_destination = None;
                self.resource_project_root = project_root(&source).to_path_buf();
                self.resource_index = ResourceIndex::scan_project(&self.resource_project_root);
                self.clear_resource_texture_cache();
                self.last_autosave = Instant::now();
                self.status = format!(
                    "Импортировано: {} групп, {} глав, {} квестов, {} языков. Экспорт: {}",
                    self.book.groups.len(),
                    self.book.chapters.len(),
                    quest_count,
                    report.imported_languages,
                    destination.display()
                );
                if report.preserved_total() > 0 {
                    self.status.push_str(&format!(
                        " Сохранено без потерь в режиме только чтения: {} задач, {} наград.",
                        report.preserved_tasks, report.preserved_rewards
                    ));
                }
                self.push_log(LogLevel::Info, self.status.clone());
                self.run_diagnostics("Диагностика после импорта");
            }
            Err(error) => {
                self.status = format!("Ошибка импорта: {error}");
                self.push_log(LogLevel::Error, self.status.clone());
            }
        }
    }

    fn choose_import_folder(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Выберите папку проекта FTB Quests")
            .pick_folder()
        {
            self.import_project(path);
        }
    }

    fn choose_destination(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Выберите папку для сохранения")
            .pick_folder()
        {
            self.destination = path.display().to_string();
            self.saved_destination = None;
            self.resource_project_root = project_root(&path).to_path_buf();
            self.resource_index = ResourceIndex::scan_project(&self.resource_project_root);
            self.clear_resource_texture_cache();
            self.status = format!("Папка сохранения: {}", path.display());
            self.last_autosave = Instant::now();
            self.push_log(LogLevel::Info, self.status.clone());
        }
    }

    fn save_project(&mut self) {
        self.run_diagnostics("Диагностика перед сохранением");
        let destination = PathBuf::from(self.destination.trim());
        if self.destination.trim().is_empty() {
            self.status = "Укажите папку для сохранения.".to_owned();
            self.push_log(LogLevel::Error, self.status.clone());
            return;
        }

        let quests_root = if destination.file_name().is_some_and(|name| name == "quests") {
            destination.clone()
        } else {
            destination.join("quests")
        };
        let already_exists = quests_root.join("data.snbt").exists();
        let is_our_destination = self.saved_destination.as_ref() == Some(&destination);
        if already_exists && !is_our_destination {
            self.status = format!(
                "Сохранение отменено: {} уже содержит книгу. Выберите пустую папку.",
                quests_root.display()
            );
            self.push_log(LogLevel::Error, self.status.clone());
            return;
        }

        match self.book.save(&destination) {
            Ok(()) => {
                self.saved_destination = Some(destination);
                self.status = format!(
                    "Сохранено: {} групп, {} глав, {} квестов.",
                    self.book.groups.len(),
                    self.book.chapters.len(),
                    self.book
                        .chapters
                        .iter()
                        .map(|chapter| chapter.quests.len())
                        .sum::<usize>()
                );
                self.push_log(LogLevel::Info, self.status.clone());
            }
            Err(error) => {
                self.status = format!("Ошибка сохранения: {error}");
                self.push_log(LogLevel::Error, self.status.clone());
            }
        }
    }

    fn push_log(&mut self, level: LogLevel, message: impl Into<String>) {
        if self.logs.len() >= MAX_LOG_ENTRIES {
            self.logs.remove(0);
        }
        self.logs.push(LogEntry {
            elapsed: self.session_started.elapsed(),
            level,
            message: message.into(),
        });
    }

    fn run_diagnostics(&mut self, reason: &str) {
        let cycles = find_dependency_cycles(&self.book);
        let broken = find_broken_dependencies(&self.book);
        let level = if cycles.is_empty() && broken.is_empty() {
            LogLevel::Info
        } else {
            LogLevel::Warning
        };
        self.push_log(
            level,
            format!(
                "{reason}: циклов — {}, битых зависимостей — {}.",
                cycles.len(),
                broken.len()
            ),
        );

        for cycle in cycles {
            self.log_cycle(&cycle, "Обнаружен цикл");
        }
        for dependency in broken {
            self.push_log(
                LogLevel::Warning,
                format!(
                    "Битая зависимость: {} ссылается на отсутствующий квест {}. \
                     Исправление: удалите эту зависимость или восстановите квест с данным ID.",
                    self.quest_label(&dependency.quest_id),
                    dependency.missing_quest_id
                ),
            );
        }
    }

    fn log_cycle(&mut self, cycle: &DependencyCycle, prefix: &str) {
        let route = format_cycle_route(cycle, |id| self.quest_label(id));
        let suggestion = cycle
            .suggested_edge()
            .map(|(from, to)| {
                format!(
                    "Исправление: удалите одну зависимость в цикле, например {} → {}.",
                    self.quest_label(from),
                    self.quest_label(to)
                )
            })
            .unwrap_or_else(|| "Исправление: удалите циклическую зависимость.".to_owned());
        self.push_log(
            LogLevel::Warning,
            format!(
                "{prefix}, длина пути: {}. Маршрут: {route}. {suggestion}",
                cycle.length()
            ),
        );
    }

    fn quest_label(&self, quest_id: &str) -> String {
        self.book
            .chapters
            .iter()
            .flat_map(|chapter| chapter.quests.iter())
            .find(|quest| quest.id == quest_id)
            .map(|quest| {
                format!(
                    "{} [{}]",
                    self.book.translations.resolve(&quest.title),
                    quest.id
                )
            })
            .unwrap_or_else(|| format!("[{quest_id}]"))
    }

    fn tick_autosave(&mut self, context: &egui::Context) {
        if !self.autosave_enabled {
            return;
        }
        let elapsed = self.last_autosave.elapsed();
        if elapsed >= AUTOSAVE_INTERVAL {
            self.last_autosave = Instant::now();
            self.autosave();
        } else {
            context.request_repaint_after(AUTOSAVE_INTERVAL - elapsed);
        }
    }

    fn autosave(&mut self) {
        let destination = PathBuf::from(self.destination.trim());
        if self.destination.trim().is_empty() {
            self.push_log(
                LogLevel::Error,
                "Автосохранение пропущено: не указана папка проекта.",
            );
            return;
        }
        let autosave = autosave_destination(&destination);
        match save_autosave_snapshot(&self.book, &autosave) {
            Ok(()) => {
                self.status = format!("Автосохранение: {}", autosave.display());
                self.push_log(LogLevel::Info, self.status.clone());
            }
            Err(error) => {
                self.status = format!("Ошибка автосохранения: {error}");
                self.push_log(LogLevel::Error, self.status.clone());
            }
        }
    }

    fn top_bar(&mut self, root_ui: &mut egui::Ui) {
        egui::Panel::top("top_bar").show_inside(root_ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("FTB Quests Editor");
                ui.separator();
                if ui.button("Новый проект").clicked() {
                    self.reset_project();
                }
                if ui.button("Импорт проекта…").clicked() {
                    self.choose_import_folder();
                }
                if ui.button("Сохранить").clicked() {
                    self.save_project();
                }
                if ui.button("Сохранить как…").clicked() {
                    self.choose_destination();
                }
                if ui.button("Центрировать").clicked() {
                    self.scene_rect = Rect::from_center_size(Pos2::ZERO, vec2(1280.0, 720.0));
                }
                if ui.button("Поиск").clicked() {
                    self.right_tab = RightTab::Search;
                }
                if ui.button("Логи").clicked() {
                    self.right_tab = RightTab::Logs;
                }
                if ui.button("Языки").clicked() {
                    self.right_tab = RightTab::Languages;
                }
            });
            ui.horizontal(|ui| {
                ui.separator();
                ui.strong("Инструменты:");
                for (tool, shortcut) in [
                    (CanvasTool::Cursor, "Ctrl+1"),
                    (CanvasTool::Connect, "Ctrl+2"),
                    (CanvasTool::Select, "Ctrl+3"),
                    (CanvasTool::Delete, "Ctrl+4"),
                ] {
                    if ui
                        .selectable_label(self.canvas_tool == tool, tool.title())
                        .on_hover_text(shortcut)
                        .clicked()
                    {
                        self.canvas_tool = tool;
                        self.connection_start = None;
                    }
                }
                ui.separator();
                if ui
                    .add_enabled(
                        !self.selected_quests.is_empty(),
                        egui::Button::new("Копировать"),
                    )
                    .on_hover_text("Ctrl+C")
                    .clicked()
                {
                    self.copy_selected_quests();
                }
                if ui
                    .add_enabled(
                        !self.quest_clipboard.is_empty(),
                        egui::Button::new("Вставить"),
                    )
                    .on_hover_text("Ctrl+V")
                    .clicked()
                {
                    self.paste_quests();
                }
                if ui
                    .add_enabled(
                        !self.selected_quests.is_empty(),
                        egui::Button::new("К сетке"),
                    )
                    .clicked()
                {
                    self.snap_selected();
                }
                ui.checkbox(&mut self.snap_enabled, "Snap");
            });
            ui.horizontal(|ui| {
                ui.separator();
                ui.label("Папка:");
                ui.add(
                    TextEdit::singleline(&mut self.destination)
                        .desired_width(260.0)
                        .hint_text("./my_ftbquests"),
                );
                ui.separator();
                ui.label("Сетка:");
                ui.add(
                    DragValue::new(&mut self.book.grid_scale)
                        .range(0.05..=4.0)
                        .speed(0.05),
                )
                .on_hover_text("FTB grid_scale. При 0.5 обычные квесты привязываются с шагом 0.5.");
                ui.separator();
                let changed = ui
                    .checkbox(&mut self.autosave_enabled, "Автосохранение каждые 5 минут")
                    .changed();
                if changed {
                    self.last_autosave = Instant::now();
                    let state = if self.autosave_enabled {
                        "включено"
                    } else {
                        "выключено"
                    };
                    self.push_log(LogLevel::Info, format!("Автосохранение {state}."));
                }
            });
        });
    }

    fn left_panel(&mut self, root_ui: &mut egui::Ui) {
        egui::Panel::left("book_tree")
            .resizable(true)
            .default_size(250.0)
            .size_range(210.0..=360.0)
            .show_inside(root_ui, |ui| {
                ui.heading("Книга");
                ui.label("Название");
                localized_singleline(
                    ui,
                    &mut self.book.translations,
                    &mut self.book.title,
                    self.multilingual_mode,
                    220.0,
                    None,
                );
                ui.separator();

                ui.horizontal(|ui| {
                    ui.strong("Группы и главы");
                    if ui.small_button("+ группа").clicked() {
                        self.book
                            .add_group("Новая группа".to_owned(), &mut self.ids);
                    }
                });

                egui::ScrollArea::vertical()
                    .id_salt("book_tree_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.small("Название редактируется прямо в списке. Тяните главу за ≡.");
                        let groups = self.book.groups.clone();
                        let mut remove_group = None;
                        for (group_index, group) in groups.iter().enumerate() {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    let collapsed = self.collapsed_groups.contains(&group.id);
                                    if ui
                                        .small_button(if collapsed { "▶" } else { "▼" })
                                        .clicked()
                                        && !self.collapsed_groups.remove(&group.id)
                                    {
                                        self.collapsed_groups.insert(group.id.clone());
                                    }
                                    ui.label("Группа:");
                                    localized_singleline(
                                        ui,
                                        &mut self.book.translations,
                                        &mut self.book.groups[group_index].title,
                                        self.multilingual_mode,
                                        140.0,
                                        None,
                                    );
                                    if ui.small_button("Удалить").clicked() {
                                        remove_group = Some(group.id.clone());
                                    }
                                });
                                let collapsed = self.collapsed_groups.contains(&group.id);
                                let (_, dropped) = ui.dnd_drop_zone::<String, _>(
                                    egui::Frame::group(ui.style()),
                                    |ui| {
                                        if collapsed {
                                            ui.small("Группа свернута. Главу всё равно можно перетащить сюда.");
                                            return;
                                        }
                                        let chapter_indices = self
                                            .book
                                            .chapters
                                            .iter()
                                            .enumerate()
                                            .filter_map(|(index, chapter)| {
                                                (chapter.group.as_deref()
                                                    == Some(group.id.as_str()))
                                                .then_some(index)
                                            })
                                            .collect::<Vec<_>>();
                                        for chapter_index in chapter_indices {
                                            self.chapter_button(ui, chapter_index);
                                        }
                                        if ui.small_button("+ глава").clicked() {
                                            self.selected_chapter = self.book.add_chapter(
                                                "Новая глава".to_owned(),
                                                Some(group.id.clone()),
                                                &mut self.ids,
                                            );
                                            self.clear_quest_selection();
                                        }
                                    },
                                );
                                if let Some(chapter_id) = dropped {
                                    self.move_chapter_to_group(
                                        chapter_id.as_str(),
                                        Some(&group.id),
                                    );
                                }
                            });
                        }
                        if let Some(removed_id) = remove_group {
                            self.book.groups.retain(|group| group.id != removed_id);
                            for chapter in &mut self.book.chapters {
                                if chapter.group.as_deref() == Some(&removed_id) {
                                    chapter.group = None;
                                }
                            }
                        }

                        ui.separator();
                        ui.strong("Без группы");
                        let (_, dropped) =
                            ui.dnd_drop_zone::<String, _>(egui::Frame::group(ui.style()), |ui| {
                                let ungrouped = self
                                    .book
                                    .chapters
                                    .iter()
                                    .enumerate()
                                    .filter_map(|(index, chapter)| {
                                        chapter.group.is_none().then_some(index)
                                    })
                                    .collect::<Vec<_>>();
                                for chapter_index in ungrouped {
                                    self.chapter_button(ui, chapter_index);
                                }
                                if ui.small_button("+ глава без группы").clicked() {
                                    self.selected_chapter = self.book.add_chapter(
                                        "Новая глава".to_owned(),
                                        None,
                                        &mut self.ids,
                                    );
                                    self.clear_quest_selection();
                                }
                            });
                        if let Some(chapter_id) = dropped {
                            self.move_chapter_to_group(chapter_id.as_str(), None);
                        }

                        ui.separator();
                        ui.small(
                            "Главы экспортируются отдельными файлами. Группы задают порядок \
                             разделов в левой панели Minecraft.",
                        );
                    });
            });
    }

    fn chapter_button(&mut self, ui: &mut egui::Ui, chapter_index: usize) {
        let chapter_id = self.book.chapters[chapter_index].id.clone();
        let quest_count = self.book.chapters[chapter_index].quests.len();
        ui.horizontal(|ui| {
            ui.dnd_drag_source(Id::new(("chapter_drag", &chapter_id)), chapter_id, |ui| {
                ui.label(RichText::new("≡").strong());
            })
            .response
            .on_hover_text("Перетащить главу в другую группу");
            let selected = self.selected_chapter == chapter_index;
            let response = localized_singleline(
                ui,
                &mut self.book.translations,
                &mut self.book.chapters[chapter_index].title,
                self.multilingual_mode,
                145.0,
                Some(if selected {
                    ui.visuals().selection.bg_fill
                } else {
                    Color32::TRANSPARENT
                }),
            );
            if response.clicked() {
                self.selected_chapter = chapter_index;
                self.clear_quest_selection();
            }
            ui.small(format!("({quest_count})"));
        });
    }

    fn move_chapter_to_group(&mut self, chapter_id: &str, group_id: Option<&str>) {
        let Some(chapter_index) = self
            .book
            .chapters
            .iter()
            .position(|chapter| chapter.id == chapter_id)
        else {
            return;
        };
        self.book.chapters[chapter_index].group = group_id.map(str::to_owned);
        let chapter_title = self
            .book
            .translations
            .resolve(&self.book.chapters[chapter_index].title);
        self.status = match group_id {
            Some(group_id) => {
                let title = self
                    .book
                    .groups
                    .iter()
                    .find(|group| group.id == group_id)
                    .map(|group| group_display_title(&self.book.translations, &group.title))
                    .unwrap_or_else(|| "неизвестную группу".to_owned());
                format!("Глава «{chapter_title}» перенесена в «{title}».")
            }
            None => format!("Глава «{chapter_title}» перенесена в раздел без группы."),
        };
    }

    fn right_panel(&mut self, root_ui: &mut egui::Ui) {
        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(370.0)
            .size_range(300.0..=560.0)
            .show_inside(root_ui, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.right_tab, RightTab::Inspector, "Инспектор");
                    ui.selectable_value(&mut self.right_tab, RightTab::Search, "Поиск");
                    ui.selectable_value(&mut self.right_tab, RightTab::Logs, "Логи");
                    ui.selectable_value(&mut self.right_tab, RightTab::Languages, "Языки");
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| match self.right_tab {
                    RightTab::Inspector => {
                        if let Some(quest_index) = self.selected_quest_index() {
                            self.quest_inspector(ui, quest_index);
                        } else {
                            self.chapter_inspector(ui);
                        }
                    }
                    RightTab::Search => self.search_panel(ui),
                    RightTab::Logs => self.logs_panel(ui),
                    RightTab::Languages => self.languages_panel(ui),
                });
            });
    }

    fn languages_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Локализация KubeJS");
        ui.checkbox(
            &mut self.multilingual_mode,
            "Предлагать локализацию обычного текста",
        );
        if ui.button("Локализовать всю книгу").clicked() {
            let count = self.localize_entire_book();
            self.status = format!(
                "Создано {count} ключей для языка {}.",
                self.book.translations.active_locale
            );
            self.push_log(LogLevel::Info, self.status.clone());
        }
        ui.small("Ключи остаются в SNBT, а редактор показывает и изменяет текст выбранного языка.");
        ui.separator();

        ui.label("Текущий язык");
        let locales = self
            .book
            .translations
            .languages
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        ComboBox::from_id_salt("active_translation_locale")
            .selected_text(&self.book.translations.active_locale)
            .show_ui(ui, |ui| {
                for locale in locales {
                    ui.selectable_value(
                        &mut self.book.translations.active_locale,
                        locale.clone(),
                        locale,
                    );
                }
            });
        let active_count = self
            .book
            .translations
            .languages
            .get(&self.book.translations.active_locale)
            .map(BTreeMap::len)
            .unwrap_or(0);
        ui.small(format!(
            "{} языков, {} строк в выбранном.",
            self.book.translations.languages.len(),
            active_count
        ));

        ui.separator();
        ui.label("Добавить язык, например ru_ru");
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.new_locale);
            if ui.button("Добавить").clicked() {
                if self.book.translations.add_locale(&self.new_locale) {
                    self.status =
                        format!("Добавлен язык {}.", self.book.translations.active_locale);
                    self.new_locale.clear();
                } else {
                    self.status = "Язык уже существует или код локали некорректен.".to_owned();
                }
            }
        });
        ui.separator();
        ui.small(
            "В многоязычном режиме рядом с обычными строками появляется кнопка \
             «Локализовать». Она создаёт UUID v4 и переносит текущий текст в выбранный язык.",
        );
    }

    fn localize_entire_book(&mut self) -> usize {
        let mut count = 0;
        let book = &mut self.book;
        count += localize_nonempty(&mut book.translations, &mut book.title);
        for group in &mut book.groups {
            count += localize_nonempty(&mut book.translations, &mut group.title);
        }
        for chapter in &mut book.chapters {
            count += localize_nonempty(&mut book.translations, &mut chapter.title);
            for quest in &mut chapter.quests {
                count += localize_nonempty(&mut book.translations, &mut quest.title);
                count += localize_nonempty(&mut book.translations, &mut quest.description);
                count += localize_nonempty(&mut book.translations, &mut quest.settings.subtitle);
                for task in &mut quest.tasks {
                    match task {
                        Task::Checkmark { title, .. }
                        | Task::Item { title, .. }
                        | Task::Kill { title, .. } => {
                            count += localize_nonempty(&mut book.translations, title);
                        }
                        Task::Unsupported { .. } => {}
                    }
                }
            }
        }
        count
    }

    fn search_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Поиск по книге");
        let response = ui.add(
            TextEdit::singleline(&mut self.search_query)
                .hint_text("ID, название, описание, предмет…")
                .desired_width(f32::INFINITY),
        );
        if response.gained_focus() {
            response.request_focus();
        }
        if self.search_query.trim().is_empty() {
            ui.small(
                "Поиск охватывает квесты, главы, группы, описания, ID, иконки, \
                 задачи, предметы и сущности.",
            );
            return;
        }

        let (hits, total) = search_book(&self.book, &self.search_query);
        ui.label(format!(
            "Найдено: {total}{}",
            if total > hits.len() {
                format!(", показаны первые {}", hits.len())
            } else {
                String::new()
            }
        ));
        ui.separator();
        for hit in hits {
            let selected = match &hit.target {
                SearchTarget::Quest {
                    chapter_index,
                    quest_id,
                } => {
                    self.selected_chapter == *chapter_index
                        && self.selected_quest.as_deref() == Some(quest_id)
                }
                SearchTarget::Chapter { chapter_index } => {
                    self.selected_chapter == *chapter_index && self.selected_quest.is_none()
                }
                SearchTarget::Group { .. } => false,
            };
            if ui.selectable_label(selected, &hit.title).clicked() {
                self.open_search_target(hit.target);
            }
            ui.small(RichText::new(hit.subtitle).color(Color32::GRAY));
            ui.add_space(4.0);
        }
    }

    fn open_search_target(&mut self, target: SearchTarget) {
        match target {
            SearchTarget::Quest {
                chapter_index,
                quest_id,
            } => {
                self.selected_chapter = chapter_index;
                self.select_only(quest_id.clone());
                self.right_tab = RightTab::Inspector;
                if let Some(quest) = self.book.chapters[chapter_index]
                    .quests
                    .iter()
                    .find(|quest| quest.id == quest_id)
                {
                    self.scene_rect = Rect::from_center_size(
                        Pos2::new(quest.x as f32 * FTB_UNIT, quest.y as f32 * FTB_UNIT),
                        self.scene_rect.size(),
                    );
                }
            }
            SearchTarget::Chapter { chapter_index } => {
                self.selected_chapter = chapter_index;
                self.clear_quest_selection();
                self.right_tab = RightTab::Inspector;
            }
            SearchTarget::Group { group_id } => {
                if let Some(chapter_index) = self
                    .book
                    .chapters
                    .iter()
                    .position(|chapter| chapter.group.as_deref() == Some(group_id.as_str()))
                {
                    self.selected_chapter = chapter_index;
                    self.clear_quest_selection();
                    self.right_tab = RightTab::Inspector;
                }
            }
        }
    }

    fn logs_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Логи");
            if ui.button("Проверить зависимости").clicked() {
                self.run_diagnostics("Ручная диагностика");
            }
            if ui.button("Очистить").clicked() {
                self.logs.clear();
            }
        });
        if self.autosave_enabled {
            let remaining = AUTOSAVE_INTERVAL.saturating_sub(self.last_autosave.elapsed());
            ui.small(format!(
                "Следующее автосохранение через {}:{:02}. Текущий снимок: {}",
                remaining.as_secs() / 60,
                remaining.as_secs() % 60,
                autosave_destination(std::path::Path::new(self.destination.trim())).display()
            ));
        } else {
            ui.small("Автосохранение выключено.");
        }
        ui.separator();
        if self.logs.is_empty() {
            ui.small("Журнал пуст.");
        }
        for entry in self.logs.iter().rev() {
            let seconds = entry.elapsed.as_secs();
            let color = match entry.level {
                LogLevel::Info => Color32::LIGHT_GRAY,
                LogLevel::Warning => Color32::YELLOW,
                LogLevel::Error => Color32::LIGHT_RED,
            };
            ui.colored_label(
                color,
                format!(
                    "[{:02}:{:02}] {}",
                    seconds / 60,
                    seconds % 60,
                    entry.message
                ),
            );
            ui.add_space(3.0);
        }
    }

    fn chapter_inspector(&mut self, ui: &mut egui::Ui) {
        ui.heading("Свойства главы");
        if self.selected_chapter >= self.book.chapters.len() {
            return;
        }

        let group_options = self
            .book
            .groups
            .iter()
            .map(|group| {
                (
                    group.id.clone(),
                    group_display_title(&self.book.translations, &group.title),
                    group.title.clone(),
                )
            })
            .collect::<Vec<_>>();
        let chapter = &mut self.book.chapters[self.selected_chapter];
        ui.label("Название");
        localized_singleline(
            ui,
            &mut self.book.translations,
            &mut chapter.title,
            self.multilingual_mode,
            f32::INFINITY,
            None,
        );
        ui.label("Имя SNBT-файла");
        ui.text_edit_singleline(&mut chapter.filename);
        ui.label("ID");
        ui.monospace(&chapter.id);
        ui.label("Группа");
        let selected_group_title = chapter
            .group
            .as_deref()
            .and_then(|id| {
                group_options
                    .iter()
                    .find(|(group_id, _, _)| group_id == id)
                    .map(|(_, title, _)| title.as_str())
            })
            .unwrap_or("Без группы");
        ComboBox::from_id_salt("chapter_group")
            .selected_text(selected_group_title)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut chapter.group, None, "Без группы");
                for (id, title, raw_title) in &group_options {
                    ui.selectable_value(&mut chapter.group, Some(id.clone()), title)
                        .on_hover_text(format!("ID: {id}\nИсходное название: {raw_title}"));
                }
            });

        ui.separator();
        if ui.button("Добавить квест в (0, 0)").clicked() {
            self.book
                .add_quest(self.selected_chapter, (0.0, 0.0), &mut self.ids);
            if let Some(id) = self.book.chapters[self.selected_chapter]
                .quests
                .last()
                .map(|quest| quest.id.clone())
            {
                self.select_only(id);
            }
        }
        if self.book.chapters.len() > 1
            && ui
                .button(RichText::new("Удалить главу").color(Color32::LIGHT_RED))
                .clicked()
        {
            self.book.chapters.remove(self.selected_chapter);
            self.selected_chapter = self
                .selected_chapter
                .min(self.book.chapters.len().saturating_sub(1));
            self.clear_quest_selection();
        }
    }

    fn quest_inspector(&mut self, ui: &mut egui::Ui, quest_index: usize) {
        let chapter_index = self.selected_chapter;
        let quest_id = self.book.chapters[chapter_index].quests[quest_index]
            .id
            .clone();
        ui.heading("Квест");
        let mut open_resource_picker = None;

        {
            let quest = &mut self.book.chapters[chapter_index].quests[quest_index];
            ui.label("Название");
            localized_singleline(
                ui,
                &mut self.book.translations,
                &mut quest.title,
                self.multilingual_mode,
                f32::INFINITY,
                None,
            );
            ui.label("Описание");
            localized_multiline(
                ui,
                &mut self.book.translations,
                &mut quest.description,
                self.multilingual_mode,
                3,
            );
            ui.label("ID");
            ui.monospace(&quest.id);

            ui.horizontal(|ui| {
                ui.label("X");
                ui.add(DragValue::new(&mut quest.x).speed(0.25));
                ui.label("Y");
                ui.add(DragValue::new(&mut quest.y).speed(0.25));
            });
            ui.horizontal(|ui| {
                ui.label("Размер");
                ui.add(DragValue::new(&mut quest.size).range(0.25..=8.0).speed(0.1));
                ComboBox::from_id_salt("quest_shape")
                    .selected_text(if quest.shape.is_empty() {
                        "default"
                    } else {
                        &quest.shape
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut quest.shape, String::new(), "default");
                        for shape in [
                            "circle", "square", "rsquare", "diamond", "hexagon", "octagon",
                            "pentagon", "heart", "gear",
                        ] {
                            ui.selectable_value(&mut quest.shape, shape.to_owned(), shape);
                        }
                    });
            });
            ui.label("Иконка (ID предмета)");
            ui.horizontal(|ui| {
                let response = ui.text_edit_singleline(&mut quest.icon);
                if response.changed() || ui.small_button("Справочник…").clicked() {
                    open_resource_picker = Some((
                        ResourcePickerTarget::QuestIcon {
                            quest_id: quest_id.clone(),
                        },
                        quest.icon.clone(),
                    ));
                }
            });
        }
        if let Some((target, query)) = open_resource_picker {
            self.resource_picker = Some(target);
            self.resource_query = query;
        }

        self.quest_settings_editor(ui, chapter_index, quest_index);
        ui.separator();
        self.tasks_editor(ui, chapter_index, quest_index);
        ui.separator();
        self.rewards_editor(ui, chapter_index, quest_index);
        ui.separator();
        self.dependencies_editor(ui, chapter_index, quest_index);
        ui.separator();

        if ui
            .button(RichText::new("Удалить квест").color(Color32::LIGHT_RED))
            .clicked()
        {
            self.delete_confirmation = Some(vec![quest_id]);
        }
    }

    fn quest_settings_editor(
        &mut self,
        ui: &mut egui::Ui,
        chapter_index: usize,
        quest_index: usize,
    ) {
        let settings = &mut self.book.chapters[chapter_index].quests[quest_index].settings;
        egui::CollapsingHeader::new("Расширенные настройки FTB")
            .default_open(false)
            .show(ui, |ui| {
                ui.label("Подзаголовок");
                localized_singleline(
                    ui,
                    &mut self.book.translations,
                    &mut settings.subtitle,
                    self.multilingual_mode,
                    f32::INFINITY,
                    None,
                );
                ui.label("Теги через запятую");
                let mut tags = settings.tags.join(", ");
                if ui.text_edit_singleline(&mut tags).changed() {
                    settings.tags = tags
                        .split(',')
                        .map(str::trim)
                        .filter(|tag| !tag.is_empty())
                        .map(str::to_owned)
                        .collect();
                }

                ui.separator();
                ui.strong("Внешний вид");
                ui.horizontal(|ui| {
                    ui.label("Масштаб иконки");
                    ui.add(
                        DragValue::new(&mut settings.icon_scale)
                            .range(0.1..=2.0)
                            .speed(0.05),
                    );
                    ui.label("Мин. ширина");
                    ui.add(DragValue::new(&mut settings.min_width).range(0..=3000));
                });

                ui.separator();
                ui.strong("Видимость");
                tristate_editor(
                    ui,
                    "hide_until_deps_visible",
                    "До видимости зависимостей",
                    &mut settings.hide_until_deps_visible,
                );
                tristate_editor(
                    ui,
                    "hide_until_deps_complete",
                    "До завершения зависимостей",
                    &mut settings.hide_until_deps_complete,
                );
                tristate_editor(
                    ui,
                    "hide_details_until_startable",
                    "Скрывать детали до старта",
                    &mut settings.hide_details_until_startable,
                );
                tristate_editor(
                    ui,
                    "hide_text_until_complete",
                    "Скрывать текст до завершения",
                    &mut settings.hide_text_until_complete,
                );
                ui.checkbox(&mut settings.invisible, "Невидимый до завершения");
                if settings.invisible {
                    ui.add(
                        DragValue::new(&mut settings.invisible_until_tasks)
                            .prefix("Показать после задач: "),
                    );
                }
                ui.checkbox(&mut settings.hide_lock_icon, "Скрывать значок замка");

                ui.separator();
                ui.strong("Зависимости");
                ComboBox::from_id_salt("dependency_requirement")
                    .selected_text(&settings.dependency_requirement)
                    .show_ui(ui, |ui| {
                        for (id, title) in [
                            ("all_completed", "Все завершены"),
                            ("one_completed", "Одна завершена"),
                            ("all_started", "Все начаты"),
                            ("one_started", "Одна начата"),
                        ] {
                            ui.selectable_value(
                                &mut settings.dependency_requirement,
                                id.to_owned(),
                                title,
                            );
                        }
                    });
                ui.horizontal(|ui| {
                    ui.add(
                        DragValue::new(&mut settings.min_required_dependencies).prefix("Минимум: "),
                    );
                    ui.add(
                        DragValue::new(&mut settings.max_completable_dependents)
                            .prefix("Макс. зависимых: "),
                    );
                });
                tristate_editor(
                    ui,
                    "hide_dependency_lines",
                    "Скрывать линии зависимостей",
                    &mut settings.hide_dependency_lines,
                );
                ui.checkbox(
                    &mut settings.hide_dependent_lines,
                    "Скрывать линии зависимых квестов",
                );

                ui.separator();
                ui.strong("Поведение");
                ui.checkbox(&mut settings.optional, "Необязательный квест");
                tristate_editor(
                    ui,
                    "can_repeat",
                    "Повторяемый квест",
                    &mut settings.can_repeat,
                );
                if settings.can_repeat == Tristate::True {
                    ui.add(
                        DragValue::new(&mut settings.repeat_cooldown).prefix("Перерыв, секунд: "),
                    );
                }
                tristate_editor(
                    ui,
                    "require_sequential_tasks",
                    "Выполнять задачи последовательно",
                    &mut settings.require_sequential_tasks,
                );
                ui.checkbox(
                    &mut settings.ignore_reward_blocking,
                    "Игнорировать блокировку наград",
                );
                tristate_editor(
                    ui,
                    "disable_recipe_mod",
                    "Не показывать рецепт в JEI/REI",
                    &mut settings.disable_recipe_mod,
                );
                ComboBox::from_id_salt("quest_progression_mode")
                    .selected_text(&settings.progression_mode)
                    .show_ui(ui, |ui| {
                        for (id, title) in [
                            ("default", "По умолчанию главы"),
                            ("flexible", "Гибкий"),
                            ("linear", "Линейный"),
                        ] {
                            ui.selectable_value(
                                &mut settings.progression_mode,
                                id.to_owned(),
                                title,
                            );
                        }
                    });
            });
    }

    fn tasks_editor(&mut self, ui: &mut egui::Ui, chapter_index: usize, quest_index: usize) {
        let quest_id = self.book.chapters[chapter_index].quests[quest_index]
            .id
            .clone();
        let mut open_resource_picker = None;
        egui::CollapsingHeader::new("Задачи")
            .default_open(true)
            .show(ui, |ui| {
                let tasks = &mut self.book.chapters[chapter_index].quests[quest_index].tasks;
                let mut remove = None;
                for (index, task) in tasks.iter_mut().enumerate() {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.strong(match task {
                                Task::Checkmark { .. } => "Отметка",
                                Task::Item { .. } => "Предмет",
                                Task::Kill { .. } => "Убийство",
                                Task::Unsupported { kind, .. } => kind,
                            });
                            if ui.small_button("Удалить").clicked() {
                                remove = Some(index);
                            }
                        });
                        match task {
                            Task::Checkmark { title, .. } => {
                                localized_singleline(
                                    ui,
                                    &mut self.book.translations,
                                    title,
                                    self.multilingual_mode,
                                    f32::INFINITY,
                                    None,
                                );
                            }
                            Task::Item {
                                title, item, count, ..
                            } => {
                                localized_singleline(
                                    ui,
                                    &mut self.book.translations,
                                    title,
                                    self.multilingual_mode,
                                    f32::INFINITY,
                                    None,
                                );
                                ui.horizontal(|ui| {
                                    ui.label("Предмет");
                                    let response = ui.text_edit_singleline(item);
                                    if response.changed()
                                        || ui.small_button("Справочник…").clicked()
                                    {
                                        open_resource_picker = Some((
                                            ResourcePickerTarget::TaskItem {
                                                quest_id: quest_id.clone(),
                                                index,
                                            },
                                            item.clone(),
                                        ));
                                    }
                                });
                                ui.add(DragValue::new(count).range(1..=u32::MAX));
                            }
                            Task::Kill {
                                title,
                                entity,
                                count,
                                ..
                            } => {
                                localized_singleline(
                                    ui,
                                    &mut self.book.translations,
                                    title,
                                    self.multilingual_mode,
                                    f32::INFINITY,
                                    None,
                                );
                                ui.horizontal(|ui| {
                                    ui.label("Сущность");
                                    let response = ui.text_edit_singleline(entity);
                                    if response.changed()
                                        || ui.small_button("Справочник…").clicked()
                                    {
                                        open_resource_picker = Some((
                                            ResourcePickerTarget::TaskEntity {
                                                quest_id: quest_id.clone(),
                                                index,
                                            },
                                            entity.clone(),
                                        ));
                                    }
                                });
                                ui.add(DragValue::new(count).range(1..=u32::MAX));
                            }
                            Task::Unsupported { kind, .. } => {
                                ui.small(format!(
                                    "Тип «{kind}» сохранён из исходного SNBT без изменений."
                                ));
                            }
                        }
                    });
                }
                if let Some(index) = remove {
                    tasks.remove(index);
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.small_button("+ отметка").clicked() {
                        tasks.push(Task::Checkmark {
                            id: self.ids.next_id(),
                            title: "Выполнить".to_owned(),
                            raw: None,
                        });
                    }
                    if ui.small_button("+ предмет").clicked() {
                        tasks.push(Task::Item {
                            id: self.ids.next_id(),
                            title: "Получить предмет".to_owned(),
                            item: "minecraft:stone".to_owned(),
                            count: 1,
                            raw: None,
                        });
                    }
                    if ui.small_button("+ убийство").clicked() {
                        tasks.push(Task::Kill {
                            id: self.ids.next_id(),
                            title: "Убить существо".to_owned(),
                            entity: "minecraft:zombie".to_owned(),
                            count: 1,
                            raw: None,
                        });
                    }
                });
            });
        if let Some((target, query)) = open_resource_picker {
            self.resource_picker = Some(target);
            self.resource_query = query;
        }
    }

    fn rewards_editor(&mut self, ui: &mut egui::Ui, chapter_index: usize, quest_index: usize) {
        let quest_id = self.book.chapters[chapter_index].quests[quest_index]
            .id
            .clone();
        let mut open_resource_picker = None;
        egui::CollapsingHeader::new("Награды")
            .default_open(true)
            .show(ui, |ui| {
                let rewards = &mut self.book.chapters[chapter_index].quests[quest_index].rewards;
                let mut remove = None;
                for (index, reward) in rewards.iter_mut().enumerate() {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.strong(match reward {
                                Reward::Xp { .. } => "Опыт",
                                Reward::Item { .. } => "Предмет",
                                Reward::Unsupported { kind, .. } => kind,
                            });
                            if ui.small_button("Удалить").clicked() {
                                remove = Some(index);
                            }
                        });
                        match reward {
                            Reward::Xp { amount, .. } => {
                                ui.add(DragValue::new(amount).range(1..=u32::MAX).prefix("XP: "));
                            }
                            Reward::Item { item, count, .. } => {
                                ui.horizontal(|ui| {
                                    let response = ui.text_edit_singleline(item);
                                    if response.changed()
                                        || ui.small_button("Справочник…").clicked()
                                    {
                                        open_resource_picker = Some((
                                            ResourcePickerTarget::RewardItem {
                                                quest_id: quest_id.clone(),
                                                index,
                                            },
                                            item.clone(),
                                        ));
                                    }
                                });
                                ui.add(
                                    DragValue::new(count)
                                        .range(1..=u32::MAX)
                                        .prefix("Количество: "),
                                );
                            }
                            Reward::Unsupported { kind, .. } => {
                                ui.small(format!(
                                    "Тип «{kind}» сохранён из исходного SNBT без изменений."
                                ));
                            }
                        }
                    });
                }
                if let Some(index) = remove {
                    rewards.remove(index);
                }
                ui.horizontal(|ui| {
                    if ui.small_button("+ XP").clicked() {
                        rewards.push(Reward::Xp {
                            id: self.ids.next_id(),
                            amount: 10,
                            raw: None,
                        });
                    }
                    if ui.small_button("+ предмет").clicked() {
                        rewards.push(Reward::Item {
                            id: self.ids.next_id(),
                            item: "minecraft:diamond".to_owned(),
                            count: 1,
                            raw: None,
                        });
                    }
                });
            });
        if let Some((target, query)) = open_resource_picker {
            self.resource_picker = Some(target);
            self.resource_query = query;
        }
    }

    fn dependencies_editor(&mut self, ui: &mut egui::Ui, chapter_index: usize, quest_index: usize) {
        let chapter = &self.book.chapters[chapter_index];
        let quest_id = chapter.quests[quest_index].id.clone();
        let current_dependencies = chapter.quests[quest_index]
            .dependencies
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let candidates = chapter
            .quests
            .iter()
            .filter(|quest| quest.id != quest_id)
            .map(|quest| {
                (
                    quest.id.clone(),
                    dependency_candidate_label(&self.book.translations, quest),
                    quest.title.clone(),
                )
            })
            .collect::<Vec<_>>();
        let mut changes = Vec::new();

        egui::CollapsingHeader::new("Зависимости")
            .default_open(true)
            .show(ui, |ui| {
                if candidates.is_empty() {
                    ui.small("В главе пока нет других квестов.");
                }
                for (candidate_id, title, raw_title) in &candidates {
                    let mut enabled = current_dependencies.contains(candidate_id);
                    let response = ui.checkbox(&mut enabled, title).on_hover_text(format!(
                        "ID: {candidate_id}\nИсходное название: {raw_title}"
                    ));
                    if response.changed() {
                        changes.push((candidate_id.clone(), enabled));
                    }
                }
            });

        for (dependency_id, enabled) in changes {
            if enabled
                && let Some(cycle) =
                    cycle_after_adding_dependency(&self.book, &quest_id, &dependency_id)
            {
                self.status =
                    "Зависимость не добавлена: получился бы циклический маршрут.".to_owned();
                self.log_cycle(&cycle, "Отклонена новая зависимость");
                self.right_tab = RightTab::Logs;
                continue;
            }
            let dependencies =
                &mut self.book.chapters[chapter_index].quests[quest_index].dependencies;
            if enabled {
                if !dependencies.contains(&dependency_id) {
                    dependencies.push(dependency_id);
                }
            } else {
                dependencies.retain(|id| id != &dependency_id);
            }
        }
    }

    fn clear_resource_texture_cache(&mut self) {
        self.resource_textures.clear();
        self.failed_resource_textures.clear();
    }

    fn resource_texture(
        &mut self,
        context: &egui::Context,
        resource_id: &str,
    ) -> Option<TextureId> {
        if let Some(texture) = self.resource_textures.get(resource_id) {
            return Some(texture.id());
        }
        if self.failed_resource_textures.contains(resource_id) {
            return None;
        }
        let bytes = self
            .resource_index
            .entries
            .iter()
            .find(|entry| entry.id == resource_id)?
            .icon_png
            .clone()?;
        let Ok(image) = image::load_from_memory(&bytes) else {
            self.failed_resource_textures.insert(resource_id.to_owned());
            return None;
        };
        let image = image.to_rgba8();
        let size = [image.width() as usize, image.height() as usize];
        let color_image = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
        let texture = context.load_texture(
            format!("resource:{resource_id}"),
            color_image,
            TextureOptions::NEAREST,
        );
        let texture_id = texture.id();
        self.resource_textures
            .insert(resource_id.to_owned(), texture);
        Some(texture_id)
    }

    fn resource_picker_window(&mut self, context: &egui::Context) {
        let Some(target) = self.resource_picker.clone() else {
            return;
        };
        let mut open = true;
        let mut selected_id = None;
        let screen = context.content_rect().shrink(12.0);
        let window_size = vec2(screen.width().min(460.0), screen.height().min(460.0));
        egui::Window::new(match target.kind() {
            ResourceKind::Item => "Справочник предметов",
            ResourceKind::Entity => "Справочник сущностей",
        })
        .id(Id::new("resource_picker_compact_v2"))
        .open(&mut open)
        .default_size(window_size)
        .max_width(window_size.x)
        .max_height(window_size.y)
        .constrain_to(screen)
        .show(context, |ui| {
            ui.label("Поиск");
            ui.add_sized(
                [ui.available_width(), 24.0],
                TextEdit::singleline(&mut self.resource_query)
                    .hint_text("Zombie, brass, minecraft:diamond…"),
            );
            ui.horizontal_wrapped(|ui| {
                if ui.button("Добавить Minecraft JAR…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .set_title("Выберите клиентский Minecraft JAR")
                        .add_filter("Minecraft JAR", &["jar"])
                        .pick_file()
                {
                    match ResourceIndex::import_minecraft_jar(&self.resource_project_root, &path) {
                        Ok(destination) => {
                            self.resource_index =
                                ResourceIndex::scan_project(&self.resource_project_root);
                            self.clear_resource_texture_cache();
                            self.status = format!(
                                "Minecraft JAR импортирован: {}. В справочнике {} записей.",
                                destination.display(),
                                self.resource_index.entries.len()
                            );
                            self.push_log(LogLevel::Info, self.status.clone());
                        }
                        Err(error) => {
                            self.status =
                                format!("Не удалось импортировать Minecraft JAR: {error}");
                            self.push_log(LogLevel::Error, self.status.clone());
                        }
                    }
                }
                if ui.button("Переиндексировать").clicked() {
                    self.resource_index = ResourceIndex::scan_project(&self.resource_project_root);
                    self.clear_resource_texture_cache();
                    self.status = format!(
                        "Справочник обновлён: {} записей.",
                        self.resource_index.entries.len()
                    );
                }
                ui.small(format!(
                    "{} записей, источников: {}",
                    self.resource_index.entries.len(),
                    self.resource_index.scanned_sources.len()
                ));
            });
            ui.small(format!(
                "Моды проекта: {}",
                self.resource_project_root.join("mods").display()
            ));
            let vanilla_jar = minecraft_jar_path(&self.resource_project_root);
            ui.small(if vanilla_jar.is_file() {
                format!("Vanilla: {}", vanilla_jar.display())
            } else {
                "Vanilla: добавьте клиентский JAR для полного набора Minecraft.".to_owned()
            });
            if !self.resource_index.warnings.is_empty() {
                ui.colored_label(
                    Color32::YELLOW,
                    format!(
                        "Не удалось прочитать источников: {}",
                        self.resource_index.warnings.len()
                    ),
                );
            }
            ui.separator();
            let query = self.resource_query.trim().to_lowercase();
            let matches = self
                .resource_index
                .entries
                .iter()
                .filter(|entry| entry.kind == target.kind())
                .filter(|entry| {
                    query.is_empty()
                        || entry.id.to_lowercase().contains(&query)
                        || entry.name.to_lowercase().contains(&query)
                })
                .take(500)
                .map(|entry| (entry.id.clone(), entry.name.clone()))
                .collect::<Vec<_>>();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (id, name) in matches {
                    let response = ui
                        .horizontal(|ui| {
                            let icon_size = vec2(24.0, 24.0);
                            if let Some(texture) = self.resource_texture(context, &id) {
                                ui.add_sized(icon_size, egui::Image::new((texture, icon_size)));
                            } else {
                                ui.allocate_space(icon_size);
                            }
                            ui.add_sized(
                                [ui.available_width(), 24.0],
                                egui::Label::new(format!("{name}   {id}")).truncate(),
                            );
                        })
                        .response
                        .interact(Sense::click());
                    if response.clicked() {
                        selected_id = Some(id);
                    }
                }
            });
        });
        if let Some(id) = selected_id {
            self.apply_resource_choice(&target, id);
            self.resource_picker = None;
        } else if !open {
            self.resource_picker = None;
        }
    }

    fn apply_resource_choice(&mut self, target: &ResourcePickerTarget, id: String) {
        let quest_id = match target {
            ResourcePickerTarget::QuestIcon { quest_id }
            | ResourcePickerTarget::TaskItem { quest_id, .. }
            | ResourcePickerTarget::TaskEntity { quest_id, .. }
            | ResourcePickerTarget::RewardItem { quest_id, .. } => quest_id,
        };
        let Some(quest) = self
            .book
            .chapters
            .iter_mut()
            .flat_map(|chapter| chapter.quests.iter_mut())
            .find(|quest| &quest.id == quest_id)
        else {
            return;
        };
        match target {
            ResourcePickerTarget::QuestIcon { .. } => quest.icon = id,
            ResourcePickerTarget::TaskItem { index, .. } => {
                if let Some(Task::Item { item, .. }) = quest.tasks.get_mut(*index) {
                    *item = id;
                }
            }
            ResourcePickerTarget::TaskEntity { index, .. } => {
                if let Some(Task::Kill { entity, .. }) = quest.tasks.get_mut(*index) {
                    *entity = id;
                }
            }
            ResourcePickerTarget::RewardItem { index, .. } => {
                if let Some(Reward::Item { item, .. }) = quest.rewards.get_mut(*index) {
                    *item = id;
                }
            }
        }
    }

    fn confirmation_windows(&mut self, context: &egui::Context) {
        let Some(ids) = self.delete_confirmation.clone() else {
            return;
        };
        egui::Window::new("Удалить квесты?")
            .id(Id::new("delete_quests_confirmation"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(context, |ui| {
                ui.label(format!(
                    "Будет удалено квестов: {}. Ссылки на них тоже будут очищены.",
                    ids.len()
                ));
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::new("Удалить").color(Color32::LIGHT_RED))
                        .clicked()
                    {
                        self.delete_quests(&ids);
                        self.delete_confirmation = None;
                    }
                    if ui.button("Отмена").clicked() {
                        self.delete_confirmation = None;
                    }
                });
            });
    }

    fn canvas(&mut self, root_ui: &mut egui::Ui) {
        egui::CentralPanel::default().show_inside(root_ui, |ui| {
            let scene = egui::Scene::new()
                .zoom_range(0.15..=3.5)
                .max_inner_size(Vec2::splat(200_000.0))
                .drag_pan_buttons(egui::DragPanButtons::MIDDLE | egui::DragPanButtons::SECONDARY);
            let chapter_index = self.selected_chapter;
            let mut clicked_quest = None;
            let mut dragged_quest = None;
            let mut pointer_in_scene = None;
            let quest_icons = self
                .book
                .chapters
                .get(chapter_index)
                .map(|chapter| {
                    chapter
                        .quests
                        .iter()
                        .filter(|quest| !quest.icon.trim().is_empty())
                        .map(|quest| (quest.id.clone(), quest.icon.trim().to_owned()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(quest_id, resource_id)| {
                    self.resource_texture(ui.ctx(), &resource_id)
                        .map(|texture| (quest_id, texture))
                })
                .collect::<HashMap<_, _>>();

            let result = scene.show(ui, &mut self.scene_rect, |scene_ui| {
                let painter = scene_ui.painter();
                pointer_in_scene =
                    scene_ui
                        .input(|input| input.pointer.hover_pos())
                        .and_then(|position| {
                            scene_ui
                                .ctx()
                                .layer_transform_from_global(scene_ui.layer_id())
                                .map(|transform| transform * position)
                        });
                draw_grid(
                    painter,
                    scene_ui.clip_rect(),
                    self.book.grid_scale as f32 * FTB_UNIT,
                );

                if let Some(chapter) = self.book.chapters.get(chapter_index) {
                    let positions = chapter
                        .quests
                        .iter()
                        .map(|quest| {
                            (
                                quest.id.as_str(),
                                Pos2::new(quest.x as f32 * FTB_UNIT, quest.y as f32 * FTB_UNIT),
                            )
                        })
                        .collect::<HashMap<_, _>>();
                    for quest in &chapter.quests {
                        let end = positions[quest.id.as_str()];
                        for dependency in &quest.dependencies {
                            if let Some(start) = positions.get(dependency.as_str()) {
                                painter.line_segment(
                                    [*start, end],
                                    Stroke::new(4.0, Color32::from_rgb(83, 123, 164)),
                                );
                            }
                        }
                    }

                    for quest in &chapter.quests {
                        let display_title = self.book.translations.resolve(&quest.title);
                        let center =
                            Pos2::new(quest.x as f32 * FTB_UNIT, quest.y as f32 * FTB_UNIT);
                        let diameter = (QUEST_SIZE * quest.size as f32).max(18.0);
                        let rect = Rect::from_center_size(center, Vec2::splat(diameter));
                        let response = scene_ui
                            .interact(
                                rect,
                                Id::new(("quest_node", &quest.id)),
                                Sense::click_and_drag(),
                            )
                            .on_hover_text(format!(
                                "{}\n[{:.2}, {:.2}]\n{} задач, {} наград",
                                display_title,
                                quest.x,
                                quest.y,
                                quest.tasks.len(),
                                quest.rewards.len()
                            ));

                        if response.clicked() {
                            clicked_quest = Some((
                                quest.id.clone(),
                                scene_ui.input(|input| input.modifiers.shift),
                            ));
                        }
                        if response.dragged_by(PointerButton::Primary)
                            || response.drag_stopped_by(PointerButton::Primary)
                        {
                            dragged_quest = Some((
                                quest.id.clone(),
                                response.drag_delta() / FTB_UNIT,
                                response.drag_started(),
                                response.drag_stopped(),
                                scene_ui.input(|input| input.modifiers.shift),
                            ));
                        }

                        let selected = self.selected_quests.contains(&quest.id);
                        draw_quest_node(
                            painter,
                            rect,
                            quest,
                            &display_title,
                            selected,
                            response.hovered(),
                            quest_icons.get(&quest.id).copied(),
                        );
                    }
                }

                if let Some(start) = self.selection_drag_start
                    && let Some(current) = pointer_in_scene
                {
                    painter.rect_stroke(
                        Rect::from_two_pos(start, current),
                        0.0,
                        Stroke::new(2.0, Color32::from_rgb(99, 180, 255)),
                        StrokeKind::Inside,
                    );
                }
                if self.canvas_tool == CanvasTool::Connect
                    && let Some(start_id) = self.connection_start.as_deref()
                    && let Some(chapter) = self.book.chapters.get(chapter_index)
                    && let Some(start) = chapter.quests.iter().find(|quest| quest.id == start_id)
                    && let Some(current) = pointer_in_scene
                {
                    painter.line_segment(
                        [
                            Pos2::new(start.x as f32 * FTB_UNIT, start.y as f32 * FTB_UNIT),
                            current,
                        ],
                        Stroke::new(3.0, Color32::LIGHT_BLUE),
                    );
                }
            });

            if let Some((id, shift)) = clicked_quest {
                match self.canvas_tool {
                    CanvasTool::Cursor | CanvasTool::Select => {
                        if shift || self.canvas_tool == CanvasTool::Select {
                            self.toggle_selection(id);
                        } else {
                            self.select_only(id);
                        }
                    }
                    CanvasTool::Connect => self.handle_connection_click(id),
                    CanvasTool::Delete => {
                        self.delete_confirmation = Some(vec![id]);
                    }
                }
            } else if result.response.clicked_by(PointerButton::Primary) {
                if self.canvas_tool != CanvasTool::Connect {
                    self.clear_quest_selection();
                } else {
                    self.connection_start = None;
                }
            }

            if let Some(position) = result.response.hover_pos() {
                self.cursor_world = Some((
                    position.x as f64 / FTB_UNIT as f64,
                    position.y as f64 / FTB_UNIT as f64,
                ));
            } else {
                self.cursor_world = None;
            }

            if result.response.double_clicked_by(PointerButton::Primary)
                && self.canvas_tool == CanvasTool::Cursor
                && let Some(position) = result.response.hover_pos()
            {
                let x = snap_coordinate(
                    position.x as f64 / FTB_UNIT as f64,
                    self.book.grid_scale,
                    1.0,
                );
                let y = snap_coordinate(
                    position.y as f64 / FTB_UNIT as f64,
                    self.book.grid_scale,
                    1.0,
                );
                self.book.add_quest(chapter_index, (x, y), &mut self.ids);
                if let Some(id) = self.book.chapters[chapter_index]
                    .quests
                    .last()
                    .map(|quest| quest.id.clone())
                {
                    self.select_only(id);
                }
            }

            if let Some((id, delta, drag_started, drag_stopped, disable_snap)) = dragged_quest
                && matches!(self.canvas_tool, CanvasTool::Cursor | CanvasTool::Select)
            {
                if drag_started || self.drag_origins.is_none() {
                    if !self.selected_quests.contains(&id) {
                        self.select_only(id);
                    }
                    self.drag_accumulated = Vec2::ZERO;
                    self.drag_origins = self.book.chapters.get(chapter_index).map(|chapter| {
                        chapter
                            .quests
                            .iter()
                            .filter(|quest| self.selected_quests.contains(&quest.id))
                            .map(|quest| (quest.id.clone(), (quest.x, quest.y)))
                            .collect()
                    });
                }
                self.drag_accumulated += delta;
                if let (Some(origins), Some(chapter)) = (
                    self.drag_origins.as_ref(),
                    self.book.chapters.get_mut(chapter_index),
                ) {
                    for quest in &mut chapter.quests {
                        if let Some((x, y)) = origins.get(&quest.id) {
                            let mut new_x = x + self.drag_accumulated.x as f64;
                            let mut new_y = y + self.drag_accumulated.y as f64;
                            if self.snap_enabled && !disable_snap {
                                new_x = snap_coordinate(new_x, self.book.grid_scale, quest.size);
                                new_y = snap_coordinate(new_y, self.book.grid_scale, quest.size);
                            }
                            quest.x = new_x;
                            quest.y = new_y;
                        }
                    }
                }
                if drag_stopped {
                    self.drag_origins = None;
                    self.drag_accumulated = Vec2::ZERO;
                }
            }

            if self.canvas_tool == CanvasTool::Select {
                if result.response.drag_started_by(PointerButton::Primary) {
                    self.selection_drag_start = pointer_in_scene;
                }
                if result.response.drag_stopped_by(PointerButton::Primary)
                    && let (Some(start), Some(end)) =
                        (self.selection_drag_start.take(), pointer_in_scene)
                {
                    let area = Rect::from_two_pos(start, end);
                    let additive = ui.input(|input| input.modifiers.shift);
                    if !additive {
                        self.clear_quest_selection();
                    }
                    if let Some(chapter) = self.book.chapters.get(chapter_index) {
                        let selected = chapter
                            .quests
                            .iter()
                            .filter(|quest| {
                                area.contains(Pos2::new(
                                    quest.x as f32 * FTB_UNIT,
                                    quest.y as f32 * FTB_UNIT,
                                ))
                            })
                            .map(|quest| quest.id.clone())
                            .collect::<Vec<_>>();
                        for id in selected {
                            self.selected_quests.insert(id.clone());
                            self.selected_quest = Some(id);
                        }
                    }
                }
            } else {
                self.selection_drag_start = None;
            }
        });
    }

    fn handle_connection_click(&mut self, quest_id: String) {
        let Some(start_id) = self.connection_start.take() else {
            self.connection_start = Some(quest_id.clone());
            self.select_only(quest_id);
            self.status =
                "Выберите зависимый квест: связь строится от требования к результату.".to_owned();
            return;
        };
        if start_id == quest_id {
            self.status = "Квест не может зависеть от самого себя.".to_owned();
            return;
        }
        if let Some(cycle) = cycle_after_adding_dependency(&self.book, &quest_id, &start_id) {
            self.status = "Связь отклонена: она создаёт цикл.".to_owned();
            self.log_cycle(&cycle, "Отклонена новая зависимость");
            self.right_tab = RightTab::Logs;
            return;
        }
        if let Some(quest) = self.book.chapters[self.selected_chapter]
            .quests
            .iter_mut()
            .find(|quest| quest.id == quest_id)
            && !quest.dependencies.contains(&start_id)
        {
            quest.dependencies.push(start_id);
        }
        self.select_only(quest_id);
        self.status = "Зависимость создана.".to_owned();
    }

    fn bottom_bar(&mut self, root_ui: &mut egui::Ui) {
        egui::Panel::bottom("status_bar").show_inside(root_ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status);
                if let Some((x, y)) = self.cursor_world {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.monospace(format!("Курсор: [{x:+.2}, {y:+.2}]"));
                    });
                }
            });
        });
    }
}

impl eframe::App for QuestEditorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.tick_autosave(ui.ctx());
        self.handle_shortcuts(ui.ctx());
        self.top_bar(ui);
        self.bottom_bar(ui);
        self.left_panel(ui);
        self.right_panel(ui);
        self.canvas(ui);
        self.resource_picker_window(ui.ctx());
        self.confirmation_windows(ui.ctx());
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Ok(value) = serde_json::to_string(&self.preferences()) {
            storage.set_string(APP_STATE_KEY, value);
        }
    }
}

fn project_root(path: &std::path::Path) -> &std::path::Path {
    if path.file_name().is_some_and(|name| name == "quests") {
        path.parent().unwrap_or(path)
    } else {
        path
    }
}

fn import_destination(source: &std::path::Path) -> PathBuf {
    let project_root = project_root(source);
    let parent = project_root
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let name = project_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ftbquests".to_owned());

    for suffix in 1.. {
        let edited_name = if suffix == 1 {
            format!("{name}_edited")
        } else {
            format!("{name}_edited_{suffix}")
        };
        let candidate = parent.join(edited_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

#[derive(Debug, Clone)]
enum SearchTarget {
    Group {
        group_id: String,
    },
    Chapter {
        chapter_index: usize,
    },
    Quest {
        chapter_index: usize,
        quest_id: String,
    },
}

struct SearchHit {
    score: u8,
    title: String,
    subtitle: String,
    target: SearchTarget,
}

fn search_book(book: &EditableBook, query: &str) -> (Vec<SearchHit>, usize) {
    let query = query.trim().to_lowercase();
    let mut hits = Vec::new();

    for group in &book.groups {
        let group_title = book.translations.resolve(&group.title);
        if let Some(score) = search_score(&query, &group.id, &group_title, &group.title) {
            hits.push(SearchHit {
                score,
                title: format!("Группа: {group_title}"),
                subtitle: format!("ID: {}", group.id),
                target: SearchTarget::Group {
                    group_id: group.id.clone(),
                },
            });
        }
    }

    for (chapter_index, chapter) in book.chapters.iter().enumerate() {
        let group_title_raw = chapter
            .group
            .as_deref()
            .and_then(|group_id| book.groups.iter().find(|group| group.id == group_id))
            .map(|group| group.title.as_str())
            .unwrap_or("Без группы");
        let group_title = book.translations.resolve(group_title_raw);
        let chapter_title = book.translations.resolve(&chapter.title);
        if let Some(score) = search_score(
            &query,
            &chapter.id,
            &chapter_title,
            &format!(
                "{} {} {} {}",
                chapter.filename, group_title, chapter.title, group_title_raw
            ),
        ) {
            hits.push(SearchHit {
                score,
                title: format!("Глава: {chapter_title}"),
                subtitle: format!(
                    "{} · файл {} · ID {}",
                    group_title, chapter.filename, chapter.id
                ),
                target: SearchTarget::Chapter { chapter_index },
            });
        }

        for quest in &chapter.quests {
            let quest_title = book.translations.resolve(&quest.title);
            let quest_description = book.translations.resolve(&quest.description);
            let subtitle = book.translations.resolve(&quest.settings.subtitle);
            let mut details = format!(
                "{} {} {} {} {} {} {} {} {} {}",
                quest_description,
                quest.description,
                quest.icon,
                chapter_title,
                chapter.title,
                group_title,
                chapter.filename,
                subtitle,
                quest.settings.subtitle,
                quest.settings.tags.join(" ")
            );
            for task in &quest.tasks {
                match task {
                    Task::Checkmark { id, title, .. } => {
                        details.push_str(&format!(" {id} {title} checkmark"));
                    }
                    Task::Item {
                        id, title, item, ..
                    } => {
                        details.push_str(&format!(" {id} {title} {item} item"));
                    }
                    Task::Kill {
                        id, title, entity, ..
                    } => {
                        details.push_str(&format!(" {id} {title} {entity} kill"));
                    }
                    Task::Unsupported { kind, raw } => {
                        details.push_str(&format!(" {kind} {}", snbt::stringify(raw)));
                    }
                }
            }
            for reward in &quest.rewards {
                match reward {
                    Reward::Xp { id, amount, .. } => {
                        details.push_str(&format!(" {id} {amount} xp"));
                    }
                    Reward::Item {
                        id, item, count, ..
                    } => {
                        details.push_str(&format!(" {id} {item} {count} item"));
                    }
                    Reward::Unsupported { kind, raw } => {
                        details.push_str(&format!(" {kind} {}", snbt::stringify(raw)));
                    }
                }
            }
            if let Some(score) = search_score(&query, &quest.id, &quest_title, &details) {
                hits.push(SearchHit {
                    score,
                    title: format!("Квест: {quest_title}"),
                    subtitle: format!(
                        "{} / {} · ID {} · [{:.2}, {:.2}]",
                        group_title, chapter_title, quest.id, quest.x, quest.y
                    ),
                    target: SearchTarget::Quest {
                        chapter_index,
                        quest_id: quest.id.clone(),
                    },
                });
            }
        }
    }

    hits.sort_by(|left, right| {
        left.score
            .cmp(&right.score)
            .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
    });
    let total = hits.len();
    hits.truncate(MAX_SEARCH_RESULTS);
    (hits, total)
}

fn search_score(query: &str, id: &str, title: &str, details: &str) -> Option<u8> {
    let id = id.to_lowercase();
    let title = title.to_lowercase();
    let details = details.to_lowercase();
    if id == query {
        Some(0)
    } else if title == query {
        Some(1)
    } else if id.starts_with(query) {
        Some(2)
    } else if title.starts_with(query) {
        Some(3)
    } else if id.contains(query) {
        Some(4)
    } else if title.contains(query) {
        Some(5)
    } else if details.contains(query) {
        Some(6)
    } else {
        None
    }
}

fn dependency_candidate_label(
    translations: &crate::editor::TranslationCatalog,
    quest: &Quest,
) -> String {
    let resolved = translations.resolve(&quest.title);
    if resolved == quest.title && translations.exact_key(&quest.title).is_some() {
        format!("{} [{}]", resolved, quest.id)
    } else {
        resolved
    }
}

fn group_display_title(
    translations: &crate::editor::TranslationCatalog,
    raw_title: &str,
) -> String {
    translations.resolve(raw_title)
}

fn localized_singleline(
    ui: &mut egui::Ui,
    translations: &mut crate::editor::TranslationCatalog,
    raw_value: &mut String,
    multilingual_mode: bool,
    width: f32,
    background: Option<Color32>,
) -> egui::Response {
    let key = translations.exact_key(raw_value).map(str::to_owned);
    if let Some(key) = key {
        let locale = translations.active_locale.clone();
        let stored = translations
            .translation(&locale, &key)
            .or_else(|| translations.translation("en_us", &key))
            .unwrap_or(raw_value);
        let mut display = translation_display_text(stored);
        let mut edit = TextEdit::singleline(&mut display).desired_width(width);
        if let Some(background) = background {
            edit = edit.background_color(background);
        }
        let response = ui.add(edit).on_hover_text(format!("Ключ перевода: {key}"));
        if response.changed() {
            translations.set_translation(&locale, &key, translation_storage_text(&display));
        }
        response
    } else {
        let response = ui.horizontal(|ui| {
            let mut edit = TextEdit::singleline(raw_value).desired_width(width);
            if let Some(background) = background {
                edit = edit.background_color(background);
            }
            let response = ui.add(edit);
            if multilingual_mode && ui.small_button("Локализовать").clicked() {
                translations.localize(raw_value);
            }
            response
        });
        response.inner
    }
}

fn localize_nonempty(
    translations: &mut crate::editor::TranslationCatalog,
    value: &mut String,
) -> usize {
    if value.trim().is_empty() || translations.exact_key(value).is_some() {
        return 0;
    }
    translations.localize(value);
    1
}

fn localized_multiline(
    ui: &mut egui::Ui,
    translations: &mut crate::editor::TranslationCatalog,
    raw_value: &mut String,
    multilingual_mode: bool,
    rows: usize,
) -> egui::Response {
    let key = translations.exact_key(raw_value).map(str::to_owned);
    if let Some(key) = key {
        let locale = translations.active_locale.clone();
        let stored = translations
            .translation(&locale, &key)
            .or_else(|| translations.translation("en_us", &key))
            .unwrap_or(raw_value);
        let mut display = translation_display_text(stored);
        let response = ui
            .add(TextEdit::multiline(&mut display).desired_rows(rows))
            .on_hover_text(format!("Ключ перевода: {key}"));
        if response.changed() {
            translations.set_translation(&locale, &key, translation_storage_text(&display));
        }
        response
    } else {
        let response = ui.add(TextEdit::multiline(raw_value).desired_rows(rows));
        if multilingual_mode && ui.small_button("Локализовать").clicked() {
            translations.localize(raw_value);
        }
        response
    }
}

fn tristate_editor(ui: &mut egui::Ui, id: &str, label: &str, value: &mut Tristate) {
    ui.horizontal(|ui| {
        ui.label(label);
        ComboBox::from_id_salt(id)
            .selected_text(match value {
                Tristate::Default => "По умолчанию",
                Tristate::True => "Да",
                Tristate::False => "Нет",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(value, Tristate::Default, "По умолчанию");
                ui.selectable_value(value, Tristate::True, "Да");
                ui.selectable_value(value, Tristate::False, "Нет");
            });
    });
}

fn format_cycle_route(cycle: &DependencyCycle, mut label: impl FnMut(&str) -> String) -> String {
    let ids = &cycle.quest_ids;
    let visible = if cycle.length() < 10 {
        ids.iter().map(|id| label(id)).collect::<Vec<_>>()
    } else {
        let mut route = ids.iter().take(5).map(|id| label(id)).collect::<Vec<_>>();
        route.push("…".to_owned());
        route.extend(
            ids.iter()
                .skip(ids.len().saturating_sub(5))
                .map(|id| label(id)),
        );
        route
    };
    visible.join(" → ")
}

fn autosave_destination(destination: &std::path::Path) -> PathBuf {
    let project_root = if destination.file_name().is_some_and(|name| name == "quests") {
        destination.parent().unwrap_or(destination)
    } else {
        destination
    };
    let parent = project_root
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let name = project_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "ftbquests".to_owned());
    parent.join(format!("{name}_autosave"))
}

fn save_autosave_snapshot(
    book: &EditableBook,
    destination: &std::path::Path,
) -> Result<(), String> {
    let name = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ftbquests_autosave".to_owned());
    let parent = destination
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let temporary = parent.join(format!("{name}_temporary"));
    let previous = parent.join(format!("{name}_previous"));

    if temporary.exists() {
        fs::remove_dir_all(&temporary).map_err(|error| error.to_string())?;
    }
    book.save(&temporary).map_err(|error| error.to_string())?;
    if previous.exists() {
        fs::remove_dir_all(&previous).map_err(|error| error.to_string())?;
    }
    if destination.exists() {
        fs::rename(destination, &previous).map_err(|error| error.to_string())?;
    }
    if let Err(error) = fs::rename(&temporary, destination) {
        if previous.exists() && !destination.exists() {
            let _ = fs::rename(&previous, destination);
        }
        return Err(error.to_string());
    }
    Ok(())
}

fn draw_grid(painter: &egui::Painter, rect: Rect, step: f32) {
    let step = step.max(4.0);
    let first_x = (rect.left() / step).floor() as i32 - 1;
    let last_x = (rect.right() / step).ceil() as i32 + 1;
    let first_y = (rect.top() / step).floor() as i32 - 1;
    let last_y = (rect.bottom() / step).ceil() as i32 + 1;
    let minor = Stroke::new(1.0, Color32::from_gray(43));
    let major = Stroke::new(1.5, Color32::from_gray(62));

    for index in first_x..=last_x {
        let x = index as f32 * step;
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            if index % 2 == 0 { major } else { minor },
        );
    }
    for index in first_y..=last_y {
        let y = index as f32 * step;
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            if index % 2 == 0 { major } else { minor },
        );
    }

    painter.line_segment(
        [Pos2::new(0.0, rect.top()), Pos2::new(0.0, rect.bottom())],
        Stroke::new(2.0, Color32::from_rgb(110, 62, 62)),
    );
    painter.line_segment(
        [Pos2::new(rect.left(), 0.0), Pos2::new(rect.right(), 0.0)],
        Stroke::new(2.0, Color32::from_rgb(62, 96, 110)),
    );
}

fn draw_quest_node(
    painter: &egui::Painter,
    rect: Rect,
    quest: &Quest,
    display_title: &str,
    selected: bool,
    hovered: bool,
    icon: Option<TextureId>,
) {
    let fill = if selected {
        Color32::from_rgb(84, 141, 211)
    } else if hovered {
        Color32::from_rgb(84, 96, 120)
    } else {
        Color32::from_rgb(58, 66, 82)
    };
    let stroke = Stroke::new(
        if selected { 3.0 } else { 2.0 },
        if selected {
            Color32::WHITE
        } else {
            Color32::from_gray(160)
        },
    );
    let center = rect.center();
    let radius = rect.width() / 2.0;

    match quest.shape.as_str() {
        "square" => {
            painter.rect_filled(rect, 0.0, fill);
            painter.rect_stroke(rect, 0.0, stroke, StrokeKind::Inside);
        }
        "rsquare" => {
            painter.rect_filled(rect, 8.0, fill);
            painter.rect_stroke(rect, 8.0, stroke, StrokeKind::Inside);
        }
        "diamond" => {
            painter.add(Shape::convex_polygon(
                vec![
                    Pos2::new(center.x, rect.top()),
                    Pos2::new(rect.right(), center.y),
                    Pos2::new(center.x, rect.bottom()),
                    Pos2::new(rect.left(), center.y),
                ],
                fill,
                stroke,
            ));
        }
        "hexagon" => {
            painter.add(Shape::convex_polygon(
                polygon(center, radius, 6, 0.0),
                fill,
                stroke,
            ));
        }
        "pentagon" => {
            painter.add(Shape::convex_polygon(
                polygon(center, radius, 5, -std::f32::consts::FRAC_PI_2),
                fill,
                stroke,
            ));
        }
        "octagon" | "gear" => {
            painter.add(Shape::convex_polygon(
                polygon(center, radius, 8, std::f32::consts::FRAC_PI_8),
                fill,
                stroke,
            ));
        }
        "heart" => {
            let lobe_radius = radius * 0.42;
            painter.circle_filled(
                center + vec2(-radius * 0.28, -radius * 0.2),
                lobe_radius,
                fill,
            );
            painter.circle_filled(
                center + vec2(radius * 0.28, -radius * 0.2),
                lobe_radius,
                fill,
            );
            painter.add(Shape::convex_polygon(
                vec![
                    center + vec2(-radius * 0.72, -radius * 0.12),
                    center + vec2(radius * 0.72, -radius * 0.12),
                    center + vec2(0.0, radius * 0.9),
                ],
                fill,
                stroke,
            ));
        }
        _ => {
            painter.circle_filled(center, radius, fill);
            painter.circle_stroke(center, radius, stroke);
        }
    };

    if let Some(icon) = icon {
        let icon_rect = Rect::from_center_size(center, rect.size() * 0.66);
        painter.image(
            icon,
            icon_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        let label = if display_title.trim().is_empty() {
            "?"
        } else {
            display_title.trim()
        };
        let short_label = label.chars().take(3).collect::<String>();
        painter.text(
            center,
            Align2::CENTER_CENTER,
            short_label,
            FontId::proportional((rect.width() * 0.26).clamp(10.0, 18.0)),
            Color32::WHITE,
        );
    }
}

fn polygon(center: Pos2, radius: f32, sides: usize, rotation: f32) -> Vec<Pos2> {
    (0..sides)
        .map(|index| {
            let angle = rotation + index as f32 * std::f32::consts::TAU / sides as f32;
            center + vec2(angle.cos(), angle.sin()) * radius
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::diagnostics::DependencyCycle;
    use crate::editor::{EditableBook, IdGenerator, Quest, QuestSettings, TranslationCatalog};

    #[test]
    fn short_cycle_route_is_complete() {
        let cycle = DependencyCycle {
            quest_ids: ["A", "B", "C", "D", "A"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        };
        assert_eq!(
            super::format_cycle_route(&cycle, str::to_owned),
            "A → B → C → D → A"
        );
    }

    #[test]
    fn long_cycle_route_keeps_first_and_last_five_points() {
        let mut ids = (0..12).map(|id| id.to_string()).collect::<Vec<_>>();
        ids.push("0".to_owned());
        let route = super::format_cycle_route(&DependencyCycle { quest_ids: ids }, str::to_owned);
        assert_eq!(route, "0 → 1 → 2 → 3 → 4 → … → 8 → 9 → 10 → 11 → 0");
    }

    #[test]
    fn search_finds_quest_by_task_item() {
        let mut ids = IdGenerator::new();
        let mut book = EditableBook::blank(&mut ids);
        book.add_quest(0, (0.0, 0.0), &mut ids);
        book.chapters[0].quests[0]
            .tasks
            .push(crate::editor::Task::Item {
                id: ids.next_id(),
                title: "Collect".to_owned(),
                item: "minecraft:diamond".to_owned(),
                count: 1,
                raw: None,
            });

        let (hits, total) = super::search_book(&book, "diamond");
        assert_eq!(total, 1);
        assert!(matches!(hits[0].target, super::SearchTarget::Quest { .. }));
    }

    #[test]
    fn dependency_label_uses_active_translation() {
        let mut translations = TranslationCatalog::default();
        translations.set_translation("en_us", "quest.power", "Power Generation".to_owned());
        let quest = Quest {
            id: "QUEST_ID".to_owned(),
            title: "{quest.power}".to_owned(),
            description: String::new(),
            description_raw: None,
            x: 0.0,
            y: 0.0,
            size: 1.0,
            shape: String::new(),
            icon: String::new(),
            icon_raw: None,
            dependencies: Vec::new(),
            tasks: Vec::new(),
            rewards: Vec::new(),
            settings: QuestSettings::default(),
            extra: Default::default(),
        };

        assert_eq!(
            super::dependency_candidate_label(&translations, &quest),
            "Power Generation"
        );
    }

    #[test]
    fn group_label_uses_active_translation() {
        let mut translations = TranslationCatalog::default();
        translations.set_translation("en_us", "chapters.group.1", "Technology".to_owned());
        assert_eq!(
            super::group_display_title(&translations, "{chapters.group.1}"),
            "Technology"
        );
    }

    #[test]
    fn search_handles_bundled_large_example_when_present() {
        let path = std::path::Path::new("ftbquestsATM10");
        if !path.exists() {
            return;
        }
        let (book, _) = EditableBook::load(path).unwrap();
        let (hits, total) = super::search_book(&book, "minecraft:diamond");
        assert!(total > 0);
        assert!(!hits.is_empty());
    }

    #[test]
    fn autosave_rotates_current_and_previous_snapshots() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ftbgui-autosave-{unique}"));
        let destination = root.join("book_autosave");
        let mut ids = IdGenerator::new();
        let mut book = EditableBook::blank(&mut ids);

        super::save_autosave_snapshot(&book, &destination).unwrap();
        book.title = "Second version".to_owned();
        super::save_autosave_snapshot(&book, &destination).unwrap();

        let (current, _) = EditableBook::load(&destination).unwrap();
        let (previous, _) =
            EditableBook::load(root.join("book_autosave_previous").as_path()).unwrap();
        assert_eq!(current.title, "Second version");
        assert_eq!(previous.title, "Новая книга");

        fs::remove_dir_all(root).unwrap();
    }
}
