//! Render smoke tests using ratatui's TestBackend.
//!
//! These verify that the UI module produces the expected text output for
//! typical inputs without needing a real terminal.

#[cfg(test)]
mod tests {
    use core::{Buffer, Document, PieceTableBuffer};
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
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
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
    fn project_tree_scrolls_selected_file_into_view() {
        let dir = std::env::temp_dir().join(format!("nova_tui_tree_scroll_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        for index in 0..20 {
            std::fs::write(dir.join(format!("file_{index:02}.rs")), "").unwrap();
        }
        let target = dir.join("file_19.rs");
        let buf: Box<dyn Buffer> =
            Box::new(PieceTableBuffer::from_bytes_with_path(Vec::new(), target));
        let mut app = App::new(buf);
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();

        let out = terminal.backend().to_string();
        assert!(
            out.contains("file_19.rs"),
            "selected tree row should be visible: {out:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
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

    #[test]
    fn status_bar_shows_active_theme_name() {
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::new());
        let app = App::new(buf);
        let theme_name = app.syntax.theme_name().to_string();
        let backend = TestBackend::new(80, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let mut a = app;
                ui::render(frame, &mut a)
            })
            .unwrap();
        let out = terminal.backend().to_string();
        assert!(
            out.contains(&theme_name),
            "expected theme name '{theme_name}' in status bar: {out:?}"
        );
    }

    #[test]
    fn header_shows_tab_strip_with_multiple_docs() {
        // Three named docs → the header becomes a tab strip with all
        // three filenames visible, separated by "│". The active tab is
        // highlighted via a background colour, but visually we just
        // confirm every filename is rendered.
        let docs: Vec<Document> = ["alpha.txt", "beta.rs", "gamma.md"]
            .iter()
            .map(|name| {
                let path = std::path::PathBuf::from(format!("/tmp/{name}"));
                let buf: Box<dyn Buffer> =
                    Box::new(PieceTableBuffer::from_bytes_with_path(b"".to_vec(), path));
                Document::new(buf)
            })
            .collect();
        let mut app = App::new_with_documents(docs, core::Config::default());

        let backend = TestBackend::new(80, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
        let out = terminal.backend().to_string();
        assert!(
            out.contains("alpha.txt"),
            "expected tab 'alpha.txt': {out:?}"
        );
        assert!(out.contains("beta.rs"), "expected tab 'beta.rs': {out:?}");
        assert!(out.contains("gamma.md"), "expected tab 'gamma.md': {out:?}");
        assert!(out.contains("│"), "expected tab separator: {out:?}");
        // The legacy "(1/3)" counter is gone — the tabs themselves
        // communicate position now.
        assert!(
            !out.contains("(1/3)"),
            "counter no longer rendered: {out:?}"
        );

        // Cycle to the middle doc and re-render. Every tab is still
        // visible; only the highlight moves.
        app.handle_event(core::EditorEvent::NextDoc);
        let backend = TestBackend::new(80, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
        let out = terminal.backend().to_string();
        assert!(
            out.contains("alpha.txt"),
            "tabs persist on tab switch: {out:?}"
        );
        assert!(
            out.contains("beta.rs"),
            "tabs persist on tab switch: {out:?}"
        );
        assert!(
            out.contains("gamma.md"),
            "tabs persist on tab switch: {out:?}"
        );
    }

    #[test]
    fn header_hides_tab_strip_when_only_one_doc() {
        // With a single doc, the tab strip is suppressed — we just
        // render the legacy "filename + dirty" header. The single-doc
        // case is the most common one and deserves the quieter UI.
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(b"".to_vec()));
        let mut app = App::new(buf);
        let backend = TestBackend::new(80, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
        let out = terminal.backend().to_string();
        // The header row (first line of output) should just contain
        // "[No Name]" — no tab separator, no other doc names.
        let header_line = out.lines().next().unwrap_or("");
        assert!(
            header_line.contains("[No Name]"),
            "expected single-doc header: {header_line:?}"
        );
        // No tab separator in the single-doc header line. The content
        // area's line-number gutter also uses "│", so we only look at
        // the first (header) line.
        assert!(
            !header_line.contains("│"),
            "single-doc header has no tab strip: {header_line:?}"
        );
    }

    #[test]
    fn tab_strip_shows_dirty_marker() {
        // An unsaved edit on the middle doc shows up as "*" in its tab.
        let docs: Vec<Document> = ["alpha.txt", "beta.rs", "gamma.md"]
            .iter()
            .map(|name| {
                let path = std::path::PathBuf::from(format!("/tmp/{name}"));
                let buf: Box<dyn Buffer> =
                    Box::new(PieceTableBuffer::from_bytes_with_path(b"".to_vec(), path));
                Document::new(buf)
            })
            .collect();
        let mut app = App::new_with_documents(docs, core::Config::default());
        app.active = 1;
        app.handle_event(core::EditorEvent::Insert('x'));
        let backend = TestBackend::new(80, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
        let out = terminal.backend().to_string();
        // "beta" tab should be marked dirty — either "beta.rs*" or "beta.rs *".
        assert!(
            out.contains("beta.rs*") || out.contains("beta.rs *"),
            "expected dirty marker on beta tab: {out:?}"
        );
        // Untouched tabs should NOT have the marker.
        assert!(
            !out.contains("alpha.txt*") && !out.contains("alpha.txt *"),
            "alpha tab is clean: {out:?}"
        );
        assert!(
            !out.contains("gamma.md*") && !out.contains("gamma.md *"),
            "gamma tab is clean: {out:?}"
        );
    }

    // ----- close-on-dirty prompt rendering -----

    #[test]
    fn close_confirm_prompt_renders_when_open() {
        // With a dirty buffer, opening the prompt should add a row
        // showing "has unsaved changes" and the three options.
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "nova_render_close_{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes_with_path(
            b"hello".to_vec(),
            path.clone(),
        ));
        let mut app = App::new(buf);
        app.handle_event(core::EditorEvent::Insert('!'));
        app.handle_event(core::EditorEvent::CloseDoc);

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
        let out = terminal.backend().to_string();
        assert!(
            out.contains("has unsaved changes"),
            "expected prompt copy in: {out:?}"
        );
        assert!(out.contains("Save"), "expected Save option: {out:?}");
        assert!(out.contains("Discard"), "expected Discard option: {out:?}");
        assert!(out.contains("Cancel"), "expected Cancel option: {out:?}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn go_to_line_dialog_renders_when_open() {
        let mut app = App::new(Box::new(PieceTableBuffer::new()));
        app.handle_event(core::EditorEvent::GoToLine(None));
        app.push_go_to_line_query('4');
        app.push_go_to_line_query('2');

        // 6 rows: header + content + status + dialog row.
        let backend = TestBackend::new(80, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
        let out = terminal.backend().to_string();
        assert!(
            out.contains("Go to line: 42"),
            "expected typed line 'Go to line: 42' in output: {out:?}"
        );
    }

    // ----- find-match highlight -----

    #[test]
    fn find_match_is_present_when_search_has_results() {
        // Smoke test: rendering with an active search query that has
        // a match does not panic and produces output containing the
        // matched word. (We can't easily inspect Style objects from a
        // TestBackend, but we can confirm the layout didn't blow up.)
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(
            b"the quick brown fox".to_vec(),
        ));
        let mut app = App::new(buf);
        app.handle_event(core::EditorEvent::FindQueryChanged("quick".to_string()));
        let backend = TestBackend::new(80, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
        let out = terminal.backend().to_string();
        assert!(
            out.contains("quick"),
            "matched word should appear in rendered output: {out:?}"
        );
        assert!(out.contains("the"), "line text still rendered: {out:?}");
    }

    #[test]
    fn find_match_persists_across_multiple_finds() {
        // FindNext on a buffer with two matches should highlight the
        // second match. Visual inspection isn't possible from
        // TestBackend, but `current_match()` reports the byte
        // position — we use that as a proxy to confirm the renderer
        // would have something to highlight.
        let buf: Box<dyn Buffer> =
            Box::new(PieceTableBuffer::from_bytes(b"foo bar foo baz".to_vec()));
        let mut app = App::new(buf);
        app.handle_event(core::EditorEvent::FindQueryChanged("foo".to_string()));
        assert_eq!(app.search.current_match(), Some(0));
        app.handle_event(core::EditorEvent::FindNext);
        assert_eq!(app.search.current_match(), Some(8));
    }

    #[test]
    fn find_with_multiple_matches_renders_without_panic() {
        // Smoke test for highlight-all: with several matches on a
        // line and across lines, the renderer must not panic and the
        // line text must still come through. TestBackend can't see
        // styles, so we just check that the content survives the
        // multi-segment push_highlight_spans path.
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(
            b"foo bar foo baz foo qux foo".to_vec(),
        ));
        let mut app = App::new(buf);
        app.handle_event(core::EditorEvent::FindQueryChanged("foo".to_string()));
        // Four matches on line 0.
        assert_eq!(app.search.matches.len(), 4);
        // Open the find bar so the counter is rendered (the bar
        // shows "1/4" by default).
        app.handle_event(core::EditorEvent::FindOpen);
        let backend = TestBackend::new(80, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
        let out = terminal.backend().to_string();
        assert!(
            out.contains("foo bar foo baz foo qux foo"),
            "line text survives multi-match render: {out:?}"
        );
        assert!(out.contains("1/4"), "find counter still works: {out:?}");
    }

    #[test]
    fn find_match_across_multiple_lines_renders_each_line() {
        // Matches on more than one line — make sure each line's
        // render path still produces output (i.e. the per-line
        // partition_point + iterate doesn't drop a match).
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(
            b"foo here\nand foo there\nfoo again".to_vec(),
        ));
        let mut app = App::new(buf);
        app.handle_event(core::EditorEvent::FindQueryChanged("foo".to_string()));
        assert_eq!(app.search.matches.len(), 3);
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
        let out = terminal.backend().to_string();
        assert!(out.contains("foo here"), "line 0 rendered: {out:?}");
        assert!(out.contains("and foo there"), "line 1 rendered: {out:?}");
        assert!(out.contains("foo again"), "line 2 rendered: {out:?}");
    }

    // ----- replace bar -----

    #[test]
    fn replace_bar_renders_when_open() {
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(b"hello".to_vec()));
        let mut app = App::new(buf);
        app.handle_event(core::EditorEvent::FindOpen);
        app.handle_event(core::EditorEvent::ReplaceOpen);
        app.search.replace_query = "world".to_string();
        // 7 rows: header + content + status + find + replace rows.
        let backend = TestBackend::new(80, 7);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
        let out = terminal.backend().to_string();
        assert!(out.contains("Find:"), "find bar still renders: {out:?}");
        assert!(
            out.contains("Replace: world"),
            "replace bar with query rendered: {out:?}"
        );
    }

    #[test]
    fn replace_bar_shows_empty_placeholder() {
        let buf: Box<dyn Buffer> = Box::new(PieceTableBuffer::from_bytes(b"hello".to_vec()));
        let mut app = App::new(buf);
        app.handle_event(core::EditorEvent::FindOpen);
        app.handle_event(core::EditorEvent::ReplaceOpen);
        // Replace query is empty — should render with "(empty)" hint.
        let backend = TestBackend::new(80, 7);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
        let out = terminal.backend().to_string();
        assert!(
            out.contains("(empty)"),
            "empty replace shows placeholder: {out:?}"
        );
    }
}
