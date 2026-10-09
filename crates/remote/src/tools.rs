//! What a server offers for new terminals: its shells and the agent CLIs
//! installed there, found once per connection.

use std::io;

use crate::{Connection, shell_quote};

/// Where agent CLIs usually live when the login shell's `PATH` misses them
/// (a profile that only sets it up for interactive shells, say).
const EXTRA_DIRS: &str = "$HOME/.local/bin:$HOME/.bun/bin:$HOME/.npm-global/bin:\
                          $HOME/.volta/bin:$HOME/.cargo/bin:/usr/local/bin:/opt/homebrew/bin";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteTools {
    /// Shell paths, the login shell first.
    pub shells: Vec<String>,
    /// Those of the asked-for agent commands that the server can run.
    pub agents: Vec<String>,
}

impl Connection {
    /// The server's shells, and which of `agents` (command names such as
    /// `claude`) it has installed.
    pub fn tools(&self, agents: &[&str]) -> io::Result<RemoteTools> {
        let output = self.execute(&script(agents), None)?;
        Ok(parse(&String::from_utf8_lossy(&output.stdout)))
    }
}

/// A POSIX `sh` script printing `shell <path>` and `agent <name>` lines.
/// Agents are looked up with the `PATH` an interactive login shell sets up,
/// as typing their name in a terminal would.
fn script(agents: &[&str]) -> String {
    let agents: Vec<String> = agents.iter().map(|agent| shell_quote(agent)).collect();
    format!(
        r#"login="${{SHELL:-/bin/sh}}"
echo "shell $login"
if [ -r /etc/shells ]; then
  while IFS= read -r s; do
    case "$s" in /*) [ "$s" != "$login" ] && [ -x "$s" ] && echo "shell $s";; esac
  done < /etc/shells
fi
p=$("$login" -lic 'printf "\nion-path:%s\n" "$PATH"' </dev/null 2>/dev/null | sed -n 's/^ion-path://p' | tail -n 1)
[ -n "$p" ] && PATH="$p"
PATH="$PATH:{EXTRA_DIRS}"
for a in {agents}; do command -v "$a" >/dev/null 2>&1 && echo "agent $a"; done
exit 0"#,
        agents = agents.join(" "),
    )
}

/// Entries in `/etc/shells` that aren't shells to open a terminal with.
const NOT_SHELLS: [&str; 6] = ["tmux", "screen", "rbash", "nologin", "false", "git-shell"];

fn parse(output: &str) -> RemoteTools {
    let name = |path: &str| path.rsplit('/').next().unwrap_or(path).to_owned();
    let mut tools = RemoteTools::default();
    for line in output.lines() {
        match line.split_once(' ') {
            // `/bin/bash` and `/usr/bin/bash` are one shell; keep the first.
            Some(("shell", path))
                if !NOT_SHELLS.contains(&name(path).as_str())
                    && !tools.shells.iter().any(|known| name(known) == name(path)) =>
            {
                tools.shells.push(path.to_owned());
            }
            Some(("agent", name)) => tools.agents.push(name.to_owned()),
            _ => {}
        }
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shells_and_agents() {
        let tools = parse(
            "motd noise\nshell /bin/zsh\nshell /bin/bash\nshell /usr/bin/zsh\n\
             shell /usr/bin/tmux\nagent codex\n",
        );
        assert_eq!(tools.shells, ["/bin/zsh", "/bin/bash"]);
        assert_eq!(tools.agents, ["codex"]);
    }

    #[cfg(unix)]
    #[test]
    fn finds_tools_with_a_real_shell() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("fake-agent");
        std::fs::write(&agent, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&agent, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let script = script(&["fake-agent", "missing-agent"]);
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .env("SHELL", "/bin/sh")
            .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
            .output()
            .unwrap();
        let tools = parse(&String::from_utf8_lossy(&output.stdout));
        assert_eq!(tools.shells.first().map(String::as_str), Some("/bin/sh"));
        assert_eq!(tools.agents, ["fake-agent"]);
    }
}
