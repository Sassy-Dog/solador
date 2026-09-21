//! The **Ready** backlog count: open issues whose project-board `Status` is
//! `Ready`, read through the GraphQL API (Projects v2 has no REST surface).
//!
//! The read is **issue-side, never board-side**: it walks the repo's open
//! issues and asks each one which boards it sits on, rather than walking a
//! board and grouping by repo. That is what makes the number exact without
//! any board identity in configuration — the app is public, a stranger's
//! board has a different number and different option ids — and it holds
//! whether or not the repo is *linked* to the board. `repository.issues`
//! excludes pull requests by construction, and an issue on two boards is one
//! issue, so it counts once.
//!
//! **Unknown is not zero**, as everywhere in this crate. A GraphQL response
//! carrying `errors[]` is refused as a whole, even when `data` is partially
//! filled: a fine-grained PAT without the org's *Projects (read)* permission
//! comes back exactly like that — the issues decode, and the refused
//! `projectItems` field arrives as `null` beside a `FORBIDDEN` error (the
//! field introspects as nullable, which is what the spec does with a field
//! that errors) — and reading the partial answer would report that every
//! board is empty on a token that cannot see any board. So every field a
//! refusal can null is an `Option` here, `errors[]` is read before any of
//! them, and a `null` that arrives with *no* error attached is refused too
//! rather than read as an issue on no board. The page cap is the same
//! argument at a different scale: a repo with more open issues than the walk
//! will visit gets no number, never the number so far.

use serde::Deserialize;

/// The board column that means "groomed and dispatchable". Matched exactly:
/// the column is literally `Ready` in every board this cockpit was built
/// against, and a looser match would silently count a `Ready for review`
/// column as backlog.
pub const READY_STATUS: &str = "Ready";

/// Open issues per page. GraphQL's ceiling for a connection.
pub const ISSUES_PER_PAGE: u32 = 100;

/// Boards an issue is inspected on. Ten is an order of magnitude past any
/// real portfolio's use of one issue; an issue on more boards than this could
/// be counted wrong, so the walk refuses (see [`count_page`]).
pub const PROJECT_ITEMS_PER_ISSUE: u32 = 10;

/// The walk's backstop: 10 pages × 100 = 1,000 open issues. Hitting it is a
/// refusal, not a truncation — an undercount is a wrong number wearing the
/// clothes of a right one.
pub const PAGE_CAP: usize = 10;

/// One page of the walk, by field: `$owner`, `$name`, `$after`.
pub const QUERY: &str = "\
query($owner: String!, $name: String!, $after: String) {
  repository(owner: $owner, name: $name) {
    issues(states: OPEN, first: 100, after: $after) {
      pageInfo { hasNextPage endCursor }
      nodes {
        projectItems(first: 10) {
          pageInfo { hasNextPage }
          nodes {
            fieldValueByName(name: \"Status\") {
              ... on ProjectV2ItemFieldSingleSelectValue { name }
            }
          }
        }
      }
    }
  }
}";

/// The envelope every GraphQL response wears. `errors` is the whole verdict:
/// present means refused, whatever `data` holds.
#[derive(Debug, Deserialize)]
pub struct Envelope {
    #[serde(default)]
    pub data: Option<Data>,
    #[serde(default)]
    pub errors: Vec<GraphQlError>,
}

/// One entry of `errors[]`. GitHub's `type` is the classification — a
/// `RATE_LIMITED` arrives as HTTP 200 with one of these, a refused field as
/// `FORBIDDEN` — and `message` is for the log.
#[derive(Debug, Deserialize)]
pub struct GraphQlError {
    #[serde(default, rename = "type")]
    pub error_type: String,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct Data {
    pub repository: Option<Repository>,
}

#[derive(Debug, Deserialize)]
pub struct Repository {
    pub issues: IssueConnection,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueConnection {
    pub page_info: PageInfo,
    /// A node the server could not resolve is `null` in its slot.
    pub nodes: Vec<Option<Issue>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub has_next_page: bool,
    #[serde(default)]
    pub end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    /// `null` when the field was refused — the no-Projects-permission shape.
    #[serde(default)]
    pub project_items: Option<ProjectItemConnection>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectItemConnection {
    pub page_info: ItemPageInfo,
    /// `[ProjectV2Item]` — each entry nullable. An item on a board the
    /// token cannot read is `null` in its slot: that is the shape a
    /// fine-grained PAT without the org's Projects permission produces on
    /// every issue that sits on a board, and the one that reached production
    /// as "couldn't read GitHub's response" while this was `Vec<ProjectItem>`.
    pub nodes: Vec<Option<ProjectItem>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemPageInfo {
    pub has_next_page: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectItem {
    /// `null` when the board has no `Status` field or the item has no value
    /// in it — neither is `Ready`.
    #[serde(default)]
    pub field_value_by_name: Option<FieldValue>,
}

/// Only the single-select variant carries a `name`; every other field type
/// decodes to `{}` through the inline fragment, so `name` defaults to absent.
#[derive(Debug, Deserialize)]
pub struct FieldValue {
    #[serde(default)]
    pub name: Option<String>,
}

/// Why one page could not be counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageError {
    /// The response carried `errors[]`: the first entry's `type` and message.
    Refused { error_type: String, message: String },
    /// An issue node, its `projectItems`, or one of its items was `null`
    /// with no error attached. Refused rather than read as "on no board": a
    /// null is what an unreadable board looks like, and an unexplained one
    /// is not a count.
    NullField,
    /// `data.repository` was `null` with no error attached — a repo the token
    /// cannot see, which GitHub reports as absence.
    NoRepository,
    /// An issue sat on more boards than [`PROJECT_ITEMS_PER_ISSUE`] asked
    /// for, so its `Ready` may be on a board this page did not show.
    TooManyBoards,
}

/// The tally of one page and where the next one starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub ready: u32,
    pub next: Option<String>,
}

/// Count one decoded page: the issues with *any* board item whose `Status`
/// is [`READY_STATUS`].
///
/// # Errors
///
/// Refuses, rather than counting what it can, when the envelope carries
/// errors, when the repository is absent, or when an issue's board list was
/// cut short — every one of those is a page whose count would be an
/// undercount.
pub fn count_page(envelope: Envelope) -> Result<Page, PageError> {
    if let Some(first) = envelope.errors.first() {
        return Err(PageError::Refused {
            error_type: first.error_type.clone(),
            message: first.message.clone(),
        });
    }
    let repository = envelope
        .data
        .and_then(|d| d.repository)
        .ok_or(PageError::NoRepository)?;
    let issues = repository.issues;
    let mut ready = 0u32;
    for issue in &issues.nodes {
        let items = issue
            .as_ref()
            .and_then(|issue| issue.project_items.as_ref())
            .ok_or(PageError::NullField)?;
        if items.page_info.has_next_page {
            return Err(PageError::TooManyBoards);
        }
        let mut is_ready = false;
        for item in &items.nodes {
            let item = item.as_ref().ok_or(PageError::NullField)?;
            if item
                .field_value_by_name
                .as_ref()
                .and_then(|v| v.name.as_deref())
                == Some(READY_STATUS)
            {
                is_ready = true;
            }
        }
        if is_ready {
            ready += 1;
        }
    }
    let next = if issues.page_info.has_next_page {
        issues.page_info.end_cursor
    } else {
        None
    };
    Ok(Page { ready, next })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(body: &str) -> Envelope {
        serde_json::from_str(body).expect("envelope decodes")
    }

    fn item(status: Option<&str>) -> String {
        match status {
            Some(s) => format!(r#"{{"fieldValueByName":{{"name":"{s}"}}}}"#),
            None => r#"{"fieldValueByName":null}"#.into(),
        }
    }

    fn issue(items: &[String]) -> String {
        format!(
            r#"{{"projectItems":{{"pageInfo":{{"hasNextPage":false}},"nodes":[{}]}}}}"#,
            items.join(",")
        )
    }

    fn page(issues: &[String], next: Option<&str>) -> String {
        let page_info = match next {
            Some(c) => format!(r#"{{"hasNextPage":true,"endCursor":"{c}"}}"#),
            None => r#"{"hasNextPage":false,"endCursor":null}"#.into(),
        };
        format!(
            r#"{{"data":{{"repository":{{"issues":{{"pageInfo":{page_info},"nodes":[{}]}}}}}}}}"#,
            issues.join(",")
        )
    }

    #[test]
    fn counts_issues_whose_status_is_ready_exactly() {
        let body = page(
            &[
                issue(&[item(Some("Ready"))]),
                issue(&[item(Some("Backlog"))]),
                issue(&[item(Some("In progress"))]),
                // Not the column: a looser match would count this as backlog.
                issue(&[item(Some("Ready for review"))]),
                issue(&[item(Some("ready"))]),
                issue(&[item(None)]),
                issue(&[]),
            ],
            None,
        );
        assert_eq!(
            count_page(decode(&body)),
            Ok(Page {
                ready: 1,
                next: None
            })
        );
    }

    #[test]
    fn an_issue_on_two_boards_counts_once() {
        let body = page(&[issue(&[item(Some("Ready")), item(Some("Ready"))])], None);
        assert_eq!(count_page(decode(&body)).map(|p| p.ready), Ok(1));
    }

    #[test]
    fn ready_on_any_board_counts() {
        let body = page(
            &[issue(&[
                item(Some("Done")),
                item(None),
                item(Some("Ready")),
            ])],
            None,
        );
        assert_eq!(count_page(decode(&body)).map(|p| p.ready), Ok(1));
    }

    #[test]
    fn a_page_with_more_hands_back_its_cursor() {
        let body = page(&[issue(&[item(Some("Ready"))])], Some("Y3Vyc29y"));
        assert_eq!(
            count_page(decode(&body)),
            Ok(Page {
                ready: 1,
                next: Some("Y3Vyc29y".into())
            })
        );
    }

    #[test]
    fn a_last_page_carrying_a_cursor_does_not_hand_it_back() {
        // GraphQL sends `endCursor` on every non-empty page; only
        // `hasNextPage` says whether to follow it.
        let body = r#"{"data":{"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":"abc"},"nodes":[]}}}}"#;
        assert_eq!(count_page(decode(body)).map(|p| p.next), Ok(None));
    }

    #[test]
    fn no_open_issues_is_a_genuine_zero() {
        assert_eq!(
            count_page(decode(&page(&[], None))),
            Ok(Page {
                ready: 0,
                next: None
            })
        );
    }

    /// The fail-closed rule. This is the shape a fine-grained PAT without the
    /// org's Projects permission produces: the issues decode, the refused
    /// `projectItems` field is `null` in every node, and `errors[]` says why.
    /// The envelope must decode (a required field there would fail serde and
    /// land on the decode arm, hiding the reason) and the error must win.
    #[test]
    fn errors_beside_partial_data_refuse_the_whole_page() {
        let body = r#"{
          "data":{"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":null},
            "nodes":[{"projectItems":null},{"projectItems":null}]}}},
          "errors":[{"type":"FORBIDDEN","path":["repository","issues","nodes",0,"projectItems"],
                     "message":"Resource not accessible by personal access token"}]
        }"#;
        assert_eq!(
            count_page(decode(body)),
            Err(PageError::Refused {
                error_type: "FORBIDDEN".into(),
                message: "Resource not accessible by personal access token".into()
            })
        );
    }

    #[test]
    fn errors_with_no_data_at_all_refuse() {
        let body = r#"{"errors":[{"message":"Something went wrong"}]}"#;
        assert_eq!(
            count_page(decode(body)),
            Err(PageError::Refused {
                error_type: String::new(),
                message: "Something went wrong".into()
            })
        );
    }

    /// The shape that reached production: the issue decodes, its
    /// `projectItems` connection decodes, and the *item* is `null` — a board
    /// the token cannot read. Beside a `Ready` sibling on a readable board
    /// it is still refused: the null one might be the Ready one.
    #[test]
    fn a_null_project_item_is_refused_even_beside_a_ready_one() {
        let body = r#"{"data":{"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":null},
          "nodes":[{"projectItems":{"pageInfo":{"hasNextPage":false},"nodes":[{"fieldValueByName":{"name":"Ready"}},null]}}]}}}}"#;
        assert_eq!(count_page(decode(body)), Err(PageError::NullField));
    }

    /// A `null` with nothing in `errors[]` to explain it is not "on no
    /// board" — it is the refusal shape without its reason, and is refused.
    #[test]
    fn a_null_project_items_or_node_without_an_error_is_refused() {
        let body = r#"{"data":{"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":null},
          "nodes":[{"projectItems":{"pageInfo":{"hasNextPage":false},"nodes":[]}},{"projectItems":null}]}}}}"#;
        assert_eq!(count_page(decode(body)), Err(PageError::NullField));
        let body = r#"{"data":{"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":null},
          "nodes":[null]}}}}"#;
        assert_eq!(count_page(decode(body)), Err(PageError::NullField));
    }

    #[test]
    fn a_null_repository_is_absence_not_zero() {
        let body = r#"{"data":{"repository":null}}"#;
        assert_eq!(count_page(decode(body)), Err(PageError::NoRepository));
    }

    #[test]
    fn an_issue_on_more_boards_than_asked_for_refuses() {
        let body = r#"{"data":{"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":null},
          "nodes":[{"projectItems":{"pageInfo":{"hasNextPage":true},"nodes":[{"fieldValueByName":{"name":"Backlog"}}]}}]}}}}"#;
        assert_eq!(count_page(decode(body)), Err(PageError::TooManyBoards));
    }

    /// A non-single-select `Status` field (text, number…) decodes through the
    /// inline fragment as `{}`: present, nameless, and not Ready.
    #[test]
    fn a_status_field_of_another_type_is_not_ready() {
        let body = r#"{"data":{"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":null},
          "nodes":[{"projectItems":{"pageInfo":{"hasNextPage":false},"nodes":[{"fieldValueByName":{}}]}}]}}}}"#;
        assert_eq!(count_page(decode(body)).map(|p| p.ready), Ok(0));
    }

    #[test]
    fn the_query_asks_for_exactly_the_documented_page_sizes() {
        assert!(QUERY.contains(&format!("first: {ISSUES_PER_PAGE}, after: $after")));
        assert!(QUERY.contains(&format!("projectItems(first: {PROJECT_ITEMS_PER_ISSUE})")));
        assert!(QUERY.contains("states: OPEN"));
        assert!(QUERY.contains("fieldValueByName(name: \"Status\")"));
    }
}
