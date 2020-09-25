use crate::program::FunctionName;
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
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
    pub frames: Mutex<Vec<FrameInfo>>,
}

pub struct FrameInfo {
    function: FunctionName,
    source_file: String,
    source_line: u32,
    /// Map from source line numbers to call functions on that line
    pub line_to_callsites: HashMap<u32, Vec<CallInstruction>>,
}

#[derive(Debug)]
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
    // Map from (stringified) tag to (duration, count)
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
    pub fn new(program_path: String, tag: u32, frame: FrameInfo) -> TraceStack {
        let frames = Mutex::new(vec![frame]);
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
            if i != frames.len() - 1 {
                // TODO
            }
        }
        let frame = frames.last().unwrap();
        let tag = frame.source_line;
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
