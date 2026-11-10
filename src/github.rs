//! Reads Horizon's published releases from the GitHub API.
//!
//! Two requests per run: the newest releases, and the moving `nightly`
//! release by tag, because GitHub does not list it by publication time and
//! a burst of launcher releases could push it off the first page.
//!
//! The requests are plain, not conditional. Release answers carry asset
//! download counts, so their ETags change from minute to minute and
//! `If-None-Match` never gets a 304 for a busy repository; this was
//! measured before the idea was dropped.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use ureq::Agent;

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

/// The newest releases plus the current `nightly`. The same release may
/// appear twice; the caller tells releases apart by key.
pub fn fetch(agent: &Agent, repo: &str, token: Option<&str>) -> Result<Vec<Release>, String> {
    let list_url = format!("https://api.github.com/repos/{repo}/releases?per_page={PAGE_SIZE}");
    let nightly_url = format!("https://api.github.com/repos/{repo}/releases/tags/nightly");

    let mut releases = get::<Vec<Release>>(agent, &list_url, token)?
        .ok_or_else(|| format!("repository {repo} was not found (HTTP 404)"))?;
    // A repository without a `nightly` release is fine.
    releases.extend(get::<Release>(agent, &nightly_url, token)?);
    Ok(releases)
}

/// `None` stands for HTTP 404.
fn get<T: DeserializeOwned>(
    agent: &Agent,
    url: &str,
    token: Option<&str>,
) -> Result<Option<T>, String> {
    let mut request = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT);
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }

    let mut response = request.call().map_err(|e| format!("GET {url}: {e}"))?;
    match response.status().as_u16() {
        200 => response
            .body_mut()
            .read_json::<T>()
            .map(Some)
            .map_err(|e| format!("GET {url}: unexpected response body: {e}")),
        404 => Ok(None),
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
