//! The keybinding engine (FR-7.2, FR-8.3).
//!
//! Supports multi-key sequences with an ambiguity timeout, a configurable
//! leader, per-mode tables plus a global table, unbinding, and startup warnings
//! for unknown actions or conflicting bindings.
//!
//! Defaults are compiled in; `keybinds.toml` only overrides (FR-8.3).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};

use crate::config::UiConfig;
use crate::paths::Home;
use crate::tui::action;

/// The input mode a binding applies in (FR-7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mode {
    /// Navigation.
    Normal,
    /// Text entry.
    Insert,
    /// The `:` command line.
    Command,
    /// `/` and `?` search entry.
    Search,
    /// A popup owns the keyboard.
    Popup,
    /// Line or range selection (M1).
    Visual,
}

impl Mode {
    /// Lowercase name, also used as the `keybinds.toml` section name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Insert => "insert",
            Self::Command => "command",
            Self::Search => "search",
            Self::Popup => "popup",
            Self::Visual => "visual",
        }
    }

    /// Uppercase name for the status line.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Command => "COMMAND",
            Self::Search => "SEARCH",
            Self::Popup => "POPUP",
            Self::Visual => "VISUAL",
        }
    }

    /// Parses a `keybinds.toml` section name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "normal" => Some(Self::Normal),
            "insert" => Some(Self::Insert),
            "command" => Some(Self::Command),
            "search" => Some(Self::Search),
            "popup" => Some(Self::Popup),
            "visual" => Some(Self::Visual),
            _ => None,
        }
    }
}

/// A scope is either every mode or one specific mode (FR-8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// Applies everywhere; `[keys.global]`.
    Global,
    /// Applies in one mode.
    In(Mode),
}

impl Scope {
    /// Whether this scope applies while `mode` is active.
    #[must_use]
    pub const fn applies_to(self, mode: Mode) -> bool {
        match self {
            Self::Global => true,
            Self::In(scope) => scope as u8 == mode as u8,
        }
    }
}

/// A single key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    /// Which key.
    pub code: KeyCode,
    /// Which modifiers were held.
    pub modifiers: KeyModifiers,
}

impl KeyCombo {
    /// Builds a combo.
    #[must_use]
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    /// A bare character.
    #[must_use]
    pub const fn char(value: char) -> Self {
        Self::new(KeyCode::Char(value), KeyModifiers::empty())
    }

    /// The key's name without modifiers, in vim-ish notation.
    #[must_use]
    pub fn name(&self) -> String {
        match self.code {
            KeyCode::Char(' ') => "Space".to_owned(),
            KeyCode::Char(value) => value.to_string(),
            KeyCode::Enter => "CR".to_owned(),
            KeyCode::Esc => "Esc".to_owned(),
            KeyCode::Backspace => "BS".to_owned(),
            KeyCode::Tab => "Tab".to_owned(),
            KeyCode::BackTab => "S-Tab".to_owned(),
            KeyCode::Up => "Up".to_owned(),
            KeyCode::Down => "Down".to_owned(),
            KeyCode::Left => "Left".to_owned(),
            KeyCode::Right => "Right".to_owned(),
            KeyCode::Home => "Home".to_owned(),
            KeyCode::End => "End".to_owned(),
            KeyCode::PageUp => "PageUp".to_owned(),
            KeyCode::PageDown => "PageDown".to_owned(),
            KeyCode::Delete => "Del".to_owned(),
            KeyCode::Insert => "Ins".to_owned(),
            KeyCode::F(number) => format!("F{number}"),
            other => format!("{other:?}"),
        }
    }

    /// Renders the combo the way it is written in `keybinds.toml`.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut modifiers = String::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            modifiers.push_str("C-");
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            modifiers.push_str("A-");
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            modifiers.push_str("S-");
        }
        let name = self.name();
        if modifiers.is_empty() && name.chars().count() == 1 {
            name
        } else {
            format!("<{modifiers}{name}>")
        }
    }
}

impl From<crossterm::event::KeyEvent> for KeyCombo {
    fn from(event: crossterm::event::KeyEvent) -> Self {
        Self::new(event.code, event.modifiers)
    }
}

/// Renders a whole sequence, e.g. `<Space>gg`.
pub fn describe_sequence(keys: &[KeyCombo]) -> String {
    keys.iter().map(KeyCombo::describe).collect()
}

/// Where a binding came from, so warnings can name the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Compiled in.
    Default,
    /// From `keybinds.toml`.
    User {
        /// The file it came from.
        file: String,
        /// The section it was declared in.
        section: String,
        /// The key string as written.
        key: String,
    },
}

/// One binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The key sequence.
    pub keys: Vec<KeyCombo>,
    /// The action id it triggers.
    pub action: String,
    /// The mode it applies in.
    pub scope: Scope,
    /// Where it came from.
    pub origin: Origin,
}

impl Binding {
    /// Whether this binding applies while `mode` is active.
    #[must_use]
    pub const fn applies_to(&self, mode: Mode) -> bool {
        self.scope.applies_to(mode)
    }
}

/// What the engine makes of the keys pressed so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution<'a> {
    /// Exactly one binding matched and cannot be extended.
    Match(&'a Binding),
    /// A binding matched, but a longer one could still match: wait for
    /// `timeoutlen` before firing (FR-7.2).
    Ambiguous(&'a Binding),
    /// No binding yet, but a longer one could still match.
    Prefix,
    /// Nothing matches.
    None,
}

/// Everything that can go wrong loading keybindings.
#[derive(Debug, thiserror::Error)]
pub enum KeymapError {
    #[error("could not read keybinds file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("keybinds file {path} is not valid TOML: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("`{binding}` is not a key sequence: {reason}")]
    InvalidKeys { binding: String, reason: String },
    #[error("`{binding}` cannot be the leader key: {reason}")]
    InvalidLeader { binding: String, reason: String },
}

/// The default bindings, compiled in (FR-8.3).
///
/// Deliberately absent: a plain `q` to quit. `REQUIREMENTS.md` reserves `q` for
/// "go back" once there are screens to go back from (FR-3.4), so quitting is
/// bound to `<C-c>`, `<leader>q` and `:q`.
pub const DEFAULT_BINDINGS: &[(Scope, &str, &str)] = &[
    (Scope::Global, "<C-c>", "app.quit"),
    (Scope::Global, "?", "app.help"),
    (Scope::Global, ":", "app.command"),
    (Scope::Global, "<leader>", "app.leader_menu"),
    (Scope::Global, "<Esc>", "app.cancel"),
    (Scope::In(Mode::Normal), "j", "nav.down"),
    (Scope::In(Mode::Normal), "<Down>", "nav.down"),
    (Scope::In(Mode::Normal), "k", "nav.up"),
    (Scope::In(Mode::Normal), "<Up>", "nav.up"),
    (Scope::In(Mode::Normal), "gg", "nav.top"),
    (Scope::In(Mode::Normal), "G", "nav.bottom"),
    (Scope::In(Mode::Normal), "<Tab>", "pane.next"),
    (Scope::In(Mode::Normal), "<S-Tab>", "pane.prev"),
    (Scope::In(Mode::Normal), "<leader>t", "app.theme_picker"),
    (Scope::In(Mode::Normal), "<leader>d", "theme.use_dark"),
    (Scope::In(Mode::Normal), "<leader>l", "theme.use_light"),
    (Scope::In(Mode::Normal), "<leader>?", "app.help"),
    (Scope::In(Mode::Normal), "<leader>r", "app.refresh"),
    (Scope::In(Mode::Normal), "<leader>q", "app.quit"),
];

/// The keybinding engine.
#[derive(Debug, Clone)]
pub struct Keymap {
    leader: KeyCombo,
    timeout: Duration,
    bindings: Vec<Binding>,
}

impl Keymap {
    /// Builds the compiled-in default map, reporting any default binding that
    /// names an action this build does not implement.
    pub fn with_defaults(leader: KeyCombo, timeout: Duration, warnings: &mut Vec<String>) -> Self {
        let mut keymap = Self {
            leader,
            timeout,
            bindings: Vec::new(),
        };
        for (scope, keys, action) in DEFAULT_BINDINGS {
            let parsed = match parse_keys(keys, &leader) {
                Ok(parsed) => parsed,
                Err(error) => {
                    warnings.push(format!(
                        "keybinds: built-in binding `{keys}` is invalid: {error}"
                    ));
                    continue;
                }
            };
            if !action::is_known(action) {
                warnings.push(format!(
                    "keybinds: built-in binding `{keys}` names unknown action `{action}`"
                ));
                continue;
            }
            keymap.bindings.push(Binding {
                keys: parsed,
                action: (*action).to_owned(),
                scope: *scope,
                origin: Origin::Default,
            });
        }
        keymap
    }

    /// The leader key.
    #[must_use]
    pub const fn leader(&self) -> KeyCombo {
        self.leader
    }

    /// The ambiguity timeout.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Changes the ambiguity timeout at runtime (`:set ui.timeoutlen`).
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Every binding, for help output.
    #[must_use]
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Bindings whose sequence starts with the leader and continues, i.e. the
    /// contents of the leader menu (FR-7.3).
    #[must_use]
    pub fn leader_menu(&self) -> Vec<&Binding> {
        let mut found: Vec<&Binding> = self
            .bindings
            .iter()
            .filter(|binding| binding.keys.len() > 1 && binding.keys[0] == self.leader)
            .collect();
        found.sort_by_key(|binding| describe_sequence(&binding.keys));
        found
    }

    /// All bindings that apply in `mode`, for help output.
    #[must_use]
    pub fn bindings_in(&self, mode: Mode) -> Vec<&Binding> {
        self.bindings
            .iter()
            .filter(|binding| binding.applies_to(mode))
            .collect()
    }

    /// Resolves the keys pressed so far (FR-7.2).
    #[must_use]
    pub fn resolve(&self, mode: Mode, sequence: &[KeyCombo]) -> Resolution<'_> {
        let mut best: Option<&Binding> = None;
        let mut extendable = false;

        for binding in &self.bindings {
            if !binding.applies_to(mode) {
                continue;
            }
            if binding.keys.len() > sequence.len() && binding.keys.starts_with(sequence) {
                extendable = true;
                continue;
            }
            if binding.keys == sequence {
                // A mode-specific binding beats a global one for the same keys.
                best = Some(match best {
                    Some(current) if current.scope >= binding.scope => current,
                    _ => binding,
                });
            }
        }

        match (best, extendable) {
            (Some(binding), true) => Resolution::Ambiguous(binding),
            (Some(binding), false) => Resolution::Match(binding),
            (None, true) => Resolution::Prefix,
            (None, false) => Resolution::None,
        }
    }

    /// Applies a user override: an action id binds the keys, `none` unbinds
    /// whatever was there.
    pub fn apply_override(
        &mut self,
        scope: Scope,
        keys: &[KeyCombo],
        action: &str,
        origin: Origin,
        warnings: &mut Vec<String>,
    ) {
        if action == "none" {
            let before = self.bindings.len();
            self.bindings
                .retain(|binding| !(binding.scope == scope && binding.keys == keys));
            if before == self.bindings.len() {
                warnings.push(format!(
                    "keybinds: `{}` was not bound, so unbinding it does nothing",
                    describe_sequence(keys)
                ));
            }
            return;
        }

        if !action::is_known(action) {
            let mut message =
                format!("keybinds: `{action}` is not an action this build implements");
            if let Some(suggestion) = action::suggest(action) {
                let _ = write!(message, "; did you mean `{}`?", suggestion.id);
            }
            warnings.push(message);
            return;
        }

        if let Some(previous) = self
            .bindings
            .iter()
            .find(|binding| binding.scope == scope && binding.keys == keys)
            && previous.origin != Origin::Default
            && previous.action != action
        {
            warnings.push(format!(
                "keybinds: `{}` was already bound to `{}`; the later binding wins",
                describe_sequence(keys),
                previous.action
            ));
        }

        self.bindings
            .retain(|binding| !(binding.scope == scope && binding.keys == keys));
        self.bindings.push(Binding {
            keys: keys.to_vec(),
            action: action.to_owned(),
            scope,
            origin,
        });
    }
}

/// Normalises a key press so terminals that report modifiers differently still
/// match the same binding.
///
/// Uppercase characters already imply Shift, and `BackTab` is Shift+Tab.
#[must_use]
pub fn normalize(combo: KeyCombo) -> KeyCombo {
    let mut combo = combo;
    if let KeyCode::Char(value) = combo.code
        && value.is_uppercase()
    {
        combo.modifiers.remove(KeyModifiers::SHIFT);
    }
    if combo.code == KeyCode::BackTab {
        combo.code = KeyCode::Tab;
        combo.modifiers.insert(KeyModifiers::SHIFT);
    }
    combo
}

/// Parses a key sequence such as `gg`, `<C-d>`, `]c` or `<leader>t`.
///
/// # Errors
///
/// Returns [`KeymapError::InvalidKeys`] when the sequence contains an unknown
/// key name or modifier, and when it is empty.
pub fn parse_keys(input: &str, leader: &KeyCombo) -> Result<Vec<KeyCombo>, KeymapError> {
    let mut keys = Vec::new();
    let characters: Vec<char> = input.chars().collect();
    let mut index = 0;

    while index < characters.len() {
        if characters[index] == '<' {
            let closing = characters[index + 1..].iter().position(|c| *c == '>');
            if let Some(offset) = closing {
                let token: String = characters[index + 1..index + 1 + offset].iter().collect();
                keys.push(parse_token(&token, input, leader)?);
                index += offset + 2;
                continue;
            }
        }
        keys.push(KeyCombo::char(characters[index]));
        index += 1;
    }

    if keys.is_empty() {
        return Err(KeymapError::InvalidKeys {
            binding: input.to_owned(),
            reason: "it is empty".to_owned(),
        });
    }
    Ok(keys)
}

fn parse_token(token: &str, input: &str, leader: &KeyCombo) -> Result<KeyCombo, KeymapError> {
    if token.eq_ignore_ascii_case("leader") {
        return Ok(*leader);
    }

    let parts: Vec<&str> = token.split('-').collect();
    let (modifier_parts, name) = if parts.len() == 1 {
        (&[][..], token)
    } else {
        (&parts[..parts.len() - 1], *parts.last().unwrap_or(&token))
    };

    let mut modifiers = KeyModifiers::empty();
    for part in modifier_parts {
        match part.to_ascii_lowercase().as_str() {
            "c" | "ctrl" | "control" => modifiers.insert(KeyModifiers::CONTROL),
            "s" | "shift" => modifiers.insert(KeyModifiers::SHIFT),
            "a" | "alt" | "m" | "meta" => modifiers.insert(KeyModifiers::ALT),
            other => {
                return Err(KeymapError::InvalidKeys {
                    binding: input.to_owned(),
                    reason: format!("`{other}` is not a modifier (use C-, S- or A-)"),
                });
            }
        }
    }

    let code = match name.to_ascii_lowercase().as_str() {
        "space" => KeyCode::Char(' '),
        "lt" => KeyCode::Char('<'),
        "gt" => KeyCode::Char('>'),
        "cr" | "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "bs" | "backspace" => KeyCode::Backspace,
        "tab" => KeyCode::Tab,
        "btab" => {
            modifiers.insert(KeyModifiers::SHIFT);
            KeyCode::Tab
        }
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "del" | "delete" => KeyCode::Delete,
        "ins" | "insert" => KeyCode::Insert,
        other => {
            if let Some(number) = parse_function_key(other) {
                KeyCode::F(number)
            } else {
                let mut characters = name.chars();
                match (characters.next(), characters.next()) {
                    (Some(single), None) => KeyCode::Char(single),
                    _ => {
                        return Err(KeymapError::InvalidKeys {
                            binding: input.to_owned(),
                            reason: format!("`{name}` is not a key name"),
                        });
                    }
                }
            }
        }
    };

    Ok(normalize(KeyCombo::new(code, modifiers)))
}

/// Parses `f1`..`f12`.
fn parse_function_key(name: &str) -> Option<u8> {
    let number = name.strip_prefix('f')?.parse::<u8>().ok()?;
    (1..=12).contains(&number).then_some(number)
}

/// Loads the default map and applies `keybinds.toml` overrides (FR-8.3).
///
/// # Errors
///
/// Returns [`KeymapError`] when the keybinds file cannot be read or is not valid
/// TOML, and [`KeymapError::InvalidLeader`] when the configured leader is not a
/// single key.
pub fn load(home: &Home, ui: &UiConfig, warnings: &mut Vec<String>) -> Result<Keymap, KeymapError> {
    let path = home.keybinds();
    let table = if path.exists() {
        let text = std::fs::read_to_string(&path).map_err(|source| KeymapError::Read {
            path: path.clone(),
            source,
        })?;
        toml::from_str::<toml::Table>(&text).map_err(|source| KeymapError::Parse {
            path: path.clone(),
            message: source.to_string(),
        })?
    } else {
        toml::Table::new()
    };

    // Accept both `[keys.normal]` (as documented) and a bare `[normal]`.
    let root = table
        .get("keys")
        .and_then(toml::Value::as_table)
        .unwrap_or(&table);

    let leader_spec = root
        .get("leader")
        .and_then(toml::Value::as_str)
        .map_or_else(|| ui.leader.clone(), str::to_owned);
    let leader = parse_leader(&leader_spec)?;

    let timeout = root
        .get("timeoutlen")
        .and_then(toml::Value::as_integer)
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(ui.timeoutlen);

    let mut keymap = Keymap::with_defaults(leader, Duration::from_millis(timeout), warnings);
    let file = path.display().to_string();

    for (section_name, value) in root {
        if section_name == "leader" || section_name == "timeoutlen" {
            continue;
        }
        let scope = if section_name == "global" {
            Some(Scope::Global)
        } else {
            Mode::parse(section_name).map(Scope::In)
        };
        let Some(scope) = scope else {
            warnings.push(format!(
                "keybinds: unknown section `[{section_name}]` is ignored"
            ));
            continue;
        };
        let Some(section) = value.as_table() else {
            warnings.push(format!(
                "keybinds: `[{section_name}]` should be a table of \"keys\" = \"action\""
            ));
            continue;
        };

        for (key_spec, action_value) in section {
            let Some(action) = action_value.as_str() else {
                warnings.push(format!(
                    "keybinds: `[{section_name}] {key_spec}` should map to an action id string"
                ));
                continue;
            };
            let keys = match parse_keys(key_spec, &leader) {
                Ok(keys) => keys,
                Err(error) => {
                    warnings.push(format!("keybinds: {error}"));
                    continue;
                }
            };
            let origin = Origin::User {
                file: file.clone(),
                section: section_name.clone(),
                key: key_spec.clone(),
            };
            keymap.apply_override(scope, &keys, action, origin, warnings);
        }
    }

    Ok(keymap)
}

fn parse_leader(spec: &str) -> Result<KeyCombo, KeymapError> {
    let keys = parse_keys(spec, &KeyCombo::char(' '))?;
    match keys.as_slice() {
        [single] => Ok(*single),
        _ => Err(KeymapError::InvalidLeader {
            binding: spec.to_owned(),
            reason: "it must be exactly one key, for example <Space> or ,".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_home;

    fn space() -> KeyCombo {
        KeyCombo::char(' ')
    }

    fn keymap() -> Keymap {
        let mut warnings = Vec::new();
        let keymap = Keymap::with_defaults(space(), Duration::from_millis(500), &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        keymap
    }

    fn press(value: char) -> KeyCombo {
        normalize(KeyCombo::char(value))
    }

    #[test]
    fn default_bindings_all_name_known_actions() {
        // Guards against the registry and the default map drifting apart.
        let mut warnings = Vec::new();
        Keymap::with_defaults(space(), Duration::from_millis(500), &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn parses_simple_and_modified_keys() {
        let leader = space();
        assert_eq!(parse_keys("gg", &leader).unwrap().len(), 2);
        assert_eq!(parse_keys("j", &leader).unwrap(), vec![KeyCombo::char('j')]);
        assert_eq!(
            parse_keys("<C-d>", &leader).unwrap(),
            vec![KeyCombo::new(KeyCode::Char('d'), KeyModifiers::CONTROL)]
        );
        assert_eq!(
            parse_keys("<Space>", &leader).unwrap(),
            vec![KeyCombo::char(' ')]
        );
        assert_eq!(
            parse_keys("<CR>", &leader).unwrap(),
            vec![KeyCombo::new(KeyCode::Enter, KeyModifiers::empty())]
        );
        assert_eq!(
            parse_keys("<F5>", &leader).unwrap(),
            vec![KeyCombo::new(KeyCode::F(5), KeyModifiers::empty())]
        );
    }

    #[test]
    fn parses_the_leader_token() {
        let leader = KeyCombo::char(',');
        assert_eq!(
            parse_keys("<leader>a", &leader).unwrap(),
            vec![leader, KeyCombo::char('a')]
        );
    }

    #[test]
    fn a_lone_angle_bracket_is_a_literal() {
        assert_eq!(
            parse_keys("<", &space()).unwrap(),
            vec![KeyCombo::char('<')]
        );
        assert_eq!(
            parse_keys("<lt>", &space()).unwrap(),
            vec![KeyCombo::char('<')]
        );
    }

    #[test]
    fn rejects_unknown_key_names() {
        let error = parse_keys("<NotAKey>", &space()).unwrap_err();
        assert!(error.to_string().contains("not a key name"), "{error}");
    }

    #[test]
    fn rejects_unknown_modifiers() {
        let error = parse_keys("<Z-a>", &space()).unwrap_err();
        assert!(error.to_string().contains("not a modifier"), "{error}");
    }

    #[test]
    fn uppercase_matches_with_or_without_shift() {
        let keymap = keymap();
        assert!(matches!(
            keymap.resolve(Mode::Normal, &[press('G')]),
            Resolution::Match(_)
        ));
        let shifted = KeyCombo::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert!(matches!(
            keymap.resolve(Mode::Normal, &[normalize(shifted)]),
            Resolution::Match(_)
        ));
    }

    #[test]
    fn a_single_key_is_a_prefix_of_a_longer_binding() {
        let keymap = keymap();
        // "g" alone is not bound, but "g g" is, so it must wait.
        assert_eq!(
            keymap.resolve(Mode::Normal, &[press('g')]),
            Resolution::Prefix
        );
        match keymap.resolve(Mode::Normal, &[press('g'), press('g')]) {
            Resolution::Match(binding) => assert_eq!(binding.action, "nav.top"),
            other => panic!("expected a match, got {other:?}"),
        }
    }

    #[test]
    fn the_leader_is_ambiguous_until_resolved() {
        let keymap = keymap();
        match keymap.resolve(Mode::Normal, &[space()]) {
            Resolution::Ambiguous(binding) => assert_eq!(binding.action, "app.leader_menu"),
            other => panic!("expected ambiguity, got {other:?}"),
        }
        match keymap.resolve(Mode::Normal, &[space(), press('q')]) {
            Resolution::Match(binding) => assert_eq!(binding.action, "app.quit"),
            other => panic!("expected a match, got {other:?}"),
        }
    }

    #[test]
    fn mode_specific_bindings_do_not_leak_into_other_modes() {
        let keymap = keymap();
        assert_eq!(
            keymap.resolve(Mode::Insert, &[press('j')]),
            Resolution::None
        );
        // Globals apply everywhere.
        assert!(matches!(
            keymap.resolve(
                Mode::Insert,
                &[KeyCombo::new(KeyCode::Char('c'), KeyModifiers::CONTROL)]
            ),
            Resolution::Match(_)
        ));
    }

    #[test]
    fn unbound_keys_resolve_to_nothing() {
        assert_eq!(
            keymap().resolve(Mode::Normal, &[press('z')]),
            Resolution::None
        );
    }

    #[test]
    fn the_leader_menu_lists_continuations() {
        let keymap = keymap();
        let menu = keymap.leader_menu();
        let actions: Vec<&str> = menu.iter().map(|binding| binding.action.as_str()).collect();
        assert!(actions.contains(&"app.theme_picker"));
        assert!(actions.contains(&"app.quit"));
        // The bare leader binding is not part of its own menu.
        assert!(!actions.contains(&"app.leader_menu"));
    }

    #[test]
    fn user_overrides_replace_defaults() {
        let mut keymap = keymap();
        let mut warnings = Vec::new();
        keymap.apply_override(
            Scope::In(Mode::Normal),
            &[press('x')],
            "app.help",
            Origin::User {
                file: "keybinds.toml".to_owned(),
                section: "normal".to_owned(),
                key: "x".to_owned(),
            },
            &mut warnings,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        match keymap.resolve(Mode::Normal, &[press('x')]) {
            Resolution::Match(binding) => assert_eq!(binding.action, "app.help"),
            other => panic!("expected a match, got {other:?}"),
        }
    }

    #[test]
    fn a_binding_can_be_unbound() {
        let mut keymap = keymap();
        let mut warnings = Vec::new();
        keymap.apply_override(
            Scope::In(Mode::Normal),
            &[press('j')],
            "none",
            Origin::Default,
            &mut warnings,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            keymap.resolve(Mode::Normal, &[press('j')]),
            Resolution::None
        );
    }

    #[test]
    fn unknown_actions_are_rejected_with_a_suggestion() {
        let mut keymap = keymap();
        let mut warnings = Vec::new();
        keymap.apply_override(
            Scope::Global,
            &[press('z')],
            "app.qut",
            Origin::Default,
            &mut warnings,
        );
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("did you mean `app.quit`"),
            "{warnings:?}"
        );
        assert_eq!(
            keymap.resolve(Mode::Normal, &[press('z')]),
            Resolution::None
        );
    }

    #[test]
    fn the_leader_must_be_exactly_one_key() {
        let error = parse_leader("gg").unwrap_err();
        assert!(error.to_string().contains("exactly one key"), "{error}");
    }

    #[test]
    fn loading_without_a_file_uses_defaults() {
        let dir = temp_home();
        let home = Home::resolve(Some(dir.path())).unwrap();
        let mut warnings = Vec::new();
        let keymap = load(&home, &UiConfig::default(), &mut warnings).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(keymap.leader(), space());
        assert_eq!(keymap.timeout(), Duration::from_millis(500));
    }

    #[test]
    fn loading_applies_a_user_file() {
        let dir = temp_home();
        let home = Home::resolve(Some(dir.path())).unwrap();
        dir.write(
            "keybinds.toml",
            r#"
            [keys]
            leader = ","
            timeoutlen = 250

            [keys.normal]
            "x" = "app.help"
            "j" = "none"
            "zz" = "app.qut"
            "#,
        );
        let mut warnings = Vec::new();
        let keymap = load(&home, &UiConfig::default(), &mut warnings).unwrap();

        assert_eq!(keymap.leader(), KeyCombo::char(','));
        assert_eq!(keymap.timeout(), Duration::from_millis(250));
        assert_eq!(
            keymap.resolve(Mode::Normal, &[press('j')]),
            Resolution::None
        );
        match keymap.resolve(Mode::Normal, &[press('x')]) {
            Resolution::Match(binding) => assert_eq!(binding.action, "app.help"),
            other => panic!("expected a match, got {other:?}"),
        }
        assert!(
            warnings.iter().any(|w| w.contains("did you mean")),
            "{warnings:?}"
        );
    }

    #[test]
    fn unknown_sections_are_reported() {
        let dir = temp_home();
        let home = Home::resolve(Some(dir.path())).unwrap();
        dir.write(
            "keybinds.toml",
            r#"
            [keys.silly]
            "x" = "app.help"
            "#,
        );
        let mut warnings = Vec::new();
        load(&home, &UiConfig::default(), &mut warnings).unwrap();
        assert!(
            warnings.iter().any(|w| w.contains("unknown section")),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_broken_file_is_an_error_not_a_panic() {
        let dir = temp_home();
        let home = Home::resolve(Some(dir.path())).unwrap();
        dir.write("keybinds.toml", "[keys.normal\n\"x\" =");
        let mut warnings = Vec::new();
        assert!(load(&home, &UiConfig::default(), &mut warnings).is_err());
    }
}
