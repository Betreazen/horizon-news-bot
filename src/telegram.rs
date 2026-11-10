//! Renders a block as Telegram HTML messages and sends them.

use std::thread::sleep;
use std::time::Duration;

use serde_json::{Value, json};
use ureq::Agent;

use crate::digest::{Block, Kind, Line};
use crate::parse::clip;

/// Telegram allows 4096 characters after formatting. Counting the markup
/// too leaves a wide margin.
pub const MESSAGE_LIMIT: usize = 3800;
/// Caps on text taken from releases and from the model. With them no
/// rendered line can outgrow a message, even if every character needs
/// escaping, so a single odd release cannot jam the queue.
const MAX_LINE_CHARS: usize = 500;
const MAX_LABEL_CHARS: usize = 120;
const MAX_ATTEMPTS: u32 = 4;
const MAX_RETRY_WAIT_SECS: u64 = 60;
const SERVER_ERROR_WAIT: Duration = Duration::from_secs(3);
const SOURCE_REPO_URL: &str = "https://github.com/horizon-rust/horizon";

/// One Telegram message, with the changes it reports. Once the message is
/// delivered those changes count as posted, so a block that is cut off
/// halfway is continued by the next run, not started over.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Part {
    pub text: String,
    pub item_keys: Vec<String>,
}

/// One rendered line of a message and the changes it stands for.
struct Row {
    text: String,
    keys: Vec<String>,
}

impl Row {
    fn plain(text: String) -> Self {
        Self {
            text,
            keys: Vec::new(),
        }
    }
}

/// The whole block as one text: what the parts add up to.
#[cfg(test)]
pub fn render(block: &Block) -> String {
    let texts: Vec<String> = rows(block).into_iter().map(|row| row.text).collect();
    texts.join("\n")
}

/// The block as messages that each fit `limit`. Messages are cut between
/// lines, and every line closes the tags it opens, so each part is whole
/// HTML.
pub fn parts(block: &Block, limit: usize) -> Vec<Part> {
    let mut parts: Vec<Part> = Vec::new();
    let mut current = Part::default();
    let mut current_len = 0;

    for row in rows(block) {
        let row_len = row.text.chars().count();
        if current_len > 0 && current_len + 1 + row_len > limit {
            parts.push(std::mem::take(&mut current));
            current_len = 0;
        }
        // A message does not open with a blank line.
        if current_len == 0 && row.text.is_empty() {
            continue;
        }
        if current_len > 0 {
            current.text.push('\n');
            current_len += 1;
        }
        current.text.push_str(&row.text);
        current_len += row_len;
        current.item_keys.extend(row.keys);
    }
    if current_len > 0 {
        parts.push(current);
    }
    parts
        .into_iter()
        .map(|part| Part {
            text: part.text.trim_end_matches('\n').to_string(),
            ..part
        })
        .collect()
}

fn rows(block: &Block) -> Vec<Row> {
    let label = clip(&block.label, MAX_LABEL_CHARS);
    let title = match block.kind {
        Kind::Game => format!("🎮 Horizon {label}"),
        Kind::Launcher => format!("🚀 Лаунчер Horizon {label}"),
    };

    let mut rows = vec![Row::plain(format!("<b>{}</b>", escape(&title)))];
    for section in &block.sections {
        rows.push(Row::plain(String::new()));
        rows.push(Row::plain(format!(
            "<b>{}</b>",
            escape(section_title(&section.title))
        )));
        rows.extend(section.lines.iter().map(|line| Row {
            text: render_line(line),
            keys: line.keys.clone(),
        }));
        if section.hidden > 0 {
            rows.push(Row {
                text: format!(
                    "<i>…и ещё {} {} — в релизе по ссылке ниже</i>",
                    section.hidden,
                    plural(section.hidden, "изменение", "изменения", "изменений")
                ),
                keys: section.hidden_keys.clone(),
            });
        }
    }
    rows.push(Row::plain(String::new()));
    rows.push(Row::plain(format!(
        "<a href=\"{}\">Релиз на GitHub</a>",
        escape(&block.url)
    )));
    rows
}

fn render_line(line: &Line) -> String {
    let mut text = format!("• {}", escape(&clip(&line.text, MAX_LINE_CHARS)));
    if let Some(pr) = line.pr {
        text.push_str(&format!(
            " (<a href=\"{SOURCE_REPO_URL}/pull/{pr}\">#{pr}</a>)"
        ));
    }
    if line.similar > 0 {
        text.push_str(&format!(
            " <i>и ещё {} {}</i>",
            line.similar,
            plural(line.similar, "похожее", "похожих", "похожих")
        ));
    }
    text
}

fn section_title(title: &str) -> &str {
    match title.to_ascii_lowercase().as_str() {
        "new" | "added" => "Новое",
        "fixed" | "fixes" => "Исправлено",
        "changed" => "Изменено",
        "removed" => "Удалено",
        "performance" => "Производительность",
        "known issues" => "Известные проблемы",
        _ => title,
    }
}

/// Russian plural form for a count: 1 изменение, 3 изменения, 5 изменений.
fn plural<'a>(count: usize, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    match (count % 10, count % 100) {
        (_, 11..=14) => many,
        (1, _) => one,
        (2..=4, _) => few,
        _ => many,
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Sends one message. Rate limits and Telegram's own failures are retried;
/// a network error is not, because the message may have been delivered.
pub fn send(agent: &Agent, token: &str, chat_id: &str, text: &str) -> Result<(), String> {
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    let body = json!({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "HTML",
        "link_preview_options": { "is_disabled": true },
    });

    for _ in 0..MAX_ATTEMPTS {
        // The URL carries the bot token, so it must never reach the log.
        let mut response = agent.post(&url).send_json(&body).map_err(|e| {
            format!(
                "Telegram request failed: {}",
                e.to_string().replace(token, "<token>")
            )
        })?;
        let status = response.status().as_u16();
        if status == 200 {
            return Ok(());
        }
        let reply: Value = response.body_mut().read_json().unwrap_or(Value::Null);
        if status == 429 {
            let wait = reply["parameters"]["retry_after"].as_u64().unwrap_or(5);
            sleep(Duration::from_secs(wait.min(MAX_RETRY_WAIT_SECS)));
            continue;
        }
        // Telegram answered that it failed, so nothing was delivered and a
        // retry cannot duplicate the message.
        if status >= 500 {
            sleep(SERVER_ERROR_WAIT);
            continue;
        }
        let description = reply["description"].as_str().unwrap_or("no description");
        return Err(format!(
            "Telegram refused the message: HTTP {status}: {description}"
        ));
    }
    Err(format!(
        "Telegram kept refusing or failing after {MAX_ATTEMPTS} attempts"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::BlockSection;

    fn line(text: &str, pr: Option<u32>, similar: usize) -> Line {
        Line {
            text: text.to_string(),
            pr,
            similar,
            keys: vec![format!("key of {text}")],
        }
    }

    fn block() -> Block {
        Block {
            kind: Kind::Game,
            label: "Nightly 2026-11-08 · fdc87ae".to_string(),
            url: "https://github.com/horizon-rust/horizon/releases/tag/nightly".to_string(),
            sections: vec![
                BlockSection {
                    title: "New".to_string(),
                    lines: vec![line("Магазин лаунчера", Some(11894), 0)],
                    hidden: 0,
                    hidden_keys: Vec::new(),
                },
                BlockSection {
                    title: "Fixed".to_string(),
                    lines: vec![
                        line("Порталы: a < b & c", None, 0),
                        line("Строки перевода", Some(5), 29),
                    ],
                    hidden: 3,
                    hidden_keys: vec!["hidden 1".to_string(), "hidden 2".to_string()],
                },
            ],
            release_keys: Vec::new(),
        }
    }

    /// A block of `count` numbered lines, each about 40 characters.
    fn long_block(count: usize) -> Block {
        let lines = (1..=count)
            .map(|n| line(&format!("Изменение номер {n:03} в длинной сводке"), None, 0))
            .collect();
        Block {
            sections: vec![BlockSection {
                title: "Fixed".to_string(),
                lines,
                hidden: 0,
                hidden_keys: Vec::new(),
            }],
            ..block()
        }
    }

    #[test]
    fn a_block_renders_as_one_html_message() {
        let expected = "\
<b>🎮 Horizon Nightly 2026-11-08 · fdc87ae</b>

<b>Новое</b>
• Магазин лаунчера (<a href=\"https://github.com/horizon-rust/horizon/pull/11894\">#11894</a>)

<b>Исправлено</b>
• Порталы: a &lt; b &amp; c
• Строки перевода (<a href=\"https://github.com/horizon-rust/horizon/pull/5\">#5</a>) <i>и ещё 29 похожих</i>
<i>…и ещё 3 изменения — в релизе по ссылке ниже</i>

<a href=\"https://github.com/horizon-rust/horizon/releases/tag/nightly\">Релиз на GitHub</a>";
        assert_eq!(render(&block()), expected);
    }

    #[test]
    fn a_short_block_is_one_part_that_carries_every_key() {
        let parts = parts(&block(), MESSAGE_LIMIT);

        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].text, render(&block()));
        assert_eq!(
            parts[0].item_keys,
            [
                "key of Магазин лаунчера",
                "key of Порталы: a < b & c",
                "key of Строки перевода",
                "hidden 1",
                "hidden 2"
            ]
        );
    }

    #[test]
    fn a_long_block_is_cut_between_lines_within_the_limit() {
        let block = long_block(60);
        let limit = 600;
        let parts = parts(&block, limit);

        assert!(parts.len() > 3);
        assert!(parts.iter().all(|part| part.text.chars().count() <= limit));
        assert!(
            parts
                .iter()
                .all(|part| !part.text.starts_with('\n') && !part.text.ends_with('\n'))
        );
        // Nothing is lost and nothing is repeated: the parts are the block.
        let joined: Vec<&str> = parts.iter().flat_map(|part| part.text.lines()).collect();
        let whole = render(&block);
        let expected: Vec<&str> = whole.lines().filter(|l| !l.is_empty()).collect();
        let joined: Vec<&str> = joined.into_iter().filter(|l| !l.is_empty()).collect();
        assert_eq!(joined, expected);
    }

    #[test]
    fn each_part_carries_exactly_the_keys_of_its_own_lines() {
        let block = long_block(60);
        let parts = parts(&block, 600);

        for part in &parts {
            let shown = part.text.lines().filter(|l| l.starts_with('•')).count();
            assert_eq!(part.item_keys.len(), shown);
            for key in &part.item_keys {
                let text = key.trim_start_matches("key of ");
                assert!(part.text.contains(text), "{key} is not in its part");
            }
        }
        let all: Vec<&String> = parts.iter().flat_map(|part| &part.item_keys).collect();
        assert_eq!(all.len(), 60);
    }

    /// The worst a release can do: every text at its cap and made only of
    /// characters that grow sixfold when escaped.
    #[test]
    fn no_line_of_the_worst_block_outgrows_a_message() {
        let hostile = "\"".repeat(100_000);
        let worst = Block {
            label: hostile.clone(),
            sections: vec![BlockSection {
                title: "\"".repeat(60),
                lines: vec![line(&hostile, Some(u32::MAX), usize::MAX); 12],
                hidden: usize::MAX,
                hidden_keys: Vec::new(),
            }],
            ..block()
        };

        let rendered = render(&worst);
        let longest = rendered.lines().map(|l| l.chars().count()).max().unwrap();
        assert!(longest <= MESSAGE_LIMIT, "longest line is {longest}");

        let parts = parts(&worst, MESSAGE_LIMIT);
        assert!(parts.len() > 1);
        assert!(
            parts
                .iter()
                .all(|part| part.text.chars().count() <= MESSAGE_LIMIT)
        );
        // Every part is whole HTML: each tag it opens, it closes.
        for part in &parts {
            for tag in ["b", "i", "a"] {
                let opened = part.text.matches(&format!("<{tag}")).count();
                let closed = part.text.matches(&format!("</{tag}>")).count();
                assert_eq!(opened, closed, "<{tag}> in a part");
            }
        }
    }

    #[test]
    fn a_launcher_block_has_its_own_title() {
        let launcher = Block {
            kind: Kind::Launcher,
            label: "0.2.56 – 0.2.58".to_string(),
            ..block()
        };
        assert!(render(&launcher).starts_with("<b>🚀 Лаунчер Horizon 0.2.56 – 0.2.58</b>"));
    }

    #[test]
    fn unknown_section_titles_are_kept_as_written() {
        assert_eq!(section_title("Fixed"), "Исправлено");
        assert_eq!(section_title("KNOWN ISSUES"), "Известные проблемы");
        assert_eq!(section_title("Security"), "Security");
    }

    #[test]
    fn plural_follows_russian_rules() {
        let form = |n| plural(n, "изменение", "изменения", "изменений");
        assert_eq!(
            [form(1), form(2), form(5), form(11), form(21), form(112)],
            [
                "изменение",
                "изменения",
                "изменений",
                "изменений",
                "изменение",
                "изменений"
            ]
        );
    }

    #[test]
    fn the_limit_counts_characters_not_bytes() {
        // Cyrillic takes two bytes a character; a byte count would split this.
        let block = long_block(3);
        let whole = render(&block);
        let exact = whole.chars().count();
        assert!(whole.len() > exact);
        assert_eq!(parts(&block, exact).len(), 1);
        assert!(parts(&block, exact - 1).len() > 1);
    }
}
