//! Summaries somebody decided to keep.
//!
//! Everything else under the data folder is derived: delete it and the next launch
//! rebuilds it from the event log. The library is the exception. A summary is written
//! by a model from a day's episodes, and once the retention window has taken those
//! events away there is no rebuilding it. Saving one is the difference between a
//! record of a day and a record of having read about it.
//!
//! One Markdown file per document, with the little that has to be machine-readable in
//! a front-matter block at the top. The file is the document — exporting a copy is
//! reading it and writing it somewhere else, with no conversion in between, and a
//! document opened in any editor is the same document the application shows.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::paths;

/// The fence that opens and closes the front matter.
const FENCE: &str = "---";

/// One saved document, described without reading its body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryEntry {
    /// The file's stem. Also what a caller passes back to read, export or delete it.
    pub id: String,
    pub title: String,
    /// The local day the document describes, `YYYY-MM-DD`.
    pub date: String,
    pub saved_at: String,
    pub bytes: u64,
}

/// The Markdown files under one directory.
pub struct LibraryStore {
    dir: PathBuf,
}

impl LibraryStore {
    /// Open the real library under `%APPDATA%`.
    pub fn open() -> Result<Self> {
        Self::in_dir(paths::library_dir()?)
    }

    pub fn in_dir(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        paths::ensure_dir(&dir)?;
        Ok(LibraryStore { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Write a new document and return how it will be listed.
    ///
    /// Saving the same day twice keeps both. The second save is a second opinion — a
    /// rewritten summary, or the same day summarized by a different model — and
    /// silently replacing the first would throw away the thing the user asked to keep.
    pub fn save(&self, date: &str, title: &str, body: &str) -> Result<LibraryEntry> {
        let saved_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let id = self.free_id(date);
        let document = format!(
            "{FENCE}\ntitle: {}\ndate: {date}\nsavedAt: {saved_at}\n{FENCE}\n\n{}",
            one_line(title),
            body.trim_end()
        );

        let path = self.path(&id)?;
        crate::paths::write_atomically(&path, &document)?;

        Ok(LibraryEntry {
            id,
            title: one_line(title),
            date: date.to_owned(),
            saved_at,
            bytes: document.len() as u64,
        })
    }

    /// Every document, newest first.
    ///
    /// A file whose front matter cannot be read is still listed, under its own name.
    /// Hiding it would be the one failure mode this store must not have: a document
    /// somebody saved that the application will not show them.
    pub fn list(&self) -> Vec<LibraryEntry> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };

        let mut documents: Vec<LibraryEntry> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let id = name.strip_suffix(".md")?.to_owned();
                let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let text = std::fs::read_to_string(entry.path()).ok()?;
                Some(entry_from(&id, bytes, &text))
            })
            .collect();

        documents.sort_by(|a, b| b.saved_at.cmp(&a.saved_at).then_with(|| b.id.cmp(&a.id)));
        documents
    }

    /// The Markdown of one document, front matter and all.
    pub fn read(&self, id: &str) -> Result<String> {
        let path = self.path(id)?;
        std::fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))
    }

    /// The body of one document, with the front matter taken off.
    pub fn body(&self, id: &str) -> Result<String> {
        Ok(split_front_matter(&self.read(id)?).1.to_owned())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let path = self.path(id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("could not delete {}", path.display()))
            }
        }
    }

    /// Where a document lives, refusing anything that is not a plain name.
    ///
    /// Identifiers come back from the interface, which is where a `..` would arrive
    /// from if one ever did. This is the boundary, so this is where it is checked.
    pub fn path(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            bail!("{id:?} is not a document identifier");
        }
        Ok(self.dir.join(format!("{id}.md")))
    }

    /// An identifier for a new document about a day: the date, then the date and a
    /// number for each further save of it.
    fn free_id(&self, date: &str) -> String {
        let stem: String = date
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let stem = if stem.is_empty() {
            "summary".to_owned()
        } else {
            stem
        };

        if !self.dir.join(format!("{stem}.md")).exists() {
            return stem;
        }
        for n in 2.. {
            let candidate = format!("{stem}-{n}");
            if !self.dir.join(format!("{candidate}.md")).exists() {
                return candidate;
            }
        }
        unreachable!("the loop returns")
    }
}

/// Read one document's listing from its text, falling back to the file's own name.
fn entry_from(id: &str, bytes: u64, text: &str) -> LibraryEntry {
    let (front, _) = split_front_matter(text);
    let field = |name: &str| {
        front.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == name).then(|| value.trim().to_owned())
        })
    };

    LibraryEntry {
        id: id.to_owned(),
        title: field("title")
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| id.to_owned()),
        date: field("date").unwrap_or_default(),
        saved_at: field("savedAt").unwrap_or_default(),
        bytes,
    }
}

/// Split a document into its front matter and its body.
///
/// A document with no front matter is all body, which is what a file somebody dropped
/// into the folder by hand will be.
fn split_front_matter(text: &str) -> (&str, &str) {
    let Some(rest) = text.strip_prefix(FENCE) else {
        return ("", text);
    };
    let rest = rest.trim_start_matches(['\r', '\n']);
    match rest.split_once(&format!("\n{FENCE}")) {
        Some((front, body)) => (front, body.trim_start_matches(['\r', '\n', '-'])),
        None => ("", text),
    }
}

/// Collapse a title to one line, so it cannot break the front matter it sits in.
fn one_line(title: &str) -> String {
    let collapsed = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "Untitled".to_owned()
    } else {
        collapsed
    }
}

/// Write a document to a path outside the data folder.
///
/// Export is a copy, not a conversion: what lands is the file the application holds.
pub fn export(store: &LibraryStore, id: &str, destination: &Path) -> Result<()> {
    let text = store.read(id)?;
    if destination.as_os_str().is_empty() {
        return Err(anyhow!("no destination was chosen"));
    }
    std::fs::write(destination, text)
        .with_context(|| format!("could not write {}", destination.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, LibraryStore) {
        let temp = tempfile::tempdir().unwrap();
        let store = LibraryStore::in_dir(temp.path()).unwrap();
        (temp, store)
    }

    #[test]
    fn a_saved_document_comes_back_with_its_title_and_its_day() {
        let (_temp, store) = store();
        let saved = store
            .save(
                "2026-08-22",
                "Saturday 22 August 2026",
                "# A day\n\nIt went well.",
            )
            .unwrap();

        assert_eq!(saved.id, "2026-08-22");
        assert_eq!(saved.title, "Saturday 22 August 2026");
        assert_eq!(store.list(), vec![saved.clone()]);
        assert!(store.read(&saved.id).unwrap().contains("It went well."));
        assert_eq!(store.body(&saved.id).unwrap(), "# A day\n\nIt went well.");
    }

    #[test]
    fn saving_a_day_twice_keeps_both() {
        let (_temp, store) = store();
        let first = store.save("2026-08-22", "First take", "One.").unwrap();
        let second = store.save("2026-08-22", "Second take", "Two.").unwrap();

        assert_eq!(first.id, "2026-08-22");
        assert_eq!(second.id, "2026-08-22-2");
        assert_eq!(store.list().len(), 2);
        assert_eq!(store.body(&first.id).unwrap(), "One.");
        assert_eq!(store.body(&second.id).unwrap(), "Two.");
    }

    #[test]
    fn documents_are_listed_newest_first() {
        let (_temp, store) = store();
        store.save("2026-08-20", "Oldest", "a").unwrap();
        store.save("2026-08-21", "Middle", "b").unwrap();
        store.save("2026-08-22", "Newest", "c").unwrap();

        let titles: Vec<String> = store.list().into_iter().map(|one| one.title).collect();
        assert_eq!(titles, vec!["Newest", "Middle", "Oldest"]);
    }

    #[test]
    fn a_deleted_document_is_gone_and_deleting_it_again_is_not_an_error() {
        let (_temp, store) = store();
        let saved = store.save("2026-08-22", "A day", "text").unwrap();

        store.delete(&saved.id).unwrap();
        assert!(store.list().is_empty());
        store.delete(&saved.id).unwrap();
    }

    #[test]
    fn an_identifier_cannot_climb_out_of_the_library() {
        let (_temp, store) = store();
        for attempt in [
            "..",
            "../config",
            r"..\config",
            "a/b",
            "",
            "with space",
            "a.md",
        ] {
            assert!(
                store.path(attempt).is_err(),
                "{attempt:?} must be refused as an identifier"
            );
            assert!(store.read(attempt).is_err());
            assert!(store.delete(attempt).is_err());
        }
    }

    #[test]
    fn a_multi_line_title_cannot_break_the_front_matter() {
        let (_temp, store) = store();
        let saved = store
            .save("2026-08-22", "A day\n---\ndate: 1999-01-01", "body")
            .unwrap();

        assert_eq!(store.list()[0].date, "2026-08-22");
        assert_eq!(saved.title, "A day --- date: 1999-01-01");
    }

    #[test]
    fn a_file_with_no_front_matter_is_still_listed() {
        let (temp, store) = store();
        std::fs::write(temp.path().join("notes.md"), "# Hand written\n\nSomething.").unwrap();

        let listed = store.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "notes");
        assert_eq!(listed[0].title, "notes");
        assert_eq!(store.body("notes").unwrap(), "# Hand written\n\nSomething.");
    }

    #[test]
    fn exporting_writes_the_document_as_it_is_held() {
        let (temp, store) = store();
        let saved = store
            .save("2026-08-22", "A day", "# A day\n\nIt went well.")
            .unwrap();

        let destination = temp.path().join("elsewhere.md");
        export(&store, &saved.id, &destination).unwrap();

        assert_eq!(
            std::fs::read_to_string(&destination).unwrap(),
            store.read(&saved.id).unwrap()
        );
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        let (temp, store) = store();
        store.save("2026-08-22", "A day", "text").unwrap();

        let leftovers: Vec<String> = std::fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["2026-08-22.md".to_string()]);
    }
}
