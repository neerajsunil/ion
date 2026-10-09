//! The OpenSSH client config (`~/.ssh/config`): host aliases, users
//! and ports, for the connection dialog (`ssh` itself does the real work).

use std::path::{Path, PathBuf};

/// Where and how to connect, after applying the SSH config. Only for
/// display: `ssh` resolves the config itself when connecting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// What the user typed or picked (an alias or a host name).
    pub alias: String,
    pub host_name: String,
    pub port: u16,
    pub user: String,
}

impl Target {
    /// `user@host:port`, omitting the default port.
    pub fn label(&self) -> String {
        let host = if self.alias.is_empty() {
            &self.host_name
        } else {
            &self.alias
        };
        if self.port == 22 {
            format!("{}@{host}", self.user)
        } else {
            format!("{}@{host}:{}", self.user, self.port)
        }
    }
}

/// The keywords in effect for a host.
#[derive(Default)]
struct Values {
    host_name: Option<String>,
    port: Option<u16>,
    user: Option<String>,
}

/// A parsed config file (with Includes expanded).
#[derive(Default, Debug, Clone)]
pub struct Config {
    /// (patterns, keyword, value) in file order. Lines before any Host
    /// apply to every host.
    lines: Vec<(Vec<String>, String, String)>,
    /// Hosts named without wildcards, for the connection dialog.
    hosts: Vec<String>,
}

impl Config {
    /// Reads `~/.ssh/config`; a missing or unreadable file is an empty config.
    pub fn load() -> Self {
        let mut config = Self::default();
        if let Some(dir) = ssh_dir() {
            config.read_file(&dir.join("config"), &dir, &[], 0);
        }
        config
    }

    pub fn parse(text: &str) -> Self {
        let mut config = Self::default();
        config.read_text(text, Path::new("."), &[], 0);
        config
    }

    /// Host aliases from `Host` lines, without wildcard patterns.
    pub fn hosts(&self) -> &[String] {
        &self.hosts
    }

    fn read_file(&mut self, path: &Path, ssh_dir: &Path, block: &[String], depth: usize) {
        if let Ok(text) = std::fs::read_to_string(path) {
            self.read_text(&text, ssh_dir, block, depth);
        }
    }

    fn read_text(&mut self, text: &str, ssh_dir: &Path, block: &[String], depth: usize) {
        let mut patterns: Vec<String> = block.to_vec();
        for line in text.lines() {
            let Some((keyword, value)) = split_line(line) else {
                continue;
            };
            match keyword.as_str() {
                "host" => {
                    patterns = split_args(&value);
                    for pattern in &patterns {
                        if !pattern.contains(['*', '?', '!'])
                            && !self.hosts.iter().any(|host| host == pattern)
                        {
                            self.hosts.push(pattern.clone());
                        }
                    }
                }
                // Match conditions aren't evaluated: their lines never apply.
                "match" => patterns = vec!["!*".into()],
                "include" if depth < 8 => {
                    for file in split_args(&value) {
                        for path in expand_include(&file, ssh_dir) {
                            self.read_file(&path, ssh_dir, &patterns, depth + 1);
                        }
                    }
                }
                _ => self.lines.push((patterns.clone(), keyword, value)),
            }
        }
    }

    fn values(&self, host: &str) -> Values {
        let mut values = Values::default();
        for (patterns, keyword, value) in &self.lines {
            if !patterns.is_empty() && !host_matches(host, patterns) {
                continue;
            }
            match keyword.as_str() {
                "hostname" if values.host_name.is_none() => values.host_name = Some(value.clone()),
                "user" if values.user.is_none() => values.user = Some(value.clone()),
                "port" if values.port.is_none() => values.port = value.parse().ok(),
                _ => {}
            }
        }
        values
    }

    /// Resolves what the user typed: `host`, `user@host`, `host:port` or an
    /// alias. Explicit values win over the config.
    pub fn resolve(
        &self,
        host: &str,
        user: Option<&str>,
        port: Option<u16>,
    ) -> Result<Target, String> {
        let (spec_user, host, spec_port) = split_destination(host)?;
        let values = self.values(&host);
        let user = user
            .map(str::to_owned)
            .or(spec_user)
            .or(values.user)
            .unwrap_or_else(local_user);
        let host_name = values
            .host_name
            .map(|name| name.replace("%h", &host).replace("%%", "%"))
            .unwrap_or_else(|| host.clone());
        Ok(Target {
            alias: host,
            host_name,
            port: port.or(spec_port).or(values.port).unwrap_or(22),
            user,
        })
    }
}

/// `[user@]host[:port]`, also `[user@][ipv6]:port`.
pub(crate) fn split_destination(
    spec: &str,
) -> Result<(Option<String>, String, Option<u16>), String> {
    let spec = spec.trim();
    let (user, rest) = match spec.rsplit_once('@') {
        Some((user, rest)) if !user.is_empty() => (Some(user.to_owned()), rest),
        _ => (None, spec),
    };
    let (host, port) = if let Some(inner) = rest.strip_prefix('[') {
        let (host, after) = inner
            .split_once(']')
            .ok_or_else(|| format!("Invalid host: {spec}"))?;
        (host, after.strip_prefix(':'))
    } else if rest.matches(':').count() == 1 {
        let (host, port) = rest.split_once(':').unwrap_or((rest, ""));
        (host, Some(port))
    } else {
        (rest, None)
    };
    let port = match port {
        Some(port) => Some(
            port.parse::<u16>()
                .ok()
                .filter(|port| *port > 0)
                .ok_or_else(|| format!("Invalid port in {spec}"))?,
        ),
        None => None,
    };
    if host.is_empty()
        || host.starts_with('-')
        || host.contains(|c: char| c.is_whitespace() || matches!(c, '\0' | '/' | '\\' | '@'))
    {
        return Err(format!("Invalid host: {spec}"));
    }
    Ok((user, host.to_owned(), port))
}

fn split_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let split = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let keyword = line[..split].to_ascii_lowercase();
    let value = line[split..]
        .trim_start_matches(|c: char| c.is_whitespace())
        .trim_start_matches('=')
        .trim();
    Some((keyword, value.to_owned()))
}

/// Whitespace-separated arguments, honoring double quotes.
fn split_args(value: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in value.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

fn host_matches(host: &str, patterns: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    let mut matched = false;
    for pattern in patterns {
        let pattern = pattern.to_ascii_lowercase();
        match pattern.strip_prefix('!') {
            Some(negated) if glob(negated, &host) => return false,
            Some(_) => {}
            None => matched |= glob(&pattern, &host),
        }
    }
    matched
}

/// `*` and `?` wildcards.
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

fn expand_include(file: &str, ssh_dir: &Path) -> Vec<PathBuf> {
    let path = expand_tokens(file, "", "");
    let path = if path.is_absolute() {
        path
    } else {
        ssh_dir.join(path)
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| glob(&name, &entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found
}

fn expand_tokens(value: &str, host: &str, user: &str) -> PathBuf {
    let home = home_dir()
        .map(|home| home.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut value = value.to_owned();
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        value = format!("{home}/{rest}");
    } else if value == "~" {
        value = home.clone();
    }
    let value = value
        .replace("%d", &home)
        .replace("%h", host)
        .replace("%r", user)
        .replace("%u", &local_user())
        .replace("%%", "%");
    PathBuf::from(value)
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// `~/.ssh`, or `ION_SSH_DIR` if set (portable installs, tests).
pub(crate) fn ssh_dir() -> Option<PathBuf> {
    std::env::var_os("ION_SSH_DIR")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".ssh")))
}

fn local_user() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "
# Global
ServerAliveInterval 30

Host prod
    HostName prod.internal.example.com
    User deploy
    Port 2200
    ProxyJump bastion

Host bastion
  HostName=bastion.example.com
  User jump

Host *.dev !secret.dev
  User developer

Match host foo
  User nobody

Host *
  User fallback
";

    #[test]
    fn resolves_aliases_and_first_value_wins() {
        let config = Config::parse(CONFIG);
        let target = config.resolve("prod", None, None).unwrap();
        assert_eq!(target.host_name, "prod.internal.example.com");
        assert_eq!(target.user, "deploy");
        assert_eq!(target.port, 2200);
        assert_eq!(target.label(), "deploy@prod:2200");
        assert_eq!(config.hosts(), ["prod", "bastion"]);
    }

    #[test]
    fn patterns_negation_and_match_blocks() {
        let config = Config::parse(CONFIG);
        assert_eq!(
            config.resolve("box.dev", None, None).unwrap().user,
            "developer"
        );
        assert_eq!(
            config.resolve("secret.dev", None, None).unwrap().user,
            "fallback"
        );
        assert_eq!(config.resolve("foo", None, None).unwrap().user, "fallback");
    }

    #[test]
    fn explicit_values_and_destination_syntax() {
        let config = Config::parse(CONFIG);
        let target = config.resolve("ops@prod:2022", None, None).unwrap();
        assert_eq!((target.user.as_str(), target.port), ("ops", 2022));
        let target = config.resolve("prod", Some("me"), Some(22)).unwrap();
        assert_eq!((target.user.as_str(), target.port), ("me", 22));
        let target = config.resolve("[::1]:2200", None, None).unwrap();
        assert_eq!((target.host_name.as_str(), target.port), ("::1", 2200));
        assert!(config.resolve("-oProxyCommand=x", None, None).is_err());
        assert!(config.resolve("host:0", None, None).is_err());
    }

    #[test]
    fn wildcards() {
        assert!(glob("*.example.com", "a.example.com"));
        assert!(glob("web-?", "web-1"));
        assert!(!glob("web-?", "web-10"));
        assert!(glob("*", ""));
    }
}
