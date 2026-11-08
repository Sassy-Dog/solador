//! Token-driven repo discovery — the types behind Settings' "choose repos"
//! picker, and the org derivation the runner-org picker reads.
//!
//! `GET /user/repos` lists "repositories that the authenticated user has
//! explicit permission to access", which for a fine-grained PAT is exactly
//! the repos the token was granted — the list the picker offers checkboxes
//! over. The org list is **derived from that same result** (the distinct
//! owners of type `Organization`) rather than fetched: `GET /user/orgs`
//! answers a fine-grained PAT with `200` and an **empty list**, and
//! fine-grained PATs are the only token kind this app asks operators for. A
//! picker built on that endpoint would render empty for every operator who
//! followed the app's own setup instructions.

use serde::Deserialize;

/// One repository the token can access, as the picker renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    /// `owner/name` — the same slug spelling `TrackedRepo` keys on.
    pub full_name: String,
    /// The owner's login, org or user.
    pub owner_login: String,
    /// Whether the owner is an organization — what the org derivation reads.
    pub owner_is_org: bool,
    /// Archived repos are still offered (the operator may want a read-only
    /// watch) but the picker tags them.
    pub archived: bool,
}

/// What one discovery walk found: the accessible repos, and whether the walk
/// was cut short at [`crate::client::DISCOVERY_PAGE_CAP`].
///
/// `truncated` exists so a capped walk is **reported, never silent**: a
/// picker that quietly dropped page eleven would read as "covered
/// everything" to the one operator with more repos than the cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibleRepos {
    pub repos: Vec<RepoRef>,
    pub truncated: bool,
}

/// The wire shape of one `/user/repos` entry — only the fields the picker
/// reads. Strict on purpose: a page this build cannot read is skew to report
/// (`DecodeFailed`), not a repo list to guess at.
#[derive(Debug, Deserialize)]
pub(crate) struct UserRepoDto {
    full_name: String,
    archived: bool,
    owner: OwnerDto,
}

#[derive(Debug, Deserialize)]
struct OwnerDto {
    login: String,
    /// `"Organization"` or `"User"` — GitHub's spelling, compared exactly.
    #[serde(rename = "type")]
    kind: String,
}

/// One decoded page into picker rows.
pub(crate) fn map(dtos: Vec<UserRepoDto>) -> Vec<RepoRef> {
    dtos.into_iter()
        .map(|dto| RepoRef {
            full_name: dto.full_name,
            owner_login: dto.owner.login,
            owner_is_org: dto.owner.kind == "Organization",
            archived: dto.archived,
        })
        .collect()
}

/// The distinct organizations among the accessible repos' owners, sorted.
///
/// This is the whole of org discovery — see the module doc for why it is a
/// derivation rather than a request. A user-owned repo contributes nothing:
/// a user is not an organization, and `GET /orgs/{user}/actions/runners`
/// would 404.
#[must_use]
pub fn organizations(repos: &[RepoRef]) -> Vec<String> {
    let orgs: std::collections::BTreeSet<&str> = repos
        .iter()
        .filter(|repo| repo.owner_is_org)
        .map(|repo| repo.owner_login.as_str())
        .collect();
    orgs.into_iter().map(ToOwned::to_owned).collect()
}

/// Where `query` first occurs in `full_name`, ignoring case and the query's
/// surrounding whitespace, as **char** offsets `(start, end)` so a page can
/// bold exactly that span. `None` for an empty query or no match.
#[must_use]
pub fn match_range(full_name: &str, query: &str) -> Option<(usize, usize)> {
    let needle: Vec<char> = query.trim().chars().collect();
    let hay: Vec<char> = full_name.chars().collect();
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len())
        .find(|&start| {
            hay[start..start + needle.len()]
                .iter()
                .zip(&needle)
                .all(|(a, b)| a.to_lowercase().eq(b.to_lowercase()))
        })
        .map(|start| (start, start + needle.len()))
}

/// The accessible repos whose `owner/name` contains `query` anywhere,
/// ignoring case, in the list's own order. An empty query matches nothing:
/// the watched list already covers "show me everything".
#[must_use]
pub fn search<'a>(repos: &'a [RepoRef], query: &str) -> Vec<&'a RepoRef> {
    repos
        .iter()
        .filter(|repo| match_range(&repo.full_name, query).is_some())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = include_str!("../tests/fixtures/user_repos.json");

    fn fixture_repos() -> Vec<RepoRef> {
        map(serde_json::from_str(PAGE).expect("decode"))
    }

    #[test]
    fn a_user_repos_page_decodes_into_picker_rows() {
        let repos = fixture_repos();
        assert_eq!(
            repos,
            vec![
                RepoRef {
                    full_name: "acme/gadget".into(),
                    owner_login: "acme".into(),
                    owner_is_org: true,
                    archived: false,
                },
                RepoRef {
                    full_name: "acme/legacy-tool".into(),
                    owner_login: "acme".into(),
                    owner_is_org: true,
                    archived: true,
                },
                RepoRef {
                    full_name: "jdoe/dotfiles".into(),
                    owner_login: "jdoe".into(),
                    owner_is_org: false,
                    archived: false,
                },
            ]
        );
    }

    #[test]
    fn organizations_are_the_distinct_org_owners_and_never_a_user() {
        assert_eq!(
            organizations(&fixture_repos()),
            vec!["acme".to_owned()],
            "two acme repos derive one org, and jdoe is a person, not an org"
        );
        assert!(organizations(&[]).is_empty());
    }

    fn named(full_name: &str) -> RepoRef {
        let (owner, _) = full_name.split_once('/').unwrap();
        RepoRef {
            full_name: full_name.into(),
            owner_login: owner.into(),
            owner_is_org: true,
            archived: false,
        }
    }

    #[test]
    fn search_is_trimmed_and_case_insensitive() {
        let repos = [
            named("globex/claims-web"),
            named("globex/billing-api"),
            named("Acme/CLAIMS"),
        ];
        let found: Vec<&str> = search(&repos, " Claims ")
            .iter()
            .map(|r| r.full_name.as_str())
            .collect();
        assert_eq!(found, ["globex/claims-web", "Acme/CLAIMS"]);
        // The owner is part of what is searched.
        assert_eq!(search(&repos, "GLOBEX/").len(), 2);
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        let repos = [named("acme/gadget")];
        assert!(search(&repos, "").is_empty());
        assert!(search(&repos, "   ").is_empty());
    }

    #[test]
    fn match_range_is_char_offsets() {
        assert_eq!(match_range("acme/claims-web", "CLAIMS"), Some((5, 11)));
        assert_eq!(match_range("acme/claims-web", " web "), Some((12, 15)));
        assert_eq!(match_range("acme/claims-web", "zzz"), None);
        assert_eq!(match_range("acme/claims-web", ""), None);
        // Offsets count characters, not bytes, so a page can slice by them.
        assert_eq!(match_range("ünï/claims", "claims"), Some((4, 10)));
    }
}
