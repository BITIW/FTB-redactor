use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::snbt::{self, Value};

#[derive(Debug, Default)]
pub struct QuestBook {
    pub root: PathBuf,
    pub title: String,
    pub format_version: Option<String>,
    pub groups: usize,
    pub chapters: usize,
    pub grouped_chapters: usize,
    pub quests: usize,
    pub dependencies: usize,
    pub unique_dependencies: usize,
    pub cross_chapter_dependencies: usize,
    pub broken_dependencies: usize,
    pub broken_dependency_details: Vec<BrokenDependency>,
    pub quest_links: usize,
    pub tasks: usize,
    pub rewards: usize,
    pub reward_tables: usize,
    pub reward_table_entries: usize,
    pub duplicate_quest_ids: usize,
    pub empty_chapters: usize,
    pub task_types: BTreeMap<String, usize>,
    pub reward_types: BTreeMap<String, usize>,
    pub largest_chapters: Vec<ChapterSummary>,
    pub processing: ProcessingStats,
}

#[derive(Debug, Clone)]
pub struct ChapterSummary {
    pub title: String,
    pub filename: String,
    pub quests: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenDependency {
    pub chapter: String,
    pub quest_id: String,
    pub missing_quest_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct ProcessingStats {
    pub snbt_files: usize,
    pub input_bytes: u64,
    pub load_time: Duration,
}

#[derive(Debug)]
pub enum LoadError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: snbt::ParseError,
    },
    InvalidLayout(PathBuf),
    InvalidRoot(PathBuf),
}

impl fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io { path, source } => {
                write!(
                    formatter,
                    "не удалось прочитать {}: {source}",
                    path.display()
                )
            }
            LoadError::Parse { path, source } => {
                write!(formatter, "ошибка SNBT в {}: {source}", path.display())
            }
            LoadError::InvalidLayout(path) => write!(
                formatter,
                "в {} не найден data.snbt или quests/data.snbt",
                path.display()
            ),
            LoadError::InvalidRoot(path) => {
                write!(
                    formatter,
                    "корень {} не является SNBT-объектом",
                    path.display()
                )
            }
        }
    }
}

impl Error for LoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            LoadError::Io { source, .. } => Some(source),
            LoadError::Parse { source, .. } => Some(source),
            LoadError::InvalidLayout(_) | LoadError::InvalidRoot(_) => None,
        }
    }
}

#[derive(Debug)]
struct Dependency {
    from: String,
    to: String,
}

impl QuestBook {
    pub fn load(path: &Path) -> Result<Self, LoadError> {
        let started_at = Instant::now();
        let mut processing = ProcessingStats::default();
        let root = find_quests_root(path)?;
        let data_path = root.join("data.snbt");
        let data = read_snbt(&data_path, &mut processing)?;
        require_compound(&data, &data_path)?;

        let mut book = QuestBook {
            root: root.clone(),
            title: scalar(&data, "title").unwrap_or("Без названия").to_owned(),
            format_version: scalar(&data, "version").map(str::to_owned),
            processing,
            ..QuestBook::default()
        };

        let groups_path = root.join("chapter_groups.snbt");
        if groups_path.is_file() {
            let groups = read_snbt(&groups_path, &mut book.processing)?;
            require_compound(&groups, &groups_path)?;
            book.groups = list(&groups, "chapter_groups").map_or(0, <[Value]>::len);
        }

        let mut quest_chapters = HashMap::<String, String>::new();
        let mut dependencies = Vec::new();
        for chapter_path in snbt_files(&root.join("chapters"))? {
            let chapter = read_snbt(&chapter_path, &mut book.processing)?;
            require_compound(&chapter, &chapter_path)?;
            book.chapters += 1;
            if scalar(&chapter, "group").is_some_and(|group| !group.is_empty()) {
                book.grouped_chapters += 1;
            }

            let quests = list(&chapter, "quests").unwrap_or_default();
            if quests.is_empty() {
                book.empty_chapters += 1;
            }
            book.quests += quests.len();
            book.quest_links += list(&chapter, "quest_links").map_or(0, <[Value]>::len);

            let filename = scalar(&chapter, "filename")
                .map(str::to_owned)
                .or_else(|| {
                    chapter_path
                        .file_stem()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "?".to_owned());
            let title = scalar(&chapter, "title").unwrap_or(&filename).to_owned();
            book.largest_chapters.push(ChapterSummary {
                title,
                filename: filename.clone(),
                quests: quests.len(),
            });

            for quest in quests {
                let Some(quest_id) = scalar(quest, "id") else {
                    continue;
                };
                if quest_chapters
                    .insert(quest_id.to_owned(), filename.clone())
                    .is_some()
                {
                    book.duplicate_quest_ids += 1;
                }

                if let Some(items) = list(quest, "tasks") {
                    book.tasks += items.len();
                    count_types(items, &mut book.task_types);
                }
                if let Some(items) = list(quest, "rewards") {
                    book.rewards += items.len();
                    count_types(items, &mut book.reward_types);
                }
                if let Some(items) = list(quest, "dependencies") {
                    for dependency in items {
                        if let Some(dependency_id) = dependency.as_str() {
                            dependencies.push(Dependency {
                                from: quest_id.to_owned(),
                                to: dependency_id.to_owned(),
                            });
                        }
                    }
                }
            }
        }

        book.dependencies = dependencies.len();
        book.unique_dependencies = dependencies
            .iter()
            .map(|dependency| (&dependency.from, &dependency.to))
            .collect::<HashSet<_>>()
            .len();
        for dependency in &dependencies {
            match (
                quest_chapters.get(&dependency.from),
                quest_chapters.get(&dependency.to),
            ) {
                (from, None) => {
                    book.broken_dependency_details.push(BrokenDependency {
                        chapter: from.cloned().unwrap_or_else(|| "?".to_owned()),
                        quest_id: dependency.from.clone(),
                        missing_quest_id: dependency.to.clone(),
                    });
                }
                (Some(from), Some(to)) if from != to => {
                    book.cross_chapter_dependencies += 1;
                }
                _ => {}
            }
        }
        book.broken_dependency_details.sort_by(|left, right| {
            left.chapter
                .cmp(&right.chapter)
                .then_with(|| left.quest_id.cmp(&right.quest_id))
                .then_with(|| left.missing_quest_id.cmp(&right.missing_quest_id))
        });
        book.broken_dependencies = book.broken_dependency_details.len();

        for table_path in snbt_files(&root.join("reward_tables"))? {
            let table = read_snbt(&table_path, &mut book.processing)?;
            require_compound(&table, &table_path)?;
            book.reward_tables += 1;
            book.reward_table_entries += list(&table, "rewards").map_or(0, <[Value]>::len);
        }

        book.largest_chapters
            .sort_by_key(|chapter| Reverse(chapter.quests));
        book.largest_chapters.truncate(5);
        book.processing.load_time = started_at.elapsed();
        Ok(book)
    }

    pub fn report(&self) -> String {
        let mut output = String::new();
        output.push_str(&format!("FTB Quests: {}\n", self.title));
        output.push_str(&format!("Путь: {}\n", self.root.display()));
        if let Some(version) = &self.format_version {
            output.push_str(&format!("Версия формата: {version}\n"));
        }
        output.push('\n');
        output.push_str(&format!("Групп глав: {}\n", self.groups));
        output.push_str(&format!(
            "Глав: {} (в группах: {}, без группы: {}, пустых: {})\n",
            self.chapters,
            self.grouped_chapters,
            self.chapters.saturating_sub(self.grouped_chapters),
            self.empty_chapters
        ));
        output.push_str(&format!("Квестов: {}\n", self.quests));
        output.push_str(&format!(
            "Связей-зависимостей: {} (уникальных: {}, между главами: {}, битых: {})\n",
            self.dependencies,
            self.unique_dependencies,
            self.cross_chapter_dependencies,
            self.broken_dependencies
        ));
        output.push_str(&format!(
            "Визуальных ссылок на квесты: {}\n",
            self.quest_links
        ));
        output.push_str(&format!("Задач: {}\n", self.tasks));
        output.push_str(&format!("Наград: {}\n", self.rewards));
        output.push_str(&format!(
            "Таблиц наград: {} (вариантов наград: {})\n",
            self.reward_tables, self.reward_table_entries
        ));
        output.push_str(&format!(
            "Повторяющихся ID квестов: {}\n",
            self.duplicate_quest_ids
        ));

        if !self.broken_dependency_details.is_empty() {
            output.push_str("\nБитые зависимости:\n");
            for dependency in &self.broken_dependency_details {
                output.push_str(&format!(
                    "  [{}] {} -> {} (квест не найден)\n",
                    dependency.chapter, dependency.quest_id, dependency.missing_quest_id
                ));
            }
        }

        append_types(&mut output, "Типы задач", &self.task_types);
        append_types(&mut output, "Типы наград", &self.reward_types);

        if !self.largest_chapters.is_empty() {
            output.push_str("\nКрупнейшие главы:\n");
            for chapter in &self.largest_chapters {
                output.push_str(&format!(
                    "  {:>4}  {} [{}]\n",
                    chapter.quests, chapter.title, chapter.filename
                ));
            }
        }
        output
    }

    pub fn technical_report(&self, total_time: Duration) -> String {
        let mebibytes = self.processing.input_bytes as f64 / (1024.0 * 1024.0);
        let seconds = self.processing.load_time.as_secs_f64();
        let throughput = if seconds > 0.0 {
            mebibytes / seconds
        } else {
            0.0
        };
        let build_profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };

        format!(
            "\nТехническая статистика:\n\
             \x20 SNBT-файлов обработано: {}\n\
             \x20 Объём входных данных: {}\n\
             \x20 Загрузка и анализ: {}\n\
             \x20 Полное время выполнения: {}\n\
             \x20 Скорость обработки: {throughput:.2} MiB/s\n\
             \x20 Программа: ftbgui v{} ({build_profile})\n\
             \x20 Платформа: {}-{}\n",
            self.processing.snbt_files,
            format_bytes(self.processing.input_bytes),
            format_duration(self.processing.load_time),
            format_duration(total_time),
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    }
}

fn find_quests_root(path: &Path) -> Result<PathBuf, LoadError> {
    if path.join("data.snbt").is_file() {
        Ok(path.to_owned())
    } else if path.join("quests").join("data.snbt").is_file() {
        Ok(path.join("quests"))
    } else {
        Err(LoadError::InvalidLayout(path.to_owned()))
    }
}

fn read_snbt(path: &Path, processing: &mut ProcessingStats) -> Result<Value, LoadError> {
    let source = fs::read_to_string(path).map_err(|source| LoadError::Io {
        path: path.to_owned(),
        source,
    })?;
    let value = snbt::parse(&source).map_err(|source| LoadError::Parse {
        path: path.to_owned(),
        source,
    })?;
    processing.snbt_files += 1;
    processing.input_bytes += source.len() as u64;
    Ok(value)
}

fn require_compound<'a>(
    value: &'a Value,
    path: &Path,
) -> Result<&'a BTreeMap<String, Value>, LoadError> {
    value
        .as_compound()
        .ok_or_else(|| LoadError::InvalidRoot(path.to_owned()))
}

fn snbt_files(directory: &Path) -> Result<Vec<PathBuf>, LoadError> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(directory).map_err(|source| LoadError::Io {
        path: directory.to_owned(),
        source,
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| LoadError::Io {
            path: directory.to_owned(),
            source,
        })?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "snbt")
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn scalar<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn list<'a>(value: &'a Value, key: &str) -> Option<&'a [Value]> {
    value.get(key)?.as_list()
}

fn count_types(items: &[Value], counts: &mut BTreeMap<String, usize>) {
    for item in items {
        let item_type = scalar(item, "type").unwrap_or("(не указан)");
        *counts.entry(item_type.to_owned()).or_default() += 1;
    }
}

fn append_types(output: &mut String, title: &str, types: &BTreeMap<String, usize>) {
    if types.is_empty() {
        return;
    }
    let mut types = types.iter().collect::<Vec<_>>();
    types.sort_by(|(left_name, left_count), (right_name, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_name.cmp(right_name))
    });
    output.push_str(&format!("\n{title}:\n"));
    for (name, count) in types {
        output.push_str(&format!("  {name}: {count}\n"));
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;

    if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() >= 1 {
        format!("{:.3} s", duration.as_secs_f64())
    } else if duration.as_millis() >= 1 {
        format!("{:.3} ms", duration.as_secs_f64() * 1_000.0)
    } else {
        format!("{:.3} us", duration.as_secs_f64() * 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::QuestBook;

    #[test]
    fn loads_book_and_counts_dependencies() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ftbgui-test-{unique}"));
        fs::create_dir_all(root.join("chapters")).unwrap();
        fs::create_dir_all(root.join("reward_tables")).unwrap();
        fs::write(root.join("data.snbt"), r#"{ title: "Test", version: 13 }"#).unwrap();
        fs::write(
            root.join("chapter_groups.snbt"),
            r#"{ chapter_groups: [{ id: "G" }] }"#,
        )
        .unwrap();
        fs::write(
            root.join("chapters").join("one.snbt"),
            r#"{
                filename: "one"
                group: "G"
                quests: [
                    { id: "A", tasks: [{ type: "item" }], rewards: [{ type: "xp" }] }
                    { id: "B", dependencies: ["A" "MISSING"] }
                ]
            }"#,
        )
        .unwrap();
        fs::write(
            root.join("reward_tables").join("loot.snbt"),
            r#"{ rewards: [{ item: "minecraft:stone" }] }"#,
        )
        .unwrap();

        let book = QuestBook::load(&root).unwrap();
        assert_eq!(book.groups, 1);
        assert_eq!(book.chapters, 1);
        assert_eq!(book.quests, 2);
        assert_eq!(book.dependencies, 2);
        assert_eq!(book.broken_dependencies, 1);
        assert_eq!(book.broken_dependency_details.len(), 1);
        assert_eq!(book.broken_dependency_details[0].chapter, "one");
        assert_eq!(book.broken_dependency_details[0].quest_id, "B");
        assert_eq!(
            book.broken_dependency_details[0].missing_quest_id,
            "MISSING"
        );
        assert!(
            book.report()
                .contains("[one] B -> MISSING (квест не найден)")
        );
        assert_eq!(book.tasks, 1);
        assert_eq!(book.rewards, 1);
        assert_eq!(book.reward_table_entries, 1);
        assert_eq!(book.processing.snbt_files, 4);
        assert!(book.processing.input_bytes > 0);
        assert!(
            book.technical_report(book.processing.load_time)
                .contains("SNBT-файлов обработано: 4")
        );

        fs::remove_dir_all(root).unwrap();
    }
}
