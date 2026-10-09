//! `git blame --porcelain` parsing.

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameCommit {
    pub oid: String,
    pub author: String,
    pub author_mail: String,
    /// Unix seconds.
    pub author_time: i64,
    pub summary: String,
}

impl BlameCommit {
    pub fn short_oid(&self) -> &str {
        &self.oid[..self.oid.len().min(7)]
    }
}

/// Who last changed each line of a file.
#[derive(Debug, Default)]
pub struct Blame {
    pub commits: Vec<BlameCommit>,
    /// For each line (zero-based), an index into `commits`.
    pub lines: Vec<u32>,
}

impl Blame {
    pub fn commit_for_line(&self, line: usize) -> Option<&BlameCommit> {
        let ix = *self.lines.get(line)?;
        self.commits.get(ix as usize)
    }
}

pub(crate) fn parse(output: &str) -> Blame {
    let mut blame = Blame::default();
    let mut index_of: HashMap<String, u32> = HashMap::new();
    let mut lines = output.lines();
    while let Some(header) = lines.next() {
        // "<oid> <orig line> <final line> [<group size>]"
        let mut parts = header.split(' ');
        let (Some(oid), Some(_), Some(final_line)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Ok(final_line) = final_line.parse::<usize>() else {
            continue;
        };
        let known = index_of.get(oid).copied();
        let mut commit = known.is_none().then(|| BlameCommit {
            oid: oid.to_owned(),
            author: String::new(),
            author_mail: String::new(),
            author_time: 0,
            summary: String::new(),
        });
        // Commit details (first time only), then the line itself after a tab.
        for line in lines.by_ref() {
            if line.starts_with('\t') {
                break;
            }
            let Some(commit) = &mut commit else {
                continue;
            };
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            match key {
                "author" => commit.author = value.to_owned(),
                "author-mail" => {
                    commit.author_mail = value.trim_matches(['<', '>']).to_owned();
                }
                "author-time" => commit.author_time = value.parse().unwrap_or(0),
                "summary" => commit.summary = value.to_owned(),
                _ => {}
            }
        }
        let ix = match (known, commit) {
            (Some(ix), _) => ix,
            (None, Some(commit)) => {
                let ix = blame.commits.len() as u32;
                index_of.insert(commit.oid.clone(), ix);
                blame.commits.push(commit);
                ix
            }
            (None, None) => unreachable!("commit is built when unknown"),
        };
        let line = final_line.saturating_sub(1);
        if blame.lines.len() <= line {
            blame.lines.resize(line + 1, ix);
        }
        blame.lines[line] = ix;
    }
    blame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_blame() {
        let output = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1 1 2
author Ada
author-mail <ada@example.com>
author-time 1700000000
author-tz +0000
summary First commit
filename a.rs
\tfn main() {
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 2 2
\t    run();
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb 3 3 1
author Grace
author-mail <grace@example.com>
author-time 1710000000
summary Close the brace
previous aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa a.rs
filename a.rs
\t}
";
        let blame = parse(output);
        assert_eq!(blame.commits.len(), 2);
        assert_eq!(blame.lines, [0, 0, 1]);
        let grace = blame.commit_for_line(2).unwrap();
        assert_eq!(grace.author, "Grace");
        assert_eq!(grace.author_mail, "grace@example.com");
        assert_eq!(grace.summary, "Close the brace");
        assert_eq!(grace.short_oid(), "bbbbbbb");
        assert_eq!(blame.commit_for_line(0).unwrap().author_time, 1_700_000_000);
    }
}
