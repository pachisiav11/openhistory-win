//! The bearer token the MCP server checks.
//!
//! Only the SHA-256 of a token is written to `tokens.json`. The token itself exists in
//! two places: the clipboard of whoever generated it, and this process's memory for as
//! long as the application runs.
//!
//! That means the settings page can show a token it just made, and cannot show one
//! made before the last restart — it offers **Regenerate** instead. Keeping the
//! plaintext on disk so it could always be re-displayed would put a working credential
//! in a file, which is the thing hashing it was meant to avoid.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use oh_core::paths;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Prefix on every token, so one is recognizable in a config file or a log.
const PREFIX: &str = "oh_";

/// Bytes of randomness behind a token. 32 bytes is 256 bits.
const TOKEN_BYTES: usize = 32;

/// One accepted credential, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredToken {
    /// Lowercase hex SHA-256 of the token.
    pub hash: String,
    pub created_at: String,
    /// What the token is for, when the user said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenFile {
    #[serde(default)]
    tokens: Vec<StoredToken>,
}

/// The accepted tokens, and where they are kept.
#[derive(Debug, Clone)]
pub struct TokenStore {
    path: PathBuf,
    tokens: Vec<StoredToken>,
}

impl TokenStore {
    /// Load `tokens.json` from the data directory, or start empty.
    pub fn open() -> Result<Self> {
        Self::at(paths::tokens_file()?)
    }

    pub fn at(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let tokens = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<TokenFile>(&text)
                .map(|file| file.tokens)
                // An unreadable token file is treated as no tokens rather than as a
                // failure to start: the server then refuses every request, which is
                // the safe direction, and a new token can be generated.
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "unreadable tokens.json; ignoring it");
                    Vec::new()
                }),
            Err(_) => Vec::new(),
        };
        Ok(TokenStore { path, tokens })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn tokens(&self) -> &[StoredToken] {
        &self.tokens
    }

    /// Whether this token is one of the accepted ones.
    pub fn accepts(&self, presented: &str) -> bool {
        let presented = hash(presented);
        // Compared in full every time rather than short-circuiting on the first
        // mismatch, so the time taken says nothing about how much of a guess was right.
        self.tokens.iter().fold(false, |found, stored| {
            found | equal(&stored.hash, &presented)
        })
    }

    /// Replace every token with one new one and return it. Shown once.
    pub fn regenerate(&mut self, label: Option<String>) -> Result<String> {
        let token = mint();
        self.tokens = vec![StoredToken {
            hash: hash(&token),
            created_at: oh_core::summary::now(),
            label,
        }];
        self.save()?;
        Ok(token)
    }

    /// Make a token if there is none, so the server is usable on first run.
    pub fn ensure_one(&mut self) -> Result<Option<String>> {
        if self.tokens.is_empty() {
            return self.regenerate(None).map(Some);
        }
        Ok(None)
    }

    pub fn clear(&mut self) -> Result<()> {
        self.tokens.clear();
        self.save()
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            paths::ensure_dir(parent)?;
        }
        let text = serde_json::to_string_pretty(&TokenFile {
            tokens: self.tokens.clone(),
        })
        .context("could not serialize the token file")?;

        let temporary = self.path.with_extension("json.writing");
        std::fs::write(&temporary, text)
            .with_context(|| format!("could not write {}", temporary.display()))?;
        std::fs::rename(&temporary, &self.path)
            .with_context(|| format!("could not replace {}", self.path.display()))?;
        Ok(())
    }
}

/// A fresh token: the prefix and 256 bits of randomness in hex.
pub fn mint() -> String {
    let bytes: [u8; TOKEN_BYTES] = rand::random();
    format!("{PREFIX}{}", hex::encode(bytes))
}

/// Lowercase hex SHA-256 of a token.
pub fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// The token out of an `Authorization` header, if it carries a bearer one.
pub fn from_header(header: &str) -> Option<&str> {
    let (scheme, value) = header.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| value.trim())
        .filter(|token| !token.is_empty())
}

/// Compare two hex digests without letting the time taken depend on where they differ.
fn equal(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, TokenStore) {
        let temp = tempfile::tempdir().unwrap();
        let store = TokenStore::at(temp.path().join("tokens.json")).unwrap();
        (temp, store)
    }

    #[test]
    fn a_new_token_is_recognizable_and_long_enough_to_be_unguessable() {
        let token = mint();
        assert!(token.starts_with("oh_"));
        assert_eq!(token.len(), 3 + TOKEN_BYTES * 2);
        assert_ne!(token, mint());
    }

    #[test]
    fn the_token_itself_is_never_written_to_the_file() {
        let (_temp, mut store) = store();
        let token = store.regenerate(None).unwrap();

        let written = std::fs::read_to_string(store.path()).unwrap();
        assert!(!written.contains(&token), "{written}");
        assert!(written.contains(&hash(&token)));
    }

    #[test]
    fn a_generated_token_is_accepted_and_others_are_not() {
        let (_temp, mut store) = store();
        let token = store.regenerate(None).unwrap();

        assert!(store.accepts(&token));
        assert!(!store.accepts("oh_0000"));
        assert!(!store.accepts(""));
        assert!(!store.accepts(&mint()));
    }

    #[test]
    fn regenerating_stops_the_previous_token_from_working() {
        let (_temp, mut store) = store();
        let first = store.regenerate(None).unwrap();
        let second = store.regenerate(None).unwrap();

        assert!(!store.accepts(&first));
        assert!(store.accepts(&second));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn tokens_survive_a_reload() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tokens.json");

        let mut store = TokenStore::at(&path).unwrap();
        let token = store.regenerate(Some("Claude Code".into())).unwrap();

        let reopened = TokenStore::at(&path).unwrap();
        assert!(reopened.accepts(&token));
        assert_eq!(reopened.tokens()[0].label.as_deref(), Some("Claude Code"));
    }

    #[test]
    fn a_first_run_gets_a_token_and_a_second_run_keeps_it() {
        let (_temp, mut store) = store();
        let first = store.ensure_one().unwrap().expect("a token on first run");
        assert!(store.ensure_one().unwrap().is_none());
        assert!(store.accepts(&first));
    }

    #[test]
    fn an_unreadable_token_file_accepts_nothing_rather_than_everything() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tokens.json");
        std::fs::write(&path, "{ not json").unwrap();

        let store = TokenStore::at(&path).unwrap();
        assert!(store.is_empty());
        assert!(!store.accepts("anything"));
    }

    #[test]
    fn clearing_leaves_a_server_that_refuses_everyone() {
        let (_temp, mut store) = store();
        let token = store.regenerate(None).unwrap();
        store.clear().unwrap();
        assert!(!store.accepts(&token));
    }

    #[test]
    fn only_a_bearer_header_yields_a_token() {
        assert_eq!(from_header("Bearer oh_abc"), Some("oh_abc"));
        assert_eq!(from_header("bearer oh_abc"), Some("oh_abc"));
        assert_eq!(from_header("Basic oh_abc"), None);
        assert_eq!(from_header("Bearer "), None);
        assert_eq!(from_header("oh_abc"), None);
    }
}
