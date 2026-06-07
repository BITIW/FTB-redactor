use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    Item,
    Entity,
}

#[derive(Debug, Clone)]
pub struct ResourceEntry {
    pub id: String,
    pub name: String,
    pub kind: ResourceKind,
    pub source: String,
    pub icon_png: Option<Arc<[u8]>>,
}

#[derive(Debug, Default)]
pub struct ResourceIndex {
    pub entries: Vec<ResourceEntry>,
    pub scanned_sources: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

impl ResourceIndex {
    pub fn scan_project(project: &Path) -> Self {
        let mut index = Self::default();
        index.add_builtin_entries();

        let project = project_root(project);
        let minecraft_jar = minecraft_jar_path(project);
        if minecraft_jar.is_file() {
            index.scan_jar(&minecraft_jar);
        }
        let mods = project.join("mods");
        let Ok(children) = fs::read_dir(&mods) else {
            index.finish();
            return index;
        };
        for child in children.flatten() {
            let path = child.path();
            if path.is_dir() && path.join("assets").is_dir() {
                index.scan_directory(&path);
            } else if path.extension().is_some_and(|extension| extension == "jar") {
                index.scan_jar(&path);
            }
        }
        index.finish();
        index
    }

    pub fn import_minecraft_jar(project: &Path, source: &Path) -> Result<PathBuf, String> {
        let file = fs::File::open(source).map_err(|error| error.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
        archive
            .by_name("assets/minecraft/lang/en_us.json")
            .map_err(|_| {
                "Это не клиентский Minecraft JAR: отсутствует assets/minecraft/lang/en_us.json."
                    .to_owned()
            })?;
        let destination = minecraft_jar_path(project);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        if source.canonicalize().ok() == destination.canonicalize().ok() && destination.is_file() {
            return Ok(destination);
        }
        fs::copy(source, &destination).map_err(|error| error.to_string())?;
        Ok(destination)
    }

    fn scan_directory(&mut self, root: &Path) {
        let assets = root.join("assets");
        let Ok(namespaces) = fs::read_dir(&assets) else {
            return;
        };
        self.scanned_sources.push(root.to_path_buf());
        let mut all_translations = BTreeMap::new();
        let mut models = BTreeMap::new();
        let mut textures = BTreeMap::new();
        for namespace in namespaces.flatten() {
            let namespace_path = namespace.path();
            if !namespace_path.is_dir() {
                continue;
            }
            let namespace_name = namespace.file_name().to_string_lossy().into_owned();
            let translations = read_directory_translations(&namespace_path);
            all_translations.insert(namespace_name.clone(), translations);
            collect_directory_models(&namespace_name, &namespace_path.join("models"), &mut models);
            collect_directory_textures(
                &namespace_name,
                &namespace_path.join("textures"),
                &mut textures,
            );
        }
        let source = root.display().to_string();
        for (namespace, translations) in &all_translations {
            self.add_translation_entries(namespace, translations, &source);
        }
        let item_models = models
            .keys()
            .filter_map(|model| {
                let (namespace, path) = model.split_once(':')?;
                path.strip_prefix("item/")
                    .map(|item| (namespace.to_owned(), item.to_owned(), model.clone()))
            })
            .collect::<Vec<_>>();
        for (namespace, item, model) in item_models {
            let icon = resolve_model_icon(&model, &models, &textures, &mut BTreeSet::new());
            self.add_item(
                &namespace,
                &item,
                all_translations.get(&namespace).unwrap_or(&BTreeMap::new()),
                &source,
                icon,
            );
        }
    }

    fn scan_jar(&mut self, path: &Path) {
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let file = fs::File::open(path)?;
            let mut archive = zip::ZipArchive::new(file)?;
            let mut translations = BTreeMap::<String, BTreeMap<String, String>>::new();
            let mut models = BTreeMap::new();
            let mut textures = BTreeMap::new();

            for index in 0..archive.len() {
                let name = archive.by_index(index)?.name().to_owned();
                if let Some(namespace) = lang_namespace(&name) {
                    let mut contents = String::new();
                    archive.by_index(index)?.read_to_string(&mut contents)?;
                    if let Ok(values) = serde_json::from_str::<BTreeMap<String, String>>(&contents)
                    {
                        translations.entry(namespace).or_default().extend(values);
                    }
                } else if let Some(model_id) = model_id(&name) {
                    let mut contents = String::new();
                    archive.by_index(index)?.read_to_string(&mut contents)?;
                    if let Ok(model) = serde_json::from_str(&contents) {
                        models.insert(model_id, model);
                    }
                } else if let Some(texture_id) = texture_id_from_path(&name) {
                    let mut contents = Vec::new();
                    archive.by_index(index)?.read_to_end(&mut contents)?;
                    textures.insert(texture_id, Arc::<[u8]>::from(contents));
                }
            }

            let source = path.display().to_string();
            for (namespace, values) in &translations {
                self.add_translation_entries(namespace, values, &source);
            }
            let item_models = models
                .keys()
                .filter_map(|model| {
                    let (namespace, path) = model.split_once(':')?;
                    path.strip_prefix("item/")
                        .map(|item| (namespace.to_owned(), item.to_owned(), model.clone()))
                })
                .collect::<Vec<_>>();
            for (namespace, item, model) in item_models {
                let icon = resolve_model_icon(&model, &models, &textures, &mut BTreeSet::new());
                self.add_item(
                    &namespace,
                    &item,
                    translations.get(&namespace).unwrap_or(&BTreeMap::new()),
                    &source,
                    icon,
                );
            }
            Ok(())
        })();

        match result {
            Ok(()) => self.scanned_sources.push(path.to_path_buf()),
            Err(error) => self.warnings.push(format!("{}: {error}", path.display())),
        }
    }

    fn add_translation_entries(
        &mut self,
        namespace: &str,
        translations: &BTreeMap<String, String>,
        source: &str,
    ) {
        for (key, name) in translations {
            if let Some(path) = key.strip_prefix(&format!("item.{namespace}.")) {
                if is_auxiliary_translation(path) {
                    continue;
                }
                let path = registry_path(namespace, path);
                self.entries.push(ResourceEntry {
                    id: format!("{namespace}:{path}"),
                    name: name.clone(),
                    kind: ResourceKind::Item,
                    source: source.to_owned(),
                    icon_png: None,
                });
            } else if let Some(path) = key.strip_prefix(&format!("block.{namespace}.")) {
                if is_auxiliary_translation(path) {
                    continue;
                }
                let path = registry_path(namespace, path);
                self.entries.push(ResourceEntry {
                    id: format!("{namespace}:{path}"),
                    name: name.clone(),
                    kind: ResourceKind::Item,
                    source: source.to_owned(),
                    icon_png: None,
                });
            } else if let Some(path) = key.strip_prefix(&format!("entity.{namespace}.")) {
                if is_auxiliary_translation(path) {
                    continue;
                }
                let path = registry_path(namespace, path);
                self.entries.push(ResourceEntry {
                    id: format!("{namespace}:{path}"),
                    name: name.clone(),
                    kind: ResourceKind::Entity,
                    source: source.to_owned(),
                    icon_png: None,
                });
            }
        }
    }

    fn add_item(
        &mut self,
        namespace: &str,
        path: &str,
        translations: &BTreeMap<String, String>,
        source: &str,
        icon_png: Option<Arc<[u8]>>,
    ) {
        let dotted = path.replace('/', ".");
        let translated_name = translations
            .get(&format!("item.{namespace}.{dotted}"))
            .or_else(|| translations.get(&format!("block.{namespace}.{dotted}")))
            .cloned();
        if namespace == "minecraft" && translated_name.is_none() {
            return;
        }
        let name = translated_name.unwrap_or_else(|| humanize(path));
        self.entries.push(ResourceEntry {
            id: format!("{namespace}:{path}"),
            name,
            kind: ResourceKind::Item,
            source: source.to_owned(),
            icon_png,
        });
    }

    fn add_builtin_entries(&mut self) {
        for (id, name, kind) in [
            ("minecraft:stone", "Stone", ResourceKind::Item),
            ("minecraft:diamond", "Diamond", ResourceKind::Item),
            ("minecraft:carpet", "Carpet", ResourceKind::Item),
            ("minecraft:oak_log", "Oak Log", ResourceKind::Item),
            ("minecraft:iron_ingot", "Iron Ingot", ResourceKind::Item),
            ("minecraft:gold_ingot", "Gold Ingot", ResourceKind::Item),
            ("minecraft:emerald", "Emerald", ResourceKind::Item),
            ("minecraft:zombie", "Zombie", ResourceKind::Entity),
            ("minecraft:skeleton", "Skeleton", ResourceKind::Entity),
            ("minecraft:creeper", "Creeper", ResourceKind::Entity),
            ("minecraft:spider", "Spider", ResourceKind::Entity),
            (
                "minecraft:ender_dragon",
                "Ender Dragon",
                ResourceKind::Entity,
            ),
        ] {
            self.entries.push(ResourceEntry {
                id: id.to_owned(),
                name: name.to_owned(),
                kind,
                source: "Встроенный минимум".to_owned(),
                icon_png: None,
            });
        }
    }

    fn finish(&mut self) {
        let mut merged = BTreeMap::<(ResourceKind, String), ResourceEntry>::new();
        for entry in self.entries.drain(..) {
            let key = (entry.kind, entry.id.clone());
            match merged.entry(key) {
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(entry);
                }
                std::collections::btree_map::Entry::Occupied(mut slot) => {
                    if slot.get().icon_png.is_none() && entry.icon_png.is_some() {
                        slot.get_mut().icon_png = entry.icon_png;
                    }
                }
            }
        }
        self.entries = merged.into_values().collect();
    }
}

pub fn minecraft_jar_path(project: &Path) -> PathBuf {
    project_root(project)
        .join(".ftbgui")
        .join("minecraft-client.jar")
}

fn project_root(project: &Path) -> &Path {
    if project.file_name().is_some_and(|name| name == "quests") {
        project.parent().unwrap_or(project)
    } else {
        project
    }
}

fn registry_path<'a>(namespace: &str, translation_path: &'a str) -> &'a str {
    if namespace == "minecraft" {
        translation_path
            .split_once('.')
            .map_or(translation_path, |(path, _)| path)
    } else {
        translation_path
    }
}

fn is_auxiliary_translation(path: &str) -> bool {
    let markers = [
        ".tooltip",
        ".description",
        ".desc",
        ".summary",
        ".condition",
        ".behaviour",
        ".behavior",
    ];
    markers
        .iter()
        .any(|marker| path.ends_with(marker) || path.contains(&format!("{marker}.")))
}

fn read_directory_translations(namespace: &Path) -> BTreeMap<String, String> {
    let lang = namespace.join("lang");
    for name in ["en_us.json", "en_gb.json"] {
        if let Ok(contents) = fs::read_to_string(lang.join(name))
            && let Ok(values) = serde_json::from_str(&contents)
        {
            return values;
        }
    }
    BTreeMap::new()
}

fn collect_directory_models(
    namespace: &str,
    root: &Path,
    output: &mut BTreeMap<String, serde_json::Value>,
) {
    collect_directory_models_inner(namespace, root, root, output);
}

fn collect_directory_models_inner(
    namespace: &str,
    root: &Path,
    base: &Path,
    output: &mut BTreeMap<String, serde_json::Value>,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_directory_models_inner(namespace, &path, base, output);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
            && let Ok(relative) = path.strip_prefix(base)
            && let Ok(contents) = fs::read_to_string(&path)
            && let Ok(model) = serde_json::from_str(&contents)
        {
            let mut model_path = relative.to_string_lossy().replace('\\', "/");
            model_path.truncate(model_path.len().saturating_sub(".json".len()));
            output.insert(format!("{namespace}:{model_path}"), model);
        }
    }
}

fn collect_directory_textures(
    namespace: &str,
    root: &Path,
    output: &mut BTreeMap<String, Arc<[u8]>>,
) {
    collect_directory_textures_inner(namespace, root, root, output);
}

fn collect_directory_textures_inner(
    namespace: &str,
    root: &Path,
    base: &Path,
    output: &mut BTreeMap<String, Arc<[u8]>>,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_directory_textures_inner(namespace, &path, base, output);
        } else if path.extension().is_some_and(|extension| extension == "png")
            && let Ok(relative) = path.strip_prefix(base)
            && let Ok(contents) = fs::read(&path)
        {
            let mut texture_path = relative.to_string_lossy().replace('\\', "/");
            texture_path.truncate(texture_path.len().saturating_sub(".png".len()));
            output.insert(
                format!("{namespace}:{texture_path}"),
                Arc::<[u8]>::from(contents),
            );
        }
    }
}

fn lang_namespace(name: &str) -> Option<String> {
    let parts = name.split('/').collect::<Vec<_>>();
    (parts.len() == 4
        && parts[0] == "assets"
        && parts[2] == "lang"
        && matches!(parts[3], "en_us.json" | "en_gb.json"))
    .then(|| parts[1].to_owned())
}

fn model_id(name: &str) -> Option<String> {
    let rest = name.strip_prefix("assets/")?;
    let (namespace, rest) = rest.split_once("/models/")?;
    let model = rest.strip_suffix(".json")?;
    Some(format!("{namespace}:{model}"))
}

fn texture_id_from_path(name: &str) -> Option<String> {
    let rest = name.strip_prefix("assets/")?;
    let (namespace, rest) = rest.split_once("/textures/")?;
    let texture = rest.strip_suffix(".png")?;
    Some(format!("{namespace}:{texture}"))
}

fn resolve_model_icon(
    model_id: &str,
    models: &BTreeMap<String, serde_json::Value>,
    textures: &BTreeMap<String, Arc<[u8]>>,
    visited: &mut BTreeSet<String>,
) -> Option<Arc<[u8]>> {
    if !visited.insert(model_id.to_owned()) {
        return None;
    }
    let model = models.get(model_id)?;
    let namespace = model_id.split_once(':')?.0;
    if let Some(model_textures) = model.get("textures").and_then(serde_json::Value::as_object) {
        for key in ["layer0", "layer1", "all", "front", "side", "0", "particle"] {
            if let Some(texture) = model_textures
                .get(key)
                .and_then(serde_json::Value::as_str)
                .and_then(|value| resolve_texture_reference(value, model_textures, namespace))
                .and_then(|texture| textures.get(&texture))
            {
                return Some(Arc::clone(texture));
            }
        }
        for value in model_textures
            .values()
            .filter_map(serde_json::Value::as_str)
        {
            if let Some(texture) = resolve_texture_reference(value, model_textures, namespace)
                .and_then(|texture| textures.get(&texture))
            {
                return Some(Arc::clone(texture));
            }
        }
    }
    let parent = model.get("parent")?.as_str()?;
    if matches!(
        parent,
        "item/generated" | "minecraft:item/generated" | "item/handheld" | "minecraft:item/handheld"
    ) {
        return None;
    }
    let parent = normalize_model_reference(parent, namespace);
    resolve_model_icon(&parent, models, textures, visited)
}

fn resolve_texture_reference(
    value: &str,
    model_textures: &serde_json::Map<String, serde_json::Value>,
    namespace: &str,
) -> Option<String> {
    let value = if let Some(alias) = value.strip_prefix('#') {
        model_textures.get(alias)?.as_str()?
    } else {
        value
    };
    if value.starts_with('#') {
        return None;
    }
    Some(if value.contains(':') {
        value.to_owned()
    } else if value.starts_with("item/") || value.starts_with("block/") {
        format!("minecraft:{value}")
    } else {
        format!("{namespace}:{value}")
    })
}

fn normalize_model_reference(value: &str, namespace: &str) -> String {
    if value.contains(':') {
        value.to_owned()
    } else if value.starts_with("item/") || value.starts_with("block/") {
        format!("minecraft:{value}")
    } else {
        format!("{namespace}:{value}")
    }
}

fn humanize(value: &str) -> String {
    value
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl Ord for ResourceKind {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (*self as u8).cmp(&(*other as u8))
    }
}

impl PartialOrd for ResourceKind {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::io::Write;
    use std::sync::Arc;

    use super::{
        ResourceIndex, ResourceKind, humanize, is_auxiliary_translation, minecraft_jar_path,
        registry_path, resolve_model_icon,
    };

    #[test]
    fn humanizes_resource_paths() {
        assert_eq!(humanize("materials/brass_ingot"), "Brass Ingot");
    }

    #[test]
    fn strips_vanilla_translation_variants_from_registry_ids() {
        assert_eq!(registry_path("minecraft", "villager.armorer"), "villager");
        assert_eq!(registry_path("example", "machine.active"), "machine.active");
    }

    #[test]
    fn rejects_tooltip_and_description_translation_keys() {
        assert!(is_auxiliary_translation("haunted_bell.tooltip.summary"));
        assert!(is_auxiliary_translation("machine.description"));
        assert!(!is_auxiliary_translation("haunted_bell"));
    }

    #[test]
    fn resolves_item_icon_through_a_block_model_parent() {
        let models = BTreeMap::from([
            (
                "example:item/cogwheel".to_owned(),
                serde_json::json!({"parent": "example:block/cogwheel"}),
            ),
            (
                "example:block/cogwheel".to_owned(),
                serde_json::json!({"textures": {"0": "example:block/cogwheel"}}),
            ),
        ]);
        let expected = Arc::<[u8]>::from(vec![1, 2, 3]);
        let textures =
            BTreeMap::from([("example:block/cogwheel".to_owned(), Arc::clone(&expected))]);

        let actual = resolve_model_icon(
            "example:item/cogwheel",
            &models,
            &textures,
            &mut BTreeSet::new(),
        )
        .unwrap();

        assert_eq!(&*actual, &*expected);
    }

    #[test]
    fn contains_builtin_item_and_entity_entries() {
        let index = ResourceIndex::scan_project(std::path::Path::new("/missing"));
        assert!(
            index.entries.iter().any(|entry| {
                entry.id == "minecraft:diamond" && entry.kind == ResourceKind::Item
            })
        );
        assert!(
            index.entries.iter().any(|entry| {
                entry.id == "minecraft:zombie" && entry.kind == ResourceKind::Entity
            })
        );
    }

    #[test]
    fn scans_only_the_current_projects_mods_directory() {
        let root = std::env::temp_dir().join(format!("ftbgui-resources-{}", uuid::Uuid::new_v4()));
        let project_a = root.join("project_a");
        let project_b = root.join("project_b");
        let lang = project_a.join("mods/example/assets/example/lang");
        fs::create_dir_all(&lang).unwrap();
        fs::write(
            lang.join("en_us.json"),
            r#"{"item.example.private_item":"Private Item","item.example.private_item.tooltip.summary":"Internal tooltip"}"#,
        )
        .unwrap();
        fs::create_dir_all(project_b.join("mods")).unwrap();

        let index_a = ResourceIndex::scan_project(&project_a);
        let index_b = ResourceIndex::scan_project(&project_b);

        assert!(
            index_a
                .entries
                .iter()
                .any(|entry| entry.id == "example:private_item")
        );
        assert!(
            !index_a
                .entries
                .iter()
                .any(|entry| entry.id.contains("tooltip"))
        );
        assert!(
            !index_b
                .entries
                .iter()
                .any(|entry| entry.id == "example:private_item")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn imports_and_scans_a_minecraft_client_jar() {
        let root = std::env::temp_dir().join(format!("ftbgui-vanilla-{}", uuid::Uuid::new_v4()));
        let source = root.join("client.jar");
        fs::create_dir_all(&root).unwrap();
        let file = fs::File::create(&source).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file(
                "assets/minecraft/lang/en_us.json",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        archive
            .write_all(
                br#"{"item.minecraft.diamond":"Diamond","block.minecraft.stone":"Stone","entity.minecraft.villager.armorer":"Armorer"}"#,
            )
            .unwrap();
        archive.finish().unwrap();

        let project = root.join("project");
        ResourceIndex::import_minecraft_jar(&project, &source).unwrap();
        let index = ResourceIndex::scan_project(&project);

        assert!(minecraft_jar_path(&project).is_file());
        assert!(
            index
                .entries
                .iter()
                .any(|entry| entry.id == "minecraft:diamond")
        );
        assert!(
            index
                .entries
                .iter()
                .any(|entry| entry.id == "minecraft:villager")
        );
        assert!(
            !index
                .entries
                .iter()
                .any(|entry| entry.id == "minecraft:villager.armorer")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn extracts_icons_from_bundled_create_when_present() {
        let project = std::path::Path::new("ftbquestsATM9");
        if !project.join("mods").is_dir() {
            return;
        }
        let index = ResourceIndex::scan_project(project);
        let brass = index
            .entries
            .iter()
            .find(|entry| entry.id == "create:brass_ingot")
            .expect("Create brass ingot should be indexed");
        assert!(brass.icon_png.is_some());
    }
}
