use crate::program::FunctionName;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::Duration;

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

/// Manages the stack of functions being traced and helps generate appropriate
/// bpftrace programs.
pub struct TraceStack {
    counter: AtomicU64,
    program_path: String,
    /// Stack of functions being traced. Guaranteed to be non-empty.
    stack: Mutex<Frames>,
}

pub struct Frames {
    frames: Vec<FrameInfo>,
    /// Gets notified whenever the stack is modified (i.e. get_bpftrace_expr
    /// would change).
    tx: Sender<()>,
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
pub enum InstructionType {
    /// Dynamic symbol
    DynamicSymbol(FunctionName),
    /// Function being called, if it's a hardcoded function
    Function(FunctionName),
    /// Register being called. Note: must be a bpftrace register
    /// https://github.com/iovisor/bpftrace/blob/master/src/arch/x86_64.cpp,
    /// which notably does not have E or R prefixes.
    Register(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallInstruction {
    /// IP of call instruction, relative to start of function
    relative_ip: u32,
    /// Size of instruction
    length: u8,
    pub instruction: InstructionType,
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

    /// Source line numbers that contain a call instruction
    pub fn called_lines(&self) -> Vec<u32> {
        self.line_to_callsites.keys().map(|l| *l).collect()
    }

    pub fn get_source_file(&self) -> &str {
        &self.source_file
    }

    pub fn get_source_line(&self) -> u32 {
        self.source_line
    }
}

impl CallInstruction {
    pub fn dynamic_symbol(relative_ip: u32, length: u8, function: FunctionName) -> CallInstruction {
        CallInstruction {
            relative_ip,
            length,
            instruction: InstructionType::DynamicSymbol(function),
        }
    }

    pub fn function(relative_ip: u32, length: u8, function: FunctionName) -> CallInstruction {
        CallInstruction {
            relative_ip,
            length,
            instruction: InstructionType::Function(function),
        }
    }

    pub fn register(relative_ip: u32, length: u8, register: String) -> CallInstruction {
        CallInstruction {
            relative_ip,
            length,
            instruction: InstructionType::Register(register),
        }
    }
}

impl From<CallInstruction> for String {
    fn from(c: CallInstruction) -> Self {
        let mut out = format!("{}: ", c.relative_ip);
        match c.instruction {
            InstructionType::DynamicSymbol(addr) => {
                out += &addr.pretty_print();
            }
            InstructionType::Function(function) => out += function.0,
            InstructionType::Register(register) => out += &register,
        }
        out
    }
}

impl TraceStack {
    pub fn new(program_path: String, frame: FrameInfo, tx: Sender<()>) -> TraceStack {
        let stack = Mutex::new(Frames {
            frames: vec![frame],
            tx,
        });
        TraceStack {
            counter: AtomicU64::new(0),
            program_path,
            stack,
        }
    }

    pub fn get_current_function(&self) -> FunctionName {
        let guard = self.stack.lock().unwrap();
        guard.frames.last().unwrap().function
    }

    pub fn get_callsites(&self, line: u32) -> Vec<CallInstruction> {
        let guard = self.stack.lock().unwrap();
        let callsites = guard
            .frames
            .last()
            .unwrap()
            .line_to_callsites
            .get(&line)
            .map(|v| v.clone())
            .unwrap_or_default();
        log::debug!("{:?}", callsites);
        callsites
    }

    /// Note: does not update counter as any existing trace data is presumed to still be valid
    pub fn add_callsite(&self, line: u32, ci: CallInstruction) {
        let mut guard = self.stack.lock().unwrap();
        let top_frame = guard.frames.last_mut().unwrap();
        assert!(top_frame
            .line_to_callsites
            .get(&line)
            .unwrap()
            .contains(&ci));
        top_frame.traced_callsites.insert(line, ci);
        guard.tx.send(()).unwrap();
    }

    /// Remove traced callsite, returning true if one exists corresponding to this line.
    pub fn remove_callsite(&self, line: u32) -> bool {
        let mut guard = self.stack.lock().unwrap();
        let top_frame = guard.frames.last_mut().unwrap();
        if top_frame.traced_callsites.remove(&line).is_some() {
            self.counter.fetch_add(1, Ordering::Release);
            guard.tx.send(()).unwrap();
            true
        } else {
            false
        }
    }

    pub fn push(&self, frame: FrameInfo) {
        let mut guard = self.stack.lock().unwrap();
        guard.frames.push(frame);
        self.counter.fetch_add(1, Ordering::Release);
        guard.tx.send(()).unwrap();
    }

    /// Get appropriate bpftrace expression for current state, along with
    /// current counter value.
    /// Panics if called with empty stack
    pub fn get_bpftrace_expr(&self) -> (String, u64) {
        // TODO add tests, update examples
        // Example:
        // BEGIN { @start_time = nsecs } uprobe:/home/ubuntu/test:foo { @start4[tid] = nsecs; } uretprobe:/home/ubuntu/test:foo { @duration4 += nsecs - @start4[tid]; @count4 += 1; delete(@start4[tid]); }  interval:s:1 { printf("{\"time\": %d, \"traces\": {\"4\": [%lld, %lld]}}\n", (nsecs - @start_time) / 1000000000, @duration4, @count4); }
        // We use line number in variable naming to identify the results.
        let guard = self.stack.lock().unwrap();
        let frames = &guard.frames;
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

        parts.push(r#"interval:s:1 { printf("{\"time\": %d, ", (nsecs - @start_time) / 1000000000); printf("\"traces\": {"); "#.into());
        for (i, line) in lines.iter().enumerate() {
            let mut format_str = format!(r#"\"{}\": [%lld, %lld]"#, line);
            if i != lines.len() - 1 {
                format_str.push_str(", ");
            }
            parts.push(format!(
                r#"printf("{format_str}", @duration{line}, @count{line}); "#,
                format_str = format_str,
                line = line
            ));
        }
        parts.push(r#"printf("}}\n"); }"#.into());
        let expr = parts.concat();
        log::debug!("Current bpftrace expression: {}", expr);
        // Since we hold lock we know counter won't change
        (expr, self.counter.load(Ordering::Relaxed))
    }

    /// Parse bpftrace output
    pub fn parse(line: &str, counter: u64) -> Result<TraceInfo, serde_json::Error> {
        let info: TraceOutput = serde_json::from_str(line)?;
        Ok(TraceInfo {
            counter,
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

    pub fn is_counter_current(&self, counter: u64) -> bool {
        return counter == self.counter.load(Ordering::Acquire);
    }
}
