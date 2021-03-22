use crate::events::Event;
use crate::program::{SymbolInfo, SymbolsGenerator};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use itertools::Itertools;
use std::borrow::Cow;
use std::cmp;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

enum SearchCommand {
    SetEmptySearchResults(Vec<(String, Option<SymbolInfo>)>),
    SetFixedItems(Vec<SymbolInfo>),
    /// Counter, search view name, search string and (max) number of results.
    /// Must be sent after SetEmptySearchResults and SetFixedItems. The
    /// search will be performed on fixed items combined with
    /// `program.symbols_iterator()`.
    Search(u64, String, String, usize),
    Exit,
}
/// Handles async searches
pub struct Searcher {
    tx: mpsc::Sender<SearchCommand>,
    search_thread: Option<thread::JoinHandle<()>>,
    counter: Arc<AtomicU64>,
}

impl Searcher {
    pub fn new(tx: mpsc::Sender<Event>, symbols: SymbolsGenerator) -> Searcher {
        let (command_tx, command_rx) = mpsc::channel();
        let counter = Arc::new(AtomicU64::new(0));
        let counter_copy = Arc::clone(&counter);
        let search_thread =
            thread::spawn(move || Searcher::search_thread(command_rx, tx, symbols, counter_copy));
        Searcher {
            tx: command_tx,
            search_thread: Some(search_thread),
            counter,
        }
    }

    pub fn setup_search(
        &self,
        empty_search_results: Vec<(String, Option<SymbolInfo>)>,
        fixed_items: Vec<SymbolInfo>,
    ) {
        self.counter.fetch_add(1, Ordering::Release);
        self.tx
            .send(SearchCommand::SetEmptySearchResults(empty_search_results))
            .unwrap();
        self.tx
            .send(SearchCommand::SetFixedItems(fixed_items))
            .unwrap();
    }

    pub fn search(&self, view_name: &str, search: &str, n_results: usize) {
        let counter = self.counter.fetch_add(1, Ordering::Release) + 1;
        self.tx
            .send(SearchCommand::Search(
                counter,
                view_name.to_string(),
                search.to_string(),
                n_results,
            ))
            .unwrap();
    }

    pub fn is_counter_current(&self, counter: u64) -> bool {
        counter == self.counter.load(Ordering::Acquire)
    }

    fn search_thread(
        command_rx: mpsc::Receiver<SearchCommand>,
        tx: mpsc::Sender<Event>,
        symbols: SymbolsGenerator,
        counter: Arc<AtomicU64>,
    ) {
        let mut empty_search_results = None;
        let mut fixed_items = None;
        for cmd in command_rx {
            match cmd {
                SearchCommand::SetEmptySearchResults(results) => {
                    empty_search_results = Some(results)
                }
                SearchCommand::SetFixedItems(items) => fixed_items = Some(items),
                SearchCommand::Search(counter_val, view_name, search, n_results) => {
                    let is_cancelled_fn = || counter_val != counter.load(Ordering::Acquire);
                    if is_cancelled_fn() {
                        // This is not the latest search, abort
                        continue;
                    }

                    let results_opt = if search.is_empty() {
                        Some(empty_search_results.clone().unwrap())
                    } else {
                        log::debug!("Searching for {}", search);
                        let start_time = std::time::Instant::now();
                        let it = fixed_items.as_ref().unwrap().iter().chain(&symbols);
                        let results_opt =
                            rank_fn_with_cancellation(it, &search, n_results, is_cancelled_fn);
                        match results_opt {
                            Some(_) => log::debug!(
                                "Completed search for {}, returning {} results in {:#?}",
                                search,
                                results_opt.as_ref().map(|r| r.len()).unwrap_or(0),
                                start_time.elapsed()
                            ),
                            None => log::debug!("Canceled in {:#?}", start_time.elapsed()),
                        }
                        results_opt
                    };
                    results_opt.map(|r| {
                        tx.send(Event::SearchResults(counter_val, view_name, r))
                            .unwrap()
                    });
                }
                SearchCommand::Exit => return,
            }
        }
    }
}

impl Drop for Searcher {
    fn drop(&mut self) {
        self.tx.send(SearchCommand::Exit).unwrap();
        // This is the only place we modify `search_thread`, so it must be
        // non-empty here.
        self.search_thread.take().unwrap().join().unwrap();
    }
}

pub trait Label {
    fn label(&self) -> Cow<str>;
}

impl Label for &str {
    fn label(&self) -> Cow<str> {
        Cow::Borrowed(self)
    }
}

/// Rank matches using fuzzy search and return the top results
pub fn rank_fn<'a, T, I>(it: I, search: &str, n_results: usize) -> Vec<(String, Option<T>)>
where
    T: Clone + std::fmt::Display + Label + 'static,
    I: Iterator<Item = &'a T>,
{
    let is_cancelled_fn = || false;
    rank_fn_with_cancellation(it, search, n_results, is_cancelled_fn).unwrap()
}

/// Rank matches using fuzzy search and return the top results, allowing for
/// cancellation in between (since fuzzy search can take a long time). Returns
/// `None` only when cancelled.
fn rank_fn_with_cancellation<'a, T, I, F>(
    it: I,
    search: &str,
    n_results: usize,
    is_cancelled_fn: F,
) -> Option<Vec<(String, Option<T>)>>
where
    T: Clone + std::fmt::Display + Label + 'static,
    I: Iterator<Item = &'a T>,
    F: Fn() -> bool,
{
    let matcher = SkimMatcherV2::default();
    let mut candidates = Vec::new();
    for (i, val) in it.enumerate() {
        if i % 32 == 0 && is_cancelled_fn() {
            return None;
        }
        match matcher.fuzzy_match(&*val.label(), search) {
            Some(score) => candidates.push((score, val)),
            _ => (),
        }
    }

    Some(
        candidates
            .into_iter()
            .sorted_by(|(score1, val1), (score2, val2)| {
                match score1.cmp(score2).reverse() {
                    // Prefer shorter candidates - e.g. in C++ you often have
                    // types that are stored in templatized types like
                    // unique_ptr/map etc. along with some templatized
                    // functions, but the non-templatized i.e. shortest
                    // functions are typically the ones I want to trace.
                    cmp::Ordering::Equal => {
                        let len1 = val1.label().len();
                        let len2 = val2.label().len();
                        len1.cmp(&len2)
                    }
                    o => o,
                }
            })
            .take(n_results)
            .map(|(_, i)| (i.to_string(), Some(i.clone())))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore]
    /// Very crude benchmark for the ranking function
    fn bench_rank_fn() {
        let program = crate::program::Program::new("program".to_string()).unwrap();
        println!("Loaded");
        let now = std::time::Instant::now();
        let results = rank_fn(program.symbols_generator().into_iter(), "test", 10);
        println!("{:#?}", results);
        println!("{:#?}", now.elapsed());
    }
}
