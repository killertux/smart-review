//! Snapshot tests for the rendered shell.
//!
//! Rendering is a pure function of state, so a full frame can be captured with
//! `TestBackend` and compared against a committed file. Run with
//! `UPDATE_SNAPSHOTS=1` to rewrite the snapshots.
//!
//! Volatile content (the home path and the uptime counter) is normalised so the
//! snapshots are stable across machines and runs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::path::{Path, PathBuf};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use smart_review::cli::Cli;
use smart_review::tui::event::KeyEvent;
use smart_review::tui::keymap::parse_keys;
use smart_review::{Startup, tui::App};

/// A fixed home directory so the snapshots do not depend on the machine.
fn snapshot_home() -> PathBuf {
    let home = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("snapshot-home");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create the snapshot home");
    home
}

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
    };
    let startup = Startup::load(&cli).expect("bootstrap");
    App::new(startup).expect("build the app")
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

/// Removes the parts of a frame that legitimately differ between runs.
fn normalize(text: &str, home: &Path) -> String {
    text.replace(&home.display().to_string(), "<HOME>")
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("uptime") {
                " uptime    0s".to_owned()
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    let home = snapshot_home();
    let mut app = build(&home);
    let frame = normalize(&render(&mut app, 100, 30), &home);
    assert_snapshot("shell_normal", &frame);
}

#[test]
fn shell_with_help_open() {
    let home = snapshot_home();
    let mut app = build(&home);
    press(&mut app, "?");
    let frame = normalize(&render(&mut app, 100, 30), &home);
    assert_snapshot("shell_help", &frame);
}

#[test]
fn shell_with_leader_menu_open() {
    let home = snapshot_home();
    let mut app = build(&home);
    press(&mut app, "<Space>");
    app.on_timeout();
    let frame = normalize(&render(&mut app, 100, 30), &home);
    assert_snapshot("shell_leader", &frame);
}

#[test]
fn shell_with_theme_picker_open() {
    let home = snapshot_home();
    let mut app = build(&home);
    press(&mut app, "<Space>t");
    let frame = normalize(&render(&mut app, 100, 30), &home);
    assert_snapshot("shell_theme_picker", &frame);
}

#[test]
fn shell_in_light_theme() {
    let home = snapshot_home();
    let mut app = build(&home);
    press(&mut app, "<Space>l");
    let frame = normalize(&render(&mut app, 100, 30), &home);
    assert_snapshot("shell_light", &frame);
}

#[test]
fn shell_on_a_small_terminal() {
    let home = snapshot_home();
    let mut app = build(&home);
    let frame = normalize(&render(&mut app, 60, 12), &home);
    assert_snapshot("shell_too_small", &frame);
}

#[test]
fn shell_with_the_command_line_open() {
    let home = snapshot_home();
    let mut app = build(&home);
    press(&mut app, ":them");
    let frame = normalize(&render(&mut app, 100, 30), &home);
    assert_snapshot("shell_command_line", &frame);
}
