use core::cmp::Ordering;

mod source_view {
    #[derive(Copy, Clone, PartialEq, Eq, Hash)]
    pub enum Column {
        Duration,
        Count,
        LineNumber,
        Line,
    }

    #[derive(Clone, Debug)]
    pub struct Item {
        pub duration: Option<std::time::Duration>,
        pub count: Option<i64>,
        pub line_number: u32,
        pub line: String,
    }

    impl cursive_table_view::TableViewItem<Column> for Item {
        fn to_column(&self, column: Column) -> String {
            match column {
                Column::Duration => match self.duration {
                    Some(duration) => format!("{:?}", duration),
                    None => String::new(),
                },
                Column::Count => self.count.map_or(String::new(), |f| f.to_string()),
                Column::LineNumber => self.line_number.to_string(),
                Column::Line => self.line.clone(),
            }
        }

        fn cmp(&self, other: &Self, _column: Column) -> core::cmp::Ordering {
            self.line_number.cmp(&other.line_number)
        }
    }
}

/// View to display source code files with inline tracing info.
pub fn new_source_view(
    source: Vec<String>,
    selected_line: u32,
) -> cursive_table_view::TableView<source_view::Item, source_view::Column> {
    use source_view::Column;
    use source_view::Item;
    let line_num_width = (source.len() as f32).log10().ceil() as usize + 1;
    let mut table = cursive_table_view::TableView::<Item, Column>::new()
        .column(Column::Duration, "Duration", |c| c.width(8))
        .column(Column::Count, "Count", |c| c.width(8))
        .column(Column::LineNumber, "", |c| {
            c.width(line_num_width).align(cursive::align::HAlign::Right)
        })
        .column(Column::Line, "", |c| c);
    table.sort_by(Column::LineNumber, Ordering::Less);
    let items: Vec<Item> = source
        .into_iter()
        .enumerate()
        .map(|(i, line)| Item {
            duration: None,
            count: None,
            line_number: i as u32 + 1,
            line,
        })
        .collect();
    table = table.items(items).selected_item(selected_line as usize - 1);
    table
}
