use crate::error::Error;
use crate::program::Program;
use crate::tracer::{TraceData, Tracer};
use crate::views;
use cursive::traits::{Nameable, Resizable};
use std::io::BufRead;

pub struct Controller<'a> {
    program: Program<'a>,
    tracer: Tracer<'a>,
}

impl<'a> Controller<'a> {
    pub fn start(program: Program<'a>, function_name: &str) -> Result<(), Error> {
        let tracer = Tracer::new(Box::new(Controller::handle_trace_data))?;

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

        let controller = Controller { program, tracer };

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

        siv.set_user_data(controller);
        siv.run();
        Ok(())
    }

    pub fn run(&mut self) {
        self.siv.run();
    }

    fn handle_trace_data(data: TraceData) {}
}
