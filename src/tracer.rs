use crate::error::Error;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Encapsulates a scheme for tracing a particular program and its functions
pub struct Tracer {
    /// Callback run whenever new trace data is generated
    callback: Option<Box<dyn FnMut(TraceData) -> ()>>,
}

pub struct TraceData {}

impl Tracer {
    pub fn new() -> Result<Arc<Mutex<Tracer>>, Error> {
        match Command::new("bpftrace").arg("--version").output() {
            Ok(output) => log::trace!("bpftrace version: {:?}", output),
            Err(err) => {
                let msg = match err.kind() {
                    std::io::ErrorKind::NotFound => format!("bpftrace not found. See https://github.com/iovisor/bpftrace/blob/master/INSTALL.md for instructions on installation."),
                    _ => format!("Error running bpftrace: {:?}", err),
                };
                return Err(msg.into());
            }
        }

        Ok(Arc::new(Mutex::new(Tracer { callback: None })))
    }

    pub fn set_callback(&mut self, callback: Box<dyn FnMut(TraceData) -> ()>) {
        self.callback = Some(callback)
    }

    pub fn set_traced_function(&mut self, function: crate::program::FunctionName) {
        // Command::new("bpftrace")
    }
}
