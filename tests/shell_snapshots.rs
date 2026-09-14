//! Snapshot tests for the rendered shell.
//!
//! Rendering is a pure function of state, so a full frame can be captured with
//! `TestBackend` and compared against a committed file. Run with
//! `UPDATE_SNAPSHOTS=1` to rewrite the snapshots.
//!
//! Volatile content (the home path and the uptime counter) is normalised so the
//! snapshots are stable across machines and runs.

// An integration test is its own crate, so it needs its own allow: assertions
// unwrap, and `UPDATE_SNAPSHOTS=1` prints. Production code is still covered by
// the workspace lints (AGENTS.md §7).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use smart_review::cli::Cli;
use smart_review::tui::event::KeyEvent;
use smart_review::tui::keymap::parse_keys;
use smart_review::{Startup, tui::App};

/// The snapshots share one home directory, which each case deletes on entry, so
/// the suite must not run its cases concurrently.
static SERIAL: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A fixed, short home directory.
///
/// It must not be derived from the checkout location: a path longer than the
/// pane's value column is shortened, and how it is shortened depends on the path,
/// so the rendered text would differ between a repository under `$HOME` and one
/// under `/tmp`. Keeping it short also means the path never reaches the columns a
/// centred popup leaves uncovered. `/tmp` exists on every Unix runner; Windows is
/// best effort (DEC-12).
#[cfg(unix)]
const SNAPSHOT_HOME: &str = "/tmp/smart-review-snapshot";

#[cfg(not(unix))]
const SNAPSHOT_HOME: &str = "smart-review-snapshot";

/// Takes the suite lock and returns a fresh home directory of a fixed length.
fn snapshot_home() -> (MutexGuard<'static, ()>, PathBuf) {
    let guard = lock();
    // The tests in this binary are serialised, but two *runs* of the suite (a
    // developer's and a validation run, or two checkouts) would delete each other's
    // home mid-test, so the directory is per process. The length is fixed either way,
    // which is what keeps the rendered paths stable.
    let home = PathBuf::from(format!("{}-{}", SNAPSHOT_HOME, std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create the snapshot home");
    (guard, home)
}

/// The instant the snapshot frames are rendered at, so ages are stable.
const SNAPSHOT_NOW: u64 = 1_700_010_800;

fn build(home: &Path) -> App {
    let cli = Cli {
        repo: Some("acme/service".to_owned()),
        pr: None,
        path: None,
        remote: None,
        config: None,
        theme: None,
        home: Some(home.to_path_buf()),
        log_level: None,
        check: false,
        dry_run: false,
    };
    let startup = Startup::load(&cli).expect("bootstrap");
    let mut app = App::new(startup).expect("build the app");
    // Ages in the list are relative to now, which the loop normally supplies.
    app.set_now(SNAPSHOT_NOW);
    app
}

fn press(app: &mut App, keys: &str) {
    // The compiled-in leader; the tests exercise the default map.
    let leader = smart_review::tui::keymap::KeyCombo::char(' ');
    for combo in parse_keys(keys, &leader).expect("parse keys") {
        app.on_key(KeyEvent::new(combo.code, combo.modifiers));
    }
}

fn render(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal
        .draw(|frame| app.render(frame))
        .expect("draw the frame");
    buffer_to_string(terminal.backend().buffer())
}

/// Renders a buffer as text with trailing whitespace removed.
fn buffer_to_string(buffer: &Buffer) -> String {
    let area = buffer.area();
    let mut output = String::new();
    for y in area.top()..area.bottom() {
        let mut row = String::new();
        for x in area.left()..area.right() {
            row.push_str(buffer[(x, y)].symbol());
        }
        output.push_str(row.trim_end());
        output.push('\n');
    }
    output
}

/// Rebuilds one row of the right-hand pane with a fixed value.
///
/// The path rows depend on where the checkout lives — the pane shortens long
/// paths, and whether it shortens at all depends on the length of the whole path
/// — so replacing the rendered text is the only way to keep the snapshot stable.
/// `paths::shorten_for_display` has its own test for the shortening itself.
fn rewrite_row(line: &str, label: &str, value: &str) -> Option<String> {
    let (left, right) = line.split_once("││")?;
    if !right.trim_start().starts_with(label) {
        return None;
    }
    // Character count, not byte offset: the border is multi-byte, so `rfind`
    // would inflate the width by its extra bytes and shift the padding.
    let width = right[..right.rfind('│')?].chars().count();
    let rebuilt = format!(" {label:<9}{value}");
    Some(format!("{left}││{rebuilt:<width$}│"))
}

/// Removes the parts of a frame that legitimately differ between runs.
fn normalize(text: &str, home: &Path) -> String {
    let normalized = text
        .lines()
        .map(|line| {
            rewrite_row(line, "home", "<HOME>")
                .or_else(|| rewrite_row(line, "config", "<HOME>/config.toml"))
                .or_else(|| rewrite_row(line, "uptime", "0s"))
                .unwrap_or_else(|| line.to_owned())
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Anything else that mentions the checkout path is the same on every run of
    // this machine but not on another, so normalise it too.
    normalized.replace(&home.display().to_string(), "<HOME>")
}

fn assert_snapshot(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join(format!("{name}.txt"));

    if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        eprintln!("updated {}", path.display());
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "could not read {}: {error}\nrun with UPDATE_SNAPSHOTS=1 to create it",
            path.display()
        )
    });

    if expected != actual {
        let diff = expected
            .lines()
            .zip(actual.lines())
            .enumerate()
            .filter(|(_, (left, right))| left != right)
            .map(|(index, (left, right))| {
                format!(
                    "line {}:\n  expected: {left:?}\n  actual:   {right:?}",
                    index + 1
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        panic!("snapshot `{name}` changed:\n{diff}");
    }
}

#[test]
fn shell_in_normal_mode() {
    let (_serial, home) = snapshot_home();
    let mut app = build(&home);
    let frame = normalize(&render(&mut app, 100, 30), &home);
    assert_snapshot("shell_normal", &frame);
}

#[test]
fn shell_with_help_open() {
    let (_serial, home) = snapshot_home();
    let mut app = build(&home);
    press(&mut app, "?");
    let frame = normalize(&render(&mut app, 100, 30), &home);
    assert_snapshot("shell_help", &frame);
}

#[test]
fn shell_with_leader_menu_open() {
    let (_serial, home) = snapshot_home();
    let mut app = build(&home);
    press(&mut app, "<Space>");
    app.on_timeout();
    let frame = normalize(&render(&mut app, 100, 30), &home);
    assert_snapshot("shell_leader", &frame);
}

#[test]
fn shell_with_theme_picker_open() {
    let (_serial, home) = snapshot_home();
    let mut app = build(&home);
    press(&mut app, "<Space>t");
    let frame = normalize(&render(&mut app, 100, 30), &home);
    assert_snapshot("shell_theme_picker", &frame);
}

#[test]
fn shell_in_light_theme() {
    let (_serial, home) = snapshot_home();
    let mut app = build(&home);
    press(&mut app, "<Space>T");
    let frame = normalize(&render(&mut app, 100, 30), &home);
    assert_snapshot("shell_light", &frame);
}

/// A detection result, so the list pane shows pull requests rather than the
/// "looking for the repository" state.
fn environment() -> smart_review::domain::environment::Environment {
    smart_review::domain::environment::Environment {
        repo: smart_review::domain::RepoId::parse("acme/service").expect("repository"),
        mode: smart_review::domain::environment::RunMode::InRepo,
        remote: Some("origin".to_owned()),
        root: Some(PathBuf::from("/src/service")),
        default_branch: Some("main".to_owned()),
        git_version: "2.43.0".to_owned(),
        gh: smart_review::domain::environment::GhInstall {
            path: PathBuf::from("/usr/bin/gh"),
            version: "2.45.0".to_owned(),
            account: Some("bruno".to_owned()),
            scopes: vec!["repo".to_owned()],
        },
    }
}

/// Three pull requests with the markers the list promises to show.
fn pull_requests() -> smart_review::ports::forge::PullRequestPage {
    use smart_review::domain::pr::{
        CheckState, CheckSummary, PrState, PullRequestSummary, ReviewDecision,
    };

    let make = |number: u64,
                title: &str,
                author: &str,
                draft: bool,
                checks: (CheckState, u32, u32),
                decision: Option<ReviewDecision>| PullRequestSummary {
        number,
        title: title.to_owned(),
        author: author.to_owned(),
        state: PrState::Open,
        is_draft: draft,
        base_ref: "main".to_owned(),
        head_ref: "topic".to_owned(),
        head_sha: "5e44fd9d2e1d".to_owned(),
        created_at: smart_review::domain::from_unix_secs(1_700_000_000),
        updated_at: smart_review::domain::from_unix_secs(1_700_003_600),
        additions: 218,
        deletions: 43,
        changed_files: 7,
        labels: Vec::new(),
        review_decision: decision,
        checks: CheckSummary {
            state: checks.0,
            passed: checks.1,
            total: checks.2,
        },
        url: "https://github.com/acme/service/pull/142".to_owned(),
        is_cross_repository: false,
    };

    smart_review::ports::forge::PullRequestPage {
        items: vec![
            make(
                142,
                "Add retry to the webhook dispatcher",
                "alice",
                false,
                (CheckState::Success, 3, 3),
                Some(ReviewDecision::Approved),
            ),
            make(
                141,
                "WIP refactor of billing domain",
                "bruno",
                true,
                (CheckState::Failure, 1, 3),
                Some(ReviewDecision::ChangesRequested),
            ),
            make(
                138,
                "Bump tokio to 1.53",
                "dependabot",
                false,
                (CheckState::Unknown, 0, 0),
                None,
            ),
        ],
        limit: 50,
        total: Some(137),
    }
}

/// A detail, so the review screen's tab bar has something to name.
fn detail() -> smart_review::domain::pr::PullRequestDetail {
    use smart_review::domain::pr::{PrState, PullRequestSummary};
    use smart_review::domain::time::Timestamp;

    let mut summary = PullRequestSummary {
        number: 141,
        title: "WIP refactor of billing domain".to_owned(),
        author: "bruno".to_owned(),
        state: PrState::Open,
        is_draft: true,
        base_ref: "main".to_owned(),
        head_ref: "refactor/billing".to_owned(),
        head_sha: "ba6c89f0a1b2".to_owned(),
        created_at: Timestamp::default(),
        updated_at: Timestamp::default(),
        additions: 412,
        deletions: 96,
        changed_files: 12,
        labels: Vec::new(),
        review_decision: None,
        checks: smart_review::domain::pr::CheckSummary::default(),
        url: String::new(),
        is_cross_repository: false,
    };
    summary.number = 141;

    smart_review::domain::pr::PullRequestDetail {
        summary,
        body: "Splits Invoice into a domain object and a projection.".to_owned(),
        merge_state_status: Some("BLOCKED".to_owned()),
        reviewers: vec!["alice".to_owned()],
        commits: Vec::new(),
        checks: Vec::new(),
        reviews: Vec::new(),
        comments: Vec::new(),
        base_sha: None,
    }
}

/// A patch with the cases the diff pane has to render distinctly.
fn diff_view() -> smart_review::tui::diff_view::DiffView {
    const PATCH: &str = "\
diff --git a/src/domain/invoice.rs b/src/domain/invoice.rs
index 1a2b3c4..5d6e7f8 100644
--- a/src/domain/invoice.rs
+++ b/src/domain/invoice.rs
@@ -12,4 +14,5 @@ impl Invoice {
     pub fn total(&self) -> Money {
-        self.lines.sum()
+        let gross = self.lines.sum();
+        gross - self.discount
     }
 }
diff --git a/docs/logo.png b/docs/logo.png
new file mode 100644
index 0000000..2222222
Binary files /dev/null and b/docs/logo.png differ
diff --git a/scripts/build.sh b/scripts/build.sh
old mode 100644
new mode 100755
";
    smart_review::tui::diff_view::DiffView::new(smart_review::domain::diff::parse_patch(PATCH))
}

#[test]
fn shell_with_pull_requests() {
    let (_serial, home) = snapshot_home();
    let mut app = build(&home);
    app.set_environment(environment());
    app.set_pull_requests(pull_requests());
    let frame = normalize(&render(&mut app, 110, 24), &home);
    assert_snapshot("shell_list", &frame);
}

#[test]
fn shell_with_a_search_that_matches_nothing() {
    let (_serial, home) = snapshot_home();
    let mut app = build(&home);
    app.set_environment(environment());
    app.set_pull_requests(pull_requests());
    press(&mut app, "/");
    for character in "postgres".chars() {
        press(&mut app, &character.to_string());
    }
    let frame = normalize(&render(&mut app, 110, 24), &home);
    assert_snapshot("shell_list_no_match", &frame);
}

#[test]
fn shell_with_a_diff_open() {
    let (_serial, home) = snapshot_home();
    let mut app = build(&home);
    app.set_environment(environment());
    app.set_pull_requests(pull_requests());
    app.open_review(detail(), diff_view());
    let frame = normalize(&render(&mut app, 110, 24), &home);
    assert_snapshot("shell_diff", &frame);
}

#[test]
fn shell_with_the_diff_in_split_view() {
    let (_serial, home) = snapshot_home();
    let mut app = build(&home);
    app.set_environment(environment());
    app.set_pull_requests(pull_requests());
    app.open_review(detail(), diff_view());
    // Wide enough for the split view, which needs 140 columns (DEC-4).
    press(&mut app, "<Space>ds");
    let frame = normalize(&render(&mut app, 150, 24), &home);
    assert_snapshot("shell_diff_split", &frame);
}

#[test]
fn shell_on_a_small_terminal() {
    let (_serial, home) = snapshot_home();
    let mut app = build(&home);
    let frame = normalize(&render(&mut app, 60, 12), &home);
    assert_snapshot("shell_too_small", &frame);
}

#[test]
fn shell_with_the_command_line_open() {
    let (_serial, home) = snapshot_home();
    let mut app = build(&home);
    press(&mut app, ":them");
    let frame = normalize(&render(&mut app, 100, 30), &home);
    assert_snapshot("shell_command_line", &frame);
}
