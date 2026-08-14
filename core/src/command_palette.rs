//! Command palette data model.
//!
//! The command palette is a fuzzy-searchable list of editor commands.
//! Each command maps to an [`EditorEvent`] that the frontends execute.

use crate::EditorEvent;

/// A command exposed through the command palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// Short, unique identifier used for scoring and execution.
    pub id: &'static str,
    /// Human-readable label shown in the palette.
    pub label: &'static str,
    /// Optional keybinding hint shown next to the label.
    pub keybinding: &'static str,
    /// The editor event emitted when the command is executed.
    pub event: EditorEvent,
}

impl Command {
    /// Binding hint for the active preset. Empty means palette-only.
    pub fn keybinding_for(&self, mode: crate::KeybindingMode) -> &'static str {
        use crate::KeybindingMode::{Emacs, Standard, Vim};
        match (mode, self.id) {
            (Standard, _) => self.keybinding,
            (Vim, "save") => ":w",
            (Vim, "open_file") => ":e",
            (Vim, "close_doc" | "quit") => ":q",
            (Vim, "undo") => "u",
            (Vim, "redo") => "Ctrl+R",
            (Vim, "find") => "/",
            (Emacs, "save") => "Ctrl+X Ctrl+S",
            (Emacs, "save_as") => "Ctrl+X Ctrl+W",
            (Emacs, "open_file") => "Ctrl+X Ctrl+F",
            (Emacs, "close_doc") => "Ctrl+X K",
            (Emacs, "next_doc") => "Ctrl+X Right",
            (Emacs, "prev_doc") => "Ctrl+X Left",
            (Emacs, "undo") => "Ctrl+/",
            (Emacs, "find") => "Ctrl+S",
            (Emacs, "quit") => "Ctrl+X Ctrl+C",
            _ => "",
        }
    }
}

/// All commands available in the palette. Order is the default order
/// before filtering.
pub const COMMANDS: &[Command] = &[
    Command {
        id: "keybindings_standard",
        label: "Keybindings: Standard",
        keybinding: "",
        event: EditorEvent::SetKeybindingMode(crate::KeybindingMode::Standard),
    },
    Command {
        id: "keybindings_vim",
        label: "Keybindings: Vim",
        keybinding: "",
        event: EditorEvent::SetKeybindingMode(crate::KeybindingMode::Vim),
    },
    Command {
        id: "keybindings_emacs",
        label: "Keybindings: Emacs",
        keybinding: "",
        event: EditorEvent::SetKeybindingMode(crate::KeybindingMode::Emacs),
    },
    Command {
        id: "open_file",
        label: "Open file...",
        keybinding: "Ctrl+O",
        event: EditorEvent::OpenFile(None),
    },
    Command {
        id: "save",
        label: "Save",
        keybinding: "Ctrl+S",
        event: EditorEvent::Save,
    },
    Command {
        id: "save_as",
        label: "Save As...",
        keybinding: "Ctrl+Shift+S",
        event: EditorEvent::SaveAs(None),
    },
    Command {
        id: "new_doc",
        label: "New document",
        keybinding: "Ctrl+N",
        event: EditorEvent::NewDoc,
    },
    Command {
        id: "close_doc",
        label: "Close document",
        keybinding: "Ctrl+W",
        event: EditorEvent::CloseDoc,
    },
    Command {
        id: "next_doc",
        label: "Next document",
        keybinding: "Ctrl+Tab",
        event: EditorEvent::NextDoc,
    },
    Command {
        id: "prev_doc",
        label: "Previous document",
        keybinding: "Ctrl+Shift+Tab",
        event: EditorEvent::PrevDoc,
    },
    Command {
        id: "undo",
        label: "Undo",
        keybinding: "Ctrl+Z",
        event: EditorEvent::Undo,
    },
    Command {
        id: "redo",
        label: "Redo",
        keybinding: "Ctrl+Y",
        event: EditorEvent::Redo,
    },
    Command {
        id: "find",
        label: "Find in file",
        keybinding: "Ctrl+F",
        event: EditorEvent::FindOpen,
    },
    Command {
        id: "replace",
        label: "Replace in file",
        keybinding: "Ctrl+R",
        event: EditorEvent::ReplaceOpen,
    },
    Command {
        id: "project_search",
        label: "Project search",
        keybinding: "Ctrl+Shift+F",
        event: EditorEvent::ProjectSearch(None),
    },
    Command {
        id: "fuzzy_finder",
        label: "Fuzzy finder",
        keybinding: "Ctrl+P",
        event: EditorEvent::FuzzyFinder(None),
    },
    Command {
        id: "toggle_git_gutter",
        label: "Toggle git gutter",
        keybinding: "",
        event: EditorEvent::ToggleGitGutter,
    },
    Command {
        id: "toggle_git_blame",
        label: "Toggle git blame",
        keybinding: "",
        event: EditorEvent::ToggleGitBlame,
    },
    Command {
        id: "refresh_git_gutter",
        label: "Refresh git gutter",
        keybinding: "",
        event: EditorEvent::RefreshGitGutter,
    },
    Command {
        id: "next_hunk",
        label: "Next hunk",
        keybinding: "Ctrl+Shift+Down",
        event: EditorEvent::NextHunk,
    },
    Command {
        id: "prev_hunk",
        label: "Previous hunk",
        keybinding: "Ctrl+Shift+Up",
        event: EditorEvent::PrevHunk,
    },
    Command {
        id: "go_to_line",
        label: "Go to line...",
        keybinding: "Ctrl+G",
        event: EditorEvent::GoToLine(None),
    },
    Command {
        id: "go_to_symbol",
        label: "Go to symbol...",
        keybinding: "Ctrl+Shift+O",
        event: EditorEvent::GoToSymbol,
    },
    Command {
        id: "toggle_project_tree",
        label: "Toggle project tree",
        keybinding: "F8",
        event: EditorEvent::ToggleProjectTree,
    },
    Command {
        id: "toggle_soft_wrap",
        label: "Toggle soft wrap",
        keybinding: "Ctrl+Shift+W",
        event: EditorEvent::ToggleSoftWrap,
    },
    Command {
        id: "toggle_minimap",
        label: "Toggle minimap",
        keybinding: "",
        event: EditorEvent::ToggleMinimap,
    },
    Command {
        id: "cycle_markdown_preview",
        label: "Cycle Markdown preview",
        keybinding: "",
        event: EditorEvent::CycleMarkdownPreview,
    },
    Command {
        id: "cycle_indent_mode",
        label: "Cycle indent mode",
        keybinding: "Ctrl+I",
        event: EditorEvent::CycleIndentMode,
    },
    Command {
        id: "cycle_theme",
        label: "Cycle theme",
        keybinding: "F5",
        event: EditorEvent::CycleTheme,
    },
    Command {
        id: "select_next_occurrence",
        label: "Select next occurrence",
        keybinding: "Ctrl+D",
        event: EditorEvent::SelectNextOccurrence,
    },
    Command {
        id: "select_all_occurrences",
        label: "Select all occurrences",
        keybinding: "Ctrl+Shift+L",
        event: EditorEvent::SelectAllOccurrences,
    },
    Command {
        id: "select_all",
        label: "Select all",
        keybinding: "Ctrl+A",
        event: EditorEvent::SelectAll,
    },
    Command {
        id: "toggle_comment",
        label: "Toggle comment",
        keybinding: "Ctrl+/",
        event: EditorEvent::ToggleComment,
    },
    Command {
        id: "unfold_all",
        label: "Unfold all",
        keybinding: "",
        event: EditorEvent::UnfoldAll,
    },
    Command {
        id: "rename_symbol",
        label: "Rename symbol",
        keybinding: "F2",
        event: EditorEvent::RenameSymbol,
    },
    Command {
        id: "format_document",
        label: "Format document",
        keybinding: "",
        event: EditorEvent::FormatDocument,
    },
    Command {
        id: "duplicate_line",
        label: "Duplicate line",
        keybinding: "Ctrl+Shift+D",
        event: EditorEvent::DuplicateLine,
    },
    Command {
        id: "go_to_definition",
        label: "Go to definition",
        keybinding: "F12",
        event: EditorEvent::LspGoToDefinition,
    },
    Command {
        id: "hover",
        label: "Hover documentation",
        keybinding: "Ctrl+K",
        event: EditorEvent::LspHover,
    },
    Command {
        id: "quit",
        label: "Quit",
        keybinding: "Ctrl+Q",
        event: EditorEvent::Quit,
    },
];

/// Filter commands by a query string. v1 uses simple substring matching
/// against the label and id; the results are ordered by relevance
/// (exact prefix match first, then substring match).
pub fn filter_commands(query: &str) -> Vec<&'static Command> {
    let query = query.to_lowercase();
    let mut matches: Vec<&'static Command> = COMMANDS
        .iter()
        .filter(|cmd| {
            cmd.label.to_lowercase().contains(&query) || cmd.id.to_lowercase().contains(&query)
        })
        .collect();
    matches.sort_by_key(|cmd| {
        let label_lower = cmd.label.to_lowercase();
        if label_lower.starts_with(&query) {
            0
        } else if cmd.id.to_lowercase().starts_with(&query) {
            1
        } else {
            2
        }
    });
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_finds_by_label() {
        let results = filter_commands("save");
        assert!(results.iter().any(|c| c.id == "save"));
    }

    #[test]
    fn filter_finds_by_id() {
        let results = filter_commands("project_search");
        assert_eq!(results.first().map(|c| c.id), Some("project_search"));
    }

    #[test]
    fn markdown_preview_is_discoverable() {
        let results = filter_commands("markdown preview");
        assert_eq!(
            results.first().map(|command| command.id),
            Some("cycle_markdown_preview")
        );
    }

    #[test]
    fn empty_query_returns_all() {
        assert_eq!(filter_commands("").len(), COMMANDS.len());
    }

    #[test]
    fn prefix_match_ranks_first() {
        let results = filter_commands("pro");
        assert_eq!(results.first().map(|c| c.id), Some("project_search"));
    }
}
