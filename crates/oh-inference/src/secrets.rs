//! Where the Anthropic API key is kept.
//!
//! In the Windows Credential Manager, through the `keyring` crate. Not in
//! `config.json`, which is a plain file the user is invited to read and hand-edit, and
//! not in any file this application writes.
//!
//! The plan named `tauri-plugin-stronghold`. That plugin encrypts a vault with a
//! password the application would then have to store somewhere, which moves the
//! problem rather than solving it. The Credential Manager is the operating system's
//! own answer, is already protected by the user's login, and needs no extra secret.

use oh_core::InferenceProvider;

/// The service name the credential is filed under. Visible to the user in the
/// Credential Manager, so it says what it is.
const SERVICE: &str = "OpenHistory";

/// Which secret is being read or written.
///
/// One per cloud provider. They are separate credentials in the Credential Manager, so
/// choosing a different model in the dropdown does not throw away the key for the one
/// that was there before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Secret {
    AnthropicApiKey,
    #[serde(rename = "openAiApiKey")]
    OpenAiApiKey,
    GoogleApiKey,
}

/// Every secret this application knows how to store, in dropdown order.
pub const SECRETS: &[Secret] = &[
    Secret::AnthropicApiKey,
    Secret::OpenAiApiKey,
    Secret::GoogleApiKey,
];

impl Secret {
    fn account(self) -> &'static str {
        match self {
            Secret::AnthropicApiKey => "anthropic-api-key",
            Secret::OpenAiApiKey => "openai-api-key",
            Secret::GoogleApiKey => "google-api-key",
        }
    }

    /// The provider this key belongs to.
    pub fn provider(self) -> InferenceProvider {
        match self {
            Secret::AnthropicApiKey => InferenceProvider::Anthropic,
            Secret::OpenAiApiKey => InferenceProvider::OpenAi,
            Secret::GoogleApiKey => InferenceProvider::Google,
        }
    }

    /// What the settings page calls it.
    pub fn label(self) -> &'static str {
        match self {
            Secret::AnthropicApiKey => "Anthropic API key",
            Secret::OpenAiApiKey => "OpenAI API key",
            Secret::GoogleApiKey => "Google AI Studio API key",
        }
    }

    pub fn for_provider(provider: InferenceProvider) -> Option<Self> {
        match provider {
            InferenceProvider::Anthropic => Some(Secret::AnthropicApiKey),
            InferenceProvider::OpenAi => Some(Secret::OpenAiApiKey),
            InferenceProvider::Google => Some(Secret::GoogleApiKey),
            InferenceProvider::Disabled | InferenceProvider::Local => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the Windows Credential Manager refused the request: {0}")]
pub struct SecretError(String);

pub type Result<T> = std::result::Result<T, SecretError>;

fn entry(secret: Secret) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, secret.account()).map_err(|error| SecretError(error.to_string()))
}

/// Store a secret, replacing whatever was there.
pub fn store(secret: Secret, value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        return forget(secret);
    }
    entry(secret)?
        .set_password(value)
        .map_err(|error| SecretError(error.to_string()))
}

/// Read a secret. `None` when none has been stored.
pub fn load(secret: Secret) -> Result<Option<String>> {
    #[cfg(test)]
    match pretended() {
        Some(Pretence::Stored(value)) => return Ok(Some(value)),
        Some(Pretence::Missing) => return Ok(None),
        None => {}
    }

    match entry(secret)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(SecretError(error.to_string())),
    }
}

/// Delete a secret. Deleting one that is not there is not an error.
pub fn forget(secret: Secret) -> Result<()> {
    match entry(secret)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(SecretError(error.to_string())),
    }
}

/// Whether a secret is present, without reading it.
///
/// This is what the interface asks: settings shows that a key is stored, never the key
/// itself, and nothing in the application has any reason to send a stored key back to
/// the window it was typed into.
pub fn is_stored(secret: Secret) -> bool {
    matches!(load(secret), Ok(Some(_)))
}

/// What a test has decided the credential store holds.
///
/// Absent means the real Credential Manager answers, which only the ignored
/// round-trip tests below want.
#[cfg(test)]
#[derive(Clone)]
enum Pretence {
    Stored(String),
    Missing,
}

#[cfg(test)]
thread_local! {
    static PRETENDED: std::cell::RefCell<Option<Pretence>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn pretended() -> Option<Pretence> {
    PRETENDED.with(|slot| slot.borrow().clone())
}

/// Behave as though this key were stored, for this thread only.
///
/// Tests must not read or write the developer's real Credential Manager, and the
/// override is thread-local because the test harness runs tests in parallel.
#[cfg(test)]
pub(crate) fn pretend_stored(value: &str) {
    PRETENDED.with(|slot| *slot.borrow_mut() = Some(Pretence::Stored(value.to_owned())));
}

/// Behave as though no key were stored, for this thread only.
///
/// A test that expects a missing key has to say so. Reading the real store instead
/// would make it pass or fail according to whether the person running it happens to
/// use the application.
#[cfg(test)]
pub(crate) fn pretend_missing() {
    PRETENDED.with(|slot| *slot.borrow_mut() = Some(Pretence::Missing));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cloud_provider_has_its_own_secret() {
        assert_eq!(
            Secret::for_provider(InferenceProvider::Anthropic),
            Some(Secret::AnthropicApiKey)
        );
        assert_eq!(
            Secret::for_provider(InferenceProvider::OpenAi),
            Some(Secret::OpenAiApiKey)
        );
        assert_eq!(
            Secret::for_provider(InferenceProvider::Google),
            Some(Secret::GoogleApiKey)
        );
        assert_eq!(Secret::for_provider(InferenceProvider::Local), None);
        assert_eq!(Secret::for_provider(InferenceProvider::Disabled), None);
    }

    #[test]
    fn the_three_keys_are_filed_under_three_different_accounts() {
        let accounts: Vec<&str> = SECRETS.iter().map(|secret| secret.account()).collect();
        let mut unique = accounts.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(accounts.len(), unique.len(), "{accounts:?}");

        for secret in SECRETS {
            assert_eq!(Secret::for_provider(secret.provider()), Some(*secret));
        }
    }

    /// Touches the real Credential Manager, so it is ignored by default: a test that
    /// writes to the user's credential store should be asked for, not stumbled into.
    ///
    /// Run with `cargo test -p oh-inference -- --ignored`.
    #[test]
    #[ignore = "writes to the Windows Credential Manager"]
    fn a_secret_survives_a_store_and_load() {
        let secret = Secret::AnthropicApiKey;
        let existing = load(secret).unwrap();

        store(secret, "sk-ant-test-value").unwrap();
        assert_eq!(load(secret).unwrap().as_deref(), Some("sk-ant-test-value"));
        assert!(is_stored(secret));

        forget(secret).unwrap();
        assert_eq!(load(secret).unwrap(), None);
        assert!(!is_stored(secret));
        forget(secret).unwrap();

        if let Some(original) = existing {
            store(secret, &original).unwrap();
        }
    }

    #[test]
    #[ignore = "writes to the Windows Credential Manager"]
    fn storing_an_empty_value_clears_the_secret() {
        let secret = Secret::AnthropicApiKey;
        let existing = load(secret).unwrap();

        store(secret, "sk-ant-test-value").unwrap();
        store(secret, "   ").unwrap();
        assert_eq!(load(secret).unwrap(), None);

        if let Some(original) = existing {
            store(secret, &original).unwrap();
        }
    }
}
