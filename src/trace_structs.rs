use itertools::Itertools;

use crate::bpftrace_compiler::BlockType::{Uprobe, UprobeOffset, Uretprobe};
use crate::bpftrace_compiler::Expression::Printf;
use crate::bpftrace_compiler::{self, Block, BlockType, Expression};
use crate::error::Error;
use crate::events::{Event, TraceCumulative, TraceInfo, TraceInfoMode};
use crate::program::FunctionName;
use std::collections::HashMap;
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use std::{fmt, iter};

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
    /// When in Breakdown mode, trace these functions
    breakdown_functions: Vec<FunctionName>,
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
    /// Trace amount of time spent in each of the specified nest functions
    Breakdown,
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
    /// bpftrace filter to apply on function entry (uprobe)
    filter: Option<String>,
    /// bpftrace filter to apply on function exit (uretprobe). Necessary to
    /// support things like `$duration` which have to be evaluated on return.
    ret_filter: Option<String>,
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
    // Map from (stringified) index to (duration, count)
    breakdown: Option<HashMap<String, (u64, u64)>>,
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
            filter: None,
            ret_filter: None,
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
            breakdown_functions: Vec::new(),
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

    fn command_modified(&self, guard: MutexGuard<Frames>) {
        self.counter.fetch_add(1, Ordering::Release);
        guard.tx.send(Event::TraceCommandModified).unwrap();
    }

    /// Remove traced callsite, returning true if one exists corresponding to this line.
    pub fn remove_callsite(&self, line: u32) -> bool {
        let mut guard = self.stack.lock().unwrap();
        let top_frame = guard.frames.last_mut().unwrap();
        if top_frame.traced_callsites.remove(&line).is_some() {
            self.command_modified(guard);
            true
        } else {
            false
        }
    }

    pub fn push(&self, frame: FrameInfo) {
        let mut guard = self.stack.lock().unwrap();
        // TODO prevent recursive (or do we need to?)
        guard.frames.push(frame);
        self.command_modified(guard);
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
        self.command_modified(guard);
        Some(frame)
    }

    pub fn set_mode(&self, mode: TraceMode) {
        let mut guard = self.stack.lock().unwrap();
        guard.mode = mode;
        self.command_modified(guard);
    }

    pub fn get_current_filter(&self, is_ret_filter: bool) -> Option<String> {
        let mut guard = self.stack.lock().unwrap();
        if is_ret_filter {
            guard.frames.last_mut().unwrap().ret_filter.clone()
        } else {
            guard.frames.last_mut().unwrap().filter.clone()
        }
    }

    /// Set the filter for the current function, with `is_ret_filter` denoting
    /// whether it should apply on function return (each one can be set
    /// independently). Empty string removes the filter. Checks that it is valid
    /// bpftrace syntax, returning a descriptive error message if not.
    pub fn set_current_filter(&self, filter: String, is_ret_filter: bool) -> Result<(), Error> {
        let mut guard = self.stack.lock().unwrap();
        let frame = guard.frames.last_mut().unwrap();
        let frame_filter = if is_ret_filter {
            &mut frame.ret_filter
        } else {
            &mut frame.filter
        };
        if filter.is_empty() {
            *frame_filter = None;
            self.command_modified(guard);
            return Ok(());
        }

        let prev_filter = frame_filter.clone();
        *frame_filter = Some(filter);
        // Run bpftrace in dry run mode to ensure filter compiles
        let output = bpftrace_cmd()
            .args(&["-d", "-e", &self.get_bpftrace_expr_locked(&guard).0])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("bpftrace failed to start");
        if !output.status.success() {
            // Restore old filter on error. Can't reference `frame_filter`
            // directly here due to lifetimes.
            if is_ret_filter {
                guard.frames.last_mut().unwrap().ret_filter = prev_filter;
            } else {
                guard.frames.last_mut().unwrap().filter = prev_filter;
            }
            Err(String::from_utf8(output.stderr).unwrap().into())
        } else {
            self.command_modified(guard);
            Ok(())
        }
    }

    pub fn add_breakdown_function(&self, function: FunctionName) {
        let mut guard = self.stack.lock().unwrap();
        guard.breakdown_functions.push(function);
    }

    pub fn get_breakdown_functions(&self) -> Vec<FunctionName> {
        let guard = self.stack.lock().unwrap();
        guard.breakdown_functions.clone()
    }

    /// Get appropriate bpftrace expression for current state, along with
    /// current counter value.
    /// Panics if called with empty stack
    pub fn get_bpftrace_expr(&self) -> (String, u64) {
        let guard = self.stack.lock().unwrap();
        self.get_bpftrace_expr_locked(&guard)
    }

    fn get_bpftrace_expr_locked(&self, guard: &MutexGuard<Frames>) -> (String, u64) {
        // General approach to codegen:
        // 1. Maintain `@depth` on function entry and exit to ensure we are
        //    following the trace stack.
        // 2. Source line number is used in variable naming to identify the
        //    traces.
        // 3. In `TraceMode::Line`, results are stored as duration and count.
        // 4. Current thread's trace info is stored in `_tmp` vars, only after
        //    we verify all the `RetFilter`s we move it to the global vars which
        //    are output.
        let frames = &guard.frames;
        let num_retfilters: u32 = frames
            .iter()
            .map(|f| match f.ret_filter {
                Some(_) => 1,
                None => 0,
            })
            .sum();

        let mut program = bpftrace_compiler::Program::new();
        program.add(Block::new(
            BlockType::Begin,
            None,
            vec![
                "@start_time = nsecs",
                "@depth[-1] = 0",
                "@matched_retfilters[-1] = 0",
            ],
        ));

        let depth_condition =
            |depth: usize| -> Option<String> { Some(format!("@depth[tid] == {}", depth)) };
        for (i, frame) in frames.iter().take(frames.len() - 1).enumerate() {
            program.add(Block::new(
                Uprobe(frame.function),
                depth_condition(i),
                TraceStack::add_user_filter(
                    &frame.filter,
                    false,
                    vec![
                        format!("@depth[tid] = {}", i + 1),
                        format!("@start_frame{}[tid] = nsecs", i),
                    ],
                ),
            ));
            program.add(Block::new(
                Uretprobe(frame.function),
                depth_condition(i + 1),
                TraceStack::add_user_filter(
                    &frame.ret_filter,
                    true,
                    vec![
                        format!("@depth[tid] = {}", i),
                        format!("$duration = nsecs - @start_frame{}[tid]", i),
                    ],
                ),
            ));
        }

        let last_frame = frames.last().unwrap();
        let lines: Vec<u32> = last_frame
            .traced_callsites
            .iter()
            .map(|(line, _)| *line)
            .chain(iter::once(last_frame.source_line))
            .collect();
        let frame_depth = frames.len() - 1;
        let line = last_frame.source_line;
        let function = last_frame.function;

        program.add(Block::new(
            Uprobe(function),
            depth_condition(frame_depth),
            TraceStack::add_user_filter(
                &last_frame.filter,
                false,
                vec![
                    format!("@start{}[tid] = nsecs", line),
                    format!("@depth[tid] = {}", frame_depth + 1),
                ],
            ),
        ));

        match guard.mode {
            TraceMode::Line => {
                program.add(Block::new(
                    Uretprobe(function),
                    depth_condition(frame_depth + 1),
                    TraceStack::add_user_filter(
                        &last_frame.ret_filter,
                        true,
                        vec![
                            format!(
                                "@duration_tmp{line}[tid] += (nsecs - @start{line}[tid])",
                                line = line
                            ),
                            format!("$duration = @duration_tmp{}[tid]", line),
                            format!("@count_tmp{}[tid] += 1", line),
                            format!("delete(@start{}[tid])", line),
                            format!("@depth[tid] = {}", frame_depth),
                        ],
                    ),
                ));

                for (&line, callsite) in &last_frame.traced_callsites {
                    program.add(Block::new(
                        UprobeOffset(function, callsite.relative_ip),
                        depth_condition(frame_depth + 1),
                        vec![format!("@start{}[tid] = nsecs", line)],
                    ));
                    // Ensure the tracepoint at the end of the call is only
                    // triggered if we traced the start.
                    let call_done_condition = depth_condition(frame_depth + 1)
                        .map(|c| c + &format!(" && @start{}[tid]", line));
                    program.add(Block::new(
                        UprobeOffset(function, callsite.relative_ip + callsite.length as u32),
                        call_done_condition,
                        vec![
                            format!(
                                "@duration_tmp{line}[tid] += (nsecs - @start{line}[tid])",
                                line = line
                            ),
                            format!("@count_tmp{}[tid] += 1", line),
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
                    Uretprobe(last_frame.function),
                    depth_condition(frame_depth + 1),
                    TraceStack::add_user_filter(
                        &last_frame.ret_filter,
                        true,
                        vec![
                            format!("@duration_tmp[tid] = nsecs - @start{}[tid]", line),
                            "$duration = @duration_tmp[tid]".to_string(),
                            format!("delete(@start{}[tid])", line),
                            format!("@depth[tid] = {}", frame_depth),
                        ],
                    ),
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
            TraceMode::Breakdown => {
                // Need `+=` here for most variables rather than `=` because we
                // only "commit" the values after returning from the topmost
                // frame (we need to ensure ret filters are satisfied). During
                // that time we may reach other intermediate/nested frames
                // multiple times but still have to accumulate time for all of
                // them.
                program.add(Block::new(
                    Uretprobe(function),
                    depth_condition(frame_depth + 1),
                    TraceStack::add_user_filter(
                        &last_frame.ret_filter,
                        true,
                        vec![
                            format!("@duration_tmp[tid] += (nsecs - @start{}[tid])", line),
                            "$duration = @duration_tmp[tid]".to_string(),
                            "@count_tmp[tid] += 1".to_string(),
                            format!("delete(@start{}[tid])", line),
                            format!("@depth[tid] = {}", frame_depth),
                        ],
                    ),
                ));
                for (i, &function) in guard.breakdown_functions.iter().enumerate() {
                    program.add(Block::new(
                        Uprobe(function),
                        depth_condition(frame_depth + 1),
                        vec![format!("@start_breakdown{}[tid] = nsecs", i)],
                    ));
                    // If a function is called recursively, `@start_breakdown`
                    // var will have been cleared after first (most nested)
                    // retprobe. Ensure we only accumulate if `@start_breakdown`
                    // is set - this will result in inaccurate traces but at
                    // least prevent underflow.
                    // TODO properly handle recursive calls
                    let ret_condition = depth_condition(frame_depth + 1)
                        .map(|c| c + &format!(" && @start_breakdown{}[tid]", i));
                    program.add(Block::new(
                        Uretprobe(function),
                        ret_condition,
                        vec![
                            format!(
                                "@duration_breakdown_tmp{i}[tid] += (nsecs - @start_breakdown{i}[tid])",
                                i = i
                            ),
                            format!("@count_breakdown_tmp{}[tid] += 1", i),
                            format!("delete(@start_breakdown{}[tid])", i),
                        ],
                    ));
                }

                let mut print_exprs = vec![Printf {
                    format: r#"{"time": %d, "breakdown": {"#.to_string(),
                    args: vec!["(nsecs - @start_time) / 1000000000".to_string()],
                }];
                let num_breakdown_functions = guard.breakdown_functions.len();
                let mut format = r#""last_frame": [%lld, %lld]"#.to_string();
                if num_breakdown_functions > 0 {
                    format.push_str(", ");
                }
                print_exprs.push(Printf {
                    format,
                    args: vec!["@duration".to_string(), "@count".to_string()],
                });
                for i in 0..num_breakdown_functions {
                    let mut format = format!(r#""{}": [%lld, %lld]"#, i);
                    if i != num_breakdown_functions - 1 {
                        format.push_str(", ");
                    }
                    print_exprs.push(Printf {
                        format,
                        args: vec![
                            format!("@duration_breakdown{}", i),
                            format!("@count_breakdown{}", i),
                        ],
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
        };

        // Add expression to commit `_tmp` vars to their final version when
        // appropriate and always clear. This should happen in the first/topmost
        // retprobe, which can be in parent trace frame, or if there are none
        // then `last_frame`, so easiest to patch it up at the end.
        let last_retprobe = program
            .iter_mut()
            .find(|b| match b.get_type() {
                Uretprobe(_) => true,
                _ => false,
            })
            .unwrap();
        match guard.mode {
            TraceMode::Line => {
                last_retprobe.add(Expression::If {
                    condition: format!("@matched_retfilters[tid] == {}", num_retfilters),
                    body: lines
                        .iter()
                        .map(|line| {
                            format!(
                                "@duration{line} += @duration_tmp{line}[tid]; @count{line} += @count_tmp{line}[tid]",
                                line = line
                            )
                        })
                        .map(|e| e.into())
                        .collect(),
                });
                last_retprobe.extend(
                    lines
                        .iter()
                        .map(|line| {
                            format!(
                                "delete(@duration_tmp{line}[tid]); delete(@count_tmp{line}[tid])",
                                line = line
                            )
                        })
                        .chain(iter::once("delete(@matched_retfilters[tid])".to_string()))
                        .collect(),
                );
            }
            TraceMode::Histogram => {
                last_retprobe.add(Expression::If {
                    // We may not have actually reached the place where
                    // `@duration_tmp` is set, so check that it is non-zero.
                    // TODO are we guaranteed duration will be non-zero when
                    // actually hit or would this end up dropping 0ns calls?
                    condition: format!(
                        "@matched_retfilters[tid] == {} && @duration_tmp[tid]",
                        num_retfilters
                    ),
                    body: vec!["@histogram = hist(@duration_tmp[tid])".into()],
                });
                last_retprobe.extend(vec![
                    "delete(@duration_tmp[tid])",
                    "delete(@matched_retfilters[tid])",
                ]);
            }
            TraceMode::Breakdown => {
                last_retprobe.add(Expression::If {
                    condition: format!(
                        "@matched_retfilters[tid] == {}",
                        num_retfilters
                    ),
                    body: guard
                        .breakdown_functions
                        .iter()
                        .enumerate()
                        .map(|(i, _)| {
                            format!(
                                "@duration_breakdown{i} += @duration_breakdown_tmp{i}[tid]; @count_breakdown{i} += @count_breakdown_tmp{i}[tid]",
                                i = i
                            )
                        })
                        .chain(iter::once("@duration += @duration_tmp[tid]; @count += @count_tmp[tid]".to_string()))
                        .map(|e| e.into())
                        .collect(),
                });
                last_retprobe.extend(
                    guard
                        .breakdown_functions
                        .iter()
                        .enumerate()
                        .map(|(i, _)| {
                            format!(
                                "delete(@duration_breakdown_tmp{i}[tid]); delete(@count_breakdown_tmp{i}[tid])",
                                i = i
                            )
                        })
                        .chain(iter::once("delete(@matched_retfilters[tid]); delete(@duration_tmp[tid]); delete(@count_tmp[tid])".to_string()))
                        .collect(),
                );
            }
        };

        let expr = program.compile(&self.program_path);
        log::debug!("Current bpftrace expression: {}", expr);
        // Since we hold lock we know counter won't change
        (expr, self.counter.load(Ordering::Relaxed))
    }

    fn add_user_filter<T>(
        filter: &Option<String>,
        is_ret_filter: bool,
        exprs: Vec<T>,
    ) -> Vec<Expression>
    where
        T: Into<Expression>,
    {
        let mut exprs = exprs.into_iter().map(|e| e.into()).collect();
        match filter {
            None => exprs,
            Some(f) => {
                // If this is a ret filter, we need to update depth (i.e. run
                // `exprs`) unconditionally, but maintain
                // `@matched_retfilters[tid]` depending on the filter. For an
                // entry filter, we skip updating depth if it doesn't match.

                // TODO need to use bitwise `|=` rather than ++
                if is_ret_filter {
                    exprs.push(Expression::If {
                        condition: f.clone(),
                        body: vec!["@matched_retfilters[tid] += 1".into()],
                    });
                    exprs
                } else {
                    vec![Expression::If {
                        condition: f.clone(),
                        body: exprs,
                    }]
                }
            }
        }
    }

    /// Parse bpftrace output
    pub fn parse(line: &str, counter: u64) -> Result<TraceInfo, serde_json::Error> {
        // Histogram is printed with newlines, we need to escape it to be valid
        // JSON.
        let line = line.replace("\n", "\\n");
        let info: TraceOutput = serde_json::from_str(&line)?;
        let tuple_to_trace_cumulative = |tuple: (u64, u64)| -> TraceCumulative {
            TraceCumulative {
                duration: Duration::from_nanos(tuple.0),
                count: tuple.1,
            }
        };
        let traces = if let Some(lines) = info.lines {
            TraceInfoMode::Lines(
                lines
                    .into_iter()
                    .map(|(line, value)| {
                        // If JSON parsing succeeded we assume it is valid output, so `line` must be valid to parse
                        (
                            line.parse::<u32>().unwrap(),
                            tuple_to_trace_cumulative(value),
                        )
                    })
                    .collect(),
            )
        } else if let Some(histogram) = info.histogram {
            TraceInfoMode::Histogram(histogram)
        } else {
            let breakdown = info.breakdown.unwrap();
            TraceInfoMode::Breakdown {
                last_frame_trace: tuple_to_trace_cumulative(breakdown["last_frame"]),
                breakdown_traces: breakdown
                    .into_iter()
                    .filter(|(k, _)| k != "last_frame")
                    .map(|(i, value)| (i.parse::<u32>().unwrap(), tuple_to_trace_cumulative(value)))
                    .sorted_by_key(|(i, _)| *i)
                    .map(|(_, v)| v)
                    .collect(),
            }
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

pub fn bpftrace_cmd() -> Command {
    Command::new("bpftrace")
}
