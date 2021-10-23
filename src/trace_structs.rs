use crate::bpftrace_compiler::BlockType::{Uprobe, UprobeOffset, Uretprobe};
use crate::bpftrace_compiler::Expression::Printf;
use crate::bpftrace_compiler::{self, Block, BlockType, Expression};
use crate::error::Error;
use crate::events::{Event, TraceCumulative, TraceInfo, TraceInfoMode};
use crate::program::FunctionName;
use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// Manages the stack of functions being traced and helps generate appropriate
/// bpftrace programs.
pub struct TraceStack {
    counter: AtomicU64,
    program_path: String,
    /// Stack of functions being traced
    stack: Mutex<Frames>,
}

pub struct Frames {
    mode: TraceMode,
    /// Guaranteed to be non-empty
    frames: Vec<FrameInfo>,
    /// Gets notified whenever the stack is modified (i.e. trace command
    /// get_bpftrace_expr would change).
    tx: Sender<Event>,
}

#[derive(Copy, Clone)]
pub enum TraceMode {
    /// Trace latency per traced line in current view
    Line,
    /// Trace histogram of latency for the current function
    Histogram,
}

#[derive(Debug, Clone)]
pub struct FrameInfo {
    function: FunctionName,
    source_file: String,
    source_line: u32,
    /// Map from source line numbers to call functions on that line
    line_to_callsites: HashMap<u32, Vec<CallInstruction>>,
    /// List of inlined call functions that are do not have source code in this
    /// file.
    unattached_callsites: Vec<CallInstruction>,
    /// Function calls that are actively traced. Currently we only allow one per
    /// line.
    traced_callsites: HashMap<u32, CallInstruction>,
    /// bpftrace filter to apply to the function
    filter: Filter,
}

#[derive(Debug, Clone)]
pub enum Filter {
    None,
    /// Filter evaluated on function entry (uprobe)
    Filter(String),
    /// Filter evaluated on function exit (uretprobe). E.g. something like
    /// `$duration` has to be evaluated on return. Syntax for user to specify it
    /// is `ret:<filter>`.
    RetFilter(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum InstructionType {
    /// Dynamically linked function
    DynamicSymbol(FunctionName),
    /// Function being called, if it's a hardcoded function
    Function(FunctionName),
    /// Register being called. Note: should be a bpftrace register
    /// https://github.com/iovisor/bpftrace/blob/master/src/arch/x86_64.cpp,
    /// which notably does not have E or R prefixes.
    /// Second field represents displacement within register.
    Register(String, Option<i64>),
    /// Manually specified start/end offset for tracing
    Manual,
    /// Unknown function call - doesn't correspond to any symbols
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallInstruction {
    /// IP of call instruction, relative to start of function
    relative_ip: u32,
    /// Size of instruction
    length: u32,
    pub instruction: InstructionType,
}

#[derive(serde::Deserialize, Debug)]
struct TraceOutput {
    time: u64,
    // Map from (stringified) line to (duration, count)
    lines: Option<HashMap<String, (u64, u64)>>,
    histogram: Option<String>,
}

impl FrameInfo {
    pub fn new(
        function: FunctionName,
        source_file: String,
        source_line: u32,
        line_to_callsites: HashMap<u32, Vec<CallInstruction>>,
        unattached_callsites: Vec<CallInstruction>,
    ) -> FrameInfo {
        FrameInfo {
            function,
            source_file,
            source_line,
            line_to_callsites,
            unattached_callsites,
            traced_callsites: HashMap::new(),
            filter: Filter::None,
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

    /// Get largest line number for a callsite in this frame
    pub fn max_line(&self) -> u32 {
        self.line_to_callsites
            .keys()
            .max()
            .map_or(self.source_line, |l| *l)
    }
}

impl CallInstruction {
    pub fn dynamic_symbol(relative_ip: u32, length: u8, function: FunctionName) -> CallInstruction {
        CallInstruction {
            relative_ip,
            length: length as u32,
            instruction: InstructionType::DynamicSymbol(function),
        }
    }

    pub fn function(relative_ip: u32, length: u8, function: FunctionName) -> CallInstruction {
        CallInstruction {
            relative_ip,
            length: length as u32,
            instruction: InstructionType::Function(function),
        }
    }

    pub fn register(
        relative_ip: u32,
        length: u8,
        register: String,
        displacement: Option<i64>,
    ) -> CallInstruction {
        CallInstruction {
            relative_ip,
            length: length as u32,
            instruction: InstructionType::Register(register, displacement),
        }
    }

    pub fn manual(relative_ip: u32, length: u32) -> CallInstruction {
        CallInstruction {
            relative_ip,
            length,
            instruction: InstructionType::Manual,
        }
    }

    pub fn unknown(relative_ip: u32, length: u8) -> CallInstruction {
        CallInstruction {
            relative_ip,
            length: length as u32,
            instruction: InstructionType::Unknown,
        }
    }
}

impl fmt::Display for CallInstruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("{}: ", self.relative_ip))?;
        let i = &self.instruction;
        match i {
            InstructionType::DynamicSymbol(_) => f.write_fmt(format_args!("(D) {}", i)),
            InstructionType::Function(_) => f.write_fmt(format_args!("{}", i)),
            InstructionType::Register(_, _) => f.write_fmt(format_args!("(I) register {}", i)),
            InstructionType::Manual => f.write_fmt(format_args!(
                "Manual {}-{}",
                self.relative_ip,
                self.relative_ip + self.length
            )),
            InstructionType::Unknown => f.write_fmt(format_args!("{}", i)),
        }
    }
}

impl fmt::Display for InstructionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstructionType::DynamicSymbol(function) => function.fmt(f),
            InstructionType::Function(function) => function.fmt(f),
            InstructionType::Register(register, displacement) => match displacement {
                Some(d) => f.write_fmt(format_args!("[{}+0x{:x}]", register, d)),
                None => f.write_str(register),
            },
            InstructionType::Manual => f.write_str("(Manual)"),
            InstructionType::Unknown => f.write_str("(UNKNOWN)"),
        }
    }
}

impl TraceStack {
    pub fn new(program_path: String, frame: FrameInfo, tx: Sender<Event>) -> TraceStack {
        let stack = Mutex::new(Frames {
            mode: TraceMode::Line,
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

    pub fn get_unattached_callsites(&self) -> Vec<CallInstruction> {
        let guard = self.stack.lock().unwrap();
        let callsites = guard.frames.last().unwrap().unattached_callsites.clone();
        log::debug!("{:?}", callsites);
        callsites
    }

    /// Note: does not update counter as any existing trace data is presumed to still be valid
    pub fn add_callsite(&self, line: u32, ci: CallInstruction) {
        let mut guard = self.stack.lock().unwrap();
        let top_frame = guard.frames.last_mut().unwrap();
        assert!(
            ci.instruction == InstructionType::Manual
                || top_frame
                    .line_to_callsites
                    .get(&line)
                    .map_or(false, |cis| cis.contains(&ci))
                || top_frame.unattached_callsites.contains(&ci)
        );
        log::info!("Tracing callsite {}", ci);
        top_frame.traced_callsites.insert(line, ci);
        guard.tx.send(Event::TraceCommandModified).unwrap();
    }

    /// Remove traced callsite, returning true if one exists corresponding to this line.
    pub fn remove_callsite(&self, line: u32) -> bool {
        let mut guard = self.stack.lock().unwrap();
        let top_frame = guard.frames.last_mut().unwrap();
        if top_frame.traced_callsites.remove(&line).is_some() {
            self.counter.fetch_add(1, Ordering::Release);
            guard.tx.send(Event::TraceCommandModified).unwrap();
            true
        } else {
            false
        }
    }

    pub fn push(&self, frame: FrameInfo) {
        let mut guard = self.stack.lock().unwrap();
        // TODO prevent recursive (or do we need to?)
        guard.frames.push(frame);
        self.counter.fetch_add(1, Ordering::Release);
        guard.tx.send(Event::TraceCommandModified).unwrap();
    }

    /// Pops the current frame, if it is not the last one. Returns the new top
    /// of the frame (note this is different from typical stack behavior).
    pub fn pop(&self) -> Option<FrameInfo> {
        let mut guard = self.stack.lock().unwrap();
        if guard.frames.len() == 1 {
            // We do not allow popping the last frame
            return None;
        }
        guard.frames.pop();
        let frame = (*guard.frames.last().unwrap()).clone();
        self.counter.fetch_add(1, Ordering::Release);
        guard.tx.send(Event::TraceCommandModified).unwrap();
        Some(frame)
    }

    pub fn set_mode(&self, mode: TraceMode) {
        let mut guard = self.stack.lock().unwrap();
        guard.mode = mode;
        self.counter.fetch_add(1, Ordering::Release);
        guard.tx.send(Event::TraceCommandModified).unwrap();
    }

    pub fn get_current_filter(&self) -> Option<String> {
        let mut guard = self.stack.lock().unwrap();
        match &guard.frames.last_mut().unwrap().filter {
            Filter::None => None,
            Filter::Filter(f) => Some(f.clone()),
            Filter::RetFilter(f) => Some(format!("ret:{}", f)),
        }
    }

    /// Set the filter for the current function. Empty string removes the
    /// filter. Checks that it is valid bpftrace syntax, returning a descriptive
    /// error message if not.
    pub fn set_current_filter(&self, filter: String) -> Result<(), Error> {
        let mut guard = self.stack.lock().unwrap();
        let frame = guard.frames.last_mut().unwrap();
        if filter.is_empty() {
            frame.filter = Filter::None;
            return Ok(());
        }

        let prev_filter = frame.filter.clone();
        frame.filter = if filter.starts_with("ret:") {
            Filter::RetFilter(filter)
        } else {
            Filter::Filter(filter)
        };
        // Run bpftrace in dry run mode to ensure filter compiles
        let mut program = std::process::Command::new("bpftrace")
            .args(&["-d", "-e", &self.get_bpftrace_expr_locked(&guard).0])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("bpftrace failed to start");
        let status = program.wait().unwrap();
        if !status.success() {
            // Restore old filter on error. Can't reference `frame` directly
            // here due to lifetimes.
            guard.frames.last_mut().unwrap().filter = prev_filter;
            let mut stderr = String::new();
            match program.stderr.unwrap().read_to_string(&mut stderr) {
                Err(err) => return Err(format!("Failed to read bpftrace stderr: {:?}", err).into()),
                _ => (),
            }
            Err(stderr.into())
        } else {
            self.counter.fetch_add(1, Ordering::Release);
            guard.tx.send(Event::TraceCommandModified).unwrap();
            Ok(())
        }
    }

    /// Get appropriate bpftrace expression for current state, along with
    /// current counter value.
    /// Panics if called with empty stack
    pub fn get_bpftrace_expr(&self) -> (String, u64) {
        let guard = self.stack.lock().unwrap();
        self.get_bpftrace_expr_locked(&guard)
    }

    fn get_bpftrace_expr_locked(&self, guard: &MutexGuard<Frames>) -> (String, u64) {
        // We use line number in variable naming to identify the results
        let frames = &guard.frames;
        let mut program = bpftrace_compiler::Program::new();
        program.add(Block::new(
            BlockType::Begin,
            None,
            vec!["@start_time = nsecs", "@depth[-1] = 0"],
        ));

        let depth_condition = |depth| Some(format!("@depth[tid] == {}", depth));
        for (i, frame) in frames.iter().enumerate() {
            if i != frames.len() - 1 {
                program.add(Block::new(
                    Uprobe(frame.function),
                    depth_condition(i),
                    vec![format!("@depth[tid] = {}", i + 1)],
                ));
                program.add(Block::new(
                    Uretprobe(frame.function),
                    depth_condition(i + 1),
                    vec![format!("@depth[tid] = {}", i)],
                ));
            }
        }

        let frame_depth = frames.len() - 1;
        let frame = frames.last().unwrap();
        let line = frame.source_line;
        let mut lines = vec![line];
        let function = frame.function;

        let filter = ""; // TODO
        program.add(Block::new(
            Uprobe(function),
            depth_condition(frame_depth),
            vec![
                format!("@start{}[tid] = nsecs", line),
                format!("@depth[tid] = {}", frame_depth + 1),
            ],
        ));
        match guard.mode {
            TraceMode::Line => {
                program.add(Block::new(
                    Uretprobe(function),
                    depth_condition(frame_depth + 1),
                    vec![
                        format!("@duration{line} += nsecs - @start{line}[tid]", line = line),
                        format!("@count{} += 1", line),
                        format!("delete(@start{}[tid])", line),
                        format!("@depth[tid] = {}", frame_depth),
                    ],
                ));

                for (&line, callsite) in &frame.traced_callsites {
                    lines.push(line);
                    program.add(Block::new(
                        UprobeOffset(function, callsite.relative_ip),
                        depth_condition(frame_depth + 1),
                        vec![format!("@start{}[tid] = nsecs", line)],
                    ));
                    // Ensure the tracepoint at the end of the call is only
                    // triggered if we traced the start.
                    let call_done_filter = depth_condition(frame_depth + 1)
                        .map(|c| c + &format!(" && start{}[tid]", line));
                    program.add(Block::new(
                        UprobeOffset(function, callsite.relative_ip + callsite.length as u32),
                        call_done_filter,
                        vec![
                            format!("@duration{line} += nsecs - @start{line}[tid]", line = line),
                            format!("@count{} += 1", line),
                            format!("delete(@start{}[tid])", line),
                        ],
                    ));
                }

                let mut print_exprs = vec![Printf {
                    format: r#"{"time": %d, "lines": {"#.to_string(),
                    args: vec!["(nsecs - @start_time) / 1000000000".to_string()],
                }];
                for (i, line) in lines.iter().enumerate() {
                    let mut format = format!(r#""{}": [%lld, %lld]"#, line);
                    if i != lines.len() - 1 {
                        format.push_str(", ");
                    }
                    print_exprs.push(Printf {
                        format,
                        args: vec![format!("@duration{}", line), format!("@count{}", line)],
                    });
                }
                print_exprs.push(Printf {
                    format: r#"}}\n"#.to_string(),
                    args: Vec::new(),
                });
                program.add(Block::new(
                    BlockType::Interval { rate_seconds: 1 },
                    None,
                    print_exprs,
                ));
            }
            TraceMode::Histogram => {
                program.add(Block::new(
                    Uretprobe(frame.function),
                    depth_condition(frame_depth + 1),
                    vec![
                        format!("@histogram = hist(nsecs - @start{}[tid])", line),
                        format!("delete(@start{}[tid])", line),
                        format!("@depth[tid] = {}", frame_depth),
                    ],
                ));

                let print_exprs = vec![
                    Printf {
                        format: r#"{"time": %d, "histogram": ""#.to_string(),
                        args: vec!["(nsecs - @start_time) / 1000000000".to_string()],
                    },
                    Expression::Print("@histogram".to_string()),
                    Printf {
                        format: r#""}\n"#.to_string(),
                        args: Vec::new(),
                    },
                ];
                program.add(Block::new(
                    BlockType::Interval { rate_seconds: 1 },
                    None,
                    print_exprs,
                ));
            }
        };

        let expr = program.compile(&self.program_path);
        log::debug!("Current bpftrace expression: {}", expr);
        // Since we hold lock we know counter won't change
        (expr, self.counter.load(Ordering::Relaxed))
    }

    /// Parse bpftrace output
    pub fn parse(line: &str, counter: u64) -> Result<TraceInfo, serde_json::Error> {
        // Histogram is printed with newlines, we need to escape it to be valid
        // JSON.
        let line = line.replace("\n", "\\n");
        let info: TraceOutput = serde_json::from_str(&line)?;
        let traces = match info.lines {
            Some(lines) => TraceInfoMode::Lines(
                lines
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
            ),
            None => TraceInfoMode::Histogram(info.histogram.unwrap()),
        };
        Ok(TraceInfo {
            counter,
            time: Duration::from_secs(info.time),
            traces,
        })
    }

    pub fn is_counter_current(&self, counter: u64) -> bool {
        counter == self.counter.load(Ordering::Acquire)
    }
}
