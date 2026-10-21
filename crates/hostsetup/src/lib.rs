//! Adding a remote host from the command line, through the running app.
//!
//! Two halves of one exchange. The **command mode** (`cli`) is the app's own
//! executable invoked as `solador-app host …`: it never opens the store or the
//! credential store, it asks. The **server** (`server`, over `channel`) runs
//! inside the app and does every write through the same code the Settings form
//! uses, so the app stays the only writer of `store.json` and the keychain.
//!
//! Pairing from here keeps Settings' rule that nothing is pinned by a probe
//! alone. The operator's eyes comparing two fingerprints become a comparison
//! the app makes between what it finds at the address and what the host itself
//! printed (`--fingerprint`), and the token is checked over that pinned
//! connection before anything is saved.
//!
//! `host add --ssh` (`remote`) is the same pairing with the operator's two
//! copy-and-pastes done over ssh: the fingerprint and the token are read on
//! the host, as the ssh user, by the script it embeds, and the app still
//! checks both itself.

#[cfg(unix)]
pub mod channel;
pub mod cli;
pub mod pairing;
pub mod protocol;
#[cfg(unix)]
pub mod remote;
mod secret;
pub mod server;

pub use secret::Secret;
