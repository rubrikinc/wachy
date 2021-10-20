use crate::error::Error;
use crate::events::Event;
use crate::trace_structs::TraceStack;
use std::io::{BufRead, Read};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

/// Encapsulates a scheme for tracing a particular program and its functions
pub struct Tracer {
    tx: mpsc::Sender<TraceCommand>,
    command_thread: Option<thread::JoinHandle<()>>,
}

enum TraceCommand {
    /// TraceStack has changed, rerun the tracer from scratch
    RerunTracer,
    Exit,
}

impl Tracer {
    pub fn run_prechecks() -> Result<(), Error> {
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

        Ok(())
    }

    /// tx is used to transmit trace data in response to the requests given to
    /// this class.
    pub fn new(
        trace_stack: Arc<TraceStack>,
        data_tx: mpsc::Sender<Event>,
    ) -> Result<Tracer, Error> {
        let (command_tx, command_rx) = mpsc::channel();
        let command_thread = thread::spawn(move || {
            TraceCommandHandler::new(trace_stack, data_tx).run(command_rx);
        });
        let tracer = Tracer {
            tx: command_tx,
            command_thread: Some(command_thread),
        };

        Ok(tracer)
    }

    /// Rerun tracer after modifying TraceStack (results of which will be sent
    /// to the callback). This is non-blocking - actual tracing updates will
    /// happen in the background. However, `data_tx` is guaranteed to only be
    /// sent to if TraceStack::counter matches what it was when the tracer was
    /// started.
    pub fn rerun_tracer(&self) {
        self.tx.send(TraceCommand::RerunTracer).unwrap()
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
    data_tx: mpsc::Sender<Event>,
    trace_stack: Arc<TraceStack>,
    /// Used to track bpftrace pid so we can kill it when needed
    program_id: Option<u32>,
    output_processor: Option<thread::JoinHandle<()>>,
    /// Usually bpftrace exits successfully on SIGTERM, but that's not the case
    /// if it's killed during setup. If bpftrace has an error on exit, we use
    /// this to track if we tried to kill it and if so ignore the error,
    /// otherwise display an error and exit ourselves.
    is_killing: Arc<AtomicBool>,
}

impl TraceCommandHandler {
    fn new(trace_stack: Arc<TraceStack>, data_tx: mpsc::Sender<Event>) -> TraceCommandHandler {
        TraceCommandHandler {
            data_tx,
            trace_stack,
            program_id: None,
            output_processor: None,
            is_killing: Arc::new(AtomicBool::new(false)),
        }
    }

    fn run(mut self, command_rx: mpsc::Receiver<TraceCommand>) {
        self.rerun_bpftrace();
        for cmd in command_rx {
            match cmd {
                TraceCommand::RerunTracer => self.rerun_bpftrace(),
                TraceCommand::Exit => return,
            }
        }
    }

    fn rerun_bpftrace(&mut self) {
        self.is_killing.store(true, Ordering::Release);
        self.program_id.map(|pid| unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        });
        self.output_processor.take().map(|t| t.join());
        self.is_killing.store(false, Ordering::Release);

        let (expr, counter) = self.trace_stack.get_bpftrace_expr();
        let mut program = Command::new("bpftrace")
            .args(&["-e", &expr])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("bpftrace failed to start");
        self.program_id = Some(program.id());
        log::trace!("bpftrace program_id: {:?}", self.program_id);
        let tx = self.data_tx.clone();
        let is_killing_copy = Arc::clone(&self.is_killing);
        self.output_processor = Some(thread::spawn(move || {
            let stdout = program.stdout.as_mut().unwrap();
            let stdout_reader = std::io::BufReader::new(stdout);
            log::trace!("Starting!");
            let mut json_buf = String::new();
            for line in stdout_reader.lines() {
                log::trace!("bpftrace stdout: {:?}", line);
                let line = match line {
                    Err(_) => continue,
                    Ok(line) => line,
                };
                // Histograms are printed across multiple lines - we need to
                // collect and send them all in one call. We detect line ending
                // in `}` and use that to assume end of JSON.
                // Is there a better way to do this?
                if !json_buf.is_empty() {
                    json_buf += "\n";
                    json_buf += &line;
                } else if !line.starts_with("{") {
                    // bpftrace prints all maps on exit, which we want to ignore
                    continue;
                } else {
                    json_buf = line;
                }
                if json_buf.ends_with("}") {
                    let parsed = TraceStack::parse(&json_buf, counter);
                    let parsed = match parsed {
                        Err(err) => {
                            tx.send(Event::FatalTraceError(format!(
                                "Failed to parse bpftrace output '{}': {:?}",
                                json_buf, err
                            )))
                            .unwrap();
                            continue;
                        }
                        Ok(parsed) => parsed,
                    };
                    tx.send(Event::TraceData(parsed)).unwrap();
                    json_buf.clear();
                }
            }
            let status = program.wait().unwrap();
            log::trace!("Done, status: {}!", status);
            let mut stderr = String::new();
            match program.stderr.unwrap().read_to_string(&mut stderr) {
                Err(err) => log::error!("Failed to read bpftrace stderr: {:?}", err),
                _ => (),
            }
            if !status.success() && !is_killing_copy.load(Ordering::Acquire) {
                tx.send(Event::FatalTraceError(format!(
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
