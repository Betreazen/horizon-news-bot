//! Renders a block as a Telegram HTML message and sends it.

use std::thread::sleep;
use std::time::Duration;

use serde_json::{Value, json};
use ureq::Agent;

use crate::digest::{Block, BlockSection, Kind, Line};

/// Telegram allows 4096 characters after formatting. Counting the markup
/// too leaves a wide margin.
pub const MESSAGE_LIMIT: usize = 3800;
const MAX_ATTEMPTS: u32 = 4;
const MAX_RETRY_WAIT_SECS: u64 = 60;
const SOURCE_REPO_URL: &str = "https://github.com/horizon-rust/horizon";

pub fn render(block: &Block) -> String {
    let title = match block.kind {
        Kind::Game => format!("🎮 Horizon {}", block.label),
        Kind::Launcher => format!("🚀 Лаунчер Horizon {}", block.label),
    };
    let mut parts = vec![format!("<b>{}</b>", escape(&title))];
    parts.extend(block.sections.iter().map(render_section));
    parts.push(format!(
        "<a href=\"{}\">Релиз на GitHub</a>",
        escape(&block.url)
    ));
    parts.join("\n\n")
}

fn render_section(section: &BlockSection) -> String {
    let mut lines = vec![format!("<b>{}</b>", escape(section_title(&section.title)))];
    lines.extend(section.lines.iter().map(render_line));
    if section.hidden > 0 {
        lines.push(format!(
            "<i>…и ещё {} {} — в релизе по ссылке ниже</i>",
            section.hidden,
            plural(section.hidden, "изменение", "изменения", "изменений")
        ));
    }
    lines.join("\n")
}

fn render_line(line: &Line) -> String {
    let mut text = format!("• {}", escape(&line.text));
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

/// Splits a message at line breaks so each part fits the limit. Every line
/// closes the tags it opens, so any line break is a safe place to cut.
pub fn split(text: &str, limit: usize) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;
    for line in text.split('\n') {
        let line_len = line.chars().count();
        if current_len > 0 && current_len + 1 + line_len > limit {
            parts.push(std::mem::take(&mut current));
            current_len = 0;
        }
        if current_len > 0 {
            current.push('\n');
            current_len += 1;
        }
        current.push_str(line);
        current_len += line_len;
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    parts
        .into_iter()
        .map(|part| part.trim_matches('\n').to_string())
        .collect()
}

/// Sends one block, in as many messages as its length needs.
pub fn send_block(agent: &Agent, token: &str, chat_id: &str, block: &Block) -> Result<(), String> {
    split(&render(block), MESSAGE_LIMIT)
        .iter()
        .try_for_each(|part| send(agent, token, chat_id, part))
}

fn send(agent: &Agent, token: &str, chat_id: &str, text: &str) -> Result<(), String> {
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
        let description = reply["description"].as_str().unwrap_or("no description");
        return Err(format!(
            "Telegram refused the message: HTTP {status}: {description}"
        ));
    }
    Err(format!(
        "Telegram kept rate-limiting after {MAX_ATTEMPTS} attempts"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, pr: Option<u32>, similar: usize) -> Line {
        Line {
            text: text.to_string(),
            pr,
            similar,
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
                },
                BlockSection {
                    title: "Fixed".to_string(),
                    lines: vec![
                        line("Порталы: a < b & c", None, 0),
                        line("Строки перевода", Some(5), 29),
                    ],
                    hidden: 3,
                },
            ],
            release_keys: Vec::new(),
            item_keys: Vec::new(),
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
    fn a_short_message_is_not_split() {
        assert_eq!(split("one\n\ntwo", 100), ["one\n\ntwo"]);
    }

    #[test]
    fn a_long_message_is_split_at_line_breaks_within_the_limit() {
        let text = (1..=10)
            .map(|n| format!("line number {n:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let parts = split(&text, 45);

        assert!(parts.len() > 1);
        assert!(parts.iter().all(|part| part.chars().count() <= 45));
        assert_eq!(parts.join("\n"), text);
    }

    #[test]
    fn parts_do_not_start_or_end_with_blank_lines() {
        let parts = split("aaaa\n\nbbbb\n\ncccc", 6);
        assert_eq!(parts, ["aaaa", "bbbb", "cccc"]);
    }

    #[test]
    fn the_limit_counts_characters_not_bytes() {
        let text = "привет\nпривет";
        assert_eq!(split(text, 13), [text]);
    }
}
