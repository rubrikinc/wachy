use crate::error::Error;
use crate::program::Program;
use crate::tracer::{TraceData, Tracer};
use crate::views;
use cursive::traits::{Nameable, Resizable};
use cursive::Cursive;
use std::io::BufRead;
use std::sync::mpsc;

pub struct Controller {
    program: Program,
    tracer: Tracer,
}

impl Controller {
    pub fn run(program: Program, function_name: &str) -> Result<(), Error> {
        let (tx, rx) = mpsc::channel();
        let tracer = Tracer::new(program.file_path.clone(), tx)?;

        let matches = program.get_matches(function_name);
        // TODO ensure one and only one match
        let function = matches.into_iter().next().unwrap();
        let location = program.get_location(function);
        let source_file = location.file.ok_or(format!("Failed to get source file name corresponding to function {}, please ensure {} has debugging symbols", function_name, program.file_path))?;
        let source_line = location.line.ok_or(format!("Failed to get source file line number corresponding to function {}, please ensure {} has debugging symbols", function_name, program.file_path))?;
        log::debug!(
            "Function {} is at {}:{}",
            function_name,
            source_file,
            source_line
        );
        tracer.reset_traced_function(source_line, function);

        let file = std::fs::File::open(source_file).unwrap();
        let source_code: Vec<String> = std::io::BufReader::new(file)
            .lines()
            .map(|l| l.unwrap())
            .collect();

        let source_view = views::new_source_view(source_code, source_line);
        let mut siv = cursive::default();
        siv.add_layer(
            cursive::views::Dialog::around(source_view.with_name("source_view"))
                .title(format!("wachy | {}", program.file_path))
                .full_screen(),
        );

        let controller = Controller { program, tracer };
        siv.set_user_data(controller);

        siv.refresh();
        while siv.is_running() {
            siv.step();
            match rx.try_recv() {
                Ok(data) => Controller::handle_trace_data(&mut siv, data)?,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(format!("Unexpected error: trace channel disconnected").into())
                }
                Err(mpsc::TryRecvError::Empty) => (),
            }
        }
        Ok(())
    }

    fn handle_trace_data(siv: &mut Cursive, data: TraceData) -> Result<(), Error> {
        match data {
            TraceData::FatalError(message) => {
                siv.quit();
                Err(message.into())
            }
            TraceData::Data(data) => {
                siv.call_on_name("source_view", |table: &mut views::SourceView| {
                    let items = table.borrow_items_mut();
                    for (line, info) in data.traces {
                        // TODO check for err
                        let item = items.get_mut(line as usize - 1).unwrap();
                        item.latency = Some(info.duration / info.count as u32);
                        item.frequency = Some(info.count as f32 / data.time.as_secs_f32());
                    }
                });
                Ok(())
            }
        }
    }
}
