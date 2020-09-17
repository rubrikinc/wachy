use crate::error::Error;

pub struct Tracer {}

impl Tracer {
    pub fn new() -> Result<Tracer, Error> {
        // TODO check if bpftrace available
        Ok(Tracer {})
    }
}
