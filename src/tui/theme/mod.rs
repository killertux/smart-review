//! The theme engine (FR-7.7, FR-8.4).
//!
//! Every colour a widget draws comes from here: there are no hard-coded colours
//! in the components, which is what makes `:theme` and user themes work.
//!
//! Element names are dotted paths that line up with the sections of a theme file
//! (`diff.add`, `status.normal`, `colors.bg`). A missing element falls back to
//! the theme's foreground colour, and a missing key in a user theme falls back to
//! its `base` theme, then to the built-in default (FR-7.7, FR-8.4).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

use crate::paths::Home;

/// Element names understood by the built-in themes and by theme files.
pub mod element {
    /// Application background.
    pub const BG: &str = "colors.bg";
    /// Application foreground.
    pub const FG: &str = "colors.fg";
    /// Accent colour used for highlights.
    pub const ACCENT: &str = "colors.accent";
    /// Border of unfocused panes.
    pub const BORDER: &str = "ui.border";
    /// Border of the focused pane.
    pub const BORDER_FOCUSED: &str = "ui.border_focused";
    /// Titles of panes and popups.
    pub const TITLE: &str = "ui.title";
    /// The row the cursor is on.
    pub const CURSOR_LINE: &str = "ui.cursor_line";
    /// The selected item when a popup has focus.
    pub const SELECTION: &str = "ui.selection";
    /// Muted text such as placeholders and hints.
    pub const MUTED: &str = "ui.muted";
    /// Status line while in normal mode.
    pub const STATUS_NORMAL: &str = "status.normal";
    /// Status line while in insert mode.
    pub const STATUS_INSERT: &str = "status.insert";
    /// Status line while in command or search mode.
    pub const STATUS_COMMAND: &str = "status.command";
    /// Status line while an error is shown.
    pub const STATUS_ERROR: &str = "status.error";
    /// Informational notification.
    pub const NOTICE_INFO: &str = "notice.info";
    /// Warning notification.
    pub const NOTICE_WARN: &str = "notice.warn";
    /// Error notification.
    pub const NOTICE_ERROR: &str = "notice.error";
    /// Success notification.
    pub const NOTICE_SUCCESS: &str = "notice.success";
    /// Help popup group headings.
    pub const HELP_GROUP: &str = "help.group";
    /// Help popup key column.
    pub const HELP_KEY: &str = "help.key";
    /// Help popup description column.
    pub const HELP_DESCRIPTION: &str = "help.description";
    /// The `:` prompt.
    pub const COMMAND_PROMPT: &str = "command.prompt";
    /// The error shown under the `:` prompt.
    pub const COMMAND_ERROR: &str = "command.error";
    /// The highlighted row in a picker.
    pub const PICKER_SELECTED: &str = "picker.selected";
}

/// Every element name, used to warn about typos in theme files.
pub const KNOWN_ELEMENTS: &[&str] = &[
    element::BG,
    element::FG,
    element::ACCENT,
    element::BORDER,
    element::BORDER_FOCUSED,
    element::TITLE,
    element::CURSOR_LINE,
    element::SELECTION,
    element::MUTED,
    element::STATUS_NORMAL,
    element::STATUS_INSERT,
    element::STATUS_COMMAND,
    element::STATUS_ERROR,
    element::NOTICE_INFO,
    element::NOTICE_WARN,
    element::NOTICE_ERROR,
    element::NOTICE_SUCCESS,
    element::HELP_GROUP,
    element::HELP_KEY,
    element::HELP_DESCRIPTION,
    element::COMMAND_PROMPT,
    element::COMMAND_ERROR,
    element::PICKER_SELECTED,
];

/// A resolved theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    name: String,
    base: Option<String>,
    styles: BTreeMap<String, Style>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "dark".to_owned(),
            base: None,
            styles: dark_styles()
                .into_iter()
                .map(|(name, style)| (name.to_owned(), style))
                .collect(),
        }
    }
}

impl Theme {
    /// A built-in theme, or `None` when `name` is not one.
    #[must_use]
    pub fn builtin(name: &str) -> Option<Self> {
        let styles = match name {
            "dark" => dark_styles(),
            "light" => light_styles(),
            _ => return None,
        };
        Some(Self {
            name: name.to_owned(),
            base: None,
            styles: styles
                .into_iter()
                .map(|(element, style)| (element.to_owned(), style))
                .collect(),
        })
    }

    /// Names of the compiled-in themes.
    #[must_use]
    pub fn builtin_names() -> &'static [&'static str] {
        &["dark", "light"]
    }

    /// The theme's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The theme this one inherits from, if any.
    #[must_use]
    pub fn base(&self) -> Option<&str> {
        self.base.as_deref()
    }

    /// Whether the theme defines this element itself.
    #[must_use]
    pub fn defines(&self, element: &str) -> bool {
        self.styles.contains_key(element)
    }

    /// The style for an element, falling back to the theme's foreground.
    #[must_use]
    pub fn style(&self, element: &str) -> Style {
        self.styles
            .get(element)
            .copied()
            .or_else(|| self.styles.get(element::FG).copied())
            .unwrap_or_default()
    }

    /// The foreground colour for an element.
    #[must_use]
    pub fn color(&self, element: &str) -> Color {
        self.style(element).fg.unwrap_or(Color::Reset)
    }

    fn set(&mut self, element: &str, style: Style) {
        self.styles.insert(element.to_owned(), style);
    }
}

fn dark_styles() -> Vec<(&'static str, Style)> {
    let bg = Color::Rgb(0x0d, 0x11, 0x17);
    let fg = Color::Rgb(0xc9, 0xd1, 0xd9);
    let accent = Color::Rgb(0x58, 0xa6, 0xff);
    let muted = Color::Rgb(0x6e, 0x76, 0x81);
    let border = Color::Rgb(0x30, 0x36, 0x3d);

    vec![
        (element::BG, Style::default().bg(bg).fg(fg)),
        (element::FG, Style::default().fg(fg)),
        (element::ACCENT, Style::default().fg(accent)),
        (element::BORDER, Style::default().fg(border)),
        (element::BORDER_FOCUSED, Style::default().fg(accent)),
        (
            element::TITLE,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        (
            element::CURSOR_LINE,
            Style::default().bg(Color::Rgb(0x16, 0x1b, 0x22)),
        ),
        (
            element::SELECTION,
            Style::default()
                .bg(Color::Rgb(0x1f, 0x6f, 0xeb))
                .fg(Color::Rgb(0xff, 0xff, 0xff))
                .add_modifier(Modifier::BOLD),
        ),
        (element::MUTED, Style::default().fg(muted)),
        (
            element::STATUS_NORMAL,
            Style::default()
                .bg(accent)
                .fg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        (
            element::STATUS_INSERT,
            Style::default()
                .bg(Color::Rgb(0x3f, 0xb9, 0x50))
                .fg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        (
            element::STATUS_COMMAND,
            Style::default()
                .bg(Color::Rgb(0xd2, 0x99, 0x22))
                .fg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        (
            element::STATUS_ERROR,
            Style::default()
                .bg(Color::Rgb(0xf8, 0x51, 0x49))
                .fg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        (element::NOTICE_INFO, Style::default().fg(accent)),
        (
            element::NOTICE_WARN,
            Style::default().fg(Color::Rgb(0xd2, 0x99, 0x22)),
        ),
        (
            element::NOTICE_ERROR,
            Style::default().fg(Color::Rgb(0xf8, 0x51, 0x49)),
        ),
        (
            element::NOTICE_SUCCESS,
            Style::default().fg(Color::Rgb(0x3f, 0xb9, 0x50)),
        ),
        (
            element::HELP_GROUP,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        (
            element::HELP_KEY,
            Style::default().fg(Color::Rgb(0xd2, 0xa8, 0xff)),
        ),
        (element::HELP_DESCRIPTION, Style::default().fg(fg)),
        (
            element::COMMAND_PROMPT,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        (
            element::COMMAND_ERROR,
            Style::default().fg(Color::Rgb(0xf8, 0x51, 0x49)),
        ),
        (
            element::PICKER_SELECTED,
            Style::default()
                .bg(border)
                .fg(fg)
                .add_modifier(Modifier::BOLD),
        ),
    ]
}

fn light_styles() -> Vec<(&'static str, Style)> {
    let bg = Color::Rgb(0xff, 0xff, 0xff);
    let fg = Color::Rgb(0x1f, 0x23, 0x28);
    let accent = Color::Rgb(0x09, 0x69, 0xda);
    let muted = Color::Rgb(0x6e, 0x77, 0x81);
    let border = Color::Rgb(0xd0, 0xd7, 0xde);

    vec![
        (element::BG, Style::default().bg(bg).fg(fg)),
        (element::FG, Style::default().fg(fg)),
        (element::ACCENT, Style::default().fg(accent)),
        (element::BORDER, Style::default().fg(border)),
        (element::BORDER_FOCUSED, Style::default().fg(accent)),
        (
            element::TITLE,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        (
            element::CURSOR_LINE,
            Style::default().bg(Color::Rgb(0xf6, 0xf8, 0xfa)),
        ),
        (
            element::SELECTION,
            Style::default()
                .bg(accent)
                .fg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        (element::MUTED, Style::default().fg(muted)),
        (
            element::STATUS_NORMAL,
            Style::default()
                .bg(accent)
                .fg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        (
            element::STATUS_INSERT,
            Style::default()
                .bg(Color::Rgb(0x1a, 0x7f, 0x37))
                .fg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        (
            element::STATUS_COMMAND,
            Style::default()
                .bg(Color::Rgb(0x9a, 0x67, 0x00))
                .fg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        (
            element::STATUS_ERROR,
            Style::default()
                .bg(Color::Rgb(0xcf, 0x22, 0x2e))
                .fg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        (element::NOTICE_INFO, Style::default().fg(accent)),
        (
            element::NOTICE_WARN,
            Style::default().fg(Color::Rgb(0x9a, 0x67, 0x00)),
        ),
        (
            element::NOTICE_ERROR,
            Style::default().fg(Color::Rgb(0xcf, 0x22, 0x2e)),
        ),
        (
            element::NOTICE_SUCCESS,
            Style::default().fg(Color::Rgb(0x1a, 0x7f, 0x37)),
        ),
        (
            element::HELP_GROUP,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        (
            element::HELP_KEY,
            Style::default().fg(Color::Rgb(0x82, 0x50, 0xdf)),
        ),
        (element::HELP_DESCRIPTION, Style::default().fg(fg)),
        (
            element::COMMAND_PROMPT,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        (
            element::COMMAND_ERROR,
            Style::default().fg(Color::Rgb(0xcf, 0x22, 0x2e)),
        ),
        (
            element::PICKER_SELECTED,
            Style::default()
                .bg(border)
                .fg(fg)
                .add_modifier(Modifier::BOLD),
        ),
    ]
}

/// A user theme file (FR-8.4).
#[derive(Debug, Clone, Deserialize)]
pub struct ThemeFile {
    /// Optional display name; defaults to the file stem.
    pub name: Option<String>,
    /// Theme to inherit from before applying this file's values.
    pub base: Option<String>,
    /// Grouped element styles, e.g. `[diff] add = "#aff5b4"`.
    #[serde(flatten)]
    pub groups: BTreeMap<String, BTreeMap<String, StyleSpec>>,
}

/// One element's style, written either as a single colour or as a table.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StyleSpec {
    /// Shorthand: just a foreground colour.
    Color(String),
    /// Full form.
    Detailed {
        /// Foreground colour.
        fg: Option<String>,
        /// Background colour.
        bg: Option<String>,
        /// Style modifiers such as `bold` or `italic`.
        modifiers: Option<Vec<String>>,
    },
}

/// Everything that stops a theme from loading.
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("could not read theme file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("theme file {path} is not valid TOML: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("theme `{requested}` was not found; available themes: {}", .available.join(", "))]
    NotFound {
        requested: String,
        available: Vec<String>,
    },
    #[error("theme file {path} inherits from unknown base `{base}`")]
    UnknownBase { path: PathBuf, base: String },
}

/// Loads a theme by name: a built-in, or a file in `themes/`.
///
/// Returns the theme and a description of where it came from, for `:doctor`.
///
/// # Errors
///
/// Returns [`ThemeError`] when the theme does not exist, cannot be read, is not
/// valid TOML, or inherits from an unknown base.
pub fn load(
    home: &Home,
    requested: &str,
    warnings: &mut Vec<String>,
) -> Result<(Theme, String), ThemeError> {
    if let Some(theme) = Theme::builtin(requested) {
        return Ok((theme, "built-in".to_owned()));
    }

    let path = theme_path(home, requested);
    if !path.exists() {
        return Err(ThemeError::NotFound {
            requested: requested.to_owned(),
            available: available(home),
        });
    }

    let text = std::fs::read_to_string(&path).map_err(|source| ThemeError::Read {
        path: path.clone(),
        source,
    })?;
    let file: ThemeFile = toml::from_str(&text).map_err(|source| ThemeError::Parse {
        path: path.clone(),
        message: source.to_string(),
    })?;

    let base = file.base.clone().unwrap_or_else(|| "dark".to_owned());
    let mut theme = Theme::builtin(&base).ok_or_else(|| ThemeError::UnknownBase {
        path: path.clone(),
        base: base.clone(),
    })?;
    theme.name = file.name.clone().unwrap_or_else(|| requested.to_owned());
    theme.base = Some(base);
    apply(&mut theme, &file, &path, warnings);

    Ok((theme, path.display().to_string()))
}

fn apply(theme: &mut Theme, file: &ThemeFile, path: &Path, warnings: &mut Vec<String>) {
    for (group, entries) in &file.groups {
        for (key, spec) in entries {
            let element = format!("{group}.{key}");
            if !KNOWN_ELEMENTS.contains(&element.as_str()) {
                warnings.push(format!(
                    "theme: {} defines unknown element `{element}`; it is ignored",
                    path.display()
                ));
                continue;
            }
            if let Some(style) = style_from_spec(spec, &element, path, warnings) {
                // Patch rather than replace: a theme that only sets `colors.bg`
                // must keep the foreground it inherited from its base (FR-8.4).
                let merged = theme.style(&element).patch(style);
                theme.set(&element, merged);
            }
        }
    }
}

fn style_from_spec(
    spec: &StyleSpec,
    element: &str,
    path: &Path,
    warnings: &mut Vec<String>,
) -> Option<Style> {
    match spec {
        StyleSpec::Color(value) => match parse_color(value) {
            // `colors.bg` sets the background; everything else sets the
            // foreground, so `border = "red"` reads the way it looks.
            Ok(color) => Some(if sets_background(element) {
                Style::default().bg(color)
            } else {
                Style::default().fg(color)
            }),
            Err(reason) => {
                warnings.push(format!("theme: {} `{element}` {reason}", path.display()));
                None
            }
        },
        StyleSpec::Detailed { fg, bg, modifiers } => {
            let mut style = Style::default();
            if let Some(value) = fg {
                match parse_color(value) {
                    Ok(color) => style = style.fg(color),
                    Err(reason) => {
                        warnings.push(format!("theme: {} `{element}.fg` {reason}", path.display()));
                    }
                }
            }
            if let Some(value) = bg {
                match parse_color(value) {
                    Ok(color) => style = style.bg(color),
                    Err(reason) => {
                        warnings.push(format!("theme: {} `{element}.bg` {reason}", path.display()));
                    }
                }
            }
            if let Some(names) = modifiers {
                for name in names {
                    match parse_modifier(name) {
                        Some(modifier) => style = style.add_modifier(modifier),
                        None => warnings.push(format!(
                            "theme: {} `{element}` has unknown modifier `{name}`; expected one of \
                             bold, dim, italic, underlined, reversed, crossedout, hidden, blink",
                            path.display()
                        )),
                    }
                }
            }
            Some(style)
        }
    }
}

/// Parses the colour formats a theme file accepts (FR-8.4).
///
/// `ratatui`'s own parser handles names and `#RRGGBB` but not the `#RGB`
/// shorthand, and it reads `indexed:N` as a bare number, so both are handled
/// here to match what the requirements document promises.
fn parse_color(value: &str) -> Result<Color, String> {
    let trimmed = value.trim();
    let invalid = || {
        format!(
            "has invalid colour `{value}`; use a name (red, lightblue), #RRGGBB, #RGB \
             or indexed:N"
        )
    };

    if let Some(index) = trimmed.strip_prefix("indexed:") {
        return index
            .trim()
            .parse::<u8>()
            .map(Color::Indexed)
            .map_err(|_| invalid());
    }

    if let Some(hex) = trimmed.strip_prefix('#')
        && hex.len() == 3
    {
        let expanded: String = hex.chars().flat_map(|digit| [digit, digit]).collect();
        return Color::from_str(&format!("#{expanded}")).map_err(|_| invalid());
    }

    Color::from_str(trimmed).map_err(|_| invalid())
}

/// Whether a shorthand colour value sets the background.
///
/// The rule is the last segment of the element name, so `colors.bg` and any
/// future `diff.bg` both mean the background (FR-8.4).
fn sets_background(element: &str) -> bool {
    element.rsplit('.').next() == Some("bg")
}

fn parse_modifier(name: &str) -> Option<Modifier> {
    match name.to_ascii_lowercase().as_str() {
        "bold" => Some(Modifier::BOLD),
        "dim" => Some(Modifier::DIM),
        "italic" => Some(Modifier::ITALIC),
        "underlined" | "underline" => Some(Modifier::UNDERLINED),
        "reversed" | "reverse" => Some(Modifier::REVERSED),
        "crossedout" | "strikethrough" => Some(Modifier::CROSSED_OUT),
        "hidden" => Some(Modifier::HIDDEN),
        "blink" | "slowblink" => Some(Modifier::SLOW_BLINK),
        _ => None,
    }
}

fn theme_path(home: &Home, name: &str) -> PathBuf {
    home.themes().join(format!("{name}.toml"))
}

/// Theme names the user can choose: built-ins first, then `themes/*.toml`.
pub fn available(home: &Home) -> Vec<String> {
    let mut names: Vec<String> = Theme::builtin_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();

    if let Ok(entries) = std::fs::read_dir(home.themes()) {
        let mut user: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                if path
                    .extension()
                    .is_none_or(|extension| !extension.eq_ignore_ascii_case("toml"))
                {
                    return None;
                }
                path.file_stem()?.to_str().map(str::to_owned)
            })
            .collect();
        user.sort();
        names.extend(user);
    }

    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_home;

    #[test]
    fn built_in_themes_define_every_element() {
        for name in Theme::builtin_names() {
            let theme = Theme::builtin(name).unwrap();
            for element in KNOWN_ELEMENTS {
                assert!(
                    theme.defines(element),
                    "theme {name} is missing element {element}"
                );
            }
        }
    }

    #[test]
    fn unknown_theme_name_is_not_a_builtin() {
        assert!(Theme::builtin("solarized").is_none());
    }

    #[test]
    fn style_of_an_unknown_element_falls_back_to_the_foreground() {
        let theme = Theme::builtin("dark").unwrap();
        assert_eq!(theme.style("nothing.here"), theme.style(element::FG));
    }

    #[test]
    fn a_two_colour_theme_is_usable() {
        let dir = temp_home();
        let home = Home::resolve(Some(dir.path())).unwrap();
        home.ensure().unwrap();
        dir.write(
            "themes/mine.toml",
            r##"
            name = "Mine"
            base = "dark"

            [colors]
            bg = "#101010"
            fg = "#eeeeee"
            "##,
        );
        let mut warnings = Vec::new();
        let (theme, source) = load(&home, "mine", &mut warnings).unwrap();

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(theme.name(), "Mine");
        assert_eq!(theme.base(), Some("dark"));
        assert_eq!(
            theme.style(element::BG).bg,
            Some(Color::Rgb(0x10, 0x10, 0x10))
        );
        // Untouched elements come from the base theme.
        assert!(theme.defines(element::STATUS_NORMAL));
        assert!(source.ends_with("mine.toml"));
    }

    #[test]
    fn colours_can_be_written_in_several_formats() {
        let dir = temp_home();
        let home = Home::resolve(Some(dir.path())).unwrap();
        home.ensure().unwrap();
        dir.write(
            "themes/formats.toml",
            r##"
            base = "dark"

            [colors]
            bg = "black"
            fg = "#fff"

            [ui]
            border = "indexed:240"
            title = { fg = "#58a6ff", modifiers = ["bold", "italic"] }
            "##,
        );
        let mut warnings = Vec::new();
        let (theme, _) = load(&home, "formats", &mut warnings).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(theme.style(element::BG).bg, Some(Color::Black));
        assert_eq!(
            theme.style(element::FG).fg,
            Some(Color::Rgb(0xff, 0xff, 0xff))
        );
        assert_eq!(theme.color(element::BORDER), Color::Indexed(240));
        let title = theme.style(element::TITLE);
        assert!(title.add_modifier.contains(Modifier::BOLD));
        assert!(title.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn an_invalid_colour_is_reported_and_the_rest_still_loads() {
        let dir = temp_home();
        let home = Home::resolve(Some(dir.path())).unwrap();
        home.ensure().unwrap();
        dir.write(
            "themes/broken.toml",
            r##"
            base = "dark"

            [colors]
            bg = "not-a-colour"
            fg = "#ffffff"
            "##,
        );
        let mut warnings = Vec::new();
        let (theme, _) = load(&home, "broken", &mut warnings).unwrap();

        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("not-a-colour"), "{warnings:?}");
        assert!(warnings[0].contains("#RRGGBB"), "{warnings:?}");
        // The good key still applied; the bad one kept the base value.
        assert_eq!(
            theme.style(element::FG).fg,
            Some(Color::Rgb(0xff, 0xff, 0xff))
        );
        assert_eq!(
            theme.style(element::BG).bg,
            Theme::builtin("dark").unwrap().style(element::BG).bg
        );
    }

    #[test]
    fn unknown_elements_and_modifiers_are_reported() {
        let dir = temp_home();
        let home = Home::resolve(Some(dir.path())).unwrap();
        home.ensure().unwrap();
        dir.write(
            "themes/odd.toml",
            r##"
            base = "dark"

            [nonsense]
            whatever = "#ffffff"

            [ui]
            border = { fg = "#ffffff", modifiers = ["sparkly"] }
            "##,
        );
        let mut warnings = Vec::new();
        load(&home, "odd", &mut warnings).unwrap();
        assert!(
            warnings.iter().any(|w| w.contains("unknown element")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("unknown modifier")),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_missing_theme_lists_what_is_available() {
        let dir = temp_home();
        let home = Home::resolve(Some(dir.path())).unwrap();
        home.ensure().unwrap();
        let error = load(&home, "nope", &mut Vec::new()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("dark"), "{message}");
        assert!(message.contains("light"), "{message}");
    }

    #[test]
    fn an_unknown_base_is_an_error() {
        let dir = temp_home();
        let home = Home::resolve(Some(dir.path())).unwrap();
        home.ensure().unwrap();
        dir.write(
            "themes/base.toml",
            "base = \"dracula\"\n[colors]\nfg = \"#ffffff\"\n",
        );
        let error = load(&home, "base", &mut Vec::new()).unwrap_err();
        assert!(error.to_string().contains("dracula"), "{error}");
    }

    #[test]
    fn available_lists_builtins_and_user_files() {
        let dir = temp_home();
        let home = Home::resolve(Some(dir.path())).unwrap();
        home.ensure().unwrap();
        dir.write("themes/custom.toml", "base = \"dark\"\n");
        let names = available(&home);
        assert_eq!(names, vec!["dark", "light", "custom"]);
    }

    #[test]
    fn the_default_theme_is_dark() {
        assert_eq!(Theme::default().name(), "dark");
    }
}
