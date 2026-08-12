//! GitHub `Link`-header arithmetic. Port of the `lastPage`/`hasNextPage`
//! statics on `GitHubService`.
//!
//! Counting a collection normally costs one request per page. GitHub's `Link`
//! header makes it cost one request full stop: ask for `per_page=1` and the
//! `page=` of the `rel="last"` URL *is* the total, because one item per page
//! means one page per item.
//!
//! The trap is the endpoint that paginates by cursor (`/issues`): it emits
//! `rel="next"` with an opaque `after=` cursor and **no** `rel="last"`. Reading
//! "no last page" as "single page" there silently reports every repo as having
//! one open issue. [`has_next_page`] exists purely so the caller can tell those
//! two cases apart and refuse to answer rather than undercount.

/// The `page=` value of the `rel="last"` URL — the total, when paging with
/// `per_page=1`.
///
/// `None` means the header carried no `rel="last"`: either a genuinely
/// single-page result (no `Link` at all) or cursor pagination. Those are not
/// the same thing, which is why callers must consult [`has_next_page`] before
/// falling back to the returned array's length.
#[must_use]
pub fn last_page(header: Option<&str>) -> Option<u32> {
    let header = header?;
    // Each comma-separated part looks like:
    // <https://api.github.com/…?per_page=1&page=37>; rel="last"
    for part in header.split(',') {
        if !part.contains("rel=\"last\"") {
            continue;
        }
        let open = part.find('<')?;
        let close = part.find('>')?;
        if open >= close {
            return None;
        }
        return query_value(&part[open + 1..close], "page")?.parse().ok();
    }
    None
}

/// Whether the header advertises a `rel="next"` page.
///
/// Distinguishes a single-page result (no `Link` at all → safe to count the
/// returned array) from cursor pagination (`rel="next"` without `rel="last"` →
/// total unknowable, must not undercount).
#[must_use]
pub fn has_next_page(header: Option<&str>) -> bool {
    header.is_some_and(|h| h.contains("rel=\"next\""))
}

/// First value of `name` in `url`'s query string. Parsed by name, never by
/// position — GitHub does not promise an ordering, and `?page=12&per_page=1` is
/// as valid as `?per_page=1&page=12`.
fn query_value<'a>(url: &'a str, name: &str) -> Option<&'a str> {
    let (_, query) = url.split_once('?')?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The branch count is the `page=` of the `rel="last"` URL when paging with
    /// `per_page=1`: page 37 of 1-per-page == 37 branches.
    #[test]
    fn last_page_parses_rel_last() {
        let header = "<https://api.github.com/repositories/1/branches?per_page=1&page=2>; rel=\"next\", \
                      <https://api.github.com/repositories/1/branches?per_page=1&page=37>; rel=\"last\"";
        assert_eq!(last_page(Some(header)), Some(37));
    }

    /// A single-page result has no `Link` header — the caller falls back to the
    /// returned array's length, so the parser answers `None` here.
    #[test]
    fn last_page_is_none_when_the_header_is_absent() {
        assert_eq!(last_page(None), None);
    }

    /// A `Link` header without a `rel="last"` relation (e.g. only `first`/`prev`
    /// on the final page) yields `None` rather than a wrong count.
    #[test]
    fn last_page_is_none_without_a_last_relation() {
        let header =
            "<https://api.github.com/repositories/1/branches?per_page=1&page=1>; rel=\"first\"";
        assert_eq!(last_page(Some(header)), None);
    }

    /// `page` ordering within the URL must not matter — it is parsed by query
    /// name.
    #[test]
    fn last_page_handles_page_before_per_page() {
        assert_eq!(
            last_page(Some(
                "<https://api.github.com/x?page=12&per_page=1>; rel=\"last\""
            )),
            Some(12)
        );
    }

    /// The `/issues` endpoint paginates by cursor: `rel="next"` with an opaque
    /// `after=` cursor and NO `rel="last"`. The total is unknowable from the
    /// header, so the last-page trick cannot count it.
    #[test]
    fn last_page_is_none_for_cursor_paginated_issues() {
        let header = "<https://api.github.com/repositories/1/issues?state=open&per_page=1\
                      &after=Y3Vyc29yOnYyOpLP&page=2>; rel=\"next\"";
        assert_eq!(last_page(Some(header)), None);
    }

    /// `has_next_page` is what lets the caller refuse a silent `per_page=1`
    /// undercount when the total is unknowable.
    #[test]
    fn has_next_page_detects_a_next_relation() {
        let cursor =
            "<https://api.github.com/repositories/1/issues?per_page=1&after=abc&page=2>; rel=\"next\"";
        assert!(has_next_page(Some(cursor)));
        assert!(!has_next_page(Some(
            "<https://api.github.com/x?per_page=1&page=3>; rel=\"last\""
        )));
        assert!(!has_next_page(None));
    }

    /// A `rel="last"` URL carrying no `page=` at all is not a count of 1 — it is
    /// no answer. Anything else would invent a number from a malformed header.
    #[test]
    fn last_page_is_none_when_the_last_url_has_no_page_query() {
        assert_eq!(
            last_page(Some("<https://api.github.com/x?per_page=1>; rel=\"last\"")),
            None
        );
        assert_eq!(
            last_page(Some("<https://api.github.com/x>; rel=\"last\"")),
            None
        );
    }

    /// A non-numeric `page=` is garbage, not zero.
    #[test]
    fn last_page_is_none_when_page_is_not_a_number() {
        assert_eq!(
            last_page(Some("<https://api.github.com/x?page=many>; rel=\"last\"")),
            None
        );
    }
}
