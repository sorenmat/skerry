//! Cached, read-only CSV table preview for the GUI.

use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::theme::GuiTheme;

pub(crate) const MAX_PREVIEW_BYTES: usize = 32 * 1024 * 1024;
const MAX_ROWS: usize = 50_000;
const MAX_COLUMNS: usize = 256;
const MAX_CELL_CHARS: usize = 4_096;
const MAX_STORED_CELLS: usize = 1_000_000;
const MAX_STORED_TEXT_BYTES: usize = 16 * 1024 * 1024;
const ROW_HEIGHT: f32 = 24.0;
const HEADER_HEIGHT: f32 = 30.0;
const ROW_NUMBER_WIDTH: f32 = 52.0;
const DEFAULT_COLUMN_WIDTH: f32 = 160.0;

#[derive(Debug, Default)]
pub(crate) struct CsvPreview {
    key: Option<(u64, u64)>,
    rows: Vec<Vec<Box<str>>>,
    column_count: usize,
    error: Option<String>,
    truncated: bool,
}

impl CsvPreview {
    pub fn needs_refresh(&self, key: (u64, u64)) -> bool {
        self.key != Some(key)
    }

    pub fn refresh(&mut self, key: (u64, u64), bytes: &[u8]) {
        self.rows.clear();
        self.column_count = 0;
        self.error = None;
        self.truncated = false;
        self.key = Some(key);

        if bytes.len() > MAX_PREVIEW_BYTES {
            self.error = Some(format!(
                "CSV table view is limited to {} MiB; switch to Source for this file.",
                MAX_PREVIEW_BYTES / (1024 * 1024)
            ));
            return;
        }
        if let Err(error) = validate_shape(bytes) {
            self.error = Some(error.to_owned());
            return;
        }

        let mut stored_cells = 0usize;
        let mut stored_text_bytes = 0usize;
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(bytes);
        'records: for record in reader.records() {
            let record = match record {
                Ok(record) => record,
                Err(error) => {
                    self.error = Some(format!("Could not parse CSV: {error}"));
                    self.rows.clear();
                    self.column_count = 0;
                    return;
                }
            };
            if self.rows.len() == MAX_ROWS {
                self.truncated = true;
                break;
            }
            let mut row = Vec::with_capacity(record.len().min(MAX_COLUMNS));
            for field in record.iter().take(MAX_COLUMNS) {
                if stored_cells == MAX_STORED_CELLS {
                    self.truncated = true;
                    break 'records;
                }
                let display = display_cell(field);
                if stored_text_bytes.saturating_add(display.len()) > MAX_STORED_TEXT_BYTES {
                    self.truncated = true;
                    break 'records;
                }
                stored_cells += 1;
                stored_text_bytes += display.len();
                row.push(display);
            }
            if record.len() > MAX_COLUMNS {
                self.truncated = true;
            }
            self.column_count = self.column_count.max(row.len());
            self.rows.push(row);
        }
    }

    pub fn reject_oversized(&mut self, key: (u64, u64), byte_len: usize) {
        self.rows.clear();
        self.column_count = 0;
        self.truncated = false;
        self.key = Some(key);
        self.error = Some(format!(
            "CSV table view is limited to {} MiB (this file is {:.1} MiB); switch to Source.",
            MAX_PREVIEW_BYTES / (1024 * 1024),
            byte_len as f64 / (1024.0 * 1024.0)
        ));
    }

    pub fn render(&self, ui: &mut egui::Ui, document_id: u64, theme: &GuiTheme) {
        if let Some(error) = &self.error {
            render_message(ui, error, theme.error);
            return;
        }
        if self.rows.is_empty() || self.column_count == 0 {
            render_message(ui, "This CSV file is empty.", theme.dim_text);
            return;
        }

        let table_width = ROW_NUMBER_WIDTH + self.column_count as f32 * DEFAULT_COLUMN_WIDTH;
        egui::ScrollArea::horizontal()
            .id_salt(("csv_table_horizontal", document_id))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_min_width(table_width);
                let mut table = TableBuilder::new(ui)
                    .id_salt(("csv_table", document_id))
                    .striped(true)
                    .resizable(true)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::exact(ROW_NUMBER_WIDTH).clip(true));
                for _ in 0..self.column_count {
                    table = table.column(
                        Column::initial(DEFAULT_COLUMN_WIDTH)
                            .at_least(64.0)
                            .resizable(true)
                            .clip(true),
                    );
                }

                table
                    .header(HEADER_HEIGHT, |mut header| {
                        header.col(|ui| {
                            render_header_cell(ui, "#", theme);
                        });
                        let headings = &self.rows[0];
                        for column in 0..self.column_count {
                            header.col(|ui| {
                                let fallback = format!("Column {}", column + 1);
                                let value = headings
                                    .get(column)
                                    .map(Box::as_ref)
                                    .filter(|value| !value.is_empty())
                                    .unwrap_or(&fallback);
                                render_header_cell(ui, value, theme).on_hover_text(value);
                            });
                        }
                    })
                    .body(|body| {
                        let data_rows = self.rows.len().saturating_sub(1);
                        body.rows(ROW_HEIGHT, data_rows, |mut table_row| {
                            let row_index = table_row.index() + 1;
                            table_row.col(|ui| {
                                ui.label(
                                    egui::RichText::new(row_index.to_string())
                                        .monospace()
                                        .color(theme.text),
                                );
                            });
                            let record = &self.rows[row_index];
                            for column in 0..self.column_count {
                                table_row.col(|ui| {
                                    let value = record.get(column).map(Box::as_ref).unwrap_or("");
                                    ui.label(value).on_hover_text(value);
                                });
                            }
                        });
                    });
            });

        if self.truncated {
            ui.label(
                egui::RichText::new(format!(
                    "Preview reached a safety limit ({MAX_ROWS} rows, {MAX_COLUMNS} columns, {MAX_STORED_CELLS} cells, or 16 MiB of displayed text)."
                ))
                .small()
                .color(theme.warning),
            );
        }
    }
}

fn render_header_cell(ui: &mut egui::Ui, value: &str, theme: &GuiTheme) -> egui::Response {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, theme.button_bg);
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        egui::Stroke::new(1.0_f32, theme.border),
    );
    ui.label(egui::RichText::new(value).strong().color(theme.button_text))
}

fn display_cell(text: &str) -> Box<str> {
    let mut display = String::with_capacity(text.len().min(MAX_CELL_CHARS));
    let mut chars = text.chars();
    for ch in chars.by_ref().take(MAX_CELL_CHARS) {
        match ch {
            '\r' => {}
            '\n' => display.push('↵'),
            _ => display.push(ch),
        }
    }
    if chars.next().is_some() {
        display.push('…');
    }
    display.into_boxed_str()
}

/// Validate record width before `csv::StringRecord` allocates one offset per
/// field. This quote-aware pass keeps a comma-only record from creating
/// millions of transient offsets on the UI thread.
fn validate_shape(bytes: &[u8]) -> Result<(), &'static str> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut index = 0usize;
    let mut columns = 1usize;
    let mut in_quotes = false;
    let mut field_start = true;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_quotes {
            if byte == b'"' {
                if bytes.get(index + 1) == Some(&b'"') {
                    index += 2;
                    continue;
                }
                in_quotes = false;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' if field_start => {
                in_quotes = true;
                field_start = false;
            }
            b',' => {
                columns += 1;
                if columns > MAX_COLUMNS {
                    return Err("CSV table view supports at most 256 columns per record.");
                }
                field_start = true;
            }
            b'\n' | b'\r' => {
                columns = 1;
                field_start = true;
            }
            _ => field_start = false,
        }
        index += 1;
    }
    if in_quotes {
        return Err("Could not parse CSV: unterminated quoted field.");
    }
    Ok(())
}

fn render_message(ui: &mut egui::Ui, message: &str, color: egui::Color32) {
    ui.vertical_centered(|ui| {
        ui.add_space(48.0);
        ui.label(egui::RichText::new(message).color(color));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_headers_quoted_commas_and_multiline_cells() {
        let mut preview = CsvPreview::default();
        preview.refresh(
            (1, 0),
            b"name,notes,count\nAlice,\"hello, world\",2\nBob,\"line one\nline two\",3\n",
        );

        assert_eq!(preview.column_count, 3);
        assert_eq!(preview.rows.len(), 3);
        assert_eq!(preview.rows[1][1].as_ref(), "hello, world");
        assert_eq!(preview.rows[2][1].as_ref(), "line one↵line two");
        assert!(preview.error.is_none());
    }

    #[test]
    fn reports_malformed_csv_without_partial_rows() {
        let mut preview = CsvPreview::default();
        preview.refresh((1, 0), b"name,value\nAlice,\xFF\n");

        assert!(preview.error.is_some());
        assert!(preview.rows.is_empty());
    }

    #[test]
    fn refresh_is_keyed_by_document_and_revision() {
        let mut preview = CsvPreview::default();
        preview.refresh((7, 3), b"a,b\n1,2\n");

        assert!(!preview.needs_refresh((7, 3)));
        assert!(preview.needs_refresh((7, 4)));
        assert!(preview.needs_refresh((8, 3)));
    }

    #[test]
    fn oversized_files_are_rejected_without_materializing_content() {
        let mut preview = CsvPreview::default();

        preview.reject_oversized((4, 2), MAX_PREVIEW_BYTES + 1);

        assert!(!preview.needs_refresh((4, 2)));
        assert!(preview
            .error
            .as_deref()
            .unwrap()
            .contains("limited to 32 MiB"));
        assert!(preview.rows.is_empty());
    }

    #[test]
    fn sparse_wide_data_stops_at_the_total_cell_budget() {
        let columns = 256;
        let rows = MAX_STORED_CELLS / columns + 10;
        let record = ",".repeat(columns - 1) + "\n";
        let input = record.repeat(rows);
        let mut preview = CsvPreview::default();

        preview.refresh((1, 0), input.as_bytes());

        let stored_cells: usize = preview.rows.iter().map(Vec::len).sum();
        assert!(stored_cells <= MAX_STORED_CELLS);
        assert!(preview.truncated);
    }

    #[test]
    fn extreme_single_record_is_rejected_before_csv_record_allocation() {
        let input = ",".repeat(1_000_000);
        let mut preview = CsvPreview::default();

        preview.refresh((1, 0), input.as_bytes());

        assert_eq!(
            preview.error.as_deref(),
            Some("CSV table view supports at most 256 columns per record.")
        );
        assert!(preview.rows.is_empty());
    }

    #[test]
    fn quoted_commas_do_not_count_as_columns_during_preflight() {
        let mut preview = CsvPreview::default();

        preview.refresh((1, 0), b"name,notes\nAlice,\"a,b,c,d\"\n");

        assert!(preview.error.is_none());
        assert_eq!(preview.column_count, 2);
    }

    #[test]
    fn bom_prefixed_quoted_first_field_matches_parser_semantics() {
        let mut preview = CsvPreview::default();

        preview.refresh((1, 0), b"\xEF\xBB\xBF\"name, label\",count\nAlice,2\n");

        assert!(preview.error.is_none());
        assert_eq!(preview.column_count, 2);
        assert_eq!(preview.rows[0][0].as_ref(), "name, label");
    }

    #[test]
    fn cr_only_records_reset_preflight_column_count() {
        let row = ",".repeat(MAX_COLUMNS - 1);
        let input = format!("{row}\r{row}\r");
        let mut preview = CsvPreview::default();

        preview.refresh((1, 0), input.as_bytes());

        assert!(preview.error.is_none());
        assert_eq!(preview.rows.len(), 2);
        assert_eq!(preview.column_count, MAX_COLUMNS);
    }

    #[test]
    fn table_renders_in_a_narrow_viewport() {
        let context = egui::Context::default();
        let mut preview = CsvPreview::default();
        preview.refresh((1, 0), b"name,city,total\nAlice,Copenhagen,12\n");

        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(320.0, 240.0),
                )),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    preview.render(ui, 1, &crate::theme::DARK);
                });
            },
        );

        assert!(!output.shapes.is_empty());
    }
}
