//! One rule for dropping a caller-supplied identifier into a URL *path*.
//!
//! Both provider clients splice an org identifier the user typed free-hand in
//! Settings into a path segment — `crates/store` declares `neon_org_id` and the
//! Sentry org slug as plain `String`s, with no format or charset validation. A
//! separator that survives unencoded does not produce a bad request, it
//! produces a *different* one: `a/../b` walks out of the organizations path.
//! Sharing the set is what keeps the two call sites from drifting into two
//! answers.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC};

/// Percent-encode everything outside RFC 3986's unreserved set.
///
/// Deliberately stricter than the original's `.urlPathAllowed`, which *permits* `/`
/// and so leaves the retargeting above possible. Encoding the separator is the
/// only way the segment stays one segment.
pub(crate) const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');
