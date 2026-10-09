//! Commit history.

use crate::patch::{self, FileDiff};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitSummary {
    pub oid: String,
    pub short_oid: String,
    pub author: String,
    /// Unix seconds.
    pub time: i64,
    pub subject: String,
    /// Branch and tag names pointing here, e.g. "HEAD -> main, origin/main".
    pub refs: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitDetails {
    pub oid: String,
    pub author: String,
    pub email: String,
    /// Author date in local time, "YYYY-MM-DD HH:MM".
    pub date: String,
    pub message: String,
    pub files: Vec<FileDiff>,
}

const FIELD: char = '\x1f';
const RECORD: char = '\x1e';

pub(crate) const LOG_FORMAT: &str = "--format=%H%x1f%h%x1f%an%x1f%at%x1f%s%x1f%D%x1e";
pub(crate) const SHOW_FORMAT: &str = "--format=%H%x1f%an%x1f%ae%x1f%ad%x1f%B%x1e";

pub(crate) fn parse_log(output: &str) -> Vec<CommitSummary> {
    output
        .split(RECORD)
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').split(FIELD);
            Some(CommitSummary {
                oid: fields.next().filter(|oid| !oid.is_empty())?.to_owned(),
                short_oid: fields.next()?.to_owned(),
                author: fields.next()?.to_owned(),
                time: fields.next()?.parse().unwrap_or(0),
                subject: fields.next()?.to_owned(),
                refs: fields.next().unwrap_or("").trim().to_owned(),
            })
        })
        .collect()
}

pub(crate) fn parse_show(output: &str) -> Option<CommitDetails> {
    let (header, diff) = output.split_once(RECORD)?;
    let mut fields = header.splitn(5, FIELD);
    Some(CommitDetails {
        oid: fields.next()?.to_owned(),
        author: fields.next()?.to_owned(),
        email: fields.next()?.to_owned(),
        date: fields.next()?.to_owned(),
        message: fields.next()?.trim_end().to_owned(),
        files: patch::parse(diff),
    })
}

/// "3 days ago" style age of a timestamp.
pub fn relative_time(then: i64, now: i64) -> String {
    let seconds = (now - then).max(0);
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    let plural = |n: i64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    match () {
        _ if minutes < 1 => "just now".into(),
        _ if hours < 1 => plural(minutes, "minute"),
        _ if days < 1 => plural(hours, "hour"),
        _ if days == 1 => "yesterday".into(),
        _ if days < 14 => plural(days, "day"),
        _ if days < 60 => plural(days / 7, "week"),
        _ if days < 365 => plural(days / 30, "month"),
        _ => plural(days / 365, "year"),
    }
}

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_log_records() {
        let output = "aaa\x1fa1\x1fAda\x1f100\x1fFirst\x1fHEAD -> main\x1e\n\
                      bbb\x1fb2\x1fGrace\x1f50\x1fSecond\x1f\x1e\n";
        let commits = parse_log(output);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].refs, "HEAD -> main");
        assert_eq!(commits[1].author, "Grace");
        assert_eq!(commits[1].time, 50);
    }

    #[test]
    fn formats_relative_times() {
        let day = 86_400;
        assert_eq!(relative_time(0, 30), "just now");
        assert_eq!(relative_time(0, 60), "1 minute ago");
        assert_eq!(relative_time(0, 3 * 3600), "3 hours ago");
        assert_eq!(relative_time(0, day + 5), "yesterday");
        assert_eq!(relative_time(0, 20 * day), "2 weeks ago");
        assert_eq!(relative_time(0, 800 * day), "2 years ago");
    }
}
