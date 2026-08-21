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
}
