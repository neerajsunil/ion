//! File-type icons: a glyph and a tint per file name, so lists of paths
//! (git changes, file tree) can be scanned by type at a glance.

use gpui::{Hsla, Pixels, Svg, hsla};

use crate::icons::{IconName, icon_sized};

/// The icon for `path` (a file name or `/`-separated path) at `size`.
pub fn file_icon(path: &str, size: Pixels) -> Svg {
    let (name, color) = file_icon_for(path);
    icon_sized(name, size, color)
}

/// The glyph and tint for `path`, chosen by file name, then extension.
pub fn file_icon_for(path: &str) -> (IconName, Hsla) {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    let (icon, hue) = by_name(&lower).unwrap_or_else(|| {
        let ext = lower.rsplit_once('.').map_or("", |(_, ext)| ext);
        by_extension(ext)
    });
    (icon, tint(hue))
}

/// Hue in degrees, or `None` for the neutral (muted text) tint.
type Hue = Option<f32>;

const RED: Hue = Some(4.);
const ORANGE: Hue = Some(22.);
const YELLOW: Hue = Some(48.);
const GREEN: Hue = Some(130.);
const TEAL: Hue = Some(172.);
const CYAN: Hue = Some(192.);
const BLUE: Hue = Some(212.);
const PURPLE: Hue = Some(270.);
const PINK: Hue = Some(326.);
const NEUTRAL: Hue = None;

fn tint(hue: Hue) -> Hsla {
    let light = theme::appearance() == theme::Appearance::Light;
    match hue {
        Some(h) if light => hsla(h / 360., 0.65, 0.42, 1.),
        Some(h) => hsla(h / 360., 0.6, 0.62, 1.),
        None => theme::text_muted(),
    }
}

fn by_name(name: &str) -> Option<(IconName, Hue)> {
    use IconName::*;
    Some(match name {
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "bun.lockb"
        | "poetry.lock" | "uv.lock" | "gemfile.lock" | "composer.lock" | "flake.lock" => {
            (FileLock, NEUTRAL)
        }
        "cargo.toml" => (FileCog, ORANGE),
        "package.json" | "tsconfig.json" | "jsconfig.json" => (FileCog, GREEN),
        "dockerfile"
        | "containerfile"
        | "docker-compose.yml"
        | "docker-compose.yaml"
        | "compose.yml"
        | "compose.yaml" => (FileCog, BLUE),
        "makefile" | "justfile" | "cmakelists.txt" | "build.gradle" | "pom.xml" => {
            (FileCog, ORANGE)
        }
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitkeep" => (FileCog, RED),
        ".editorconfig"
        | ".prettierrc"
        | ".eslintrc"
        | ".npmrc"
        | "rustfmt.toml"
        | "clippy.toml"
        | "rust-toolchain"
        | "rust-toolchain.toml" => (FileCog, NEUTRAL),
        ".env" => (FileKey, YELLOW),
        "license" | "licence" | "license.md" | "license.txt" | "copying" => (FileText, YELLOW),
        _ if name.starts_with(".env.") => (FileKey, YELLOW),
        _ if name.starts_with("readme") => (FileText, BLUE),
        _ => return None,
    })
}

fn by_extension(ext: &str) -> (IconName, Hue) {
    use IconName::*;
    match ext {
        "rs" => (FileCode, ORANGE),
        "ts" | "tsx" | "mts" | "cts" => (FileCode, BLUE),
        "js" | "jsx" | "mjs" | "cjs" => (FileCode, YELLOW),
        "py" | "pyi" => (FileCode, BLUE),
        "go" => (FileCode, CYAN),
        "c" | "h" => (FileCode, BLUE),
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => (FileCode, PURPLE),
        "cs" => (FileCode, PURPLE),
        "java" | "kt" | "kts" | "scala" => (FileCode, RED),
        "swift" => (FileCode, ORANGE),
        "rb" | "erb" => (FileCode, RED),
        "php" => (FileCode, PURPLE),
        "lua" => (FileCode, BLUE),
        "zig" => (FileCode, ORANGE),
        "dart" => (FileCode, CYAN),
        "ex" | "exs" | "erl" | "hs" | "ml" | "mli" | "clj" | "elm" => (FileCode, PURPLE),
        "vue" => (FileCode, GREEN),
        "svelte" => (FileCode, ORANGE),
        "html" | "htm" => (FileCode, ORANGE),
        "css" | "scss" | "sass" | "less" => (FileCode, BLUE),
        "sql" => (FileCode, PINK),
        "wgsl" | "glsl" | "hlsl" | "vert" | "frag" => (FileCode, TEAL),
        "json" | "jsonc" | "json5" => (FileBraces, YELLOW),
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "xml" | "plist" => (FileCog, NEUTRAL),
        "lock" => (FileLock, NEUTRAL),
        "md" | "mdx" | "markdown" | "rst" => (FileText, BLUE),
        "txt" | "log" => (FileText, NEUTRAL),
        "pdf" => (FileText, RED),
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "psm1" | "bat" | "cmd" | "nu" => {
            (FileTerminal, GREEN)
        }
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "svg" | "tif" | "tiff"
        | "avif" => (FileImage, PURPLE),
        "mp4" | "mov" | "mkv" | "webm" | "avi" => (FilePlay, PINK),
        "mp3" | "wav" | "flac" | "ogg" | "m4a" => (FileMusic, PINK),
        "ttf" | "otf" | "woff" | "woff2" => (FileType, RED),
        "zip" | "tar" | "gz" | "tgz" | "xz" | "zst" | "7z" | "rar" | "bz2" => (FileArchive, YELLOW),
        "csv" | "tsv" | "xls" | "xlsx" => (FileSpreadsheet, GREEN),
        "diff" | "patch" => (FileDiff, GREEN),
        "pem" | "key" | "crt" | "cer" | "pub" => (FileKey, YELLOW),
        _ => (File, NEUTRAL),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_by_name_then_extension() {
        assert_eq!(file_icon_for("crates/ui/Cargo.toml").0, IconName::FileCog);
        assert_eq!(file_icon_for("Cargo.lock").0, IconName::FileLock);
        assert_eq!(file_icon_for("src/main.rs").0, IconName::FileCode);
        assert_eq!(file_icon_for("a/b/.env.local").0, IconName::FileKey);
        assert_eq!(file_icon_for("docs/README.md").0, IconName::FileText);
        assert_eq!(file_icon_for("logo.SVG").0, IconName::FileImage);
        assert_eq!(file_icon_for("noext").0, IconName::File);
    }
}
