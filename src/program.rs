use crate::error::Error;
use addr2line::Location;
use object::Object;
use object::ObjectSection;
use object::ObjectSymbol;
use object::ObjectSymbolTable;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use zydis::ffi::Decoder;
use zydis::formatter::{Formatter, OutputBuffer};
use zydis::{
    enums::generated::{AddressWidth, FormatterStyle, MachineMode, Mnemonic},
    DecodedInstruction,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
/// Name corresponding to a function symbol that exists in the program
pub struct FunctionName(pub &'static str);

impl fmt::Display for FunctionName {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self.0, f)
    }
}

impl FunctionName {
    pub fn pretty_print(&self) -> String {
        cplus_demangle::demangle(self.0).unwrap_or(String::from(self.0))
    }
}

pub struct Program {
    /// Only used when printing error messages
    pub file_path: String,
    file: object::read::File<'static>,
    name_to_symbol: HashMap<FunctionName, SymbolInfo>,
    address_to_name: HashMap<u64, FunctionName>,
    context: addr2line::Context<gimli::EndianArcSlice<gimli::RunTimeEndian>>,
    // (start_address, size) of runtime addresses for dynamic symbols (functions
    // loaded from shared libraries)
    dynamic_symbols_range: std::ops::Range<u64>,
    dynamic_symbols_map: HashMap<u64, FunctionName>,
}

#[derive(Debug)]
struct SymbolInfo {
    name: FunctionName,
    demangled_name: Option<String>,
    section_index: Option<object::SectionIndex>,
    address: u64,
    size: u64,
}

impl SymbolInfo {
    fn display_name(&self) -> &str {
        match &self.demangled_name {
            Some(dn) => &dn,
            None => self.name.0,
        }
    }
}

impl Program {
    pub fn new(file_path: String) -> Result<Self, Error> {
        let file = match std::fs::File::open(&file_path) {
            Ok(file) => file,
            Err(err) => return Err(format!("Failed to open file {}: {}", file_path, err).into()),
        };
        let mmap = match unsafe { memmap::Mmap::map(&file) } {
            Ok(mmap) => mmap,
            Err(err) => return Err(format!("Failed to mmap file {}: {}", file_path, err).into()),
        };
        // Yeah yeah this is a terrible thing to do. I couldn't find any way to
        // propagate appropriate lifetimes into cursive (if you know a way let
        // me know), so it's either making this mmap static or some other
        // struct, and doing it here simplifies LOTS of annotations.
        let mmap = Box::leak(Box::new(mmap));

        let file = match object::File::parse(&*mmap) {
            Ok(file) => file,
            Err(err) => return Err(format!("Failed to parse file {}: {}", file_path, err).into()),
        };

        // TODO fixup unwraps
        let dynamic_symbols_range = file
            .sections()
            .filter(|s| s.name().unwrap() == ".plt")
            .map(|s| std::ops::Range {
                start: s.address(),
                end: s.address() + s.size(),
            })
            .next()
            .unwrap();

        let function_names: Vec<SymbolInfo> = file
            .symbols()
            .filter(|symbol| symbol.kind() == object::SymbolKind::Text) // Filter to functions
            .map(|symbol| {
                symbol.name().map(|name| {
                    let demangled_name = cplus_demangle::demangle(name).ok();
                    SymbolInfo {
                        name: FunctionName(name),
                        demangled_name,
                        section_index: symbol.section_index(),
                        address: symbol.address(),
                        size: symbol.size(),
                    }
                })
            })
            .flat_map(|x| {
                log::trace!("{:?}", x);
                x
            })
            .collect();

        let dynamic_symbols_map = Program::dynamic_symbols_map(&file);

        // Note: reversing this map can create collisions.
        // https://stackoverflow.com/questions/49824915/ambiguity-of-de-mangled-c-symbols
        let name_to_symbol: HashMap<_, _> =
            function_names.into_iter().map(|si| (si.name, si)).collect();

        let address_to_name: HashMap<_, _> = name_to_symbol
            .iter()
            .filter(|(_, s)| s.address != 0)
            .map(|(n, s)| (s.address, n.clone()))
            .collect();

        let context = new_context(&file).unwrap();

        Ok(Program {
            file_path,
            file,
            name_to_symbol,
            address_to_name,
            context,
            dynamic_symbols_range,
            dynamic_symbols_map,
        })
    }

    fn dynamic_symbols_map(file: &object::read::File<'static>) -> HashMap<u64, FunctionName> {
        let mut relocations = HashMap::new();
        let dynamic_symbols = file.dynamic_symbol_table().unwrap();
        let reloc_iter = file.dynamic_relocations().unwrap();
        for (address, relocation) in reloc_iter {
            if let object::RelocationTarget::Symbol(index) = relocation.target() {
                let symbol = dynamic_symbols.symbol_by_index(index).unwrap();
                if symbol.kind() == object::SymbolKind::Text {
                    if let Ok(name) = symbol.name() {
                        log::trace!("Relocation {:x} = {}", address, name);
                        relocations.insert(address, name);
                    }
                }
            }
        }

        let mut map = HashMap::new();
        let decoder = create_decoder();
        for section in file.sections() {
            if let (Ok(name), address) = (section.name(), section.address()) {
                // Include .plt and .plt.got
                if name.starts_with(".plt") {
                    let code = section.uncompressed_data().unwrap();
                    for (instruction, ip) in
                        get_instructions_with_mnemonic(&decoder, address, &code, Mnemonic::JMP)
                    {
                        assert!(instruction.operand_count > 0);
                        let jump_address = instruction
                            .calc_absolute_address(ip, &instruction.operands[0])
                            .unwrap();
                        log::trace!("PLT {:#x?} -> GOT {:#x?}", ip, jump_address);
                        // Ignore expected jumps to PLT0 - figure A-9 in
                        // https://refspecs.linuxfoundation.org/elf/elf.pdf
                        if let Some(name) = relocations.get(&jump_address) {
                            map.insert(ip, FunctionName(*name));
                        }
                    }
                }
            }
        }
        log::trace!("{:?}", map);
        map
    }

    pub fn get_matches(&self, function_name: &str) -> Vec<FunctionName> {
        let mut matches = Vec::new();
        for (name, symbol) in &self.name_to_symbol {
            let display_name = symbol.display_name();
            if display_name == function_name {
                return vec![*name];
            }
            if display_name.contains(function_name) {
                matches.push(*name);
            }
        }
        matches
    }

    pub fn get_address(&self, function: FunctionName) -> u64 {
        self.name_to_symbol.get(&function).unwrap().address
    }

    /// If something is returned, it is guaranteed to have file and line number
    /// set.
    pub fn get_location(&self, address: u64) -> Option<Location> {
        match self.context.find_location(address) {
            Ok(l) => match l {
                Some(l) => {
                    l.file?;
                    l.line?;
                    Some(l)
                }
                None => None,
            },
            Err(_) => None,
        }
    }

    // Returns (address, data) for given function
    pub fn get_data(&self, function: FunctionName) -> Result<(u64, &[u8]), Error> {
        let symbol = &self.name_to_symbol.get(&function).unwrap();
        let address = symbol.address;
        if address == 0 {
            return Err(
                format!("Cannot get data for dynamically linked symbol {}", function).into(),
            );
        }
        let size = symbol.size;
        let index = symbol.section_index.unwrap();
        Ok((
            address,
            self.file
                .section_by_index(index)
                .unwrap()
                .data_range(address, size)
                .unwrap()
                .unwrap(),
        ))
    }

    pub fn get_function_for_address(&self, address: u64) -> Option<FunctionName> {
        if self.is_dynamic_symbol(address) {
            self.dynamic_symbols_map.get(&address).map(|f| f.clone())
        } else {
            self.address_to_name.get(&address).map(|f| f.clone())
        }
    }

    pub fn is_dynamic_symbol(&self, address: u64) -> bool {
        return self.dynamic_symbols_range.contains(&address);
    }
}

pub fn create_decoder() -> Decoder {
    // TODO make platform independent
    Decoder::new(MachineMode::LONG_64, AddressWidth::_64).unwrap()
}

pub fn get_instructions_with_mnemonic<'a, 'b>(
    decoder: &'a Decoder,
    start_address: u64,
    code: &'b [u8],
    mnemonic: Mnemonic,
) -> CallIterator<'a, 'b> {
    CallIterator {
        it: decoder.instruction_iterator(code, start_address),
        mnemonic,
    }
}

pub struct CallIterator<'a, 'b> {
    it: zydis::InstructionIterator<'a, 'b>,
    mnemonic: Mnemonic,
}

impl Iterator for CallIterator<'_, '_> {
    type Item = (DecodedInstruction, u64);

    fn next(&mut self) -> Option<(DecodedInstruction, u64)> {
        while let Some((instruction, ip)) = self.it.next() {
            if instruction.mnemonic == self.mnemonic {
                if log::log_enabled!(log::Level::Trace) {
                    let formatter = Formatter::new(FormatterStyle::INTEL).unwrap();
                    let mut buffer = [0u8; 200];
                    let mut buffer = OutputBuffer::new(&mut buffer[..]);
                    formatter
                        .format_instruction(&instruction, &mut buffer, Some(ip), None)
                        .unwrap();
                    log::trace!("{} 0x{:016X} {}", instruction.operand_count, ip, buffer);
                }

                return Some((instruction, ip));
            }
        }
        None
    }
}

/// Clone of addr2line::ObjectContext::new, just using Arc instead of Rc.
///
/// Construct a new `Context`.
///
/// The resulting `Context` uses `gimli::EndianRcSlice<gimli::RunTimeEndian>`.
/// This means it is not thread safe, has no lifetime constraints (since it copies
/// the input data), and works for any endianity.
///
/// Performance sensitive applications may want to use `Context::from_sections`
/// with a more specialised `gimli::Reader` implementation.
pub fn new_context<'data: 'file, 'file, O: object::Object<'data, 'file>>(
    file: &'file O,
) -> Result<addr2line::Context<gimli::EndianArcSlice<gimli::RunTimeEndian>>, gimli::Error> {
    let endian = if file.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };

    fn load_section<'data: 'file, 'file, O, S, Endian>(file: &'file O, endian: Endian) -> S
    where
        O: object::Object<'data, 'file>,
        S: gimli::Section<gimli::EndianArcSlice<Endian>>,
        Endian: gimli::Endianity,
    {
        let data = file
            .section_by_name(S::section_name())
            .and_then(|section| section.uncompressed_data().ok())
            .unwrap_or(Cow::Borrowed(&[]));
        S::from(gimli::EndianArcSlice::new(Arc::from(&*data), endian))
    }

    let debug_abbrev: gimli::DebugAbbrev<_> = load_section(file, endian);
    let debug_addr: gimli::DebugAddr<_> = load_section(file, endian);
    let debug_info: gimli::DebugInfo<_> = load_section(file, endian);
    let debug_line: gimli::DebugLine<_> = load_section(file, endian);
    let debug_line_str: gimli::DebugLineStr<_> = load_section(file, endian);
    let debug_ranges: gimli::DebugRanges<_> = load_section(file, endian);
    let debug_rnglists: gimli::DebugRngLists<_> = load_section(file, endian);
    let debug_str: gimli::DebugStr<_> = load_section(file, endian);
    let debug_str_offsets: gimli::DebugStrOffsets<_> = load_section(file, endian);
    let default_section = gimli::EndianArcSlice::new(Arc::from(&[][..]), endian);

    addr2line::Context::from_sections(
        debug_abbrev,
        debug_addr,
        debug_info,
        debug_line,
        debug_line_str,
        debug_ranges,
        debug_rnglists,
        debug_str,
        debug_str_offsets,
        default_section,
    )
}
