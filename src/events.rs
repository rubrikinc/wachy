use crate::program::FunctionName;
use crate::program::SymbolInfo;
use std::collections::HashMap;
use std::time::Duration;

/// Events communicated to the controller
pub enum Event {
    /// The program should quit on receiving this
    FatalTraceError {
        error_message: String,
    },
    TraceData(TraceInfo),
    TraceCommandModified,
    SearchResults {
        counter: u64,
        view_name: String,
        results: Vec<(String, Option<SymbolInfo>)>,
    },
    SelectedFunction(FunctionName),
}

/// Format in which trace data is passed back
pub struct TraceInfo {
    /// Counter corresponding to when bpftrace command was last updated
    pub counter: u64,
    /// Time for which current trace has been running
    pub time: Duration,
    pub traces: TraceInfoMode,
}

pub enum TraceInfoMode {
    /// Map from line to cumulative values
    Lines(HashMap<u32, TraceCumulative>),
    /// String representation of histogram values
    Histogram(String),
    Breakdown {
        last_frame_trace: TraceCumulative,
        /// Vector of cumulative values, each entry corresponding to
        /// `TraceStack.breakdown_functions`.
        breakdown_traces: Vec<TraceCumulative>,
    },
}

pub struct TraceCumulative {
    /// Cumulative time spent
    pub duration: Duration,
    /// Cumulative count
    pub count: u64,
}
