//! The two types that hold borrowed secret material, and the reason neither
//! can leak by accident.
//!
//! Neither type implements `Display`, `Clone`, `Serialize` or `PartialEq`, and
//! each `Debug` prints a placeholder. A token can only leave through
//! [`AccessToken::expose`], which is crate-private and called in exactly one
//! place per client: the `Authorization` header. Formatting one into a log
//! line, an error or a test assertion does not compile into a leak; it
//! compiles into `AccessToken(<redacted>)`.

use std::fmt;

/// One OAuth access token, borrowed from another program's saved sign-in for
/// one request and dropped after it.
pub struct AccessToken(String);

impl AccessToken {
    /// `None` for a blank value. A whitespace token is not a token, and a
    /// request carrying one would only come back 401.
    pub(crate) fn new(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| AccessToken(trimmed.to_owned()))
    }

    /// The token, for the `Authorization` header and nothing else.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

/// Raw text read out of a keychain item. For Claude Code's item that is a JSON
/// document holding the access token **and the refresh token**, so it is held
/// just long enough to pull the access token out.
pub struct SecretText(String);

impl SecretText {
    pub(crate) fn new(raw: String) -> Self {
        SecretText(raw)
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretText(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guarantee the types exist for: formatting one, by any route a log
    /// line or an assertion would take, never prints the value.
    #[test]
    fn neither_type_prints_its_value_when_formatted() {
        let token = AccessToken::new("sk-ant-oat01-SECRET").expect("token");
        let text = SecretText::new(r#"{"claudeAiOauth":{"refreshToken":"RT-SECRET"}}"#.into());
        for printed in [
            format!("{token:?}"),
            format!("{token:#?}"),
            format!("{text:?}"),
            format!("{:?}", Some(&token)),
        ] {
            assert!(!printed.contains("SECRET"), "{printed}");
        }
        assert_eq!(token.expose(), "sk-ant-oat01-SECRET");
    }

    #[test]
    fn a_blank_token_is_no_token() {
        assert!(AccessToken::new("").is_none());
        assert!(AccessToken::new("  \n").is_none());
        assert_eq!(AccessToken::new(" tok \n").expect("token").expose(), "tok");
    }
}
