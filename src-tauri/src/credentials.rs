use std::fmt;

use keyring::{Entry, Error as KeyringError};

use crate::models::CloudProvider;

const SERVICE: &str = "io.github.phdalandan.luna";
const HOME_ASSISTANT_TOKEN: &str = "home-assistant-token";

/// A secret that never appears in logs or debug output.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken(String);

impl AccessToken {
    pub fn new(value: String) -> Option<Self> {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| Self(trimmed.to_owned()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

#[derive(Debug, thiserror::Error)]
#[error("credential store error: {0}")]
pub struct CredentialError(#[from] KeyringError);

/// Each provider's key is stored separately, so switching providers keeps both.
fn api_key_account(provider: CloudProvider) -> &'static str {
    match provider {
        CloudProvider::OpenAi => "openai-api-key",
        CloudProvider::Anthropic => "anthropic-api-key",
    }
}

fn load(account: &str) -> Result<Option<AccessToken>, CredentialError> {
    match Entry::new(SERVICE, account)?.get_password() {
        Ok(value) => Ok(AccessToken::new(value)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn save(account: &str, secret: &AccessToken) -> Result<(), CredentialError> {
    Ok(Entry::new(SERVICE, account)?.set_password(secret.expose())?)
}

fn delete(account: &str) -> Result<(), CredentialError> {
    match Entry::new(SERVICE, account)?.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn load_home_assistant_token() -> Result<Option<AccessToken>, CredentialError> {
    load(HOME_ASSISTANT_TOKEN)
}

pub fn save_home_assistant_token(token: &AccessToken) -> Result<(), CredentialError> {
    save(HOME_ASSISTANT_TOKEN, token)
}

pub fn delete_home_assistant_token() -> Result<(), CredentialError> {
    delete(HOME_ASSISTANT_TOKEN)
}

pub fn load_api_key(provider: CloudProvider) -> Result<Option<AccessToken>, CredentialError> {
    load(api_key_account(provider))
}

pub fn save_api_key(provider: CloudProvider, key: &AccessToken) -> Result<(), CredentialError> {
    save(api_key_account(provider), key)
}

pub fn delete_api_key(provider: CloudProvider) -> Result<(), CredentialError> {
    delete(api_key_account(provider))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_redacted_in_debug_output() {
        let token = AccessToken::new("secret-value".into()).unwrap();
        assert!(!format!("{token:?}").contains("secret"));
    }

    /// Uses an in-memory store in place of the OS credential store, which is set up first.
    #[test]
    fn api_keys_are_stored_per_provider_and_removed() {
        Entry::store_status().as_ref().unwrap();
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
        let openai = AccessToken::new("openai-key".into()).unwrap();
        let anthropic = AccessToken::new("anthropic-key".into()).unwrap();

        save_api_key(CloudProvider::OpenAi, &openai).unwrap();
        save_api_key(CloudProvider::Anthropic, &anthropic).unwrap();
        assert_eq!(load_api_key(CloudProvider::OpenAi).unwrap(), Some(openai));
        assert_eq!(load(HOME_ASSISTANT_TOKEN).unwrap(), None);

        delete_api_key(CloudProvider::OpenAi).unwrap();
        delete_api_key(CloudProvider::OpenAi).unwrap();
        assert_eq!(load_api_key(CloudProvider::OpenAi).unwrap(), None);
        assert_eq!(
            load_api_key(CloudProvider::Anthropic).unwrap(),
            Some(anthropic)
        );
    }

    #[test]
    fn blank_tokens_are_rejected() {
        assert_eq!(AccessToken::new("   ".into()), None);
        assert_eq!(AccessToken::new(" abc ".into()).unwrap().expose(), "abc");
    }
}
