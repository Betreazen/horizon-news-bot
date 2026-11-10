//! English changelog lines into Russian, through a local `llama-server`.
//!
//! A translation is used only when it passes [`accept`]; otherwise the line
//! is posted in English as written. The model never sees links or pull
//! request numbers, so it cannot change them.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use ureq::Agent;

use crate::digest::{Block, BlockSection, Line};

const SYSTEM_PROMPT: &str = include_str!("prompt.txt");
const PORT: u16 = 8089;
const CONTEXT_TOKENS: &str = "4096";
const MAX_REPLY_TOKENS: u32 = 256;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// A translation much longer than its source is the model rambling.
const MAX_LENGTH_RATIO: usize = 3;
const LENGTH_SLACK: usize = 40;
/// A translation under a third of its source has dropped most of it.
const MIN_LENGTH_DIVISOR: usize = 3;
/// Share of letters that must be Cyrillic. Kept names and identifiers stay
/// Latin, so the bar is a quarter, not a half.
const MIN_CYRILLIC_DIVISOR: usize = 4;
const MIN_DOTTED_PART: usize = 2;
/// Things a translation must never introduce on its own.
const LINK_MARKERS: [&str; 3] = ["://", "@", "t.me/"];
/// The only variables the model server gets. It is third-party code, so it
/// must not inherit the bot token or the GitHub token.
const PASSED_ENV: [&str; 2] = ["PATH", "HOME"];

pub trait Translator {
    /// Russian for one line, or `None` when no trustworthy translation came back.
    fn translate(&mut self, line: &str) -> Option<String>;
}

/// Posts lines as written. Used when no model is configured.
pub struct NoTranslator;

impl Translator for NoTranslator {
    fn translate(&mut self, _line: &str) -> Option<String> {
        None
    }
}

/// Replaces every line of a block with its Russian translation where one
/// was accepted.
pub fn translate_block(block: Block, translator: &mut dyn Translator) -> Block {
    let sections = block
        .sections
        .into_iter()
        .map(|section| BlockSection {
            lines: section
                .lines
                .into_iter()
                .map(|line| Line {
                    text: translator.translate(&line.text).unwrap_or(line.text),
                    ..line
                })
                .collect(),
            ..section
        })
        .collect();
    Block { sections, ..block }
}

/// A `llama-server` child process, stopped when dropped.
pub struct LlamaServer {
    child: Child,
    agent: Agent,
    url: String,
}

impl LlamaServer {
    /// Starts the server and waits until the model is loaded.
    pub fn start(binary: &Path, model: &Path, agent: Agent) -> Result<Self, String> {
        let log_path = log_path();
        let log = File::create(&log_path)
            .map_err(|e| format!("cannot create {}: {e}", log_path.display()))?;
        let log_copy = log
            .try_clone()
            .map_err(|e| format!("cannot open {}: {e}", log_path.display()))?;
        let threads = std::thread::available_parallelism().map_or(2, usize::from);

        let mut command = Command::new(binary);
        command
            .arg("-m")
            .arg(model)
            .args(["-c", CONTEXT_TOKENS, "--host", "127.0.0.1"])
            .args(["--port", &PORT.to_string(), "-t", &threads.to_string()])
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log_copy);
        command.env_clear();
        for name in PASSED_ENV {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        // The release archive ships its shared libraries next to the binary.
        if let Some(dir) = binary.parent() {
            command.env("LD_LIBRARY_PATH", dir);
        }
        let child = command
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", binary.display()))?;

        let mut server = Self {
            child,
            agent,
            url: format!("http://127.0.0.1:{PORT}"),
        };
        server.wait_until_ready(&log_path)?;
        Ok(server)
    }

    fn wait_until_ready(&mut self, log_path: &Path) -> Result<(), String> {
        let started = Instant::now();
        let health = format!("{}/health", self.url);
        while started.elapsed() < STARTUP_TIMEOUT {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(format!(
                    "llama-server exited early ({status}); see {}",
                    log_path.display()
                ));
            }
            let ready = self
                .agent
                .get(&health)
                .call()
                .is_ok_and(|response| response.status().as_u16() == 200);
            if ready {
                eprintln!("llama-server ready after {} s", started.elapsed().as_secs());
                return Ok(());
            }
            sleep(POLL_INTERVAL);
        }
        Err(format!(
            "llama-server did not load the model in {} s; see {}",
            STARTUP_TIMEOUT.as_secs(),
            log_path.display()
        ))
    }

    fn ask(&self, line: &str) -> Result<String, String> {
        let body = json!({
            "temperature": 0,
            "max_tokens": MAX_REPLY_TOKENS,
            // Gemma 4 otherwise spends its reply budget thinking aloud.
            // Measured on a runner: the server flag `--reasoning-budget 0`
            // does not stop it, this template switch does.
            "chat_template_kwargs": { "enable_thinking": false },
            "messages": [
                { "role": "system", "content": SYSTEM_PROMPT },
                { "role": "user", "content": line },
            ],
        });
        let mut response = self
            .agent
            .post(format!("{}/v1/chat/completions", self.url))
            .send_json(&body)
            .map_err(|e| format!("llama-server request failed: {e}"))?;
        let status = response.status().as_u16();
        let reply: Value = response
            .body_mut()
            .read_json()
            .map_err(|e| format!("llama-server sent an unreadable reply: {e}"))?;
        if status != 200 {
            return Err(format!("llama-server answered HTTP {status}: {reply}"));
        }
        let choice = &reply["choices"][0];
        // Anything but a natural stop is a reply cut off at the token limit.
        if choice["finish_reason"].as_str() != Some("stop") {
            return Err(format!(
                "llama-server did not finish its reply ({})",
                choice["finish_reason"]
            ));
        }
        choice["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("llama-server reply has no text: {reply}"))
    }
}

impl Translator for LlamaServer {
    fn translate(&mut self, line: &str) -> Option<String> {
        match self.ask(line) {
            Ok(reply) => {
                let accepted = accept(line, &reply);
                if accepted.is_none() {
                    eprintln!("translation rejected, posting the original: {line:?} -> {reply:?}");
                }
                accepted
            }
            Err(error) => {
                eprintln!("{error}; posting the original: {line:?}");
                None
            }
        }
    }
}

impl Drop for LlamaServer {
    fn drop(&mut self) {
        // The process may already be gone; there is nothing useful to do then.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn log_path() -> PathBuf {
    std::env::temp_dir().join("llama-server.log")
}

/// Checks a model reply against its source line and returns the cleaned
/// translation, or `None` when the reply cannot be trusted. The reply must
/// be a single line, mostly Russian, of a sane length, carry the numbers of
/// the source in the same order, keep its code identifiers letter for
/// letter, and add no link or mention of its own.
pub fn accept(source: &str, reply: &str) -> Option<String> {
    let mut lines = reply.lines().map(str::trim).filter(|line| !line.is_empty());
    // A second line is a preface or a note, and which line is the
    // translation is then a guess.
    let (line, None) = (lines.next()?, lines.next()) else {
        return None;
    };
    let line = unwrap_quotes(line, source);

    let source_len = source.chars().count();
    let len = line.chars().count();
    let checks = [
        is_mostly_russian(line),
        len * MIN_LENGTH_DIVISOR >= source_len,
        len <= source_len * MAX_LENGTH_RATIO + LENGTH_SLACK,
        numbers(source) == numbers(line),
        code_tokens(source).all(|token| contains_word(line, token)),
        !LINK_MARKERS
            .iter()
            .any(|marker| line.contains(marker) && !source.contains(marker)),
    ];
    checks
        .iter()
        .all(|passed| *passed)
        .then(|| line.to_string())
}

fn is_mostly_russian(line: &str) -> bool {
    let letters = line.chars().filter(|c| c.is_alphabetic()).count();
    let cyrillic = line
        .chars()
        .filter(|c| matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё'))
        .count();
    cyrillic > 0 && cyrillic * MIN_CYRILLIC_DIVISOR >= letters
}

/// Models like to wrap an answer in quotes the source never had.
fn unwrap_quotes<'a>(line: &'a str, source: &str) -> &'a str {
    const PAIRS: [(char, char); 3] = [('"', '"'), ('«', '»'), ('“', '”')];
    for (open, close) in PAIRS {
        let wrapped = line.starts_with(open) && line.ends_with(close) && line.chars().count() > 1;
        if wrapped && !source.starts_with(open) {
            let inner = &line[open.len_utf8()..line.len() - close.len_utf8()];
            // Quotes inside mean the outer pair belongs to the sentence.
            if !inner.contains([open, close]) {
                return inner.trim();
            }
        }
    }
    line
}

/// Every run of digits, in order. "From 8 to 16" and "from 16 to 8" share
/// their numbers and mean the opposite, so the order is part of the check.
fn numbers(text: &str) -> Vec<&str> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|run| !run.is_empty())
        .collect()
}

/// Words that are code rather than prose: `snake_case`, `dotted.names`,
/// `camelCase`, `/commands` and `namespaced:ids`. They must survive
/// translation letter for letter.
fn code_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';'))
        .map(|word| {
            word.trim_start_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/')
                .trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
        })
        .filter(|word| word.is_ascii() && !word.contains('\''))
        .filter(|word| is_code(word))
}

fn is_code(word: &str) -> bool {
    let bytes = word.as_bytes();
    let joined_by = |separator: u8| {
        bytes.windows(3).any(|w| {
            w[1] == separator && w[0].is_ascii_alphanumeric() && w[2].is_ascii_alphanumeric()
        })
    };
    // Parts of two letters or more, so "e.g" and version numbers are not code.
    let dotted = word.contains('.') && word.split('.').all(|part| part.len() >= MIN_DOTTED_PART);
    let camel = bytes
        .windows(2)
        .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
    let command = bytes.len() > 2 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic();
    joined_by(b'_') || joined_by(b':') || dotted || camel || command
}

/// Whether `token` stands in `text` as a whole word, so `get_foo` is not
/// satisfied by `get_foo_bar`.
fn contains_word(text: &str, token: &str) -> bool {
    let is_word_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(token).any(|(start, found)| {
        let before = text[..start].chars().next_back();
        let after = text[start + found.len()..].chars().next();
        !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::Kind;

    #[test]
    fn a_plain_russian_translation_is_accepted() {
        let result = accept(
            "Crowded mobs take cramming damage, as vanilla",
            "Мобы в толпе получают урон от давки, как в ванильной игре",
        );
        assert_eq!(
            result.as_deref(),
            Some("Мобы в толпе получают урон от давки, как в ванильной игре")
        );
    }

    #[test]
    fn blank_lines_around_the_translation_are_ignored() {
        let result = accept("Boats stop", "\n  Лодки останавливаются  \n\n");
        assert_eq!(result.as_deref(), Some("Лодки останавливаются"));
    }

    #[test]
    fn a_reply_of_several_lines_is_rejected() {
        let with_note = "Лодки останавливаются\n\nПримечание: перевод дословный.";
        let with_preface = "Вот перевод:\nЛодки останавливаются";
        assert_eq!(accept("Boats stop", with_note), None);
        assert_eq!(accept("Boats stop", with_preface), None);
    }

    #[test]
    fn a_reply_that_drops_most_of_the_line_is_rejected() {
        let source = "Boats no longer stop when colliding with a minecart on rails";
        assert_eq!(accept(source, "Лодки"), None);
    }

    #[test]
    fn a_mostly_english_reply_is_rejected() {
        let source = "Crowded mobs take cramming damage, as vanilla";
        let reply = "The user wants me to translate this Minecraft changelog line: мобы";
        assert_eq!(accept(source, reply), None);
    }

    #[test]
    fn a_reply_may_keep_many_latin_names() {
        let source = "horizon-client: discord, friends and heartbeat features (default on)";
        let reply = "horizon-client: функции discord, friends и heartbeat (включены по умолчанию)";
        assert!(accept(source, reply).is_some());
    }

    #[test]
    fn a_link_or_mention_the_source_never_had_is_rejected() {
        assert_eq!(
            accept(
                "Boats stop",
                "Лодки останавливаются, см. https://example.test"
            ),
            None
        );
        assert_eq!(
            accept("Boats stop", "Лодки останавливаются, спасибо @someone"),
            None
        );
        let source = "Docs moved to https://playhorizon.dev for good";
        assert!(
            accept(
                source,
                "Документация переехала на https://playhorizon.dev насовсем"
            )
            .is_some()
        );
    }

    #[test]
    fn quotes_that_belong_to_the_sentence_stay() {
        let reply = "\"Лодки\" и \"вагонетки\"";
        assert_eq!(accept("Boats and minecarts", reply).as_deref(), Some(reply));
    }

    #[test]
    fn wrapping_quotes_are_removed() {
        assert_eq!(
            accept("Boats stop", "«Лодки останавливаются»").as_deref(),
            Some("Лодки останавливаются")
        );
        assert_eq!(
            accept("Boats stop", "\"Лодки останавливаются\"").as_deref(),
            Some("Лодки останавливаются")
        );
    }

    #[test]
    fn an_untranslated_reply_is_rejected() {
        assert_eq!(accept("Boats stop", "Boats stop"), None);
        assert_eq!(accept("Boats stop", "   "), None);
    }

    #[test]
    fn a_changed_number_is_rejected() {
        let source = "Signs: keep laid-out sign text between frames (3458 signs: 124 → 222 fps)";
        assert_eq!(
            accept(
                source,
                "Таблички: текст сохраняется между кадрами (3458 табличек: 124 → 232 кадр/с)"
            ),
            None
        );
        assert!(
            accept(
                source,
                "Таблички: текст сохраняется между кадрами (3458 табличек: 124 → 222 кадр/с)"
            )
            .is_some()
        );
    }

    #[test]
    fn a_dropped_or_invented_number_is_rejected() {
        assert_eq!(
            accept(
                "Launcher: the 155 strings Kazakh still shows in English",
                "Лаунчер: строки на казахском"
            ),
            None
        );
        assert_eq!(
            accept("Boats stop", "Лодки останавливаются через 5 секунд"),
            None
        );
    }

    #[test]
    fn swapped_numbers_are_rejected() {
        let source = "Render distance raised from 8 to 16 chunks";
        assert!(accept(source, "Дальность прорисовки увеличена с 8 до 16 чанков").is_some());
        assert_eq!(
            accept(source, "Дальность прорисовки снижена с 16 до 8 чанков"),
            None
        );
    }

    #[test]
    fn commands_and_namespaced_ids_must_survive_unchanged() {
        let source = "The /gamemode command accepts minecraft:spectator again";
        assert!(
            accept(
                source,
                "Команда /gamemode снова принимает minecraft:spectator"
            )
            .is_some()
        );
        assert_eq!(
            accept(source, "Команда /режим снова принимает minecraft:spectator"),
            None
        );
        assert_eq!(
            accept(source, "Команда /gamemode снова принимает наблюдателя"),
            None
        );
    }

    #[test]
    fn an_identifier_inside_a_longer_one_does_not_count() {
        let source = "The toast_add hook is cancellable";
        assert!(accept(source, "Хук toast_add можно отменить").is_some());
        assert_eq!(accept(source, "Хук toast_add_event можно отменить"), None);
    }

    #[test]
    fn identifiers_in_a_comma_list_are_checked_one_by_one() {
        let source = "Hooks getFoo,setBar are public";
        assert!(accept(source, "Хуки getFoo, setBar стали публичными").is_some());
        assert_eq!(
            accept(source, "Хуки getFoo, установитьBar стали публичными"),
            None
        );
    }

    #[test]
    fn code_identifiers_must_survive_unchanged() {
        let source = "F1 hides the HUD only with no screen open, as key.toggleGui does";
        assert!(
            accept(
                source,
                "F1 скрывает интерфейс только без открытого экрана, как key.toggleGui"
            )
            .is_some()
        );
        assert_eq!(
            accept(
                source,
                "F1 скрывает интерфейс только без открытого экрана, как клавиша переключения"
            ),
            None
        );

        let hook = "Native mods: a cancellable toast_add hook (ToastAddEvent)";
        assert!(
            accept(
                hook,
                "Нативные моды: отменяемый хук toast_add (ToastAddEvent)"
            )
            .is_some()
        );
        assert_eq!(
            accept(
                hook,
                "Нативные моды: отменяемый хук добавления уведомления (ToastAddEvent)"
            ),
            None
        );
    }

    #[test]
    fn ordinary_words_are_not_code() {
        let tokens: Vec<&str> = code_tokens(
            "Villagers stand beside the bed where vanilla's area ends. Mods' fluids flow, e.g. now",
        )
        .collect();
        assert!(tokens.is_empty(), "{tokens:?}");
    }

    #[test]
    fn a_rambling_reply_is_rejected() {
        let reply = "Лодки ".repeat(40);
        assert_eq!(accept("Boats stop", &reply), None);
    }

    struct Upper;
    impl Translator for Upper {
        fn translate(&mut self, line: &str) -> Option<String> {
            (line != "keep").then(|| line.to_uppercase())
        }
    }

    #[test]
    fn a_block_keeps_the_original_where_translation_fails() {
        let block = Block {
            kind: Kind::Game,
            label: "Nightly".to_string(),
            url: "https://example.test".to_string(),
            sections: vec![BlockSection {
                title: "Fixed".to_string(),
                lines: vec![
                    Line {
                        text: "one".to_string(),
                        pr: Some(7),
                        similar: 2,
                        keys: vec!["#7".to_string(), "#8".to_string(), "#9".to_string()],
                    },
                    Line {
                        text: "keep".to_string(),
                        pr: None,
                        similar: 0,
                        keys: vec!["t:1".to_string()],
                    },
                ],
                hidden: 3,
                hidden_keys: vec!["#10".to_string()],
            }],
            release_keys: vec!["Nightly".to_string()],
        };

        let translated = translate_block(block.clone(), &mut Upper);
        let lines = &translated.sections[0].lines;

        assert_eq!(lines[0].text, "ONE");
        assert_eq!(lines[1].text, "keep");
        // Only the text changes: links, counts and keys stay as they were.
        assert_eq!((lines[0].pr, lines[0].similar), (Some(7), 2));
        assert_eq!(translated.sections[0].hidden, 3);
        assert_eq!(translated.item_keys(), block.item_keys());
        assert_eq!(translated.release_keys, block.release_keys);
    }
}
