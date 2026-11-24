//! Names the faulting code when title code crashes.
//!
//! A vectored handler sees every exception first, so it only prints the
//! kinds that end a process (access violations and the like, and
//! breakpoints: titles assert with `int3`, fatal without a debugger) as
//! first-chance, since a handler may still take them (ntdll's loader does
//! with its own probes). What nobody handles, of any kind (an uncaught C++
//! exception too), is reported from the unhandled-exception filter
//! (`report_unhandled`, which the runtime calls ahead of the title's own)
//! and appended to the crash log, with the title's last debug output, which
//! often states the reason. Panics of ours go to the crash log too.

use std::ptr::null_mut;
use std::sync::atomic::{AtomicU64, Ordering};
use windows_sys::Win32::Foundation::{
    EXCEPTION_ACCESS_VIOLATION, EXCEPTION_BREAKPOINT, EXCEPTION_ILLEGAL_INSTRUCTION,
    EXCEPTION_INT_DIVIDE_BY_ZERO, EXCEPTION_PRIV_INSTRUCTION, EXCEPTION_STACK_OVERFLOW, HMODULE,
};

use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, EXCEPTION_POINTERS, M128A, RtlLookupFunctionEntry,
    RtlVirtualUnwind, SetUnhandledExceptionFilter, UNW_FLAG_NHANDLER,
};
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadStackLimits;

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
/// Frames listed per report.
pub(crate) const MAX_FRAMES: usize = 40;

/// Memory saved to files when a crash is reported: (address, length, file).
#[cfg(feature = "diagnostics")]
static SAVE: std::sync::OnceLock<Vec<(usize, usize, std::path::PathBuf)>> =
    std::sync::OnceLock::new();

/// Debugging aid: on a reported crash, also write each `[address, address
/// + len)` (readable title memory, e.g. an engine table) to its file.
#[cfg(feature = "diagnostics")]
pub fn save_on_crash(ranges: Vec<(usize, usize, std::path::PathBuf)>) {
    let _ = SAVE.set(ranges);
}

/// The file crash reports are appended to, besides being printed.
static LOG: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Also appends every unhandled-exception report and panic to `file`, so a
/// crash can be read after the console is gone.
pub fn log_crashes_to(file: std::path::PathBuf) {
    if LOG.set(file).is_ok() {
        let print = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            print(info);
            append_to_log(&format!(
                "VIctory: panic on thread {:?}: {info}\n{}",
                std::thread::current().name().unwrap_or("?"),
                title_output()
            ));
        }));
    }
}

/// Appends a failure that is not an exception (the host GPU's) to the crash
/// log, with the title's last debug output, and prints it.
pub fn log_crash(text: &str) {
    let text = format!("{text}
{}", title_output());
    eprint!("{text}");
    append_to_log(&text);
}

fn append_to_log(text: &str) {
    use std::io::Write;
    let Some(file) = LOG.get() else { return };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let opened = std::fs::OpenOptions::new().create(true).append(true).open(file);
    if let Ok(mut f) = opened {
        let _ = write!(f, "unix time {now}, process {}\n{text}\n", std::process::id());
    }
}

/// The title's last lines of debug output, kept for crash reports.
static SAID: std::sync::Mutex<std::collections::VecDeque<String>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());
const SAID_LINES: usize = 8;

/// Keeps a line of the title's debug output for the next crash report.
pub fn note_title_output(line: &str) {
    if line.is_empty() {
        return;
    }
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.len() == SAID_LINES {
        said.pop_front();
    }
    said.push_back(line.to_owned());
}

/// The kept lines, one per report line; nothing if another thread holds
/// them (it may be the one that faulted).
fn title_output() -> String {
    let Ok(said) = SAID.try_lock() else {
        return String::new();
    };
    said.iter().map(|l| format!("  title said: {l}\n")).collect()
}

/// Installs the handlers; call once before running title code.
pub fn report_crashes() {
    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        // SAFETY: the system passes the exception being dispatched.
        unsafe { report_unhandled(info) };
        EXCEPTION_CONTINUE_SEARCH
    }
    // SAFETY: registers handlers that only read the exception they are
    // given; the title's own filter, once it sets one, takes the place of
    // ours and calls `report_unhandled` first.
    unsafe {
        AddVectoredExceptionHandler(1, Some(handler));
        SetUnhandledExceptionFilter(Some(filter));
    }
}

/// What the vectored handler reports as it happens.
const FATAL: [i32; 6] = [
    EXCEPTION_ACCESS_VIOLATION,
    EXCEPTION_ILLEGAL_INSTRUCTION,
    EXCEPTION_PRIV_INSTRUCTION,
    EXCEPTION_INT_DIVIDE_BY_ZERO,
    EXCEPTION_STACK_OVERFLOW,
    EXCEPTION_BREAKPOINT,
];

/// Console CPUs are Zen 2: titles use AMD-only instructions Intel CPUs
/// lack. Does their job instead: `clzero` (`0f 01 fc`) zeroes the 64-byte
/// line at `rax`; `monitorx`/`mwaitx` (`0f 01 fa`/`fb`) are wait hints, so
/// a spin loop around them works with mwaitx as a yield.
unsafe fn emulate_amd_only(info: *mut EXCEPTION_POINTERS) -> bool {
    // SAFETY: the system passes valid exception pointers; the faulting
    // instruction's bytes are readable, as the CPU just fetched them.
    unsafe {
        let record = &*(*info).ExceptionRecord;
        let context = &mut *(*info).ContextRecord;
        if record.ExceptionCode != EXCEPTION_ILLEGAL_INSTRUCTION {
            return false;
        }
        let code = context.Rip as *const u8;
        match code.cast::<[u8; 3]>().read_unaligned() {
            [0x0f, 0x01, 0xfc] => ((context.Rax & !63) as *mut u8).write_bytes(0, 64),
            [0x0f, 0x01, 0xfa] => {}
            [0x0f, 0x01, 0xfb] => std::thread::yield_now(),
            _ => {
                // Only bytes inside the faulting instruction's page are read: the next page may
                // not be mapped.
                let room = 0x1000 - (context.Rip as usize & 0xfff);
                let mut bytes = [0u8; 32];
                let n = room.min(bytes.len());
                std::ptr::copy_nonoverlapping(code, bytes.as_mut_ptr(), n);
                if let Some(pass) = clwb_loop(&bytes[..n]) {
                    // A whole write-back loop in one exception (see `clwb_loop`).
                    let limit = *gpr(context, pass.limit);
                    let base = gpr(context, pass.base);
                    *base = if *base <= limit {
                        base.wrapping_add(((limit - *base) / 64 + 1) * 64)
                    } else {
                        base.wrapping_add(64)
                    };
                    let flags = cmp_flags(*base, limit);
                    context.EFlags = context.EFlags & !CMP_FLAGS | flags;
                    context.Rip += pass.len as u64;
                    return true;
                }
                if n >= 15
                    && let Some(len) = clwb_len(bytes[..15].try_into().unwrap_or([0; 15]))
                {
                    // A write-back hint: the PC's memory is coherent, so it has nothing to do.
                    context.Rip += len as u64;
                    return true;
                }
                let Some(len) = sse4a(code, &mut context.Anonymous.FltSave.XmmRegisters) else {
                    return false;
                };
                context.Rip += len as u64;
                return true;
            }
        }
        context.Rip += 3;
    }
    true
}

/// `clwb m8` (`66 [rex] 0f ae /6`, a memory operand), which Intel CPUs before
/// Ice Lake lack: its length, or `None` for any other instruction.
fn clwb_len(code: [u8; 15]) -> Option<usize> {
    let at = if code[1] & 0xf0 == 0x40 { 2 } else { 1 };
    if code[0] != 0x66 || code[at] != 0x0f || code[at + 1] != 0xae {
        return None;
    }
    let modrm = code[at + 2];
    let (mode, rm) = (modrm >> 6, modrm & 7);
    if mode == 3 || (modrm >> 3) & 7 != 6 {
        return None;
    }
    let mut len = at + 3;
    let mut base = rm;
    if rm == 4 {
        base = code[len] & 7;
        len += 1;
    }
    len += match mode {
        0 if rm == 5 || (rm == 4 && base == 5) => 4,
        1 => 1,
        2 => 4,
        _ => 0,
    };
    Some(len)
}

/// A `clwb` write-back loop as the title's compilers build it, one cache line a
/// pass (`base` and `limit` any registers):
///
/// ```text
/// top: clwb [base]
///      add  base, 0x40
///      cmp  base, limit
///      jbe  top
/// ```
///
/// An exception costs about a microsecond and the title writes back whole
/// buffers this way while it loads, so the loop is done in one: `clwb` has
/// nothing to do on the PC, `base` ends past `limit` as the last pass leaves it.
struct ClwbLoop {
    base: u8,
    limit: u8,
    /// Bytes from `clwb` to the instruction after `jbe`.
    len: usize,
}

fn clwb_loop(code: &[u8]) -> Option<ClwbLoop> {
    // clwb [base]: 66 [40|41] 0f ae, ModRM mod 00, reg 6, no SIB or displacement.
    let (rex, at) = match code.get(1)? {
        0x40 | 0x41 => (code[1], 2),
        _ => (0, 1),
    };
    if *code.first()? != 0x66 || code.get(at..at + 2)? != [0x0f, 0xae] {
        return None;
    }
    let modrm = *code.get(at + 2)?;
    let rm = modrm & 7;
    if modrm >> 6 != 0 || (modrm >> 3) & 7 != 6 || rm == 4 || rm == 5 {
        return None;
    }
    let base = rm | (rex & 1) << 3;
    // add base, 0x40: REX.W(+B) 83 /0 ib.
    let add = at + 3;
    if *code.get(add..add + 4)? != [0x48 | base >> 3, 0x83, 0xc0 | (base & 7), 0x40] {
        return None;
    }
    // cmp base, limit: REX.W 39 /r with base in r/m.
    let cmp = add + 4;
    let &[cmp_rex @ 0x48..=0x4f, 0x39, cmp_modrm] = code.get(cmp..cmp + 3)? else {
        return None;
    };
    if cmp_modrm >> 6 != 3 || (cmp_modrm & 7 | (cmp_rex & 1) << 3) != base {
        return None;
    }
    let limit = (cmp_modrm >> 3) & 7 | (cmp_rex & 4) << 1;
    // jbe top.
    let jump = cmp + 3;
    let len = jump + 2;
    if *code.get(jump)? != 0x76 || i64::from(*code.get(jump + 1)? as i8) != -(len as i64) {
        return None;
    }
    (limit != base).then_some(ClwbLoop { base, limit, len })
}

/// The general register numbered `n` as instructions encode it.
fn gpr(context: &mut CONTEXT, n: u8) -> &mut u64 {
    match n & 15 {
        0 => &mut context.Rax,
        1 => &mut context.Rcx,
        2 => &mut context.Rdx,
        3 => &mut context.Rbx,
        4 => &mut context.Rsp,
        5 => &mut context.Rbp,
        6 => &mut context.Rsi,
        7 => &mut context.Rdi,
        8 => &mut context.R8,
        9 => &mut context.R9,
        10 => &mut context.R10,
        11 => &mut context.R11,
        12 => &mut context.R12,
        13 => &mut context.R13,
        14 => &mut context.R14,
        _ => &mut context.R15,
    }
}

/// The status flags `cmp a, b` leaves (CF, PF, AF, ZF, SF, OF) as `EFLAGS` bits.
fn cmp_flags(a: u64, b: u64) -> u32 {
    let r = a.wrapping_sub(b);
    u32::from(a < b)
        | u32::from((r as u8).count_ones().is_multiple_of(2)) << 2
        | (((a ^ b ^ r) >> 4) as u32 & 1) << 4
        | u32::from(r == 0) << 6
        | ((r >> 63) as u32) << 7
        | ((((a ^ b) & (a ^ r)) >> 63) as u32) << 11
}
const CMP_FLAGS: u32 = 0x8d5;

/// SSE4a's `extrq`/`insertq` (register forms, the only ones there are) on
/// the saved XMM registers; returns the instruction's length.
unsafe fn sse4a(code: *const u8, xmm: &mut [M128A; 16]) -> Option<usize> {
    // SAFETY: the caller's code bytes are readable up to the instruction's end.
    let b = |i: usize| unsafe { code.add(i).read() };
    let prefix = b(0);
    let (rex, at) = if b(1) & 0xf0 == 0x40 {
        (b(1), 2)
    } else {
        (0, 1)
    };
    if !matches!(prefix, 0x66 | 0xf2) || b(at) != 0x0f || !matches!(b(at + 1), 0x78 | 0x79) {
        return None;
    }
    let modrm = b(at + 2);
    if modrm >> 6 != 3 {
        return None;
    }
    let dst = usize::from((modrm >> 3) & 7 | (rex & 4) << 1);
    let src = usize::from(modrm & 7 | (rex & 1) << 3);
    let immediate = b(at + 1) == 0x78;
    let (fields, len) = if immediate {
        ((b(at + 3), b(at + 4)), at + 5)
    } else {
        // extrq reads its fields from src's low half, insertq from its high.
        let f = if prefix == 0x66 { xmm[src].Low } else { xmm[src].High as u64 };
        ((f as u8, (f >> 8) as u8), at + 3)
    };
    let (n, at_bit) = (u32::from(fields.0 & 63), u32::from(fields.1 & 63));
    let bits = if n == 0 { 64 } else { n };
    let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    if prefix == 0x66 {
        // extrq: register form takes its fields from src, works on dst.
        if immediate && (modrm >> 3) & 7 != 0 {
            return None;
        }
        let target = if immediate { src } else { dst };
        xmm[target].Low = (xmm[target].Low >> at_bit) & mask;
    } else {
        let value = (xmm[src].Low & mask) << at_bit;
        xmm[dst].Low = xmm[dst].Low & !(mask << at_bit) | value;
    }
    Some(len)
}

/// Decodes the byte length of an indirect `call [memory]` or `call reg` instruction (`ff /2`).
fn indirect_call_len(code: *const u8, room: usize) -> Option<usize> {
    if room < 2 {
        return None;
    }
    // SAFETY: caller verified that at least `room` bytes are within the mapped page.
    let b0 = unsafe { *code };
    let (_rex, at) = if (0x40..=0x4f).contains(&b0) {
        (b0, 1)
    } else {
        (0, 0)
    };
    if at + 2 > room || unsafe { *code.add(at) } != 0xff {
        return None;
    }
    let modrm = unsafe { *code.add(at + 1) };
    let mod_val = modrm >> 6;
    let reg_val = (modrm >> 3) & 7;
    if reg_val != 2 {
        return None;
    }
    if mod_val == 3 {
        return Some(at + 2);
    }
    let rm = modrm & 7;
    let mut len = at + 2;
    let mut base = rm;
    if rm == 4 {
        if len >= room {
            return None;
        }
        let sib = unsafe { *code.add(len) };
        base = sib & 7;
        len += 1;
    }
    let disp_len = match mod_val {
        0 if rm == 5 || (rm == 4 && base == 5) => 4,
        1 => 1,
        2 => 4,
        _ => 0,
    };
    if len + disp_len > room {
        return None;
    }
    Some(len + disp_len)
}

fn safe_hex_dump(addr: usize, len: usize) -> String {
    use windows_sys::Win32::System::Memory::{
        MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery,
    };
    if addr < 0x1000 || addr > 0x7FFE_00000000 {
        return format!("    {addr:#x} (invalid/non-canonical pointer)\n");
    }
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let n = unsafe { VirtualQuery(addr as *const _, &mut info, size_of_val(&info)) };
    if n == 0 || info.State != MEM_COMMIT || (info.Protect & PAGE_GUARD != 0) || (info.Protect & 0xff == PAGE_NOACCESS) {
        return format!("    {addr:#x} (unreadable or not committed)\n");
    }
    let bytes_in_page = (0x1000 - (addr & 0xfff)).min(len);
    let slice = unsafe { std::slice::from_raw_parts(addr as *const u8, bytes_in_page) };
    let mut out = String::new();
    for (i, chunk) in slice.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' })
            .collect();
        out.push_str(&format!("    {:#010x}: {:<48} |{}|\n", addr + i * 16, hex.join(" "), ascii));
    }
    out
}

fn safe_read_u64(addr: usize) -> Option<u64> {
    use windows_sys::Win32::System::Memory::{
        MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery,
    };
    if addr < 0x1000 || addr > 0x7FFE_00000000 || addr % 8 != 0 {
        return None;
    }
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let n = unsafe { VirtualQuery(addr as *const _, &mut info, size_of_val(&info)) };
    if n == 0 || info.State != MEM_COMMIT || (info.Protect & PAGE_GUARD != 0) || (info.Protect & 0xff == PAGE_NOACCESS) {
        return None;
    }
    Some(unsafe { *(addr as *const u64) })
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HookCounts {
    pub hook33a: u64,
    pub hook33b: u64,
}

fn hook33_counts() -> HookCounts {
    type FnCounts = extern "C" fn() -> HookCounts;
    unsafe {
        let h = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
        if !h.is_null() {
            let proc = windows_sys::Win32::System::LibraryLoader::GetProcAddress(
                h,
                c"victory_game_hook33_counts".as_ptr().cast(),
            );
            if let Some(f) = proc {
                let f: FnCounts = std::mem::transmute(f);
                return f();
            }
        }
        HookCounts::default()
    }
}

fn module_rva(address: usize) -> Option<usize> {
    let mut module: HMODULE = std::ptr::null_mut();
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address as *const u16,
            &mut module,
        )
    };
    if found == 0 {
        return None;
    }
    Some(address - module as usize)
}

/// Recovers from attempts to execute unmapped or invalid function pointers
/// (such as pure-virtual / unimplemented vtable slots, NULL, or sentinel tokens like 0x103),
/// or read access violations on indirect `call [reg + disp]` instructions where the vtable
/// or target pointer is invalid.
unsafe fn emulate_invalid_call(info: *mut EXCEPTION_POINTERS) -> bool {
    // SAFETY: system-provided exception pointers.
    let record = unsafe { &*(*info).ExceptionRecord };
    let context = unsafe { &mut *(*info).ContextRecord };
    if record.ExceptionCode != EXCEPTION_ACCESS_VIOLATION {
        return false;
    }

    let access_kind = record.ExceptionInformation[0];
    let fault_target = record.ExceptionInformation[1] as usize;

    static READ_RECOVERIES: AtomicU64 = AtomicU64::new(0);
    static EXEC_RECOVERIES: AtomicU64 = AtomicU64::new(0);

    // Case 1: Read access violation on an indirect `call [memory]` instruction.
    if access_kind == 0 {
        let rip = context.Rip as usize;
        let room = 0x1000 - (rip & 0xfff);
        if let Some(call_len) = indirect_call_len(rip as *const u8, room) {
            // Target being read is outside mapped module memory.
            if fault_target < 0x10000
                || fault_target > 0x7FFE_00000000
                || module_of(fault_target).is_none()
            {
                let count = READ_RECOVERIES.fetch_add(1, Ordering::Relaxed);
                if count == 0 {
                    let counts = hook33_counts();
                    let (h33a, h33b) = (counts.hook33a, counts.hook33b);
                    let rva = module_rva(rip).unwrap_or(0);
                    let mut dump = String::new();
                    dump.push_str("\n==================== VIctory Diagnostic Dump: first invalid call memory read (total == 1) ====================\n");
                    dump.push_str(&format!(
                        "Fault target read: {fault_target:#x}, Caller instruction: {} (RVA {rva:#x}), Call length: {call_len}\n",
                        locate(rip),
                    ));
                    dump.push_str(&format!(
                        "Active Hook 33 counts: Hook 33A (0x4da62a0) = {h33a}, Hook 33B (0x5312d20) = {h33b}\n"
                    ));
                    dump.push_str(&format!(
                        "Registers:\n  rax {:#x} rbx {:#x} rcx {:#x} rdx {:#x} rsi {:#x} rdi {:#x} rsp {:#x} rbp {:#x}\n  r8 {:#x} r9 {:#x} r10 {:#x} r11 {:#x} r12 {:#x} r13 {:#x} r14 {:#x} r15 {:#x}\n",
                        context.Rax, context.Rbx, context.Rcx, context.Rdx,
                        context.Rsi, context.Rdi, context.Rsp, context.Rbp,
                        context.R8, context.R9, context.R10, context.R11,
                        context.R12, context.R13, context.R14, context.R15,
                    ));
                    dump.push_str("Call Stack:\n");
                    for (i, pc) in unwind(*context).into_iter().enumerate() {
                        dump.push_str(&format!("  #{i:<2} {}\n", locate(pc as usize)));
                    }
                    dump.push_str(&format!("Memory at rdi ({:#x}):\n", context.Rdi));
                    dump.push_str(&safe_hex_dump(context.Rdi as usize, 0x80));
                    dump.push_str(&format!("Memory at rbx ({:#x}):\n", context.Rbx));
                    dump.push_str(&safe_hex_dump(context.Rbx as usize, 0x80));
                    dump.push_str("===============================================================================================================\n\n");
                    eprint!("{dump}");
                    append_to_log(&dump);
                } else if count < 20 || count % 1000 == 0 {
                    eprintln!(
                        "VIctory: recovered from invalid call memory read at {:#x} (caller {}, call length {}, total {})",
                        fault_target,
                        locate(rip),
                        call_len,
                        count + 1
                    );
                }
                context.Rip += call_len as u64;
                context.Rax = 0;
                // SAFETY: access to union field to zero return register XMM0.
                unsafe {
                    context.Anonymous.FltSave.XmmRegisters[0].Low = 0;
                    context.Anonymous.FltSave.XmmRegisters[0].High = 0;
                }
                return true;
            }
        }
        return false;
    }

    // Writes are never call faults.
    if access_kind == 1 {
        return false;
    }

    // Case 2: Execute access violation (DEP or unmapped target).
    // If the fault target is inside a valid loaded module and > 0x10000, let normal crash reporting handle it.
    if fault_target > 0x10000 && module_of(fault_target).is_some() {
        return false;
    }
    // Verify stack bounds before reading [RSP]
    let (mut low, mut high) = (0, 0);
    // SAFETY: current thread stack limits.
    unsafe { GetCurrentThreadStackLimits(&mut low, &mut high) };
    let rsp = context.Rsp as usize;
    if rsp < low || rsp + 8 > high {
        return false;
    }
    let ret_addr = unsafe { *(rsp as *const u64) };
    // The return address must be inside a loaded module (the calling function)
    if module_of(ret_addr as usize).is_none() {
        return false;
    }

    let count = EXEC_RECOVERIES.fetch_add(1, Ordering::Relaxed);
    let rva = module_rva(ret_addr as usize).unwrap_or(0);
    static HIT_4E4F47D: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static HIT_258B7FE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    let is_first_total = count == 0;
    let is_first_4e4f47d = rva == 0x4e4f47d && !HIT_4E4F47D.swap(true, Ordering::Relaxed);
    let is_first_258b7fe = rva == 0x258b7fe && !HIT_258B7FE.swap(true, Ordering::Relaxed);

    if is_first_total || is_first_4e4f47d || is_first_258b7fe {
        let counts = hook33_counts();
        let (h33a, h33b) = (counts.hook33a, counts.hook33b);
        let reason = if is_first_total {
            "first recovered invalid execution (total == 1)"
        } else if is_first_4e4f47d {
            "first hit of +0x4e4f47d (PoolArray dispatch)"
        } else {
            "first hit of +0x258b7fe (Chunk Header virtual call)"
        };

        let mut dump = String::new();
        dump.push_str(&format!(
            "\n==================== VIctory Diagnostic Dump: {reason} ====================\n"
        ));
        dump.push_str(&format!(
            "Fault target: {fault_target:#x}, Caller return site: {} (RVA {rva:#x}), Total: {}\n",
            locate(ret_addr as usize),
            count + 1
        ));
        dump.push_str(&format!(
            "Active Hook 33 counts: Hook 33A (0x4da62a0) = {h33a}, Hook 33B (0x5312d20) = {h33b}\n"
        ));
        dump.push_str(&format!(
            "Registers:\n  rax {:#x} rbx {:#x} rcx {:#x} rdx {:#x} rsi {:#x} rdi {:#x} rsp {:#x} rbp {:#x}\n  r8 {:#x} r9 {:#x} r10 {:#x} r11 {:#x} r12 {:#x} r13 {:#x} r14 {:#x} r15 {:#x}\n",
            context.Rax, context.Rbx, context.Rcx, context.Rdx,
            context.Rsi, context.Rdi, context.Rsp, context.Rbp,
            context.R8, context.R9, context.R10, context.R11,
            context.R12, context.R13, context.R14, context.R15,
        ));
        dump.push_str("Call Stack:\n");
        for (i, pc) in unwind(*context).into_iter().enumerate() {
            dump.push_str(&format!("  #{i:<2} {}\n", locate(pc as usize)));
        }
        dump.push_str(&format!("Memory at rdi ({:#x}):\n", context.Rdi));
        dump.push_str(&safe_hex_dump(context.Rdi as usize, 0x80));
        if let Some(vtable_rdi) = safe_read_u64(context.Rdi as usize) {
            dump.push_str(&format!("Memory at *(u64*)rdi ({vtable_rdi:#x}):\n"));
            dump.push_str(&safe_hex_dump(vtable_rdi as usize, 0x80));
        } else {
            dump.push_str("  *(u64*)rdi is not readable or invalid\n");
        }
        dump.push_str(&format!("Memory at rbx ({:#x}):\n", context.Rbx));
        dump.push_str(&safe_hex_dump(context.Rbx as usize, 0x80));
        if let Some(vtable_rbx) = safe_read_u64(context.Rbx as usize) {
            dump.push_str(&format!("Memory at *(u64*)rbx ({vtable_rbx:#x}):\n"));
            dump.push_str(&safe_hex_dump(vtable_rbx as usize, 0x80));
        } else {
            dump.push_str("  *(u64*)rbx is not readable or invalid\n");
        }
        dump.push_str("================================================================================\n\n");
        eprint!("{dump}");
        append_to_log(&dump);
    } else if count < 20 || count % 1000 == 0 {
        eprintln!(
            "VIctory: recovered from invalid execution at {:#x} (caller {}, total {})",
            fault_target,
            locate(ret_addr as usize),
            count + 1
        );
    }
    context.Rip = ret_addr;
    context.Rsp += 8;
    context.Rax = 0;
    // SAFETY: access to union field to zero return register XMM0.
    unsafe {
        context.Anonymous.FltSave.XmmRegisters[0].Low = 0;
        context.Anonymous.FltSave.XmmRegisters[0].High = 0;
    }
    true
}

const WATCH_TARGET_RVA: usize = 0xa024230;
const WATCH_RDATA_START_RVA: usize = 0x5f41000;
const WATCH_RDATA_END_RVA: usize = 0x67e2000;

static WATCH_IMAGE_BASE: AtomicU64 = AtomicU64::new(0);
static WATCH_TARGET_ADDR: AtomicU64 = AtomicU64::new(0);
static WATCH_LAST_VAL: AtomicU64 = AtomicU64::new(0);
static WATCH_WORKER_TID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn arm_hw_watchpoint_tid(tid: u32, target: u64) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Diagnostics::Debug::{GetThreadContext, SetThreadContext};
    use windows_sys::Win32::System::Threading::{
        OpenThread, ResumeThread, SuspendThread, THREAD_GET_CONTEXT, THREAD_SET_CONTEXT,
        THREAD_SUSPEND_RESUME,
    };
    const CONTEXT_AMD64: u32 = 0x0010_0000;
    const CONTEXT_DEBUG_REGISTERS: u32 = CONTEXT_AMD64 | 0x10;

    unsafe {
        let h = OpenThread(
            THREAD_GET_CONTEXT | THREAD_SET_CONTEXT | THREAD_SUSPEND_RESUME,
            0,
            tid,
        );
        if h.is_null() {
            return;
        }
        if SuspendThread(h) != u32::MAX {
            let mut ctx = Aligned16(std::mem::zeroed());
            ctx.0.ContextFlags = CONTEXT_DEBUG_REGISTERS;
            if GetThreadContext(h, &mut ctx.0) != 0 {
                ctx.0.Dr0 = target;
                ctx.0.Dr6 = 0;
                // L0=1 (bit 0), RW0=01 (write, bits 16..17), LEN0=11 (4 bytes, bits 18..19)
                ctx.0.Dr7 = (ctx.0.Dr7 & !0x000f_0003) | 0x000d_0001;
                if SetThreadContext(h, &ctx.0) == 0 {
                    eprintln!("VIctory: SetThreadContext(Dr0) failed on tid {tid}");
                }
            }
            ResumeThread(h);
        }
        CloseHandle(h);
    }
}

/// Arms an x86-64 hardware write-watchpoint (`Dr0`, 8 bytes) on `image+0xa024230`
/// across all existing and future threads.
pub fn watch_map_data_store(image_base: usize) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};

    let target = (image_base + WATCH_TARGET_RVA) as u64;
    WATCH_IMAGE_BASE.store(image_base as u64, Ordering::Relaxed);
    WATCH_LAST_VAL.store(unsafe { *(target as *const u64) }, Ordering::Relaxed);
    WATCH_TARGET_ADDR.store(target, Ordering::Release);

    std::thread::spawn(move || {
        use std::collections::HashMap;
        let worker_tid = unsafe { GetCurrentThreadId() };
        WATCH_WORKER_TID.store(worker_tid, Ordering::Release);
        let mut armed_count: HashMap<u32, u8> = HashMap::new();
        loop {
            let pid = unsafe { GetCurrentProcessId() };
            unsafe {
                let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
                if snap != -1isize as *mut _ {
                    let mut te: THREADENTRY32 = std::mem::zeroed();
                    te.dwSize = size_of_val(&te) as u32;
                    if Thread32First(snap, &mut te) != 0 {
                        loop {
                            if te.th32OwnerProcessID == pid && te.th32ThreadID != worker_tid {
                                let c = armed_count.entry(te.th32ThreadID).or_insert(0);
                                if *c < 3 {
                                    arm_hw_watchpoint_tid(te.th32ThreadID, target);
                                    *c += 1;
                                }
                            }
                            if Thread32Next(snap, &mut te) == 0 {
                                break;
                            }
                        }
                    }
                    CloseHandle(snap);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
    eprintln!("VIctory: armed HW write-watchpoint (Dr0) on image+0xa024230 ({target:#x})");
}

unsafe fn handle_watchpoint(info: *mut EXCEPTION_POINTERS) -> bool {
    use windows_sys::Win32::Foundation::EXCEPTION_SINGLE_STEP;

    let target_addr = WATCH_TARGET_ADDR.load(Ordering::Acquire) as usize;
    if target_addr == 0 {
        return false;
    }
    let record = unsafe { &*(*info).ExceptionRecord };
    let context = unsafe { &mut *(*info).ContextRecord };

    if record.ExceptionCode == EXCEPTION_SINGLE_STEP && (context.Dr6 & 1) != 0 {
        context.Dr6 &= !1;
        let new_val = unsafe { *(target_addr as *const u64) };
        let old_val = WATCH_LAST_VAL.swap(new_val, Ordering::SeqCst);
        let image_base = WATCH_IMAGE_BASE.load(Ordering::Relaxed) as usize;
        let rdata_start = image_base + WATCH_RDATA_START_RVA;
        let rdata_end = image_base + WATCH_RDATA_END_RVA;
        let in_rdata = (new_val as usize) >= rdata_start && (new_val as usize) < rdata_end;
        let rip = context.Rip as usize;
        let tag = if in_rdata {
            "HW write to [image+0xa024230] (in .rdata)"
        } else {
            "!!! HW NON-.RDATA OVERWRITE OF [image+0xa024230] !!!"
        };
        let mut msg = format!(
            "\n==================== VIctory Watchpoint: {tag} ====================\n\
             Next instruction (after write): {}\n\
             Old [image+0xa024230]: {old_val:#x}\n\
             New [image+0xa024230]: {new_val:#x} (in_rdata={in_rdata})\n",
            locate(rip),
        );
        if !in_rdata {
            msg.push_str(&format!(
                "Registers:\n  rax {:#x} rbx {:#x} rcx {:#x} rdx {:#x} rsi {:#x} rdi {:#x} rsp {:#x} rbp {:#x}\n  r8 {:#x} r9 {:#x} r10 {:#x} r11 {:#x} r12 {:#x} r13 {:#x} r14 {:#x} r15 {:#x}\n",
                context.Rax, context.Rbx, context.Rcx, context.Rdx,
                context.Rsi, context.Rdi, context.Rsp, context.Rbp,
                context.R8, context.R9, context.R10, context.R11,
                context.R12, context.R13, context.R14, context.R15,
            ));
            msg.push_str("Call Stack:\n");
            for (i, pc) in unwind(*context).into_iter().enumerate() {
                msg.push_str(&format!("  #{i:<2} {}\n", locate(pc as usize)));
            }
        }
        msg.push_str("=========================================================================\n");
        eprint!("{msg}");
        append_to_log(&msg);
        return true;
    }
    false
}

unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system passes valid exception pointers.
    if unsafe { handle_watchpoint(info) } {
        return -1; // EXCEPTION_CONTINUE_EXECUTION
    }
    // SAFETY: the system passes valid exception pointers.
    if unsafe { emulate_amd_only(info) } {
        return -1; // EXCEPTION_CONTINUE_EXECUTION
    }
    // SAFETY: system passes valid exception pointers; recovers unmapped virtual calls.
    if unsafe { emulate_invalid_call(info) } {
        return -1; // EXCEPTION_CONTINUE_EXECUTION
    }
    // SAFETY: as above.
    if FATAL.contains(&unsafe { (*(*info).ExceptionRecord).ExceptionCode }) {
        // SAFETY: as above.
        unsafe { report("first-chance exception", info, false) };
    }
    EXCEPTION_CONTINUE_SEARCH
}

/// For an unhandled-exception filter: reports the exception that is about
/// to end the process, and appends it to the crash log.
///
/// # Safety
/// `info` comes from the system, for an exception being dispatched.
pub unsafe fn report_unhandled(info: *const EXCEPTION_POINTERS) {
    // SAFETY: forwarded from the caller.
    unsafe { report("unhandled exception", info, true) };
}

/// The bytes at `at` as hex, as many as the longest x86 instruction (15),
/// but none past the end of `at`'s page: the next page may not be mapped.
/// For an instruction the CPU refused, they name it (`0f 01 fd` is
/// `rdpru`), which the address alone does not.
///
/// # Safety
/// `at`'s page is readable.
unsafe fn instruction_bytes(at: usize) -> String {
    let n = (0x1000 - (at & 0xfff)).min(15);
    // SAFETY: `n` bytes from `at` stay inside its page, which the caller
    // says is readable.
    let bytes = unsafe { std::slice::from_raw_parts(at as *const u8, n) };
    let hex: Vec<_> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    hex.join(" ")
}

/// MSVC's code for a C++ `throw`, and its magic first parameter.
const CPP_EXCEPTION: i32 = 0xE06D_7363_u32 as i32;
const CPP_MAGIC: usize = 0x1993_0520;

/// What a C++ throw threw: the decorated name of its type, then its value
/// where that names the cause (an integer, e.g. an HRESULT; a standard
/// exception's message).
///
/// # Safety
/// `p` are the parameters of a live MSVC throw (`p[0]` the magic).
unsafe fn thrown(p: &[usize]) -> String {
    // SAFETY: the throw's ThrowInfo, its RVAs relative to `p[3]`: catchable
    // type array at +12 (count, then type RVAs), each type's descriptor at
    // +4, whose decorated name starts at +16; the object is at `p[1]`.
    unsafe {
        let rva = |at: usize| (at as *const u32).read_unaligned() as usize;
        let types = p[3] + rva(p[2] + 12);
        let name = |i: usize| {
            let descriptor = p[3] + rva(p[3] + rva(types + 4 + 4 * i) + 4);
            std::ffi::CStr::from_ptr((descriptor + 16) as *const std::ffi::c_char)
                .to_string_lossy()
        };
        let first = name(0);
        let value = match &*first {
            // int, unsigned, long (HRESULT), unsigned long
            ".H" | ".I" | ".J" | ".K" if p[1] != 0 => {
                format!(" = {:#010x}", (p[1] as *const u32).read_unaligned())
            }
            // std::exception keeps its message after the table pointer.
            _ if (0..rva(types)).any(|i| name(i) == ".?AVexception@std@@") => {
                let message = (p[1] as *const *const std::ffi::c_char).add(1).read();
                if message.is_null() {
                    String::new()
                } else {
                    format!(": {}", std::ffi::CStr::from_ptr(message).to_string_lossy())
                }
            }
            _ => String::new(),
        };
        format!("{first}{value}")
    }
}

/// # Safety
/// See [`report_unhandled`].
unsafe fn report(what: &str, info: *const EXCEPTION_POINTERS, to_file: bool) {
    // SAFETY: the system passes valid exception pointers.
    let (record, context) = unsafe { (&*(*info).ExceptionRecord, &*(*info).ContextRecord) };
    let code = record.ExceptionCode;
    let at = record.ExceptionAddress as usize;
    let p = record.ExceptionInformation;
    let detail = if code == EXCEPTION_ACCESS_VIOLATION {
        let kind = match p[0] {
            0 => "read",
            1 => "write",
            _ => "execute",
        };
        format!(" ({kind} {:#x}: {})", p[1], memory_at(p[1]))
    } else if code == CPP_EXCEPTION && record.NumberParameters >= 4 && p[0] == CPP_MAGIC {
        // SAFETY: a live MSVC throw.
        format!(" (C++ {} thrown)", unsafe { thrown(&p) })
    } else if code == EXCEPTION_ILLEGAL_INSTRUCTION || code == EXCEPTION_PRIV_INSTRUCTION {
        // SAFETY: the CPU refused the instruction at `at`, so it fetched it.
        format!(" (code {})", unsafe { instruction_bytes(at) })
    } else {
        String::new()
    };
    // Printed line by line (a fault while unwinding must not lose the
    // lines before it), and kept for the file.
    let mut text = String::new();
    let mut line = |l: String| {
        eprintln!("{l}");
        text.push_str(&l);
        text.push('\n');
    };
    line(format!(
        "VIctory: {what} {:#010x} at {}{detail}",
        code as u32,
        locate(at)
    ));
    line(format!(
        "  rax {:#x} rbx {:#x} rcx {:#x} rdx {:#x} rsi {:#x} rdi {:#x} rsp {:#x} rbp {:#x}",
        context.Rax,
        context.Rbx,
        context.Rcx,
        context.Rdx,
        context.Rsi,
        context.Rdi,
        context.Rsp,
        context.Rbp
    ));
    line(format!(
        "  r8 {:#x} r9 {:#x} r10 {:#x} r11 {:#x} r12 {:#x} r13 {:#x} r14 {:#x} r15 {:#x}",
        context.R8,
        context.R9,
        context.R10,
        context.R11,
        context.R12,
        context.R13,
        context.R14,
        context.R15
    ));
    for (i, pc) in unwind(*context).into_iter().enumerate() {
        line(format!("  #{i:<2} {}", locate(pc as usize)));
    }
    if to_file {
        let said = title_output();
        eprint!("{said}");
        text.push_str(&said);
        append_to_log(&text);
    }
    #[cfg(feature = "diagnostics")]
    for (address, len, file) in SAVE.get().into_iter().flatten() {
        // SAFETY: the caller of `save_on_crash` named readable memory.
        let bytes = unsafe { std::slice::from_raw_parts(*address as *const u8, *len) };
        let saved = std::fs::write(file, bytes);
        eprintln!(
            "  saved {address:#x}+{len:#x} to {} ({saved:?})",
            file.display()
        );
    }
}

/// Return addresses of the faulting thread's frames, from the faulting
/// instruction up, by the unwind data of each image (the Windows loader
/// registered it for title images too). Code without unwind data (our
/// thunks, leaf functions) is taken to have its return address at `rsp`.
fn unwind(context: CONTEXT) -> Vec<u64> {
    let (mut low, mut high) = (0, 0);
    // SAFETY: writes the current thread's stack bounds.
    unsafe { GetCurrentThreadStackLimits(&mut low, &mut high) };
    let mut frames = Vec::with_capacity(MAX_FRAMES);
    unwind_into(context, low, high, &mut frames);
    frames
}

/// A `CONTEXT` at the 16-byte alignment the unwind and capture functions need.
#[repr(C, align(16))]
pub(crate) struct Aligned16(pub(crate) CONTEXT);

/// [`unwind`] for any thread of the process, its stack being `low..high`;
/// pushes no more than `MAX_FRAMES`, so it does not allocate when `frames`
/// has that capacity (another thread may be suspended holding the heap).
pub(crate) fn unwind_into(context: CONTEXT, low: usize, high: usize, frames: &mut Vec<u64>) {
    // `RtlVirtualUnwind` writes the context with 16-byte aligned stores, and
    // windows-sys declares `CONTEXT` without that alignment: a by-value copy on
    // the stack can land 8 bytes off and fault (seen in a debug build).
    let mut aligned = Aligned16(context);
    let context = &mut aligned.0;
    frames.push(context.Rip);
    while frames.len() < MAX_FRAMES {
        // Stop where reading the stack would fault inside this handler.
        let rsp = context.Rsp as usize;
        if rsp < low || rsp + 8 > high {
            break;
        }
        let mut image = 0;
        // SAFETY: looks up the function table entry covering `Rip`, if any.
        let entry = unsafe { RtlLookupFunctionEntry(context.Rip, &mut image, null_mut()) };
        if entry.is_null() {
            // SAFETY: `rsp` is inside this thread's stack.
            context.Rip = unsafe { (rsp as *const u64).read() };
            context.Rsp += 8;
        } else {
            let (mut data, mut frame) = (null_mut(), 0);
            // SAFETY: `entry` belongs to `image`; the context is our copy.
            unsafe {
                RtlVirtualUnwind(
                    UNW_FLAG_NHANDLER,
                    image,
                    context.Rip,
                    entry,
                    context,
                    &mut data,
                    &mut frame,
                    null_mut(),
                )
            };
        }
        if context.Rip == 0 {
            break;
        }
        frames.push(context.Rip);
    }
}

/// `module+rva`, or the bare address.
pub fn locate(address: usize) -> String {
    module_of(address).unwrap_or_else(|| format!("{address:#x}"))
}

fn module_of(address: usize) -> Option<String> {
    let mut module: HMODULE = std::ptr::null_mut();
    // SAFETY: looks up the module containing `address`; no reference taken.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address as *const u16,
            &mut module,
        )
    };
    if found == 0 {
        return None;
    }
    let mut path = [0u16; 260];
    // SAFETY: `module` is a loaded module; the buffer length is passed.
    let len = unsafe { GetModuleFileNameW(module, path.as_mut_ptr(), path.len() as u32) };
    let path = String::from_utf16_lossy(&path[..len as usize]);
    let name = path.rsplit('\\').next().unwrap_or(&path);
    Some(format!("{name}+{:#x}", address - module as usize))
}

/// What is at `address`, for an access violation there: nothing, memory
/// reserved but never committed, a guard page, or the protection the access
/// broke (with the module, for an image's pages). Each points at a different
/// cause: a wild pointer, an allocation whose commit failed, a stack
/// overrun, a write to code or constants.
fn memory_at(address: usize) -> String {
    use windows_sys::Win32::System::Memory::{
        MEM_FREE, MEM_IMAGE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE,
        PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD,
        PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY, VirtualQuery,
    };
    // SAFETY: a zeroed MEMORY_BASIC_INFORMATION is valid.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: reads the address space's bookkeeping, not `address` itself.
    let n = unsafe { VirtualQuery(address as *const _, &mut info, size_of_val(&info)) };
    if n == 0 {
        // Above the process's address space: Windows reports a
        // non-canonical address as all ones.
        return "outside the address space".into();
    }
    let protect = match info.State {
        MEM_FREE => return "nothing mapped there".into(),
        MEM_RESERVE => return "reserved, never committed".into(),
        _ if info.Protect & PAGE_GUARD != 0 => return "a guard page".into(),
        _ => match info.Protect & 0xff {
            PAGE_NOACCESS => "no access".into(),
            PAGE_READONLY => "read-only".into(),
            PAGE_READWRITE => "read-write".into(),
            PAGE_WRITECOPY => "copy-on-write".into(),
            PAGE_EXECUTE => "execute-only".into(),
            PAGE_EXECUTE_READ => "read-execute".into(),
            PAGE_EXECUTE_READWRITE => "read-write-execute".into(),
            PAGE_EXECUTE_WRITECOPY => "copy-on-write, executable".into(),
            _ => format!("protection {:#x}", info.Protect),
        },
    };
    match module_of(address) {
        Some(module) if info.Type == MEM_IMAGE => format!("{protect}, in {module}"),
        _ => protect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        EXCEPTION_RECORD, RtlCaptureContext, RtlCaptureStackBackTrace,
    };

    #[test]
    #[inline(never)]
    fn unwinds_like_the_system_walk() {
        // SAFETY: zeroed CONTEXT is valid; both calls write our buffers.
        let (frames, walked) = unsafe {
            // `RtlCaptureContext` wants it 16-byte aligned too.
            let mut context = Aligned16(std::mem::zeroed());
            RtlCaptureContext(&mut context.0);
            let mut walked = [null_mut(); 8];
            let n = RtlCaptureStackBackTrace(0, 8, walked.as_mut_ptr(), null_mut());
            (unwind(context.0), walked[..usize::from(n)].to_vec())
        };
        // Frame 0 differs (two call sites in this function); callers agree.
        assert!(walked.len() >= 4, "{walked:?}");
        let ours: Vec<_> = frames[1..walked.len()]
            .iter()
            .map(|&f| f as usize)
            .collect();
        let system: Vec<_> = walked[1..].iter().map(|&f| f as usize).collect();
        assert_eq!(ours, system);
    }

    #[test]
    fn memory_at_names_what_an_access_hit() {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_GUARD, PAGE_NOACCESS, PAGE_READONLY,
            PAGE_READWRITE, VirtualAlloc, VirtualFree,
        };
        // SAFETY: reserves four pages of ours and commits three of them;
        // nothing reads or writes them.
        let base = unsafe {
            let base = VirtualAlloc(std::ptr::null(), 0x4000, MEM_RESERVE, PAGE_NOACCESS);
            assert!(!base.is_null());
            let page = |i: usize, protect| {
                let at = base.cast::<u8>().add(i * 0x1000).cast();
                assert!(!VirtualAlloc(at, 0x1000, MEM_COMMIT, protect).is_null());
            };
            page(1, PAGE_READONLY);
            page(2, PAGE_READWRITE | PAGE_GUARD);
            page(3, PAGE_NOACCESS);
            base as usize
        };
        assert_eq!(memory_at(base + 8), "reserved, never committed");
        assert_eq!(memory_at(base + 0x1008), "read-only");
        assert_eq!(memory_at(base + 0x2008), "a guard page");
        assert_eq!(memory_at(base + 0x3008), "no access");
        // SAFETY: releases the reservation made above.
        unsafe { VirtualFree(base as *mut _, 0, MEM_RELEASE) };
        assert_eq!(memory_at(base + 8), "nothing mapped there");
        assert_eq!(memory_at(0), "nothing mapped there");
        assert_eq!(memory_at(usize::MAX), "outside the address space");
        // This test's own code: an image page, named by its module.
        let code = memory_at(memory_at_names_what_an_access_hit as *const () as usize);
        let named = code.starts_with("read-execute, in victory_loader-");
        assert!(named, "{code}");
    }

    /// On an Intel CPU, the real instructions go through the vectored
    /// handler. On an AMD CPU they run natively, and the tests below test
    /// the emulation.
    #[test]
    fn clwb_loops() {
        // clwb [rcx]; add rcx, 0x40; cmp rcx, rax; jbe top
        let a = clwb_loop(&[
            0x66, 0x0f, 0xae, 0x31, 0x48, 0x83, 0xc1, 0x40, 0x48, 0x39, 0xc1, 0x76, 0xf3,
        ])
        .expect("rcx loop");
        assert_eq!((a.base, a.limit, a.len), (1, 0, 13));
        // clwb [r9]; add r9, 0x40; cmp r9, rax; jbe top
        let b = clwb_loop(&[
            0x66, 0x41, 0x0f, 0xae, 0x31, 0x49, 0x83, 0xc1, 0x40, 0x49, 0x39, 0xc1, 0x76, 0xf2,
        ])
        .expect("r9 loop");
        assert_eq!((b.base, b.limit, b.len), (9, 0, 14));
        // A different stride, a jump elsewhere, or a cut-off loop is not one.
        assert!(
            clwb_loop(&[
                0x66, 0x0f, 0xae, 0x31, 0x48, 0x83, 0xc1, 0x20, 0x48, 0x39, 0xc1, 0x76, 0xf3
            ])
            .is_none()
        );
        assert!(
            clwb_loop(&[
                0x66, 0x0f, 0xae, 0x31, 0x48, 0x83, 0xc1, 0x40, 0x48, 0x39, 0xc1, 0x76, 0xf0
            ])
            .is_none()
        );
        assert!(clwb_loop(&[0x66, 0x0f, 0xae, 0x31, 0x48, 0x83, 0xc1, 0x40]).is_none());
        // The flags the last cmp leaves: base just past limit.
        assert_eq!(cmp_flags(0x1040, 0x1000) & 0x41, 0); // CF and ZF clear: the loop ends
        assert_eq!(cmp_flags(0x1000, 0x1000) & 0x40, 0x40);
    }

    #[test]
    fn clwb_lengths() {
        let pad = |b: &[u8]| {
            let mut c = [0u8; 15];
            c[..b.len()].copy_from_slice(b);
            c
        };
        assert_eq!(clwb_len(pad(&[0x66, 0x41, 0x0f, 0xae, 0x31])), Some(5)); // clwb [r9]
        assert_eq!(clwb_len(pad(&[0x66, 0x0f, 0xae, 0x30])), Some(4)); // clwb [rax]
        assert_eq!(clwb_len(pad(&[0x66, 0x0f, 0xae, 0x70, 0x40])), Some(5)); // clwb [rax+0x40]
        assert_eq!(
            clwb_len(pad(&[0x66, 0x0f, 0xae, 0xb0, 0, 1, 0, 0])),
            Some(8)
        ); // [rax+disp32]
        assert_eq!(clwb_len(pad(&[0x66, 0x0f, 0xae, 0x34, 0x24])), Some(5)); // clwb [rsp]
        assert_eq!(
            clwb_len(pad(&[0x66, 0x0f, 0xae, 0x35, 0, 0, 0, 0])),
            Some(8)
        ); // [rip+disp32]
        assert_eq!(clwb_len(pad(&[0x66, 0x0f, 0xae, 0x38])), None); // clflushopt, /7
        assert_eq!(clwb_len(pad(&[0x0f, 0xae, 0x30])), None); // xsaveopt, no 66
        assert_eq!(clwb_len(pad(&[0x66, 0x0f, 0xae, 0xf0])), None); // register form
    }

    #[test]
    fn clzero_zeroes_its_line_on_any_cpu() {
        report_crashes();
        #[repr(align(64))]
        struct Line([u8; 128]);
        let mut line = Line([0xff; 128]);
        let at = line.0.as_mut_ptr().wrapping_add(5); // inside the first line
        // SAFETY: clzero writes only the 64-byte line holding `at`, ours.
        unsafe { std::arch::asm!(".byte 0x0f, 0x01, 0xfc", in("rax") at) };
        assert!(line.0[..64].iter().all(|&b| b == 0));
        assert!(line.0[64..].iter().all(|&b| b == 0xff));
        // insertq xmm0, xmm1, len 16, at 16; then extrq xmm0, len 8, at 20.
        let out: u64;
        // SAFETY: touches only the named registers.
        unsafe {
            std::arch::asm!(
                "movq xmm0, {a}", "movq xmm1, {b}",
                ".byte 0xf2, 0x0f, 0x78, 0xc1, 0x10, 0x10",
                "movq {a}, xmm0",
                ".byte 0x66, 0x0f, 0x78, 0xc0, 0x08, 0x14",
                "movq {b}, xmm0",
                a = inout(reg) 0x1111_2222_3333_4444u64 => _, b = inout(reg) 0xabcdu64 => out,
                out("xmm0") _, out("xmm1") _)
        };
        assert_eq!(out, 0xbc); // 0x1111_2222_abcd_4444 >> 20, 8 bits
        // SAFETY: monitorx/mwaitx are wait hints that write no memory.
        unsafe {
            std::arch::asm!(".byte 0x0f, 0x01, 0xfa", ".byte 0x0f, 0x01, 0xfb",
                in("rax") at, in("ecx") 0, in("edx") 0)
        };
    }

    fn reg(low: u64, high: u64) -> M128A {
        M128A {
            Low: low,
            High: high as i64,
        }
    }

    /// Gives `code` to the emulation as if it faulted with `exception` at
    /// its first byte, with `rax` and `xmm` as the saved registers. The
    /// emulation does not execute `code`, so this works on any CPU. Returns
    /// the number of bytes that the emulation skipped, or `None` if it did
    /// not emulate the instruction. `xmm` gets the saved registers after.
    fn emulate_as(exception: i32, code: &[u8], rax: u64, xmm: &mut [M128A; 16]) -> Option<u64> {
        // `int3` after the instruction, so that a decoder that reads too
        // far sees no valid bytes.
        let mut bytes = [0xcc_u8; 16];
        bytes[..code.len()].copy_from_slice(code);
        let start = bytes.as_ptr() as u64;
        // SAFETY: a zeroed record and context are valid; the emulation
        // reads only `bytes` and writes only `context` (and memory at
        // `rax` for clzero, which the caller owns).
        unsafe {
            let mut record: EXCEPTION_RECORD = std::mem::zeroed();
            record.ExceptionCode = exception;
            let mut context: CONTEXT = std::mem::zeroed();
            context.Rip = start;
            context.Rax = rax;
            context.Anonymous.FltSave.XmmRegisters = *xmm;
            let mut pointers = EXCEPTION_POINTERS {
                ExceptionRecord: &mut record,
                ContextRecord: &mut context,
            };
            let emulated = emulate_amd_only(&mut pointers);
            *xmm = context.Anonymous.FltSave.XmmRegisters;
            emulated.then(|| context.Rip - start)
        }
    }

    fn emulate(code: &[u8], rax: u64, xmm: &mut [M128A; 16]) -> Option<u64> {
        emulate_as(EXCEPTION_ILLEGAL_INSTRUCTION, code, rax, xmm)
    }

    fn low(xmm: &[M128A; 16], i: usize) -> u64 {
        xmm[i].Low
    }

    #[test]
    fn emulated_clzero_zeroes_its_line() {
        #[repr(align(64))]
        struct Line([u8; 128]);
        let mut line = Line([0xff; 128]);
        let at = line.0.as_mut_ptr().wrapping_add(5) as u64; // inside the first line
        let mut xmm = [reg(0, 0); 16];
        assert_eq!(emulate(&[0x0f, 0x01, 0xfc], at, &mut xmm), Some(3));
        assert!(line.0[..64].iter().all(|&b| b == 0));
        assert!(line.0[64..].iter().all(|&b| b == 0xff));
    }

    #[test]
    fn emulated_monitorx_and_mwaitx_are_skipped() {
        let mut xmm = [reg(1, 2); 16];
        assert_eq!(emulate(&[0x0f, 0x01, 0xfa], 0, &mut xmm), Some(3));
        assert_eq!(emulate(&[0x0f, 0x01, 0xfb], 0, &mut xmm), Some(3));
        assert!(xmm.iter().all(|r| r.Low == 1 && r.High == 2));
    }

    #[test]
    fn emulated_insertq_and_extrq_with_immediates() {
        let mut xmm = [reg(0, 0); 16];
        xmm[0] = reg(0x1111_2222_3333_4444, 0x5555);
        xmm[1] = reg(0xabcd, 0x6666);
        // insertq xmm0, xmm1, length 16, index 16
        let insertq = [0xf2, 0x0f, 0x78, 0xc1, 0x10, 0x10];
        assert_eq!(emulate(&insertq, 0, &mut xmm), Some(6));
        assert_eq!(low(&xmm, 0), 0x1111_2222_abcd_4444);
        assert_eq!((low(&xmm, 1), xmm[1].High), (0xabcd, 0x6666));
        // extrq xmm0, length 8, index 20
        let extrq = [0x66, 0x0f, 0x78, 0xc0, 0x08, 0x14];
        assert_eq!(emulate(&extrq, 0, &mut xmm), Some(6));
        assert_eq!(low(&xmm, 0), 0xbc);
    }

    #[test]
    fn emulated_insertq_and_extrq_with_fields_in_a_register() {
        let mut xmm = [reg(0, 0); 16];
        // insertq xmm0, xmm1: length and index in xmm1's high half.
        xmm[0] = reg(0x1111_2222_3333_4444, 0);
        xmm[1] = reg(0xabcd, 0x10_10);
        assert_eq!(emulate(&[0xf2, 0x0f, 0x79, 0xc1], 0, &mut xmm), Some(4));
        assert_eq!(low(&xmm, 0), 0x1111_2222_abcd_4444);
        // extrq xmm0, xmm1: length and index in xmm1's low half.
        xmm[1] = reg(0x14_08, 0);
        assert_eq!(emulate(&[0x66, 0x0f, 0x79, 0xc1], 0, &mut xmm), Some(4));
        assert_eq!(low(&xmm, 0), 0xbc);
    }

    #[test]
    fn emulated_sse4a_reaches_xmm8_to_xmm15() {
        let mut xmm = [reg(0, 0); 16];
        xmm[8] = reg(0x1111_2222_3333_4444, 0);
        xmm[9] = reg(0xabcd, 0);
        // insertq xmm8, xmm9, length 16, index 16 (REX.R and REX.B)
        let insertq = [0xf2, 0x45, 0x0f, 0x78, 0xc1, 0x10, 0x10];
        assert_eq!(emulate(&insertq, 0, &mut xmm), Some(7));
        assert_eq!(low(&xmm, 8), 0x1111_2222_abcd_4444);
        assert_eq!((low(&xmm, 0), low(&xmm, 1)), (0, 0));
        // extrq xmm9, length 8, index 4 (REX.B)
        let extrq = [0x66, 0x41, 0x0f, 0x78, 0xc1, 0x08, 0x04];
        assert_eq!(emulate(&extrq, 0, &mut xmm), Some(7));
        assert_eq!(low(&xmm, 9), 0xbc);
        assert_eq!(low(&xmm, 1), 0);
    }

    #[test]
    fn emulated_sse4a_length_0_is_64_bits() {
        let mut xmm = [reg(0, 0); 16];
        xmm[0] = reg(0x1122_3344_5566_7788, 0);
        // extrq xmm0, length 0 (64), index 0. A non-zero index with
        // length 64 goes past bit 63, and AMD leaves that result undefined.
        assert_eq!(
            emulate(&[0x66, 0x0f, 0x78, 0xc0, 0x00, 0x00], 0, &mut xmm),
            Some(6)
        );
        assert_eq!(low(&xmm, 0), 0x1122_3344_5566_7788);
        // insertq xmm0, xmm1, length 0 (64), index 0
        xmm[1] = reg(0xdead_beef_cafe_f00d, 0);
        assert_eq!(
            emulate(&[0xf2, 0x0f, 0x78, 0xc1, 0x00, 0x00], 0, &mut xmm),
            Some(6)
        );
        assert_eq!(low(&xmm, 0), 0xdead_beef_cafe_f00d);
    }

    #[test]
    fn other_instructions_and_exceptions_are_not_emulated() {
        let mut xmm = [reg(0, 0); 16];
        assert_eq!(emulate(&[0x0f, 0x0b], 0, &mut xmm), None); // ud2
        // extrq with a memory operand, which does not exist
        assert_eq!(emulate(&[0x66, 0x0f, 0x79, 0x00], 0, &mut xmm), None);
        // extrq with immediates needs ModRM.reg 0
        let bad_extrq = [0x66, 0x0f, 0x78, 0xc8, 0x08, 0x14];
        assert_eq!(emulate(&bad_extrq, 0, &mut xmm), None);
        // a clzero that did not cause an illegal instruction
        let clzero = [0x0f, 0x01, 0xfc];
        assert_eq!(
            emulate_as(EXCEPTION_ACCESS_VIOLATION, &clzero, 0, &mut xmm),
            None
        );
    }

    #[test]
    fn instruction_bytes_stop_at_the_end_of_their_page() {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS, PAGE_READWRITE, VirtualAlloc,
            VirtualFree, VirtualProtect,
        };
        // rdpru, then the next instruction (mov rcx, rax) and padding.
        let mut code = [0x90_u8; 16];
        code[..6].copy_from_slice(&[0x0f, 0x01, 0xfd, 0x48, 0x89, 0xc1]);
        // SAFETY: two fresh pages of ours, the second made unreadable, so a
        // read past the end of the first faults.
        unsafe {
            let page = VirtualAlloc(
                std::ptr::null(),
                0x2000,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
            .cast::<u8>();
            assert!(!page.is_null());
            let mut old = 0;
            let guard = page.add(0x1000).cast();
            assert_ne!(VirtualProtect(guard, 0x1000, PAGE_NOACCESS, &mut old), 0);
            page.copy_from_nonoverlapping(code.as_ptr(), code.len());
            let whole = "0f 01 fd 48 89 c1 90 90 90 90 90 90 90 90 90";
            assert_eq!(instruction_bytes(page as usize), whole);
            // The page's last three bytes: rdpru, and nothing read after it.
            let end = page.add(0x1000 - 3);
            end.copy_from_nonoverlapping(code.as_ptr(), 3);
            assert_eq!(instruction_bytes(end as usize), "0f 01 fd");
            VirtualFree(page.cast(), 0, MEM_RELEASE);
        }
    }
}
