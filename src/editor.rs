use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::snbt::{self, Value};

#[derive(Debug, Clone)]
pub struct EditableBook {
    pub title: String,
    pub grid_scale: f64,
    pub groups: Vec<ChapterGroup>,
    pub chapters: Vec<Chapter>,
    pub preserved_files: Vec<PreservedFile>,
    pub translations: TranslationCatalog,
}

#[derive(Debug, Clone)]
pub struct TranslationCatalog {
    pub languages: BTreeMap<String, BTreeMap<String, String>>,
    pub active_locale: String,
}

impl Default for TranslationCatalog {
    fn default() -> Self {
        Self {
            languages: BTreeMap::new(),
            active_locale: "en_us".to_owned(),
        }
    }
}

impl TranslationCatalog {
    pub fn resolve(&self, value: &str) -> String {
        resolve_translation_tags(
            value,
            self.languages.get(&self.active_locale),
            self.languages.get("en_us"),
        )
    }

    pub fn exact_key<'a>(&self, value: &'a str) -> Option<&'a str> {
        translation_key(value)
    }

    pub fn translation(&self, locale: &str, key: &str) -> Option<&str> {
        self.languages.get(locale)?.get(key).map(String::as_str)
    }

    pub fn set_translation(&mut self, locale: &str, key: &str, value: String) {
        self.languages
            .entry(locale.to_owned())
            .or_default()
            .insert(key.to_owned(), value);
    }

    pub fn add_locale(&mut self, locale: &str) -> bool {
        let locale = normalize_locale(locale);
        if locale.is_empty() || self.languages.contains_key(&locale) {
            return false;
        }
        self.languages.insert(locale.clone(), BTreeMap::new());
        self.active_locale = locale;
        true
    }

    pub fn localize(&mut self, raw_value: &mut String) -> String {
        if let Some(key) = translation_key(raw_value) {
            return key.to_owned();
        }
        let original = raw_value.clone();
        let key = format!("ftbgui.{}", uuid::Uuid::new_v4());
        *raw_value = format!("{{{key}}}");
        let locale = self.active_locale.clone();
        self.set_translation(&locale, &key, translation_storage_text(&original));
        key
    }
}

#[derive(Debug, Clone)]
pub struct PreservedFile {
    pub relative_path: PathBuf,
    pub contents: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ChapterGroup {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub id: String,
    pub filename: String,
    pub title: String,
    pub group: Option<String>,
    pub order_index: i32,
    pub quests: Vec<Quest>,
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone)]
pub struct Quest {
    pub id: String,
    pub title: String,
    pub description: String,
    pub description_raw: Option<Value>,
    pub x: f64,
    pub y: f64,
    pub size: f64,
    pub shape: String,
    pub icon: String,
    pub icon_raw: Option<Value>,
    pub dependencies: Vec<String>,
    pub tasks: Vec<Task>,
    pub rewards: Vec<Reward>,
    pub settings: QuestSettings,
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone)]
pub struct QuestSettings {
    pub subtitle: String,
    pub tags: Vec<String>,
    pub icon_scale: f64,
    pub min_width: i32,
    pub dependency_requirement: String,
    pub min_required_dependencies: u32,
    pub max_completable_dependents: u32,
    pub hide_until_deps_visible: Tristate,
    pub hide_until_deps_complete: Tristate,
    pub hide_dependency_lines: Tristate,
    pub hide_dependent_lines: bool,
    pub hide_details_until_startable: Tristate,
    pub hide_text_until_complete: Tristate,
    pub hide_lock_icon: bool,
    pub invisible: bool,
    pub invisible_until_tasks: u32,
    pub optional: bool,
    pub can_repeat: Tristate,
    pub repeat_cooldown: u32,
    pub ignore_reward_blocking: bool,
    pub progression_mode: String,
    pub require_sequential_tasks: Tristate,
    pub disable_recipe_mod: Tristate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tristate {
    #[default]
    Default,
    True,
    False,
}

impl Default for QuestSettings {
    fn default() -> Self {
        Self {
            subtitle: String::new(),
            tags: Vec::new(),
            icon_scale: 1.0,
            min_width: 0,
            dependency_requirement: "all_completed".to_owned(),
            min_required_dependencies: 0,
            max_completable_dependents: 0,
            hide_until_deps_visible: Tristate::Default,
            hide_until_deps_complete: Tristate::Default,
            hide_dependency_lines: Tristate::Default,
            hide_dependent_lines: false,
            hide_details_until_startable: Tristate::Default,
            hide_text_until_complete: Tristate::Default,
            hide_lock_icon: false,
            invisible: false,
            invisible_until_tasks: 0,
            optional: false,
            can_repeat: Tristate::Default,
            repeat_cooldown: 0,
            ignore_reward_blocking: false,
            progression_mode: "default".to_owned(),
            require_sequential_tasks: Tristate::Default,
            disable_recipe_mod: Tristate::Default,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Task {
    Checkmark {
        id: String,
        title: String,
        raw: Option<Value>,
    },
    Item {
        id: String,
        title: String,
        item: String,
        count: u32,
        raw: Option<Value>,
    },
    Kill {
        id: String,
        title: String,
        entity: String,
        count: u32,
        raw: Option<Value>,
    },
    Unsupported {
        kind: String,
        raw: Value,
    },
}

#[derive(Debug, Clone)]
pub enum Reward {
    Xp {
        id: String,
        amount: u32,
        raw: Option<Value>,
    },
    Item {
        id: String,
        item: String,
        count: u32,
        raw: Option<Value>,
    },
    Unsupported {
        kind: String,
        raw: Value,
    },
}

#[derive(Debug)]
pub enum EditorError {
    Io(io::Error),
    Parse {
        path: PathBuf,
        source: snbt::ParseError,
    },
    InvalidBook(PathBuf),
    ExistingBook(PathBuf),
    EmptyInput(&'static str),
}

impl fmt::Display for EditorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditorError::Io(error) => write!(formatter, "{error}"),
            EditorError::Parse { path, source } => {
                write!(formatter, "ошибка SNBT в {}: {source}", path.display())
            }
            EditorError::InvalidBook(path) => write!(
                formatter,
                "в {} не найден data.snbt или quests/data.snbt",
                path.display()
            ),
            EditorError::ExistingBook(path) => write!(
                formatter,
                "книга уже существует в {}; выберите новую папку",
                path.display()
            ),
            EditorError::EmptyInput(field) => {
                write!(formatter, "поле «{field}» не может быть пустым")
            }
        }
    }
}

impl Error for EditorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            EditorError::Io(error) => Some(error),
            EditorError::Parse { source, .. } => Some(source),
            EditorError::InvalidBook(_)
            | EditorError::ExistingBook(_)
            | EditorError::EmptyInput(_) => None,
        }
    }
}

impl From<io::Error> for EditorError {
    fn from(error: io::Error) -> Self {
        EditorError::Io(error)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    pub preserved_tasks: usize,
    pub preserved_rewards: usize,
    pub imported_languages: usize,
}

impl ImportReport {
    pub fn preserved_total(&self) -> usize {
        self.preserved_tasks + self.preserved_rewards
    }
}

pub fn run_interactive(output: &Path) -> Result<(), EditorError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    run_editor(stdin.lock(), stdout.lock(), output)
}

pub fn run_editor<R: BufRead, W: Write>(
    mut input: R,
    mut output: W,
    destination: &Path,
) -> Result<(), EditorError> {
    let quests_root = quests_root(destination);
    if quests_root.join("data.snbt").exists() {
        return Err(EditorError::ExistingBook(quests_root));
    }

    writeln!(output, "Создание новой книги FTB Quests")?;
    let title = prompt_required(
        &mut input,
        &mut output,
        "Название книги: ",
        "Название книги",
    )?;
    let chapter_title = prompt_required(
        &mut input,
        &mut output,
        "Название первой главы: ",
        "Название главы",
    )?;
    let default_filename = slugify(&chapter_title);
    let filename = prompt_default(
        &mut input,
        &mut output,
        &format!("Имя файла главы [{default_filename}]: "),
        &default_filename,
    )?;

    let mut ids = IdGenerator::new();
    let mut book = EditableBook::new(title, chapter_title, filename, &mut ids);

    loop {
        writeln!(
            output,
            "\n1. Добавить квест\n\
             2. Добавить задачу к квесту\n\
             3. Связать квесты зависимостью\n\
             4. Добавить награду\n\
             5. Показать книгу\n\
             6. Сохранить\n\
             0. Сохранить и выйти"
        )?;
        let command = prompt(&mut input, &mut output, "Выбор: ")?;
        match command.trim() {
            "1" => add_quest(&mut input, &mut output, &mut book, &mut ids)?,
            "2" => add_task(&mut input, &mut output, &mut book, &mut ids)?,
            "3" => add_dependency(&mut input, &mut output, &mut book)?,
            "4" => add_reward(&mut input, &mut output, &mut book, &mut ids)?,
            "5" => print_book(&mut output, &book)?,
            "6" => {
                book.save(destination)?;
                writeln!(output, "Сохранено в {}", quests_root.display())?;
            }
            "0" => {
                book.save(destination)?;
                writeln!(output, "Сохранено в {}", quests_root.display())?;
                return Ok(());
            }
            _ => writeln!(output, "Неизвестный пункт меню.")?,
        }
    }
}

impl EditableBook {
    pub fn load(source: &Path) -> Result<(Self, ImportReport), EditorError> {
        let root = find_quests_root(source)?;
        let data = read_snbt(&root.join("data.snbt"))?;
        let title = scalar(&data, "title")
            .map(str::to_owned)
            .or_else(|| {
                source
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "Импортированная книга".to_owned());
        let grid_scale = number(&data, "grid_scale").unwrap_or(0.5);

        let groups_path = root.join("chapter_groups.snbt");
        let groups = if groups_path.is_file() {
            let value = read_snbt(&groups_path)?;
            list(&value, "chapter_groups")
                .unwrap_or_default()
                .iter()
                .filter_map(|group| {
                    Some(ChapterGroup {
                        id: scalar(group, "id")?.to_owned(),
                        title: scalar(group, "title").unwrap_or("Без названия").to_owned(),
                    })
                })
                .collect()
        } else {
            Vec::new()
        };

        let mut report = ImportReport::default();
        let preserved_files = load_preserved_files(&root)?;
        let translations = load_translations(&root)?;
        report.imported_languages = translations.languages.len();
        let mut chapters = Vec::new();
        for chapter_path in snbt_files(&root.join("chapters"))? {
            let value = read_snbt(&chapter_path)?;
            let filename = scalar(&value, "filename")
                .map(str::to_owned)
                .or_else(|| {
                    chapter_path
                        .file_stem()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "chapter".to_owned());
            let group = scalar(&value, "group")
                .filter(|group| !group.is_empty())
                .map(str::to_owned);
            let mut quests = Vec::new();
            for quest in list(&value, "quests").unwrap_or_default() {
                let Some(id) = scalar(quest, "id") else {
                    continue;
                };
                let tasks = list(quest, "tasks")
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|task| import_task(task, &mut report))
                    .collect();
                let rewards = list(quest, "rewards")
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|reward| import_reward(reward, &mut report))
                    .collect();
                let dependencies = list(quest, "dependencies")
                    .unwrap_or_default()
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                let description = list(quest, "description")
                    .unwrap_or_default()
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n");
                let settings = import_quest_settings(quest);
                quests.push(Quest {
                    id: id.to_owned(),
                    title: scalar(quest, "title").unwrap_or("Без названия").to_owned(),
                    description,
                    description_raw: quest.get("description").cloned(),
                    x: number(quest, "x").unwrap_or(0.0),
                    y: number(quest, "y").unwrap_or(0.0),
                    size: number(quest, "size").unwrap_or(1.0),
                    shape: scalar(quest, "shape").unwrap_or("").to_owned(),
                    icon: item_id(quest.get("icon")).unwrap_or_default(),
                    icon_raw: quest.get("icon").cloned(),
                    dependencies,
                    tasks,
                    rewards,
                    settings,
                    extra: extra_fields(
                        quest,
                        &[
                            "id",
                            "title",
                            "description",
                            "x",
                            "y",
                            "size",
                            "shape",
                            "icon",
                            "dependencies",
                            "tasks",
                            "rewards",
                            "subtitle",
                            "tags",
                            "icon_scale",
                            "min_width",
                            "dependency_requirement",
                            "min_required_dependencies",
                            "max_completable_dependents",
                            "hide_until_deps_visible",
                            "hide_until_deps_complete",
                            "hide_dependency_lines",
                            "hide_dependent_lines",
                            "hide_details_until_startable",
                            "hide_text_until_complete",
                            "hide_lock_icon",
                            "invisible",
                            "invisible_until_tasks",
                            "optional",
                            "can_repeat",
                            "repeat_cooldown",
                            "ignore_reward_blocking",
                            "progression_mode",
                            "require_sequential_tasks",
                            "disable_recipe_mod",
                        ],
                    ),
                });
            }
            chapters.push(Chapter {
                id: scalar(&value, "id").unwrap_or("0").to_owned(),
                filename,
                title: scalar(&value, "title").unwrap_or("Без названия").to_owned(),
                group,
                order_index: integer(&value, "order_index").unwrap_or(chapters.len() as i32),
                quests,
                extra: extra_fields(
                    &value,
                    &["filename", "group", "id", "order_index", "quests", "title"],
                ),
            });
        }
        chapters.sort_by_key(|chapter| chapter.order_index);
        if chapters.is_empty() {
            return Err(EditorError::InvalidBook(root));
        }

        Ok((
            Self {
                title,
                grid_scale,
                groups,
                chapters,
                preserved_files,
                translations,
            },
            report,
        ))
    }

    pub fn new(
        title: String,
        chapter_title: String,
        filename: String,
        ids: &mut IdGenerator,
    ) -> Self {
        Self {
            title,
            grid_scale: 0.5,
            groups: Vec::new(),
            chapters: vec![Chapter {
                id: ids.next_id(),
                filename,
                title: chapter_title,
                group: None,
                order_index: 0,
                quests: Vec::new(),
                extra: BTreeMap::new(),
            }],
            preserved_files: Vec::new(),
            translations: TranslationCatalog::default(),
        }
    }

    pub fn blank(ids: &mut IdGenerator) -> Self {
        Self::new(
            "Новая книга".to_owned(),
            "Первая глава".to_owned(),
            "first_chapter".to_owned(),
            ids,
        )
    }

    pub fn add_group(&mut self, title: String, ids: &mut IdGenerator) -> String {
        let id = ids.next_id();
        self.groups.push(ChapterGroup {
            id: id.clone(),
            title,
        });
        id
    }

    pub fn add_chapter(
        &mut self,
        title: String,
        group: Option<String>,
        ids: &mut IdGenerator,
    ) -> usize {
        let filename = unique_chapter_filename(&self.chapters, &slugify(&title));
        let index = self.chapters.len();
        self.chapters.push(Chapter {
            id: ids.next_id(),
            filename,
            title,
            group,
            order_index: index as i32,
            quests: Vec::new(),
            extra: BTreeMap::new(),
        });
        index
    }

    pub fn add_quest(&mut self, chapter_index: usize, position: (f64, f64), ids: &mut IdGenerator) {
        if let Some(chapter) = self.chapters.get_mut(chapter_index) {
            chapter.quests.push(Quest {
                id: ids.next_id(),
                title: "Новый квест".to_owned(),
                description: String::new(),
                description_raw: None,
                x: position.0,
                y: position.1,
                size: 1.0,
                shape: "circle".to_owned(),
                icon: String::new(),
                icon_raw: None,
                dependencies: Vec::new(),
                tasks: Vec::new(),
                rewards: Vec::new(),
                settings: QuestSettings::default(),
                extra: BTreeMap::new(),
            });
        }
    }

    pub fn duplicate_quests(
        &mut self,
        chapter_index: usize,
        templates: &[Quest],
        offset: (f64, f64),
        ids: &mut IdGenerator,
    ) -> Vec<String> {
        let Some(chapter) = self.chapters.get_mut(chapter_index) else {
            return Vec::new();
        };
        let id_map = templates
            .iter()
            .map(|quest| (quest.id.clone(), ids.next_id()))
            .collect::<HashMap<_, _>>();
        let mut new_ids = Vec::with_capacity(templates.len());

        for template in templates {
            let mut quest = template.clone();
            quest.id = id_map[&template.id].clone();
            quest.x += offset.0;
            quest.y += offset.1;
            quest.dependencies = quest
                .dependencies
                .into_iter()
                .map(|dependency| id_map.get(&dependency).cloned().unwrap_or(dependency))
                .collect();
            for task in &mut quest.tasks {
                assign_task_id(task, ids.next_id());
            }
            for reward in &mut quest.rewards {
                assign_reward_id(reward, ids.next_id());
            }
            new_ids.push(quest.id.clone());
            chapter.quests.push(quest);
        }
        new_ids
    }

    pub fn save(&self, destination: &Path) -> Result<(), EditorError> {
        let root = quests_root(destination);
        fs::create_dir_all(root.join("chapters"))?;
        fs::create_dir_all(root.join("reward_tables"))?;
        for file in &self.preserved_files {
            let path = root.join(&file.relative_path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, &file.contents)?;
        }
        save_translations(destination, &self.translations)?;

        fs::write(root.join("data.snbt"), self.data_snbt())?;
        fs::write(root.join("chapter_groups.snbt"), self.chapter_groups_snbt())?;
        for chapter in &self.chapters {
            fs::write(
                root.join("chapters")
                    .join(format!("{}.snbt", chapter.filename)),
                self.chapter_snbt(chapter),
            )?;
        }
        Ok(())
    }

    fn data_snbt(&self) -> String {
        format!(
            "{{\n\
             \tdefault_autoclaim_rewards: \"disabled\"\n\
             \tdefault_consume_items: false\n\
             \tdefault_quest_shape: \"circle\"\n\
             \tgrid_scale: {}d\n\
             \tprogression_mode: \"flexible\"\n\
             \ttitle: {}\n\
             \tversion: 13\n\
             }}\n",
            format_number(self.grid_scale),
            quoted(&self.title)
        )
    }

    fn chapter_groups_snbt(&self) -> String {
        let mut output = String::from("{\n\tchapter_groups: [\n");
        for group in &self.groups {
            output.push_str(&format!(
                "\t\t{{ id: {}, title: {} }}\n",
                quoted(&group.id),
                quoted(&group.title)
            ));
        }
        output.push_str("\t]\n}\n");
        output
    }

    fn chapter_snbt(&self, chapter: &Chapter) -> String {
        let mut output = String::new();
        output.push_str("{\n");
        write_extra_fields(&mut output, &chapter.extra, 1);
        if !chapter.extra.contains_key("default_hide_dependency_lines") {
            output.push_str("\tdefault_hide_dependency_lines: false\n");
        }
        if !chapter.extra.contains_key("default_quest_shape") {
            output.push_str("\tdefault_quest_shape: \"\"\n");
        }
        output.push_str(&format!("\tfilename: {}\n", quoted(&chapter.filename)));
        output.push_str(&format!(
            "\tgroup: {}\n",
            quoted(chapter.group.as_deref().unwrap_or(""))
        ));
        output.push_str(&format!("\tid: {}\n", quoted(&chapter.id)));
        output.push_str(&format!("\torder_index: {}\n", chapter.order_index));
        if !chapter.extra.contains_key("progression_mode") {
            output.push_str("\tprogression_mode: \"flexible\"\n");
        }
        if !chapter.extra.contains_key("quest_links") {
            output.push_str("\tquest_links: [ ]\n");
        }
        output.push_str("\tquests: [\n");
        for quest in &chapter.quests {
            write_quest(&mut output, quest);
        }
        output.push_str("\t]\n");
        output.push_str(&format!("\ttitle: {}\n", quoted(&chapter.title)));
        output.push_str("}\n");
        output
    }
}

fn import_task(value: &Value, report: &mut ImportReport) -> Option<Task> {
    let id = scalar(value, "id").unwrap_or("0").to_owned();
    let title = scalar(value, "title").unwrap_or("").to_owned();
    match scalar(value, "type")? {
        "checkmark" => Some(Task::Checkmark {
            id,
            title,
            raw: Some(value.clone()),
        }),
        "item" => Some(Task::Item {
            id,
            title,
            item: item_id(value.get("item")).unwrap_or_default(),
            count: entry_item_count(value),
            raw: Some(value.clone()),
        }),
        "kill" => Some(Task::Kill {
            id,
            title,
            entity: scalar(value, "entity")
                .unwrap_or("minecraft:pig")
                .to_owned(),
            count: unsigned(value, "value").unwrap_or(1),
            raw: Some(value.clone()),
        }),
        kind => {
            report.preserved_tasks += 1;
            Some(Task::Unsupported {
                kind: kind.to_owned(),
                raw: value.clone(),
            })
        }
    }
}

fn import_reward(value: &Value, report: &mut ImportReport) -> Option<Reward> {
    let id = scalar(value, "id").unwrap_or("0").to_owned();
    match scalar(value, "type")? {
        "xp" => Some(Reward::Xp {
            id,
            amount: unsigned(value, "xp").unwrap_or(1),
            raw: Some(value.clone()),
        }),
        "item" => Some(Reward::Item {
            id,
            item: item_id(value.get("item")).unwrap_or_default(),
            count: entry_item_count(value),
            raw: Some(value.clone()),
        }),
        kind => {
            report.preserved_rewards += 1;
            Some(Reward::Unsupported {
                kind: kind.to_owned(),
                raw: value.clone(),
            })
        }
    }
}

fn import_quest_settings(value: &Value) -> QuestSettings {
    QuestSettings {
        subtitle: scalar(value, "subtitle").unwrap_or("").to_owned(),
        tags: list(value, "tags")
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        icon_scale: number(value, "icon_scale").unwrap_or(1.0),
        min_width: integer(value, "min_width").unwrap_or(0),
        dependency_requirement: scalar(value, "dependency_requirement")
            .unwrap_or("all_completed")
            .to_owned(),
        min_required_dependencies: unsigned(value, "min_required_dependencies").unwrap_or(0),
        max_completable_dependents: unsigned(value, "max_completable_dependents").unwrap_or(0),
        hide_until_deps_visible: tristate(value, "hide_until_deps_visible"),
        hide_until_deps_complete: tristate(value, "hide_until_deps_complete"),
        hide_dependency_lines: tristate(value, "hide_dependency_lines"),
        hide_dependent_lines: boolean(value, "hide_dependent_lines"),
        hide_details_until_startable: tristate(value, "hide_details_until_startable"),
        hide_text_until_complete: tristate(value, "hide_text_until_complete"),
        hide_lock_icon: boolean(value, "hide_lock_icon"),
        invisible: boolean(value, "invisible"),
        invisible_until_tasks: unsigned(value, "invisible_until_tasks").unwrap_or(0),
        optional: boolean(value, "optional"),
        can_repeat: tristate(value, "can_repeat"),
        repeat_cooldown: unsigned(value, "repeat_cooldown").unwrap_or(0),
        ignore_reward_blocking: boolean(value, "ignore_reward_blocking"),
        progression_mode: scalar(value, "progression_mode")
            .unwrap_or("default")
            .to_owned(),
        require_sequential_tasks: tristate(value, "require_sequential_tasks"),
        disable_recipe_mod: tristate(value, "disable_recipe_mod"),
    }
}

fn extra_fields(value: &Value, managed: &[&str]) -> BTreeMap<String, Value> {
    value
        .as_compound()
        .into_iter()
        .flat_map(|compound| compound.iter())
        .filter(|(key, _)| !managed.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn add_quest<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    book: &mut EditableBook,
    ids: &mut IdGenerator,
) -> Result<(), EditorError> {
    let title = prompt_required(input, output, "Название квеста: ", "Название квеста")?;
    let description = prompt(input, output, "Описание (можно оставить пустым): ")?;
    let index = book.chapters[0].quests.len();
    let quest = Quest {
        id: ids.next_id(),
        title,
        description,
        description_raw: None,
        x: (index % 6) as f64 * 2.5,
        y: (index / 6) as f64 * 2.5,
        size: 1.0,
        shape: "circle".to_owned(),
        icon: String::new(),
        icon_raw: None,
        dependencies: Vec::new(),
        tasks: Vec::new(),
        rewards: Vec::new(),
        settings: QuestSettings::default(),
        extra: BTreeMap::new(),
    };
    writeln!(output, "Добавлен квест #{} с ID {}.", index + 1, quest.id)?;
    book.chapters[0].quests.push(quest);
    Ok(())
}

fn add_task<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    book: &mut EditableBook,
    ids: &mut IdGenerator,
) -> Result<(), EditorError> {
    let Some(index) = choose_quest(input, output, book, "К какому квесту добавить задачу?")?
    else {
        return Ok(());
    };
    writeln!(output, "Тип задачи: 1 — отметка, 2 — предмет, 3 — убийство")?;
    let kind = prompt(input, output, "Тип: ")?;
    let title = prompt_required(input, output, "Название задачи: ", "Название задачи")?;
    let task = match kind.trim() {
        "1" => Task::Checkmark {
            id: ids.next_id(),
            title,
            raw: None,
        },
        "2" => Task::Item {
            id: ids.next_id(),
            title,
            item: prompt_required(
                input,
                output,
                "ID предмета (например minecraft:stone): ",
                "ID предмета",
            )?,
            count: prompt_u32(input, output, "Количество [1]: ", 1)?,
            raw: None,
        },
        "3" => Task::Kill {
            id: ids.next_id(),
            title,
            entity: prompt_required(
                input,
                output,
                "ID сущности (например minecraft:zombie): ",
                "ID сущности",
            )?,
            count: prompt_u32(input, output, "Количество [1]: ", 1)?,
            raw: None,
        },
        _ => {
            writeln!(output, "Неизвестный тип задачи.")?;
            return Ok(());
        }
    };
    book.chapters[0].quests[index].tasks.push(task);
    writeln!(output, "Задача добавлена.")?;
    Ok(())
}

fn add_dependency<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    book: &mut EditableBook,
) -> Result<(), EditorError> {
    if book.chapters[0].quests.len() < 2 {
        writeln!(output, "Для связи нужно хотя бы два квеста.")?;
        return Ok(());
    }
    let Some(quest_index) = choose_quest(input, output, book, "Какой квест будет зависимым?")?
    else {
        return Ok(());
    };
    let Some(required_index) =
        choose_quest(input, output, book, "Какой квест нужно выполнить раньше?")?
    else {
        return Ok(());
    };
    if quest_index == required_index {
        writeln!(output, "Квест не может зависеть от самого себя.")?;
        return Ok(());
    }
    let required_id = book.chapters[0].quests[required_index].id.clone();
    if !book.chapters[0].quests[quest_index]
        .dependencies
        .contains(&required_id)
    {
        book.chapters[0].quests[quest_index]
            .dependencies
            .push(required_id);
    }
    writeln!(output, "Зависимость добавлена.")?;
    Ok(())
}

fn add_reward<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    book: &mut EditableBook,
    ids: &mut IdGenerator,
) -> Result<(), EditorError> {
    let Some(index) = choose_quest(input, output, book, "К какому квесту добавить награду?")?
    else {
        return Ok(());
    };
    writeln!(output, "Тип награды: 1 — опыт, 2 — предмет")?;
    let kind = prompt(input, output, "Тип: ")?;
    let reward = match kind.trim() {
        "1" => Reward::Xp {
            id: ids.next_id(),
            amount: prompt_u32(input, output, "Количество XP [10]: ", 10)?,
            raw: None,
        },
        "2" => Reward::Item {
            id: ids.next_id(),
            item: prompt_required(input, output, "ID предмета: ", "ID предмета")?,
            count: prompt_u32(input, output, "Количество [1]: ", 1)?,
            raw: None,
        },
        _ => {
            writeln!(output, "Неизвестный тип награды.")?;
            return Ok(());
        }
    };
    book.chapters[0].quests[index].rewards.push(reward);
    writeln!(output, "Награда добавлена.")?;
    Ok(())
}

fn choose_quest<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    book: &EditableBook,
    question: &str,
) -> Result<Option<usize>, EditorError> {
    if book.chapters[0].quests.is_empty() {
        writeln!(output, "Сначала добавьте хотя бы один квест.")?;
        return Ok(None);
    }
    writeln!(output, "{question}")?;
    for (index, quest) in book.chapters[0].quests.iter().enumerate() {
        writeln!(
            output,
            "  {}. {} [{}]",
            index + 1,
            book.translations.resolve(&quest.title),
            quest.id
        )?;
    }
    loop {
        let value = prompt(input, output, "Номер квеста (0 — отмена): ")?;
        match value.trim().parse::<usize>() {
            Ok(0) => return Ok(None),
            Ok(number) if number <= book.chapters[0].quests.len() => {
                return Ok(Some(number - 1));
            }
            _ => writeln!(output, "Введите номер из списка.")?,
        }
    }
}

fn print_book<W: Write>(output: &mut W, book: &EditableBook) -> Result<(), EditorError> {
    writeln!(
        output,
        "\nКнига: {}\nГлава: {} ({})",
        book.translations.resolve(&book.title),
        book.translations.resolve(&book.chapters[0].title),
        book.chapters[0].filename
    )?;
    if book.chapters[0].quests.is_empty() {
        writeln!(output, "Квестов пока нет.")?;
        return Ok(());
    }
    for (index, quest) in book.chapters[0].quests.iter().enumerate() {
        writeln!(
            output,
            "{}. {} [{}] — задач: {}, наград: {}, зависимостей: {}",
            index + 1,
            book.translations.resolve(&quest.title),
            quest.id,
            quest.tasks.len(),
            quest.rewards.len(),
            quest.dependencies.len()
        )?;
    }
    Ok(())
}

fn write_quest(output: &mut String, quest: &Quest) {
    output.push_str("\t\t{\n");
    write_extra_fields(output, &quest.extra, 3);
    if !quest.dependencies.is_empty() {
        output.push_str("\t\t\tdependencies: [");
        for (index, dependency) in quest.dependencies.iter().enumerate() {
            if index > 0 {
                output.push(' ');
            }
            output.push_str(&quoted(dependency));
        }
        output.push_str("]\n");
    }
    if !quest.description.is_empty() {
        let raw_description = quest.description_raw.as_ref().filter(|raw| {
            raw.as_list()
                .unwrap_or_default()
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
                == quest.description
        });
        if let Some(raw) = raw_description {
            output.push_str(&format!("\t\t\tdescription: {}\n", snbt::stringify(raw)));
        } else {
            output.push_str(&format!(
                "\t\t\tdescription: [{}]\n",
                quoted(&quest.description)
            ));
        }
    }
    if !quest.icon.is_empty() {
        let raw_icon = quest
            .icon_raw
            .as_ref()
            .filter(|raw| item_id(Some(raw)) == Some(quest.icon.clone()));
        if let Some(raw) = raw_icon {
            output.push_str(&format!("\t\t\ticon: {}\n", snbt::stringify(raw)));
        } else {
            output.push_str(&format!("\t\t\ticon: {}\n", quoted(&quest.icon)));
        }
    }
    write_quest_settings(output, &quest.settings);
    output.push_str(&format!("\t\t\tid: {}\n", quoted(&quest.id)));
    output.push_str("\t\t\trewards: [\n");
    for reward in &quest.rewards {
        write_reward(output, reward);
    }
    output.push_str("\t\t\t]\n");
    if !quest.shape.is_empty() {
        output.push_str(&format!("\t\t\tshape: {}\n", quoted(&quest.shape)));
    }
    if (quest.size - 1.0).abs() > f64::EPSILON {
        output.push_str(&format!("\t\t\tsize: {}d\n", format_number(quest.size)));
    }
    output.push_str("\t\t\ttasks: [\n");
    for task in &quest.tasks {
        write_task(output, task);
    }
    output.push_str("\t\t\t]\n");
    output.push_str(&format!("\t\t\ttitle: {}\n", quoted(&quest.title)));
    output.push_str(&format!("\t\t\tx: {}d\n", format_number(quest.x)));
    output.push_str(&format!("\t\t\ty: {}d\n", format_number(quest.y)));
    output.push_str("\t\t}\n");
}

fn write_quest_settings(output: &mut String, settings: &QuestSettings) {
    write_string_field(output, "subtitle", &settings.subtitle, 3);
    if !settings.tags.is_empty() {
        output.push_str("\t\t\ttags: [");
        for (index, tag) in settings.tags.iter().enumerate() {
            if index > 0 {
                output.push(' ');
            }
            output.push_str(&quoted(tag));
        }
        output.push_str("]\n");
    }
    write_non_default_f64(output, "icon_scale", settings.icon_scale, 1.0, 3);
    write_positive_i32(output, "min_width", settings.min_width, 3);
    if settings.dependency_requirement != "all_completed" {
        write_string_field(
            output,
            "dependency_requirement",
            &settings.dependency_requirement,
            3,
        );
    }
    write_positive_u32(
        output,
        "min_required_dependencies",
        settings.min_required_dependencies,
        3,
    );
    write_positive_u32(
        output,
        "max_completable_dependents",
        settings.max_completable_dependents,
        3,
    );
    for (key, value) in [
        ("hide_dependent_lines", settings.hide_dependent_lines),
        ("hide_lock_icon", settings.hide_lock_icon),
        ("invisible", settings.invisible),
        ("optional", settings.optional),
        ("ignore_reward_blocking", settings.ignore_reward_blocking),
    ] {
        if value {
            output.push_str(&format!("\t\t\t{key}: true\n"));
        }
    }
    for (key, value) in [
        ("hide_until_deps_visible", settings.hide_until_deps_visible),
        (
            "hide_until_deps_complete",
            settings.hide_until_deps_complete,
        ),
        ("hide_dependency_lines", settings.hide_dependency_lines),
        (
            "hide_details_until_startable",
            settings.hide_details_until_startable,
        ),
        (
            "hide_text_until_complete",
            settings.hide_text_until_complete,
        ),
        ("can_repeat", settings.can_repeat),
        (
            "require_sequential_tasks",
            settings.require_sequential_tasks,
        ),
        ("disable_recipe_mod", settings.disable_recipe_mod),
    ] {
        write_tristate(output, key, value, 3);
    }
    write_positive_u32(
        output,
        "invisible_until_tasks",
        settings.invisible_until_tasks,
        3,
    );
    write_positive_u32(output, "repeat_cooldown", settings.repeat_cooldown, 3);
    if settings.progression_mode != "default" {
        write_string_field(output, "progression_mode", &settings.progression_mode, 3);
    }
}

fn write_extra_fields(output: &mut String, fields: &BTreeMap<String, Value>, indent: usize) {
    let indentation = "\t".repeat(indent);
    for (key, value) in fields {
        output.push_str(&indentation);
        output.push_str(key);
        output.push_str(": ");
        output.push_str(&snbt::stringify(value));
        output.push('\n');
    }
}

fn write_string_field(output: &mut String, key: &str, value: &str, indent: usize) {
    if !value.is_empty() {
        output.push_str(&format!(
            "{}{key}: {}\n",
            "\t".repeat(indent),
            quoted(value)
        ));
    }
}

fn write_non_default_f64(output: &mut String, key: &str, value: f64, default: f64, indent: usize) {
    if (value - default).abs() > f64::EPSILON {
        output.push_str(&format!(
            "{}{key}: {}d\n",
            "\t".repeat(indent),
            format_number(value)
        ));
    }
}

fn write_positive_i32(output: &mut String, key: &str, value: i32, indent: usize) {
    if value > 0 {
        output.push_str(&format!("{}{key}: {value}\n", "\t".repeat(indent)));
    }
}

fn write_positive_u32(output: &mut String, key: &str, value: u32, indent: usize) {
    if value > 0 {
        output.push_str(&format!("{}{key}: {value}\n", "\t".repeat(indent)));
    }
}

fn write_tristate(output: &mut String, key: &str, value: Tristate, indent: usize) {
    match value {
        Tristate::Default => {}
        Tristate::True => {
            output.push_str(&format!("{}{key}: true\n", "\t".repeat(indent)));
        }
        Tristate::False => {
            output.push_str(&format!("{}{key}: false\n", "\t".repeat(indent)));
        }
    }
}

fn write_task(output: &mut String, task: &Task) {
    if let Some(value) = merged_task_value(task) {
        output.push_str("\t\t\t\t");
        output.push_str(&snbt::stringify(&value));
        output.push('\n');
        return;
    }

    output.push_str("\t\t\t\t{\n");
    match task {
        Task::Checkmark { id, title, .. } => {
            output.push_str(&format!("\t\t\t\t\tid: {}\n", quoted(id)));
            output.push_str(&format!("\t\t\t\t\ttitle: {}\n", quoted(title)));
            output.push_str("\t\t\t\t\ttype: \"checkmark\"\n");
        }
        Task::Item {
            id,
            title,
            item,
            count,
            ..
        } => {
            output.push_str(&format!("\t\t\t\t\tcount: {count}L\n"));
            output.push_str(&format!("\t\t\t\t\tid: {}\n", quoted(id)));
            output.push_str(&format!("\t\t\t\t\titem: {}\n", quoted(item)));
            output.push_str(&format!("\t\t\t\t\ttitle: {}\n", quoted(title)));
            output.push_str("\t\t\t\t\ttype: \"item\"\n");
        }
        Task::Kill {
            id,
            title,
            entity,
            count,
            ..
        } => {
            output.push_str(&format!("\t\t\t\t\tentity: {}\n", quoted(entity)));
            output.push_str(&format!("\t\t\t\t\tid: {}\n", quoted(id)));
            output.push_str(&format!("\t\t\t\t\ttitle: {}\n", quoted(title)));
            output.push_str("\t\t\t\t\ttype: \"kill\"\n");
            output.push_str(&format!("\t\t\t\t\tvalue: {count}L\n"));
        }
        Task::Unsupported { .. } => unreachable!(),
    }
    output.push_str("\t\t\t\t}\n");
}

fn write_reward(output: &mut String, reward: &Reward) {
    if let Some(value) = merged_reward_value(reward) {
        output.push_str("\t\t\t\t");
        output.push_str(&snbt::stringify(&value));
        output.push('\n');
        return;
    }

    output.push_str("\t\t\t\t{\n");
    match reward {
        Reward::Xp { id, amount, .. } => {
            output.push_str(&format!("\t\t\t\t\tid: {}\n", quoted(id)));
            output.push_str("\t\t\t\t\ttype: \"xp\"\n");
            output.push_str(&format!("\t\t\t\t\txp: {amount}\n"));
        }
        Reward::Item {
            id, item, count, ..
        } => {
            output.push_str(&format!("\t\t\t\t\tcount: {count}\n"));
            output.push_str(&format!("\t\t\t\t\tid: {}\n", quoted(id)));
            output.push_str(&format!("\t\t\t\t\titem: {}\n", quoted(item)));
            output.push_str("\t\t\t\t\ttype: \"item\"\n");
        }
        Reward::Unsupported { .. } => unreachable!(),
    }
    output.push_str("\t\t\t\t}\n");
}

fn merged_task_value(task: &Task) -> Option<Value> {
    match task {
        Task::Unsupported { raw, .. } => Some(raw.clone()),
        Task::Checkmark {
            id,
            title,
            raw: Some(raw),
        } => Some(merge_fields(
            raw,
            [
                ("id", id.as_str()),
                ("title", title.as_str()),
                ("type", "checkmark"),
            ],
        )),
        Task::Item {
            id,
            title,
            item,
            count,
            raw: Some(raw),
        } => {
            let mut value = merge_fields(
                raw,
                [
                    ("id", id.as_str()),
                    ("title", title.as_str()),
                    ("type", "item"),
                ],
            );
            merge_item_fields(&mut value, raw, item, *count);
            Some(value)
        }
        Task::Kill {
            id,
            title,
            entity,
            count,
            raw: Some(raw),
        } => {
            let mut value = merge_fields(
                raw,
                [
                    ("id", id.as_str()),
                    ("title", title.as_str()),
                    ("entity", entity.as_str()),
                    ("type", "kill"),
                ],
            );
            set_scalar(&mut value, "value", format!("{count}L"));
            Some(value)
        }
        _ => None,
    }
}

fn merged_reward_value(reward: &Reward) -> Option<Value> {
    match reward {
        Reward::Unsupported { raw, .. } => Some(raw.clone()),
        Reward::Xp {
            id,
            amount,
            raw: Some(raw),
        } => {
            let mut value = merge_fields(raw, [("id", id.as_str()), ("type", "xp")]);
            set_scalar(&mut value, "xp", amount.to_string());
            Some(value)
        }
        Reward::Item {
            id,
            item,
            count,
            raw: Some(raw),
        } => {
            let mut value = merge_fields(raw, [("id", id.as_str()), ("type", "item")]);
            merge_item_fields(&mut value, raw, item, *count);
            Some(value)
        }
        _ => None,
    }
}

fn merge_fields<'a>(raw: &Value, fields: impl IntoIterator<Item = (&'a str, &'a str)>) -> Value {
    let mut value = raw.clone();
    for (key, field_value) in fields {
        set_scalar(&mut value, key, field_value.to_owned());
    }
    value
}

fn merge_item_fields(value: &mut Value, raw: &Value, item: &str, count: u32) {
    let original_item = item_id(raw.get("item")).unwrap_or_default();
    let original_count = entry_item_count(raw);
    if item != original_item
        && let Some(compound) = value.as_compound_mut()
    {
        compound.insert("item".to_owned(), Value::Scalar(item.to_owned()));
    }
    if count != original_count {
        set_scalar(value, "count", format!("{count}L"));
    }
}

fn set_scalar(value: &mut Value, key: &str, field_value: String) {
    if let Some(compound) = value.as_compound_mut() {
        compound.insert(key.to_owned(), Value::Scalar(field_value));
    }
}

fn assign_task_id(task: &mut Task, id: String) {
    match task {
        Task::Checkmark {
            id: task_id, raw, ..
        }
        | Task::Item {
            id: task_id, raw, ..
        }
        | Task::Kill {
            id: task_id, raw, ..
        } => {
            *task_id = id.clone();
            if let Some(raw) = raw {
                set_scalar(raw, "id", id);
            }
        }
        Task::Unsupported { raw, .. } => set_scalar(raw, "id", id),
    }
}

fn assign_reward_id(reward: &mut Reward, id: String) {
    match reward {
        Reward::Xp {
            id: reward_id, raw, ..
        }
        | Reward::Item {
            id: reward_id, raw, ..
        } => {
            *reward_id = id.clone();
            if let Some(raw) = raw {
                set_scalar(raw, "id", id);
            }
        }
        Reward::Unsupported { raw, .. } => set_scalar(raw, "id", id),
    }
}

fn find_quests_root(path: &Path) -> Result<PathBuf, EditorError> {
    if path.join("data.snbt").is_file() {
        Ok(path.to_owned())
    } else if path.join("quests").join("data.snbt").is_file() {
        Ok(path.join("quests"))
    } else {
        Err(EditorError::InvalidBook(path.to_owned()))
    }
}

fn read_snbt(path: &Path) -> Result<Value, EditorError> {
    let source = fs::read_to_string(path)?;
    snbt::parse(&source).map_err(|source| EditorError::Parse {
        path: path.to_owned(),
        source,
    })
}

fn snbt_files(directory: &Path) -> Result<Vec<PathBuf>, EditorError> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "snbt")
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn scalar<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn list<'a>(value: &'a Value, key: &str) -> Option<&'a [Value]> {
    value.get(key)?.as_list()
}

fn number(value: &Value, key: &str) -> Option<f64> {
    parse_number(scalar(value, key)?)
}

fn integer(value: &Value, key: &str) -> Option<i32> {
    let number = parse_number(scalar(value, key)?)?.round();
    (number >= i32::MIN as f64 && number <= i32::MAX as f64).then_some(number as i32)
}

fn unsigned(value: &Value, key: &str) -> Option<u32> {
    let number = parse_number(scalar(value, key)?)?;
    (number >= 0.0).then_some(number.round() as u32)
}

fn boolean(value: &Value, key: &str) -> bool {
    scalar(value, key).is_some_and(|value| value.eq_ignore_ascii_case("true") || value == "1b")
}

fn tristate(value: &Value, key: &str) -> Tristate {
    match scalar(value, key) {
        Some(value) if value.eq_ignore_ascii_case("true") || value == "1b" => Tristate::True,
        Some(value) if value.eq_ignore_ascii_case("false") || value == "0b" => Tristate::False,
        _ => Tristate::Default,
    }
}

fn parse_number(value: &str) -> Option<f64> {
    value
        .trim_end_matches(|character: char| {
            matches!(
                character,
                'b' | 'B' | 's' | 'S' | 'l' | 'L' | 'f' | 'F' | 'd' | 'D'
            )
        })
        .parse()
        .ok()
}

fn item_id(value: Option<&Value>) -> Option<String> {
    let value = value?;
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.get("id")?.as_str().map(str::to_owned))
}

fn entry_item_count(value: &Value) -> u32 {
    unsigned(value, "count")
        .or_else(|| value.get("item").and_then(|item| unsigned(item, "count")))
        .unwrap_or(1)
}

fn load_preserved_files(root: &Path) -> Result<Vec<PreservedFile>, EditorError> {
    let mut files = Vec::new();
    let reward_tables = root.join("reward_tables");
    if reward_tables.is_dir() {
        collect_preserved_files(root, &reward_tables, &mut files)?;
    }
    Ok(files)
}

fn collect_preserved_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PreservedFile>,
) -> Result<(), EditorError> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_preserved_files(root, &path, files)?;
        } else {
            files.push(PreservedFile {
                relative_path: path.strip_prefix(root).unwrap_or(&path).to_owned(),
                contents: fs::read(path)?,
            });
        }
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(())
}

fn load_translations(quests_root: &Path) -> Result<TranslationCatalog, EditorError> {
    let project_root = quests_root.parent().unwrap_or(quests_root);
    let lang_root = project_root.join("kubejs").join("lang");
    let mut catalog = TranslationCatalog::default();
    if !lang_root.is_dir() {
        return Ok(catalog);
    }

    for entry in fs::read_dir(lang_root)? {
        let path = entry?.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let Some(locale) = path
            .file_stem()
            .map(|name| normalize_locale(&name.to_string_lossy()))
        else {
            continue;
        };
        let values = serde_json::from_slice::<BTreeMap<String, String>>(&fs::read(&path)?)
            .map_err(|error| {
                EditorError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("ошибка JSON в {}: {error}", path.display()),
                ))
            })?;
        catalog.languages.insert(locale, values);
    }
    if !catalog.languages.contains_key(&catalog.active_locale)
        && let Some(locale) = catalog.languages.keys().next()
    {
        catalog.active_locale = locale.clone();
    }
    Ok(catalog)
}

fn save_translations(
    destination: &Path,
    translations: &TranslationCatalog,
) -> Result<(), EditorError> {
    if translations.languages.is_empty() {
        return Ok(());
    }
    let project_root = if destination.file_name().is_some_and(|name| name == "quests") {
        destination.parent().unwrap_or(destination)
    } else {
        destination
    };
    let lang_root = project_root.join("kubejs").join("lang");
    fs::create_dir_all(&lang_root)?;
    for (locale, values) in &translations.languages {
        let file = fs::File::create(lang_root.join(format!("{locale}.json")))?;
        serde_json::to_writer(file, values).map_err(|error| {
            EditorError::Io(io::Error::other(format!(
                "не удалось записать перевод {locale}: {error}"
            )))
        })?;
    }
    Ok(())
}

fn translation_key(value: &str) -> Option<&str> {
    let value = value.trim();
    value
        .strip_prefix('{')?
        .strip_suffix('}')
        .filter(|key| !key.is_empty() && !key.contains(['{', '}']))
}

fn resolve_translation_tags(
    value: &str,
    selected: Option<&BTreeMap<String, String>>,
    fallback: Option<&BTreeMap<String, String>>,
) -> String {
    let mut output = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(start) = remaining.find('{') {
        output.push_str(&remaining[..start]);
        let after_start = &remaining[start + 1..];
        let Some(end) = after_start.find('}') else {
            output.push_str(&remaining[start..]);
            return output;
        };
        let key = &after_start[..end];
        if key.is_empty() || key.contains('{') {
            output.push_str(&remaining[start..start + end + 2]);
        } else if let Some(translation) = selected
            .and_then(|language| language.get(key))
            .or_else(|| fallback.and_then(|language| language.get(key)))
        {
            output.push_str(translation);
        } else {
            output.push_str(&remaining[start..start + end + 2]);
        }
        remaining = &after_start[end + 1..];
    }
    output.push_str(remaining);
    translation_display_text(&output)
}

pub fn translation_display_text(value: &str) -> String {
    value.replace("\\n", "\n")
}

pub fn translation_storage_text(value: &str) -> String {
    value.replace('\n', "\\n")
}

fn normalize_locale(locale: &str) -> String {
    locale
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect()
}

fn quests_root(destination: &Path) -> PathBuf {
    if destination.file_name().is_some_and(|name| name == "quests") {
        destination.to_owned()
    } else {
        destination.join("quests")
    }
}

fn prompt<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    message: &str,
) -> Result<String, EditorError> {
    write!(output, "{message}")?;
    output.flush()?;
    let mut value = String::new();
    input.read_line(&mut value)?;
    Ok(value.trim_end_matches(['\r', '\n']).to_owned())
}

fn prompt_required<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    message: &str,
    field: &'static str,
) -> Result<String, EditorError> {
    let value = prompt(input, output, message)?;
    if value.trim().is_empty() {
        Err(EditorError::EmptyInput(field))
    } else {
        Ok(value)
    }
}

fn prompt_default<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    message: &str,
    default: &str,
) -> Result<String, EditorError> {
    let value = prompt(input, output, message)?;
    if value.trim().is_empty() {
        Ok(default.to_owned())
    } else {
        Ok(slugify(&value))
    }
}

fn prompt_u32<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    message: &str,
    default: u32,
) -> Result<u32, EditorError> {
    loop {
        let value = prompt(input, output, message)?;
        if value.trim().is_empty() {
            return Ok(default);
        }
        match value.trim().parse::<u32>() {
            Ok(number) if number > 0 => return Ok(number),
            _ => writeln!(output, "Введите положительное целое число.")?,
        }
    }
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut previous_separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            previous_separator = false;
        } else if !previous_separator && !slug.is_empty() {
            slug.push('_');
            previous_separator = true;
        }
    }
    while slug.ends_with('_') {
        slug.pop();
    }
    if slug.is_empty() {
        "main".to_owned()
    } else {
        slug
    }
}

fn unique_chapter_filename(chapters: &[Chapter], base: &str) -> String {
    if !chapters.iter().any(|chapter| chapter.filename == base) {
        return base.to_owned();
    }
    for suffix in 2.. {
        let candidate = format!("{base}_{suffix}");
        if !chapters.iter().any(|chapter| chapter.filename == candidate) {
            return candidate;
        }
    }
    unreachable!()
}

fn format_number(value: f64) -> String {
    let mut text = format!("{value:.4}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.push('0');
    }
    text
}

pub fn snap_coordinate(value: f64, grid_scale: f64, object_size: f64) -> f64 {
    if grid_scale <= 0.0 || object_size <= 0.0 {
        return value;
    }
    let snap = 1.0 / (grid_scale * object_size);
    (value * snap).round() / snap
}

fn quoted(value: &str) -> String {
    let mut result = String::with_capacity(value.len() + 2);
    result.push('"');
    for character in value.chars() {
        match character {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            character => result.push(character),
        }
    }
    result.push('"');
    result
}

pub struct IdGenerator {
    state: u64,
}

impl IdGenerator {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        Self {
            state: nanos ^ u64::from(std::process::id()) ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    pub fn next_id(&mut self) -> String {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        format!("{:016X}", self.state)
    }
}

impl Default for IdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Cursor;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::book::QuestBook;

    use super::{EditableBook, IdGenerator, Reward, Task, Tristate, run_editor, snap_coordinate};

    #[test]
    fn duplicates_quests_with_fresh_ids_and_remapped_internal_dependencies() {
        let mut ids = IdGenerator::new();
        let mut book = EditableBook::blank(&mut ids);
        book.add_quest(0, (1.0, 2.0), &mut ids);
        book.add_quest(0, (3.0, 4.0), &mut ids);
        let original_ids = book.chapters[0]
            .quests
            .iter()
            .map(|quest| quest.id.clone())
            .collect::<Vec<_>>();
        book.chapters[0].quests[0].tasks.push(Task::Item {
            id: ids.next_id(),
            title: "Item".to_owned(),
            item: "minecraft:stone".to_owned(),
            count: 1,
            raw: None,
        });
        book.chapters[0].quests[1].rewards.push(Reward::Xp {
            id: ids.next_id(),
            amount: 5,
            raw: None,
        });
        book.chapters[0].quests[1]
            .dependencies
            .push(original_ids[0].clone());
        let templates = book.chapters[0].quests.clone();

        let copied_ids = book.duplicate_quests(0, &templates, (10.0, -2.0), &mut ids);

        assert_eq!(copied_ids.len(), 2);
        assert!(!original_ids.contains(&copied_ids[0]));
        assert_eq!(book.chapters[0].quests[2].x, 11.0);
        assert_eq!(book.chapters[0].quests[3].y, 2.0);
        assert_eq!(
            book.chapters[0].quests[3].dependencies,
            vec![copied_ids[0].clone()]
        );
        let Task::Item { id, .. } = &book.chapters[0].quests[2].tasks[0] else {
            panic!("expected item task");
        };
        let Task::Item {
            id: original_task_id,
            ..
        } = &book.chapters[0].quests[0].tasks[0]
        else {
            panic!("expected item task");
        };
        assert_ne!(id, original_task_id);
    }

    #[test]
    fn creates_book_with_tasks_reward_and_dependency() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ftbgui-editor-{unique}"));
        let script = "\
Test Book\n\
Main Chapter\n\
\n\
1\n\
First Quest\n\
Start here\n\
2\n\
1\n\
1\n\
Press the button\n\
4\n\
1\n\
1\n\
25\n\
1\n\
Second Quest\n\
Continue\n\
2\n\
2\n\
2\n\
Bring stone\n\
minecraft:stone\n\
4\n\
3\n\
2\n\
1\n\
0\n";
        let mut transcript = Vec::new();
        run_editor(Cursor::new(script), &mut transcript, &root).unwrap();

        let book = QuestBook::load(&root).unwrap();
        assert_eq!(book.chapters, 1);
        assert_eq!(book.quests, 2);
        assert_eq!(book.tasks, 2);
        assert_eq!(book.rewards, 1);
        assert_eq!(book.dependencies, 1);
        assert_eq!(book.broken_dependencies, 0);
        assert_eq!(book.task_types.get("checkmark"), Some(&1));
        assert_eq!(book.task_types.get("item"), Some(&1));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn snapping_matches_ftb_grid_formula() {
        assert_eq!(snap_coordinate(1.24, 0.5, 1.0), 1.0);
        assert_eq!(snap_coordinate(1.26, 0.5, 1.0), 1.5);
        assert_eq!(snap_coordinate(1.24, 0.5, 2.0), 1.0);
    }

    #[test]
    fn saves_multiple_groups_and_chapters() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ftbgui-multi-{unique}"));
        let mut ids = IdGenerator::new();
        let mut editable = EditableBook::blank(&mut ids);
        let group = editable.add_group("Machines".to_owned(), &mut ids);
        editable.chapters[0].group = Some(group.clone());
        let second = editable.add_chapter("Magic".to_owned(), None, &mut ids);
        editable.add_quest(0, (1.5, -2.0), &mut ids);
        editable.add_quest(second, (4.0, 3.5), &mut ids);
        editable.save(&root).unwrap();

        let book = QuestBook::load(&root).unwrap();
        assert_eq!(book.groups, 1);
        assert_eq!(book.chapters, 2);
        assert_eq!(book.grouped_chapters, 1);
        assert_eq!(book.quests, 2);
        assert_eq!(book.broken_dependencies, 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn imports_editable_book_and_reports_unsupported_entries() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ftbgui-import-{unique}"));
        let quests = root.join("quests");
        fs::create_dir_all(quests.join("chapters")).unwrap();
        fs::write(
            quests.join("data.snbt"),
            "{ title: \"Imported\" grid_scale: 0.25d version: 13 }",
        )
        .unwrap();
        fs::write(
            quests.join("chapter_groups.snbt"),
            "{ chapter_groups: [{ id: \"GROUP\" title: \"Machines\" }] }",
        )
        .unwrap();
        fs::write(
            quests.join("chapters/main.snbt"),
            r#"{
                filename: "main"
                group: "GROUP"
                id: "CHAPTER"
                order_index: 2
                title: "Main"
                quests: [{
                    id: "QUEST"
                    title: "First"
                    subtitle: "Hover text"
                    tags: ["bluequests" "tutorial"]
                    can_repeat: true
                    repeat_cooldown: 60
                    dependency_requirement: "one_completed"
                    hide_until_deps_visible: true
                    description: ["Line one" "Line two"]
                    x: 1.5d
                    y: -2.0d
                    dependencies: ["OTHER"]
                    tasks: [
                        { id: "TASK" title: "Done" type: "checkmark" }
                        { id: "UNKNOWN_TASK" type: "dimension" }
                    ]
                    rewards: [
                        { id: "REWARD" type: "xp" xp: 25 }
                        { id: "UNKNOWN_REWARD" type: "command" }
                    ]
                }]
            }"#,
        )
        .unwrap();

        let (book, report) = EditableBook::load(&root).unwrap();
        assert_eq!(book.title, "Imported");
        assert_eq!(book.grid_scale, 0.25);
        assert_eq!(book.groups[0].title, "Machines");
        assert_eq!(book.chapters[0].title, "Main");
        assert_eq!(book.chapters[0].group.as_deref(), Some("GROUP"));
        assert_eq!(book.chapters[0].quests[0].description, "Line one\nLine two");
        assert_eq!(book.chapters[0].quests[0].settings.subtitle, "Hover text");
        assert_eq!(
            book.chapters[0].quests[0].settings.tags,
            ["bluequests", "tutorial"]
        );
        assert_eq!(
            book.chapters[0].quests[0].settings.can_repeat,
            Tristate::True
        );
        assert_eq!(book.chapters[0].quests[0].settings.repeat_cooldown, 60);
        assert_eq!(
            book.chapters[0].quests[0].settings.dependency_requirement,
            "one_completed"
        );
        assert_eq!(
            book.chapters[0].quests[0].settings.hide_until_deps_visible,
            Tristate::True
        );
        assert_eq!(book.chapters[0].quests[0].tasks.len(), 2);
        assert_eq!(book.chapters[0].quests[0].rewards.len(), 2);
        assert_eq!(report.preserved_tasks, 1);
        assert_eq!(report.preserved_rewards, 1);

        let exported = root.with_file_name(format!("ftbgui-import-export-{unique}"));
        book.save(&exported).unwrap();
        let (roundtrip, roundtrip_report) = EditableBook::load(&exported).unwrap();
        assert_eq!(roundtrip.chapters[0].quests[0].tasks.len(), 2);
        assert_eq!(roundtrip.chapters[0].quests[0].rewards.len(), 2);
        assert_eq!(roundtrip_report.preserved_tasks, 1);
        assert_eq!(roundtrip_report.preserved_rewards, 1);

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(exported).unwrap();
    }

    #[test]
    fn preserves_reward_tables_and_extended_item_data() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ftbgui-preserve-{unique}"));
        let quests = root.join("quests");
        fs::create_dir_all(quests.join("chapters")).unwrap();
        fs::create_dir_all(quests.join("reward_tables")).unwrap();
        fs::write(
            quests.join("data.snbt"),
            "{ title: \"Preserve\" version: 13 }",
        )
        .unwrap();
        fs::write(
            quests.join("chapters/main.snbt"),
            r#"{
                filename: "main"
                id: "CHAPTER"
                title: "Main"
                quests: [{
                    id: "QUEST"
                    rewards: [{
                        id: "REWARD"
                        item: {
                            components: { "minecraft:custom_name": "Shiny" }
                            count: 3
                            id: "minecraft:diamond"
                        }
                        random_bonus: 2
                        type: "item"
                    }]
                    tasks: []
                    title: "Quest"
                    x: 0d
                    y: 0d
                }]
            }"#,
        )
        .unwrap();
        let reward_table = "{ id: \"TABLE\" rewards: [] }";
        fs::write(quests.join("reward_tables/example.snbt"), reward_table).unwrap();

        let (book, _) = EditableBook::load(&root).unwrap();
        let output = root.with_file_name(format!("ftbgui-preserve-export-{unique}"));
        book.save(&output).unwrap();
        let chapter = fs::read_to_string(output.join("quests/chapters/main.snbt")).unwrap();
        assert!(chapter.contains("minecraft:custom_name"));
        assert!(chapter.contains("random_bonus: 2"));
        assert_eq!(
            fs::read_to_string(output.join("quests/reward_tables/example.snbt")).unwrap(),
            reward_table
        );

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn imports_bundled_atm_examples_when_present() {
        for path in ["ftbquestsATM9", "ftbquestsATM10"] {
            let path = std::path::Path::new(path);
            if !path.exists() {
                continue;
            }
            let (book, _) = EditableBook::load(path).unwrap();
            assert!(!book.chapters.is_empty());
            assert!(
                book.chapters
                    .iter()
                    .any(|chapter| !chapter.quests.is_empty())
            );
        }
    }

    #[test]
    fn imports_and_resolves_bundled_kubejs_languages_when_present() {
        let path = std::path::Path::new("ftbquestsATM9");
        if !path.exists() {
            return;
        }
        let (book, report) = EditableBook::load(path).unwrap();
        assert!(report.imported_languages >= 10);
        assert_eq!(
            book.translations.resolve("{atm9.quest.SG.sword}"),
            "Sword Blueprint"
        );
        assert_eq!(
            book.translations
                .translation("es_es", "atm9.quest.SG.sword"),
            Some("Plano de Espada")
        );
        assert!(
            book.translations
                .resolve("{atm9.quest.SG.desc.sword}")
                .contains('\n')
        );
    }

    #[test]
    fn creates_uuid_translation_and_writes_minified_json() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output = std::env::temp_dir().join(format!("ftbgui-i18n-{unique}"));
        let mut ids = IdGenerator::new();
        let mut book = EditableBook::blank(&mut ids);
        book.translations.add_locale("ru-RU");
        let key = book.translations.localize(&mut book.chapters[0].title);
        assert!(uuid::Uuid::parse_str(key.trim_start_matches("ftbgui.")).is_ok());
        assert_eq!(
            book.translations.resolve(&book.chapters[0].title),
            "Первая глава"
        );

        book.save(&output).unwrap();
        let json = fs::read_to_string(output.join("kubejs/lang/ru_ru.json")).unwrap();
        assert!(!json.contains('\n'));
        let values = serde_json::from_str::<BTreeMap<String, String>>(&json).unwrap();
        assert_eq!(values.get(&key).map(String::as_str), Some("Первая глава"));

        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn roundtrips_bundled_large_example_without_dropping_entries() {
        let source = std::path::Path::new("ftbquestsATM10");
        if !source.exists() {
            return;
        }
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output = std::env::temp_dir().join(format!("ftbgui-atm10-roundtrip-{unique}"));
        let (book, source_report) = EditableBook::load(source).unwrap();
        let source_tasks = book
            .chapters
            .iter()
            .flat_map(|chapter| &chapter.quests)
            .map(|quest| quest.tasks.len())
            .sum::<usize>();
        let source_rewards = book
            .chapters
            .iter()
            .flat_map(|chapter| &chapter.quests)
            .map(|quest| quest.rewards.len())
            .sum::<usize>();
        let source_tables = book.preserved_files.len();

        book.save(&output).unwrap();
        let (roundtrip, roundtrip_report) = EditableBook::load(&output).unwrap();
        assert_eq!(
            roundtrip
                .chapters
                .iter()
                .flat_map(|chapter| &chapter.quests)
                .map(|quest| quest.tasks.len())
                .sum::<usize>(),
            source_tasks
        );
        assert_eq!(
            roundtrip
                .chapters
                .iter()
                .flat_map(|chapter| &chapter.quests)
                .map(|quest| quest.rewards.len())
                .sum::<usize>(),
            source_rewards
        );
        assert_eq!(roundtrip.preserved_files.len(), source_tables);
        assert_eq!(
            roundtrip_report.preserved_tasks,
            source_report.preserved_tasks
        );
        assert_eq!(
            roundtrip_report.preserved_rewards,
            source_report.preserved_rewards
        );

        fs::remove_dir_all(output).unwrap();
    }
}
