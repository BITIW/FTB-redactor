use std::collections::{HashMap, HashSet, VecDeque};

use crate::editor::EditableBook;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyCycle {
    pub quest_ids: Vec<String>,
}

impl DependencyCycle {
    pub fn length(&self) -> usize {
        self.quest_ids.len().saturating_sub(1)
    }

    pub fn suggested_edge(&self) -> Option<(&str, &str)> {
        let edge = self.quest_ids.windows(2).last()?;
        Some((&edge[0], &edge[1]))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenDependency {
    pub quest_id: String,
    pub missing_quest_id: String,
}

pub fn find_dependency_cycles(book: &EditableBook) -> Vec<DependencyCycle> {
    let graph = dependency_graph(book);
    let mut state = HashMap::<&str, VisitState>::new();
    let mut stack = Vec::<&str>::new();
    let mut cycles = Vec::new();
    let mut seen = HashSet::new();

    let mut nodes = graph.keys().copied().collect::<Vec<_>>();
    nodes.sort_unstable();
    for node in nodes {
        if !state.contains_key(node) {
            visit(node, &graph, &mut state, &mut stack, &mut cycles, &mut seen);
        }
    }
    cycles.sort_by(|left, right| {
        left.length()
            .cmp(&right.length())
            .then_with(|| left.quest_ids.cmp(&right.quest_ids))
    });
    cycles
}

pub fn find_broken_dependencies(book: &EditableBook) -> Vec<BrokenDependency> {
    let known_ids = book
        .chapters
        .iter()
        .flat_map(|chapter| chapter.quests.iter())
        .map(|quest| quest.id.as_str())
        .collect::<HashSet<_>>();
    let mut broken = book
        .chapters
        .iter()
        .flat_map(|chapter| chapter.quests.iter())
        .flat_map(|quest| {
            quest
                .dependencies
                .iter()
                .filter(|dependency| !known_ids.contains(dependency.as_str()))
                .map(|dependency| BrokenDependency {
                    quest_id: quest.id.clone(),
                    missing_quest_id: dependency.clone(),
                })
        })
        .collect::<Vec<_>>();
    broken.sort_by(|left, right| {
        left.quest_id
            .cmp(&right.quest_id)
            .then_with(|| left.missing_quest_id.cmp(&right.missing_quest_id))
    });
    broken
}

pub fn cycle_after_adding_dependency(
    book: &EditableBook,
    quest_id: &str,
    dependency_id: &str,
) -> Option<DependencyCycle> {
    if quest_id == dependency_id {
        return Some(DependencyCycle {
            quest_ids: vec![quest_id.to_owned(), quest_id.to_owned()],
        });
    }

    let graph = dependency_graph(book);
    let path = shortest_path(&graph, dependency_id, quest_id)?;
    let mut quest_ids = Vec::with_capacity(path.len() + 1);
    quest_ids.push(quest_id.to_owned());
    quest_ids.extend(path.into_iter().map(str::to_owned));
    Some(DependencyCycle { quest_ids })
}

fn dependency_graph(book: &EditableBook) -> HashMap<&str, Vec<&str>> {
    book.chapters
        .iter()
        .flat_map(|chapter| chapter.quests.iter())
        .map(|quest| {
            (
                quest.id.as_str(),
                quest.dependencies.iter().map(String::as_str).collect(),
            )
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisitState {
    Visiting,
    Done,
}

fn visit<'a>(
    node: &'a str,
    graph: &HashMap<&'a str, Vec<&'a str>>,
    state: &mut HashMap<&'a str, VisitState>,
    stack: &mut Vec<&'a str>,
    cycles: &mut Vec<DependencyCycle>,
    seen: &mut HashSet<String>,
) {
    state.insert(node, VisitState::Visiting);
    stack.push(node);

    if let Some(dependencies) = graph.get(node) {
        for dependency in dependencies {
            if !graph.contains_key(dependency) {
                continue;
            }
            match state.get(dependency) {
                None => visit(dependency, graph, state, stack, cycles, seen),
                Some(VisitState::Visiting) => {
                    if let Some(start) = stack.iter().position(|item| item == dependency) {
                        let mut ids = stack[start..]
                            .iter()
                            .map(|id| (*id).to_owned())
                            .collect::<Vec<_>>();
                        ids.push((*dependency).to_owned());
                        let key = canonical_cycle_key(&ids);
                        if seen.insert(key) {
                            cycles.push(DependencyCycle { quest_ids: ids });
                        }
                    }
                }
                Some(VisitState::Done) => {}
            }
        }
    }

    stack.pop();
    state.insert(node, VisitState::Done);
}

fn canonical_cycle_key(ids: &[String]) -> String {
    let open_cycle = &ids[..ids.len().saturating_sub(1)];
    (0..open_cycle.len())
        .map(|offset| {
            open_cycle
                .iter()
                .cycle()
                .skip(offset)
                .take(open_cycle.len())
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("\0")
        })
        .min()
        .unwrap_or_default()
}

fn shortest_path<'a>(
    graph: &HashMap<&'a str, Vec<&'a str>>,
    start: &'a str,
    target: &str,
) -> Option<Vec<&'a str>> {
    let mut queue = VecDeque::from([start]);
    let mut previous = HashMap::<&str, Option<&str>>::from([(start, None)]);

    while let Some(node) = queue.pop_front() {
        if node == target {
            let mut path = Vec::new();
            let mut current = Some(node);
            while let Some(item) = current {
                path.push(item);
                current = previous[item];
            }
            path.reverse();
            return Some(path);
        }
        for dependency in graph.get(node).into_iter().flatten() {
            if graph.contains_key(dependency) && !previous.contains_key(dependency) {
                previous.insert(*dependency, Some(node));
                queue.push_back(dependency);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::editor::{Chapter, EditableBook, Quest, QuestSettings, TranslationCatalog};

    use super::{cycle_after_adding_dependency, find_broken_dependencies, find_dependency_cycles};

    fn quest(id: &str, dependencies: &[&str]) -> Quest {
        Quest {
            id: id.to_owned(),
            title: id.to_owned(),
            description: String::new(),
            description_raw: None,
            x: 0.0,
            y: 0.0,
            size: 1.0,
            shape: "circle".to_owned(),
            icon: String::new(),
            icon_raw: None,
            dependencies: dependencies.iter().map(|id| (*id).to_owned()).collect(),
            tasks: Vec::new(),
            rewards: Vec::new(),
            settings: QuestSettings::default(),
            extra: BTreeMap::new(),
        }
    }

    fn book(quests: Vec<Quest>) -> EditableBook {
        EditableBook {
            title: "Test".to_owned(),
            grid_scale: 0.5,
            groups: Vec::new(),
            chapters: vec![Chapter {
                id: "CHAPTER".to_owned(),
                filename: "chapter".to_owned(),
                title: "Chapter".to_owned(),
                group: None,
                order_index: 0,
                quests,
                extra: BTreeMap::new(),
            }],
            preserved_files: Vec::new(),
            translations: TranslationCatalog::default(),
        }
    }

    #[test]
    fn finds_long_recursive_cycle() {
        let book = book(vec![
            quest("A", &["B"]),
            quest("B", &["C"]),
            quest("C", &["D"]),
            quest("D", &["A"]),
        ]);
        let cycles = find_dependency_cycles(&book);
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].length(), 4);
        assert_eq!(cycles[0].quest_ids, ["A", "B", "C", "D", "A"]);
    }

    #[test]
    fn checks_new_edge_across_the_whole_path() {
        let book = book(vec![
            quest("A", &[]),
            quest("B", &["C"]),
            quest("C", &["D"]),
            quest("D", &["A"]),
        ]);
        let cycle = cycle_after_adding_dependency(&book, "A", "B").unwrap();
        assert_eq!(cycle.quest_ids, ["A", "B", "C", "D", "A"]);
    }

    #[test]
    fn audits_bundled_large_example_when_present() {
        let path = std::path::Path::new("ftbquestsATM10");
        if !path.exists() {
            return;
        }
        let (book, _) = EditableBook::load(path).unwrap();
        let _cycles = find_dependency_cycles(&book);
        let broken = find_broken_dependencies(&book);
        assert_eq!(broken.len(), 9);
    }
}
