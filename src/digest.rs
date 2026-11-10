//! Decides what is news: which releases have not been posted yet, which of
//! their items are new, and how they are grouped into messages.

use std::collections::{HashMap, HashSet};

use crate::github::Release;
use crate::parse::{Item, parse_body};
use crate::state::State;

/// Lines shown per section; the rest are summed up as "and N more".
pub const MAX_LINES_PER_SECTION: usize = 12;
/// How many look-alike lines it takes to fold them into one.
const MIN_GROUP_TO_COLLAPSE: usize = 3;
/// Words compared at each end of a line to call two lines look-alikes.
const SHAPE_EDGE_WORDS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Game,
    Launcher,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub pr: Option<u32>,
    /// Look-alike lines folded into this one.
    pub similar: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSection {
    /// Heading as written in the release.
    pub title: String,
    pub lines: Vec<Line>,
    /// Changes left out by the per-section cap.
    pub hidden: usize,
}

/// One Telegram post: a game release, or all new launcher releases together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: Kind,
    /// Version label, for example `Nightly 2026-11-08 · fdc87ae` or `0.2.56 – 0.2.58`.
    pub label: String,
    pub url: String,
    pub sections: Vec<BlockSection>,
    pub release_keys: Vec<String>,
    pub item_keys: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub blocks: Vec<Block>,
    /// Releases to remember without posting: nothing new in them, or they
    /// predate the bot's first run.
    pub silent_releases: Vec<String>,
}

pub fn plan(releases: &[Release], state: &State) -> Plan {
    let fresh = fresh_releases(releases, state);
    let (to_post, to_skip) = if state.initialized {
        (fresh, Vec::new())
    } else {
        first_run_split(fresh)
    };

    let mut seen_items: HashSet<String> = state.seen_items.iter().cloned().collect();
    let mut plan = Plan {
        blocks: Vec::new(),
        silent_releases: to_skip.iter().map(|r| release_key(r)).collect(),
    };

    let (launcher, game): (Vec<&Release>, Vec<&Release>) = to_post
        .into_iter()
        .partition(|r| kind_of(r) == Kind::Launcher);
    let groups = game
        .into_iter()
        .map(|release| (Kind::Game, vec![release]))
        .chain((!launcher.is_empty()).then_some((Kind::Launcher, launcher)));

    for (kind, group) in groups {
        let keys: Vec<String> = group.iter().map(|r| release_key(r)).collect();
        match build_block(kind, &group, &mut seen_items) {
            Some(block) => plan.blocks.push(block),
            None => plan.silent_releases.extend(keys),
        }
    }
    plan
}

/// Published releases the bot has not handled, oldest first, one per key.
fn fresh_releases<'a>(releases: &'a [Release], state: &State) -> Vec<&'a Release> {
    let seen: HashSet<&str> = state.seen_releases.iter().map(String::as_str).collect();
    let mut taken: HashSet<String> = HashSet::new();
    let mut fresh: Vec<&Release> = releases
        .iter()
        .filter(|r| !r.draft)
        .filter(|r| !seen.contains(release_key(r).as_str()))
        .filter(|r| taken.insert(release_key(r)))
        .collect();
    fresh.sort_by(|a, b| a.published_at.cmp(&b.published_at));
    fresh
}

/// On the very first run only the newest game release is posted, as a
/// sample. Everything older is history, not news.
fn first_run_split(fresh: Vec<&Release>) -> (Vec<&Release>, Vec<&Release>) {
    let newest_game = fresh.iter().rposition(|r| kind_of(r) == Kind::Game);
    let mut to_post = Vec::new();
    let mut to_skip = Vec::new();
    for (index, release) in fresh.into_iter().enumerate() {
        if Some(index) == newest_game {
            to_post.push(release);
        } else {
            to_skip.push(release);
        }
    }
    (to_post, to_skip)
}

pub fn kind_of(release: &Release) -> Kind {
    if release.tag.starts_with("launcher@") {
        Kind::Launcher
    } else {
        Kind::Game
    }
}

/// The moving `nightly` release and its dated archive copy share a name, so
/// the name identifies a release better than the tag does.
pub fn release_key(release: &Release) -> String {
    match release.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => release.tag.clone(),
    }
}

fn build_block(kind: Kind, group: &[&Release], seen_items: &mut HashSet<String>) -> Option<Block> {
    let mut merged: Vec<(String, Vec<Item>)> = Vec::new();
    let mut item_keys = Vec::new();

    for release in group {
        for section in parse_body(release.body.as_deref().unwrap_or_default()) {
            let new_items: Vec<Item> = section
                .items
                .into_iter()
                .filter(|item| seen_items.insert(item.key.clone()))
                .collect();
            item_keys.extend(new_items.iter().map(|item| item.key.clone()));
            match merged.iter_mut().find(|(title, _)| *title == section.title) {
                Some((_, items)) => items.extend(new_items),
                None => merged.push((section.title, new_items)),
            }
        }
    }

    let sections: Vec<BlockSection> = merged
        .into_iter()
        .filter(|(_, items)| !items.is_empty())
        .map(|(title, items)| capped(title, collapse(items)))
        .collect();
    if sections.is_empty() {
        return None;
    }

    let newest = group.last()?;
    Some(Block {
        kind,
        label: label(kind, group),
        url: newest.url.clone(),
        sections,
        release_keys: group.iter().map(|r| release_key(r)).collect(),
        item_keys,
    })
}

fn label(kind: Kind, group: &[&Release]) -> String {
    let version = |release: &Release| match kind {
        Kind::Launcher => release.tag.trim_start_matches("launcher@").to_string(),
        Kind::Game if release.tag.starts_with("nightly") => release_key(release),
        Kind::Game => release.tag.clone(),
    };
    match (group.first(), group.last()) {
        (Some(first), Some(last)) if group.len() > 1 => {
            format!("{} – {}", version(first), version(last))
        }
        (_, Some(last)) => version(last),
        _ => String::new(),
    }
}

/// Folds runs of look-alike lines (the same change repeated for thirty
/// languages) into the first of them.
fn collapse(items: Vec<Item>) -> Vec<Line> {
    let shapes: Vec<String> = items.iter().map(|item| shape(&item.text)).collect();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for shape in &shapes {
        *counts.entry(shape.as_str()).or_default() += 1;
    }

    let mut emitted: HashSet<&str> = HashSet::new();
    let mut lines = Vec::new();
    for (item, shape) in items.iter().zip(&shapes) {
        let count = counts[shape.as_str()];
        let folded = count >= MIN_GROUP_TO_COLLAPSE;
        if folded && !emitted.insert(shape.as_str()) {
            continue;
        }
        lines.push(Line {
            text: item.text.clone(),
            pr: item.pr,
            similar: if folded { count - 1 } else { 0 },
        });
    }
    lines
}

/// What a line looks like with its numbers blanked and its middle removed.
/// Short lines keep every word, so only near-identical ones match.
fn shape(text: &str) -> String {
    // A whole number becomes one '#', so 84 and 155 look the same.
    let mut blanked = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            c if !c.is_ascii_digit() => blanked.push(c),
            _ if !blanked.ends_with('#') => blanked.push('#'),
            _ => {}
        }
    }
    let words: Vec<&str> = blanked.split_whitespace().collect();
    if words.len() <= SHAPE_EDGE_WORDS * 2 {
        return words.join(" ");
    }
    let head = &words[..SHAPE_EDGE_WORDS];
    let tail = &words[words.len() - SHAPE_EDGE_WORDS..];
    format!("{} … {}", head.join(" "), tail.join(" "))
}

fn capped(title: String, lines: Vec<Line>) -> BlockSection {
    let hidden = lines
        .iter()
        .skip(MAX_LINES_PER_SECTION)
        .map(|line| 1 + line.similar)
        .sum();
    BlockSection {
        title,
        lines: lines.into_iter().take(MAX_LINES_PER_SECTION).collect(),
        hidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, name: &str, published_at: &str, body: &str) -> Release {
        Release {
            tag: tag.to_string(),
            name: Some(name.to_string()),
            body: Some(body.to_string()),
            url: format!("https://github.com/horizon-rust/horizon/releases/tag/{tag}"),
            published_at: Some(published_at.to_string()),
            draft: false,
        }
    }

    fn fixture() -> Vec<Release> {
        serde_json::from_str(include_str!("../tests/fixtures/releases.json")).unwrap()
    }

    fn running() -> State {
        State::default().with_seen([], [])
    }

    fn texts(section: &BlockSection) -> Vec<&str> {
        section
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect()
    }

    #[test]
    fn first_run_posts_only_the_newest_game_release() {
        let plan = plan(&fixture(), &State::default());

        assert_eq!(plan.blocks.len(), 1);
        assert_eq!(plan.blocks[0].kind, Kind::Game);
        assert_eq!(plan.blocks[0].label, "Nightly 2026-11-08 · fdc87ae");
        assert_eq!(plan.silent_releases.len(), 11);
        assert!(
            plan.silent_releases
                .contains(&"Horizon launcher 0.2.58".to_string())
        );
    }

    #[test]
    fn posted_releases_are_not_news_again() {
        let releases = fixture();
        let first = plan(&releases, &State::default());
        let block = &first.blocks[0];
        let state = State::default()
            .with_seen(first.silent_releases.clone(), [])
            .with_seen(block.release_keys.clone(), block.item_keys.clone());

        assert_eq!(plan(&releases, &state), Plan::default());
    }

    #[test]
    fn moving_nightly_and_its_dated_copy_are_one_release() {
        let body = "### New\n- A thing (#1)";
        let releases = [
            release(
                "nightly",
                "Nightly 2026-11-08 · abc",
                "2026-11-08T15:00:00Z",
                body,
            ),
            release(
                "nightly-2026-11-08",
                "Nightly 2026-11-08 · abc",
                "2026-11-08T15:00:00Z",
                body,
            ),
        ];
        let plan = plan(&releases, &running());
        assert_eq!(plan.blocks.len(), 1);
        assert_eq!(plan.blocks[0].release_keys, ["Nightly 2026-11-08 · abc"]);
    }

    #[test]
    fn drafts_are_never_news() {
        let mut draft = release(
            "nightly",
            "Nightly X",
            "2026-11-08T15:00:00Z",
            "### New\n- A thing (#1)",
        );
        draft.draft = true;
        assert_eq!(plan(&[draft], &running()), Plan::default());
    }

    #[test]
    fn game_releases_are_posted_oldest_first_each_in_its_own_block() {
        let releases = [
            release(
                "nightly",
                "Nightly B",
                "2026-11-09T10:00:00Z",
                "### New\n- Second (#2)",
            ),
            release(
                "nightly-a",
                "Nightly A",
                "2026-11-08T10:00:00Z",
                "### New\n- First (#1)",
            ),
        ];
        let plan = plan(&releases, &running());
        let labels: Vec<&str> = plan.blocks.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["Nightly A", "Nightly B"]);
    }

    #[test]
    fn launcher_releases_merge_into_one_block_after_the_game() {
        let releases = [
            release(
                "launcher@0.2.2",
                "Horizon launcher 0.2.2",
                "2026-11-09T12:00:00Z",
                "### Fixed\n- Two (#2)",
            ),
            release(
                "nightly",
                "Nightly A",
                "2026-11-09T11:00:00Z",
                "### New\n- Game (#3)",
            ),
            release(
                "launcher@0.2.1",
                "Horizon launcher 0.2.1",
                "2026-11-09T10:00:00Z",
                "### Fixed\n- One (#1)",
            ),
        ];
        let plan = plan(&releases, &running());

        assert_eq!(plan.blocks.len(), 2);
        assert_eq!(plan.blocks[0].kind, Kind::Game);
        let launcher = &plan.blocks[1];
        assert_eq!(launcher.kind, Kind::Launcher);
        assert_eq!(launcher.label, "0.2.1 – 0.2.2");
        assert!(launcher.url.ends_with("launcher@0.2.2"));
        assert_eq!(texts(&launcher.sections[0]), ["One", "Two"]);
        assert_eq!(launcher.release_keys.len(), 2);
    }

    #[test]
    fn an_item_already_posted_is_left_out() {
        let releases = [release(
            "nightly",
            "Nightly A",
            "2026-11-09T11:00:00Z",
            "### Fixed\n- Old (#1)\n- Fresh (#2)",
        )];
        let state = running().with_seen([], ["#1".to_string()]);
        let plan = plan(&releases, &state);
        assert_eq!(texts(&plan.blocks[0].sections[0]), ["Fresh"]);
        assert_eq!(plan.blocks[0].item_keys, ["#2"]);
    }

    #[test]
    fn the_same_item_in_two_new_releases_is_posted_once() {
        let releases = [
            release(
                "nightly-a",
                "Nightly A",
                "2026-11-08T10:00:00Z",
                "### New\n- Thing (#1)",
            ),
            release(
                "nightly",
                "Nightly B",
                "2026-11-09T10:00:00Z",
                "### New\n- Thing (#1)\n- Other (#2)",
            ),
        ];
        let plan = plan(&releases, &running());
        assert_eq!(texts(&plan.blocks[0].sections[0]), ["Thing"]);
        assert_eq!(texts(&plan.blocks[1].sections[0]), ["Other"]);
    }

    #[test]
    fn a_release_with_nothing_new_is_remembered_silently() {
        let releases = [release(
            "nightly",
            "Nightly A",
            "2026-11-09T11:00:00Z",
            "### Fixed\n- Old (#1)",
        )];
        let state = running().with_seen([], ["#1".to_string()]);
        let plan = plan(&releases, &state);
        assert!(plan.blocks.is_empty());
        assert_eq!(plan.silent_releases, ["Nightly A"]);
    }

    #[test]
    fn real_launcher_release_folds_the_language_lines() {
        let launcher: Vec<Release> = fixture()
            .into_iter()
            .filter(|r| r.tag == "launcher@0.2.58")
            .collect();
        let plan = plan(&launcher, &running());
        let new = &plan.blocks[0].sections[0];

        assert_eq!(new.title, "New");
        let folded: Vec<&Line> = new.lines.iter().filter(|line| line.similar > 0).collect();
        assert_eq!(folded.len(), 1);
        assert!(folded[0].text.contains("still shows in English"));
        assert!(folded[0].similar >= 10);
        assert!(
            new.lines.len() <= 4,
            "three real changes plus the folded line"
        );
        // Every folded change is still recorded, so none is posted later.
        assert!(plan.blocks[0].item_keys.len() > new.lines.len() + folded[0].similar - 1);
    }

    #[test]
    fn two_look_alike_lines_are_not_folded() {
        let items = [
            "Launcher: the 5 strings Kazakh still shows in English",
            "Launcher: the 7 strings Latin still shows in English",
        ]
        .map(|text| Item {
            text: text.to_string(),
            pr: None,
            key: text.to_string(),
        });
        let lines = collapse(items.to_vec());
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.similar == 0));
    }

    #[test]
    fn unrelated_lines_keep_their_own_shape() {
        assert_ne!(
            shape("Portals: entities past the simulation distance neither cross nor cool down"),
            shape("Portals: entities in unloaded chunks neither cross nor cool down")
        );
        assert_eq!(
            shape("Launcher: the 155 strings Kazakh still shows in English"),
            shape("Launcher: the 210 strings West Frisian still shows in English")
        );
    }

    #[test]
    fn long_sections_are_capped_and_count_what_they_hide() {
        // Look-alike lines fold first, so every line gets its own subject.
        const SUBJECTS: [&str; 20] = [
            "Boats", "Signs", "Portals", "Chunks", "Mobs", "Maps", "Beds", "Books", "Doors",
            "Rails", "Crops", "Torches", "Chests", "Anvils", "Banners", "Buckets", "Arrows",
            "Shields", "Saddles", "Bells",
        ];
        let body: String = SUBJECTS
            .iter()
            .zip(1..)
            .fold("### Fixed\n".to_string(), |body, (subject, n)| {
                format!("{body}- {subject} behave as vanilla (#{n})\n")
            });
        let releases = [release(
            "nightly",
            "Nightly A",
            "2026-11-09T11:00:00Z",
            &body,
        )];
        let plan = plan(&releases, &running());
        let section = &plan.blocks[0].sections[0];

        assert_eq!(section.lines.len(), MAX_LINES_PER_SECTION);
        assert_eq!(section.hidden, 20 - MAX_LINES_PER_SECTION);
        assert_eq!(plan.blocks[0].item_keys.len(), 20);
    }
}
