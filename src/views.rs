use core::cmp::Ordering;
use cursive::view::{Nameable, Resizable};
use cursive::views::{Dialog, EditView, LinearLayout, ResizedView, ScrollView, SelectView};
use cursive::Cursive;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use itertools::Itertools;
use std::borrow::{Borrow, Cow};
use std::rc::Rc;

#[derive(Clone, Copy, Debug)]
pub enum TraceState<T> {
    Untraced,
    Pending,
    Traced(T),
}

mod source_view {
    use super::TraceState;
    use std::time::Duration;

    pub const LINE_NUMBER_LEN: usize = 4;
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
        pub latency: TraceState<Duration>,
        /// Frequency per second
        pub frequency: TraceState<f32>,
        pub line_number: u32,
        pub line: String,
        pub marked: bool,
    }

    impl Item {
        // Number of significant figures to show when formatting
        const SIGNIFICANT_FIGURES: usize = 3;
        const LATENCY_LABELS: &'static [&'static str] = &["ns", "us", "ms", "s"];
        const FREQUENCY_LABELS: &'static [&'static str] = &["/Ks", "/s", "K/s", "M/s"];
        const PENDING_STR: &'static str = "  ---";

        fn format_latency(&self) -> String {
            match self.latency {
                TraceState::Traced(l) => Self::format(l.as_nanos() as f64, Self::LATENCY_LABELS),
                TraceState::Pending => Self::PENDING_STR.into(),
                TraceState::Untraced => String::new(),
            }
        }

        fn format_frequency(&self) -> String {
            match self.frequency {
                TraceState::Traced(f) => Self::format(f as f64 * 1000.0, Self::FREQUENCY_LABELS),
                TraceState::Pending => Self::PENDING_STR.into(),
                TraceState::Untraced => String::new(),
            }
        }

        /// Given labels representing increasing order of magnitude values,
        /// format to display SIGNIFICANT_FIGURES.
        fn format(mut value: f64, labels: &'static [&'static str]) -> String {
            // TODO add tests
            let n_decimals = |value: f64| -> usize {
                Item::SIGNIFICANT_FIGURES.saturating_sub(value.abs().log10() as usize + 1)
            };

            for (i, label) in labels.iter().enumerate() {
                if value < 1000.0 {
                    if value == 0.0 {
                        return format!("0{}", label);
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
                Column::Latency => self.format_latency(),
                Column::Frequency => self.format_frequency(),
                Column::LineNumber => {
                    let call_annotation = if self.marked { " ▶" } else { "  " };
                    assert_eq!(call_annotation.chars().count(), CALL_ANNOTATION_LEN);
                    format!("{}{}", self.line_number, call_annotation)
                }
                Column::Line => self.line.clone(),
            }
        }

        fn cmp(&self, other: &Self, _column: Column) -> core::cmp::Ordering {
            self.line_number.cmp(&other.line_number)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_formatting() {
            assert_eq!(Item::format(0.02934924, Item::FREQUENCY_LABELS), "0.03/ms");
        }
    }
}

pub type SourceView = cursive_table_view::TableView<source_view::Item, source_view::Column>;

/// View to display source code files with inline tracing info.
pub fn new_source_view() -> SourceView {
    use source_view::Column;
    let line_num_width = source_view::LINE_NUMBER_LEN + source_view::CALL_ANNOTATION_LEN + 1;
    let mut table = cursive_table_view::TableView::<source_view::Item, Column>::new()
        .column(Column::Latency, "Duration", |c| c.width(8))
        .column(Column::Frequency, "Frequency", |c| c.width(8))
        .column(Column::LineNumber, "", |c| {
            c.width(line_num_width).align(cursive::align::HAlign::Right)
        })
        .column(Column::Line, "", |c| c);
    table.sort_by(Column::LineNumber, Ordering::Less);
    table
}

pub fn set_source_view(
    sview: &mut SourceView,
    source_code: Vec<String>,
    selected_line: u32,
    marked_lines: Vec<u32>,
) {
    use source_view::Item;
    let mut items: Vec<Item> = source_code
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let pending = i as u32 == selected_line - 1;
            Item {
                latency: if pending {
                    TraceState::Pending
                } else {
                    TraceState::Untraced
                },
                frequency: if pending {
                    TraceState::Pending
                } else {
                    TraceState::Untraced
                },
                line_number: i as u32 + 1,
                line,
                marked: false,
            }
        })
        .collect();
    for line in marked_lines {
        items.get_mut(line as usize - 1).unwrap().marked = true;
    }
    // Set this twice - once before to prevent out of bounds, second time to
    // ensure the table scrolls to the right place.
    sview.set_selected_row(selected_line as usize - 1);
    sview.set_items(items);
    // TODO is second time necessary?
    sview.set_selected_row(selected_line as usize - 1);
}

pub type SearchView = ResizedView<Dialog>;

const SEARCH_VIEW_WIDTH: usize = 70;
const SEARCH_VIEW_HEIGHT: usize = 8;

pub trait Label {
    fn label(&self) -> Cow<str>;
}

impl Label for &str {
    fn label(&self) -> Cow<str> {
        Cow::Borrowed(self)
    }
}

pub fn rank_fn<'a, T, I>(it: I, search: &str, n_results: usize) -> Vec<(String, Option<T>)>
where
    T: Clone + std::fmt::Display + Label + 'static,
    I: Iterator<Item = &'a T>,
{
    let matcher = SkimMatcherV2::default();
    it.filter_map(|i| match matcher.fuzzy_match(i.label().borrow(), search) {
        Some(score) => Some((score, i)),
        None => None,
    })
    .sorted_by(|(score1, _), (score2, _)| score1.cmp(score2).reverse())
    .take(n_results)
    .map(|(_, i)| (i.to_string(), Some(i.clone())))
    .collect()
}

/// `title` must be unique (it is used in the name of the view)
pub fn new_search_view<T, F, G>(
    title: &str,
    initial_results: Vec<(String, Option<T>)>,
    edit_search_fn: F,
    submit_fn: G,
) -> SearchView
where
    // Parameters are search view name, search string, and (max) number of
    // results.
    F: Fn(&mut Cursive, &str, &str, usize) + 'static,
    T: 'static,
    G: Fn(&mut Cursive, &T) + 'static,
{
    let submit_cb = Rc::new(submit_fn);
    let submit_cb_copy = Rc::clone(&submit_cb);
    let name = format!("select_{}", title);
    let name_copy = name.clone();

    // SelectView value of None will be a no-op to hit enter on.
    let mut select_view = SelectView::<Option<T>>::new();
    for (label, value) in initial_results {
        select_view.add_item(label, value);
    }

    let select_view = ScrollView::new(
        select_view
            .on_submit(move |siv: &mut Cursive, sel: &Option<T>| {
                if let Some(item) = sel {
                    siv.pop_layer();
                    submit_cb(siv, item);
                }
            })
            .with_name(&name)
            .min_width(SEARCH_VIEW_WIDTH - 2), // ScrollView adds 2 character border
    )
    .scroll_x(true)
    .fixed_size((SEARCH_VIEW_WIDTH, 8));

    let update_edit_view = move |siv: &mut Cursive, search: &str, _| {
        // TODO we should add more results and allow scrolling?
        edit_search_fn(siv, &name, search, SEARCH_VIEW_HEIGHT);
    };
    let edit_view = EditView::new()
        .filler(" ")
        .on_edit_mut(update_edit_view)
        .on_submit(move |siv: &mut Cursive, _| {
            let select_view = siv.find_name::<SelectView<Option<T>>>(&name_copy).unwrap();
            if let Some(sel) = select_view.selection() {
                if let Some(item) = sel.borrow() {
                    siv.pop_layer();
                    submit_cb_copy(siv, item);
                }
            }
        })
        .with_name(format!("search_{}", title))
        .fixed_width(SEARCH_VIEW_WIDTH);

    Dialog::around(LinearLayout::vertical().child(edit_view).child(select_view))
        .title(title)
        .fixed_width(SEARCH_VIEW_WIDTH)
}

pub fn update_search_view<T>(
    siv: &mut Cursive,
    search_view_name: &str,
    results: Vec<(String, Option<T>)>,
) where
    T: 'static,
{
    let mut select_view = siv
        .find_name::<SelectView<Option<T>>>(&search_view_name)
        .unwrap();
    select_view.clear();
    for (label, value) in results {
        select_view.add_item(label, value);
    }
}

/// Convenience wrapper for new_search_view with results searched using rank_fn
pub fn new_simple_search_view<T, G>(title: &str, items: Vec<T>, submit_fn: G) -> SearchView
where
    T: Clone + std::fmt::Display + Label + 'static,
    G: Fn(&mut Cursive, &T) + 'static,
{
    let initial_results = rank_fn(items.iter(), "", usize::MAX);
    new_search_view(
        title,
        initial_results,
        move |siv, view_name, search, n_results| {
            let results = rank_fn(items.iter(), search, n_results);
            update_search_view(siv, view_name, results);
        },
        submit_fn,
    )
}

/// Simple dialog with single confirmation button that closes it
pub fn new_dialog(text: &str) -> Dialog {
    Dialog::text(text).button("OK", |siv| {
        siv.pop_layer();
    })
}

pub fn new_quit_dialog(text: &str) -> Dialog {
    Dialog::text(text)
        .button("Quit", Cursive::quit)
        .button("Cancel", |siv| {
            siv.pop_layer();
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore]
    /// Just set up a simple example search view for quicker iteration/manual testing
    fn test_search_view() {
        let mut siv = cursive::default();
        let items = vec![
            "Bananas",
            "Apples",
            "Grapes",
            "Strawberries",
            "Oranges",
            "Watermelons",
            "Lemons",
            "Avocados",
        ];

        let submit_fn = |siv: &mut Cursive, selection: &&str| {
            siv.add_layer(
                Dialog::text(format!("You selected: {}", selection)).button("Quit", Cursive::quit),
            );
        };
        let search_view = new_simple_search_view("test", items, submit_fn);
        siv.add_layer(search_view);
        siv.run();
    }

    #[test]
    #[ignore]
    /// Very crude benchmark for the ranking function
    fn bench_rank_fn() {
        let program = crate::program::Program::new("program".to_string()).unwrap();
        println!("Loaded");
        let now = std::time::Instant::now();
        let results = rank_fn(program.symbols_iterator(), "test", 10);
        println!("{:#?}", results);
        println!("{:#?}", now.elapsed());
    }
}
