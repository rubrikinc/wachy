use crate::program::FunctionName;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Format in which trace data is passed back
pub struct TraceInfo {
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

/// Manages the stack of functions being traced and helps generate appropriate
/// bpftrace programs.
pub struct TraceStack {
    counter: AtomicU64,
    program_path: String,
    /// Stack of functions being traced. Guaranteed to be non-empty.
    frames: Mutex<Vec<FrameInfo>>,
}

pub struct FrameInfo {
    function: FunctionName,
    source_file: String,
    source_line: u32,
    /// Map from source line numbers to call functions on that line
    line_to_callsites: HashMap<u32, Vec<CallInstruction>>,
    /// Function calls to trace. Currently we only allow one per line.
    traced_callsites: HashMap<u32, CallInstruction>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallInstruction {
    /// IP of call instruction, relative to start of function
    relative_ip: u32,
    /// Size of instruction
    length: u8,
    /// Address of dynamic symbol
    dynamic_symbol_address: Option<u64>,
    /// Function being called, if it's a hardcoded function
    function: Option<FunctionName>,
    /// Register being called. Note: must be a bpftrace register
    /// https://github.com/iovisor/bpftrace/blob/master/src/arch/x86_64.cpp,
    /// which notably does not have E or R prefixes.
    register: Option<String>,
}

#[derive(serde::Deserialize, Debug)]
struct TraceOutput {
    time: u64,
    // Map from (stringified) line to (duration, count)
    traces: HashMap<String, (u64, u64)>,
}

impl FrameInfo {
    pub fn new(
        function: FunctionName,
        source_file: String,
        source_line: u32,
        line_to_callsites: HashMap<u32, Vec<CallInstruction>>,
    ) -> FrameInfo {
        FrameInfo {
            function,
            source_file,
            source_line,
            line_to_callsites,
            traced_callsites: HashMap::new(),
        }
    }
}

impl CallInstruction {
    pub fn dynamic_symbol(relative_ip: u32, length: u8, address: u64) -> CallInstruction {
        CallInstruction {
            relative_ip,
            length,
            dynamic_symbol_address: Some(address),
            function: None,
            register: None,
        }
    }

    pub fn function(relative_ip: u32, length: u8, function: FunctionName) -> CallInstruction {
        CallInstruction {
            relative_ip,
            length,
            dynamic_symbol_address: None,
            function: Some(function),
            register: None,
        }
    }

    pub fn register(relative_ip: u32, length: u8, register: String) -> CallInstruction {
        CallInstruction {
            relative_ip,
            length,
            dynamic_symbol_address: None,
            function: None,
            register: Some(register),
        }
    }
}

impl TraceStack {
    pub fn new(program_path: String, frame: FrameInfo) -> TraceStack {
        let frames = Mutex::new(vec![frame]);
        TraceStack {
            counter: AtomicU64::new(0),
            program_path,
            frames,
        }
    }

    pub fn get_callsites(&self, line: u32) -> Vec<CallInstruction> {
        let guard = self.frames.lock().unwrap();
        let callsites = guard
            .last()
            .unwrap()
            .line_to_callsites
            .get(&line)
            .map(|v| v.clone())
            .unwrap_or_default();
        log::debug!("{:?}", callsites);
        callsites
    }

    pub fn add_callsite(&self, line: u32, ci: CallInstruction) {
        // We rely on the lock for actual ordering
        self.counter.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.frames.lock().unwrap();
        let mut top_frame = guard.last_mut().unwrap();
        assert!(top_frame
            .line_to_callsites
            .get(&line)
            .unwrap()
            .contains(&ci));
        top_frame.traced_callsites.insert(line, ci);
    }

    /// Panics if called with empty stack
    pub fn get_bpftrace_expr(&self) -> String {
        // Example:
        // BEGIN { @start_time = nsecs } uprobe:/home/ubuntu/test:foo { @start4[tid] = nsecs; } uretprobe:/home/ubuntu/test:foo { @duration4 += nsecs - @start4[tid]; @count4 += 1; delete(@start4[tid]); }  interval:s:1 { printf("{\"time\": %d, \"traces\": {\"4\": [%lld, %lld]}}\n", (nsecs - @start_time) / 1000000000, @duration4, @count4); }
        // We use line number in variable naming to identify the results.
        // TODO add tests, update examples
        let frames = self.frames.lock().unwrap();
        let mut parts: Vec<String> = vec!["BEGIN { @start_time = nsecs } ".into()];
        for (i, frame) in frames.iter().enumerate() {
            if i != frames.len() - 1 {
                // TODO
            }
        }
        let frame = frames.last().unwrap();
        let line = frame.source_line;
        let mut lines = vec![line];
        let function = frame.function;
        parts.push(format!(
            "uprobe:{}:{} {{ @start{}[tid] = nsecs; }} ",
            self.program_path, function, line
        ));
        parts.push(format!("uretprobe:{}:{} {{ @duration{line} += nsecs - @start{line}[tid]; @count{line} += 1; delete(@start{line}[tid]); }} ", self.program_path, function, line = line));

        for (&line, callsite) in &frame.traced_callsites {
            lines.push(line);
            parts.push(format!(
                "uprobe:{}:{}+{} {{ @start{}[tid] = nsecs; }} ",
                self.program_path, function, callsite.relative_ip, line
            ));
            parts.push(format!(
                "uprobe:{}:{}+{} /@start{line}[tid] != 0/ {{ @duration{line} += nsecs - @start{line}[tid]; @count{line} += 1; delete(@start{line}[tid]); }} ",
                self.program_path, function, callsite.relative_ip + callsite.length as u32, line = line));
        }

        parts.push(r#"interval:s:1 { printf("{\"time\": %d, ", (nsecs - @start_time) / 1000000000); printf("\"traces\": {"#.into());
        // Pass 1: get all the formatting specifiers
        for (i, line) in lines.iter().enumerate() {
            parts.push(format!(r#"\"{}\": [%lld, %lld]"#, line));
            if i != lines.len() - 1 {
                parts.push(", ".into());
            }
        }
        parts.push(r#"}}\n""#.into());
        // Pass 2: print all the values
        for line in lines {
            parts.push(format!(", @duration{line}, @count{line}", line = line));
        }
        parts.push(r#"); }"#.into());
        let expr = parts.concat();
        log::debug!("Current bpftrace expression: {}", expr);
        expr
    }

    pub fn parse(line: &str) -> Result<TraceInfo, serde_json::Error> {
        let info: TraceOutput = serde_json::from_str(line)?;
        Ok(TraceInfo {
            time: Duration::from_secs(info.time),
            traces: info
                .traces
                .into_iter()
                .map(|(line, value)| {
                    // If JSON parsing succeeded we assume it is valid output, so `line` must be valid to parse
                    (
                        line.parse::<u32>().unwrap(),
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
