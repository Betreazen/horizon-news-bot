//! What the bot remembers between runs: which releases and changelog items
//! it has already posted.

use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};

const MAX_SEEN_RELEASES: usize = 400;
const MAX_SEEN_ITEMS: usize = 4000;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// False until the first run has recorded the releases that already existed.
    pub initialized: bool,
    pub seen_releases: Vec<String>,
    pub seen_items: Vec<String>,
}

impl State {
    /// A missing file is a first run. A broken file is an error: starting
    /// from scratch would post everything again.
    pub fn load(path: &Path) -> Result<Self, String> {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| format!("state file {} is not valid JSON: {e}", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("cannot read state file {}: {e}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let describe = |e: io::Error| format!("cannot write state file {}: {e}", path.display());
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).map_err(describe)?;
        }
        let mut text = serde_json::to_string_pretty(self)
            .map_err(|e| format!("cannot serialize state: {e}"))?;
        text.push('\n');
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text).map_err(describe)?;
        fs::rename(&tmp, path).map_err(describe)
    }

    /// Records releases and items as posted. Oldest entries fall off once a
    /// list outgrows its cap.
    #[must_use]
    pub fn with_seen(
        self,
        releases: impl IntoIterator<Item = String>,
        items: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            initialized: true,
            seen_releases: appended(self.seen_releases, releases, MAX_SEEN_RELEASES),
            seen_items: appended(self.seen_items, items, MAX_SEEN_ITEMS),
        }
    }
}

fn appended(
    mut list: Vec<String>,
    extra: impl IntoIterator<Item = String>,
    cap: usize,
) -> Vec<String> {
    for key in extra {
        if !list.contains(&key) {
            list.push(key);
        }
    }
    let overflow = list.len().saturating_sub(cap);
    list.drain(..overflow);
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(range: std::ops::Range<usize>) -> impl Iterator<Item = String> {
        range.map(|n| format!("#{n}"))
    }

    #[test]
    fn missing_file_loads_as_first_run() {
        let path = std::env::temp_dir().join("horizon-news-bot-no-such-state.json");
        let state = State::load(&path).unwrap();
        assert_eq!(state, State::default());
        assert!(!state.initialized);
    }

    #[test]
    fn broken_file_is_an_error_not_a_reset() {
        let path = std::env::temp_dir().join("horizon-news-bot-broken-state.json");
        fs::write(&path, "{ not json").unwrap();
        let result = State::load(&path);
        fs::remove_file(&path).unwrap();
        assert!(result.unwrap_err().contains("not valid JSON"));
    }

    #[test]
    fn saved_state_loads_back_unchanged() {
        let path = std::env::temp_dir().join("horizon-news-bot-roundtrip-state.json");
        let state = State::default().with_seen(["Nightly 1".to_string()], keys(1..4));
        state.save(&path).unwrap();
        let loaded = State::load(&path);
        fs::remove_file(&path).unwrap();
        assert_eq!(loaded.unwrap(), state);
    }

    #[test]
    fn fields_of_older_versions_are_ignored() {
        let json = r#"{"initialized":true,"etag_list":"\"abc\"","seen_releases":["a"]}"#;
        let state: State = serde_json::from_str(json).unwrap();
        assert!(state.initialized);
        assert_eq!(state.seen_releases, ["a"]);
    }

    #[test]
    fn recording_marks_the_state_initialized() {
        let state = State::default().with_seen([], []);
        assert!(state.initialized);
    }

    #[test]
    fn repeated_keys_are_stored_once() {
        let state = State::default()
            .with_seen([], keys(1..3))
            .with_seen([], keys(2..4));
        assert_eq!(state.seen_items, ["#1", "#2", "#3"]);
    }

    #[test]
    fn oldest_items_fall_off_past_the_cap() {
        let state = State::default().with_seen([], keys(0..MAX_SEEN_ITEMS + 5));
        assert_eq!(state.seen_items.len(), MAX_SEEN_ITEMS);
        assert_eq!(state.seen_items[0], "#5");
    }

    #[test]
    fn old_state_files_without_new_fields_still_load() {
        let state: State = serde_json::from_str(r#"{"seen_releases":["a"]}"#).unwrap();
        assert_eq!(state.seen_releases, ["a"]);
        assert!(state.seen_items.is_empty());
    }
}
