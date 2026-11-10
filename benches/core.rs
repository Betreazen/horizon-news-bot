//! Benchmarks of the bot's own work on one run's worth of real data.
//!
//! These are a contract, like the tests: they were written before any
//! optimisation and are not edited to make a change look better. Results
//! and decisions are logged in `docs/perf-log.md`.
//!
//! ```sh
//! cargo bench --bench core -- --save-baseline before
//! cargo bench --bench core -- --baseline before
//! ```

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use serde_json::{Value, json};

use horizon_news_bot::digest::{self, Block};
use horizon_news_bot::github::Release;
use horizon_news_bot::parse;
use horizon_news_bot::state::State;
use horizon_news_bot::telegram::{self, MESSAGE_LIMIT};
use horizon_news_bot::translate;

/// Twelve real Horizon releases, with the fields the bot does not read
/// stripped to keep the repository small.
const RELEASES: &str = include_str!("../tests/fixtures/releases.json");
const LAUNCHER_BODY: &str = include_str!("../tests/fixtures/launcher.md");

/// What the release list looks like on the wire: every release carries an
/// author and a few dozen assets the bot skips over. Rebuilt here in the
/// shape and size of the real answer (about 1.7 kB an asset).
fn wire_payload() -> String {
    let asset = json!({
        "url": "https://api.github.com/repos/horizon-rust/horizon/releases/assets/100000001",
        "id": 100000001u64,
        "node_id": "RA_kwDOExampleExampleExample",
        "name": "horizon-launcher-0.2.58-x86_64-unknown-linux-gnu.tar.gz",
        "label": "",
        "uploader": uploader(),
        "content_type": "application/gzip",
        "state": "uploaded",
        "size": 48211034,
        "digest": "sha256:368227edcc229be5f9bed16a2223d79b5e41d0ac2e19bcf5825e82291c060e55",
        "download_count": 18342,
        "created_at": "2026-11-09T10:08:35Z",
        "updated_at": "2026-11-09T10:08:41Z",
        "browser_download_url": "https://github.com/horizon-rust/horizon/releases/download/launcher%400.2.58/horizon-launcher-0.2.58-x86_64-unknown-linux-gnu.tar.gz"
    });
    let mut releases: Vec<Value> = serde_json::from_str(RELEASES).expect("fixture is valid JSON");
    for release in &mut releases {
        release["author"] = uploader();
        release["assets"] = Value::Array(vec![asset.clone(); 38]);
        release["tarball_url"] =
            json!("https://api.github.com/repos/horizon-rust/horizon/tarball/launcher@0.2.58");
        release["zipball_url"] =
            json!("https://api.github.com/repos/horizon-rust/horizon/zipball/launcher@0.2.58");
        release["target_commitish"] = json!("main");
    }
    // A full page is thirty releases; the fixture holds twelve.
    let page: Vec<Value> = releases.iter().cycle().take(30).cloned().collect();
    serde_json::to_string(&page).expect("payload serialises")
}

fn uploader() -> Value {
    json!({
        "login": "github-actions[bot]",
        "id": 41898282,
        "node_id": "MDM6Qm90NDE4OTgyODI=",
        "avatar_url": "https://avatars.githubusercontent.com/in/15368?v=4",
        "gravatar_id": "",
        "url": "https://api.github.com/users/github-actions%5Bbot%5D",
        "html_url": "https://github.com/apps/github-actions",
        "followers_url": "https://api.github.com/users/github-actions%5Bbot%5D/followers",
        "following_url": "https://api.github.com/users/github-actions%5Bbot%5D/following{/other_user}",
        "gists_url": "https://api.github.com/users/github-actions%5Bbot%5D/gists{/gist_id}",
        "starred_url": "https://api.github.com/users/github-actions%5Bbot%5D/starred{/owner}{/repo}",
        "subscriptions_url": "https://api.github.com/users/github-actions%5Bbot%5D/subscriptions",
        "organizations_url": "https://api.github.com/users/github-actions%5Bbot%5D/orgs",
        "repos_url": "https://api.github.com/users/github-actions%5Bbot%5D/repos",
        "events_url": "https://api.github.com/users/github-actions%5Bbot%5D/events{/privacy}",
        "received_events_url": "https://api.github.com/users/github-actions%5Bbot%5D/received_events",
        "type": "Bot",
        "user_view_type": "public",
        "site_admin": false
    })
}

/// A state as it looks after months of running: both lists at their caps.
fn long_running_state() -> State {
    State::default().with_seen(
        (0..400).map(|n| format!("Old release {n}")),
        (0..4000).map(|n| format!("#{}", 1_000_000 + n)),
    )
}

fn keys_of(blocks: &[Block]) -> (Vec<String>, Vec<String>) {
    let releases = blocks
        .iter()
        .flat_map(|block| block.release_keys.clone())
        .collect();
    let items = blocks
        .iter()
        .flat_map(|block| telegram::parts(block, MESSAGE_LIMIT))
        .flat_map(|part| part.item_keys)
        .collect();
    (releases, items)
}

fn benches(c: &mut Criterion) {
    let payload = wire_payload();
    let releases: Vec<Release> = serde_json::from_str(&payload).expect("payload deserialises");
    let state = long_running_state();
    let plan = digest::plan(&releases, &state);
    assert!(plan.blocks.len() >= 2, "the fixture must give real work");
    let (release_keys, item_keys) = keys_of(&plan.blocks);
    let caught_up = state
        .clone()
        .with_seen(release_keys.clone(), item_keys.clone());
    assert!(digest::plan(&releases, &caught_up).blocks.is_empty());

    c.bench_function("deserialize/wire_release_list", |b| {
        b.iter(|| serde_json::from_str::<Vec<Release>>(black_box(&payload)).expect("valid"))
    });

    c.bench_function("parse_body/launcher_release", |b| {
        b.iter(|| parse::parse_body(black_box(LAUNCHER_BODY)))
    });

    // The two shapes of a run: a burst of news, and the usual quiet check.
    c.bench_function("plan/everything_new", |b| {
        b.iter(|| digest::plan(black_box(&releases), black_box(&state)))
    });
    c.bench_function("plan/nothing_new", |b| {
        b.iter(|| digest::plan(black_box(&releases), black_box(&caught_up)))
    });

    c.bench_function("parts/all_blocks", |b| {
        b.iter(|| {
            plan.blocks
                .iter()
                .map(|block| telegram::parts(black_box(block), MESSAGE_LIMIT).len())
                .sum::<usize>()
        })
    });

    c.bench_function("state/record_a_post", |b| {
        b.iter_batched(
            || (state.clone(), release_keys.clone(), item_keys.clone()),
            |(state, releases, items)| state.with_seen(releases, items),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("accept/typical_line", |b| {
        b.iter(|| {
            translate::accept(
                black_box("Signs: keep laid-out sign text between frames (3458 signs: 124 → 222 fps), as key.toggleGui does"),
                black_box("Таблички: текст сохраняется между кадрами (3458 табличек: 124 → 222 fps), как делает key.toggleGui"),
            )
        })
    });

    // Everything a quiet run computes after the network answered.
    c.bench_function("run/quiet_check", |b| {
        b.iter(|| {
            let releases: Vec<Release> = serde_json::from_str(black_box(&payload)).expect("valid");
            digest::plan(&releases, black_box(&caught_up)).blocks.len()
        })
    });
}

criterion_group!(core, benches);
criterion_main!(core);
