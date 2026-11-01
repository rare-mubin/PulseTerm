//! The languages the code editor highlights.
//!
//! gpui-component's `tree-sitter-languages` feature is all-or-nothing: 28
//! grammars, whose parse tables were ~30 MB of the release binary. So the
//! feature is off and the grammars worth keeping are registered here instead.
//! Each entry mirrors what that feature registered -- same grammar crate, same
//! queries, same injected languages -- minus C#, Scala, Swift, SQL, Ruby and
//! Elixir, which now open as plain text. Where gpui-component used query files
//! of its own rather than the grammar crate's, they are copied under
//! `assets/queries` (Apache-2.0; licence alongside), so highlighting looks the
//! same as before.

use gpui::SharedString;
use gpui_component::highlighter::{LanguageConfig, LanguageRegistry};

/// A query file copied from gpui-component, compiled in.
macro_rules! query {
    ($path:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/queries/", $path))
    };
}

/// The language a file with no recognised name or extension opens as: a
/// grammar with no highlight query, so nothing is coloured.
pub const PLAIN: &str = "text";

/// Languages that JavaScript and TypeScript inject into template literals and
/// the like. gpui-component's list, less `sql`.
const JS_INJECTIONS: &[&str] = &[
    "jsdoc",
    "json",
    "css",
    "html",
    "typescript",
    "javascript",
    "tsx",
    "yaml",
    "graphql",
];

fn config(
    name: &str,
    language: tree_sitter::Language,
    injections: &[&'static str],
    highlights: &str,
    injection_query: &str,
    locals: &str,
) -> LanguageConfig {
    LanguageConfig::new(
        name.to_string(),
        language,
        injections.iter().map(|&name| SharedString::from(name)).collect(),
        highlights,
        injection_query,
        locals,
    )
}

/// Every language this build highlights, with the other names it answers to.
///
/// The aliases matter more than they look. gpui-component used to resolve
/// `rs`, `py` or `markdown-inline` through an alias table that belongs to the
/// feature that's now off -- and with it off, that lookup answers *JSON* for
/// every unknown name. So each alias is registered as a name in its own right:
/// the editor asks by file extension, and markdown code fences and injection
/// queries ask by whatever name they were written with.
fn languages() -> Vec<(&'static [&'static str], LanguageConfig)> {
    // tree-sitter ships C++ highlights as a *fragment*: the query file opens
    // with `; inherits: c`, which is an instruction to the editor, not something
    // the file carries. On its own it colours `virtual`, `concept` and the other
    // C++-only keywords -- and nothing else, so `if`, `while`, `return`, every
    // type and every comment stay plain. C's query is the missing base. It goes
    // *after* the C++ one: where two patterns match the same token, this
    // highlighter keeps the earlier, so C++'s own rules must come first.
    let cpp_highlights = format!(
        "{}\n{}",
        tree_sitter_cpp::HIGHLIGHT_QUERY,
        tree_sitter_c::HIGHLIGHT_QUERY
    );

    vec![
        (
            &[],
            config(PLAIN, tree_sitter_bash::LANGUAGE.into(), &[], "", "", ""),
        ),
        (
            &["md", "mdx"],
            config(
                "markdown",
                tree_sitter_md::LANGUAGE.into(),
                &["markdown-inline", "html", "toml", "yaml"],
                query!("markdown/highlights.scm"),
                query!("markdown/injections.scm"),
                "",
            ),
        ),
        (
            &["markdown-inline"],
            config(
                "markdown_inline",
                tree_sitter_md::INLINE_LANGUAGE.into(),
                &[],
                query!("markdown_inline/highlights.scm"),
                "",
                "",
            ),
        ),
        (
            &[],
            config(
                "toml",
                tree_sitter_toml_ng::LANGUAGE.into(),
                &[],
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        ),
        (
            &["yml"],
            config(
                "yaml",
                tree_sitter_yaml::LANGUAGE.into(),
                &[],
                tree_sitter_yaml::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        ),
        (
            &["rs"],
            config(
                "rust",
                tree_sitter_rust::LANGUAGE.into(),
                &["rust"],
                query!("rust/highlights.scm"),
                query!("rust/injections.scm"),
                "",
            ),
        ),
        (
            &[],
            config(
                "go",
                tree_sitter_go::LANGUAGE.into(),
                &[],
                query!("go/highlights.scm"),
                "",
                "",
            ),
        ),
        (
            &["h"],
            config(
                "c",
                tree_sitter_c::LANGUAGE.into(),
                &[],
                tree_sitter_c::HIGHLIGHT_QUERY,
                "",
                "",
            ),
        ),
        (
            &["c++", "cc", "cxx", "hpp", "hh", "hxx"],
            config(
                "cpp",
                tree_sitter_cpp::LANGUAGE.into(),
                &[],
                &cpp_highlights,
                "",
                "",
            ),
        ),
        (
            &["js", "mjs", "cjs", "jsx"],
            config(
                "javascript",
                tree_sitter_javascript::LANGUAGE.into(),
                JS_INJECTIONS,
                query!("javascript/highlights.scm"),
                query!("javascript/injections.scm"),
                tree_sitter_javascript::LOCALS_QUERY,
            ),
        ),
        (
            &[],
            config(
                "jsdoc",
                tree_sitter_jsdoc::LANGUAGE.into(),
                &[],
                tree_sitter_jsdoc::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        ),
        (
            &["ts", "mts", "cts"],
            config(
                "typescript",
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                JS_INJECTIONS,
                query!("typescript/highlights.scm"),
                query!("javascript/injections.scm"),
                tree_sitter_typescript::LOCALS_QUERY,
            ),
        ),
        (
            &[],
            config(
                "tsx",
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                JS_INJECTIONS,
                query!("typescript/highlights.scm"),
                "",
                tree_sitter_typescript::LOCALS_QUERY,
            ),
        ),
        (
            &[],
            config(
                "zig",
                tree_sitter_zig::LANGUAGE.into(),
                &[],
                query!("zig/highlights.scm"),
                query!("zig/injections.scm"),
                "",
            ),
        ),
        (
            &[],
            config(
                "java",
                tree_sitter_java::LANGUAGE.into(),
                &[],
                tree_sitter_java::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        ),
        (
            &["py", "pyi"],
            config(
                "python",
                tree_sitter_python::LANGUAGE.into(),
                &[],
                tree_sitter_python::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        ),
        (
            &["sh", "zsh"],
            config(
                "bash",
                tree_sitter_bash::LANGUAGE.into(),
                &[],
                tree_sitter_bash::HIGHLIGHT_QUERY,
                "",
                "",
            ),
        ),
        (
            &["htm"],
            config(
                "html",
                tree_sitter_html::LANGUAGE.into(),
                &["javascript", "css"],
                query!("html/highlights.scm"),
                query!("html/injections.scm"),
                "",
            ),
        ),
        (
            &["scss"],
            config(
                "css",
                tree_sitter_css::LANGUAGE.into(),
                &[],
                tree_sitter_css::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        ),
        // Grammar only, no highlight query -- as gpui-component had them. They
        // still get the editor's structure-aware behaviour, just no colour.
        (
            &[],
            config("graphql", tree_sitter_graphql::LANGUAGE.into(), &[], "", "", ""),
        ),
        (
            &["protobuf"],
            config("proto", tree_sitter_proto::LANGUAGE.into(), &[], "", "", ""),
        ),
        (
            &[],
            config("cmake", tree_sitter_cmake::LANGUAGE.into(), &[], "", "", ""),
        ),
        (
            &["makefile", "mk"],
            config(
                "make",
                tree_sitter_make::LANGUAGE.into(),
                &[],
                tree_sitter_make::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        ),
        (
            &["patch"],
            config(
                "diff",
                tree_sitter_diff::LANGUAGE.into(),
                &[],
                tree_sitter_diff::HIGHLIGHTS_QUERY,
                "",
                "",
            ),
        ),
        (
            &[],
            config(
                "erb",
                tree_sitter_embedded_template::LANGUAGE.into(),
                &[],
                tree_sitter_embedded_template::HIGHLIGHTS_QUERY,
                tree_sitter_embedded_template::INJECTIONS_EJS_QUERY,
                "",
            ),
        ),
        (
            &[],
            config(
                "ejs",
                tree_sitter_embedded_template::LANGUAGE.into(),
                &[],
                tree_sitter_embedded_template::HIGHLIGHTS_QUERY,
                tree_sitter_embedded_template::INJECTIONS_EJS_QUERY,
                "",
            ),
        ),
    ]
}

/// Registers every language, and its aliases, with gpui-component's registry.
/// JSON is the one it still registers itself with the feature off.
pub fn register() {
    let registry = LanguageRegistry::singleton();
    for (aliases, config) in languages() {
        registry.register(&config.name, &config);
        for alias in aliases {
            registry.register(alias, &config);
        }
    }
    if let Some(json) = registry.language("json") {
        registry.register("jsonc", &json);
    }
}

/// A grammar this binary carries, by the name a plugin manifest asks for.
///
/// This is the whole of `GrammarSource::Builtin`: a plugin describes a file
/// kind -- its extensions, its queries, its icon -- and borrows a parser that
/// is already linked in, so adding one costs no code loading and cannot bring
/// a bad parser into the process.
pub fn builtin_grammar(name: &str) -> Option<tree_sitter::Language> {
    Some(match name {
        "bash" | "text" => tree_sitter_bash::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cmake" => tree_sitter_cmake::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "css" => tree_sitter_css::LANGUAGE.into(),
        "diff" => tree_sitter_diff::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "graphql" => tree_sitter_graphql::LANGUAGE.into(),
        "html" => tree_sitter_html::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "jsdoc" => tree_sitter_jsdoc::LANGUAGE.into(),
        "make" => tree_sitter_make::LANGUAGE.into(),
        "markdown" => tree_sitter_md::LANGUAGE.into(),
        "markdown-inline" => tree_sitter_md::INLINE_LANGUAGE.into(),
        "proto" => tree_sitter_proto::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "toml" => tree_sitter_toml_ng::LANGUAGE.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "yaml" => tree_sitter_yaml::LANGUAGE.into(),
        "zig" => tree_sitter_zig::LANGUAGE.into(),
        _ => return None,
    })
}

/// Registers a plugin's language the first time a file wants it.
///
/// This is the lazy half of the plugin system: the catalogue knows which
/// plugin claims a file from the cached index alone, and only here -- with a
/// file actually being opened -- is the manifest re-read, the `.scm` queries
/// pulled off the disk and the language handed to the registry. A plugin that
/// is never used is never read past its index entry.
fn from_plugins(file_name: &str) -> Option<SharedString> {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static DONE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

    let entry = crate::plugins::catalog().claim(file_name).cloned()?;
    let name = SharedString::from(entry.id.clone());

    let done = DONE.get_or_init(|| Mutex::new(HashSet::new()));
    let mut done = done.lock().unwrap_or_else(|e| e.into_inner());
    if done.contains(&entry.id) {
        return Some(name);
    }

    let loaded = match crate::plugins::catalog().load_language(&entry) {
        Ok(loaded) => loaded,
        Err(error) => {
            // Remembered as done either way: a plugin that cannot load should
            // report itself once, not on every keystroke that opens a file.
            eprintln!("plugin {}: {error}", entry.id);
            done.insert(entry.id);
            return None;
        }
    };
    let Some(grammar) = builtin_grammar(&loaded.grammar) else {
        eprintln!("plugin {}: no built-in grammar named {}", entry.id, loaded.grammar);
        done.insert(entry.id);
        return None;
    };

    let registry = LanguageRegistry::singleton();
    let config = LanguageConfig::new(
        loaded.id.clone(),
        grammar,
        Vec::new(),
        loaded.highlights.as_deref().unwrap_or(""),
        loaded.injections.as_deref().unwrap_or(""),
        "",
    );
    registry.register(&loaded.id, &config);
    for alias in &loaded.aliases {
        registry.register(alias, &config);
    }
    done.insert(entry.id);
    Some(name)
}

/// The registered language to open a file as, from its name.
///
/// Never an unregistered name: the registry's own fallback for one is JSON,
/// which would colour a log file or a README as if it were JSON.
pub fn for_file_name(file_name: &str) -> SharedString {
    // Plugins first: an installed plugin is an explicit choice by whoever
    // installed it, so it may replace a built-in as well as extend the set.
    // A plugin that fails to load falls through to the built-in below rather
    // than leaving the file unhighlighted.
    if let Some(name) = from_plugins(file_name) {
        return name;
    }
    let lower = file_name.to_ascii_lowercase();
    let by_name = match lower.as_str() {
        "makefile" | "gnumakefile" => Some("make"),
        "cmakelists.txt" => Some("cmake"),
        ".bashrc" | ".bash_profile" | ".bash_logout" | ".bash_aliases" | ".profile"
        | ".zshrc" | ".zprofile" => Some("bash"),
        _ => None,
    };
    if let Some(language) = by_name {
        return language.into();
    }

    let extension = lower.rsplit_once('.').map(|(_, extension)| extension);
    let registered = LanguageRegistry::singleton().languages();
    match extension {
        Some(extension) if registered.iter().any(|name| name.as_ref() == extension) => {
            extension.to_string().into()
        }
        _ => PLAIN.into(),
    }
}

#[cfg(test)]
mod tests {
    /// A query that doesn't compile against its grammar's version doesn't
    /// fail loudly at runtime -- the file just opens uncoloured. So every one
    /// is compiled here, against the grammar it is registered with.
    #[test]
    fn every_query_compiles_against_its_grammar() {
        for (_, config) in super::languages() {
            for (kind, query) in [
                ("highlights", &config.highlights),
                ("injections", &config.injections),
                ("locals", &config.locals),
            ] {
                if query.is_empty() {
                    continue;
                }
                if let Err(error) = tree_sitter::Query::new(&config.language, query) {
                    panic!("{} {kind} query: {error}", config.name);
                }
            }
        }
    }

    /// Compiling isn't enough: an upstream query can be a fragment that
    /// inherits another language's, and then it parses fine while colouring
    /// almost nothing -- which is how C++ shipped here with plain `while` and
    /// `return`. So a real keyword has to come out styled.
    #[test]
    fn keywords_are_highlighted() {
        use gpui_component::highlighter::{HighlightTheme, SyntaxHighlighter};
        use gpui_component::Rope;

        super::register();
        let theme = HighlightTheme::default_dark();

        for (language, code, keyword) in [
            ("cpp", "static void f() { while (true) { return; } }", "while"),
            ("c", "static void f(void) { while (1) { return; } }", "while"),
            ("typescript", "const x: number = 1;", "const"),
            ("tsx", "const f = () => { return 1; };", "return"),
            ("rust", "fn main() { let x = 1; }", "fn"),
            ("python", "def f():
    return 1
", "def"),
        ] {
            let mut highlighter = SyntaxHighlighter::new(language);
            highlighter.update(None, &Rope::from(code));
            let offset = code.find(keyword).expect("keyword is in the sample");

            let styled = highlighter
                .styles(&(0..code.len()), &theme)
                .into_iter()
                .any(|(range, style)| range.contains(&offset) && style.color.is_some());

            assert!(styled, "{language}: `{keyword}` came out unhighlighted");
        }
    }

    #[test]
    fn file_names_map_to_registered_languages() {
        super::register();
        assert_eq!(super::for_file_name("main.rs"), "rs");
        assert_eq!(super::for_file_name("train.PY"), "py");
        assert_eq!(super::for_file_name("Makefile"), "make");
        assert_eq!(super::for_file_name(".bashrc"), "bash");
        assert_eq!(super::for_file_name("server.log"), super::PLAIN);
        assert_eq!(super::for_file_name("Program.cs"), super::PLAIN);
        assert_eq!(super::for_file_name("README"), super::PLAIN);
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use std::fs;

    /// The whole lazy path, end to end: a plugin folder on disk, an index
    /// built from it, and a file name resolving to a language that was not
    /// compiled in -- with the manifest and the query read only at that point.
    #[test]
    fn a_plugin_adds_a_language_when_a_file_wants_it() {
        // `for_file_name` only ever answers with a *registered* name, so the
        // built-ins have to be in place before a fall-through can be checked.
        register();

        let dir = std::env::temp_dir().join("pulseterm-plugin-language");
        let _ = fs::remove_dir_all(&dir);
        let folder = dir.join("pulse-demo");
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join("plugin.json"),
            r#"{"id":"pulse-demo","name":"Pulse Demo","language":{
                 "extensions":["pulse"],"aliases":["pulsedemo"],
                 "grammar":{"kind":"builtin","name":"rust"},
                 "highlights":"highlights.scm"}}"#,
        )
        .unwrap();
        // A real query against the borrowed grammar, so registration is not
        // merely accepted but usable.
        fs::write(folder.join("highlights.scm"), "\"fn\" @keyword\n").unwrap();

        let index = crate::plugins::Catalog::rescan(&dir);
        assert_eq!(index.plugins.len(), 1, "the manifest should be indexed");
        crate::plugins::install(crate::plugins::Catalog::from_entries(dir, index.plugins));

        // `language()` answers for *any* name -- an unregistered one falls
        // back to JSON (see the module docs) -- so "registered" means the
        // config that comes back is actually ours.
        let registered = |name: &str| {
            LanguageRegistry::singleton()
                .language(name)
                .map(|config| config.name.to_string())
        };

        // Nothing is loaded yet: the index alone never reads a manifest.
        assert_ne!(registered("pulse-demo").as_deref(), Some("pulse-demo"));

        // Opening a file it claims is what loads it.
        assert_eq!(for_file_name("thing.pulse"), SharedString::from("pulse-demo"));
        assert_eq!(registered("pulse-demo").as_deref(), Some("pulse-demo"));
        assert_eq!(registered("pulsedemo").as_deref(), Some("pulse-demo"));

        // A file it does not claim still resolves the built-in way. The
        // answer is the extension itself because aliases are registered as
        // names in their own right -- `rs` and `rust` are the same grammar.
        assert_eq!(for_file_name("main.rs"), SharedString::from("rs"));

        crate::plugins::install(crate::plugins::Catalog::default());
    }
}
