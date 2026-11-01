//! Plugins: everything the app knows about *kinds of files*, kept outside the
//! binary and loaded only when something needs it.
//!
//! A plugin is a folder with a `plugin.json` manifest and its data beside it --
//! tree-sitter queries, an icon name, the extensions it claims. It is not code:
//! nothing here is executed, so a bad plugin can produce a file that opens
//! uncoloured, never a crash in a process that is holding SSH channels and a
//! GPU device. The grammar itself still comes from the binary's own table (see
//! `GrammarSource`), which is what makes the common case cost nothing at all.
//!
//! **Startup reads one small file and nothing else.** The catalogue is a cached
//! index (`.index.json`) listing what each plugin claims; the manifests, the
//! `.scm` queries and the grammar are read the first time a file of that kind
//! is actually opened. A plugin folder with a hundred entries costs the same at
//! launch as an empty one, which is the whole point -- see `Catalog::load`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Index format. Bumped when the cached shape changes so a stale file is
/// rebuilt rather than misread.
const INDEX_VERSION: u32 = 1;
const INDEX_FILE: &str = ".index.json";
const MANIFEST_FILE: &str = "plugin.json";

/// Where a language's parser comes from.
///
/// `Builtin` is the whole of it today: the grammars are crate dependencies
/// compiled into the binary, so a plugin naming one costs no load at all. A
/// manifest may instead name a `library`, which is reserved for grammars
/// shipped as a dynamic library beside the manifest -- the shape is settled
/// here so manifests written now stay valid, but loading one is not wired up
/// and `load` says so plainly rather than failing somewhere later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum GrammarSource {
    /// A grammar the binary already carries, by its registered name.
    Builtin { name: String },
    /// A `tree_sitter_<name>` symbol in a dynamic library beside the manifest.
    Library { path: String, symbol: String },
}

impl Default for GrammarSource {
    fn default() -> Self {
        GrammarSource::Builtin {
            name: "text".to_string(),
        }
    }
}

/// What a plugin says it can do with a kind of file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LanguageSpec {
    /// Extensions claimed, without the dot, lower case.
    pub extensions: Vec<String>,
    /// Whole file names claimed, like `Makefile` or `.bashrc`.
    pub filenames: Vec<String>,
    /// Other names this language answers to in the registry (`rs`, `golang`).
    pub aliases: Vec<String>,
    pub grammar: GrammarSource,
    /// Query files, relative to the plugin folder. Read on first use.
    pub highlights: Option<String>,
    pub injections: Option<String>,
    /// Lucide icon name for the explorer and tab strip.
    pub icon: Option<String>,
}

/// One plugin's manifest.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Manifest {
    /// Stable id, also the registry name for its language.
    pub id: String,
    pub name: String,
    pub version: String,
    /// Absent for a plugin that adds no file support -- a theme, say. Those
    /// are not read at all until something asks for that kind.
    pub language: Option<LanguageSpec>,
}

/// A plugin as the index knows it: enough to decide whether it is wanted,
/// never enough to use it. Using it means reading the folder.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// Folder name under the plugins directory, not an absolute path: the
    /// index stays valid when the whole folder is moved or synced.
    pub folder: String,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    pub icon: Option<String>,
}

/// The cached index: what is on disk, without having read it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Index {
    pub version: u32,
    /// Folder names present when the index was written, sorted. Compared
    /// against a fresh listing to notice an install or a removal without
    /// opening a single manifest.
    pub folders: Vec<String>,
    pub plugins: Vec<Entry>,
}

/// The plugins the app knows about, and where they live.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub dir: PathBuf,
    entries: Vec<Entry>,
    /// extension -> index into `entries`.
    by_extension: HashMap<String, usize>,
    /// exact file name -> index into `entries`.
    by_filename: HashMap<String, usize>,
}

impl Catalog {
    /// Reads the cached index and nothing else. This is the only plugin work
    /// on the startup path, and it is one small file: a folder with a hundred
    /// plugins in it costs what an empty one costs.
    ///
    /// A missing or stale index yields an empty catalogue rather than a scan --
    /// the caller refreshes in the background (`rescan`) and swaps the result
    /// in. Nothing waits for the disk before the first frame.
    pub fn load(dir: PathBuf) -> Self {
        let index: Index = fs::read_to_string(dir.join(INDEX_FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();

        if index.version != INDEX_VERSION {
            return Self {
                dir,
                ..Self::default()
            };
        }
        Self::from_entries(dir, index.plugins)
    }

    pub fn from_entries(dir: PathBuf, entries: Vec<Entry>) -> Self {
        let mut by_extension = HashMap::new();
        let mut by_filename = HashMap::new();
        for (ix, entry) in entries.iter().enumerate() {
            for extension in &entry.extensions {
                by_extension.insert(extension.to_ascii_lowercase(), ix);
            }
            for name in &entry.filenames {
                by_filename.insert(name.to_ascii_lowercase(), ix);
            }
        }
        Self {
            dir,
            entries,
            by_extension,
            by_filename,
        }
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The plugin that claims this file, by exact name first and extension
    /// second -- `Makefile` beats `*.mk`, and a plugin claiming `rs` never
    /// shadows one claiming the whole name.
    pub fn claim(&self, file_name: &str) -> Option<&Entry> {
        let lower = file_name.to_ascii_lowercase();
        if let Some(&ix) = self.by_filename.get(&lower) {
            return self.entries.get(ix);
        }
        let extension = lower.rsplit_once('.').map(|(_, ext)| ext)?;
        self.by_extension.get(extension).and_then(|&ix| self.entries.get(ix))
    }

    /// Walks the folder and reads every manifest. Never call this before the
    /// first frame: it is the slow path, meant for a background task or an
    /// explicit refresh from Settings.
    pub fn rescan(dir: &Path) -> Index {
        let mut folders = Vec::new();
        let mut plugins = Vec::new();

        let listing = match fs::read_dir(dir) {
            Ok(listing) => listing,
            // No folder is not an error: it means no plugins.
            Err(_) => {
                return Index {
                    version: INDEX_VERSION,
                    ..Index::default()
                }
            }
        };

        for item in listing.flatten() {
            if !item.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let folder = item.file_name().to_string_lossy().to_string();
            folders.push(folder.clone());

            let Some(manifest) = read_manifest(&item.path()) else {
                continue;
            };
            let language = manifest.language.unwrap_or_default();
            plugins.push(Entry {
                id: if manifest.id.is_empty() {
                    folder.clone()
                } else {
                    manifest.id
                },
                name: manifest.name,
                folder,
                extensions: language
                    .extensions
                    .iter()
                    .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
                    .collect(),
                filenames: language
                    .filenames
                    .iter()
                    .map(|f| f.to_ascii_lowercase())
                    .collect(),
                icon: language.icon,
            });
        }

        folders.sort();
        Index {
            version: INDEX_VERSION,
            folders,
            plugins,
        }
    }

    /// Whether the folder listing still matches the index. Cheap: it lists
    /// directory names and compares, without opening a manifest.
    pub fn is_stale(dir: &Path, index: &Index) -> bool {
        let mut folders: Vec<String> = match fs::read_dir(dir) {
            Ok(listing) => listing
                .flatten()
                .filter(|item| item.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|item| item.file_name().to_string_lossy().to_string())
                .collect(),
            Err(_) => Vec::new(),
        };
        folders.sort();
        folders != index.folders
    }

    pub fn write_index(dir: &Path, index: &Index) -> std::io::Result<()> {
        fs::create_dir_all(dir)?;
        let text = serde_json::to_string_pretty(index)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(dir.join(INDEX_FILE), text)
    }

    /// Everything needed to register one plugin's language, read now. Called
    /// the first time a file it claims is opened, never before.
    pub fn load_language(&self, entry: &Entry) -> Result<LoadedLanguage, String> {
        let folder = self.dir.join(&entry.folder);
        let manifest = read_manifest(&folder)
            .ok_or_else(|| format!("{}: no readable {MANIFEST_FILE}", entry.folder))?;
        let spec = manifest
            .language
            .ok_or_else(|| format!("{}: manifest declares no language", entry.folder))?;

        let read = |relative: &Option<String>| -> Result<Option<String>, String> {
            let Some(relative) = relative else {
                return Ok(None);
            };
            // Query paths are joined under the plugin's own folder and must
            // stay there: a manifest is data from outside, and `../` in it
            // should not reach the rest of the disk.
            if Path::new(relative)
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(format!("{}: query path escapes the plugin", entry.folder));
            }
            fs::read_to_string(folder.join(relative))
                .map(Some)
                .map_err(|e| format!("{}/{relative}: {e}", entry.folder))
        };

        let grammar = match &spec.grammar {
            GrammarSource::Builtin { name } => name.clone(),
            GrammarSource::Library { .. } => {
                return Err(format!(
                    "{}: grammars from a library are not loadable yet; name a built-in grammar instead",
                    entry.folder
                ))
            }
        };

        Ok(LoadedLanguage {
            id: entry.id.clone(),
            grammar,
            aliases: spec.aliases,
            highlights: read(&spec.highlights)?,
            injections: read(&spec.injections)?,
        })
    }
}

/// One plugin's language, read off the disk and ready to register.
#[derive(Clone, Debug)]
pub struct LoadedLanguage {
    pub id: String,
    /// Name of the built-in grammar to parse with.
    pub grammar: String,
    pub aliases: Vec<String>,
    pub highlights: Option<String>,
    pub injections: Option<String>,
}

fn read_manifest(folder: &Path) -> Option<Manifest> {
    let text = fs::read_to_string(folder.join(MANIFEST_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Where plugins live when Settings names nothing: beside the other stores, so
/// one folder holds everything the app keeps.
pub fn default_dir() -> PathBuf {
    let mut dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push("PulseTerm");
    dir.push("plugins");
    dir
}

/// The folder to use, from the setting. An empty setting means the default --
/// and so does one naming somewhere that does not exist, because a typo in a
/// path should not silently turn every plugin off.
pub fn resolve_dir(configured: &str) -> PathBuf {
    if configured.is_empty() {
        return default_dir();
    }
    let path = PathBuf::from(configured);
    if path.is_dir() {
        path
    } else {
        default_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, manifest: &str) {
        let folder = dir.join(name);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(MANIFEST_FILE), manifest).unwrap();
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pulseterm-plugins-{tag}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_folder_is_no_plugins_not_an_error() {
        let dir = std::env::temp_dir().join("pulseterm-plugins-absent");
        let _ = fs::remove_dir_all(&dir);
        let index = Catalog::rescan(&dir);
        assert_eq!(index.version, INDEX_VERSION);
        assert!(index.plugins.is_empty());
        assert!(Catalog::load(dir).is_empty());
    }

    #[test]
    fn scanning_reads_manifests_and_claims_files() {
        let dir = temp("scan");
        write(
            &dir,
            "nim",
            r#"{"id":"nim","name":"Nim","language":{
                 "extensions":[".nim","nims"],"filenames":["nim.cfg"],
                 "grammar":{"kind":"builtin","name":"rust"},
                 "highlights":"queries/highlights.scm","icon":"FileCode"}}"#,
        );
        let index = Catalog::rescan(&dir);
        assert_eq!(index.plugins.len(), 1);

        let catalog = Catalog::from_entries(dir, index.plugins);
        // The leading dot is stripped, and matching ignores case.
        assert_eq!(catalog.claim("main.nim").unwrap().id, "nim");
        assert_eq!(catalog.claim("MAIN.NIMS").unwrap().id, "nim");
        // A whole-name claim wins, and an unclaimed file claims nothing.
        assert_eq!(catalog.claim("nim.cfg").unwrap().id, "nim");
        assert!(catalog.claim("main.rs").is_none());
    }

    #[test]
    fn the_index_round_trips_and_notices_an_install() {
        let dir = temp("index");
        write(&dir, "one", r#"{"id":"one","name":"One"}"#);
        let index = Catalog::rescan(&dir);
        Catalog::write_index(&dir, &index).unwrap();

        // Reading the cache gives the same catalogue without a scan.
        let catalog = Catalog::load(dir.clone());
        assert_eq!(catalog.entries().len(), 1);
        assert!(!Catalog::is_stale(&dir, &index));

        // A new folder is noticed by listing names alone.
        write(&dir, "two", r#"{"id":"two","name":"Two"}"#);
        assert!(Catalog::is_stale(&dir, &index));
    }

    #[test]
    fn an_index_from_another_version_is_ignored() {
        let dir = temp("version");
        let stale = r#"{"version":9999,"folders":["x"],"plugins":[
            {"id":"x","name":"X","folder":"x","extensions":["x"],"filenames":[],"icon":null}]}"#;
        fs::write(dir.join(INDEX_FILE), stale).unwrap();
        assert!(Catalog::load(dir).is_empty());
    }

    #[test]
    fn a_query_path_cannot_escape_the_plugin() {
        let dir = temp("escape");
        write(
            &dir,
            "bad",
            r#"{"id":"bad","name":"Bad","language":{
                 "extensions":["bad"],"grammar":{"kind":"builtin","name":"json"},
                 "highlights":"../../secrets.scm"}}"#,
        );
        let index = Catalog::rescan(&dir);
        let catalog = Catalog::from_entries(dir, index.plugins);
        let entry = catalog.claim("x.bad").unwrap().clone();
        let error = catalog.load_language(&entry).unwrap_err();
        assert!(error.contains("escapes the plugin"), "{error}");
    }

    #[test]
    fn a_library_grammar_is_refused_clearly_for_now() {
        let dir = temp("library");
        write(
            &dir,
            "exotic",
            r#"{"id":"exotic","name":"Exotic","language":{
                 "extensions":["exo"],
                 "grammar":{"kind":"library","path":"exotic.dll","symbol":"tree_sitter_exotic"}}}"#,
        );
        let index = Catalog::rescan(&dir);
        let catalog = Catalog::from_entries(dir, index.plugins);
        let entry = catalog.claim("a.exo").unwrap().clone();
        let error = catalog.load_language(&entry).unwrap_err();
        assert!(error.contains("not loadable yet"), "{error}");
    }

    #[test]
    fn a_configured_folder_that_is_missing_falls_back() {
        assert_eq!(resolve_dir(""), default_dir());
        assert_eq!(resolve_dir("Z:/nope/not/here"), default_dir());
    }
}

// ---------------------------------------------------------------------------
// The catalogue in force
// ---------------------------------------------------------------------------

use std::sync::{OnceLock, RwLock, RwLockReadGuard};

static CATALOG: OnceLock<RwLock<Catalog>> = OnceLock::new();

fn slot() -> &'static RwLock<Catalog> {
    CATALOG.get_or_init(|| RwLock::new(Catalog::default()))
}

/// The catalogue, for a lookup. Empty until `install` has run, so anything
/// asking before then is told "no plugin" rather than made to wait.
pub fn catalog() -> RwLockReadGuard<'static, Catalog> {
    slot().read().unwrap_or_else(|e| e.into_inner())
}

pub fn install(catalog: Catalog) {
    *slot().write().unwrap_or_else(|e| e.into_inner()) = catalog;
}

/// Reads the cached index for the configured folder, installs it, and then
/// checks the folder for changes off the main thread.
///
/// Startup does the first half only, and the first half is one small file.
/// The rescan -- which opens every manifest -- runs on a background thread and
/// swaps its result in when it finishes, so installing a plugin takes effect
/// without a restart and nothing waits on the disk before the first frame.
pub fn install_from_settings(cx: &mut gpui::App) {
    let dir = resolve_dir(&cx.global::<crate::store::Settings>().plugins_dir);
    install(Catalog::load(dir.clone()));

    cx.background_executor()
        .spawn(async move {
            let cached: Index = fs::read_to_string(dir.join(INDEX_FILE))
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
            if cached.version == INDEX_VERSION && !Catalog::is_stale(&dir, &cached) {
                return;
            }
            let fresh = Catalog::rescan(&dir);
            // Best effort: a read-only plugins folder still works, it just
            // pays for a scan on every launch.
            let _ = Catalog::write_index(&dir, &fresh);
            install(Catalog::from_entries(dir, fresh.plugins));
        })
        .detach();
}
