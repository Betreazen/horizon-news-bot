//! Reads Horizon's published releases from the GitHub API.
//!
//! Two requests per run: the newest releases, and the moving `nightly`
//! release by tag, because GitHub does not list it by publication time and
//! a burst of launcher releases could push it off the first page. Both send
//! the ETag of the previous answer, so an unchanged repository costs two
//! empty 304 replies.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use ureq::Agent;

use crate::state::{Etags, State};

const USER_AGENT: &str = "horizon-news-bot";
const PAGE_SIZE: u32 = 30;

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    #[serde(rename = "tag_name")]
    pub tag: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(rename = "html_url")]
    pub url: String,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub draft: bool,
}

/// Everything one run learned from GitHub.
#[derive(Debug)]
pub struct Snapshot {
    pub releases: Vec<Release>,
    /// ETags to store once this snapshot has been fully handled.
    pub etags: Etags,
}

enum Fetched<T> {
    NotModified,
    Missing,
    Fresh { value: T, etag: Option<String> },
}

pub fn fetch(
    agent: &Agent,
    repo: &str,
    token: Option<&str>,
    state: &State,
) -> Result<Snapshot, String> {
    let list_url = format!("https://api.github.com/repos/{repo}/releases?per_page={PAGE_SIZE}");
    let nightly_url = format!("https://api.github.com/repos/{repo}/releases/tags/nightly");

    let list = get::<Vec<Release>>(agent, &list_url, token, state.etag_list.as_deref())?;
    let nightly = get::<Release>(agent, &nightly_url, token, state.etag_nightly.as_deref())?;

    let (mut releases, etag_list) = match list {
        Fetched::Fresh { value, etag } => (value, etag),
        Fetched::NotModified => (Vec::new(), state.etag_list.clone()),
        Fetched::Missing => {
            return Err(format!(
                "repository {repo} has no releases endpoint (HTTP 404)"
            ));
        }
    };
    let etag_nightly = match nightly {
        Fetched::Fresh { value, etag } => {
            releases.push(value);
            etag
        }
        Fetched::NotModified => state.etag_nightly.clone(),
        Fetched::Missing => None,
    };

    Ok(Snapshot {
        releases,
        etags: Etags {
            list: etag_list,
            nightly: etag_nightly,
        },
    })
}

fn get<T: DeserializeOwned>(
    agent: &Agent,
    url: &str,
    token: Option<&str>,
    etag: Option<&str>,
) -> Result<Fetched<T>, String> {
    let mut request = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT);
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    if let Some(etag) = etag {
        request = request.header("If-None-Match", etag);
    }

    let mut response = request.call().map_err(|e| format!("GET {url}: {e}"))?;
    match response.status().as_u16() {
        304 => Ok(Fetched::NotModified),
        404 => Ok(Fetched::Missing),
        200 => {
            let etag = response
                .headers()
                .get("etag")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let value = response
                .body_mut()
                .read_json::<T>()
                .map_err(|e| format!("GET {url}: unexpected response body: {e}"))?;
            Ok(Fetched::Fresh { value, etag })
        }
        code => Err(format!("GET {url}: HTTP {code}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_release_list_deserializes() {
        let releases: Vec<Release> =
            serde_json::from_str(include_str!("../tests/fixtures/releases.json")).unwrap();
        assert_eq!(releases.len(), 12);
        assert_eq!(releases[0].tag, "launcher@0.2.58");
        assert!(releases.iter().any(|r| r.tag == "nightly"));
        assert!(
            releases
                .iter()
                .all(|r| r.url.starts_with("https://github.com/"))
        );
    }

    #[test]
    fn release_with_null_name_and_body_deserializes() {
        let json = r#"{"tag_name":"v1","name":null,"body":null,"html_url":"https://x","published_at":null}"#;
        let release: Release = serde_json::from_str(json).unwrap();
        assert_eq!(release.name, None);
        assert_eq!(release.body, None);
        assert!(!release.draft);
    }
}
