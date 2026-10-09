//! Commands a project can run, found in its manifests: `package.json`
//! scripts, Cargo, Go and Makefile targets. Offered by the Run button.

use std::path::Path;

use crate::FileSystem;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunTask {
    /// What the menu shows, like `dev` or `cargo test`.
    pub label: String,
    /// What is typed into the terminal.
    pub command: String,
    /// The tool, used to group the menu: `npm`, `cargo`, `make`.
    pub source: &'static str,
}

/// Scripts most projects have, listed first in this order.
const COMMON_SCRIPTS: [&str; 5] = ["dev", "start", "build", "test", "lint"];

/// The tasks in the project at `root`.
pub fn detect_tasks(filesystem: &FileSystem, root: &Path) -> Vec<RunTask> {
    let read = |name: &str| {
        filesystem
            .load_text(&crate::join_path(root, name))
            .ok()
            .map(|loaded| loaded.text)
    };
    let exists = |name: &str| {
        filesystem
            .exists(&crate::join_path(root, name))
            .unwrap_or(false)
    };
    let mut tasks = Vec::new();
    if let Some(text) = read("package.json") {
        let runner = if exists("bun.lockb") || exists("bun.lock") {
            "bun"
        } else if exists("pnpm-lock.yaml") {
            "pnpm"
        } else if exists("yarn.lock") {
            "yarn"
        } else {
            "npm"
        };
        tasks.extend(package_scripts(&text, runner));
    }
    if exists("Cargo.toml") {
        for command in ["cargo run", "cargo test", "cargo check", "cargo build"] {
            tasks.push(RunTask {
                label: command.into(),
                command: command.into(),
                source: "cargo",
            });
        }
    }
    if exists("go.mod") {
        for command in ["go run .", "go test ./...", "go build ./..."] {
            tasks.push(RunTask {
                label: command.into(),
                command: command.into(),
                source: "go",
            });
        }
    }
    if let Some(text) = read("Makefile").or_else(|| read("makefile")) {
        tasks.extend(makefile_targets(&text).into_iter().map(|target| RunTask {
            command: format!("make {target}"),
            label: format!("make {target}"),
            source: "make",
        }));
    }
    tasks
}

/// `package.json` scripts, common ones first, run with `runner`.
pub fn package_scripts(text: &str, runner: &'static str) -> Vec<RunTask> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(scripts) = json.get("scripts").and_then(|scripts| scripts.as_object()) else {
        return Vec::new();
    };
    let mut names: Vec<&String> = scripts.keys().collect();
    names.sort_by_key(|name| {
        COMMON_SCRIPTS
            .iter()
            .position(|common| common == name)
            .unwrap_or(COMMON_SCRIPTS.len())
    });
    names
        .into_iter()
        .map(|name| {
            let command = match (runner, name.as_str()) {
                ("npm", "start" | "test") => format!("npm {name}"),
                ("npm", _) => format!("npm run {name}"),
                _ => format!("{runner} {name}"),
            };
            RunTask {
                label: name.clone(),
                command,
                source: runner,
            }
        })
        .collect()
}

/// A Makefile's explicit targets, in order: not special (`.PHONY`),
/// pattern (`%.o`) or variable (`X := 1`) lines.
pub fn makefile_targets(text: &str) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.starts_with(['\t', ' ', '#', '.']) {
            continue;
        }
        let Some((names, rest)) = line.split_once(':') else {
            continue;
        };
        if rest.starts_with('=') || names.contains(['=', '%', '$', '?', '+']) {
            continue;
        }
        for name in names.split_whitespace() {
            if !targets.iter().any(|known| known == name) {
                targets.push(name.to_owned());
            }
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_put_common_ones_first() {
        let json = r#"{"scripts": {"format": "x", "test": "x", "dev": "x"}}"#;
        let commands: Vec<String> = package_scripts(json, "npm")
            .into_iter()
            .map(|task| task.command)
            .collect();
        assert_eq!(commands, ["npm run dev", "npm test", "npm run format"]);
        let pnpm = package_scripts(json, "pnpm");
        assert_eq!(pnpm[0].command, "pnpm dev");
        assert!(package_scripts("{}", "npm").is_empty());
        assert!(package_scripts("not json", "npm").is_empty());
    }

    #[test]
    fn makefile_targets_skip_special_lines() {
        let text = ".PHONY: all test\nCC := gcc\nall: build\n\tgcc x.c\n%.o: %.c\nbuild test: deps\n# lint:\nX ?= 1\n";
        assert_eq!(makefile_targets(text), ["all", "build", "test"]);
    }
}
