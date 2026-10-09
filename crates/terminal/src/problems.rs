//! Finding compiler, linter and test errors in terminal output.
//!
//! Recognizes the formats most projects print:
//!
//! - rustc / cargo: `error[E0425]: message` then `  --> src/main.rs:3:5`,
//!   and test panics: `panicked at src/lib.rs:10:5:` then the message.
//! - tsc, MSBuild, C#: `src/a.ts(3,5): error TS2322: message`.
//! - tsc (pretty), pyright: `src/a.ts:3:5 - error TS2322: message`.
//! - gcc, clang, go, mypy, ruff: `file:3:5: error: message`.
//! - ESLint (stylish): a file name, then `  3:5  error  message  rule`.
//! - Python tracebacks: the last `File "x.py", line 3` and the exception.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
}

/// A problem found in the output, at a path as printed (maybe relative).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Problem {
    pub path: String,
    /// One-based line and column (column 0 when unknown).
    pub line: usize,
    pub column: usize,
    pub severity: Severity,
    pub message: String,
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid problem pattern")
}

/// Path characters: no spaces, quotes or brackets, ends with an extension.
const PATH: &str = r"(?P<path>[^\s:()'`\x22<>|]*?[^\s:()'`\x22<>|/\\]\.[A-Za-z0-9]{1,8}|[A-Za-z]:[\\/][^\s:()'`\x22<>|]*?\.[A-Za-z0-9]{1,8})";

static RUST_HEADER: LazyLock<Regex> =
    LazyLock::new(|| regex(r"^(?P<sev>error|warning)(?:\[\w+\])?: (?P<msg>.+)$"));
static RUST_LOCATION: LazyLock<Regex> =
    LazyLock::new(|| regex(&format!(r"^\s*--> {PATH}:(?P<line>\d+):(?P<col>\d+)")));
static RUST_PANIC: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r"panicked at {PATH}:(?P<line>\d+):(?P<col>\d+):?(?: (?P<msg>.+))?$"
    ))
});
static PAREN: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r"^\s*{PATH}\((?P<line>\d+),(?P<col>\d+)\): (?P<sev>error|warning)(?: \w+)?: (?P<msg>.+?)(?: \[[^\]]+\])?$"
    ))
});
static DASH: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r"^\s*{PATH}:(?P<line>\d+):(?P<col>\d+) - (?P<sev>error|warning)(?: \w+)?: (?P<msg>.+)$"
    ))
});
static COLON: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r"^\s*{PATH}:(?P<line>\d+):(?:(?P<col>\d+):)? (?:(?P<sev>fatal error|error|warning|note|[A-Z]\d{{3,4}}):? )?(?P<msg>.+)$"
    ))
});
static ESLINT_FILE: LazyLock<Regex> = LazyLock::new(|| regex(&format!(r"^{PATH}$")));
static ESLINT_ROW: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"^\s+(?P<line>\d+):(?P<col>\d+)\s+(?P<sev>error|warning)\s+(?P<msg>.+?)(?:\s{2,}\S+)?$")
});
static PY_FRAME: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"^\s*File "(?P<path>[^"]+)", line (?P<line>\d+)"#));
static PY_EXCEPTION: LazyLock<Regex> =
    LazyLock::new(|| regex(r"^(?P<msg>[A-Za-z_][\w.]*(?:Error|Exception|Exit|Interrupt)\b.*)$"));

/// Escape codes some tools print even when piped (colors).
fn strip_ansi(line: &str) -> std::borrow::Cow<'_, str> {
    static ANSI: LazyLock<Regex> = LazyLock::new(|| regex(r"\x1b\[[0-9;]*[A-Za-z]"));
    ANSI.replace_all(line, "")
}

fn severity(text: Option<&str>) -> Severity {
    match text {
        Some("warning" | "note") => Severity::Warning,
        Some(code) if code.starts_with('W') => Severity::Warning,
        _ => Severity::Error,
    }
}

fn number(captures: &regex::Captures, name: &str) -> usize {
    captures
        .name(name)
        .and_then(|value| value.as_str().parse().ok())
        .unwrap_or(0)
}

/// A line like `file:3: message` without a severity is often not an error
/// (logs, grep output); keep it only when it says so.
fn looks_like_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "error",
        "undefined",
        "cannot",
        "expected",
        "failed",
        "panic",
    ]
    .iter()
    .any(|word| lower.contains(word))
}

/// The problems in `lines` of output, in order, without duplicates.
pub fn parse_problems(lines: &[String]) -> Vec<Problem> {
    let mut problems: Vec<Problem> = Vec::new();
    let mut push = |mut problem: Problem| {
        // A progress bar the line was printed over can be left after it
        // (cargo through Windows' console host): cut at a wide gap.
        if let Some(gap) = problem.message.find("   ") {
            problem.message.truncate(gap);
        }
        if problem.line > 0 && !problems.contains(&problem) {
            problems.push(problem);
        }
    };
    let lines: Vec<std::borrow::Cow<str>> = lines.iter().map(|line| strip_ansi(line)).collect();
    let mut eslint_file: Option<String> = None;
    let mut traceback: Option<(String, usize)> = None;
    let mut in_traceback = false;
    for (ix, line) in lines.iter().enumerate() {
        let line = line.trim_end();
        // Python: remember the innermost frame, report at the exception.
        if line.starts_with("Traceback (most recent call last)") {
            in_traceback = true;
            traceback = None;
            continue;
        }
        if in_traceback {
            if let Some(captures) = PY_FRAME.captures(line) {
                traceback = Some((captures["path"].to_owned(), number(&captures, "line")));
                continue;
            }
            if line.starts_with(' ') || line.is_empty() {
                continue;
            }
            in_traceback = false;
            if let (Some((path, line_number)), Some(captures)) =
                (traceback.take(), PY_EXCEPTION.captures(line))
            {
                push(Problem {
                    path,
                    line: line_number,
                    column: 0,
                    severity: Severity::Error,
                    message: captures["msg"].to_owned(),
                });
                continue;
            }
        }
        if let Some(captures) = RUST_HEADER.captures(line) {
            // The location follows within a few lines.
            let location = lines
                .iter()
                .skip(ix + 1)
                .take(3)
                .take_while(|next| !RUST_HEADER.is_match(next))
                .find_map(|next| RUST_LOCATION.captures(next));
            if let Some(location) = location {
                push(Problem {
                    path: location["path"].to_owned(),
                    line: number(&location, "line"),
                    column: number(&location, "col"),
                    severity: severity(captures.name("sev").map(|m| m.as_str())),
                    message: captures["msg"].to_owned(),
                });
            }
            continue;
        }
        if let Some(captures) = RUST_PANIC.captures(line) {
            let message = captures
                .name("msg")
                .map(|m| m.as_str().to_owned())
                .or_else(|| lines.get(ix + 1).map(|next| next.trim().to_owned()))
                .unwrap_or_else(|| "panicked".to_owned());
            push(Problem {
                path: captures["path"].to_owned(),
                line: number(&captures, "line"),
                column: number(&captures, "col"),
                severity: Severity::Error,
                message,
            });
            continue;
        }
        if RUST_LOCATION.is_match(line) {
            continue;
        }
        if let Some(captures) = PAREN.captures(line).or_else(|| DASH.captures(line)) {
            push(Problem {
                path: captures["path"].to_owned(),
                line: number(&captures, "line"),
                column: number(&captures, "col"),
                severity: severity(captures.name("sev").map(|m| m.as_str())),
                message: captures["msg"].trim().to_owned(),
            });
            continue;
        }
        if let Some(captures) = ESLINT_FILE.captures(line) {
            eslint_file = Some(captures["path"].to_owned());
            continue;
        }
        if let (Some(file), Some(captures)) = (&eslint_file, ESLINT_ROW.captures(line)) {
            push(Problem {
                path: file.clone(),
                line: number(&captures, "line"),
                column: number(&captures, "col"),
                severity: severity(captures.name("sev").map(|m| m.as_str())),
                message: captures["msg"].trim().to_owned(),
            });
            continue;
        }
        if line.trim().is_empty() {
            eslint_file = None;
        }
        if let Some(captures) = COLON.captures(line) {
            let sev = captures.name("sev").map(|m| m.as_str());
            let message = captures["msg"].trim();
            // `note:` lines belong to the error above them.
            if sev == Some("note") || message.starts_with("//") {
                continue;
            }
            if sev.is_none() && captures.name("col").is_none() && !looks_like_error(message) {
                continue;
            }
            push(Problem {
                path: captures["path"].to_owned(),
                line: number(&captures, "line"),
                column: number(&captures, "col"),
                severity: severity(sev),
                message: message.to_owned(),
            });
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Vec<Problem> {
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        parse_problems(&lines)
    }

    fn problem(
        path: &str,
        line: usize,
        column: usize,
        severity: Severity,
        message: &str,
    ) -> Problem {
        Problem {
            path: path.into(),
            line,
            column,
            severity,
            message: message.into(),
        }
    }

    #[test]
    fn cargo_errors_and_warnings() {
        let output = "   Compiling app v0.1.0\n\
error[E0425]: cannot find value `x` in this scope          ] 0/1: app(bin)\n  --> src/main.rs:3:5\n   |\n\
warning: unused variable: `y`\n --> src\\lib.rs:10:9\n\
warning: `app` (bin \"app\") generated 1 warning\n\
error: could not compile `app` (bin \"app\") due to 1 previous error\n";
        assert_eq!(
            parse(output),
            vec![
                problem(
                    "src/main.rs",
                    3,
                    5,
                    Severity::Error,
                    "cannot find value `x` in this scope"
                ),
                problem(
                    "src\\lib.rs",
                    10,
                    9,
                    Severity::Warning,
                    "unused variable: `y`"
                ),
            ]
        );
    }

    #[test]
    fn rust_test_panics() {
        let output =
            "thread 'tests::adds' panicked at src/lib.rs:12:9:\nassertion `left == right` failed\n";
        assert_eq!(
            parse(output),
            vec![problem(
                "src/lib.rs",
                12,
                9,
                Severity::Error,
                "assertion `left == right` failed"
            )]
        );
    }

    #[test]
    fn typescript_both_styles() {
        let output = "src/app.ts(4,7): error TS2322: Type 'string' is not assignable to type 'number'.\n\
src/b.tsx:10:3 - error TS2304: Cannot find name 'foo'.\n";
        assert_eq!(
            parse(output),
            vec![
                problem(
                    "src/app.ts",
                    4,
                    7,
                    Severity::Error,
                    "Type 'string' is not assignable to type 'number'."
                ),
                problem(
                    "src/b.tsx",
                    10,
                    3,
                    Severity::Error,
                    "Cannot find name 'foo'."
                ),
            ]
        );
    }

    #[test]
    fn gcc_go_and_python_linters() {
        let output = "main.c:5:10: error: expected ';' before '}' token\n\
./main.go:8:2: undefined: fmt.Printn\n\
app/models.py:14: error: Incompatible return value type\n\
app/views.py:3:1: F401 'os' imported but unused\n\
C:\\proj\\src\\x.cpp:9:1: warning: unused\n";
        assert_eq!(
            parse(output),
            vec![
                problem(
                    "main.c",
                    5,
                    10,
                    Severity::Error,
                    "expected ';' before '}' token"
                ),
                problem("./main.go", 8, 2, Severity::Error, "undefined: fmt.Printn"),
                problem(
                    "app/models.py",
                    14,
                    0,
                    Severity::Error,
                    "Incompatible return value type"
                ),
                problem(
                    "app/views.py",
                    3,
                    1,
                    Severity::Error,
                    "'os' imported but unused"
                ),
                problem("C:\\proj\\src\\x.cpp", 9, 1, Severity::Warning, "unused"),
            ]
        );
    }

    #[test]
    fn eslint_stylish() {
        let output = "\nC:\\proj\\src\\index.js\n  3:7   error    'x' is assigned a value but never used  no-unused-vars\n  9:1   warning  Unexpected console statement            no-console\n\n✖ 2 problems\n";
        assert_eq!(
            parse(output),
            vec![
                problem(
                    "C:\\proj\\src\\index.js",
                    3,
                    7,
                    Severity::Error,
                    "'x' is assigned a value but never used"
                ),
                problem(
                    "C:\\proj\\src\\index.js",
                    9,
                    1,
                    Severity::Warning,
                    "Unexpected console statement"
                ),
            ]
        );
    }

    #[test]
    fn python_traceback() {
        let output = "Traceback (most recent call last):\n  File \"app/main.py\", line 10, in <module>\n    main()\n  File \"app/util.py\", line 4, in main\n    raise ValueError(\"bad\")\nValueError: bad\n";
        assert_eq!(
            parse(output),
            vec![problem(
                "app/util.py",
                4,
                0,
                Severity::Error,
                "ValueError: bad"
            )]
        );
    }

    #[test]
    fn ignores_urls_and_logs() {
        let output = "  Local:   http://localhost:5173/\nINFO server.py:20: listening\nsee docs/guide.md for details\n";
        assert_eq!(parse(output), Vec::new());
    }
}
