//! The bearer token in transit, in a type that cannot be printed by accident.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A host's bearer token on its way from the command line to the running app.
///
/// `Debug` and `Display` both redact, so a `{:?}` in an error path, a log line
/// or a test failure message never carries the value. The only way to the
/// string is [`Secret::expose`], which is greppable on purpose.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// The token itself. Every call site is a place the token leaves this
    /// type: the credential store, the `Authorization` header, a comparison.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neither_debug_nor_display_shows_the_token() {
        let secret = Secret::new("hunter2-token");
        assert!(!format!("{secret:?}").contains("hunter2"));
        assert!(!format!("{secret}").contains("hunter2"));
        assert!(!format!("{:?}", Some(&secret)).contains("hunter2"));
        assert_eq!(secret.expose(), "hunter2-token");
    }
}
