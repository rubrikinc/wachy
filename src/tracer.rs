use crate::error::Error;
use crate::program::FunctionName;
use std::collections::HashMap;
use std::io::{BufRead, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Encapsulates a scheme for tracing a particular program and its functions
pub struct Tracer {
    tx: mpsc::Sender<TraceCommand>,
    command_thread: Option<thread::JoinHandle<()>>,
}

enum TraceCommand {
    /// The number is an arbitrary tag that will be passed back in TraceData
    ResetTraceFunction(u32, FunctionName),
    Exit,
}

pub enum TraceData {
    /// Includes error message. The program should quit on receiving this.
    FatalError(String),
    Data(TraceInfo),
}

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

impl Tracer {
    /// tx is used to transmit trace data in response to the requests given to
    /// this class.
    pub fn new(program_path: String, data_tx: mpsc::Sender<TraceData>) -> Result<Tracer, Error> {
        match Command::new("bpftrace").arg("--version").output() {
            Ok(output) => log::trace!("bpftrace version: {:?}", output),
            Err(err) => {
                let msg = match err.kind() {
                    std::io::ErrorKind::NotFound => format!("bpftrace not found. See https://github.com/iovisor/bpftrace/blob/master/INSTALL.md for installation instructions."),
                    _ => format!("Error running bpftrace: {:?}", err),
                };
                return Err(msg.into());
            }
        }
        // TODO ensure is root

        let (command_tx, command_rx) = mpsc::channel();
        let command_thread = thread::spawn(move || {
            TraceCommandHandler::new(program_path, data_tx).run(command_rx);
        });
        let tracer = Tracer {
            tx: command_tx,
            command_thread: Some(command_thread),
        };

        Ok(tracer)
    }

    /// Set function to trace (results of which will be sent to the callback).
    /// This is non-blocking - actual tracing updates will happen in the
    /// background. However, the callback is guaranteed to only be called after
    /// taking this new update into account.
    pub fn reset_traced_function(&self, tag: u32, function: crate::program::FunctionName) {
        self.tx
            .send(TraceCommand::ResetTraceFunction(tag, function))
            .unwrap()
    }
}

impl Drop for Tracer {
    fn drop(&mut self) {
        self.tx.send(TraceCommand::Exit).unwrap();
        // This is the only place we modify `command_thread`, so it must be
        // non-empty here.
        self.command_thread.take().unwrap().join().unwrap();
    }
}

/// Polls and reacts to issued commands
struct TraceCommandHandler {
    data_tx: mpsc::Sender<TraceData>,
    trace_stack: TraceStack,
    /// Used to track bpftrace pid so we can kill it when needed
    program_id: Option<u32>,
    output_processor: Option<thread::JoinHandle<()>>,
}

impl TraceCommandHandler {
    fn new(program_path: String, data_tx: mpsc::Sender<TraceData>) -> TraceCommandHandler {
        TraceCommandHandler {
            data_tx,
            trace_stack: TraceStack::new(program_path),
            program_id: None,
            output_processor: None,
        }
    }

    fn run(mut self, command_rx: mpsc::Receiver<TraceCommand>) {
        for cmd in command_rx {
            match cmd {
                TraceCommand::ResetTraceFunction(tag, function) => {
                    self.trace_stack.clear();
                    self.trace_stack.push(tag, function);
                    self.rerun_bpftrace();
                }
                TraceCommand::Exit => return,
            }
        }
    }

    fn rerun_bpftrace(&mut self) {
        self.program_id.map(|pid| unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        });
        self.output_processor.take().map(|t| t.join());

        let expr = self.trace_stack.get_bpftrace_expr();
        let mut program = Command::new("bpftrace")
            .args(&["-e", &expr])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("bpftrace failed to start");
        self.program_id = Some(program.id());
        log::trace!("bpftrace program_id: {:?}", self.program_id);
        let tx = self.data_tx.clone();
        self.output_processor = Some(thread::spawn(move || {
            let stdout = program.stdout.as_mut().unwrap();
            let stdout_reader = std::io::BufReader::new(stdout);
            log::trace!("Starting!");
            for line in stdout_reader.lines() {
                log::trace!("bpftrace stdout: {:?}", line);
                // bpftrace prints all maps on exit, which we want to ignore
                let line = match line {
                    Err(_) => continue,
                    Ok(line) => line,
                };
                if !line.starts_with("{") {
                    continue;
                }
                let parsed = TraceStack::parse(&line);
                let parsed = match parsed {
                    Err(err) => {
                        log::error!("Error parsing bpftrace output: {:?}", err);
                        continue;
                    }
                    Ok(parsed) => parsed,
                };
                tx.send(TraceData::Data(parsed));
            }
            let status = program.wait().unwrap();
            let mut stderr = String::new();
            match program.stderr.unwrap().read_to_string(&mut stderr) {
                Err(err) => log::error!("Failed to read bpftrace stderr: {:?}", err),
                _ => (),
            }
            if !status.success() {
                tx.send(TraceData::FatalError(format!(
                    "bpftrace command '{}' failed, status: {:?}, stderr:\n{}",
                    expr, status, stderr
                )))
                .unwrap();
            } else if !stderr.is_empty() {
                log::info!("bpftrace stderr:\n{}", stderr);
            }
        }));
    }
}

/// Manages the stack of functions being traced and helps generate appropriate
/// bpftrace programs.
struct TraceStack {
    program_path: String,
    frames: Vec<(u32, FunctionName)>,
}

#[derive(serde::Deserialize, Debug)]
struct TraceOutput {
    time: u64,
    /// Map from (stringified) tag to (duration, count)
    traces: HashMap<String, (u64, u64)>,
}

impl TraceStack {
    fn new(program_path: String) -> TraceStack {
        TraceStack {
            program_path,
            frames: Vec::new(),
        }
    }

    fn clear(&mut self) {
        self.frames.clear();
    }

    fn push(&mut self, tag: u32, function: FunctionName) {
        self.frames.push((tag, function));
    }

    /// Panics if called with empty stack
    fn get_bpftrace_expr(&self) -> String {
        // Example:
        // BEGIN { @start_time = nsecs } uprobe:/home/ubuntu/test:foo { @start4[tid] = nsecs; } uretprobe:/home/ubuntu/test:foo { @duration4 += nsecs - @start4[tid]; @count4 += 1; delete(@start4[tid]); }  interval:s:1 { printf("{\"time\": %d, \"traces\": {\"4\": [%lld, %lld]}}\n", (nsecs - @start_time) / 1000000000, @duration4, @count4); }
        // We use tag number in variable naming to identify the results.
        // TODO add tests
        let mut parts: Vec<String> = vec!["BEGIN { @start_time = nsecs } ".into()];
        for (i, frame) in self.frames.iter().enumerate() {
            if i != self.frames.len() - 1 {}
        }
        let (tag, function) = self.frames.last().unwrap();
        parts.push(format!(
            "uprobe:{}:{} {{ @start{}[tid] = nsecs; }}",
            self.program_path, function.0, tag
        ));
        parts.push(format!("uretprobe:{}:{} {{ @duration{tag} += nsecs - @start{tag}[tid]; @count{tag} += 1; delete(@start{tag}[tid]); }}", self.program_path, function.0, tag = tag));
        parts.push(format!(r#"interval:s:1 {{ printf("{{\"time\": %d, \"traces\": {{\"{tag}\": [%lld, %lld]}} }}\n", (nsecs - @start_time) / 1000000000, @duration{tag}, @count{tag}); }}"#, tag = tag));
        let expr = parts.concat();
        log::debug!("Current bpftrace expression: {}", expr);
        String::from(expr)
    }

    fn parse(line: &str) -> Result<TraceInfo, serde_json::Error> {
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
