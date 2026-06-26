//! Render smoke tests using ratatui's TestBackend.
//!
//! These verify that the UI module produces the expected text output for
//! typical inputs without needing a real terminal.

#[cfg(test)]
mod tests {
    use core::{Buffer, PieceTableBuffer};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::app::App;
    use crate::ui;

    fn render_to_string(content: &str, width: u16, height: u16) -> String {
        let buf: Box<dyn Buffer> =
            Box::new(PieceTableBuffer::from_bytes(content.as_bytes().to_vec()));
        let mut app = App::new(buf);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| ui::render(frame, &mut app))
            .unwrap();
        terminal.backend().to_string()
    }

    #[test]
    fn renders_line_numbers_and_text() {
        let out = render_to_string("hello\nworld", 40, 10);
        assert!(out.contains("hello"), "expected 'hello' in: {out:?}");
        assert!(out.contains("world"), "expected 'world' in: {out:?}");
        assert!(out.contains("│"), "expected gutter separator in: {out:?}");
        // Line numbers should appear.
        assert!(out.contains("1"), "expected line '1' in: {out:?}");
        assert!(out.contains("2"), "expected line '2' in: {out:?}");
    }

    #[test]
    fn renders_dirty_indicator_when_modified() {
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(b"x".to_vec()));
        let mut app = App::new(buf);
        app.handle_event(core::EditorEvent::Insert('y'));
        let backend = TestBackend::new(40, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
        let out = terminal.backend().to_string();
        assert!(out.contains("[+]"), "expected dirty marker: {out:?}");
    }

    #[test]
    fn renders_clean_when_no_changes() {
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(b"x".to_vec()));
        let app = App::new(buf);
        let backend = TestBackend::new(40, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let mut a = app;
                ui::render(frame, &mut a)
            })
            .unwrap();
        let out = terminal.backend().to_string();
        assert!(!out.contains("[+]"), "expected no dirty marker: {out:?}");
    }

    #[test]
    fn status_message_appears_in_bottom_line() {
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::new());
        let mut app = App::new(buf);
        app.status_message = Some("Saved.".to_string());
        let backend = TestBackend::new(40, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
        let out = terminal.backend().to_string();
        assert!(out.contains("Saved."), "expected status message: {out:?}");
    }
}