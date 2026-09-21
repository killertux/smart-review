//! Test-only rendering helpers.

use std::collections::BTreeMap;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use crate::tui::app::{App, Effect};
use crate::tui::event::KeyEvent;
use crate::tui::jobs::{self, Completion};

/// A deterministic reducer/job/render loop for cross-feature scenarios (IR-18).
///
/// The queue is controlled by the test, so completions can be released in any order
/// without sleeping or starting an adapter. The production reducer still validates job
/// ownership and supersession when [`Self::release`] applies each completion.
pub(crate) struct Scenario {
    app: App,
    effect: Effect,
    completions: BTreeMap<u64, Completion>,
}

impl Scenario {
    /// Starts a scenario around real application state.
    pub(crate) fn new(app: App) -> Self {
        Self {
            app,
            effect: Effect::None,
            completions: BTreeMap::new(),
        }
    }

    /// Sends one decoded terminal event through the real reducer.
    pub(crate) fn press(&mut self, event: KeyEvent) -> &Effect {
        self.effect = self.app.on_key(event);
        &self.effect
    }

    /// The latest effect emitted by the reducer.
    pub(crate) const fn effect(&self) -> &Effect {
        &self.effect
    }

    /// Records the latest effect as a submitted production job.
    pub(crate) fn record(&mut self, job: u64) {
        self.app.record_job(&self.effect, job);
    }

    /// Holds a fake job completion until the scenario chooses to release it.
    pub(crate) fn hold(&mut self, completion: Completion) {
        self.completions.insert(completion.job, completion);
    }

    /// Routes the latest effect through the production effect-to-job mapping, then lets
    /// a fake adapter boundary answer that concrete job.
    pub(crate) fn route_with_fake(
        &mut self,
        job: u64,
        fake: impl FnOnce(&jobs::Job) -> jobs::Outcome,
    ) {
        let routed = jobs::job_for(
            &self.effect,
            &self.app.list,
            self.app.doctor_request(),
            &self.app.drafts.draft,
        )
        .expect("the scenario effect should route to a background job");
        let outcome = fake(&routed);
        self.record(job);
        self.hold(Completion {
            job,
            progress_through: 0,
            owner: jobs::JobOwner::Global,
            outcome,
        });
    }

    /// Releases one held completion through the real ownership/supersession reducer.
    pub(crate) fn release(&mut self, job: u64) -> Option<Effect> {
        self.completions
            .remove(&job)
            .and_then(|completion| self.app.apply_completion(completion))
    }

    /// Renders the current state with Ratatui's deterministic in-memory backend.
    pub(crate) fn frame(&mut self, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height))
            .expect("test backend should be constructible");
        terminal
            .draw(|frame| self.app.render(frame))
            .expect("test backend draw should succeed");
        buffer_to_string(terminal.backend().buffer())
    }

    /// Reads the application state for invariants not represented by pixels.
    pub(crate) const fn app(&self) -> &App {
        &self.app
    }
}

/// Renders a `TestBackend` buffer as text, trimming trailing spaces so snapshots
/// and assertions do not depend on how wide the terminal happens to be.
pub(crate) fn buffer_to_string(buffer: &Buffer) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;

    #[test]
    fn trims_trailing_whitespace_and_keeps_newlines() {
        let mut terminal = Terminal::new(TestBackend::new(10, 2)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(Paragraph::new("ab"), frame.area()))
            .unwrap();
        assert_eq!(buffer_to_string(terminal.backend().buffer()), "ab\n\n");
    }
}
