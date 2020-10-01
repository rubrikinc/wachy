use core::cmp::Ordering;
use cursive::view::{Nameable, Resizable};
use cursive::views::{Dialog, EditView, LinearLayout, ResizedView, SelectView};

mod source_view {
    use std::time::Duration;

    pub const CALL_ANNOTATION_LEN: usize = 2;

    #[derive(Copy, Clone, PartialEq, Eq, Hash)]
    pub enum Column {
        Latency,
        Frequency,
        LineNumber,
        Line,
    }

    #[derive(Clone, Debug)]
    pub struct Item {
        pub latency: Option<Duration>,
        /// Frequency per second
        pub frequency: Option<f32>,
        pub line_number: u32,
        pub line: String,
        pub highlighted: bool,
    }

    impl Item {
        // Number of significant figures to show when formatting
        const SIGNIFICANT_FIGURES: usize = 3;
        const LATENCY_LABELS: &'static [&'static str] = &["ns", "us", "ms", "s"];
        const FREQUENCY_LABELS: &'static [&'static str] = &["/ms", "/s", "K/s", "M/s"];

        fn format_latency(&self) -> Option<String> {
            self.latency
                .map(|l| Self::format(l.as_nanos() as f64, Self::LATENCY_LABELS))
        }

        fn format_frequency(&self) -> Option<String> {
            self.frequency
                .map(|f| Self::format(f as f64 * 1000.0, Self::FREQUENCY_LABELS))
        }

        /// Given labels representing increasing order of magniture values, format
        fn format(mut value: f64, labels: &'static [&'static str]) -> String {
            // TODO add tests
            let n_decimals = |value: f64| -> usize {
                Item::SIGNIFICANT_FIGURES.saturating_sub(value.abs().log10() as usize + 1)
            };

            for (i, label) in labels.iter().enumerate() {
                if value < 1000.0 {
                    if i == 0 {
                        return format!("{}{}", value, label);
                    } else {
                        return format!("{:.*}{}", n_decimals(value), value, label);
                    }
                } else if i == labels.len() - 1 {
                    return format!("{:.0}{}", value, label);
                }

                value /= 1000.0;
            }
            unreachable!();
        }
    }

    impl cursive_table_view::TableViewItem<Column> for Item {
        fn to_column(&self, column: Column) -> String {
            match column {
                Column::Latency => self.format_latency().unwrap_or_else(String::new),
                Column::Frequency => self.format_frequency().unwrap_or_else(String::new),
                Column::LineNumber => {
                    let call_annotation = if self.highlighted { "▶ " } else { "  " };
                    assert_eq!(call_annotation.chars().count(), CALL_ANNOTATION_LEN);
                    format!("{}{}", call_annotation, self.line_number)
                }
                Column::Line => self.line.clone(),
            }
        }

        fn cmp(&self, other: &Self, _column: Column) -> core::cmp::Ordering {
            self.line_number.cmp(&other.line_number)
        }
    }
}

pub type SourceView = cursive_table_view::TableView<source_view::Item, source_view::Column>;

/// View to display source code files with inline tracing info.
pub fn new_source_view(
    source: Vec<String>,
    selected_line: u32,
    highlighted_lines: Vec<u32>,
) -> SourceView {
    use source_view::Column;
    use source_view::Item;
    let line_num_width =
        (source.len() as f32).log10().ceil() as usize + source_view::CALL_ANNOTATION_LEN + 1;
    let mut table = cursive_table_view::TableView::<Item, Column>::new()
        .column(Column::Latency, "Duration", |c| c.width(8))
        .column(Column::Frequency, "Frequency", |c| c.width(8))
        .column(Column::LineNumber, "", |c| {
            c.width(line_num_width).align(cursive::align::HAlign::Right)
        })
        .column(Column::Line, "", |c| c);
    table.sort_by(Column::LineNumber, Ordering::Less);
    let mut items: Vec<Item> = source
        .into_iter()
        .enumerate()
        .map(|(i, line)| Item {
            latency: None,
            frequency: None,
            line_number: i as u32 + 1,
            line,
            highlighted: false,
        })
        .collect();
    for line in highlighted_lines {
        items.get_mut(line as usize - 1).unwrap().highlighted = true;
    }
    table = table.items(items).selected_row(selected_line as usize - 1);
    table
}

pub type SearchView = ResizedView<Dialog>;

const SEARCH_VIEW_WIDTH: usize = 40;

pub fn new_search_view<T>(title: &str, entries: Vec<T>) -> SearchView {
    let edit_view = EditView::new()
        // .on_submit(ok)
        .with_name("name")
        .fixed_width(SEARCH_VIEW_WIDTH);
    let select_view = SelectView::<String>::new()
        // .on_submit(on_submit)
        .with_name("select")
        .fixed_size((SEARCH_VIEW_WIDTH, 8));
    Dialog::around(
        LinearLayout::horizontal()
            .child(edit_view)
            .child(select_view),
    )
    .title(title)
    .fixed_width(SEARCH_VIEW_WIDTH)
}
