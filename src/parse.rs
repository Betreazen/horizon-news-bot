//! Turns the Markdown body of a Horizon release into sections of items.
//!
//! Only bullet lines under a `### Heading` count. Everything else in a
//! release body (download notes, thanks, disclaimers) is not news.

use std::collections::HashSet;

/// One changelog line, with its trailing `(#123, fixes #45)` taken off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub text: String,
    /// The pull request that made the change, when the line names one.
    pub pr: Option<u32>,
    /// Identity used to never post the same change twice.
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Heading as written in the release, for example `Fixed`.
    pub title: String,
    pub items: Vec<Item>,
}

/// Longest item text kept. Real changelog lines run to about 150
/// characters; the cap keeps one runaway line from flooding a message.
const MAX_ITEM_CHARS: usize = 300;
const MAX_TITLE_CHARS: usize = 60;

pub fn parse_body(body: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut used_keys: HashSet<String> = HashSet::new();
    let mut in_section = false;

    for line in body.lines().map(str::trim) {
        if let Some(title) = line.strip_prefix("### ") {
            sections.push(Section {
                title: clip(title.trim(), MAX_TITLE_CHARS),
                items: Vec::new(),
            });
            in_section = true;
        } else if line.starts_with('#') || is_rule(line) {
            // Any other heading, or a horizontal rule, ends the section, so
            // a bullet list under "## Downloads" is not taken for changes.
            in_section = false;
        } else if let Some(bullet) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            let item = parse_item(bullet).and_then(|item| unique(item, &mut used_keys));
            if let (true, Some(section), Some(item)) = (in_section, sections.last_mut(), item) {
                section.items.push(item);
            }
        }
    }
    sections.retain(|section| !section.items.is_empty());
    sections
}

fn is_rule(line: &str) -> bool {
    line.len() >= 3
        && ['-', '*', '_']
            .iter()
            .any(|mark| line.chars().all(|c| c == *mark))
}

fn parse_item(bullet: &str) -> Option<Item> {
    let (text, pr) = split_refs(bullet.trim());
    if text.is_empty() {
        return None;
    }
    let key = match pr {
        Some(number) => format!("#{number}"),
        None => format!("t:{:016x}", fnv1a(&text.to_lowercase())),
    };
    Some(Item {
        text: clip(text, MAX_ITEM_CHARS),
        pr,
        key,
    })
}

/// Two different bullets may name the same pull request. The first keeps
/// the plain key; a later one with other text gets a key of its own, and an
/// exact repeat is dropped.
fn unique(item: Item, used_keys: &mut HashSet<String>) -> Option<Item> {
    let text_hash = fnv1a(&item.text.to_lowercase()) & 0xffff_ffff;
    let own_key = format!("{}:{text_hash:08x}", item.key);
    if used_keys.insert(item.key.clone()) {
        used_keys.insert(own_key);
        return Some(item);
    }
    used_keys.insert(own_key.clone()).then_some(Item {
        key: own_key,
        ..item
    })
}

/// Shortens text to `max` characters, marking the cut with an ellipsis.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// Strips trailing parenthesised groups that carry `#number` references and
/// returns the pull request of the last one. Groups without a reference,
/// such as `(3458 signs: 124 → 222 fps)`, stay in the text.
fn split_refs(line: &str) -> (&str, Option<u32>) {
    let mut text = line;
    let mut pr = None;
    while let Some(start) = trailing_group_start(text) {
        let group = &text[start..];
        let Some(number) = first_ref(group) else {
            break;
        };
        pr = pr.or(Some(number));
        text = text[..start].trim_end();
    }
    (text, pr)
}

fn trailing_group_start(text: &str) -> Option<usize> {
    if !text.ends_with(')') {
        return None;
    }
    let start = text.rfind('(')?;
    // A nested or unbalanced group is left alone.
    (text[start..].matches(')').count() == 1).then_some(start)
}

fn first_ref(group: &str) -> Option<u32> {
    group.split('#').skip(1).find_map(|rest| {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    })
}

fn fnv1a(text: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    text.bytes().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NIGHTLY: &str = include_str!("../tests/fixtures/nightly.md");
    const LAUNCHER: &str = include_str!("../tests/fixtures/launcher.md");
    const STABLE: &str = include_str!("../tests/fixtures/stable.md");

    fn item(line: &str) -> Item {
        parse_item(line).unwrap()
    }

    #[test]
    fn nightly_release_gives_new_and_fixed_sections() {
        let sections = parse_body(NIGHTLY);
        let titles: Vec<&str> = sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["New", "Fixed"]);
        assert_eq!(sections[0].items.len(), 1);
        assert_eq!(sections[1].items.len(), 2);
        assert_eq!(
            sections[0].items[0].text,
            "Launcher Shop: a game-style item shop with the items drawn"
        );
        assert_eq!(sections[0].items[0].pr, Some(11894));
    }

    #[test]
    fn text_outside_headed_sections_is_ignored() {
        let sections = parse_body(NIGHTLY);
        let all: Vec<&str> = sections
            .iter()
            .flat_map(|s| s.items.iter().map(|i| i.text.as_str()))
            .collect();
        assert!(all.iter().all(|text| !text.contains("Mojang")));
        assert!(all.iter().all(|text| !text.contains("launcher installs")));
    }

    #[test]
    fn sections_without_bullets_are_dropped() {
        let titles: Vec<String> = parse_body(LAUNCHER).into_iter().map(|s| s.title).collect();
        assert_eq!(titles, ["New", "Fixed"]);
    }

    #[test]
    fn known_issues_without_a_pr_get_a_text_key() {
        let sections = parse_body(STABLE);
        let known = sections.iter().find(|s| s.title == "Known issues").unwrap();
        assert_eq!(known.items.len(), 1);
        assert_eq!(known.items[0].pr, None);
        assert!(known.items[0].key.starts_with("t:"));
    }

    #[test]
    fn pr_is_the_first_number_of_the_reference_group() {
        let parsed = item("riding: the crosshair skips the mob being ridden (#3069, fixes #2870)");
        assert_eq!(
            parsed.text,
            "riding: the crosshair skips the mob being ridden"
        );
        assert_eq!(parsed.pr, Some(3069));
        assert_eq!(parsed.key, "#3069");
    }

    #[test]
    fn groups_without_a_reference_stay_in_the_text() {
        let parsed = item(
            "Signs: keep laid-out sign text between frames (3458 signs: 124 → 222 fps) (#3284, fixes #1461)",
        );
        assert_eq!(
            parsed.text,
            "Signs: keep laid-out sign text between frames (3458 signs: 124 → 222 fps)"
        );
        assert_eq!(parsed.pr, Some(3284));
    }

    #[test]
    fn with_two_reference_groups_the_last_one_names_the_pr() {
        let parsed = item(
            "Recipes: preserve the search footer when returning from a recipe (#11443, main merged) (#11773, fixes #11441)",
        );
        assert_eq!(
            parsed.text,
            "Recipes: preserve the search footer when returning from a recipe"
        );
        assert_eq!(parsed.pr, Some(11773));
    }

    #[test]
    fn same_text_without_a_pr_gets_the_same_key() {
        assert_eq!(
            item("Shader packs are gone").key,
            item("shader packs are GONE").key
        );
        assert_ne!(
            item("Shader packs are gone").key,
            item("Shader packs are back").key
        );
    }

    #[test]
    fn bullets_under_other_headings_are_not_changes() {
        let body = "### Fixed\n- Real fix (#1)\n\n## Downloads\n- file.zip\n\n#### Notes\n- a note";
        let sections = parse_body(body);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].items.len(), 1);
        assert_eq!(sections[0].items[0].text, "Real fix");
    }

    #[test]
    fn a_horizontal_rule_ends_the_section() {
        let sections = parse_body("### Fixed\n- Real fix (#1)\n\n---\n- Not a change");
        assert_eq!(sections[0].items.len(), 1);
    }

    #[test]
    fn two_bullets_for_one_pr_are_both_kept() {
        let sections = parse_body("### Fixed\n- Fix A (#100)\n- Fix B (#100)");
        let keys: Vec<&str> = sections[0].items.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], "#100");
        assert!(keys[1].starts_with("#100:"));
        assert_ne!(keys[0], keys[1]);
    }

    #[test]
    fn keys_are_the_same_on_every_parse() {
        let body = "### Fixed\n- Fix A (#100)\n- Fix B (#100)";
        assert_eq!(parse_body(body), parse_body(body));
    }

    #[test]
    fn an_exact_repeat_of_a_bullet_is_dropped() {
        let body = "### New\n- Fix A (#100)\n### Fixed\n- Fix A (#100)\n- Fix A (#100)";
        let sections = parse_body(body);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].items.len(), 1);
    }

    #[test]
    fn a_runaway_line_is_clipped() {
        let long = "word ".repeat(500);
        let parsed = item(&format!("{long}(#7)"));
        assert_eq!(parsed.text.chars().count(), MAX_ITEM_CHARS);
        assert!(parsed.text.ends_with('…'));
        assert_eq!(parsed.pr, Some(7));
    }

    #[test]
    fn clip_leaves_short_text_alone_and_counts_characters() {
        assert_eq!(clip("привет", 6), "привет");
        assert_eq!(clip("привет мир", 7), "привет…");
    }

    #[test]
    fn a_line_that_is_only_a_reference_is_not_an_item() {
        assert_eq!(parse_item("(#12)"), None);
    }
}
