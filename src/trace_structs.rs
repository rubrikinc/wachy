use crate::program::FunctionName;
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Mutex;
use std::time::Duration;

/// Format in which trace data is passed back
pub struct TraceInfo {
    /// Time for which current trace has been running
    pub time: Duration,
    /// Map from tag to cumulative values
    pub traces: HashMap<u32, TraceCumulative>,
}

pub struct TraceCumulative {
    /// Cumulative time spent
    pub duration: Duration,
    /// Cumulative count
    pub count: u64,
}

/// Manages the stack of functions being traced and helps generate appropriate
/// bpftrace programs.
pub struct TraceStack {
    counter: AtomicU64,
    program_path: String,
    // Stack of (tag, function being traced)
    frames: Mutex<Vec<(u32, FrameInfo)>>,
}

pub struct FrameInfo {
    function: FunctionName,
    // Map from source line numbers to call functions on that line
    tag_to_callsite: HashMap<u32, Vec<CallSite>>,
}

pub struct CallSite {
    // Relative to start of function
    relative_address: u32,
    is_call_to_dynamic_symbol: bool,
    // Function being called, if it's a hardcoded function
    function: Option<FunctionName>,
    // Register being called. Note: must be a bpftrace register
    // https://github.com/iovisor/bpftrace/blob/master/src/arch/x86_64.cpp,
    // which notably does not have E or R prefixes.
    register: Option<String>,
}

#[derive(serde::Deserialize, Debug)]
struct TraceOutput {
    time: u64,
    // Map from (stringified) tag to (duration, count)
    traces: HashMap<String, (u64, u64)>,
}

impl FrameInfo {
    pub fn new(function: FunctionName, tag_to_callsite: HashMap<u32, Vec<CallSite>>) -> FrameInfo {
        FrameInfo {
            function,
            tag_to_callsite,
        }
    }
}

impl CallSite {
    pub fn dynamic_symbol(relative_address: u32) -> CallSite {
        CallSite {
            relative_address,
            is_call_to_dynamic_symbol: true,
            function: None,
            register: None,
        }
    }

    pub fn function(relative_address: u32, function: FunctionName) -> CallSite {
        CallSite {
            relative_address,
            is_call_to_dynamic_symbol: false,
            function: Some(function),
            register: None,
        }
    }

    pub fn register(relative_address: u32, register: String) -> CallSite {
        CallSite {
            relative_address,
            is_call_to_dynamic_symbol: false,
            function: None,
            register: Some(register),
        }
    }
}

impl TraceStack {
    pub fn new(program_path: String, tag: u32, frame: FrameInfo) -> TraceStack {
        let frames = Mutex::new(vec![(tag, frame)]);
        TraceStack {
            counter: AtomicU64::new(0),
            program_path,
            frames,
        }
    }

    /// Panics if called with empty stack
    pub fn get_bpftrace_expr(&self) -> String {
        // Example:
        // BEGIN { @start_time = nsecs } uprobe:/home/ubuntu/test:foo { @start4[tid] = nsecs; } uretprobe:/home/ubuntu/test:foo { @duration4 += nsecs - @start4[tid]; @count4 += 1; delete(@start4[tid]); }  interval:s:1 { printf("{\"time\": %d, \"traces\": {\"4\": [%lld, %lld]}}\n", (nsecs - @start_time) / 1000000000, @duration4, @count4); }
        // We use tag number in variable naming to identify the results.
        // TODO add tests
        let frames = self.frames.lock().unwrap();
        let mut parts: Vec<String> = vec!["BEGIN { @start_time = nsecs } ".into()];
        for (i, frame) in frames.iter().enumerate() {
            if i != frames.len() - 1 {}
        }
        let (tag, frame) = frames.last().unwrap();
        let function = frame.function;
        parts.push(format!(
            "uprobe:{}:{} {{ @start{}[tid] = nsecs; }}",
            self.program_path, function, tag
        ));
        parts.push(format!("uretprobe:{}:{} {{ @duration{tag} += nsecs - @start{tag}[tid]; @count{tag} += 1; delete(@start{tag}[tid]); }}", self.program_path, function, tag = tag));
        parts.push(format!(r#"interval:s:1 {{ printf("{{\"time\": %d, \"traces\": {{\"{tag}\": [%lld, %lld]}} }}\n", (nsecs - @start_time) / 1000000000, @duration{tag}, @count{tag}); }}"#, tag = tag));
        let expr = parts.concat();
        log::debug!("Current bpftrace expression: {}", expr);
        String::from(expr)
    }

    pub fn parse(line: &str) -> Result<TraceInfo, serde_json::Error> {
        let info: TraceOutput = serde_json::from_str(line)?;
        Ok(TraceInfo {
            time: Duration::from_secs(info.time),
            traces: info
                .traces
                .into_iter()
                .map(|(tag, value)| {
                    // If JSON parsing succeeded we assume it is valid output, so `tag` must be valid to parse
                    (
                        tag.parse::<u32>().unwrap(),
                        TraceCumulative {
                            duration: Duration::from_nanos(value.0),
                            count: value.1,
                        },
                    )
                })
                .collect(),
        })
    }
}
