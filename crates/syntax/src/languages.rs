//! The built-in languages and how files map to them.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use text::IndentRules;
use tree_sitter::Query;

use crate::Highlight;

struct LanguageDef {
    name: &'static str,
    extensions: &'static [&'static str],
    file_names: &'static [&'static str],
    grammar: fn() -> tree_sitter::Language,
    /// Highlight query sources, concatenated. Later patterns win when two
    /// match the same node, so a base language's query goes first and the
    /// query that extends it after.
    highlights: &'static [&'static str],
    /// Tags query sources (`@definition.*` and `@name` captures), for the
    /// symbols in a file. Empty for languages without definitions.
    tags: &'static [&'static str],
}

const SHELL_TAGS: &str = "(function_definition name: (word) @name) @definition.function";

/// Headings, captured with their section so subsections nest.
const MARKDOWN_TAGS: &str = "
(section (atx_heading heading_content: (_) @name)) @definition.heading
(section (setext_heading heading_content: (_) @name)) @definition.heading
";

const LANGUAGES: &[LanguageDef] = &[
    LanguageDef {
        name: "Rust",
        extensions: &["rs"],
        file_names: &[],
        grammar: || tree_sitter_rust::LANGUAGE.into(),
        highlights: &[tree_sitter_rust::HIGHLIGHTS_QUERY],
        tags: &[tree_sitter_rust::TAGS_QUERY],
    },
    LanguageDef {
        name: "JavaScript",
        extensions: &["js", "mjs", "cjs", "jsx"],
        file_names: &[],
        grammar: || tree_sitter_javascript::LANGUAGE.into(),
        highlights: &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
        ],
        tags: &[tree_sitter_javascript::TAGS_QUERY],
    },
    LanguageDef {
        name: "TypeScript",
        extensions: &["ts", "mts", "cts"],
        file_names: &[],
        grammar: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        highlights: &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ],
        tags: &[
            tree_sitter_javascript::TAGS_QUERY,
            tree_sitter_typescript::TAGS_QUERY,
        ],
    },
    LanguageDef {
        name: "TSX",
        extensions: &["tsx"],
        file_names: &[],
        grammar: || tree_sitter_typescript::LANGUAGE_TSX.into(),
        highlights: &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ],
        tags: &[
            tree_sitter_javascript::TAGS_QUERY,
            tree_sitter_typescript::TAGS_QUERY,
        ],
    },
    LanguageDef {
        name: "Python",
        extensions: &["py", "pyi", "pyw"],
        file_names: &[],
        grammar: || tree_sitter_python::LANGUAGE.into(),
        highlights: &[tree_sitter_python::HIGHLIGHTS_QUERY],
        tags: &[tree_sitter_python::TAGS_QUERY],
    },
    LanguageDef {
        name: "Go",
        extensions: &["go"],
        file_names: &[],
        grammar: || tree_sitter_go::LANGUAGE.into(),
        highlights: &[tree_sitter_go::HIGHLIGHTS_QUERY],
        tags: &[tree_sitter_go::TAGS_QUERY],
    },
    LanguageDef {
        name: "C",
        extensions: &["c", "h"],
        file_names: &[],
        grammar: || tree_sitter_c::LANGUAGE.into(),
        highlights: &[tree_sitter_c::HIGHLIGHT_QUERY],
        tags: &[tree_sitter_c::TAGS_QUERY],
    },
    LanguageDef {
        name: "C++",
        extensions: &["cc", "cpp", "cxx", "c++", "hpp", "hh", "hxx", "h++", "inl"],
        file_names: &[],
        grammar: || tree_sitter_cpp::LANGUAGE.into(),
        highlights: &[
            tree_sitter_c::HIGHLIGHT_QUERY,
            tree_sitter_cpp::HIGHLIGHT_QUERY,
        ],
        tags: &[tree_sitter_cpp::TAGS_QUERY],
    },
    LanguageDef {
        name: "C#",
        extensions: &["cs", "csx"],
        file_names: &[],
        grammar: || tree_sitter_c_sharp::LANGUAGE.into(),
        highlights: &[tree_sitter_c_sharp::HIGHLIGHTS_QUERY],
        tags: &[tree_sitter_c_sharp::TAGS_QUERY],
    },
    LanguageDef {
        name: "Java",
        extensions: &["java"],
        file_names: &[],
        grammar: || tree_sitter_java::LANGUAGE.into(),
        highlights: &[tree_sitter_java::HIGHLIGHTS_QUERY],
        tags: &[tree_sitter_java::TAGS_QUERY],
    },
    LanguageDef {
        name: "JSON",
        extensions: &["json", "jsonc", "json5"],
        file_names: &[".prettierrc", ".babelrc", ".eslintrc"],
        grammar: || tree_sitter_json::LANGUAGE.into(),
        highlights: &[tree_sitter_json::HIGHLIGHTS_QUERY],
        tags: &[],
    },
    LanguageDef {
        name: "HTML",
        extensions: &["html", "htm", "xhtml"],
        file_names: &[],
        grammar: || tree_sitter_html::LANGUAGE.into(),
        highlights: &[tree_sitter_html::HIGHLIGHTS_QUERY],
        tags: &[],
    },
    LanguageDef {
        name: "CSS",
        extensions: &["css"],
        file_names: &[],
        grammar: || tree_sitter_css::LANGUAGE.into(),
        highlights: &[tree_sitter_css::HIGHLIGHTS_QUERY],
        tags: &[],
    },
    LanguageDef {
        name: "Shell",
        extensions: &["sh", "bash", "zsh"],
        file_names: &[".bashrc", ".bash_profile", ".zshrc", ".profile"],
        grammar: || tree_sitter_bash::LANGUAGE.into(),
        highlights: &[tree_sitter_bash::HIGHLIGHT_QUERY],
        tags: &[SHELL_TAGS],
    },
    LanguageDef {
        name: "TOML",
        extensions: &["toml"],
        file_names: &["Cargo.lock"],
        grammar: || tree_sitter_toml_ng::LANGUAGE.into(),
        highlights: &[tree_sitter_toml_ng::HIGHLIGHTS_QUERY],
        tags: &[],
    },
    LanguageDef {
        name: "YAML",
        extensions: &["yml", "yaml"],
        file_names: &[],
        grammar: || tree_sitter_yaml::LANGUAGE.into(),
        highlights: &[tree_sitter_yaml::HIGHLIGHTS_QUERY],
        tags: &[],
    },
    LanguageDef {
        name: "Markdown",
        extensions: &["md", "markdown"],
        file_names: &[],
        grammar: || tree_sitter_md::LANGUAGE.into(),
        highlights: &[tree_sitter_md::HIGHLIGHT_QUERY_BLOCK],
        tags: &[MARKDOWN_TAGS],
    },
    LanguageDef {
        name: "PHP",
        extensions: &["php"],
        file_names: &[],
        grammar: || tree_sitter_php::LANGUAGE_PHP.into(),
        highlights: &[tree_sitter_php::HIGHLIGHTS_QUERY],
        tags: &[tree_sitter_php::TAGS_QUERY],
    },
    LanguageDef {
        name: "Ruby",
        extensions: &["rb", "rake", "gemspec"],
        file_names: &["Gemfile", "Rakefile"],
        grammar: || tree_sitter_ruby::LANGUAGE.into(),
        highlights: &[tree_sitter_ruby::HIGHLIGHTS_QUERY],
        tags: &[tree_sitter_ruby::TAGS_QUERY],
    },
    LanguageDef {
        name: "Lua",
        extensions: &["lua"],
        file_names: &[],
        grammar: || tree_sitter_lua::LANGUAGE.into(),
        highlights: &[tree_sitter_lua::HIGHLIGHTS_QUERY],
        tags: &[tree_sitter_lua::TAGS_QUERY],
    },
    LanguageDef {
        name: "Swift",
        extensions: &["swift"],
        file_names: &[],
        grammar: || tree_sitter_swift::LANGUAGE.into(),
        highlights: &[tree_sitter_swift::HIGHLIGHTS_QUERY],
        tags: &[tree_sitter_swift::TAGS_QUERY],
    },
];

/// Identifies a built-in language. Cheap to get; see [`load`] for the parser
/// and queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LanguageId(usize);

impl LanguageId {
    pub fn name(self) -> &'static str {
        LANGUAGES[self.0].name
    }

    /// Source code, as opposed to prose (Markdown).
    pub fn is_code(self) -> bool {
        self.name() != "Markdown"
    }

    /// How to comment out a line: `(start, end)`, where `end` is empty for
    /// line comments. `None` for languages without comments (JSON).
    pub fn comment_tokens(self) -> Option<(&'static str, &'static str)> {
        Some(match self.name() {
            "Python" | "Shell" | "TOML" | "YAML" | "Ruby" => ("# ", ""),
            "Lua" => ("-- ", ""),
            "HTML" | "Markdown" => ("<!-- ", " -->"),
            "CSS" => ("/* ", " */"),
            "JSON" => return None,
            _ => ("// ", ""),
        })
    }

    /// How Enter indents. `None` for prose (Markdown), which keeps the
    /// line's indentation and nothing more.
    pub fn indent_rules(self) -> Option<IndentRules> {
        Some(match self.name() {
            "Markdown" => return None,
            "Python" => IndentRules {
                colon_opens: true,
                block_enders: &["return", "pass", "break", "continue", "raise"],
            },
            "YAML" => IndentRules {
                colon_opens: true,
                block_enders: &[],
            },
            _ => IndentRules::default(),
        })
    }

    pub fn all() -> impl Iterator<Item = LanguageId> {
        (0..LANGUAGES.len()).map(LanguageId)
    }
}

/// Picks a language from a file's name or extension.
pub fn detect(path: &Path) -> Option<LanguageId> {
    let file_name = path.file_name()?.to_str()?;
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    LANGUAGES
        .iter()
        .position(|def| {
            def.file_names.contains(&file_name)
                || extension
                    .as_deref()
                    .is_some_and(|ext| def.extensions.contains(&ext))
        })
        .map(LanguageId)
}

/// A loaded language: grammar plus compiled highlight query.
pub struct Language {
    pub id: LanguageId,
    pub(crate) grammar: tree_sitter::Language,
    pub(crate) query: Query,
    /// Highlight kind for each capture index in `query`.
    pub(crate) capture_highlights: Vec<Option<Highlight>>,
    /// Compiled on first use: most files are never asked for their symbols.
    tags: OnceLock<Option<Query>>,
}

impl Language {
    /// The tags query, or `None` if the language has none (or it doesn't
    /// compile, which the tests rule out).
    pub(crate) fn tags_query(&self) -> Option<&Query> {
        self.tags
            .get_or_init(|| {
                let sources = LANGUAGES[self.id.0].tags;
                if sources.is_empty() {
                    return None;
                }
                let mut query = Query::new(&self.grammar, &sources.join("\n")).ok()?;
                // Doc comments are matched with `*` quantifiers; skipping
                // them saves most of the query's work.
                query.disable_capture("doc");
                Some(query)
            })
            .as_ref()
    }

    /// Whether the language has a tags query.
    pub fn has_symbols(&self) -> bool {
        !LANGUAGES[self.id.0].tags.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn tags_error(&self) -> Option<String> {
        let sources = LANGUAGES[self.id.0].tags;
        if sources.is_empty() {
            return None;
        }
        Query::new(&self.grammar, &sources.join("\n"))
            .err()
            .map(|err| format!("{} tags query: {err}", LANGUAGES[self.id.0].name))
    }
}

/// Loads a language, compiling its highlight query on first use (a few
/// milliseconds, so call this off the UI thread). Results are cached.
pub fn load(id: LanguageId) -> Result<Arc<Language>, String> {
    static CACHE: OnceLock<Mutex<HashMap<LanguageId, Arc<Language>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(language) = cache.lock().expect("language cache poisoned").get(&id) {
        return Ok(language.clone());
    }

    let def = &LANGUAGES[id.0];
    let grammar = (def.grammar)();
    let query = Query::new(&grammar, &def.highlights.concat())
        .map_err(|err| format!("{} highlight query: {err}", def.name))?;
    let capture_highlights = query
        .capture_names()
        .iter()
        .map(|name| Highlight::from_capture_name(name))
        .collect();
    let language = Arc::new(Language {
        id,
        grammar,
        query,
        capture_highlights,
        tags: OnceLock::new(),
    });
    cache
        .lock()
        .expect("language cache poisoned")
        .insert(id, language.clone());
    Ok(language)
}
