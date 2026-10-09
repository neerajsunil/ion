//! POSIX path completion independent of the client's OS path rules.
use project::DirEntry;
use std::path::{Path, PathBuf};

pub struct Query {
    pub directory: PathBuf,
    pub prefix: String,
    display_parent: String,
}
impl Query {
    pub fn new(input: &str, home: &Path) -> Self {
        let input = input.replace('\\', "/");
        let input = if input == "~" { "~/".to_owned() } else { input };
        let (parent, prefix) = match input.rfind('/') {
            Some(index) => (&input[..=index], &input[index + 1..]),
            None => ("", input.as_str()),
        };
        let directory = if parent.starts_with('/') {
            PathBuf::from(parent)
        } else if let Some(relative) = parent.strip_prefix("~/") {
            project::join_path(home, relative)
        } else {
            project::join_path(home, parent)
        };
        Self {
            directory,
            prefix: prefix.to_owned(),
            display_parent: parent.to_owned(),
        }
    }
    pub fn suggestions(&self, entries: &[DirEntry]) -> Vec<String> {
        let mut names: Vec<_> = entries
            .iter()
            .filter(|entry| entry.is_dir && entry.name.starts_with(&self.prefix))
            .map(|entry| format!("{}{}/", self.display_parent, entry.name))
            .collect();
        names.sort();
        names.truncate(100);
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_absolute_home_and_relative_prefixes_without_client_path_rules() {
        let home = Path::new("/home/dev");
        for (input, directory, prefix) in [
            ("/srv/pro", "/srv/", "pro"),
            ("~/my pro", "/home/dev/", "my pro"),
            ("src/co", "/home/dev/src/", "co"),
            ("", "/home/dev/", ""),
            ("~", "/home/dev/", ""),
        ] {
            let query = Query::new(input, home);
            assert_eq!(query.directory, PathBuf::from(directory));
            assert_eq!(query.prefix, prefix);
        }
    }
    #[test]
    fn completes_only_matching_directories_and_preserves_spaces_and_unicode() {
        let entries: Vec<_> = [
            ("my project é", true),
            ("my project.txt", false),
            ("Other", true),
        ]
        .into_iter()
        .map(|(name, is_dir)| DirEntry {
            path: PathBuf::from(name),
            name: name.into(),
            is_dir,
        })
        .collect();
        assert_eq!(
            Query::new("~/my", Path::new("/home/dev")).suggestions(&entries),
            vec!["~/my project é/"]
        );
    }
}
