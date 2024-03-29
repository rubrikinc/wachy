use crate::error::Error;
use crate::events;
use crate::events::{Event, TraceInfoMode};
use crate::program;
use crate::program::{FunctionName, Program};
use crate::search;
use crate::search::Searcher;
use crate::trace_structs::{CallInstruction, FrameInfo, InstructionType, TraceMode, TraceStack};
use crate::tracer::Tracer;
use crate::views;
use crate::views::TraceState;
use cursive::traits::{Nameable, Resizable};
use cursive::views::{Dialog, LinearLayout};
use cursive::{Cursive, CursiveRunnable, CursiveRunner};
use program::SymbolInfo;
use std::borrow::Cow;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::io::BufRead;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use zydis::enums::generated::{Mnemonic, Register};

pub struct Controller {
    program: Program,
    searcher: Searcher,
    tracer: Tracer,
    trace_stack: Arc<TraceStack>,
    key_handler: KeyHandler,
}

impl Controller {
    /// For initial function, display searching UI after this many milliseconds
    const DISPLAY_SEARCHING_UI_MS: u128 = 100;

    pub fn run(program: Program, search: &str) -> Result<(), Error> {
        Tracer::run_prechecks()?;

        let (tx, rx) = mpsc::channel();
        let mut siv = cursive::default().into_runner();
        let function = Controller::get_initial_function(
            search,
            &mut siv,
            Searcher::new(tx.clone(), program.symbols_generator()),
            tx.clone(),
            &rx,
        )?;
        let function = match function {
            Some(f) => f,
            None => return Ok(()),
        };

        let mut sview = views::new_source_view();
        let mut fview = views::new_footer_view();
        let frame_info = Controller::setup_function(&program, function, &mut sview, &mut fview)?;
        siv.add_fullscreen_layer(
            cursive::views::Dialog::around(
                LinearLayout::vertical()
                    .child(sview.with_name("source_view").full_screen())
                    .child(fview.with_name("footer_view")),
            )
            .title(format!("wachy | {}", program.file_path))
            .full_screen(),
        );

        let trace_stack = Arc::new(TraceStack::new(
            program.file_path.clone(),
            frame_info,
            tx.clone(),
        ));
        let tracer = Tracer::new(Arc::clone(&trace_stack), tx.clone())?;

        let searcher = Searcher::new(tx, program.symbols_generator());
        Controller::add_callbacks(&mut siv);
        let controller = Controller {
            program,
            searcher,
            tracer,
            trace_stack,
            key_handler: KeyHandler::new(),
        };
        siv.set_user_data(controller);

        siv.refresh();
        while siv.is_running() {
            siv.step();

            match rx.try_recv() {
                Ok(data) => Controller::handle_event(&mut siv, data)?,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(format!("Unexpected error: channel disconnected").into())
                }
                Err(mpsc::TryRecvError::Empty) => (),
            }
        }
        Ok(())
    }

    fn get_initial_function(
        search: &str,
        siv: &mut CursiveRunner<CursiveRunnable>,
        searcher: Searcher,
        tx: mpsc::Sender<Event>,
        rx: &mpsc::Receiver<Event>,
    ) -> Result<Option<FunctionName>, Error> {
        let empty_search_results = vec![(
            "Type to select the top-level function to trace".to_string(),
            None,
        )];
        searcher.setup_search(empty_search_results, Vec::new());
        siv.set_user_data(searcher);
        let search_view = views::new_search_view(
            "Select the top-level function to trace",
            vec![("Searching...".to_string(), None)],
            move |siv: &mut Cursive, view_name: &str, search: &str, n_results: usize| {
                let searcher = siv
                    .user_data::<Searcher>()
                    .expect("Bug: Searcher does not exist");
                searcher.search(view_name, search, n_results);
            },
            move |_, symbol: &SymbolInfo| {
                // TODO cancel any pending searches
                tx.send(Event::SelectedFunction(symbol.name)).unwrap();
            },
        );
        siv.add_layer(search_view);
        // TODO pass name more cleanly
        let callback = siv
            .find_name::<cursive::views::EditView>("search_Select the top-level function to trace")
            .unwrap()
            .set_content(search);
        callback(siv);

        let mut is_initial_result = true;
        let mut start_time = Some(Instant::now());
        while siv.is_running() {
            siv.step();
            match rx.try_recv() {
                Ok(data) => match data {
                    Event::SearchResults {
                        counter,
                        view_name,
                        results,
                    } => {
                        let was_initial_result = is_initial_result;
                        is_initial_result = false;
                        if !siv
                            .user_data::<Searcher>()
                            .expect("Bug: Searcher does not exist")
                            .is_counter_current(counter)
                        {
                            continue;
                        }
                        // If this was the initial search and there's only one
                        // match, consider this to be the selected one.
                        if results.len() == 1 && was_initial_result {
                            if let Some(symbol) = &results[0].1 {
                                siv.pop_layer();
                                return Ok(Some(symbol.name));
                            };
                        }
                        if views::update_search_view(siv, &view_name, results) {
                            siv.refresh();
                        }
                    }
                    Event::SelectedFunction(function) => {
                        siv.pop_layer();
                        return Ok(Some(function));
                    }
                    _ => {
                        panic!("Bug: Unexpected event")
                    }
                },
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(format!("Unexpected error: channel disconnected").into())
                }
                Err(mpsc::TryRecvError::Empty) => (),
            }

            if start_time.map_or(false, |t| {
                t.elapsed().as_millis() > Controller::DISPLAY_SEARCHING_UI_MS
            }) {
                start_time.take();
                // We only refresh after a delay so that if we get a single
                // match and it's finished quickly enough, we can return that
                // without flashing the cursive UI at all.
                siv.refresh();
            }
        }
        // Cancelled
        Ok(None)
    }

    fn handle_event(siv: &mut CursiveRunner<CursiveRunnable>, event: Event) -> Result<(), Error> {
        let result = match event {
            Event::FatalTraceError { error_message } => {
                siv.quit();
                Err(error_message.into())
            }
            Event::TraceData(data) => {
                // Ignore any data that doesn't correspond to current view. The
                // trace command would already be in the process of being
                // updated.
                if !siv
                    .user_data::<Controller>()
                    .expect("Bug: Controller does not exist")
                    .trace_stack
                    .is_counter_current(data.counter)
                {
                    return Ok(());
                }
                let data_time = data.time.as_secs_f32();
                let get_latency = |t: &events::TraceCumulative| -> Duration {
                    t.duration / u32::try_from(t.count).unwrap()
                };
                let get_frequency =
                    |t: &events::TraceCumulative| -> f32 { t.count as f32 / data_time };

                match data.traces {
                    TraceInfoMode::Lines(ref lines) => {
                        siv.call_on_name("source_view", |sview: &mut views::SourceView| {
                            for (line, info) in lines {
                                let latency = if info.count != 0 {
                                    TraceState::Traced(get_latency(info))
                                } else {
                                    TraceState::Untraced
                                };
                                let frequency = TraceState::Traced(get_frequency(info));
                                Self::set_line_state(sview, *line, latency, frequency);
                            }
                        });
                    }
                    TraceInfoMode::Histogram(hist) => {
                        let function = &siv
                            .user_data::<Controller>()
                            .expect("Bug: Controller does not exist")
                            .trace_stack
                            .get_current_function();
                        siv.call_on_name("histogram_view", |hview: &mut views::TextDialogView| {
                            let hist_text = if !hist.is_empty() {
                                hist
                            } else {
                                "<Empty>".to_string()
                            };
                            hview.set_content(format!(
                                "Latency histogram in nanoseconds for {}:\n{}",
                                function, hist_text
                            ));
                        });
                    }
                    TraceInfoMode::Breakdown {
                        last_frame_trace,
                        breakdown_traces,
                    } => {
                        let trace_stack = &siv
                            .user_data::<Controller>()
                            .expect("Bug: Controller does not exist")
                            .trace_stack;
                        let last_function = trace_stack.get_current_function();
                        let format_latency = |t: &events::TraceCumulative| -> String {
                            if t.count != 0 {
                                views::formatting::format_latency(get_latency(t))
                            } else {
                                "N/A".to_string()
                            }
                        };
                        let format_frequency = |t: &events::TraceCumulative| -> String {
                            views::formatting::format_frequency(get_frequency(t))
                        };
                        let mut text = vec![
                            format!("Breakdown information for {}:", last_function),
                            format!(
                                "Latency: {}, Frequency: {}",
                                format_latency(&last_frame_trace),
                                format_frequency(&last_frame_trace)
                            ),
                        ];

                        let last_duration = last_frame_trace.duration;
                        trace_stack
                            .get_breakdown_functions()
                            .iter()
                            .zip(breakdown_traces.iter())
                            .for_each(|(function, trace)| {
                                text.push(format!("Function {}", function));
                                text.push(format!(
                                    "Latency: {}, Frequency: {}, Percentage: {:.1}",
                                    format_latency(&trace),
                                    format_frequency(&trace),
                                    (trace.duration.as_secs_f64() / last_duration.as_secs_f64())
                                        * (100 as f64)
                                ));
                            });
                        siv.call_on_name("breakdown_view", |bview: &mut views::TextDialogView| {
                            bview.set_content(text.join("\n"));
                        });
                    }
                }
                Ok(())
            }
            Event::TraceCommandModified => {
                siv.user_data::<Controller>()
                    .expect("Bug: Controller does not exist")
                    .tracer
                    .rerun_tracer();
                Ok(())
            }
            Event::SearchResults {
                counter,
                view_name,
                results,
            } => {
                if !siv
                    .user_data::<Controller>()
                    .unwrap()
                    .searcher
                    .is_counter_current(counter)
                {
                    return Ok(());
                }
                views::update_search_view(siv, &view_name, results);
                Ok(())
            }
            Event::SelectedFunction(_) => {
                panic!("Unexpected event");
            }
        };
        if result.is_ok() {
            // We may not _need_ to refresh in all cases, but doing this on in
            // one place makes things easier with minimal drawbacks.
            siv.refresh();
        }
        result
    }

    fn setup_function(
        program: &Program,
        function: FunctionName,
        sview: &mut views::SourceView,
        fview: &mut views::FooterView,
    ) -> Result<FrameInfo, Error> {
        let frame_info = Controller::create_frame_info(program, function)?;
        Controller::setup_source_view(&frame_info, sview, fview)?;
        Ok(frame_info)
    }

    fn setup_source_view(
        frame_info: &FrameInfo,
        sview: &mut views::SourceView,
        fview: &mut views::FooterView,
    ) -> Result<(), Error> {
        let source_code: Vec<String> = match std::fs::File::open(frame_info.get_source_file()) {
            Ok(file) => {
                // FIXME we can cache file contents
                std::io::BufReader::new(file)
                    .lines()
                    .map(|l| l.unwrap())
                    .collect()
            }
            Err(_) => {
                // TODO show error and confirm user wants to display empty lines
                // instead
                let max_line = frame_info.max_line();
                vec![String::new(); max_line as usize]
            }
        };
        views::set_source_view(
            sview,
            source_code,
            frame_info.get_source_line(),
            frame_info.called_lines(),
        );
        views::set_footer_view(fview, frame_info.get_source_file());
        Ok(())
    }

    fn create_frame_info(program: &Program, function: FunctionName) -> Result<FrameInfo, Error> {
        let location = program.get_location(program.get_address(function)).ok_or_else(|| format!("Failed to get source information corresponding to function {}, please ensure {} has appropriate debugging symbols", function, program.file_path))?;
        let source_file = location.file.unwrap();
        let source_line = location.line.unwrap();
        log::info!(
            "Function {} is at {}:{}",
            function,
            source_file,
            source_line
        );

        // TODO
        let (start_address, code) = program.get_data(function).unwrap();
        let decoder = program::create_decoder();

        let mut line_to_callsites = HashMap::<u32, Vec<CallInstruction>>::new();
        let mut unattached_callsites = Vec::<CallInstruction>::new();

        for (instruction, ip) in
            program::get_instructions_with_mnemonic(&decoder, start_address, code, Mnemonic::CALL)
        {
            let relative_ip = u32::try_from(ip - start_address).unwrap();
            assert!(instruction.operand_count > 0);
            let operand = &instruction.operands[0];
            let call_instruction = match operand.reg {
                Register::NONE => match operand.mem.base {
                    Register::NONE => {
                        let call_address = instruction
                            .calc_absolute_address(ip, &instruction.operands[0])
                            .unwrap();
                        match program.get_function_for_address(call_address) {
                            Some(function) => {
                                if program.is_dynamic_symbol_address(call_address) {
                                    CallInstruction::dynamic_symbol(
                                        relative_ip,
                                        instruction.length,
                                        function,
                                    )
                                } else {
                                    CallInstruction::function(
                                        relative_ip,
                                        instruction.length,
                                        function,
                                    )
                                }
                            }
                            None => CallInstruction::unknown(relative_ip, instruction.length),
                        }
                    }
                    r => CallInstruction::register(
                        relative_ip,
                        instruction.length,
                        r.get_string().unwrap().to_string(),
                        Some(operand.mem.disp.displacement),
                    ),
                },
                r => {
                    // TODO convert register string to bpftrace register
                    CallInstruction::register(
                        relative_ip,
                        instruction.length,
                        r.get_string().unwrap().to_string(),
                        None,
                    )
                }
            };
            let location = program.get_location(ip).unwrap();
            if location.file.unwrap() == source_file {
                line_to_callsites
                    .entry(location.line.unwrap())
                    .or_default()
                    .push(call_instruction);
            } else {
                // This is an inlined call. We don't know which line it
                // corresponds to in the source file we are displaying.
                log::trace!(
                    "Not displaying function call {} from {}:{} because it is not in current source file {}",
                    call_instruction,
                    location.file.unwrap(),
                    location.line.unwrap(),
                    source_file
                );
                unattached_callsites.push(call_instruction);
            }
        }

        log::trace!("{:?}", line_to_callsites);
        let frame_info = FrameInfo::new(
            function,
            String::from(source_file),
            source_line,
            line_to_callsites,
            unattached_callsites,
        );

        Ok(frame_info)
    }

    fn set_line_state(
        sview: &mut views::SourceView,
        line: u32,
        latency: TraceState<std::time::Duration>,
        frequency: TraceState<f32>,
    ) {
        let item = sview.borrow_items_mut().get_mut(line as usize - 1).unwrap();
        item.latency = latency;
        item.frequency = frequency;
    }

    /// Request user to input a filter. If it fails validation, the user is
    /// requested to correct the filter repeatedly until it passes or user
    /// cancels.
    fn setup_user_filter(siv: &mut Cursive, initial_filter: Option<String>, is_ret_filter: bool) {
        let trace_stack = &siv
            .user_data::<Controller>()
            .expect("Bug: Controller does not exist")
            .trace_stack;
        let function = trace_stack.get_current_function();
        let title = if is_ret_filter {
            format!(
                "Enter bpftrace filter to apply on exit of {} [empty to clear]",
                function
            )
        } else {
            format!(
                "Enter bpftrace filter to apply on entry of {} [empty to clear]",
                function
            )
        };
        siv.add_layer(views::new_edit_view(
            &title,
            "filter_view",
            initial_filter.as_deref(),
            move |siv, filter| {
                siv.pop_layer();
                if let Err(message) = siv
                    .user_data::<Controller>()
                    .expect("Bug: Controller does not exist")
                    .trace_stack
                    .set_current_filter(filter.to_string(), is_ret_filter)
                {
                    let message = format!("Invalid filter:\n{}", message);
                    let filter = filter.to_string();
                    siv.add_layer(Dialog::text(message).button("OK", move |siv| {
                        siv.pop_layer();
                        // Ask user to edit filter again
                        Controller::setup_user_filter(siv, Some(filter.clone()), is_ret_filter);
                    }));
                }
            },
        ));
    }

    fn add_callbacks(siv: &mut Cursive) {
        siv.add_global_callback(cursive::event::Event::CtrlChar('t'), |siv| {
            siv.user_data::<Controller>()
                .expect("Bug: Controller does not exist")
                .key_handler
                .advanced_mode_key_pressed();
        });

        KeyHandler::add_global_callbacks(
            siv,
            'x',
            |siv| {
                // TODO do not show duplicate view if key pressed multiple
                // times, for all of the callbacks.
                //
                // Normal trace
                let mut sview = siv
                    .find_name::<views::SourceView>("source_view")
                    .expect("Bug: source_view does not exist");
                let line = sview.row().unwrap() as u32 + 1;
                let trace_stack = &siv
                    .user_data::<Controller>()
                    .expect("Bug: Controller does not exist")
                    .trace_stack;
                // We want to toggle tracing at this line - try to remove if it
                // exists, otherwise proceed to add callsite.
                if trace_stack.remove_callsite(line) {
                    Self::set_line_state(
                        &mut *sview,
                        line,
                        TraceState::Untraced,
                        TraceState::Untraced,
                    );
                    return;
                }

                let callsites = trace_stack.get_callsites(line);
                if callsites.is_empty() {
                    let function = trace_stack.get_current_function();
                    siv.add_layer(views::new_dialog(&format!(
                        "No calls found in {} on line {}. Note the call may have been inlined.",
                        function, line
                    )));
                    return;
                }
                if callsites.len() > 1 {
                    let search_view = views::new_simple_search_view(
                        "Select the call to trace",
                        callsites,
                        move |siv: &mut Cursive, ci: &CallInstruction| {
                            let mut sview = siv
                                .find_name::<views::SourceView>("source_view")
                                .expect("Bug: source_view does not exist");
                            Self::set_line_state(
                                &mut *sview,
                                line,
                                TraceState::Pending,
                                TraceState::Pending,
                            );
                            let controller = siv
                                .user_data::<Controller>()
                                .expect("Bug: Controller does not exist");
                            controller.trace_stack.add_callsite(line, ci.clone());
                        },
                    );
                    siv.add_layer(search_view);
                } else {
                    Self::set_line_state(
                        &mut *sview,
                        line,
                        TraceState::Pending,
                        TraceState::Pending,
                    );
                    trace_stack.add_callsite(line, callsites.into_iter().nth(0).unwrap());
                }
            },
            |siv| {
                // Advanced mode - allow specifying exact addresses to trace
                let mut sview = siv
                    .find_name::<views::SourceView>("source_view")
                    .expect("Bug: source_view does not exist");
                let line = sview.row().unwrap() as u32 + 1;
                let trace_stack = &siv
                    .user_data::<Controller>()
                    .expect("Bug: Controller does not exist")
                    .trace_stack;
                // We want to toggle tracing at this line - try to remove if it
                // exists, otherwise proceed to add callsite.
                if trace_stack.remove_callsite(line) {
                    Self::set_line_state(
                        &mut *sview,
                        line,
                        TraceState::Untraced,
                        TraceState::Untraced,
                    );
                    return;
                }

                siv.add_layer(views::new_edit_view(
                    "Enter trace start offset, relative to start of the current function, in bytes",
                    "start_trace_view",
                    None,
                    move |siv, start_offset| {
                        siv.pop_layer();
                        // Clone for lifetime purposes
                        let start_offset = start_offset.to_string();
                        siv.add_layer(views::new_edit_view(
                            "Enter trace end offset, relative to start of the current function, in bytes",
                            "end_trace_view",
                            None,
                            move |siv, end_offset| {
                                siv.pop_layer();
                                let start_ip = unwrap::unwrap!(start_offset.parse::<u32>(), "Could not parse {} as number", start_offset);
                                let end_ip = unwrap::unwrap!(end_offset.parse::<u32>(), "Could not parse {} as number", end_offset);
                                assert!(end_ip > start_ip);
                                let ci = CallInstruction::manual(start_ip, end_ip - start_ip);
                                let mut sview = siv.find_name::<views::SourceView>("source_view").expect("Bug: source_view does not exist");
                                Self::set_line_state(
                                    &mut *sview,
                                    line,
                                    TraceState::Pending,
                                    TraceState::Pending,
                                );
                                let trace_stack = &siv.user_data::<Controller>().expect("Bug: Controller does not exist").trace_stack;
                                trace_stack.add_callsite(line, ci);
                            },
                        ));
                    },
                ));
            },
        );

        KeyHandler::add_global_callback(siv, 'X', |siv| {
            let mut sview = siv
                .find_name::<views::SourceView>("source_view")
                .expect("Bug: source_view does not exist");
            let trace_stack = &siv
                .user_data::<Controller>()
                .expect("Bug: Controller does not exist")
                .trace_stack;
            let line = sview.row().unwrap() as u32 + 1;
            if trace_stack.remove_callsite(line) {
                Self::set_line_state(
                    &mut *sview,
                    line,
                    TraceState::Untraced,
                    TraceState::Untraced,
                );
                return;
            }

            let callsites = trace_stack.get_unattached_callsites();
            if callsites.is_empty() {
                let function = trace_stack.get_current_function();
                siv.add_layer(views::new_dialog(&format!(
                    "No unattached calls found in {}",
                    function
                )));
                return;
            }
            let search_view = views::new_simple_search_view(
                "Select the call to trace",
                callsites,
                move |siv: &mut Cursive, ci: &CallInstruction| {
                    let mut sview = siv
                        .find_name::<views::SourceView>("source_view")
                        .expect("Bug: source_view does not exist");
                    Self::set_line_state(
                        &mut *sview,
                        line,
                        TraceState::Pending,
                        TraceState::Pending,
                    );
                    let controller = siv
                        .user_data::<Controller>()
                        .expect("Bug: Controller does not exist");
                    controller.trace_stack.add_callsite(line, ci.clone());
                },
            );
            siv.add_layer(search_view);
        });

        KeyHandler::add_global_callback(siv, '>', |siv| {
            let controller = siv
                .user_data::<Controller>()
                .expect("Bug: Controller does not exist");
            let initial_results = vec![("Type to search".to_string(), None)];
            controller
                .searcher
                .setup_search(initial_results.clone(), Vec::new());
            let search_view = views::new_search_view(
                "Select the function to enter",
                initial_results,
                move |siv: &mut Cursive, view_name: &str, search: &str, n_results: usize| {
                    let controller = siv
                        .user_data::<Controller>()
                        .expect("Bug: Controller does not exist");
                    controller.searcher.search(view_name, search, n_results);
                },
                move |siv: &mut Cursive, symbol: &SymbolInfo| {
                    let controller = siv
                        .user_data::<Controller>()
                        .expect("Bug: Controller does not exist");
                    // TODO cancel any pending searches
                    if controller.program.is_dynamic_symbol(symbol) {
                        // TODO show error for dyn fn
                    } else {
                        let mut sview = siv
                            .find_name::<views::SourceView>("source_view")
                            .expect("Bug: source_view does not exist");
                        let mut fview = siv
                            .find_name::<views::FooterView>("footer_view")
                            .expect("Bug: footer_view does not exist");
                        // Reset lifetime of `controller` to avoid overlapping
                        // mutable borrows of `siv`.
                        let controller = siv
                            .user_data::<Controller>()
                            .expect("Bug: Controller does not exist");
                        match Controller::setup_function(
                            &controller.program,
                            symbol.name,
                            &mut *sview,
                            &mut *fview,
                        ) {
                            Err(e) => siv.add_layer(views::new_dialog(&format!(
                                "Error setting up function {}: {}",
                                symbol.name, e
                            ))),
                            Ok(frame_info) => {
                                controller.trace_stack.push(frame_info);
                            }
                        };
                    }
                },
            );
            siv.add_layer(search_view);
        });

        KeyHandler::add_global_callback(siv, 'r', |siv| {
            siv.user_data::<Controller>()
                .expect("Bug: Controller does not exist")
                .tracer
                .rerun_tracer();
        });

        KeyHandler::add_global_callback(
            siv,
            cursive::event::Event::Key(cursive::event::Key::Enter),
            |siv| {
                let line = siv
                    .find_name::<views::SourceView>("source_view")
                    .expect("Bug: source_view does not exist")
                    .row()
                    .unwrap() as u32
                    + 1;
                let controller = siv
                    .user_data::<Controller>()
                    .expect("Bug: Controller does not exist");
                let trace_stack = &controller.trace_stack;
                let callsites = trace_stack.get_callsites(line);
                if callsites.is_empty() {
                    let function = trace_stack.get_current_function();
                    siv.add_layer(views::new_dialog(&format!(
                        "No calls found in {} on line {}. Note the call may have been inlined.",
                        function, line
                    )));
                    return;
                }

                let num_callsites = callsites.len();
                let direct_calls: Vec<SymbolInfo> = callsites
                    .into_iter()
                    .filter_map(|ci| match ci.instruction {
                        InstructionType::Unknown => None,
                        InstructionType::Manual => None,
                        InstructionType::Register(_, _) => None,
                        InstructionType::DynamicSymbol(function) => {
                            controller.program.get_symbol(function).or_else(|| {
                                log::warn!("Could not get symbol information for {}", function);
                                None
                            })
                        }
                        InstructionType::Function(function) => {
                            controller.program.get_symbol(function).or_else(|| {
                                log::warn!("Could not get symbol information for {}", function);
                                None
                            })
                        }
                    })
                    .map(|si| si.clone())
                    .collect();
                let num_indirect_calls = num_callsites - direct_calls.len();

                let submit_fn = move |siv: &mut Cursive, symbol: &SymbolInfo| {
                    let controller = siv
                        .user_data::<Controller>()
                        .expect("Bug: Controller does not exist");
                    // TODO cancel any pending searches
                    if controller.program.is_dynamic_symbol(symbol) {
                        // TODO show error for dyn fn
                    } else {
                        let mut sview = siv
                            .find_name::<views::SourceView>("source_view")
                            .expect("Bug: source_view does not exist");
                        let mut fview = siv
                            .find_name::<views::FooterView>("footer_view")
                            .expect("Bug: footer_view does not exist");
                        // Reset lifetime of `controller` to avoid overlapping
                        // mutable borrows of `siv`.
                        let controller = siv
                            .user_data::<Controller>()
                            .expect("Bug: Controller does not exist");
                        match Controller::setup_function(
                            &controller.program,
                            symbol.name,
                            &mut *sview,
                            &mut *fview,
                        ) {
                            Err(e) => siv.add_layer(views::new_dialog(&format!(
                                "Error setting up function {}: {}",
                                symbol.name, e
                            ))),
                            Ok(frame_info) => {
                                controller.trace_stack.push(frame_info);
                            }
                        };
                    }
                    // TODO show error for dyn fn
                };

                if num_callsites > 1 || num_indirect_calls > 0 {
                    let title = "Select the call to enter";
                    let search_view = if num_indirect_calls == 0 {
                        views::new_simple_search_view(title, direct_calls, submit_fn)
                    } else {
                        let mut initial_results =
                            search::rank_fn(direct_calls.iter(), "", usize::MAX);
                        let call_string = if num_indirect_calls == 1 {
                            "1 indirect call".to_string()
                        } else {
                            format!("{} indirect calls", num_indirect_calls)
                        };
                        initial_results
                            .insert(0, (format!("{} (type to search)", call_string), None));
                        controller
                            .searcher
                            .setup_search(initial_results.clone(), direct_calls);
                        views::new_search_view(
                            title,
                            initial_results,
                            move |siv: &mut Cursive,
                                  view_name: &str,
                                  search: &str,
                                  n_results: usize| {
                                let controller = siv
                                    .user_data::<Controller>()
                                    .expect("Bug: Controller does not exist");
                                controller.searcher.search(view_name, search, n_results);
                            },
                            submit_fn,
                        )
                    };
                    siv.add_layer(search_view);
                } else {
                    submit_fn(siv, &direct_calls[0]);
                }
            },
        );

        KeyHandler::add_global_callback(
            siv,
            cursive::event::Event::Key(cursive::event::Key::Esc),
            |siv| {
                if siv.screen().len() > 1 {
                    // Pop anything on top of source view
                    let view = siv
                        .pop_layer()
                        .expect("Pop unexpectedly empty despite len > 1");

                    // Check if this is histogram or breakdown view - we need to
                    // reset mode if so.
                    if views::is_text_dialog_view(&view, "histogram_view")
                        || views::is_text_dialog_view(&view, "breakdown_view")
                    {
                        siv.user_data::<Controller>()
                            .expect("Bug: Controller does not exist")
                            .trace_stack
                            .set_mode(TraceMode::Line);
                    }

                    return;
                }
                let controller = siv
                    .user_data::<Controller>()
                    .expect("Bug: Controller does not exist");
                match controller.trace_stack.pop() {
                    Some(frame_info) => {
                        let mut sview = siv
                            .find_name::<views::SourceView>("source_view")
                            .expect("Bug: source_view does not exist");
                        let mut fview = siv
                            .find_name::<views::FooterView>("footer_view")
                            .expect("Bug: footer_view does not exist");
                        Controller::setup_source_view(&frame_info, &mut *sview, &mut *fview)
                            .unwrap();
                    }
                    None => siv.add_layer(views::new_quit_dialog("Are you sure you want to quit?")),
                }
            },
        );

        KeyHandler::add_global_callback(siv, 'h', |siv| {
            if let Some(_) = siv.find_name::<views::TextDialogView>("histogram_view") {
                // View is already open, make it no-op
                return;
            }

            let trace_stack = &siv
                .user_data::<Controller>()
                .expect("Bug: Controller does not exist")
                .trace_stack;
            trace_stack.set_mode(TraceMode::Histogram);
            let function = trace_stack.get_current_function();
            siv.add_layer(views::new_text_dialog_view(
                &format!("Gathering latency histogram for {}...", function),
                "histogram_view",
                |siv| {
                    let trace_stack = &siv
                        .user_data::<Controller>()
                        .expect("Bug: Controller does not exist")
                        .trace_stack;
                    trace_stack.set_mode(TraceMode::Line);
                    siv.pop_layer();
                },
            ));
        });

        KeyHandler::add_global_callback(siv, 'f', |siv| {
            if let Some(_) = siv.find_name::<cursive::views::EditView>("filter_view") {
                // View is already open, make it no-op
                return;
            }

            let initial_filter = siv
                .user_data::<Controller>()
                .expect("Bug: Controller does not exist")
                .trace_stack
                .get_current_filter(false);
            Controller::setup_user_filter(siv, initial_filter, false);
        });
        KeyHandler::add_global_callback(siv, 'g', |siv| {
            if let Some(_) = siv.find_name::<cursive::views::EditView>("filter_view") {
                // View is already open, make it no-op
                return;
            }

            let initial_filter = siv
                .user_data::<Controller>()
                .expect("Bug: Controller does not exist")
                .trace_stack
                .get_current_filter(true);
            Controller::setup_user_filter(siv, initial_filter, true);
        });

        KeyHandler::add_global_callback(siv, 'b', |siv| {
            let controller = siv
                .user_data::<Controller>()
                .expect("Bug: Controller does not exist");
            let initial_results = vec![("Type to search".to_string(), None)];
            controller
                .searcher
                .setup_search(initial_results.clone(), Vec::new());
            let search_view = views::new_search_view(
                "Select the functions to trace",
                initial_results,
                move |siv: &mut Cursive, view_name: &str, search: &str, n_results: usize| {
                    let controller = siv
                        .user_data::<Controller>()
                        .expect("Bug: Controller does not exist");
                    controller.searcher.search(view_name, search, n_results);
                },
                move |siv: &mut Cursive, symbol: &SymbolInfo| {
                    let controller = siv
                        .user_data::<Controller>()
                        .expect("Bug: Controller does not exist");
                    // TODO cancel any pending searches
                    if controller.program.is_dynamic_symbol(symbol) {
                        // TODO show error for dyn fn
                    } else {
                        // TODO need way better layout, way to exit, remove fns etc
                        if symbol.name.0 == "main" {
                            controller.trace_stack.set_mode(TraceMode::Breakdown);
                            let current_function = controller.trace_stack.get_current_function();
                            siv.add_layer(views::new_text_dialog_view(
                                &format!("Gathering latency breakdown for {}...", current_function),
                                "breakdown_view",
                                |siv| {
                                    let trace_stack = &siv
                                        .user_data::<Controller>()
                                        .expect("Bug: Controller does not exist")
                                        .trace_stack;
                                    trace_stack.set_mode(TraceMode::Line);
                                    siv.pop_layer();
                                },
                            ));
                        } else {
                            controller.trace_stack.add_breakdown_function(symbol.name);
                        }
                    }
                },
            );
            siv.add_layer(search_view);
        });

        KeyHandler::add_global_callback(siv, 'm', |siv| {
            let controller = siv
                .user_data::<Controller>()
                .expect("Bug: Controller does not exist");
            let initial_results = vec![("Type to search".to_string(), None)];
            controller
                .searcher
                .setup_search(initial_results.clone(), Vec::new());
            let search_view = views::new_search_view(
                "Select a function to get its mangled name",
                initial_results,
                move |siv: &mut Cursive, view_name: &str, search: &str, n_results: usize| {
                    let controller = siv
                        .user_data::<Controller>()
                        .expect("Bug: Controller does not exist");
                    controller.searcher.search(view_name, search, n_results);
                },
                move |siv: &mut Cursive, symbol: &SymbolInfo| {
                    // TODO cancel any pending searches
                    siv.add_layer(views::new_dialog(&format!(
                        "Mangled version of {} is:\n{:?}",
                        symbol.name, symbol.name
                    )));
                },
            );
            siv.add_layer(search_view);
        });
    }
}

pub struct KeyHandler {
    advanced_mode_enable_time: Option<Instant>,
}

impl KeyHandler {
    const ADVANCED_MODE_DURATION_MS: u128 = 1000;

    pub fn new() -> KeyHandler {
        KeyHandler {
            advanced_mode_enable_time: None,
        }
    }

    pub fn advanced_mode_key_pressed(&mut self) {
        self.advanced_mode_enable_time = Some(Instant::now());
    }

    /// We support 2 callbacks for any key: one is the normal one, and the
    /// second is with "advanced mode". Advanced mode is enabled by pressing
    /// `Ctrl-t` and then the key.
    pub fn add_global_callbacks<E, F1, F2>(
        siv: &mut Cursive,
        event: E,
        mut normal_cb: F1,
        mut advanced_cb: F2,
    ) where
        E: Into<cursive::event::Event>,
        F1: FnMut(&mut Cursive) + 'static,
        F2: FnMut(&mut Cursive) + 'static,
    {
        siv.add_global_callback(event, move |siv| {
            let key_handler = &siv
                .user_data::<Controller>()
                .expect("Bug: Controller does not exist")
                .key_handler;
            if key_handler.advanced_mode_enable_time.map_or(false, |i| {
                Instant::now().duration_since(i).as_millis() < KeyHandler::ADVANCED_MODE_DURATION_MS
            }) {
                advanced_cb(siv);
            } else {
                normal_cb(siv);
            }
        });
    }

    /// Add a single callback (no advanced mode) for a key.
    pub fn add_global_callback<E, F1>(siv: &mut Cursive, event: E, mut normal_cb: F1)
    where
        E: Into<cursive::event::Event>,
        F1: FnMut(&mut Cursive) + 'static,
    {
        siv.add_global_callback(event, move |siv| {
            let key_handler = &mut siv
                .user_data::<Controller>()
                .expect("Bug: Controller does not exist")
                .key_handler;
            key_handler.advanced_mode_enable_time = None;
            normal_cb(siv);
        });
    }
}

impl search::Label for CallInstruction {
    fn label(&self) -> Cow<str> {
        Cow::Owned(self.to_string())
    }
}
impl search::Label for program::SymbolInfo {
    fn label(&self) -> Cow<str> {
        Cow::Borrowed(self.as_ref())
    }
}
