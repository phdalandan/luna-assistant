use std::fmt;

use keyring::{Entry, Error as KeyringError};

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

fn entry() -> Result<Entry, CredentialError> {
    Ok(Entry::new(SERVICE, HOME_ASSISTANT_TOKEN)?)
}

pub fn load_home_assistant_token() -> Result<Option<AccessToken>, CredentialError> {
    match entry()?.get_password() {
        Ok(value) => Ok(AccessToken::new(value)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn save_home_assistant_token(token: &AccessToken) -> Result<(), CredentialError> {
    Ok(entry()?.set_password(token.expose())?)
}

pub fn delete_home_assistant_token() -> Result<(), CredentialError> {
    match entry()?.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_redacted_in_debug_output() {
        let token = AccessToken::new("secret-value".into()).unwrap();
        assert!(!format!("{token:?}").contains("secret"));
    }

    #[test]
    fn blank_tokens_are_rejected() {
        assert_eq!(AccessToken::new("   ".into()), None);
        assert_eq!(AccessToken::new(" abc ".into()).unwrap().expose(), "abc");
    }
}
