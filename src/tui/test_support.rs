//! Test-only rendering helpers.

use ratatui::buffer::Buffer;

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
