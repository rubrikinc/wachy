use crate::program::FunctionName;
use crate::program::SymbolInfo;
use std::collections::HashMap;
use std::time::Duration;

/// Events communicated to the controller
pub enum Event {
    /// Includes error message. The program should quit on receiving this.
    FatalTraceError(String),
    TraceData(TraceInfo),
    TraceCommandModified,
    /// Counter, search view name, results
    SearchResults(u64, String, Vec<(String, Option<SymbolInfo>)>),
    SelectedFunction(FunctionName),
}

/// Format in which trace data is passed back
pub struct TraceInfo {
    /// Counter corresponding to when bpftrace command was last updated
    pub counter: u64,
    /// Time for which current trace has been running
    pub time: Duration,
    /// Map from line to cumulative values
    pub traces: HashMap<u32, TraceCumulative>,
}

pub struct TraceCumulative {
    /// Cumulative time spent
    pub duration: Duration,
    /// Cumulative count
    pub count: u64,
}
