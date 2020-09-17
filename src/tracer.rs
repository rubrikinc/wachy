use crate::error::Error;
use std::process::Command;

/// Encapsulates a method of tracing a particular program and its functions.
pub struct Tracer<'a> {
    callback: Box<dyn FnMut(TraceData) -> () + 'a>,
}

pub struct TraceData {}

impl<'a> Tracer<'a> {
    pub fn new(callback: Box<dyn FnMut(TraceData) -> () + 'a>) -> Result<Tracer<'a>, Error> {
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

        Ok(Tracer { callback })
    }
}
