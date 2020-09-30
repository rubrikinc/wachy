use crate::error::Error;
use crate::program::{FunctionName, Program};
use crate::trace_structs::{CallInstruction, FrameInfo, TraceStack};
use crate::tracer::{TraceData, Tracer};
use crate::views;
use cursive::traits::{Nameable, Resizable};
use cursive::Cursive;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::io::BufRead;
use std::sync::{mpsc, Arc};
use zydis::enums::generated::{AddressWidth, FormatterStyle, MachineMode, Mnemonic};
use zydis::ffi::Decoder;
use zydis::formatter::{Formatter, OutputBuffer};

pub struct Controller {
    program: Program,
    tracer: Tracer,
    trace_stack: Arc<TraceStack>,
}

impl Controller {
    pub fn run(program: Program, function_name: &str) -> Result<(), Error> {
        let matches = program.get_matches(function_name);
        // TODO ensure one and only one match
        let function = matches.into_iter().next().unwrap();
        let location = program.get_location(program.get_address(function)).unwrap();
        let source_file = location.file.ok_or(format!("Failed to get source file name corresponding to function {}, please ensure {} has debugging symbols", function_name, program.file_path))?;
        let source_line = location.line.ok_or(format!("Failed to get source file line number corresponding to function {}, please ensure {} has debugging symbols", function_name, program.file_path))?;
        log::info!(
            "Function {} is at {}:{}",
            function_name,
            source_file,
            source_line
        );

        let trace_stack = Arc::new(TraceStack::new(
            program.file_path.clone(),
            Controller::create_frame_info(
                &program,
                function,
                String::from(source_file),
                source_line,
            ),
        ));
        let (tx, rx) = mpsc::channel();
        let tracer = Tracer::new(Arc::clone(&trace_stack), tx)?;

        // TODO cache file contents
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
        siv.add_global_callback('x', |s| {
            let view = s.find_name::<views::SourceView>("source_view").unwrap();
            let line = view.row().unwrap() as u32 + 1;
            let controller = s.user_data::<Controller>().unwrap();
            let callsites = controller.trace_stack.get_callsites(line);
            if !callsites.is_empty() {
                if callsites.len() > 1 {
                    // TODO
                } else {
                    controller.update_trace_stack(|ts: &TraceStack| {
                        ts.add_callsite(line, callsites.into_iter().nth(0).unwrap())
                    });
                }
            } else {
                // TODO show error
            }
        });

        let controller = Controller {
            program,
            tracer,
            trace_stack,
        };
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
                // Ignore any data that doesn't correspond to current view. The trace command should
                // already be in the process of being updated.
                if !siv
                    .user_data::<Controller>()
                    .unwrap()
                    .trace_stack
                    .is_counter_current(data.counter)
                {
                    return Ok(());
                }
                siv.call_on_name("source_view", |table: &mut views::SourceView| {
                    let items = table.borrow_items_mut();
                    for (line, info) in data.traces {
                        // TODO check for err
                        let item = items.get_mut(line as usize - 1).unwrap();
                        if info.count != 0 {
                            item.latency = Some(info.duration / u32::try_from(info.count).unwrap());
                        }
                        item.frequency = Some(info.count as f32 / data.time.as_secs_f32());
                    }
                });
                siv.refresh();
                Ok(())
            }
        }
    }

    fn create_frame_info(
        program: &Program,
        function: FunctionName,
        source_file: String,
        source_line: u32,
    ) -> FrameInfo {
        let (start_address, code) = program.get_data(function).unwrap();
        let formatter = Formatter::new(FormatterStyle::INTEL).unwrap();
        let decoder = Decoder::new(MachineMode::LONG_64, AddressWidth::_64).unwrap();
        let mut buffer = [0u8; 200];
        let mut buffer = OutputBuffer::new(&mut buffer[..]);

        let mut line_to_callsites = HashMap::<u32, Vec<CallInstruction>>::new();

        // 0 is the address for our code.
        for (instruction, ip) in decoder.instruction_iterator(code, start_address) {
            if instruction.mnemonic == Mnemonic::CALL {
                if log::log_enabled!(log::Level::Trace) {
                    formatter
                        .format_instruction(&instruction, &mut buffer, Some(ip), None)
                        .unwrap();
                    log::trace!("{} 0x{:016X} {}", instruction.operand_count, ip, buffer);
                }

                assert!(instruction.operand_count > 0);
                let relative_ip = u32::try_from(ip - start_address).unwrap();
                let call_address = instruction
                    .calc_absolute_address(ip, &instruction.operands[0])
                    .unwrap();
                // TODO handle register
                let callsite = if program.is_dynamic_symbol(call_address) {
                    CallInstruction::dynamic_symbol(relative_ip, instruction.length, call_address)
                } else {
                    let function = program.get_function_for_address(call_address).unwrap();
                    CallInstruction::function(relative_ip, instruction.length, function)
                };
                let location = program.get_location(ip).unwrap();
                assert!(location.file.unwrap() == source_file);
                line_to_callsites
                    .entry(location.line.unwrap())
                    .or_default()
                    .push(callsite);
            }
        }

        log::trace!("{:?}", line_to_callsites);

        FrameInfo::new(function, source_file, source_line, line_to_callsites)
    }

    pub fn update_trace_stack<F>(&self, f: F)
    where
        F: FnOnce(&TraceStack),
    {
        f(self.trace_stack.as_ref());
        self.tracer.rerun_tracer();
    }
}
