//! The model picker: provider → model → thinking → key → confirm (FR-4.5, FR-4.8).
//!
//! The component owns the *steps*, the cursor and the filter text; the app owns the
//! data and hands it rows. That split is deliberate: every list here comes from the
//! catalog (which the app fetched and may have failed to fetch), every commit needs
//! an effect (save the key, write the config, call the provider), and a component
//! that did both would be a second application layer inside the presentation layer.
//!
//! Keys follow the rule the rest of the app set: inside a text-entry step, ordinary
//! characters are text, and only modified combinations (`<C-…>`, `<A-…>`) are
//! bindings. So navigation is the arrows and `<C-n>`/`<C-p>`, and `j` types a `j`
//! into the search box — the alternative is a search box you cannot type `j` into.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::domain::model::Thinking;
use crate::tui::layout;
use crate::tui::theme::{Theme, element};

/// Which step the picker is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Choose a provider (FR-4.7).
    Provider,
    /// Choose a model, searchable (FR-4.7).
    Model,
    /// Choose a thinking mode, constrained by the model (FR-4.8).
    Thinking,
    /// Type the key, masked (FR-4.5).
    Key,
    /// Review and commit (FR-4.5).
    Confirm,
}

impl Step {
    /// The title shown at the top of the box.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Provider => "Provider",
            Self::Model => "Model",
            Self::Thinking => "Thinking",
            Self::Key => "API key",
            Self::Confirm => "Confirm",
        }
    }

    /// Whether this step takes typed text.
    #[must_use]
    pub fn takes_text(self) -> bool {
        matches!(self, Self::Provider | Self::Model | Self::Key)
    }
}

/// What a row stands for.
#[derive(Debug, Clone, PartialEq)]
pub enum RowChoice {
    /// A catalog provider id.
    Provider(String),
    /// A model id.
    Model(String),
    /// A thinking setting.
    Thinking(Thinking),
}

/// One row of the current step's list.
#[derive(Debug, Clone, PartialEq)]
pub struct PickerRow {
    /// What the row means, which is what Enter acts on.
    pub choice: RowChoice,
    /// The main text.
    pub label: String,
    /// The right-hand text: capabilities, route, cost (FR-4.7).
    pub detail: String,
    /// Why the row cannot be chosen, when it cannot (FR-4.8).
    pub disabled: Option<String>,
}

impl PickerRow {
    /// A selectable row.
    #[must_use]
    pub fn new(choice: RowChoice, label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            choice,
            label: label.into(),
            detail: detail.into(),
            disabled: None,
        }
    }

    /// The same row, refused with a reason (FR-4.8).
    #[must_use]
    pub fn refused(mut self, reason: impl Into<String>) -> Self {
        self.disabled = Some(reason.into());
        self
    }
}

/// What pressing Enter produced.
#[derive(Debug, Clone, PartialEq)]
pub enum PickerStep {
    /// The picker moved on; nothing for the event loop to do.
    Moved,
    /// The user committed a selection: save it, then check it (FR-4.5).
    Commit {
        /// Provider id.
        provider: String,
        /// Model id.
        model: String,
        /// Thinking setting, already checked against the model (FR-4.8).
        thinking: Option<Thinking>,
    },
    /// The user finished typing a key: store it, then continue (FR-4.5).
    KeyTyped {
        /// Which provider.
        provider: String,
        /// The key, to be stored and never shown again.
        key: String,
    },
    /// Nothing happened: the row was refused, or the list was empty.
    Refused(String),
}

/// The picker's state.
///
/// `Debug` is written by hand because the struct holds a key: a derived one would put
/// a secret in any trace, panic message or snapshot that formats this (NFR-3.1).
#[derive(Clone)]
pub struct PickerState {
    step: Step,
    query: String,
    cursor: usize,
    rows: Vec<PickerRow>,
    provider: Option<String>,
    model: Option<String>,
    thinking: Option<Thinking>,
    key_input: String,
    key_present: bool,
    checking: bool,
    notice: Option<String>,
}

impl std::fmt::Debug for PickerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PickerState")
            .field("step", &self.step)
            .field("query", &self.query)
            .field("cursor", &self.cursor)
            .field("rows", &self.rows.len())
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("thinking", &self.thinking)
            .field(
                "key_input",
                &format!("<{} characters, redacted>", self.key_input.len()),
            )
            .field("key_present", &self.key_present)
            .field("checking", &self.checking)
            .field("notice", &self.notice)
            .finish()
    }
}

impl Default for PickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerState {
    /// A picker at the first step.
    #[must_use]
    pub fn new() -> Self {
        Self {
            step: Step::Provider,
            query: String::new(),
            cursor: 0,
            rows: Vec::new(),
            provider: None,
            model: None,
            thinking: None,
            key_input: String::new(),
            key_present: false,
            checking: false,
            notice: None,
        }
    }

    /// The current step.
    #[must_use]
    pub fn step(&self) -> Step {
        self.step
    }

    /// The filter text.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The row the cursor is on.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The rows to draw.
    #[must_use]
    pub fn rows(&self) -> &[PickerRow] {
        &self.rows
    }

    /// The provider chosen so far.
    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    /// The model chosen so far.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// The thinking setting chosen so far.
    #[must_use]
    pub fn thinking(&self) -> Option<&Thinking> {
        self.thinking.as_ref()
    }

    /// How many characters of a key have been typed, never the key itself.
    #[must_use]
    pub fn key_len(&self) -> usize {
        self.key_input.len()
    }

    /// Whether a check is in flight.
    #[must_use]
    pub fn is_checking(&self) -> bool {
        self.checking
    }

    /// The message under the list.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Replaces the rows, keeping the cursor inside them.
    pub fn set_rows(&mut self, rows: Vec<PickerRow>) {
        self.rows = rows;
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    /// Records whether a key already exists for the chosen provider.
    pub fn set_key_present(&mut self, present: bool) {
        self.key_present = present;
    }

    /// Shows a message, or clears it.
    pub fn set_notice(&mut self, notice: Option<String>) {
        self.notice = notice;
    }

    /// Marks the connection check as running or finished.
    pub fn set_checking(&mut self, checking: bool) {
        self.checking = checking;
    }

    /// Moves the cursor, clamping at both ends.
    ///
    /// Arithmetic in `i64` so a list of any length is safe on any target, and a
    /// movement that runs off either end stops there rather than wrapping.
    pub fn move_cursor(&mut self, delta: i32) {
        if self.rows.is_empty() {
            self.cursor = 0;
            return;
        }
        let last = i64::try_from(self.rows.len() - 1).unwrap_or(i64::MAX);
        let current = i64::try_from(self.cursor).unwrap_or(0);
        let next = (current + i64::from(delta)).clamp(0, last);
        self.cursor = usize::try_from(next).unwrap_or(0);
    }

    /// Adds a character to whatever this step is typing.
    pub fn push_char(&mut self, character: char) {
        match self.step {
            Step::Key => self.key_input.push(character),
            Step::Provider | Step::Model => self.query.push(character),
            Step::Thinking | Step::Confirm => {}
        }
        if self.step != Step::Key {
            self.cursor = 0;
        }
    }

    /// Removes the last character.
    pub fn backspace(&mut self) {
        match self.step {
            Step::Key => {
                self.key_input.pop();
            }
            Step::Provider | Step::Model => {
                self.query.pop();
                self.cursor = 0;
            }
            Step::Thinking | Step::Confirm => {}
        }
    }

    /// Clears the search box.
    pub fn clear_query(&mut self) {
        if self.step != Step::Key {
            self.query.clear();
            self.cursor = 0;
        }
    }

    /// Goes back one step.
    ///
    /// Returns `false` when there is nowhere to go, which is the caller's signal to
    /// close the picker (FR-4.5: `Esc` backs out without changing anything).
    pub fn step_back(&mut self) -> bool {
        self.notice = None;
        match self.step {
            Step::Provider => return false,
            Step::Model => {
                self.step = Step::Provider;
                self.provider = None;
                self.query.clear();
            }
            Step::Thinking => {
                self.step = Step::Model;
                self.model = None;
                self.thinking = None;
                self.query.clear();
            }
            Step::Key => {
                // A key step that was not reached from the confirm step backs out to
                // the model list; one that was backs out to the confirm step. The
                // difference is whether a selection exists to return to.
                self.step = if self.model.is_some() {
                    Step::Confirm
                } else {
                    Step::Model
                };
                self.key_input.clear();
            }
            Step::Confirm => self.step = Step::Thinking,
        }
        self.cursor = 0;
        true
    }

    /// Moves on from the key step, which the app calls once the key is stored.
    pub fn key_stored(&mut self) {
        self.key_input.clear();
        self.key_present = true;
        self.step = Step::Confirm;
        self.cursor = 0;
        self.notice = Some("key saved to credentials.toml (mode 0600)".to_owned());
    }

    /// What Enter does from here.
    ///
    /// `key_present` is decided by the app, which is the only thing that can look at
    /// the credential store.
    pub fn advance(&mut self) -> PickerStep {
        let Some(row) = self.rows.get(self.cursor).cloned() else {
            return PickerStep::Refused("nothing is selected".to_owned());
        };
        if let Some(reason) = &row.disabled {
            self.notice = Some(reason.clone());
            return PickerStep::Refused(reason.clone());
        }
        self.notice = None;
        match (self.step, row.choice) {
            (Step::Provider, RowChoice::Provider(id)) => {
                self.provider = Some(id);
                self.step = Step::Model;
                self.query.clear();
                self.cursor = 0;
                PickerStep::Moved
            }
            (Step::Model, RowChoice::Model(id)) => {
                self.model = Some(id);
                self.step = Step::Thinking;
                self.query.clear();
                self.cursor = 0;
                PickerStep::Moved
            }
            (Step::Thinking, RowChoice::Thinking(thinking)) => {
                self.thinking = Some(thinking);
                self.step = if self.key_present {
                    Step::Confirm
                } else {
                    Step::Key
                };
                self.cursor = 0;
                PickerStep::Moved
            }
            (Step::Key, _) => {
                let Some(provider) = self.provider.clone() else {
                    return PickerStep::Refused("choose a provider first".to_owned());
                };
                if self.key_input.trim().is_empty() {
                    self.notice = Some("type or paste the key first".to_owned());
                    return PickerStep::Refused("the key is empty".to_owned());
                }
                PickerStep::KeyTyped {
                    provider,
                    key: self.key_input.clone(),
                }
            }
            (Step::Confirm, _) => match (&self.provider, &self.model) {
                (Some(provider), Some(model)) => PickerStep::Commit {
                    provider: provider.clone(),
                    model: model.clone(),
                    thinking: self.thinking.clone(),
                },
                _ => PickerStep::Refused("choose a model first".to_owned()),
            },
            // A row that belongs to another step cannot happen: the app only ever
            // fills the rows for the current step. Refuse rather than panic.
            _ => PickerStep::Refused("that choice does not belong on this step".to_owned()),
        }
    }

    /// Draws the picker.
    pub fn render(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        checking_label: Option<&str>,
    ) {
        let width = area.width.saturating_sub(8).clamp(40, 88);
        let height = area.height.saturating_sub(4).clamp(10, 22);
        let box_area = layout::centered(area, width, height);
        frame.render_widget(Clear, box_area);

        let title = format!(" {} — model ", self.step.title());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.style(element::BORDER_FOCUSED))
            .title(Span::styled(title, theme.style(element::TITLE)));
        let inner = block.inner(box_area);
        frame.render_widget(block, box_area);

        // A filter line on the steps that have one, a masked indicator on the key
        // step, and the choice so far everywhere.
        let header = if self.step == Step::Key {
            Line::from(vec![
                Span::styled("key: ", theme.style(element::ACCENT)),
                Span::raw("•".repeat(self.key_input.chars().count())),
                Span::styled(
                    format!("  ({} characters)", self.key_input.chars().count()),
                    theme.style(element::MUTED),
                ),
            ])
        } else {
            {
                let mut spans = vec![Span::styled("filter: ", theme.style(element::ACCENT))];
                spans.push(Span::raw(self.query.clone()));
                if let Some(provider) = &self.provider {
                    spans.push(Span::styled(
                        format!("   provider: {provider}"),
                        theme.style(element::MUTED),
                    ));
                }
                if let Some(model) = &self.model {
                    spans.push(Span::styled(
                        format!("   model: {model}"),
                        theme.style(element::MUTED),
                    ));
                }
                Line::from(spans)
            }
        };

        let rows: Vec<ListItem<'_>> = self
            .rows
            .iter()
            .map(|row| {
                let mut spans = vec![Span::raw(row.label.clone())];
                if !row.detail.is_empty() {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        row.detail.clone(),
                        theme.style(element::MUTED),
                    ));
                }
                if let Some(reason) = &row.disabled {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        format!("unavailable: {reason}"),
                        theme.style(element::NOTICE_WARN),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let mut state = ListState::default();
        if !self.rows.is_empty() {
            state.select(Some(self.cursor));
        }
        let list = List::new(rows)
            .highlight_style(theme.style(element::PICKER_SELECTED))
            .highlight_symbol("› ");
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(inner);
        frame.render_widget(Paragraph::new(header), chunks[0]);
        frame.render_stateful_widget(list, chunks[1], &mut state);

        let mut footer = Vec::new();
        if let Some(label) = checking_label {
            footer.push(Line::from(Span::styled(
                label.to_owned(),
                theme.style(element::NOTICE_INFO),
            )));
        } else if let Some(notice) = &self.notice {
            footer.push(Line::from(Span::styled(
                notice.clone(),
                theme.style(element::MUTED),
            )));
        }
        let empty = self.rows.is_empty() && self.step != Step::Key;
        footer.push(Line::from(Span::styled(
            if empty {
                "nothing matches; <C-u> clears the filter".to_owned()
            } else {
                match self.step {
                    Step::Key => "type or paste the key · Enter saves · Esc goes back".to_owned(),
                    Step::Confirm => {
                        "Enter saves the choice and checks it with the provider · Esc goes back"
                            .to_owned()
                    }
                    _ => "<C-n>/<C-p> or ↑/↓ move · Enter chooses · Esc goes back".to_owned(),
                }
            },
            theme.style(element::MUTED),
        )));
        frame.render_widget(Paragraph::new(footer), chunks[2]);
    }
}

/// The rows for the provider step: reachable providers only (FR-4.7).
#[must_use]
pub fn provider_rows(
    state: &crate::application::models::CatalogState,
    query: &str,
) -> Vec<PickerRow> {
    state
        .providers
        .iter()
        .filter(|provider| {
            query.trim().is_empty()
                || crate::fuzzy::matches_all_words(query, &provider.label)
                || crate::fuzzy::matches_all_words(query, &provider.id)
        })
        .map(|provider| {
            PickerRow::new(
                RowChoice::Provider(provider.id.clone()),
                provider.label.clone(),
                format!(
                    "{} · {} models",
                    provider.route_label(),
                    provider.model_count
                ),
            )
        })
        .collect()
}

/// The rows for the model step, ranked by the shared search (FR-4.7).
#[must_use]
pub fn model_rows(
    state: &crate::application::models::CatalogState,
    provider: &str,
    query: &str,
    now_year: u32,
) -> Vec<PickerRow> {
    crate::application::models::model_choices(&state.load.catalog, provider, query, now_year)
        .into_iter()
        .map(|model| {
            let mut detail = model.badges;
            if model.dated {
                detail.push_str(" · older");
            }
            if model.thinking.is_empty() {
                detail.push_str(" · no thinking control");
            }
            PickerRow::new(RowChoice::Model(model.id.clone()), model.label, detail)
        })
        .collect()
}

/// The rows for the thinking step, with refusals shown rather than hidden (FR-4.8).
#[must_use]
pub fn thinking_rows(
    state: &crate::application::models::CatalogState,
    provider: &str,
    model: &str,
) -> Vec<PickerRow> {
    let Some(entry) = state.load.catalog.model(provider, model) else {
        return Vec::new();
    };
    let mut rows: Vec<PickerRow> = entry
        .thinking_choices()
        .into_iter()
        .map(|choice| {
            let detail = if choice.needs_input {
                "asks for a token count"
            } else {
                ""
            };
            match choice.refusal {
                Some(reason) => {
                    PickerRow::new(RowChoice::Thinking(choice.thinking), choice.label, detail)
                        .refused(reason)
                }
                None => PickerRow::new(RowChoice::Thinking(choice.thinking), choice.label, detail),
            }
        })
        .collect();
    if rows.is_empty() {
        // A model that does not reason has no controls at all. Saying so is better
        // than an empty list with no explanation (FR-4.8).
        rows.push(
            PickerRow::new(
                RowChoice::Thinking(Thinking::Toggle { value: false }),
                "off",
                "this model declares no thinking controls",
            )
            .refused("the model does not offer a thinking control"),
        );
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<PickerRow> {
        vec![
            PickerRow::new(
                RowChoice::Provider("deepseek".to_owned()),
                "DeepSeek",
                "native",
            ),
            PickerRow::new(
                RowChoice::Provider("lmstudio".to_owned()),
                "LM Studio",
                "compatible",
            ),
        ]
    }

    fn thinking_rows() -> Vec<PickerRow> {
        vec![
            PickerRow::new(
                RowChoice::Thinking(Thinking::Effort {
                    value: "high".to_owned(),
                }),
                "high",
                "",
            ),
            PickerRow::new(
                RowChoice::Thinking(Thinking::Effort {
                    value: "max".to_owned(),
                }),
                "max",
                "",
            )
            .refused("cannot send effort `max`"),
        ]
    }

    #[test]
    fn the_steps_run_provider_model_thinking_then_confirm() {
        let mut picker = PickerState::new();
        assert_eq!(picker.step(), Step::Provider);
        picker.set_key_present(true);
        picker.set_rows(rows());
        assert_eq!(picker.advance(), PickerStep::Moved);
        assert_eq!(picker.provider(), Some("deepseek"));
        assert_eq!(picker.step(), Step::Model);

        picker.set_rows(vec![PickerRow::new(
            RowChoice::Model("deepseek-v4-pro".to_owned()),
            "DeepSeek V4 Pro",
            "reasoning",
        )]);
        assert_eq!(picker.advance(), PickerStep::Moved);
        assert_eq!(picker.model(), Some("deepseek-v4-pro"));
        assert_eq!(picker.step(), Step::Thinking);

        picker.set_rows(thinking_rows());
        assert_eq!(picker.advance(), PickerStep::Moved);
        assert_eq!(
            picker.thinking(),
            Some(&Thinking::Effort {
                value: "high".to_owned()
            })
        );
        assert_eq!(picker.step(), Step::Confirm, "a key exists, so no key step");

        assert_eq!(
            picker.advance(),
            PickerStep::Commit {
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-pro".to_owned(),
                thinking: Some(Thinking::Effort {
                    value: "high".to_owned()
                }),
            }
        );
    }

    #[test]
    fn a_missing_key_sends_the_picker_to_the_masked_step() {
        let mut picker = PickerState::new();
        picker.set_rows(rows());
        picker.advance();
        picker.set_rows(vec![PickerRow::new(
            RowChoice::Model("m".to_owned()),
            "M",
            "",
        )]);
        picker.advance();
        picker.set_rows(thinking_rows());
        picker.advance();
        assert_eq!(picker.step(), Step::Key);

        // The key is never readable through the state.
        for character in "sk-secret".chars() {
            picker.push_char(character);
        }
        assert_eq!(picker.key_len(), 9);
        assert!(!format!("{picker:?}").contains("sk-secret"));
        assert_eq!(
            picker.advance(),
            PickerStep::KeyTyped {
                provider: "deepseek".to_owned(),
                key: "sk-secret".to_owned()
            }
        );

        picker.key_stored();
        assert_eq!(picker.step(), Step::Confirm);
        assert_eq!(picker.key_len(), 0, "the buffer does not outlive the save");
    }

    #[test]
    fn an_empty_key_is_refused_rather_than_saved() {
        let mut picker = PickerState::new();
        picker.set_rows(rows());
        picker.advance();
        picker.set_rows(vec![PickerRow::new(
            RowChoice::Model("m".to_owned()),
            "M",
            "",
        )]);
        picker.advance();
        picker.set_rows(thinking_rows());
        picker.advance();
        assert!(matches!(picker.advance(), PickerStep::Refused(_)));
        assert_eq!(picker.step(), Step::Key, "still on the key step");
        assert!(
            picker
                .notice()
                .unwrap_or_default()
                .contains("type or paste")
        );
    }

    #[test]
    fn a_refused_row_explains_itself_and_does_not_move_on() {
        let mut picker = PickerState::new();
        picker.set_rows(thinking_rows());
        picker.provider = Some("deepseek".to_owned());
        picker.model = Some("m".to_owned());
        picker.step = Step::Thinking;
        picker.move_cursor(1);
        assert_eq!(picker.cursor(), 1);
        let outcome = picker.advance();
        assert!(matches!(outcome, PickerStep::Refused(_)), "{outcome:?}");
        assert_eq!(picker.step(), Step::Thinking);
        assert_eq!(picker.thinking(), None, "nothing was chosen");
        assert!(
            picker.notice().unwrap_or_default().contains("cannot send"),
            "{:?}",
            picker.notice()
        );
    }

    #[test]
    fn escape_walks_back_one_step_and_then_closes() {
        let mut picker = PickerState::new();
        picker.set_key_present(true);
        picker.set_rows(rows());
        picker.advance();
        picker.set_rows(vec![PickerRow::new(
            RowChoice::Model("m".to_owned()),
            "M",
            "",
        )]);
        picker.advance();
        assert_eq!(picker.step(), Step::Thinking);

        assert!(picker.step_back());
        assert_eq!(picker.step(), Step::Model);
        assert!(picker.model().is_none(), "the model choice was dropped");
        assert!(picker.step_back());
        assert_eq!(picker.step(), Step::Provider);
        assert!(picker.provider().is_none());
        assert!(!picker.step_back(), "the caller closes the picker now");
    }

    #[test]
    fn text_goes_into_the_filter_and_resets_the_cursor() {
        let mut picker = PickerState::new();
        picker.set_rows(rows());
        picker.move_cursor(1);
        assert_eq!(picker.cursor(), 1);
        picker.push_char('d');
        assert_eq!(picker.query(), "d");
        assert_eq!(picker.cursor(), 0, "a new filter starts at the top");
        picker.push_char('e');
        assert_eq!(picker.query(), "de");
        picker.backspace();
        assert_eq!(picker.query(), "d");
        picker.clear_query();
        assert_eq!(picker.query(), "");
    }

    #[test]
    fn on_the_key_step_letters_are_key_material_not_filter_text() {
        let mut picker = PickerState::new();
        picker.step = Step::Key;
        picker.push_char('j');
        picker.push_char('k');
        assert_eq!(picker.query(), "", "the filter is untouched");
        assert_eq!(picker.key_len(), 2);
        picker.clear_query();
        assert_eq!(picker.key_len(), 2, "and clearing does not eat the key");
    }

    #[test]
    fn the_cursor_cannot_leave_the_list() {
        let mut picker = PickerState::new();
        picker.set_rows(rows());
        picker.move_cursor(-1);
        assert_eq!(picker.cursor(), 0);
        picker.move_cursor(5);
        assert_eq!(picker.cursor(), 1, "clamped to the last row");
        picker.set_rows(Vec::new());
        picker.move_cursor(1);
        assert_eq!(picker.cursor(), 0, "an empty list has no position");
    }

    #[test]
    fn shrinking_the_list_keeps_the_cursor_inside_it() {
        let mut picker = PickerState::new();
        picker.set_rows(rows());
        picker.move_cursor(1);
        picker.set_rows(vec![PickerRow::new(
            RowChoice::Provider("only".to_owned()),
            "Only",
            "",
        )]);
        assert_eq!(picker.cursor(), 0);
    }

    #[test]
    fn advancing_with_an_empty_list_says_so() {
        let mut picker = PickerState::new();
        assert!(matches!(picker.advance(), PickerStep::Refused(_)));
    }
}
