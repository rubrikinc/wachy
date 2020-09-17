use crate::error::Error;
use crate::program::Program;
use crate::tracer::{TraceData, Tracer};
use crate::views;
use cursive::traits::{Nameable, Resizable};
use std::io::BufRead;
use std::sync::{Arc, Mutex};

pub struct Controller {
    program: Program,
    tracer: Arc<Mutex<Tracer>>,
}

impl Controller {
    pub fn run(program: Program, function_name: &str) -> Result<(), Error> {
        let tracer = Tracer::new()?;

        let matches = program.get_matches(function_name);
        // TODO ensure one and only one match
        let function = matches.into_iter().next().unwrap();
        let location = program.get_location(function);
        let source_file = location.file.ok_or(format!("Failed to get source file name corresponding to function {}, please ensure {} has debugging symbols", function_name, program.file_path))?;
        let source_line = location.line.ok_or(format!("Failed to get source file lin number corresponding to function {}, please ensure {} has debugging symbols", function_name, program.file_path))?;
        log::debug!(
            "Function {} is at {}:{}",
            function_name,
            source_file,
            source_line
        );

        let file = std::fs::File::open(source_file).unwrap();
        let source_code: Vec<String> = std::io::BufReader::new(file)
            .lines()
            .map(|l| l.unwrap())
            .collect();

        // The line mapping starts inside function body, subtract one to try to
        // show header.
        let start_line = source_line.saturating_sub(1);
        let source_view = views::new_source_view(source_code, start_line);
        let mut siv = cursive::default();
        siv.add_layer(
            cursive::views::Dialog::around(source_view.with_name("source_view"))
                .title(format!("wachy | {}", program.file_path))
                .full_screen(),
        );

        let controller = Arc::new(Mutex::new(Controller { program, tracer }));
        let controller_ref = Arc::downgrade(&controller);
        controller
            .lock()
            .unwrap()
            .tracer
            .lock()
            .unwrap()
            .set_callback(Box::new(move |data| {
                // If tracer is alive then controller must be as well, so unwrap is safe
                controller_ref
                    .upgrade()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .handle_trace_data(data);
            }));

        siv.set_user_data(controller);
        siv.run();
        Ok(())
    }

    fn handle_trace_data(&mut self, data: TraceData) {}
}
