//! Posts confirmed Horizon news to Telegram in Russian.
//!
//! The bot is started on a schedule, does one pass and exits:
//!
//! * `check` asks GitHub whether there is anything to post and prints how
//!   many posts are waiting. The workflow uses that number to decide
//!   whether the translation model is worth loading.
//! * `post` translates the waiting posts and sends them.
//!
//! News is confirmed by construction: only items of releases Horizon has
//! already published are posted, each with a link to its pull request.

mod digest;
mod github;
mod parse;
mod state;
mod telegram;
mod translate;

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use ureq::Agent;

use crate::digest::Plan;
use crate::state::{Etags, State};
use crate::translate::{LlamaServer, NoTranslator, Translator};

const DEFAULT_SOURCE_REPO: &str = "horizon-rust/horizon";
const DEFAULT_STATE_PATH: &str = "state/state.json";
const NETWORK_TIMEOUT: Duration = Duration::from_secs(30);
/// One changelog line on a shared CPU runner; generous on purpose.
const TRANSLATION_TIMEOUT: Duration = Duration::from_secs(180);
const USAGE: &str = "\
usage: horizon-news-bot <check|post> [--state PATH] [--dry-run]

  check      print how many posts are waiting; remember releases with no news
  post       translate and send the waiting posts
  --state    state file (default: state/state.json)
  --dry-run  print instead of sending, and leave the state file alone

environment:
  TELEGRAM_BOT_TOKEN, TELEGRAM_CHAT_ID   required by post without --dry-run
  LLAMA_SERVER, LLAMA_MODEL              llama-server binary and GGUF model;
                                         without them lines stay in English
  GITHUB_TOKEN                           optional, raises the API rate limit
  SOURCE_REPO                            default: horizon-rust/horizon";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Check,
    Post,
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    mode: Mode,
    state_path: PathBuf,
    dry_run: bool,
}

struct Credentials {
    token: String,
    chat_id: String,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args(env::args().skip(1))?;
    let agent = http_agent(NETWORK_TIMEOUT, true);
    let state = State::load(&args.state_path)?;

    let repo = env_value("SOURCE_REPO").unwrap_or_else(|| DEFAULT_SOURCE_REPO.to_string());
    let snapshot = github::fetch(&agent, &repo, env_value("GITHUB_TOKEN").as_deref(), &state)?;
    let plan = digest::plan(&snapshot.releases, &state);
    let waiting = plan.blocks.len();
    eprintln!(
        "{} release(s) fetched, {waiting} post(s) waiting, {} release(s) with nothing new",
        snapshot.releases.len(),
        plan.silent_releases.len()
    );

    match args.mode {
        Mode::Check => {
            if waiting == 0 {
                settle(&args, state, plan.silent_releases, snapshot.etags)?;
            }
            println!("{waiting}");
            Ok(())
        }
        Mode::Post if waiting == 0 => settle(&args, state, plan.silent_releases, snapshot.etags),
        Mode::Post => post(&args, &agent, state, plan, snapshot.etags),
    }
}

/// Nothing to post: remember what was looked at, so the next run is cheap.
/// A run that learned nothing new leaves the file untouched.
fn settle(args: &Args, state: State, silent: Vec<String>, etags: Etags) -> Result<(), String> {
    let next = state.clone().with_seen(silent, []).with_etags(etags);
    if args.dry_run || next == state {
        return Ok(());
    }
    next.save(&args.state_path)
}

/// Sends the waiting posts in order. A post counts as done only after
/// Telegram accepted it, so a failure leaves the rest for the next run.
fn post(args: &Args, agent: &Agent, state: State, plan: Plan, etags: Etags) -> Result<(), String> {
    let credentials = if args.dry_run {
        None
    } else {
        Some(credentials_from_env()?)
    };
    let mut translator = translator_from_env()?;
    let mut next = state.with_seen(plan.silent_releases, []);
    let mut failure = None;

    for block in plan.blocks {
        let block = translate::translate_block(block, translator.as_mut());
        let sent = match &credentials {
            Some(c) => telegram::send_block(agent, &c.token, &c.chat_id, &block),
            None => {
                println!("{}\n", telegram::render(&block));
                Ok(())
            }
        };
        // Saved after every post: if the run is killed later, what was
        // already sent is not sent again.
        let recorded = sent.and_then(|()| {
            eprintln!("posted: {:?}", block.label);
            next = next.clone().with_seen(block.release_keys, block.item_keys);
            save(args, &next)
        });
        if let Err(error) = recorded {
            failure = Some(error);
            break;
        }
    }
    drop(translator);

    // The ETags stay old after a failure, so the next run fetches again.
    if failure.is_none() {
        next = next.with_etags(etags);
    }
    match (failure, save(args, &next)) {
        (Some(failure), Err(save_error)) => Err(format!("{failure}; and then {save_error}")),
        (Some(failure), Ok(())) => Err(failure),
        (None, saved) => saved,
    }
}

fn save(args: &Args, state: &State) -> Result<(), String> {
    if args.dry_run {
        return Ok(());
    }
    state.save(&args.state_path)
}

fn translator_from_env() -> Result<Box<dyn Translator>, String> {
    match (env_value("LLAMA_SERVER"), env_value("LLAMA_MODEL")) {
        (Some(server), Some(model)) => {
            let agent = http_agent(TRANSLATION_TIMEOUT, false);
            let server = LlamaServer::start(Path::new(&server), Path::new(&model), agent)?;
            Ok(Box::new(server))
        }
        (None, None) => {
            eprintln!("no translation model configured, posting lines in English");
            Ok(Box::new(NoTranslator))
        }
        _ => Err("set both LLAMA_SERVER and LLAMA_MODEL, or neither".to_string()),
    }
}

fn credentials_from_env() -> Result<Credentials, String> {
    let required = |name: &str| env_value(name).ok_or_else(|| format!("{name} is not set"));
    Ok(Credentials {
        token: required("TELEGRAM_BOT_TOKEN")?,
        chat_id: required("TELEGRAM_CHAT_ID")?,
    })
}

/// An environment variable that is set to something other than blank.
fn env_value(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Statuses are handled by the callers, so 4xx and 5xx are not errors here.
/// The local model server must be reached directly, never through a proxy.
fn http_agent(timeout: Duration, use_env_proxy: bool) -> Agent {
    let builder = Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false);
    let builder = if use_env_proxy {
        builder
    } else {
        builder.proxy(None)
    };
    builder.build().into()
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut mode = None;
    let mut state_path = PathBuf::from(DEFAULT_STATE_PATH);
    let mut dry_run = false;

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "check" if mode.is_none() => mode = Some(Mode::Check),
            "post" if mode.is_none() => mode = Some(Mode::Post),
            "--dry-run" => dry_run = true,
            "--state" => {
                state_path = args
                    .next()
                    .map(PathBuf::from)
                    .ok_or("--state needs a path")?;
            }
            other => return Err(format!("unexpected argument {other:?}\n\n{USAGE}")),
        }
    }
    let mode = mode.ok_or_else(|| USAGE.to_string())?;
    Ok(Args {
        mode,
        state_path,
        dry_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(ToString::to_string))
    }

    #[test]
    fn check_uses_the_default_state_path() {
        let args = parse(&["check"]).unwrap();
        assert_eq!(
            args,
            Args {
                mode: Mode::Check,
                state_path: PathBuf::from(DEFAULT_STATE_PATH),
                dry_run: false
            }
        );
    }

    #[test]
    fn post_accepts_state_path_and_dry_run_in_any_order() {
        let args = parse(&["--dry-run", "post", "--state", "x/s.json"]).unwrap();
        assert_eq!(
            args,
            Args {
                mode: Mode::Post,
                state_path: PathBuf::from("x/s.json"),
                dry_run: true
            }
        );
    }

    #[test]
    fn a_missing_mode_shows_the_usage() {
        assert!(parse(&[]).unwrap_err().starts_with("usage:"));
        assert!(parse(&["--dry-run"]).unwrap_err().starts_with("usage:"));
    }

    #[test]
    fn unknown_or_repeated_arguments_are_rejected() {
        assert!(
            parse(&["check", "--force"])
                .unwrap_err()
                .contains("--force")
        );
        assert!(parse(&["check", "post"]).unwrap_err().contains("post"));
        assert!(
            parse(&["post", "--state"])
                .unwrap_err()
                .contains("--state needs a path")
        );
    }
}
