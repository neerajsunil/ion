//! Finding a file reference (`src/main.rs:12:5`) in a line of terminal output.

/// A file mentioned in terminal output, with an optional 1-based position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLink {
    pub path: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

fn is_path_char(c: char) -> bool {
    !c.is_whitespace()
        && !matches!(
            c,
            '"' | '\'' | '`' | '<' | '>' | '[' | ']' | '{' | '}' | '|' | ';'
        )
}

/// The file reference under `column` (a char index) in `line`, if any.
/// Recognizes `path`, `path:line`, `path:line:col` and `path(line,col)`.
pub fn link_at(line: &str, column: usize) -> Option<FileLink> {
    let chars: Vec<char> = line.chars().collect();
    if column >= chars.len() || !is_path_char(chars[column]) {
        return None;
    }
    let mut start = column;
    while start > 0 && is_path_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = column + 1;
    while end < chars.len() && is_path_char(chars[end]) {
        end += 1;
    }
    let token: String = chars[start..end].iter().collect();
    parse(&token)
}

/// The web address under `column` in `line`: an `http(s)://` URL, or a
/// bare `localhost:3000` (dev servers print both).
pub fn url_at(line: &str, column: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let is_url_char = |c: char| !c.is_whitespace() && !matches!(c, '"' | '\'' | '`' | '<' | '>');
    if column >= chars.len() || !is_url_char(chars[column]) {
        return None;
    }
    let mut start = column;
    while start > 0 && is_url_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = column + 1;
    while end < chars.len() && is_url_char(chars[end]) {
        end += 1;
    }
    let token: String = chars[start..end].iter().collect();
    let token = token.trim_end_matches(['.', ',', ';', ':', ')', ']', '}']);
    if let Some(at) = token.find("http://").or_else(|| token.find("https://")) {
        return Some(token[at..].to_owned());
    }
    let token = token.trim_start_matches(['(', '[']);
    let (host, rest) = token.split_once(':')?;
    let port_len = rest.chars().take_while(char::is_ascii_digit).count();
    let local = matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]");
    if !local || port_len == 0 || !matches!(rest[port_len..].chars().next(), None | Some('/')) {
        return None;
    }
    let host = if host == "0.0.0.0" { "localhost" } else { host };
    Some(format!("http://{host}:{rest}"))
}

fn parse(token: &str) -> Option<FileLink> {
    // `file.rs(12,5)` (MSBuild, C#) and trailing punctuation.
    let token = token.trim_end_matches(['.', ':', ')', ',']);
    let token = token
        .trim_start_matches(['(', '.'])
        .trim_start_matches("a/")
        .trim_start_matches("b/");
    let (path, line, column) = if let Some((path, position)) = token.split_once('(') {
        let mut numbers = position.split(',').map(|n| n.trim().parse().ok());
        (
            path.to_owned(),
            numbers.next().flatten(),
            numbers.next().flatten(),
        )
    } else {
        let mut parts: Vec<String> = token.split(':').map(str::to_owned).collect();
        // A Windows drive letter isn't a position.
        if parts.len() > 1
            && parts[0].len() == 1
            && parts[0].chars().all(|c| c.is_ascii_alphabetic())
        {
            let drive = format!("{}:{}", parts[0], parts[1]);
            parts.splice(0..2, [drive]);
        }
        let numbers: Vec<Option<usize>> = parts[1..].iter().map(|n| n.parse().ok()).collect();
        let line = numbers.first().copied().flatten();
        let column = numbers.get(1).copied().flatten();
        // `host:path` or `http://…` aren't files.
        if parts.len() > 1 && line.is_none() {
            return None;
        }
        (parts.swap_remove(0), line, column)
    };
    let path = path.as_str();
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let looks_like_file =
        name.contains('.') && !name.starts_with('.') || path.contains(['/', '\\']);
    if path.is_empty() || !looks_like_file || path.contains("://") || name.ends_with('.') {
        return None;
    }
    Some(FileLink {
        path: path.to_owned(),
        line,
        column,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_urls() {
        let url = |line: &str, needle: &str| url_at(line, line.find(needle).unwrap());
        assert_eq!(
            url("  Local:   http://localhost:5173/", "5173").as_deref(),
            Some("http://localhost:5173/")
        );
        assert_eq!(
            url("listening on 0.0.0.0:8000.", "8000").as_deref(),
            Some("http://localhost:8000")
        );
        assert_eq!(
            url("see (https://example.com/a).", "example").as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(url("src/main.rs:12:5", "main"), None);
        assert_eq!(url("localhost:abc", "local"), None);
    }

    fn at(line: &str, needle: &str) -> Option<FileLink> {
        link_at(line, line.find(needle).unwrap())
    }

    #[test]
    fn finds_compiler_style_references() {
        let link = at("error: --> src/main.rs:12:5", "main").unwrap();
        assert_eq!(
            (link.path.as_str(), link.line, link.column),
            ("src/main.rs", Some(12), Some(5))
        );
        let link = at("  at foo (lib/app.js:3)", "app").unwrap();
        assert_eq!((link.path.as_str(), link.line), ("lib/app.js", Some(3)));
        let link = at(r"C:\code\Program.cs(10,4): error", "Program").unwrap();
        assert_eq!(
            (link.path.as_str(), link.line, link.column),
            (r"C:\code\Program.cs", Some(10), Some(4))
        );
        let link = at(r"see C:\code\main.rs:7:1.", "main").unwrap();
        assert_eq!(
            (link.path.as_str(), link.line),
            (r"C:\code\main.rs", Some(7))
        );
        let link = at("modified:   crates/ui/src/lib.rs", "lib").unwrap();
        assert_eq!(
            (link.path.as_str(), link.line),
            ("crates/ui/src/lib.rs", None)
        );
        let link = at("+++ b/src/a.rs", "src").unwrap();
        assert_eq!(link.path, "src/a.rs");
    }

    #[test]
    fn ignores_things_that_arent_files() {
        assert_eq!(at("visit https://example.com/a.html now", "example"), None);
        assert_eq!(at("hello world", "world"), None);
        assert_eq!(at("user@host:project", "host"), None);
        assert_eq!(link_at("abc", 10), None);
    }
}
