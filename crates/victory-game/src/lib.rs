//! Everything specific to Grand Theft Auto VI: its container formats, build
//! identities, and the hooks we place in its code (located by per-build
//! offsets or byte-pattern signatures). What may and may not be stored here
//! is defined in docs/asset-policy.md.

pub mod sgd5;

use std::sync::atomic::{AtomicU64, Ordering};

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HookCounts {
    pub hook33a: u64,
    pub hook33b: u64,
}

#[unsafe(no_mangle)]
pub static HOOK33A_COUNT: AtomicU64 = AtomicU64::new(0);
#[unsafe(no_mangle)]
pub static HOOK33B_COUNT: AtomicU64 = AtomicU64::new(0);
#[unsafe(no_mangle)]
pub static HOOK37A_COUNT: AtomicU64 = AtomicU64::new(0);

#[unsafe(no_mangle)]
pub extern "C" fn on_hook33a_bailout(bad_rdx: u64, caller_ret: u64) {
    let count = HOOK33A_COUNT.fetch_add(1, Ordering::Relaxed);
    if count == 0 || count % 1000 == 0 {
        eprintln!(
            "VIctory: Hook 33A guard triggered at 0x4da62a0 (bad rdx: {bad_rdx:#x}, caller: {caller_ret:#x}, total: {})",
            count + 1
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn on_hook33b_bailout(bad_token: u64, caller_ret: u64) {
    let count = HOOK33B_COUNT.fetch_add(1, Ordering::Relaxed);
    if count == 0 || count % 1000 == 0 {
        eprintln!(
            "VIctory: Hook 33B (0x5312d20) bailout triggered (token: {bad_token:#x}, caller: {caller_ret:#x}, total: {})",
            count + 1
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn on_hook37a_null(entity: u64, comp: u64) {
    let count = HOOK37A_COUNT.fetch_add(1, Ordering::Relaxed);
    if count == 0 || count % 1000 == 0 {
        eprintln!(
            "VIctory: Hook 37A: rdx == NULL at 0x142723f (entity: {entity:#x}, comp: {comp:#x}, total: {})",
            count + 1
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn victory_game_hook33_counts() -> HookCounts {
    HookCounts {
        hook33a: HOOK33A_COUNT.load(Ordering::Relaxed),
        hook33b: HOOK33B_COUNT.load(Ordering::Relaxed),
    }
}

/// A specific build of the Grand Theft Auto VI executable we know how to run.
#[derive(Debug, Clone, Copy)]
pub struct KnownBuild {
    /// Our own label, e.g. "1.0.0 retail".
    pub label: &'static str,
    /// BLAKE3 of the original executable, hex.
    pub blake3: &'static str,
    /// Watchdog abort convergence sequence RVA (0x2215c16).
    pub watchdog_abort_rva: usize,
}

/// Builds we support.
pub const KNOWN_BUILDS: &[KnownBuild] = &[KnownBuild {
    label: "Xbox v1.0.2.1",
    blake3: "b99f876b74d4a77f03af5f878223b27bd647b5c35a79d5decd9f716504239e93",
    watchdog_abort_rva: 0x2215c16,
}];

/// Looks up a build by the BLAKE3 hash of its executable.
pub fn identify(blake3_hex: &str) -> Option<&'static KnownBuild> {
    KNOWN_BUILDS
        .iter()
        .find(|b| b.blake3.eq_ignore_ascii_case(blake3_hex))
}

#[cfg(windows)]
impl KnownBuild {
    /// Disables engine watchdog aborts that fire during long loads, disk seeks, or shader compilation,
    /// and neutralizes unresolvable streaming resource dereferences.
    ///
    /// # Safety
    /// Modifies mapped executable code memory.
    pub unsafe fn apply_hooks(&self, base: usize) -> Result<(), String> {
        use std::ffi::c_void;
        use windows_sys::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, VirtualProtect};

        let patch = |rva: usize, bytes: &[u8]| -> Result<(), String> {
            let addr = (base + rva) as *mut c_void;
            let mut old = 0;
            if unsafe { VirtualProtect(addr, bytes.len(), PAGE_EXECUTE_READWRITE, &mut old) } == 0 {
                return Err(format!("VirtualProtect at {rva:#x} failed"));
            }
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len());
                VirtualProtect(addr, bytes.len(), old, &mut old);
            }
            Ok(())
        };

        let patch_exec = |rva: usize, bytes: &[u8]| -> Result<(), String> {
            let addr = (base + rva) as *mut c_void;
            let mut old = 0;
            if unsafe { VirtualProtect(addr, bytes.len(), PAGE_EXECUTE_READWRITE, &mut old) } == 0 {
                return Err(format!("VirtualProtect at {rva:#x} failed"));
            }
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len());
            }
            Ok(())
        };

        // 1. Neutralize the entire watchdog abort sequence at 0x2215c16 through 0x2215c41 (43 bytes).
        // Replace with:
        //   mov ebp, 0x1e      (bd 1e 00 00 00) - resets the 30s timeout counter
        //   jmp 0x2215af6      (e9 d6 fe ff ff) - jumps back to the 1s sleep loop
        //   nop...             (90 * 33)        - overwrites call, mov [0], 0, and int3
        let mut patch_bytes = [0x90u8; 43];
        patch_bytes[0..5].copy_from_slice(&[0xbd, 0x1e, 0x00, 0x00, 0x00]);
        patch_bytes[5..10].copy_from_slice(&[0xe9, 0xd6, 0xfe, 0xff, 0xff]);
        patch(self.watchdog_abort_rva, &patch_bytes)?;

        // 2. Universal Chunk Relocation Fix at RVA 0x4da6315:
        // In RAGE, 0x4da62d0 is the chunk lookup function: pgBase* ResolveChunk(ChunkTable* rcx, uint64_t token).
        // When a chunk is found, it returns rax = (chunk_base - chunk_virtual_start) (relocation delta).
        // All 90 callers in the engine execute `add REG, rax; mov [DEST], REG` where REG == token (rdx).
        // If 0x4da62d0 returns rax = 0, callers do `token + 0 = token`, leaving un-relocated tokens
        // (e.g. 0x5a00003270114436, 0x5189d0000, 0x3a95da003d, 0x100000000, 0x50, -1) in memory,
        // which later cause access violations (read 0xffffffffffffffff, read 0x18, or jumping to data pages).
        // By returning rax = -rdx (-token), caller executes `token + (-token) = 0`, writing clean NULL (0)
        // to destination across all 90 call sites in the entire engine.
        // Unused 10-byte 0xcc padding at 0x4da62c6:
        //   mov rax, rdx (48 89 d0)
        //   neg rax      (48 f7 d8)
        //   ret          (c3)
        // At 0x4da6315 (4 bytes):
        //   jmp short 0x4da62c6 (eb af)
        //   nop                 (90)
        //   nop                 (90)
        patch(0x4da62c6, &[0x48, 0x89, 0xd0, 0x48, 0xf7, 0xd8, 0xc3])?;
        patch(0x4da6315, &[0xeb, 0xaf, 0x90, 0x90])?;

        // 3. Guard pgRef resource reference resolution at RVA 0x4da63c7 in 0x4da6330.
        // When streaming world assets, unresolvable external references (< 4 GiB, e.g. 0x10050000)
        // or non-canonical tokens (>= 128 TB / bit 48 set, e.g. 0x1000000000020)
        // crash at 0x4da63f8 with an access violation if dereferenced without validation.
        // We install a hook in the .text section padding at 0x5f40980:
        // 1. Calls original 0x4da62a0 to perform chunk translation.
        // 2. Checks if [rsi] is a valid host user-mode pointer: 1 <= (ptr >> 32) <= 0x7FFE.
        // 3. If valid, returns normally to 0x4da63cc.
        // 4. If invalid (null, < 4 GiB, or non-canonical/kernel >= 128 TB), sets [rsi] = 0,
        //    rax = rdi (this), unwinds the frame, and returns cleanly to caller without crashing.
        let hook_rva: usize = 0x5f40980;
        let call_rva: usize = 0x4da63c7;
        let target_rva: usize = 0x4da62a0;
        let rel_call = (target_rva as isize - (hook_rva as isize + 5)) as i32;
        let rel_hook = (hook_rva as isize - (call_rva as isize + 5)) as i32;

        let mut hook_code = Vec::with_capacity(48);
        // call 0x4da62a0 (e8 rel32)
        hook_code.push(0xe8);
        hook_code.extend_from_slice(&rel_call.to_le_bytes());
        // mov r10, qword ptr [rsi] (4c 8b 16)
        hook_code.extend_from_slice(&[0x4c, 0x8b, 0x16]);
        // shr r10, 32 (49 c1 ea 20)
        hook_code.extend_from_slice(&[0x49, 0xc1, 0xea, 0x20]);
        // dec r10 (49 ff ca)
        hook_code.extend_from_slice(&[0x49, 0xff, 0xca]);
        // cmp r10, 0x7ffe (49 81 fa fe 7f 00 00)
        hook_code.extend_from_slice(&[0x49, 0x81, 0xfa, 0xfe, 0x7f, 0x00, 0x00]);
        // jbe valid (+18 bytes: 76 12)
        hook_code.extend_from_slice(&[0x76, 0x12]);
        // invalid path:
        // mov qword ptr [rsi], 0 (48 c7 06 00 00 00 00)
        hook_code.extend_from_slice(&[0x48, 0xc7, 0x06, 0x00, 0x00, 0x00, 0x00]);
        // mov rax, rdi (48 89 f8)
        hook_code.extend_from_slice(&[0x48, 0x89, 0xf8]);
        // add rsp, 0x28 (48 83 c4 28)
        hook_code.extend_from_slice(&[0x48, 0x83, 0xc4, 0x28]);
        // pop rbx (5b)
        hook_code.push(0x5b);
        // pop rdi (5f)
        hook_code.push(0x5f);
        // pop rsi (5e)
        hook_code.push(0x5e);
        // ret (c3)
        hook_code.push(0xc3);
        // valid:
        // ret (c3)
        hook_code.push(0xc3);

        patch(hook_rva, &hook_code)?;

        // Redirect 0x4da63c7: call 0x5f40980 (e8 rel32)
        let mut call_patch = [0xe8u8; 5];
        call_patch[1..5].copy_from_slice(&rel_hook.to_le_bytes());
        patch(call_rva, &call_patch)?;

        // 4. Guard dictionary / streaming pointer lookup at RVA 0xa5dae0.
        // Function 0xa5dae0 takes (rcx = id, rdx = ptr, r8 = &out).
        // At 0xa5db01, it reads [rdi + 0x20] where rdi = rdx without checking if rdx is valid.
        // If rdx is null, an uncommitted buffer, or invalid garbage (< 0x10000 or > 0x7ffe_00000000),
        // it crashes with 0xc0000005. Both callers (0x9ed6e1 and 0x5453ac2) test rax for null and handle it.
        // We install a trampoline in padding at 0x5f409b0:
        // 1. Checks if rdx is a valid canonical user-mode pointer: rdx >= 0x10000 && (rdx >> 32) <= 0x7ffe.
        // 2. If valid: pushes r14, rsi, rdi, rbx (re-creating the original 5 bytes) and jumps to 0xa5dae5.
        // 3. If invalid: writes 0 to [r8] (if r8 is valid), clears eax, and returns 0 to the caller.
        let fn_rva: usize = 0xa5dae0;
        let tramp_rva: usize = 0x5f409b0;
        let resume_rva: usize = 0xa5dae5;

        let mut tramp_code = Vec::with_capacity(72);
        // mov r10, rdx (49 89 d2)
        tramp_code.extend_from_slice(&[0x49, 0x89, 0xd2]);
        // shr r10, 32 (49 c1 ea 20)
        tramp_code.extend_from_slice(&[0x49, 0xc1, 0xea, 0x20]);
        // cmp r10, 0x7ffe (49 81 fa fe 7f 00 00)
        tramp_code.extend_from_slice(&[0x49, 0x81, 0xfa, 0xfe, 0x7f, 0x00, 0x00]);
        // ja invalid (+19 bytes: 77 13)
        tramp_code.extend_from_slice(&[0x77, 0x13]);
        // cmp rdx, 0x10000 (48 81 fa 00 00 01 00)
        tramp_code.extend_from_slice(&[0x48, 0x81, 0xfa, 0x00, 0x00, 0x01, 0x00]);
        // jb invalid (+10 bytes: 72 0a)
        tramp_code.extend_from_slice(&[0x72, 0x0a]);
        // push r14, rsi, rdi, rbx (41 56 56 57 53)
        tramp_code.extend_from_slice(&[0x41, 0x56, 0x56, 0x57, 0x53]);
        // jmp resume_rva (e9 rel32)
        let jmp_pos = tramp_code.len();
        let rel_resume = (resume_rva as isize - (tramp_rva as isize + jmp_pos as isize + 5)) as i32;
        tramp_code.push(0xe9);
        tramp_code.extend_from_slice(&rel_resume.to_le_bytes());

        // invalid label: (offset 35 / tramp_rva + 0x23 = 0x5f409d3)
        // test r8, r8 (4d 85 c0)
        tramp_code.extend_from_slice(&[0x4d, 0x85, 0xc0]);
        // jz ret_0 (+23 bytes: 74 17)
        tramp_code.extend_from_slice(&[0x74, 0x17]);
        // mov r10, r8 (4d 89 c2)
        tramp_code.extend_from_slice(&[0x4d, 0x89, 0xc2]);
        // shr r10, 32 (49 c1 ea 20)
        tramp_code.extend_from_slice(&[0x49, 0xc1, 0xea, 0x20]);
        // cmp r10, 0x7ffe (49 81 fa fe 7f 00 00)
        tramp_code.extend_from_slice(&[0x49, 0x81, 0xfa, 0xfe, 0x7f, 0x00, 0x00]);
        // ja ret_0 (+7 bytes: 77 07)
        tramp_code.extend_from_slice(&[0x77, 0x07]);
        // mov qword ptr [r8], 0 (49 c7 00 00 00 00 00)
        tramp_code.extend_from_slice(&[0x49, 0xc7, 0x00, 0x00, 0x00, 0x00, 0x00]);
        // ret_0 label:
        // xor eax, eax (31 c0)
        tramp_code.extend_from_slice(&[0x31, 0xc0]);
        // ret (c3)
        tramp_code.push(0xc3);

        patch(tramp_rva, &tramp_code)?;

        // Hook entry at 0xa5dae0: jmp 0x5f409b0 (e9 rel32)
        // Replaces original 5 bytes: push r14 (2), push rsi (1), push rdi (1), push rbx (1).
        let rel_tramp = (tramp_rva as isize - (fn_rva as isize + 5)) as i32;
        let mut fn_patch = [0xe9u8; 5];
        fn_patch[1..5].copy_from_slice(&rel_tramp.to_le_bytes());
        patch(fn_rva, &fn_patch)?;

        // 5. Neutralize null pointer dereference at RVA 0x14270ed in 0x1427080.
        // At 0x14270d4, 0x4e91330 looks up the context for the current stream entity into rbx (can be NULL).
        // At 0x14270ea, `test rbx, rbx; je 0x1427110` erroneously branches into 0x1427110, which immediately
        // reads [rbx + 0x58] and crashes with access violation 0xc0000005 (read 0x58: nothing mapped there).
        // All instructions from 0x14270ef to 0x1427163 access fields of rbx (offsets 0x50, 0x58, 0x60, 0x68, 0x70, 0x74, 0x7c).
        // When rbx is NULL, execution must branch to 0x1427164 to exit the block cleanly.
        // Change `je +0x21` (74 21) to `je +0x75` (74 75).
        patch(0x14270ed, &[0x74, 0x75])?;

        // 6. Guard null parent / container pointer in world entity registration at RVA 0x7b6197 in 0x7b60f0.
        // In 0x7b60f0, 0x4da24a0 is called at 0x7b6197 to create/fetch a world streaming item (returns in rax).
        // At 0x7b619c, the engine executes:
        //   mov r15, [rdi]
        //   mov rcx, rdi
        //   mov edx, ebx
        //   mov r14, rax
        //   call [r15 + 8]
        // where rdi is the container/parent object pointer loaded from [r15 + 0x10] at 0x7b6140.
        // For top-level or orphaned world entities without a parent container, rdi is NULL (0x0).
        // Dereferencing [rdi] crashes immediately with access violation 0xc0000005 (read 0x0).
        // We replace 0x7b6197..0x7b61ab (20 bytes: call + virtual call) with a jump to trampoline at 0x5f40a00:
        // 1. Calls 0x4da24a0 to create the item.
        // 2. Checks if rdi is a valid canonical user pointer: rdi != 0 && (rdi >> 32) <= 0x7ffe.
        // 3. If valid: calls [r15 + 8] as originally intended.
        // 4. Jumps to 0x7b61ab to store rdi into [rax + 0x28] and complete initialization.
        let call_site_rva: usize = 0x7b6197;
        let tramp6_rva: usize = 0x5f40a00;
        let call_target_rva: usize = 0x4da24a0;
        let return_rva: usize = 0x7b61ab;

        let mut tramp6 = Vec::with_capacity(50);
        // call 0x4da24a0 (e8 rel32)
        let rel_call = (call_target_rva as isize - (tramp6_rva as isize + 5)) as i32;
        tramp6.push(0xe8);
        tramp6.extend_from_slice(&rel_call.to_le_bytes());

        // test rdi, rdi: 48 85 ff
        tramp6.extend_from_slice(&[0x48, 0x85, 0xff]);
        // jz done: 74 1f (+31 bytes to jmp return_rva)
        tramp6.extend_from_slice(&[0x74, 0x1f]);

        // mov r10, rdi: 49 89 fa
        tramp6.extend_from_slice(&[0x49, 0x89, 0xfa]);
        // shr r10, 32: 49 c1 ea 20
        tramp6.extend_from_slice(&[0x49, 0xc1, 0xea, 0x20]);
        // cmp r10, 0x7ffe: 49 81 fa fe 7f 00 00
        tramp6.extend_from_slice(&[0x49, 0x81, 0xfa, 0xfe, 0x7f, 0x00, 0x00]);
        // ja done: 77 0f (+15 bytes to jmp return_rva)
        tramp6.extend_from_slice(&[0x77, 0x0f]);

        // mov r15, [rdi]: 4c 8b 3f
        tramp6.extend_from_slice(&[0x4c, 0x8b, 0x3f]);
        // mov rcx, rdi: 48 89 f9
        tramp6.extend_from_slice(&[0x48, 0x89, 0xf9]);
        // mov edx, ebx: 89 da
        tramp6.extend_from_slice(&[0x89, 0xda]);
        // mov r14, rax: 49 89 c6
        tramp6.extend_from_slice(&[0x49, 0x89, 0xc6]);
        // call [r15 + 8]: 41 ff 57 08
        tramp6.extend_from_slice(&[0x41, 0xff, 0x57, 0x08]);

        // done: jmp return_rva (e9 rel32)
        let done_pos = tramp6.len();
        let rel_ret = (return_rva as isize - (tramp6_rva as isize + done_pos as isize + 5)) as i32;
        tramp6.push(0xe9);
        tramp6.extend_from_slice(&rel_ret.to_le_bytes());

        patch(tramp6_rva, &tramp6)?;

        // At 0x7b6197: jmp 0x5f40a00 (5 bytes) + 15 NOPs up to 0x7b61ab
        let mut patch_site = [0x90u8; 20];
        let rel_site = (tramp6_rva as isize - (call_site_rva as isize + 5)) as i32;
        patch_site[0] = 0xe9;
        patch_site[1..5].copy_from_slice(&rel_site.to_le_bytes());
        patch(call_site_rva, &patch_site)?;

        // 7. Guard virtual method call at RVA 0x258bf1f in 0x258bef0.
        // At 0x258bf1f, the engine executes:
        //   mov rax, [r14]
        //   mov rcx, r14
        //   call [rax + 0x98]
        // For certain stream entity types, the vtable slot at [rax + 0x98] is NULL (0x0) or an invalid token (e.g. 0x103).
        // Calling it crashes immediately with access violation 0xc0000005 (execute 0x0).
        // The return value in eax is tested at 0x258bf2b (test eax, eax; jne reject). If 0, it continues.
        // We replace 0x258bf1f..0x258bf2b (12 bytes) with a jump to trampoline at 0x5f40a40:
        // 1. Validates that r14 is a valid pointer.
        // 2. Validates that rax (vtable) is a valid pointer.
        // 3. Validates that [rax + 0x98] is a valid executable function pointer (>= 0x10000 and <= 0x7FFE_00000000).
        // 4. If valid: calls the function pointer with rcx = r14.
        // 5. If invalid: clears eax to 0.
        // 6. Jumps back to 0x258bf2b to resume execution cleanly.
        let site7_rva: usize = 0x258bf1f;
        let tramp7_rva: usize = 0x5f40a40;
        let return7_rva: usize = 0x258bf2b;

        let mut tramp7 = Vec::with_capacity(80);
        // test r14, r14: 4d 85 f6
        tramp7.extend_from_slice(&[0x4d, 0x85, 0xf6]);
        // jz invalid: 74 40 (+64 bytes)
        tramp7.extend_from_slice(&[0x74, 0x40]);

        // mov r10, r14: 4d 89 f2
        tramp7.extend_from_slice(&[0x4d, 0x89, 0xf2]);
        // shr r10, 32: 49 c1 ea 20
        tramp7.extend_from_slice(&[0x49, 0xc1, 0xea, 0x20]);
        // cmp r10, 0x7ffe: 49 81 fa fe 7f 00 00
        tramp7.extend_from_slice(&[0x49, 0x81, 0xfa, 0xfe, 0x7f, 0x00, 0x00]);
        // ja invalid: 77 30 (+48 bytes)
        tramp7.extend_from_slice(&[0x77, 0x30]);

        // mov rax, [r14]: 49 8b 06
        tramp7.extend_from_slice(&[0x49, 0x8b, 0x06]);
        // test rax, rax: 48 85 c0
        tramp7.extend_from_slice(&[0x48, 0x85, 0xc0]);
        // jz invalid: 74 28 (+40 bytes)
        tramp7.extend_from_slice(&[0x74, 0x28]);

        // mov r11, [rax + 0x98]: 4c 8b 98 98 00 00 00
        tramp7.extend_from_slice(&[0x4c, 0x8b, 0x98, 0x98, 0x00, 0x00, 0x00]);
        // cmp r11, 0x10000: 49 81 fb 00 00 01 00
        tramp7.extend_from_slice(&[0x49, 0x81, 0xfb, 0x00, 0x00, 0x01, 0x00]);
        // jb invalid: 72 18 (+24 bytes)
        tramp7.extend_from_slice(&[0x72, 0x18]);

        // mov r10, r11: 4d 89 da
        tramp7.extend_from_slice(&[0x4d, 0x89, 0xda]);
        // shr r10, 32: 49 c1 ea 20
        tramp7.extend_from_slice(&[0x49, 0xc1, 0xea, 0x20]);
        // cmp r10, 0x7ffe: 49 81 fa fe 7f 00 00
        tramp7.extend_from_slice(&[0x49, 0x81, 0xfa, 0xfe, 0x7f, 0x00, 0x00]);
        // ja invalid: 77 08 (+8 bytes)
        tramp7.extend_from_slice(&[0x77, 0x08]);

        // mov rcx, r14: 4c 89 f1
        tramp7.extend_from_slice(&[0x4c, 0x89, 0xf1]);
        // call r11: 41 ff d3
        tramp7.extend_from_slice(&[0x41, 0xff, 0xd3]);
        // jmp done: eb 02 (+2 bytes)
        tramp7.extend_from_slice(&[0xeb, 0x02]);

        // invalid: xor eax, eax (31 c0)
        tramp7.extend_from_slice(&[0x31, 0xc0]);

        // done: jmp return7_rva (e9 rel32)
        let done7_pos = tramp7.len();
        let rel_ret7 = (return7_rva as isize - (tramp7_rva as isize + done7_pos as isize + 5)) as i32;
        tramp7.push(0xe9);
        tramp7.extend_from_slice(&rel_ret7.to_le_bytes());

        patch(tramp7_rva, &tramp7)?;

        // At 0x258bf1f: jmp 0x5f40a40 (5 bytes) + 7 NOPs up to 0x258bf2b (12 bytes total)
        let mut patch7_site = [0x90u8; 12];
        let rel7_site = (tramp7_rva as isize - (site7_rva as isize + 5)) as i32;
        patch7_site[0] = 0xe9;
        patch7_site[1..5].copy_from_slice(&rel7_site.to_le_bytes());
        patch(site7_rva, &patch7_site)?;

        // 8. Bounds check arena chunk table lookups at RVA 0x4f8ef40 and 0x4f8ef60.
        // In the memory allocator / arena manager (called from 0x4f8edb2), chunk index is computed:
        //   sub r14, [arena_base]
        //   shr r14, 0x10
        //   mov ecx, r14d
        // When a pointer outside the arena (such as NULL) is queried, r14 underflows to 0xffffffff0000,
        // making ecx = 0xffff0000 (~4.29 billion).
        // 0x4f8ef40 and 0x4f8ef60 index table [0x71bfb08] by rcx*8 without bounds checking, causing
        // access violation 0xc0000005 (read 0x15041f9937: nothing mapped there).
        // Both functions check flags for the chunk (bit 63 in 0x4f8ef40, bit 62 in 0x4f8ef60).
        // For out-of-bounds chunks (ecx >= 0x10000), they should return 0 (flag not set).
        // We replace both 32-byte function bodies with bounds-checked versions that return 0 if ecx >= 0x10000:
        let patch_4f8ef40 = [
            0x81, 0xf9, 0x00, 0x00, 0x01, 0x00, // cmp ecx, 0x10000
            0x73, 0x12,                         // jae +18 (to 0x4f8ef5a)
            0x48, 0x8b, 0x05, 0xb9, 0x0b, 0x23, 0x02, // mov rax, [rip + 0x2230bb9] (-> 0x71bfb08)
            0x89, 0xc9,                         // mov ecx, ecx
            0x48, 0x8b, 0x04, 0xc8,             // mov rax, [rax + rcx*8]
            0x48, 0xc1, 0xe8, 0x3f,             // shr rax, 0x3f
            0xc3,                               // ret
            0x31, 0xc0,                         // xor eax, eax
            0xc3,                               // ret
            0x90, 0x90, 0x90,                   // nop padding
        ];
        patch(0x4f8ef40, &patch_4f8ef40)?;

        let patch_4f8ef60 = [
            0x81, 0xf9, 0x00, 0x00, 0x01, 0x00, // cmp ecx, 0x10000
            0x73, 0x14,                         // jae +20 (to 0x4f8ef7c)
            0x48, 0x8b, 0x05, 0x99, 0x0b, 0x23, 0x02, // mov rax, [rip + 0x2230b99] (-> 0x71bfb08)
            0x89, 0xc9,                         // mov ecx, ecx
            0x8a, 0x44, 0xc8, 0x07,             // mov al, [rax + rcx*8 + 7]
            0x24, 0x40,                         // and al, 0x40
            0xc0, 0xe8, 0x06,                   // shr al, 6
            0xc3,                               // ret
            0x90,                               // nop
            0x31, 0xc0,                         // xor eax, eax
            0xc3,                               // ret
            0x90,                               // nop padding
        ];
        patch(0x4f8ef60, &patch_4f8ef60)?;

        // 9. Guard indirect call at RVA 0x4da241d in 0x4da23b0.
        // At 0x4da241d:
        //   mov rcx, rdi
        //   call rax
        //   jmp 0x4da23e0
        // Target in rax is frequently an invalid token (0x14d0, 0x2af0, 0x0).
        // If rax < 0x10000, clear eax to 0 and skip call directly to 0x4da23e0.
        let site9_rva: usize = 0x4da241d;
        let tramp9_rva: usize = 0x5f40ab0;
        let return9_rva: usize = 0x4da23e0;

        let mut tramp9 = Vec::with_capacity(32);
        // cmp rax, 0x10000: 48 3d 00 00 01 00
        tramp9.extend_from_slice(&[0x48, 0x3d, 0x00, 0x00, 0x01, 0x00]);
        // jb invalid (+10 bytes): 72 0a
        tramp9.extend_from_slice(&[0x72, 0x0a]);
        // mov rcx, rdi: 48 89 f9
        tramp9.extend_from_slice(&[0x48, 0x89, 0xf9]);
        // call rax: ff d0
        tramp9.extend_from_slice(&[0xff, 0xd0]);
        // jmp return9_rva
        let pos9a = tramp9.len();
        let rel9a = (return9_rva as isize - (tramp9_rva as isize + pos9a as isize + 5)) as i32;
        tramp9.push(0xe9);
        tramp9.extend_from_slice(&rel9a.to_le_bytes());
        // invalid: xor eax, eax: 31 c0
        tramp9.extend_from_slice(&[0x31, 0xc0]);
        // jmp return9_rva
        let pos9b = tramp9.len();
        let rel9b = (return9_rva as isize - (tramp9_rva as isize + pos9b as isize + 5)) as i32;
        tramp9.push(0xe9);
        tramp9.extend_from_slice(&rel9b.to_le_bytes());

        patch(tramp9_rva, &tramp9)?;

        let mut patch9 = [0x90u8; 7];
        let rel9 = (tramp9_rva as isize - (site9_rva as isize + 5)) as i32;
        patch9[0] = 0xe9;
        patch9[1..5].copy_from_slice(&rel9.to_le_bytes());
        patch(site9_rva, &patch9)?;

        // 10. Guard virtual method call at RVA 0x258c65e in 0x258c5f0.
        // At 0x258c65e:
        //   mov rax, [r15]
        //   mov rcx, r15
        //   call [rax + 0x68]
        //   test rax, rax
        // If rax < 0x10000 or [rax + 0x68] < 0x10000, clear eax to 0 and jump to 0x258c667.
        let site10_rva: usize = 0x258c65e;
        let tramp10_rva: usize = 0x5f40b00;
        let return10_rva: usize = 0x258c667;

        let mut tramp10 = Vec::with_capacity(48);
        // mov rax, [r15]: 49 8b 07
        tramp10.extend_from_slice(&[0x49, 0x8b, 0x07]);
        // cmp rax, 0x10000: 48 3d 00 00 01 00
        tramp10.extend_from_slice(&[0x48, 0x3d, 0x00, 0x00, 0x01, 0x00]);
        // jb invalid (+21 bytes): 72 15
        tramp10.extend_from_slice(&[0x72, 0x15]);
        // mov r11, [rax + 0x68]: 4c 8b 58 68
        tramp10.extend_from_slice(&[0x4c, 0x8b, 0x58, 0x68]);
        // cmp r11, 0x10000: 49 81 fb 00 00 01 00
        tramp10.extend_from_slice(&[0x49, 0x81, 0xfb, 0x00, 0x00, 0x01, 0x00]);
        // jb invalid (+10 bytes): 72 0a
        tramp10.extend_from_slice(&[0x72, 0x0a]);
        // mov rcx, r15: 4c 89 f9
        tramp10.extend_from_slice(&[0x4c, 0x89, 0xf9]);
        // call r11: 41 ff d3
        tramp10.extend_from_slice(&[0x41, 0xff, 0xd3]);
        // jmp done (+2 bytes): eb 02
        tramp10.extend_from_slice(&[0xeb, 0x02]);
        // invalid: xor eax, eax: 31 c0
        tramp10.extend_from_slice(&[0x31, 0xc0]);
        // done: jmp return10_rva
        let pos10 = tramp10.len();
        let rel10 = (return10_rva as isize - (tramp10_rva as isize + pos10 as isize + 5)) as i32;
        tramp10.push(0xe9);
        tramp10.extend_from_slice(&rel10.to_le_bytes());

        patch(tramp10_rva, &tramp10)?;

        let mut patch10 = [0x90u8; 9];
        let rel10_site = (tramp10_rva as isize - (site10_rva as isize + 5)) as i32;
        patch10[0] = 0xe9;
        patch10[1..5].copy_from_slice(&rel10_site.to_le_bytes());
        patch(site10_rva, &patch10)?;

        // 11. Guard indirect call at RVA 0x4f82fc4 in 0x4f82f80.
        // At 0x4f82fc4:
        //   mov rcx, rsi
        //   call rax
        //   jmp 0x4f82f7d
        // If rax < 0x10000, clear eax to 0 and jump to 0x4f82f7d.
        let site11_rva: usize = 0x4f82fc4;
        let tramp11_rva: usize = 0x5f40b50;
        let return11_rva: usize = 0x4f82f7d;

        let mut tramp11 = Vec::with_capacity(32);
        // cmp rax, 0x10000: 48 3d 00 00 01 00
        tramp11.extend_from_slice(&[0x48, 0x3d, 0x00, 0x00, 0x01, 0x00]);
        // jb invalid (+10 bytes): 72 0a
        tramp11.extend_from_slice(&[0x72, 0x0a]);
        // mov rcx, rsi: 48 89 f1
        tramp11.extend_from_slice(&[0x48, 0x89, 0xf1]);
        // call rax: ff d0
        tramp11.extend_from_slice(&[0xff, 0xd0]);
        // jmp return11_rva
        let pos11a = tramp11.len();
        let rel11a = (return11_rva as isize - (tramp11_rva as isize + pos11a as isize + 5)) as i32;
        tramp11.push(0xe9);
        tramp11.extend_from_slice(&rel11a.to_le_bytes());
        // invalid: xor eax, eax: 31 c0
        tramp11.extend_from_slice(&[0x31, 0xc0]);
        // jmp return11_rva
        let pos11b = tramp11.len();
        let rel11b = (return11_rva as isize - (tramp11_rva as isize + pos11b as isize + 5)) as i32;
        tramp11.push(0xe9);
        tramp11.extend_from_slice(&rel11b.to_le_bytes());

        patch(tramp11_rva, &tramp11)?;

        let mut patch11 = [0x90u8; 7];
        let rel11 = (tramp11_rva as isize - (site11_rva as isize + 5)) as i32;
        patch11[0] = 0xe9;
        patch11[1..5].copy_from_slice(&rel11.to_le_bytes());
        patch(site11_rva, &patch11)?;

        // 12. Guard virtual method call at RVA 0x258bc39 in 0x258bc20.
        // At 0x258bc39:
        //   mov rax, [rbx]
        //   mov rcx, rbx
        //   mov r8, rbp
        //   call [rax + 0x28]
        //   test al, al
        // If rax < 0x10000 or [rax + 0x28] < 0x10000, clear eax to 0 and jump to 0x258bc45.
        let site12_rva: usize = 0x258bc39;
        let tramp12_rva: usize = 0x5f40b90;
        let return12_rva: usize = 0x258bc45;

        let mut tramp12 = Vec::with_capacity(48);
        // mov rax, [rbx]: 48 8b 03
        tramp12.extend_from_slice(&[0x48, 0x8b, 0x03]);
        // cmp rax, 0x10000: 48 3d 00 00 01 00
        tramp12.extend_from_slice(&[0x48, 0x3d, 0x00, 0x00, 0x01, 0x00]);
        // jb invalid (+24 bytes): 72 18
        tramp12.extend_from_slice(&[0x72, 0x18]);
        // mov r11, [rax + 0x28]: 4c 8b 58 28
        tramp12.extend_from_slice(&[0x4c, 0x8b, 0x58, 0x28]);
        // cmp r11, 0x10000: 49 81 fb 00 00 01 00
        tramp12.extend_from_slice(&[0x49, 0x81, 0xfb, 0x00, 0x00, 0x01, 0x00]);
        // jb invalid (+13 bytes): 72 0d
        tramp12.extend_from_slice(&[0x72, 0x0d]);
        // mov rcx, rbx: 48 89 d9
        tramp12.extend_from_slice(&[0x48, 0x89, 0xd9]);
        // mov r8, rbp: 49 89 e8
        tramp12.extend_from_slice(&[0x49, 0x89, 0xe8]);
        // call r11: 41 ff d3
        tramp12.extend_from_slice(&[0x41, 0xff, 0xd3]);
        // jmp done (+2 bytes): eb 02
        tramp12.extend_from_slice(&[0xeb, 0x02]);
        // invalid: xor eax, eax: 31 c0
        tramp12.extend_from_slice(&[0x31, 0xc0]);
        // done: jmp return12_rva
        let pos12 = tramp12.len();
        let rel12 = (return12_rva as isize - (tramp12_rva as isize + pos12 as isize + 5)) as i32;
        tramp12.push(0xe9);
        tramp12.extend_from_slice(&rel12.to_le_bytes());

        patch(tramp12_rva, &tramp12)?;

        let mut patch12 = [0x90u8; 12];
        let rel12_site = (tramp12_rva as isize - (site12_rva as isize + 5)) as i32;
        patch12[0] = 0xe9;
        patch12[1..5].copy_from_slice(&rel12_site.to_le_bytes());
        patch(site12_rva, &patch12)?;

        // 13. Guard virtual method call at RVA 0x258b442 in 0x258b420.
        // At 0x258b442:
        //   mov rax, [rbx]
        //   mov rcx, rbx
        //   lea r8, [rsp + 0xc0]
        //   call [rax + 0xa0]
        //   mov r14d, [rsp + 0xc4]
        // If rax < 0x10000 or [rax + 0xa0] < 0x10000, jump to 0x258b456 directly.
        let site13_rva: usize = 0x258b442;
        let tramp13_rva: usize = 0x5f40be0;
        let return13_rva: usize = 0x258b456;

        let mut tramp13 = Vec::with_capacity(48);
        // mov rax, [rbx]: 48 8b 03
        tramp13.extend_from_slice(&[0x48, 0x8b, 0x03]);
        // cmp rax, 0x10000: 48 3d 00 00 01 00
        tramp13.extend_from_slice(&[0x48, 0x3d, 0x00, 0x00, 0x01, 0x00]);
        // jb invalid (+30 bytes): 72 1e
        tramp13.extend_from_slice(&[0x72, 0x1e]);
        // mov r11, [rax + 0xa0]: 4c 8b 98 a0 00 00 00
        tramp13.extend_from_slice(&[0x4c, 0x8b, 0x98, 0xa0, 0x00, 0x00, 0x00]);
        // cmp r11, 0x10000: 49 81 fb 00 00 01 00
        tramp13.extend_from_slice(&[0x49, 0x81, 0xfb, 0x00, 0x00, 0x01, 0x00]);
        // jb invalid (+14 bytes): 72 0e
        tramp13.extend_from_slice(&[0x72, 0x0e]);
        // mov rcx, rbx: 48 89 d9
        tramp13.extend_from_slice(&[0x48, 0x89, 0xd9]);
        // lea r8, [rsp + 0xc0]: 4c 8d 84 24 c0 00 00 00
        tramp13.extend_from_slice(&[0x4c, 0x8d, 0x84, 0x24, 0xc0, 0x00, 0x00, 0x00]);
        // call r11: 41 ff d3
        tramp13.extend_from_slice(&[0x41, 0xff, 0xd3]);
        // invalid / done: jmp return13_rva
        let pos13 = tramp13.len();
        let rel13 = (return13_rva as isize - (tramp13_rva as isize + pos13 as isize + 5)) as i32;
        tramp13.push(0xe9);
        tramp13.extend_from_slice(&rel13.to_le_bytes());

        patch(tramp13_rva, &tramp13)?;

        let mut patch13 = [0x90u8; 20];
        let rel13_site = (tramp13_rva as isize - (site13_rva as isize + 5)) as i32;
        patch13[0] = 0xe9;
        patch13[1..5].copy_from_slice(&rel13_site.to_le_bytes());
        patch(site13_rva, &patch13)?;

        // 14. Guard virtual method call at RVA 0x258b79b in 0x258b780.
        // At 0x258b79b:
        //   mov rax, [rbx]
        //   mov rcx, rbx
        //   lea rdx, [rsp + 0xc0]
        //   mov r8d, r14d
        //   call [rax + 0x70]
        //   vmovups ymm0, [rsp + 0xc0]
        // If rax < 0x10000 or [rax + 0x70] < 0x10000, zero [rsp + 0xc0..0x140] and jump to 0x258b7af.
        let site14_rva: usize = 0x258b79b;
        let tramp14_rva: usize = 0x5f40c30;
        let return14_rva: usize = 0x258b7af;

        let mut tramp14 = Vec::with_capacity(96);
        // mov rax, [rbx]: 48 8b 03
        tramp14.extend_from_slice(&[0x48, 0x8b, 0x03]);
        // cmp rax, 0x10000: 48 3d 00 00 01 00
        tramp14.extend_from_slice(&[0x48, 0x3d, 0x00, 0x00, 0x01, 0x00]);
        // jb invalid (+32 bytes): 72 20
        tramp14.extend_from_slice(&[0x72, 0x20]);
        // mov r11, [rax + 0x70]: 4c 8b 58 70
        tramp14.extend_from_slice(&[0x4c, 0x8b, 0x58, 0x70]);
        // cmp r11, 0x10000: 49 81 fb 00 00 01 00
        tramp14.extend_from_slice(&[0x49, 0x81, 0xfb, 0x00, 0x00, 0x01, 0x00]);
        // jb invalid (+19 bytes): 72 13
        tramp14.extend_from_slice(&[0x72, 0x13]);
        // mov rcx, rbx: 48 89 d9
        tramp14.extend_from_slice(&[0x48, 0x89, 0xd9]);
        // lea rdx, [rsp + 0xc0]: 48 8d 94 24 c0 00 00 00
        tramp14.extend_from_slice(&[0x48, 0x8d, 0x94, 0x24, 0xc0, 0x00, 0x00, 0x00]);
        // mov r8d, r14d: 45 89 f0
        tramp14.extend_from_slice(&[0x45, 0x89, 0xf0]);
        // call r11: 41 ff d3
        tramp14.extend_from_slice(&[0x41, 0xff, 0xd3]);
        // jmp done (+40 bytes): eb 28
        tramp14.extend_from_slice(&[0xeb, 0x28]);
        // invalid: zero 128 bytes at [rsp + 0xc0]
        // vxorps xmm0, xmm0, xmm0: c5 f8 57 c0
        tramp14.extend_from_slice(&[0xc5, 0xf8, 0x57, 0xc0]);
        // vmovups [rsp + 0xc0], ymm0: c5 fc 11 84 24 c0 00 00 00
        tramp14.extend_from_slice(&[0xc5, 0xfc, 0x11, 0x84, 0x24, 0xc0, 0x00, 0x00, 0x00]);
        // vmovups [rsp + 0xe0], ymm0: c5 fc 11 84 24 e0 00 00 00
        tramp14.extend_from_slice(&[0xc5, 0xfc, 0x11, 0x84, 0x24, 0xe0, 0x00, 0x00, 0x00]);
        // vmovups [rsp + 0x100], ymm0: c5 fc 11 84 24 00 01 00 00
        tramp14.extend_from_slice(&[0xc5, 0xfc, 0x11, 0x84, 0x24, 0x00, 0x01, 0x00, 0x00]);
        // vmovups [rsp + 0x110], ymm0: c5 fc 11 84 24 10 01 00 00
        tramp14.extend_from_slice(&[0xc5, 0xfc, 0x11, 0x84, 0x24, 0x10, 0x01, 0x00, 0x00]);
        // done: jmp return14_rva
        let pos14 = tramp14.len();
        let rel14 = (return14_rva as isize - (tramp14_rva as isize + pos14 as isize + 5)) as i32;
        tramp14.push(0xe9);
        tramp14.extend_from_slice(&rel14.to_le_bytes());

        patch(tramp14_rva, &tramp14)?;

        let mut patch14 = [0x90u8; 20];
        let rel14_site = (tramp14_rva as isize - (site14_rva as isize + 5)) as i32;
        patch14[0] = 0xe9;
        patch14[1..5].copy_from_slice(&rel14_site.to_le_bytes());
        patch(site14_rva, &patch14)?;

        // 15. Guard null pointer dereference at RVA 0x2056658 in 0x2056640.
        // At 0x2056658:
        //   mov rcx, [rdi + 0x18]
        //   add rsi, 8
        //   mov rdx, rax
        //   xor r9d, r9d
        //   mov r8, [rcx + 0x18]  <-- CRASH 0xc0000005 if [rdi + 0x18] is NULL (rcx == 0)
        // If rcx < 0x10000, skip the call and jump directly to epilogue at 0x2056677.
        let site15_rva: usize = 0x2056658;
        let tramp15_rva: usize = 0x5f40c90;
        let return15_valid_rva: usize = 0x2056663;
        let return15_null_rva: usize = 0x2056677;

        let mut tramp15 = Vec::with_capacity(32);
        // mov rcx, [rdi + 0x18]: 48 8b 4f 18
        tramp15.extend_from_slice(&[0x48, 0x8b, 0x4f, 0x18]);
        // cmp rcx, 0x10000: 48 81 f9 00 00 01 00
        tramp15.extend_from_slice(&[0x48, 0x81, 0xf9, 0x00, 0x00, 0x01, 0x00]);
        // jb null_exit (+12 bytes): 72 0c
        tramp15.extend_from_slice(&[0x72, 0x0c]);
        // add rsi, 8: 48 83 c6 08
        tramp15.extend_from_slice(&[0x48, 0x83, 0xc6, 0x08]);
        // mov rdx, rax: 48 89 c2
        tramp15.extend_from_slice(&[0x48, 0x89, 0xc2]);
        // jmp return15_valid_rva
        let pos15a = tramp15.len();
        let rel15a = (return15_valid_rva as isize - (tramp15_rva as isize + pos15a as isize + 5)) as i32;
        tramp15.push(0xe9);
        tramp15.extend_from_slice(&rel15a.to_le_bytes());
        // null_exit: jmp return15_null_rva
        let pos15b = tramp15.len();
        let rel15b = (return15_null_rva as isize - (tramp15_rva as isize + pos15b as isize + 5)) as i32;
        tramp15.push(0xe9);
        tramp15.extend_from_slice(&rel15b.to_le_bytes());

        patch(tramp15_rva, &tramp15)?;

        let mut patch15 = [0x90u8; 11];
        let rel15_site = (tramp15_rva as isize - (site15_rva as isize + 5)) as i32;
        patch15[0] = 0xe9;
        patch15[1..5].copy_from_slice(&rel15_site.to_le_bytes());
        patch(site15_rva, &patch15)?;

        // 16. Guard null pointer dereference at RVA 0x19c6ee6 in 0x19c6e80.
        // At 0x19c6ee6:
        //   mov r14, rcx
        //   mov rcx, [rcx + 0x18]
        //   mov rbx, rdx
        //   mov rsi, r8
        //   dec eax
        //   mov r15, [rcx + 0x18]  <-- CRASH 0xc0000005 if [rcx + 0x18] is NULL
        // If rcx < 0x10000 or [rcx + 0x18] < 0x10000, jump to epilogue at 0x19c8f39.
        let site16_rva: usize = 0x19c6ee6;
        let tramp16_rva: usize = 0x5f40cb0;
        let return16_valid_rva: usize = 0x19c6ef5;
        let return16_null_rva: usize = 0x19c8f39;

        let mut tramp16 = Vec::with_capacity(48);
        // mov r14, rcx: 49 89 ce
        tramp16.extend_from_slice(&[0x49, 0x89, 0xce]);
        // cmp rcx, 0x10000: 48 81 f9 00 00 01 00
        tramp16.extend_from_slice(&[0x48, 0x81, 0xf9, 0x00, 0x00, 0x01, 0x00]);
        // jb null_exit (+26 bytes): 72 1a
        tramp16.extend_from_slice(&[0x72, 0x1a]);
        // mov rcx, [rcx + 0x18]: 48 8b 49 18
        tramp16.extend_from_slice(&[0x48, 0x8b, 0x49, 0x18]);
        // cmp rcx, 0x10000: 48 81 f9 00 00 01 00
        tramp16.extend_from_slice(&[0x48, 0x81, 0xf9, 0x00, 0x00, 0x01, 0x00]);
        // jb null_exit (+13 bytes): 72 0d
        tramp16.extend_from_slice(&[0x72, 0x0d]);
        // mov rbx, rdx: 48 89 d3
        tramp16.extend_from_slice(&[0x48, 0x89, 0xd3]);
        // mov rsi, r8: 4c 89 c6
        tramp16.extend_from_slice(&[0x4c, 0x89, 0xc6]);
        // dec eax: ff c8
        tramp16.extend_from_slice(&[0xff, 0xc8]);
        // jmp return16_valid_rva
        let pos16a = tramp16.len();
        let rel16a = (return16_valid_rva as isize - (tramp16_rva as isize + pos16a as isize + 5)) as i32;
        tramp16.push(0xe9);
        tramp16.extend_from_slice(&rel16a.to_le_bytes());
        // null_exit: jmp return16_null_rva
        let pos16b = tramp16.len();
        let rel16b = (return16_null_rva as isize - (tramp16_rva as isize + pos16b as isize + 5)) as i32;
        tramp16.push(0xe9);
        tramp16.extend_from_slice(&rel16b.to_le_bytes());

        patch(tramp16_rva, &tramp16)?;

        let mut patch16 = [0x90u8; 15];
        let rel16_site = (tramp16_rva as isize - (site16_rva as isize + 5)) as i32;
        patch16[0] = 0xe9;
        patch16[1..5].copy_from_slice(&rel16_site.to_le_bytes());
        patch(site16_rva, &patch16)?;

        // 17. Guard null pointer dereference at RVA 0x108550.
        // At 0x108550:
        //   push r15
        //   push r14
        //   push rsi
        //   push rdi
        //   push rbx
        //   movzx r9d, byte ptr [rcx + 8]  <-- CRASH 0xc0000005 if rcx is NULL (read 0x8)
        // If rcx < 0x10000, return immediately without touching stack or registers.
        let site17_rva: usize = 0x108550;
        let tramp17_rva: usize = 0x5f40ce0;
        let return17_valid_rva: usize = 0x108557;

        let mut tramp17 = Vec::with_capacity(32);
        // cmp rcx, 0x10000: 48 81 f9 00 00 01 00
        tramp17.extend_from_slice(&[0x48, 0x81, 0xf9, 0x00, 0x00, 0x01, 0x00]);
        // jb null_rcx (+12 bytes): 72 0c
        tramp17.extend_from_slice(&[0x72, 0x0c]);
        // push r15: 41 57
        tramp17.extend_from_slice(&[0x41, 0x57]);
        // push r14: 41 56
        tramp17.extend_from_slice(&[0x41, 0x56]);
        // push rsi: 56
        tramp17.push(0x56);
        // push rdi: 57
        tramp17.push(0x57);
        // push rbx: 53
        tramp17.push(0x53);
        // jmp return17_valid_rva
        let pos17 = tramp17.len();
        let rel17 = (return17_valid_rva as isize - (tramp17_rva as isize + pos17 as isize + 5)) as i32;
        tramp17.push(0xe9);
        tramp17.extend_from_slice(&rel17.to_le_bytes());
        // null_rcx: ret (c3)
        tramp17.push(0xc3);

        patch(tramp17_rva, &tramp17)?;

        let mut patch17 = [0x90u8; 7];
        let rel17_site = (tramp17_rva as isize - (site17_rva as isize + 5)) as i32;
        patch17[0] = 0xe9;
        patch17[1..5].copy_from_slice(&rel17_site.to_le_bytes());
        patch(site17_rva, &patch17)?;

        // 18. Guard invalid arena bitset pointer at RVA 0x550a5f6 in 0x550a5a0.
        // At 0x550a5f6:
        //   and dword ptr [rbx + rax*4 + 8], edx
        //   cmp dword ptr [rbx + 8], 0
        //   je 0x550a753
        // If chunk is empty/unallocated, rbx is a generation tag (e.g. 0x32b00000000) where lower 32 bits is 0.
        // Writing to it crashes with access violation 0xc0000005.
        // If ebx == 0 or rbx < 0x10000, jump directly to 0x550a753 (empty chunk handler).
        let site18_rva: usize = 0x550a5f6;
        let tramp18_rva: usize = 0x5f40d00;
        let return18_valid_rva: usize = 0x550a604;
        let return18_invalid_rva: usize = 0x550a753;

        let mut tramp18 = Vec::with_capacity(40);
        // test ebx, ebx: 85 db
        tramp18.extend_from_slice(&[0x85, 0xdb]);
        // jz invalid (+24 bytes): 74 18
        tramp18.extend_from_slice(&[0x74, 0x18]);
        // cmp rbx, 0x10000: 48 81 fb 00 00 01 00
        tramp18.extend_from_slice(&[0x48, 0x81, 0xfb, 0x00, 0x00, 0x01, 0x00]);
        // jb invalid (+15 bytes): 72 0f
        tramp18.extend_from_slice(&[0x72, 0x0f]);
        // and dword ptr [rbx + rax*4 + 8], edx: 21 54 83 08
        tramp18.extend_from_slice(&[0x21, 0x54, 0x83, 0x08]);
        // cmp dword ptr [rbx + 8], 0: 83 7b 08 00
        tramp18.extend_from_slice(&[0x83, 0x7b, 0x08, 0x00]);
        // je invalid (+5 bytes): 74 05
        tramp18.extend_from_slice(&[0x74, 0x05]);
        // jmp return18_valid_rva
        let pos18a = tramp18.len();
        let rel18a = (return18_valid_rva as isize - (tramp18_rva as isize + pos18a as isize + 5)) as i32;
        tramp18.push(0xe9);
        tramp18.extend_from_slice(&rel18a.to_le_bytes());
        // invalid: jmp return18_invalid_rva
        let pos18b = tramp18.len();
        let rel18b = (return18_invalid_rva as isize - (tramp18_rva as isize + pos18b as isize + 5)) as i32;
        tramp18.push(0xe9);
        tramp18.extend_from_slice(&rel18b.to_le_bytes());

        patch(tramp18_rva, &tramp18)?;

        let mut patch18 = [0x90u8; 14];
        let rel18_site = (tramp18_rva as isize - (site18_rva as isize + 5)) as i32;
        patch18[0] = 0xe9;
        patch18[1..5].copy_from_slice(&rel18_site.to_le_bytes());
        patch(site18_rva, &patch18)?;

        // 19. Guard null pointer dereferences at RVA 0x187e6ab in 0x187e630.
        // At 0x187e6ab:
        //   mov rax, qword ptr [rsi + 0x10]  (48 8b 46 10)
        //   mov rax, qword ptr [rax + 0x20]  (48 8b 40 20)  <-- CRASH 0xc0000005 if rax is NULL (read 0x20)
        //   vmovss xmm0, dword ptr [rax + 0x20]
        //   vsubss xmm0, xmm0, dword ptr [rax + 0x30]
        //   mov eax, dword ptr [rdi + 0x20]
        //   vucomiss xmm0, [float_threshold]
        //   seta r15b
        //   shl r15, 0x2d
        //   test eax, eax                    (0x187e6d0)
        // If [rsi + 0x10] < 0x10000 or [rax + 0x20] < 0x10000:
        // r15 is already 0. We load eax from [rdi + 0x20] and jump directly to 0x187e6d0.
        let site19_rva: usize = 0x187e6ab;
        let tramp19_rva: usize = 0x5f40d40;
        let return19_valid_rva: usize = 0x187e6b3;
        let return19_null_rva: usize = 0x187e6d0;

        let mut tramp19 = Vec::with_capacity(48);
        // mov rax, qword ptr [rsi + 0x10]: 48 8b 46 10
        tramp19.extend_from_slice(&[0x48, 0x8b, 0x46, 0x10]);
        // cmp rax, 0x10000: 48 81 f8 00 00 01 00
        tramp19.extend_from_slice(&[0x48, 0x81, 0xf8, 0x00, 0x00, 0x01, 0x00]);
        // jb null_path
        tramp19.push(0x72);
        let jb1_patch_pos = tramp19.len();
        tramp19.push(0x00);
        // mov rax, qword ptr [rax + 0x20]: 48 8b 40 20
        tramp19.extend_from_slice(&[0x48, 0x8b, 0x40, 0x20]);
        // cmp rax, 0x10000: 48 81 f8 00 00 01 00
        tramp19.extend_from_slice(&[0x48, 0x81, 0xf8, 0x00, 0x00, 0x01, 0x00]);
        // jb null_path
        tramp19.push(0x72);
        let jb2_patch_pos = tramp19.len();
        tramp19.push(0x00);
        // jmp return19_valid_rva (0x187e6b3)
        let pos19a = tramp19.len();
        let rel19a = (return19_valid_rva as isize - (tramp19_rva as isize + pos19a as isize + 5)) as i32;
        tramp19.push(0xe9);
        tramp19.extend_from_slice(&rel19a.to_le_bytes());
        // null_path:
        let null_path_pos = tramp19.len();
        tramp19[jb1_patch_pos] = (null_path_pos - (jb1_patch_pos + 1)) as u8;
        tramp19[jb2_patch_pos] = (null_path_pos - (jb2_patch_pos + 1)) as u8;
        // mov eax, dword ptr [rdi + 0x20]: 8b 47 20
        tramp19.extend_from_slice(&[0x8b, 0x47, 0x20]);
        // jmp return19_null_rva (0x187e6d0)
        let pos19b = tramp19.len();
        let rel19b = (return19_null_rva as isize - (tramp19_rva as isize + pos19b as isize + 5)) as i32;
        tramp19.push(0xe9);
        tramp19.extend_from_slice(&rel19b.to_le_bytes());

        patch(tramp19_rva, &tramp19)?;

        let mut patch19 = [0x90u8; 8];
        let rel19_site = (tramp19_rva as isize - (site19_rva as isize + 5)) as i32;
        patch19[0] = 0xe9;
        patch19[1..5].copy_from_slice(&rel19_site.to_le_bytes());
        patch(site19_rva, &patch19)?;

        // 20. Guard null pointer dereference at RVA 0x187ec21 in 0x187e630.
        // At 0x187ec21:
        //   mov rax, qword ptr [rsi + 0x18]  (48 8b 46 18)
        //   mov dword ptr [rsp + 0x4c], edi  (89 7c 24 4c)
        //   mov rax, qword ptr [rax + 0x18]  (48 8b 40 18)  <-- CRASH 0xc0000005 if rax is NULL (read 0x18)
        //   mov qword ptr [rsp + 0x88], rax  (48 89 84 24 88 00 00 00)
        // If rax < 0x10000 or [rax + 0x18] < 0x10000, set [rsp + 0x88] = 0.
        let site20_rva: usize = 0x187ec21;
        let tramp20_rva: usize = 0x5f40d80;
        let return20_rva: usize = 0x187ec35;

        let mut tramp20 = Vec::with_capacity(48);
        // mov rax, qword ptr [rsi + 0x18]: 48 8b 46 18
        tramp20.extend_from_slice(&[0x48, 0x8b, 0x46, 0x18]);
        // mov dword ptr [rsp + 0x4c], edi: 89 7c 24 4c
        tramp20.extend_from_slice(&[0x89, 0x7c, 0x24, 0x4c]);
        // cmp rax, 0x10000: 48 81 f8 00 00 01 00
        tramp20.extend_from_slice(&[0x48, 0x81, 0xf8, 0x00, 0x00, 0x01, 0x00]);
        // jb null_path
        tramp20.push(0x72);
        let jb20_1_pos = tramp20.len();
        tramp20.push(0x00);
        // mov rax, qword ptr [rax + 0x18]: 48 8b 40 18
        tramp20.extend_from_slice(&[0x48, 0x8b, 0x40, 0x18]);
        // cmp rax, 0x10000: 48 81 f8 00 00 01 00
        tramp20.extend_from_slice(&[0x48, 0x81, 0xf8, 0x00, 0x00, 0x01, 0x00]);
        // jb null_path
        tramp20.push(0x72);
        let jb20_2_pos = tramp20.len();
        tramp20.push(0x00);
        // mov qword ptr [rsp + 0x88], rax: 48 89 84 24 88 00 00 00
        tramp20.extend_from_slice(&[0x48, 0x89, 0x84, 0x24, 0x88, 0x00, 0x00, 0x00]);
        // jmp return20_rva
        let pos20a = tramp20.len();
        let rel20a = (return20_rva as isize - (tramp20_rva as isize + pos20a as isize + 5)) as i32;
        tramp20.push(0xe9);
        tramp20.extend_from_slice(&rel20a.to_le_bytes());
        // null_path:
        let null20_pos = tramp20.len();
        tramp20[jb20_1_pos] = (null20_pos - (jb20_1_pos + 1)) as u8;
        tramp20[jb20_2_pos] = (null20_pos - (jb20_2_pos + 1)) as u8;
        // mov qword ptr [rsp + 0x88], 0: 48 c7 84 24 88 00 00 00 00 00 00 00
        tramp20.extend_from_slice(&[0x48, 0xc7, 0x84, 0x24, 0x88, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        // jmp return20_rva
        let pos20b = tramp20.len();
        let rel20b = (return20_rva as isize - (tramp20_rva as isize + pos20b as isize + 5)) as i32;
        tramp20.push(0xe9);
        tramp20.extend_from_slice(&rel20b.to_le_bytes());

        patch(tramp20_rva, &tramp20)?;

        let mut patch20 = [0x90u8; 20];
        let rel20_site = (tramp20_rva as isize - (site20_rva as isize + 5)) as i32;
        patch20[0] = 0xe9;
        patch20[1..5].copy_from_slice(&rel20_site.to_le_bytes());
        patch(site20_rva, &patch20)?;

        // 21. Guard null input pointer at RVA 0x47dc0.
        // At 0x47dc0:
        //   push rsi (56)
        //   cmp dword ptr [rcx + 0x1c], 0 (83 79 1c 00)
        // If rcx < 0x10000, return 0 (false) immediately without dereferencing rcx.
        let site21_rva: usize = 0x47dc0;
        let tramp21_rva: usize = 0x5f40dc0;
        let return21_valid_rva: usize = 0x47dc5; // points to je 0x47e31

        let mut tramp21 = Vec::with_capacity(32);
        // cmp rcx, 0x10000: 48 81 f9 00 00 01 00
        tramp21.extend_from_slice(&[0x48, 0x81, 0xf9, 0x00, 0x00, 0x01, 0x00]);
        // jb null_rcx
        tramp21.push(0x72);
        let jb21_pos = tramp21.len();
        tramp21.push(0x00);
        // push rsi: 56
        tramp21.push(0x56);
        // cmp dword ptr [rcx + 0x1c], 0: 83 79 1c 00
        tramp21.extend_from_slice(&[0x83, 0x79, 0x1c, 0x00]);
        // jmp return21_valid_rva
        let pos21 = tramp21.len();
        let rel21 = (return21_valid_rva as isize - (tramp21_rva as isize + pos21 as isize + 5)) as i32;
        tramp21.push(0xe9);
        tramp21.extend_from_slice(&rel21.to_le_bytes());
        // null_rcx:
        let null21_pos = tramp21.len();
        tramp21[jb21_pos] = (null21_pos - (jb21_pos + 1)) as u8;
        // xor eax, eax: 31 c0
        tramp21.extend_from_slice(&[0x31, 0xc0]);
        // ret: c3
        tramp21.push(0xc3);

        patch(tramp21_rva, &tramp21)?;

        let mut patch21 = [0x90u8; 5];
        let rel21_site = (tramp21_rva as isize - (site21_rva as isize + 5)) as i32;
        patch21[0] = 0xe9;
        patch21[1..5].copy_from_slice(&rel21_site.to_le_bytes());
        patch(site21_rva, &patch21)?;

        // 22. Guard null pointer write at RVA 0x3770d98 in 0x3770d80.
        // At 0x3770d98:
        //   mov rax, qword ptr [rcx + 0x10]  (48 8b 41 10)
        //   vmovdqu [rax + 0x28], xmm0       (c5 fa 7f 40 28) <-- CRASH 0xc0000005 if rax is NULL (write 0x28)
        //   ret                              (c3)
        // If rax < 0x10000, do not write; return immediately.
        let site22_rva: usize = 0x3770d98;
        let tramp22_rva: usize = 0x5f40e00;

        let mut tramp22 = Vec::with_capacity(32);
        // mov rax, qword ptr [rcx + 0x10]: 48 8b 41 10
        tramp22.extend_from_slice(&[0x48, 0x8b, 0x41, 0x10]);
        // cmp rax, 0x10000: 48 81 f8 00 00 01 00
        tramp22.extend_from_slice(&[0x48, 0x81, 0xf8, 0x00, 0x00, 0x01, 0x00]);
        // jb null_rax
        tramp22.push(0x72);
        let jb22_pos = tramp22.len();
        tramp22.push(0x00);
        // vmovdqu [rax + 0x28], xmm0: c5 fa 7f 40 28
        tramp22.extend_from_slice(&[0xc5, 0xfa, 0x7f, 0x40, 0x28]);
        // null_rax:
        let null22_pos = tramp22.len();
        tramp22[jb22_pos] = (null22_pos - (jb22_pos + 1)) as u8;
        // ret: c3
        tramp22.push(0xc3);

        patch(tramp22_rva, &tramp22)?;

        let mut patch22 = [0x90u8; 10];
        let rel22_site = (tramp22_rva as isize - (site22_rva as isize + 5)) as i32;
        patch22[0] = 0xe9;
        patch22[1..5].copy_from_slice(&rel22_site.to_le_bytes());
        patch(site22_rva, &patch22)?;

        // 23. Guard null pointer dereferences at RVA 0x3770db9 in 0x3770db0.
        // At 0x3770db9:
        //   mov rax, qword ptr [rcx + 0x18]  (48 8b 41 18)
        //   mov r14, rcx                     (49 89 ce)
        //   mov rcx, qword ptr [rax + 0x18]  (48 8b 48 18)  <-- CRASH if rax or rcx is NULL
        // If rcx, rax, or [rax + 0x18] < 0x10000, jump to epilogue at 0x3770fd0.
        let site23_rva: usize = 0x3770db9;
        let tramp23_rva: usize = 0x5f40e30;
        let return23_valid_rva: usize = 0x3770dc4;
        let return23_null_rva: usize = 0x3770fd0;

        let mut tramp23 = Vec::with_capacity(48);
        // cmp rcx, 0x10000: 48 81 f9 00 00 01 00
        tramp23.extend_from_slice(&[0x48, 0x81, 0xf9, 0x00, 0x00, 0x01, 0x00]);
        // jb null_exit
        tramp23.push(0x72);
        let jb23_1_pos = tramp23.len();
        tramp23.push(0x00);
        // mov rax, qword ptr [rcx + 0x18]: 48 8b 41 18
        tramp23.extend_from_slice(&[0x48, 0x8b, 0x41, 0x18]);
        // mov r14, rcx: 49 89 ce
        tramp23.extend_from_slice(&[0x49, 0x89, 0xce]);
        // cmp rax, 0x10000: 48 81 f8 00 00 01 00
        tramp23.extend_from_slice(&[0x48, 0x81, 0xf8, 0x00, 0x00, 0x01, 0x00]);
        // jb null_exit
        tramp23.push(0x72);
        let jb23_2_pos = tramp23.len();
        tramp23.push(0x00);
        // mov rcx, qword ptr [rax + 0x18]: 48 8b 48 18
        tramp23.extend_from_slice(&[0x48, 0x8b, 0x48, 0x18]);
        // cmp rcx, 0x10000: 48 81 f9 00 00 01 00
        tramp23.extend_from_slice(&[0x48, 0x81, 0xf9, 0x00, 0x00, 0x01, 0x00]);
        // jb null_exit
        tramp23.push(0x72);
        let jb23_3_pos = tramp23.len();
        tramp23.push(0x00);
        // jmp return23_valid_rva
        let pos23a = tramp23.len();
        let rel23a = (return23_valid_rva as isize - (tramp23_rva as isize + pos23a as isize + 5)) as i32;
        tramp23.push(0xe9);
        tramp23.extend_from_slice(&rel23a.to_le_bytes());
        // null_exit:
        let null23_pos = tramp23.len();
        tramp23[jb23_1_pos] = (null23_pos - (jb23_1_pos + 1)) as u8;
        tramp23[jb23_2_pos] = (null23_pos - (jb23_2_pos + 1)) as u8;
        tramp23[jb23_3_pos] = (null23_pos - (jb23_3_pos + 1)) as u8;
        // jmp return23_null_rva
        let pos23b = tramp23.len();
        let rel23b = (return23_null_rva as isize - (tramp23_rva as isize + pos23b as isize + 5)) as i32;
        tramp23.push(0xe9);
        tramp23.extend_from_slice(&rel23b.to_le_bytes());

        patch(tramp23_rva, &tramp23)?;

        let mut patch23 = [0x90u8; 11];
        let rel23_site = (tramp23_rva as isize - (site23_rva as isize + 5)) as i32;
        patch23[0] = 0xe9;
        patch23[1..5].copy_from_slice(&rel23_site.to_le_bytes());
        patch(site23_rva, &patch23)?;

        // 24. Guard null rbx at RVA 0x1427c69 in 0x1427000.
        // At 0x1427c69:
        //   cmpl $3, eax                     (83 f8 03)
        //   jb 0x1427e18                     (0f 82 a6 01 00 00)
        //   movq 0x20(%rbx), %rax            (0x1427c72) <-- CRASH if rbx is NULL
        // If eax < 3 OR rbx < 0x10000 OR [rbx + 0x20] < 0x10000:
        // Jump directly to 0x1427e18.
        let site24_rva: usize = 0x1427c69;
        let tramp24_rva: usize = 0x5f40e80;
        let return24_valid_rva: usize = 0x1427c72;
        let return24_skip_rva: usize = 0x1427e18;

        let mut tramp24 = Vec::with_capacity(48);
        // cmp eax, 3: 83 f8 03
        tramp24.extend_from_slice(&[0x83, 0xf8, 0x03]);
        // jb skip
        tramp24.push(0x72);
        let jb24_1_pos = tramp24.len();
        tramp24.push(0x00);
        // cmp rbx, 0x10000: 48 81 fb 00 00 01 00
        tramp24.extend_from_slice(&[0x48, 0x81, 0xfb, 0x00, 0x00, 0x01, 0x00]);
        // jb skip
        tramp24.push(0x72);
        let jb24_2_pos = tramp24.len();
        tramp24.push(0x00);
        // mov rax, qword ptr [rbx + 0x20]: 48 8b 43 20
        tramp24.extend_from_slice(&[0x48, 0x8b, 0x43, 0x20]);
        // cmp rax, 0x10000: 48 81 f8 00 00 01 00
        tramp24.extend_from_slice(&[0x48, 0x81, 0xf8, 0x00, 0x00, 0x01, 0x00]);
        // jb skip
        tramp24.push(0x72);
        let jb24_3_pos = tramp24.len();
        tramp24.push(0x00);
        // jmp return24_valid_rva
        let pos24a = tramp24.len();
        let rel24a = (return24_valid_rva as isize - (tramp24_rva as isize + pos24a as isize + 5)) as i32;
        tramp24.push(0xe9);
        tramp24.extend_from_slice(&rel24a.to_le_bytes());
        // skip:
        let skip24_pos = tramp24.len();
        tramp24[jb24_1_pos] = (skip24_pos - (jb24_1_pos + 1)) as u8;
        tramp24[jb24_2_pos] = (skip24_pos - (jb24_2_pos + 1)) as u8;
        tramp24[jb24_3_pos] = (skip24_pos - (jb24_3_pos + 1)) as u8;
        // jmp return24_skip_rva
        let pos24b = tramp24.len();
        let rel24b = (return24_skip_rva as isize - (tramp24_rva as isize + pos24b as isize + 5)) as i32;
        tramp24.push(0xe9);
        tramp24.extend_from_slice(&rel24b.to_le_bytes());

        patch(tramp24_rva, &tramp24)?;

        let mut patch24 = [0x90u8; 9];
        let rel24_site = (tramp24_rva as isize - (site24_rva as isize + 5)) as i32;
        patch24[0] = 0xe9;
        patch24[1..5].copy_from_slice(&rel24_site.to_le_bytes());
        patch(site24_rva, &patch24)?;

        // 25. Guard null rbx at RVA 0x1427e23 in 0x1427000.
        // At 0x1427e23:
        //   movq 0x20(%rbx), %rax
        //   vmovss 0x4d3215d(%rip), %xmm1
        //   vpxor %xmm2, %xmm2, %xmm2
        // If rbx < 0x10000 or [rbx + 0x20] < 0x10000:
        // set [rdi + 0x220] = 0 and jump to 0x1427e63.
        let site25_rva: usize = 0x1427e23;
        let tramp25_rva: usize = 0x5f40ed0;
        let return25_valid_rva: usize = 0x1427e33;
        let return25_skip_rva: usize = 0x1427e63;
        let float_addr = base + 0x6159f8c;

        let mut tramp25 = Vec::with_capacity(64);
        // cmp rbx, 0x10000: 48 81 fb 00 00 01 00
        tramp25.extend_from_slice(&[0x48, 0x81, 0xfb, 0x00, 0x00, 0x01, 0x00]);
        // jb null_b
        tramp25.push(0x72);
        let jb25_1_pos = tramp25.len();
        tramp25.push(0x00);
        // mov rax, qword ptr [rbx + 0x20]: 48 8b 43 20
        tramp25.extend_from_slice(&[0x48, 0x8b, 0x43, 0x20]);
        // cmp rax, 0x10000: 48 81 f8 00 00 01 00
        tramp25.extend_from_slice(&[0x48, 0x81, 0xf8, 0x00, 0x00, 0x01, 0x00]);
        // jb null_b
        tramp25.push(0x72);
        let jb25_2_pos = tramp25.len();
        tramp25.push(0x00);
        // valid:
        // mov r10, float_addr: 49 ba imm64
        tramp25.extend_from_slice(&[0x49, 0xba]);
        tramp25.extend_from_slice(&(float_addr as u64).to_le_bytes());
        // vmovss xmm1, dword ptr [r10]: c4 c1 7a 10 0a
        tramp25.extend_from_slice(&[0xc4, 0xc1, 0x7a, 0x10, 0x0a]);
        // vpxor xmm2, xmm2, xmm2: c5 e9 ef d2
        tramp25.extend_from_slice(&[0xc5, 0xe9, 0xef, 0xd2]);
        // jmp return25_valid_rva
        let pos25a = tramp25.len();
        let rel25a = (return25_valid_rva as isize - (tramp25_rva as isize + pos25a as isize + 5)) as i32;
        tramp25.push(0xe9);
        tramp25.extend_from_slice(&rel25a.to_le_bytes());
        // null_b:
        let null25_pos = tramp25.len();
        tramp25[jb25_1_pos] = (null25_pos - (jb25_1_pos + 1)) as u8;
        tramp25[jb25_2_pos] = (null25_pos - (jb25_2_pos + 1)) as u8;
        // mov dword ptr [rdi + 0x220], 0: c7 87 20 02 00 00 00 00 00 00
        tramp25.extend_from_slice(&[0xc7, 0x87, 0x20, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        // jmp return25_skip_rva
        let pos25b = tramp25.len();
        let rel25b = (return25_skip_rva as isize - (tramp25_rva as isize + pos25b as isize + 5)) as i32;
        tramp25.push(0xe9);
        tramp25.extend_from_slice(&rel25b.to_le_bytes());

        patch(tramp25_rva, &tramp25)?;

        let mut patch25 = [0x90u8; 16];
        let rel25_site = (tramp25_rva as isize - (site25_rva as isize + 5)) as i32;
        patch25[0] = 0xe9;
        patch25[1..5].copy_from_slice(&rel25_site.to_le_bytes());
        patch(site25_rva, &patch25)?;

        // 26. Guard null r14 in asset streaming loop at RVA 0x4f28df1 in 0x4f28dd0.
        // Original code at 0x4f28df1 (15 bytes):
        //   0x4f28df1: mov r14, [rax+0x10] (4 bytes: 4c 8b 70 10)
        //   0x4f28df5: jmp 0x4f28e09 (2 bytes: eb 12)
        //   0x4f28df7: nop word [rax+rax] (9 bytes: 66 0f 1f 84 00 00 00 00 00)
        // If r14 < 0x10000, accessing [r14+0x18] at 0x4f28e0f crashes with 0xc0000005.
        // We replace in-place (exactly 15 bytes):
        //   mov r14, [rax+0x10] (4 bytes: 4c 8b 70 10)
        //   cmp r14, 0x10000    (7 bytes: 49 81 fe 00 00 01 00)
        //   jb 0x4f28e6a        (2 bytes: 72 6c) -> jump to epilogue
        //   jmp 0x4f28e09       (2 bytes: eb 09) -> continue into loop
        let patch26_bytes: [u8; 15] = [
            0x4c, 0x8b, 0x70, 0x10,
            0x49, 0x81, 0xfe, 0x00, 0x00, 0x01, 0x00,
            0x72, 0x6c,
            0xeb, 0x09,
        ];
        patch(0x4f28df1, &patch26_bytes)?;

        // 27. Guard null [rdi + 0x18] (rax) and [rax + 0x18] (rdx) in entity component resolution at RVA 0x55185b0.
        // Original code at 0x55185b0 (13 bytes):
        //   0x55185b0: mov rax, [rdi + 0x18]        (4 bytes: 48 8b 47 18)
        //   0x55185b4: mov rdx, [rax + 0x18]        (4 bytes: 48 8b 50 18) - CRASH 0xc0000005 if rax == 0
        //   0x55185b8: movzx r8d, word [rdx + 0x6c] (5 bytes: 44 0f b7 42 6c)
        // If rax or rdx is null/invalid, skip the element processing loop and jump to 0x5518604.
        let site27_rva: usize = 0x55185b0;
        let tramp27_rva: usize = 0x5f40f20;
        let return27_valid_rva: usize = 0x55185bd;
        let return27_skip_rva: usize = 0x5518604;

        let mut tramp27 = Vec::with_capacity(48);
        // mov rax, [rdi + 0x18]: 48 8b 47 18
        tramp27.extend_from_slice(&[0x48, 0x8b, 0x47, 0x18]);
        // cmp rax, 0x10000: 48 81 f8 00 00 01 00
        tramp27.extend_from_slice(&[0x48, 0x81, 0xf8, 0x00, 0x00, 0x01, 0x00]);
        // jb skip
        tramp27.push(0x72);
        let jb27_1_pos = tramp27.len();
        tramp27.push(0x00);
        // mov rdx, [rax + 0x18]: 48 8b 50 18
        tramp27.extend_from_slice(&[0x48, 0x8b, 0x50, 0x18]);
        // cmp rdx, 0x10000: 48 81 fa 00 00 01 00
        tramp27.extend_from_slice(&[0x48, 0x81, 0xfa, 0x00, 0x00, 0x01, 0x00]);
        // jb skip
        tramp27.push(0x72);
        let jb27_2_pos = tramp27.len();
        tramp27.push(0x00);
        // movzx r8d, word ptr [rdx + 0x6c]: 44 0f b7 42 6c
        tramp27.extend_from_slice(&[0x44, 0x0f, 0xb7, 0x42, 0x6c]);
        // jmp return27_valid_rva
        let pos27a = tramp27.len();
        let rel27a = (return27_valid_rva as isize - (tramp27_rva as isize + pos27a as isize + 5)) as i32;
        tramp27.push(0xe9);
        tramp27.extend_from_slice(&rel27a.to_le_bytes());
        // skip:
        let skip27_pos = tramp27.len();
        tramp27[jb27_1_pos] = (skip27_pos - (jb27_1_pos + 1)) as u8;
        tramp27[jb27_2_pos] = (skip27_pos - (jb27_2_pos + 1)) as u8;
        // jmp return27_skip_rva
        let pos27b = tramp27.len();
        let rel27b = (return27_skip_rva as isize - (tramp27_rva as isize + pos27b as isize + 5)) as i32;
        tramp27.push(0xe9);
        tramp27.extend_from_slice(&rel27b.to_le_bytes());

        patch(tramp27_rva, &tramp27)?;

        let mut patch27 = [0x90u8; 13];
        let rel27_site = (tramp27_rva as isize - (site27_rva as isize + 5)) as i32;
        patch27[0] = 0xe9;
        patch27[1..5].copy_from_slice(&rel27_site.to_le_bytes());
        patch(site27_rva, &patch27)?;

        // 28. Guard null rcx in chunk entity pointer traversal at RVA 0xe43180 in 0xe43170.
        // Original code at 0xe43180 (13 bytes):
        //   0xe43180: mov rcx, [rcx + 0x18] (4 bytes: 48 8b 49 18)
        //   0xe43184: mov rcx, [rcx + 0x18] (4 bytes: 48 8b 49 18) - CRASH 0xc0000005 if previous rcx was 0
        //   0xe43188: test rcx, rcx        (3 bytes: 48 85 c9)
        //   0xe4318b: jz 0xe4319e           (2 bytes: 74 11) -> return 0xffffffff
        // If rcx is null at any step, cleanly branch to 0xe4319e to return 0xffffffff (-1).
        let site28_rva: usize = 0x00e43180;
        let tramp28_rva: usize = 0x5f40f60;
        let return28_valid_rva: usize = 0x00e4318d;
        let return28_skip_rva: usize = 0x00e4319e;

        let mut tramp28 = Vec::with_capacity(48);
        // cmp rcx, 0x10000: 48 81 f9 00 00 01 00
        tramp28.extend_from_slice(&[0x48, 0x81, 0xf9, 0x00, 0x00, 0x01, 0x00]);
        // jb null_exit
        tramp28.push(0x72);
        let jb28_1_pos = tramp28.len();
        tramp28.push(0x00);
        // mov rcx, [rcx + 0x18]: 48 8b 49 18
        tramp28.extend_from_slice(&[0x48, 0x8b, 0x49, 0x18]);
        // cmp rcx, 0x10000: 48 81 f9 00 00 01 00
        tramp28.extend_from_slice(&[0x48, 0x81, 0xf9, 0x00, 0x00, 0x01, 0x00]);
        // jb null_exit
        tramp28.push(0x72);
        let jb28_2_pos = tramp28.len();
        tramp28.push(0x00);
        // mov rcx, [rcx + 0x18]: 48 8b 49 18
        tramp28.extend_from_slice(&[0x48, 0x8b, 0x49, 0x18]);
        // cmp rcx, 0x10000: 48 81 f9 00 00 01 00
        tramp28.extend_from_slice(&[0x48, 0x81, 0xf9, 0x00, 0x00, 0x01, 0x00]);
        // jb null_exit
        tramp28.push(0x72);
        let jb28_3_pos = tramp28.len();
        tramp28.push(0x00);
        // jmp return28_valid_rva
        let pos28a = tramp28.len();
        let rel28a = (return28_valid_rva as isize - (tramp28_rva as isize + pos28a as isize + 5)) as i32;
        tramp28.push(0xe9);
        tramp28.extend_from_slice(&rel28a.to_le_bytes());
        // null_exit:
        let null28_pos = tramp28.len();
        tramp28[jb28_1_pos] = (null28_pos - (jb28_1_pos + 1)) as u8;
        tramp28[jb28_2_pos] = (null28_pos - (jb28_2_pos + 1)) as u8;
        tramp28[jb28_3_pos] = (null28_pos - (jb28_3_pos + 1)) as u8;
        // jmp return28_skip_rva
        let pos28b = tramp28.len();
        let rel28b = (return28_skip_rva as isize - (tramp28_rva as isize + pos28b as isize + 5)) as i32;
        tramp28.push(0xe9);
        tramp28.extend_from_slice(&rel28b.to_le_bytes());

        patch(tramp28_rva, &tramp28)?;

        let mut patch28 = [0x90u8; 13];
        let rel28_site = (tramp28_rva as isize - (site28_rva as isize + 5)) as i32;
        patch28[0] = 0xe9;
        patch28[1..5].copy_from_slice(&rel28_site.to_le_bytes());
        patch(site28_rva, &patch28)?;

        // 29. Guard uninitialized / non-canonical pgRef pointer in 0x4ec4f70 (caller of 0x4ec4f80).
        // Original code at 0x4ec4f70 (16 bytes including 6 int3 padding bytes):
        //   0x4ec4f70: test rcx, rcx (3 bytes: 48 85 c9)
        //   0x4ec4f73: jnz 0x4ec4f80 (6 bytes: 0f 85 07 00 00 00)
        //   0x4ec4f79: ret           (1 byte: c3)
        //   0x4ec4f7a: int3...       (6 bytes: cc * 6)
        // When rcx is an unresolved token (<= 0x100000000, 0x1926be00000000bc, or 0xffffffffffffffff),
        // dereferencing in 0x4ec4f80 crashes at 0x4da62a6.
        // We use a trampoline in .text padding at 0x55ff284 with canonical pointer validation:
        //   mov r10, rcx
        //   shr r10, 47
        //   jnz invalid
        //   mov r10, 0x100000000
        //   cmp rcx, r10
        //   jbe invalid
        //   jmp return_valid (0x4ec4f80)
        // invalid:
        //   ret
        let site29_rva: usize = 0x4ec4f70;
        let tramp29_rva: usize = 0x55ff284;
        let return29_valid_rva: usize = 0x4ec4f80;

        let mut tramp29 = Vec::with_capacity(36);
        // mov r10, rcx: 49 89 ca
        tramp29.extend_from_slice(&[0x49, 0x89, 0xca]);
        // shr r10, 47: 49 c1 ea 2f
        tramp29.extend_from_slice(&[0x49, 0xc1, 0xea, 0x2f]);
        // jnz invalid
        tramp29.push(0x75);
        let jnz29_pos = tramp29.len();
        tramp29.push(0x00);
        // mov r10, 0x100000000: 49 ba 00 00 00 00 01 00 00 00
        tramp29.extend_from_slice(&[0x49, 0xba, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
        // cmp rcx, r10: 4c 39 d1
        tramp29.extend_from_slice(&[0x4c, 0x39, 0xd1]);
        // jbe invalid
        tramp29.push(0x76);
        let jbe29_pos = tramp29.len();
        tramp29.push(0x00);
        // jmp return29_valid_rva (e9 rel32)
        let pos29_valid = tramp29.len();
        let rel29_valid = (return29_valid_rva as isize - (tramp29_rva as isize + pos29_valid as isize + 5)) as i32;
        tramp29.push(0xe9);
        tramp29.extend_from_slice(&rel29_valid.to_le_bytes());
        // invalid:
        let inv29_pos = tramp29.len();
        tramp29[jnz29_pos] = (inv29_pos - (jnz29_pos + 1)) as u8;
        tramp29[jbe29_pos] = (inv29_pos - (jbe29_pos + 1)) as u8;
        tramp29.push(0xc3); // ret

        patch(tramp29_rva, &tramp29)?;

        let mut patch29 = [0x90u8; 16];
        let rel29_site = (tramp29_rva as isize - (site29_rva as isize + 5)) as i32;
        patch29[0] = 0xe9;
        patch29[1..5].copy_from_slice(&rel29_site.to_le_bytes());
        patch(site29_rva, &patch29)?;

        // 30. Guard null streaming manager array dereference in 0x52ec310 at 0x52ec33d.
        // Original code at 0x52ec339 (13 bytes):
        //   0x52ec339: mov rax, [rax + 0x10]      (4 bytes: 48 8b 40 10)
        //   0x52ec33d: movzx edi, word [rax+0x18] (4 bytes: 0f b7 78 18) - CRASH if [rax+0x10] is null
        //   0x52ec341: test rdi, rdi              (3 bytes: 48 85 ff)
        //   0x52ec344: jz 0x52ec379               (2 bytes: 74 33)
        // If rax < 0x10000 after mov rax, [rax + 0x10], jump directly to epilogue at 0x52ec379.
        let site30_rva: usize = 0x52ec339;
        let tramp30_rva: usize = 0x5f40fa0;
        let return30_valid_rva: usize = 0x52ec346;
        let return30_skip_rva: usize = 0x52ec379;

        let mut tramp30 = Vec::with_capacity(36);
        // mov rax, [rax + 0x10]: 48 8b 40 10
        tramp30.extend_from_slice(&[0x48, 0x8b, 0x40, 0x10]);
        // cmp rax, 0x10000: 48 81 f8 00 00 01 00
        tramp30.extend_from_slice(&[0x48, 0x81, 0xf8, 0x00, 0x00, 0x01, 0x00]);
        // jb skip
        tramp30.push(0x72);
        let jb30_pos = tramp30.len();
        tramp30.push(0x00);
        // movzx edi, word [rax + 0x18]: 0f b7 78 18
        tramp30.extend_from_slice(&[0x0f, 0xb7, 0x78, 0x18]);
        // test rdi, rdi: 48 85 ff
        tramp30.extend_from_slice(&[0x48, 0x85, 0xff]);
        // jz skip
        tramp30.push(0x74);
        let jz30_pos = tramp30.len();
        tramp30.push(0x00);
        // jmp return30_valid_rva (e9 rel32)
        let pos30_valid = tramp30.len();
        let rel30_valid = (return30_valid_rva as isize - (tramp30_rva as isize + pos30_valid as isize + 5)) as i32;
        tramp30.push(0xe9);
        tramp30.extend_from_slice(&rel30_valid.to_le_bytes());
        // skip:
        let skip30_pos = tramp30.len();
        tramp30[jb30_pos] = (skip30_pos - (jb30_pos + 1)) as u8;
        tramp30[jz30_pos] = (skip30_pos - (jz30_pos + 1)) as u8;
        // jmp return30_skip_rva (e9 rel32)
        let pos30_skip = tramp30.len();
        let rel30_skip = (return30_skip_rva as isize - (tramp30_rva as isize + pos30_skip as isize + 5)) as i32;
        tramp30.push(0xe9);
        tramp30.extend_from_slice(&rel30_skip.to_le_bytes());

        patch(tramp30_rva, &tramp30)?;

        let mut patch30 = [0x90u8; 13];
        let rel30_site = (tramp30_rva as isize - (site30_rva as isize + 5)) as i32;
        patch30[0] = 0xe9;
        patch30[1..5].copy_from_slice(&rel30_site.to_le_bytes());
        patch(site30_rva, &patch30)?;

        // 31. Guard unresolvable / non-canonical pgRef pointer in 0xad0800 (caller of 0xad0810 / 0x4da6330).
        // Original code at 0xad0800 (16 bytes including 6 int3 padding bytes):
        //   0xad0800: test rcx, rcx (3 bytes: 48 85 c9)
        //   0xad0803: jnz 0xad0810  (6 bytes: 0f 85 07 00 00 00)
        //   0xad0809: ret           (1 byte: c3)
        //   0xad080a: int3...       (6 bytes: cc * 6)
        // When rcx is an unresolved token (e.g. <= 0x100000000, 0x1926be00000000bc, or 0xffffffffffffffff),
        // dereferencing in 0x4da6330 crashes at 0x4da6341 (mov [rax], rdx with non-canonical rax).
        // Trampoline in .text padding at 0x5f40fe0 with canonical pointer validation:
        //   mov r10, rcx
        //   shr r10, 47
        //   jnz invalid
        //   mov r10, 0x100000000
        //   cmp rcx, r10
        //   jbe invalid
        //   jmp return_valid (0xad0810)
        // invalid:
        //   ret
        let site31_rva: usize = 0xad0800;
        let tramp31_rva: usize = 0x5f40fe0;
        let return31_valid_rva: usize = 0xad0810;

        let mut tramp31 = Vec::with_capacity(36);
        // mov r10, rcx: 49 89 ca
        tramp31.extend_from_slice(&[0x49, 0x89, 0xca]);
        // shr r10, 47: 49 c1 ea 2f
        tramp31.extend_from_slice(&[0x49, 0xc1, 0xea, 0x2f]);
        // jnz invalid
        tramp31.push(0x75);
        let jnz31_pos = tramp31.len();
        tramp31.push(0x00);
        // mov r10, 0x100000000: 49 ba 00 00 00 00 01 00 00 00
        tramp31.extend_from_slice(&[0x49, 0xba, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
        // cmp rcx, r10: 4c 39 d1
        tramp31.extend_from_slice(&[0x4c, 0x39, 0xd1]);
        // jbe invalid
        tramp31.push(0x76);
        let jbe31_pos = tramp31.len();
        tramp31.push(0x00);
        // jmp return31_valid_rva (e9 rel32)
        let pos31_valid = tramp31.len();
        let rel31_valid = (return31_valid_rva as isize - (tramp31_rva as isize + pos31_valid as isize + 5)) as i32;
        tramp31.push(0xe9);
        tramp31.extend_from_slice(&rel31_valid.to_le_bytes());
        // invalid:
        let inv31_pos = tramp31.len();
        tramp31[jnz31_pos] = (inv31_pos - (jnz31_pos + 1)) as u8;
        tramp31[jbe31_pos] = (inv31_pos - (jbe31_pos + 1)) as u8;
        tramp31.push(0xc3); // ret

        patch(tramp31_rva, &tramp31)?;

        let mut patch31 = [0x90u8; 16];
        let rel31_site = (tramp31_rva as isize - (site31_rva as isize + 5)) as i32;
        patch31[0] = 0xe9;
        patch31[1..5].copy_from_slice(&rel31_site.to_le_bytes());
        patch(site31_rva, &patch31)?;

        // 32. Guard world asset streaming null chunk container and missing chunk array crashes.
        // During streaming, uninitialized/empty assets have null chunk array descriptors [chunk_container + 0x10].
        // In caller 0x5231.. (at 0x5231c4) and 0x1427500 (at 0x142750e), the engine executes 6 post-load methods
        // (0x52ec310, 0x52ed610, 0x52ec390, 0x52ec650, 0x52ec290, 0x52ec210) that dereference [chunk_container + 0x10]
        // before line 0x5231f4 natively checks `mov rax, 0x10(%rdi); test rax, rax; je 0x523330`.
        // Furthermore, 0x52ed610 has a logic inversion bug at 0x52ed664: if rdi == 0, it jumps to 0x52ed887,
        // passing rcx = 0 to 0x52ed900 which dereferences [0x0 + 0x18] at 0x52ed909.
        //
        // Comprehensive multi-layer defense:
        // A. In-place fix at 0x52ed664: if rdi == 0, jump to epilogue at 0x52ed893 instead of 0x52ed887.
        // B. Guard 0x52ed900 at entry via trampoline at 0x55e824c: if rcx < 0x10000, return xor eax, eax ; ret.
        // C. Parent-level guard at 0x5231c4 via trampoline at 0x55ffd07: if [rdi + 0x10] < 0x10000, jump directly to 0x5231f4.
        // D. Parent-level guard at 0x142750e via trampoline at 0x55fddd4: if [rbx + 0x10] < 0x10000, jump directly to 0x142753e.

        // A. In-place fix at 0x52ed664 (6 bytes: je 0x52ed893 instead of je 0x52ed887)
        let site32a_rva: usize = 0x52ed664;
        let target32a_rva: usize = 0x52ed893;
        let rel32a = (target32a_rva as isize - (site32a_rva as isize + 6)) as i32;
        let mut patch32a = [0x0fu8, 0x84, 0x00, 0x00, 0x00, 0x00];
        patch32a[2..6].copy_from_slice(&rel32a.to_le_bytes());
        patch(site32a_rva, &patch32a)?;

        // B. Guard 0x52ed900 entry via trampoline at 0x55e824c
        let site32b_rva: usize = 0x52ed900;
        let tramp32b_rva: usize = 0x55e824c;
        let return32b_valid_rva: usize = 0x52ed909;

        let mut tramp32b = Vec::with_capacity(32);
        // cmp rcx, 0x10000: 48 81 f9 00 00 01 00
        tramp32b.extend_from_slice(&[0x48, 0x81, 0xf9, 0x00, 0x00, 0x01, 0x00]);
        // jb invalid
        tramp32b.push(0x72);
        let jb32b_pos = tramp32b.len();
        tramp32b.push(0x00);
        // valid: push r14, push rsi, push rdi, push rbx, sub rsp, 0x28
        tramp32b.extend_from_slice(&[0x41, 0x56, 0x56, 0x57, 0x53, 0x48, 0x83, 0xec, 0x28]);
        // jmp return32b_valid_rva (e9 rel32)
        let pos32b_valid = tramp32b.len();
        let rel32b_valid = (return32b_valid_rva as isize - (tramp32b_rva as isize + pos32b_valid as isize + 5)) as i32;
        tramp32b.push(0xe9);
        tramp32b.extend_from_slice(&rel32b_valid.to_le_bytes());
        // invalid:
        let inv32b_pos = tramp32b.len();
        tramp32b[jb32b_pos] = (inv32b_pos - (jb32b_pos + 1)) as u8;
        // xor eax, eax ; ret
        tramp32b.extend_from_slice(&[0x31, 0xc0, 0xc3]);

        patch(tramp32b_rva, &tramp32b)?;

        let mut patch32b = [0x90u8; 9];
        let rel32b_site = (tramp32b_rva as isize - (site32b_rva as isize + 5)) as i32;
        patch32b[0] = 0xe9;
        patch32b[1..5].copy_from_slice(&rel32b_site.to_le_bytes());
        patch(site32b_rva, &patch32b)?;

        // C. Parent guard at 0x5231c4 via trampoline at 0x55ffd07
        let site32c_rva: usize = 0x5231c4;
        let tramp32c_rva: usize = 0x55ffd07;
        let method1_rva: usize = 0x52ec310;
        let return32c_valid_rva: usize = 0x5231cc;
        let return32c_skip_rva: usize = 0x5231f4;

        let mut tramp32c = Vec::with_capacity(40);
        // cmp rdi, 0x10000: 48 81 ff 00 00 01 00
        tramp32c.extend_from_slice(&[0x48, 0x81, 0xff, 0x00, 0x00, 0x01, 0x00]);
        // jb skip
        tramp32c.push(0x72);
        let jb32c_pos1 = tramp32c.len();
        tramp32c.push(0x00);
        // cmp qword ptr [rdi + 0x10], 0x10000: 48 81 7f 10 00 00 01 00
        tramp32c.extend_from_slice(&[0x48, 0x81, 0x7f, 0x10, 0x00, 0x00, 0x01, 0x00]);
        // jb skip
        tramp32c.push(0x72);
        let jb32c_pos2 = tramp32c.len();
        tramp32c.push(0x00);
        // mov rcx, rsi: 48 89 f1
        tramp32c.extend_from_slice(&[0x48, 0x89, 0xf1]);
        // call method1_rva: e8 rel32
        let pos32c_call = tramp32c.len();
        let rel32c_call = (method1_rva as isize - (tramp32c_rva as isize + pos32c_call as isize + 5)) as i32;
        tramp32c.push(0xe8);
        tramp32c.extend_from_slice(&rel32c_call.to_le_bytes());
        // jmp return32c_valid_rva: e9 rel32
        let pos32c_valid = tramp32c.len();
        let rel32c_valid = (return32c_valid_rva as isize - (tramp32c_rva as isize + pos32c_valid as isize + 5)) as i32;
        tramp32c.push(0xe9);
        tramp32c.extend_from_slice(&rel32c_valid.to_le_bytes());
        // skip:
        let skip32c_pos = tramp32c.len();
        tramp32c[jb32c_pos1] = (skip32c_pos - (jb32c_pos1 + 1)) as u8;
        tramp32c[jb32c_pos2] = (skip32c_pos - (jb32c_pos2 + 1)) as u8;
        // jmp return32c_skip_rva: e9 rel32
        let pos32c_skip = tramp32c.len();
        let rel32c_skip = (return32c_skip_rva as isize - (tramp32c_rva as isize + pos32c_skip as isize + 5)) as i32;
        tramp32c.push(0xe9);
        tramp32c.extend_from_slice(&rel32c_skip.to_le_bytes());

        patch(tramp32c_rva, &tramp32c)?;

        let mut patch32c = [0x90u8; 8];
        let rel32c_site = (tramp32c_rva as isize - (site32c_rva as isize + 5)) as i32;
        patch32c[0] = 0xe9;
        patch32c[1..5].copy_from_slice(&rel32c_site.to_le_bytes());
        patch(site32c_rva, &patch32c)?;

        // D. Parent guard at 0x142750e via trampoline at 0x55fddd4
        let site32d_rva: usize = 0x142750e;
        let tramp32d_rva: usize = 0x55fddd4;
        let return32d_valid_rva: usize = 0x1427516;
        let return32d_skip_rva: usize = 0x142753e;

        let mut tramp32d = Vec::with_capacity(40);
        // cmp rbx, 0x10000: 48 81 fb 00 00 01 00
        tramp32d.extend_from_slice(&[0x48, 0x81, 0xfb, 0x00, 0x00, 0x01, 0x00]);
        // jb skip
        tramp32d.push(0x72);
        let jb32d_pos1 = tramp32d.len();
        tramp32d.push(0x00);
        // cmp qword ptr [rbx + 0x10], 0x10000: 48 81 7b 10 00 00 01 00
        tramp32d.extend_from_slice(&[0x48, 0x81, 0x7b, 0x10, 0x00, 0x00, 0x01, 0x00]);
        // jb skip
        tramp32d.push(0x72);
        let jb32d_pos2 = tramp32d.len();
        tramp32d.push(0x00);
        // mov rcx, rsi: 48 89 f1
        tramp32d.extend_from_slice(&[0x48, 0x89, 0xf1]);
        // call method1_rva: e8 rel32
        let pos32d_call = tramp32d.len();
        let rel32d_call = (method1_rva as isize - (tramp32d_rva as isize + pos32d_call as isize + 5)) as i32;
        tramp32d.push(0xe8);
        tramp32d.extend_from_slice(&rel32d_call.to_le_bytes());
        // jmp return32d_valid_rva: e9 rel32
        let pos32d_valid = tramp32d.len();
        let rel32d_valid = (return32d_valid_rva as isize - (tramp32d_rva as isize + pos32d_valid as isize + 5)) as i32;
        tramp32d.push(0xe9);
        tramp32d.extend_from_slice(&rel32d_valid.to_le_bytes());
        // skip:
        let skip32d_pos = tramp32d.len();
        tramp32d[jb32d_pos1] = (skip32d_pos - (jb32d_pos1 + 1)) as u8;
        tramp32d[jb32d_pos2] = (skip32d_pos - (jb32d_pos2 + 1)) as u8;
        // jmp return32d_skip_rva: e9 rel32
        let pos32d_skip = tramp32d.len();
        let rel32d_skip = (return32d_skip_rva as isize - (tramp32d_rva as isize + pos32d_skip as isize + 5)) as i32;
        tramp32d.push(0xe9);
        tramp32d.extend_from_slice(&rel32d_skip.to_le_bytes());

        patch(tramp32d_rva, &tramp32d)?;

        let mut patch32d = [0x90u8; 8];
        let rel32d_site = (tramp32d_rva as isize - (site32d_rva as isize + 5)) as i32;
        patch32d[0] = 0xe9;
        patch32d[1..5].copy_from_slice(&rel32d_site.to_le_bytes());
        patch(site32d_rva, &patch32d)?;

        // 33. Guard pgRef reference resolution against raw tokens (0x50, 0x100000000, -1).
        // RAGE resource references (pgRef) are initialized to raw chunk tokens:
        //   - chunk 0 offset (0x0..0xffffffff, e.g. 0x50),
        //   - chunk 1 offset (0x100000000..0x1ffffffff, e.g. 0x100000000), or
        //   - invalid sentinel (-1 / 0xffffffffffffffff).
        // During streaming placement, when a token is not in any loaded chunk, 0x4da62d0 returns rax = 0.
        // In callers like 0x145312c70, adding rax (0) leaves the raw token in place, which is then
        // passed as `pref* rdx` into 0x4da62a0 (e.g. via 0x5312db0). At 0x4da62a6, `mov rdi, [rdx]` dereferences
        // the raw token directly, crashing with e.g. `read 0x50: nothing mapped` or `read 0x100000000`.
        //
        // Multi-level immunity:
        // A. Universal Entry Guard on 0x4da62a0 (entry RVA 0x4da62a0):
        //    Trampoline at 0x55e6b15 validates `rdx`: canonical user-space pointer above 4 GiB
        //    (`rdx >> 47 == 0` && `rdx > 0x100000000`). If invalid (e.g. 0x50, 0x100000000, -1),
        //    returns immediately without dereferencing `[rdx]`. This protects ALL callers across the binary.
        // B. Relocation Failure Null-Check in 0x145312c70 (site RVA 0x5312d20):
        //    Trampoline at 0x55ff826 calls 0x4da62d0. If rax == 0 (token not found), zeroes `rdi` and `[rsi]`.
        //    This sets the reference to NULL (0) and triggers 0x5312c70's native clean return at 0x5312d67.

        // A. Universal Entry Guard on 0x4da62a0 via trampoline at 0x67e1e30
        let site33a_rva: usize = 0x4da62a0;
        let tramp33a_rva: usize = 0x67e1e30;
        let return33a_valid_rva: usize = 0x4da62a6;
        let fn_hook33a_cb = on_hook33a_bailout as *const () as usize;

        let mut tramp33a = Vec::with_capacity(64);
        // mov r10, rdx: 49 89 d2
        tramp33a.extend_from_slice(&[0x49, 0x89, 0xd2]);
        // shr r10, 47: 49 c1 ea 2f
        tramp33a.extend_from_slice(&[0x49, 0xc1, 0xea, 0x2f]);
        // jnz invalid
        tramp33a.push(0x75);
        let jnz33a_pos = tramp33a.len();
        tramp33a.push(0x00);
        // mov r10, 0x100000000: 49 ba 00 00 00 00 01 00 00 00
        tramp33a.extend_from_slice(&[0x49, 0xba, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
        // cmp rdx, r10: 4c 39 d2
        tramp33a.extend_from_slice(&[0x4c, 0x39, 0xd2]);
        // jbe invalid
        tramp33a.push(0x76);
        let jbe33a_pos = tramp33a.len();
        tramp33a.push(0x00);
        // valid: push rsi, push rdi, sub rsp, 0x28
        tramp33a.extend_from_slice(&[0x56, 0x57, 0x48, 0x83, 0xec, 0x28]);
        // jmp return33a_valid_rva: e9 rel32
        let pos33a_valid = tramp33a.len();
        let rel33a_valid = (return33a_valid_rva as isize - (tramp33a_rva as isize + pos33a_valid as isize + 5)) as i32;
        tramp33a.push(0xe9);
        tramp33a.extend_from_slice(&rel33a_valid.to_le_bytes());
        // invalid:
        let inv33a_pos = tramp33a.len();
        tramp33a[jnz33a_pos] = (inv33a_pos - (jnz33a_pos + 1)) as u8;
        tramp33a[jbe33a_pos] = (inv33a_pos - (jbe33a_pos + 1)) as u8;
        // mov rcx, rdx: 48 89 d1
        tramp33a.extend_from_slice(&[0x48, 0x89, 0xd1]);
        // mov rdx, qword ptr [rsp]: 48 8b 14 24
        tramp33a.extend_from_slice(&[0x48, 0x8b, 0x14, 0x24]);
        // sub rsp, 0x28: 48 83 ec 28
        tramp33a.extend_from_slice(&[0x48, 0x83, 0xec, 0x28]);
        // mov r11, fn_hook33a_cb: 49 bb <8 bytes>
        tramp33a.extend_from_slice(&[0x49, 0xbb]);
        tramp33a.extend_from_slice(&(fn_hook33a_cb as u64).to_le_bytes());
        // call r11: 41 ff d3
        tramp33a.extend_from_slice(&[0x41, 0xff, 0xd3]);
        // add rsp, 0x28: 48 83 c4 28
        tramp33a.extend_from_slice(&[0x48, 0x83, 0xc4, 0x28]);
        // ret: c3
        tramp33a.push(0xc3);

        patch_exec(tramp33a_rva, &tramp33a)?;

        let mut patch33a = [0x90u8; 6];
        let rel33a_site = (tramp33a_rva as isize - (site33a_rva as isize + 5)) as i32;
        patch33a[0] = 0xe9;
        patch33a[1..5].copy_from_slice(&rel33a_site.to_le_bytes());
        patch(site33a_rva, &patch33a)?;

        // B. Relocation Failure Null-Check in 0x145312c70 via trampoline at 0x67e1e80
        let site33b_rva: usize = 0x5312d20;
        let tramp33b_rva: usize = 0x67e1e80;
        let fn_resolve_token_rva: usize = 0x4da62d0;
        let return33b_resume_rva: usize = 0x5312d2b;
        let fn_hook33b_cb = on_hook33b_bailout as *const () as usize;

        let mut tramp33b = Vec::with_capacity(64);
        // call fn_resolve_token_rva: e8 rel32
        let pos33b_call = tramp33b.len();
        let rel33b_call = (fn_resolve_token_rva as isize - (tramp33b_rva as isize + pos33b_call as isize + 5)) as i32;
        tramp33b.push(0xe8);
        tramp33b.extend_from_slice(&rel33b_call.to_le_bytes());
        // test rax, rax: 48 85 c0
        tramp33b.extend_from_slice(&[0x48, 0x85, 0xc0]);
        // jnz valid
        tramp33b.push(0x75);
        let jnz33b_pos = tramp33b.len();
        tramp33b.push(0x00);
        // mov rcx, rdi: 48 89 f9
        tramp33b.extend_from_slice(&[0x48, 0x89, 0xf9]);
        // mov rdx, qword ptr [rsp + 0x48]: 48 8b 54 24 48
        tramp33b.extend_from_slice(&[0x48, 0x8b, 0x54, 0x24, 0x48]);
        // sub rsp, 0x20: 48 83 ec 20
        tramp33b.extend_from_slice(&[0x48, 0x83, 0xec, 0x20]);
        // mov r11, fn_hook33b_cb: 49 bb <8 bytes>
        tramp33b.extend_from_slice(&[0x49, 0xbb]);
        tramp33b.extend_from_slice(&(fn_hook33b_cb as u64).to_le_bytes());
        // call r11: 41 ff d3
        tramp33b.extend_from_slice(&[0x41, 0xff, 0xd3]);
        // add rsp, 0x20: 48 83 c4 20
        tramp33b.extend_from_slice(&[0x48, 0x83, 0xc4, 0x20]);
        // xor edi, edi: 31 ff
        tramp33b.extend_from_slice(&[0x31, 0xff]);
        // mov [rsi], rdi: 48 89 3e
        tramp33b.extend_from_slice(&[0x48, 0x89, 0x3e]);
        // jmp return33b_resume_rva: e9 rel32
        let pos33b_resume1 = tramp33b.len();
        let rel33b_resume1 = (return33b_resume_rva as isize - (tramp33b_rva as isize + pos33b_resume1 as isize + 5)) as i32;
        tramp33b.push(0xe9);
        tramp33b.extend_from_slice(&rel33b_resume1.to_le_bytes());
        // valid:
        let valid33b_pos = tramp33b.len();
        tramp33b[jnz33b_pos] = (valid33b_pos - (jnz33b_pos + 1)) as u8;
        // add rdi, rax: 48 01 c7
        tramp33b.extend_from_slice(&[0x48, 0x01, 0xc7]);
        // mov [rsi], rdi: 48 89 3e
        tramp33b.extend_from_slice(&[0x48, 0x89, 0x3e]);
        // jmp return33b_resume_rva: e9 rel32
        let pos33b_resume2 = tramp33b.len();
        let rel33b_resume2 = (return33b_resume_rva as isize - (tramp33b_rva as isize + pos33b_resume2 as isize + 5)) as i32;
        tramp33b.push(0xe9);
        tramp33b.extend_from_slice(&rel33b_resume2.to_le_bytes());

        patch_exec(tramp33b_rva, &tramp33b)?;

        let mut patch33b = [0x90u8; 11];
        let rel33b_site = (tramp33b_rva as isize - (site33b_rva as isize + 5)) as i32;
        patch33b[0] = 0xe9;
        patch33b[1..5].copy_from_slice(&rel33b_site.to_le_bytes());
        patch(site33b_rva, &patch33b)?;

        // 34. Guard pgRef array loop and secondary clones against un-relocated tokens (e.g. 0x3a95da003d).
        // In RAGE, pgRef::Resolve(chunk_table, pgRef* target) has 3 compiled clones:
        //   - Clone 1: 0x4da62a0 (guarded by Hook 33A/33B)
        //   - Clone 2: 0x38160   (called in inner loops)
        //   - Clone 3: 0x4db82a0 (called at 0x54bfa4 to relocate *(rsi + 0xa0))
        //
        // When chunk 0x3a is not loaded, 0x4da62d0 returns rax = 0.
        // In Clone 3 (0x4db82a0), unpatched code at 0x4db82bb did `add rax, rsi; mov [rdi], rax`,
        // leaving raw token 0x3a95da003d in *(rsi + 0xa0).
        // Then at 0x54c3fe, the post-registration loop unconditionally dereferenced [r14 + r15],
        // crashing at 0x54c401 with `read 0x3a95da003d: nothing mapped there`.
        // Because 0x54bf10 crashed before returning to 0x673a12, the object vtable (0x62090c0)
        // was never installed, causing subsequent worker thread virtual calls to cascade into ~21,000 invalid calls.
        //
        // Multi-level immunity in .rdata padding (0x67e1c10..0x67e1cb0):
        // A. Loop Guard in 0x54bf10 at 0x54c3fe via trampoline at 0x67e1c10:
        //    Validates r14 = [rbx]: canonical, >= 0x100000000, and 8-byte aligned (r14 & 7 == 0).
        //    If invalid or null, jumps immediately to epilogue at 0x54c449, returning cleanly to 0x673a12.
        //    If valid, tests [r14 + r15] and branches to 0x54c436 (zero) or 0x54c408 (non-zero).
        // B. Entry Guard on Clone 2 (0x38160) via trampoline at 0x67e1c50:
        //    Validates rdx (canonical and > 0x100000000). Returns immediately if invalid.
        // C. Entry Guard on Clone 3 (0x4db82a0) via trampoline at 0x67e1c80:
        //    Validates rdx (canonical and > 0x100000000). Returns immediately if invalid.
        // D. Relocation Failure Null-Check in Clone 3 (0x4db82bb) via trampoline at 0x67e1cb0:
        //    Calls 0x4da62d0. If rax == 0, stores 0 into [rdi]; otherwise adds rsi and stores rax.

        // A. Loop Guard at 0x54c3fe via trampoline at 0x67e1c10
        let site34a_rva: usize = 0x54c3fe;
        let tramp34a_rva: usize = 0x67e1c10;
        let epilogue34a_rva: usize = 0x54c449;
        let valid_zero34a_rva: usize = 0x54c436;
        let valid_nonzero34a_rva: usize = 0x54c408;

        let mut tramp34a = Vec::with_capacity(64);
        // mov r14, [rbx]: 4c 8b 33
        tramp34a.extend_from_slice(&[0x4c, 0x8b, 0x33]);
        // mov r10, r14: 4d 89 f2
        tramp34a.extend_from_slice(&[0x4d, 0x89, 0xf2]);
        // shr r10, 47: 49 c1 ea 2f
        tramp34a.extend_from_slice(&[0x49, 0xc1, 0xea, 0x2f]);
        // jnz invalid
        tramp34a.push(0x75);
        let jnz34a_pos = tramp34a.len();
        tramp34a.push(0x00);
        // mov r10, 0x100000000: 49 ba 00 00 00 00 01 00 00 00
        tramp34a.extend_from_slice(&[0x49, 0xba, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
        // cmp r14, r10: 4d 39 d6
        tramp34a.extend_from_slice(&[0x4d, 0x39, 0xd6]);
        // jbe invalid
        tramp34a.push(0x76);
        let jbe34a_pos = tramp34a.len();
        tramp34a.push(0x00);
        // test r14b, 0x7: 41 f6 c6 07
        tramp34a.extend_from_slice(&[0x41, 0xf6, 0xc6, 0x07]);
        // jnz invalid
        tramp34a.push(0x75);
        let jnz_align34a_pos = tramp34a.len();
        tramp34a.push(0x00);
        // valid: cmp qword [r14 + r15], 0: 4b 83 3c 3e 00
        tramp34a.extend_from_slice(&[0x4b, 0x83, 0x3c, 0x3e, 0x00]);
        // jz is_zero
        tramp34a.push(0x74);
        let jz34a_pos = tramp34a.len();
        tramp34a.push(0x00);
        // jmp valid_nonzero34a_rva: e9 rel32
        let pos34a_nonzero = tramp34a.len();
        let rel34a_nonzero = (valid_nonzero34a_rva as isize - (tramp34a_rva as isize + pos34a_nonzero as isize + 5)) as i32;
        tramp34a.push(0xe9);
        tramp34a.extend_from_slice(&rel34a_nonzero.to_le_bytes());
        // is_zero:
        let is_zero34a_pos = tramp34a.len();
        tramp34a[jz34a_pos] = (is_zero34a_pos - (jz34a_pos + 1)) as u8;
        // jmp valid_zero34a_rva: e9 rel32
        let pos34a_zero = tramp34a.len();
        let rel34a_zero = (valid_zero34a_rva as isize - (tramp34a_rva as isize + pos34a_zero as isize + 5)) as i32;
        tramp34a.push(0xe9);
        tramp34a.extend_from_slice(&rel34a_zero.to_le_bytes());
        // invalid:
        let invalid34a_pos = tramp34a.len();
        tramp34a[jnz34a_pos] = (invalid34a_pos - (jnz34a_pos + 1)) as u8;
        tramp34a[jbe34a_pos] = (invalid34a_pos - (jbe34a_pos + 1)) as u8;
        tramp34a[jnz_align34a_pos] = (invalid34a_pos - (jnz_align34a_pos + 1)) as u8;
        // jmp epilogue34a_rva: e9 rel32
        let pos34a_epilogue = tramp34a.len();
        let rel34a_epilogue = (epilogue34a_rva as isize - (tramp34a_rva as isize + pos34a_epilogue as isize + 5)) as i32;
        tramp34a.push(0xe9);
        tramp34a.extend_from_slice(&rel34a_epilogue.to_le_bytes());

        patch_exec(tramp34a_rva, &tramp34a)?;

        let mut patch34a = [0x90u8; 8];
        let rel34a_site = (tramp34a_rva as isize - (site34a_rva as isize + 5)) as i32;
        patch34a[0] = 0xe9;
        patch34a[1..5].copy_from_slice(&rel34a_site.to_le_bytes());
        patch(site34a_rva, &patch34a)?;

        // B. Entry Guard on Clone 2 (0x38160) via trampoline at 0x67e1c50
        let site34b_rva: usize = 0x38160;
        let tramp34b_rva: usize = 0x67e1c50;
        let return34b_valid_rva: usize = 0x38166;

        let mut tramp34b = Vec::with_capacity(40);
        // mov r10, rdx: 49 89 d2
        tramp34b.extend_from_slice(&[0x49, 0x89, 0xd2]);
        // shr r10, 47: 49 c1 ea 2f
        tramp34b.extend_from_slice(&[0x49, 0xc1, 0xea, 0x2f]);
        // jnz invalid
        tramp34b.push(0x75);
        let jnz34b_pos = tramp34b.len();
        tramp34b.push(0x00);
        // mov r10, 0x100000000: 49 ba 00 00 00 00 01 00 00 00
        tramp34b.extend_from_slice(&[0x49, 0xba, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
        // cmp rdx, r10: 4c 39 d2
        tramp34b.extend_from_slice(&[0x4c, 0x39, 0xd2]);
        // jbe invalid
        tramp34b.push(0x76);
        let jbe34b_pos = tramp34b.len();
        tramp34b.push(0x00);
        // valid: push rsi, push rdi, sub rsp, 0x28: 56 57 48 83 ec 28
        tramp34b.extend_from_slice(&[0x56, 0x57, 0x48, 0x83, 0xec, 0x28]);
        // jmp return34b_valid_rva: e9 rel32
        let pos34b_valid = tramp34b.len();
        let rel34b_valid = (return34b_valid_rva as isize - (tramp34b_rva as isize + pos34b_valid as isize + 5)) as i32;
        tramp34b.push(0xe9);
        tramp34b.extend_from_slice(&rel34b_valid.to_le_bytes());
        // invalid:
        let inv34b_pos = tramp34b.len();
        tramp34b[jnz34b_pos] = (inv34b_pos - (jnz34b_pos + 1)) as u8;
        tramp34b[jbe34b_pos] = (inv34b_pos - (jbe34b_pos + 1)) as u8;
        tramp34b.push(0xc3); // ret

        patch_exec(tramp34b_rva, &tramp34b)?;

        let mut patch34b = [0x90u8; 6];
        let rel34b_site = (tramp34b_rva as isize - (site34b_rva as isize + 5)) as i32;
        patch34b[0] = 0xe9;
        patch34b[1..5].copy_from_slice(&rel34b_site.to_le_bytes());
        patch(site34b_rva, &patch34b)?;

        // C. Entry Guard on Clone 3 (0x4db82a0) via trampoline at 0x67e1c80
        let site34c_rva: usize = 0x4db82a0;
        let tramp34c_rva: usize = 0x67e1c80;
        let return34c_valid_rva: usize = 0x4db82a6;

        let mut tramp34c = Vec::with_capacity(40);
        // mov r10, rdx: 49 89 d2
        tramp34c.extend_from_slice(&[0x49, 0x89, 0xd2]);
        // shr r10, 47: 49 c1 ea 2f
        tramp34c.extend_from_slice(&[0x49, 0xc1, 0xea, 0x2f]);
        // jnz invalid
        tramp34c.push(0x75);
        let jnz34c_pos = tramp34c.len();
        tramp34c.push(0x00);
        // mov r10, 0x100000000: 49 ba 00 00 00 00 01 00 00 00
        tramp34c.extend_from_slice(&[0x49, 0xba, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
        // cmp rdx, r10: 4c 39 d2
        tramp34c.extend_from_slice(&[0x4c, 0x39, 0xd2]);
        // jbe invalid
        tramp34c.push(0x76);
        let jbe34c_pos = tramp34c.len();
        tramp34c.push(0x00);
        // valid: push rsi, push rdi, sub rsp, 0x28: 56 57 48 83 ec 28
        tramp34c.extend_from_slice(&[0x56, 0x57, 0x48, 0x83, 0xec, 0x28]);
        // jmp return34c_valid_rva: e9 rel32
        let pos34c_valid = tramp34c.len();
        let rel34c_valid = (return34c_valid_rva as isize - (tramp34c_rva as isize + pos34c_valid as isize + 5)) as i32;
        tramp34c.push(0xe9);
        tramp34c.extend_from_slice(&rel34c_valid.to_le_bytes());
        // invalid:
        let inv34c_pos = tramp34c.len();
        tramp34c[jnz34c_pos] = (inv34c_pos - (jnz34c_pos + 1)) as u8;
        tramp34c[jbe34c_pos] = (inv34c_pos - (jbe34c_pos + 1)) as u8;
        tramp34c.push(0xc3); // ret

        patch_exec(tramp34c_rva, &tramp34c)?;

        let mut patch34c = [0x90u8; 6];
        let rel34c_site = (tramp34c_rva as isize - (site34c_rva as isize + 5)) as i32;
        patch34c[0] = 0xe9;
        patch34c[1..5].copy_from_slice(&rel34c_site.to_le_bytes());
        patch(site34c_rva, &patch34c)?;

        // D. Relocation Failure Null-Check in Clone 3 (0x4db82bb) via trampoline at 0x67e1cb0
        let site34d_rva: usize = 0x4db82bb;
        let tramp34d_rva: usize = 0x67e1cb0;
        let fn_resolve_token_rva: usize = 0x4da62d0;
        let return34d_epilogue_rva: usize = 0x4db82ae;

        let mut tramp34d = Vec::with_capacity(32);
        // call fn_resolve_token_rva: e8 rel32
        let pos34d_call = tramp34d.len();
        let rel34d_call = (fn_resolve_token_rva as isize - (tramp34d_rva as isize + pos34d_call as isize + 5)) as i32;
        tramp34d.push(0xe8);
        tramp34d.extend_from_slice(&rel34d_call.to_le_bytes());
        // test rax, rax: 48 85 c0
        tramp34d.extend_from_slice(&[0x48, 0x85, 0xc0]);
        // jnz valid
        tramp34d.push(0x75);
        let jnz34d_pos = tramp34d.len();
        tramp34d.push(0x00);
        // xor eax, eax: 31 c0
        tramp34d.extend_from_slice(&[0x31, 0xc0]);
        // jmp store
        tramp34d.push(0xeb);
        let jmp34d_store_pos = tramp34d.len();
        tramp34d.push(0x00);
        // valid: add rax, rsi: 48 01 f0
        let valid34d_pos = tramp34d.len();
        tramp34d[jnz34d_pos] = (valid34d_pos - (jnz34d_pos + 1)) as u8;
        tramp34d.extend_from_slice(&[0x48, 0x01, 0xf0]);
        // store: mov [rdi], rax: 48 89 07
        let store34d_pos = tramp34d.len();
        tramp34d[jmp34d_store_pos] = (store34d_pos - (jmp34d_store_pos + 1)) as u8;
        tramp34d.extend_from_slice(&[0x48, 0x89, 0x07]);
        // jmp return34d_epilogue_rva: e9 rel32
        let pos34d_epilogue = tramp34d.len();
        let rel34d_epilogue = (return34d_epilogue_rva as isize - (tramp34d_rva as isize + pos34d_epilogue as isize + 5)) as i32;
        tramp34d.push(0xe9);
        tramp34d.extend_from_slice(&rel34d_epilogue.to_le_bytes());

        patch_exec(tramp34d_rva, &tramp34d)?;

        let mut patch34d = [0x90u8; 11];
        let rel34d_site = (tramp34d_rva as isize - (site34d_rva as isize + 5)) as i32;
        patch34d[0] = 0xe9;
        patch34d[1..5].copy_from_slice(&rel34d_site.to_le_bytes());
        patch(site34d_rva, &patch34d)?;

        // 35. Guard SafeResolve failure path (0xb593f9) and resource dispatch (0x673d0e).
        // A. Guard 0xb593f9 against non-canonical / invalid reference pointers:
        //    In 0xb59340, when IsPointerInChunks(rdi, rsi) returns false (0), line 0xb593f9 executes:
        //      mov qword [rsi], 0x0
        //    If rsi is non-canonical (e.g. 0x5a0000327011446e), this crashes with 0xc0000005
        //    (read 0xffffffffffffffff: outside the address space).
        //    Trampoline at 0x67e1cd0 validates rsi: user-mode canonical above 4 GiB (rsi >> 47 == 0 && rsi > 0x100000000).
        //    If valid, zeroes [rsi]; if invalid, skips the store and directly executes function epilogue.
        let site35a_rva: usize = 0xb593f9;
        let tramp35a_rva: usize = 0x67e1cd0;

        let mut tramp35a = Vec::with_capacity(48);
        // mov r10, rsi: 49 89 f2
        tramp35a.extend_from_slice(&[0x49, 0x89, 0xf2]);
        // shr r10, 47: 49 c1 ea 2f
        tramp35a.extend_from_slice(&[0x49, 0xc1, 0xea, 0x2f]);
        // jnz skip
        tramp35a.push(0x75);
        let jnz35a_pos = tramp35a.len();
        tramp35a.push(0x00);
        // mov r10, 0x100000000: 49 ba 00 00 00 00 01 00 00 00
        tramp35a.extend_from_slice(&[0x49, 0xba, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
        // cmp rsi, r10: 4c 39 d6
        tramp35a.extend_from_slice(&[0x4c, 0x39, 0xd6]);
        // jbe skip
        tramp35a.push(0x76);
        let jbe35a_pos = tramp35a.len();
        tramp35a.push(0x00);
        // valid: mov qword [rsi], 0: 48 c7 06 00 00 00 00
        tramp35a.extend_from_slice(&[0x48, 0xc7, 0x06, 0x00, 0x00, 0x00, 0x00]);
        // skip:
        let skip35a_pos = tramp35a.len();
        tramp35a[jnz35a_pos] = (skip35a_pos - (jnz35a_pos + 1)) as u8;
        tramp35a[jbe35a_pos] = (skip35a_pos - (jbe35a_pos + 1)) as u8;
        // add rsp, 0x20 ; pop rbx ; pop rdi ; pop rsi ; ret: 48 83 c4 20 5b 5f 5e c3
        tramp35a.extend_from_slice(&[0x48, 0x83, 0xc4, 0x20, 0x5b, 0x5f, 0x5e, 0xc3]);

        patch_exec(tramp35a_rva, &tramp35a)?;

        let mut patch35a = [0x90u8; 7];
        let rel35a_site = (tramp35a_rva as isize - (site35a_rva as isize + 5)) as i32;
        patch35a[0] = 0xe9;
        patch35a[1..5].copy_from_slice(&rel35a_site.to_le_bytes());
        patch(site35a_rva, &patch35a)?;

        // B. Guard 0x673d0e (jmp 0x269be30) against non-canonical resource object pointers:
        //    In 0x673bf0, after resolving the reference, if non-null it tail-calls 0x269be30.
        //    If rcx is an unresolved token or non-canonical address, jumping to 0x269be30 leads
        //    to invalid sub-object calls. Trampoline at 0x67e1d10 checks rcx: if invalid, returns cleanly.
        let site35b_rva: usize = 0x673d0e;
        let tramp35b_rva: usize = 0x67e1d10;
        let dest35b_rva: usize = 0x269be30;

        let mut tramp35b = Vec::with_capacity(36);
        // mov r10, rcx: 49 89 ca
        tramp35b.extend_from_slice(&[0x49, 0x89, 0xca]);
        // shr r10, 47: 49 c1 ea 2f
        tramp35b.extend_from_slice(&[0x49, 0xc1, 0xea, 0x2f]);
        // jnz skip
        tramp35b.push(0x75);
        let jnz35b_pos = tramp35b.len();
        tramp35b.push(0x00);
        // mov r10, 0x100000000: 49 ba 00 00 00 00 01 00 00 00
        tramp35b.extend_from_slice(&[0x49, 0xba, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
        // cmp rcx, r10: 4c 39 d1
        tramp35b.extend_from_slice(&[0x4c, 0x39, 0xd1]);
        // jbe skip
        tramp35b.push(0x76);
        let jbe35b_pos = tramp35b.len();
        tramp35b.push(0x00);
        // valid: jmp dest35b_rva (e9 rel32)
        let pos35b_jmp = tramp35b.len();
        let rel35b_jmp = (dest35b_rva as isize - (tramp35b_rva as isize + pos35b_jmp as isize + 5)) as i32;
        tramp35b.push(0xe9);
        tramp35b.extend_from_slice(&rel35b_jmp.to_le_bytes());
        // skip: ret: c3
        let skip35b_pos = tramp35b.len();
        tramp35b[jnz35b_pos] = (skip35b_pos - (jnz35b_pos + 1)) as u8;
        tramp35b[jbe35b_pos] = (skip35b_pos - (jbe35b_pos + 1)) as u8;
        tramp35b.push(0xc3);

        patch_exec(tramp35b_rva, &tramp35b)?;

        let mut patch35b = [0x90u8; 5];
        let rel35b_site = (tramp35b_rva as isize - (site35b_rva as isize + 5)) as i32;
        patch35b[0] = 0xe9;
        patch35b[1..5].copy_from_slice(&rel35b_site.to_le_bytes());
        patch(site35b_rva, &patch35b)?;

        // 36. Guard null resource array loop in 0x673a00 (0x673a84) and null element dereference in 0x269bd10 (0x269bd59).
        // A. Caller Guard at 0x673a84 in 0x673a00:
        //    At 0x673a7f, 0x324ef0 resolves the resource sub-array pointer *(rsi + 0xf0).
        //    When the chunk is not loaded, Universal Relocation sets *(rsi + 0xf0) = 0 (NULL).
        //    However, word [rsi + 0xf8] (count) remains non-zero. The original code at 0x673a84 only
        //    checked `cmp word [rsi + 0xf8], 0; jnz 0x673ac0`, unconditionally looping over the array
        //    and calling 0x269bd10 with rcx = [rsi + 0xf0] + rdi = 0.
        //    Trampoline at 0x67e1d40 checks BOTH count and qword [rsi + 0xf0] >= 0x10000.
        //    If count == 0 OR array pointer is NULL (< 0x10000), skips the loop and jumps directly to 0x673a8e.
        let site36a_rva: usize = 0x673a84;
        let tramp36a_rva: usize = 0x67e1d40;
        let loop36a_rva: usize = 0x673ac0;
        let skip36a_rva: usize = 0x673a8e;

        let mut tramp36a = Vec::with_capacity(40);
        // cmp word [rsi + 0xf8], 0: 66 83 be f8 00 00 00 00
        tramp36a.extend_from_slice(&[0x66, 0x83, 0xbe, 0xf8, 0x00, 0x00, 0x00, 0x00]);
        // jz skip
        tramp36a.push(0x74);
        let jz36a_pos = tramp36a.len();
        tramp36a.push(0x00);
        // cmp qword [rsi + 0xf0], 0x10000: 48 81 be f0 00 00 00 00 00 01 00
        tramp36a.extend_from_slice(&[0x48, 0x81, 0xbe, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00]);
        // jb skip
        tramp36a.push(0x72);
        let jb36a_pos = tramp36a.len();
        tramp36a.push(0x00);
        // jmp loop36a_rva: e9 rel32
        let pos36a_loop = tramp36a.len();
        let rel36a_loop = (loop36a_rva as isize - (tramp36a_rva as isize + pos36a_loop as isize + 5)) as i32;
        tramp36a.push(0xe9);
        tramp36a.extend_from_slice(&rel36a_loop.to_le_bytes());
        // skip:
        let skip36a_pos = tramp36a.len();
        tramp36a[jz36a_pos] = (skip36a_pos - (jz36a_pos + 1)) as u8;
        tramp36a[jb36a_pos] = (skip36a_pos - (jb36a_pos + 1)) as u8;
        // jmp skip36a_rva: e9 rel32
        let pos36a_skip = tramp36a.len();
        let rel36a_skip = (skip36a_rva as isize - (tramp36a_rva as isize + pos36a_skip as isize + 5)) as i32;
        tramp36a.push(0xe9);
        tramp36a.extend_from_slice(&rel36a_skip.to_le_bytes());

        patch_exec(tramp36a_rva, &tramp36a)?;

        let mut patch36a = [0x90u8; 10];
        let rel36a_site = (tramp36a_rva as isize - (site36a_rva as isize + 5)) as i32;
        patch36a[0] = 0xe9;
        patch36a[1..5].copy_from_slice(&rel36a_site.to_le_bytes());
        patch(site36a_rva, &patch36a)?;

        // B. Callee Guard at 0x269bd59 in 0x269bd10:
        //    In 0x269bd10, line 0x269bd59 executes `cmp qword [rsi], 0` assuming rsi is a valid pointer.
        //    When called with a null or unmapped element address (rsi < 0x100000000 or non-canonical),
        //    this crashes with `read 0x0: nothing mapped there`.
        //    Trampoline at 0x67e1d80 validates rsi: canonical user-space above 4 GiB.
        //    If valid, executes `cmp qword [rsi], 0` (jumping to 0x269be16 if 0, or 0x269bd63 if non-zero).
        //    If invalid or null, jumps immediately to epilogue at 0x269be1d, returning cleanly.
        let site36b_rva: usize = 0x269bd59;
        let tramp36b_rva: usize = 0x67e1d80;
        let zero36b_rva: usize = 0x269be16;
        let nonzero36b_rva: usize = 0x269bd63;
        let epilogue36b_rva: usize = 0x269be1d;

        let mut tramp36b = Vec::with_capacity(48);
        // mov r10, rsi: 49 89 f2
        tramp36b.extend_from_slice(&[0x49, 0x89, 0xf2]);
        // shr r10, 47: 49 c1 ea 2f
        tramp36b.extend_from_slice(&[0x49, 0xc1, 0xea, 0x2f]);
        // jnz invalid
        tramp36b.push(0x75);
        let jnz36b_pos = tramp36b.len();
        tramp36b.push(0x00);
        // mov r10, 0x100000000: 49 ba 00 00 00 00 01 00 00 00
        tramp36b.extend_from_slice(&[0x49, 0xba, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
        // cmp rsi, r10: 4c 39 d6
        tramp36b.extend_from_slice(&[0x4c, 0x39, 0xd6]);
        // jbe invalid
        tramp36b.push(0x76);
        let jbe36b_pos = tramp36b.len();
        tramp36b.push(0x00);
        // valid: cmp qword [rsi], 0: 48 83 3e 00
        tramp36b.extend_from_slice(&[0x48, 0x83, 0x3e, 0x00]);
        // jz zero
        tramp36b.push(0x74);
        let jz36b_zero_pos = tramp36b.len();
        tramp36b.push(0x00);
        // jmp nonzero36b_rva: e9 rel32
        let pos36b_nonzero = tramp36b.len();
        let rel36b_nonzero = (nonzero36b_rva as isize - (tramp36b_rva as isize + pos36b_nonzero as isize + 5)) as i32;
        tramp36b.push(0xe9);
        tramp36b.extend_from_slice(&rel36b_nonzero.to_le_bytes());
        // zero:
        let pos36b_zero = tramp36b.len();
        tramp36b[jz36b_zero_pos] = (pos36b_zero - (jz36b_zero_pos + 1)) as u8;
        // jmp zero36b_rva: e9 rel32
        let pos36b_zero_jmp = tramp36b.len();
        let rel36b_zero = (zero36b_rva as isize - (tramp36b_rva as isize + pos36b_zero_jmp as isize + 5)) as i32;
        tramp36b.push(0xe9);
        tramp36b.extend_from_slice(&rel36b_zero.to_le_bytes());
        // invalid:
        let pos36b_inv = tramp36b.len();
        tramp36b[jnz36b_pos] = (pos36b_inv - (jnz36b_pos + 1)) as u8;
        tramp36b[jbe36b_pos] = (pos36b_inv - (jbe36b_pos + 1)) as u8;
        // jmp epilogue36b_rva: e9 rel32
        let pos36b_epilogue = tramp36b.len();
        let rel36b_epilogue = (epilogue36b_rva as isize - (tramp36b_rva as isize + pos36b_epilogue as isize + 5)) as i32;
        tramp36b.push(0xe9);
        tramp36b.extend_from_slice(&rel36b_epilogue.to_le_bytes());

        patch_exec(tramp36b_rva, &tramp36b)?;

        let mut patch36b = [0x90u8; 10];
        let rel36b_site = (tramp36b_rva as isize - (site36b_rva as isize + 5)) as i32;
        patch36b[0] = 0xe9;
        patch36b[1..5].copy_from_slice(&rel36b_site.to_le_bytes());
        patch(site36b_rva, &patch36b)?;

        // 37. Guard AABB intersection/union null dereference at 0xe45360 (+0xe45374).
        // A. Caller Guard at 0x142723f in 0x1427210:
        //    r14 is loaded from [rdi + 0x10] and checked for non-null at 0x142723a.
        //    Then 0x142723f loads rdx = [r14 + 0x20] and unconditionally calls 0xe45360(rcx=rsi, rdx).
        //    When the entity component does not have a bounding box ([r14 + 0x20] == NULL or < 0x10000),
        //    0xe45360 crashes dereferencing [rdx + 0x30] at 0xe45374.
        //    Trampoline at 0x67e1dc0 validates rdx: if null or invalid, calls on_hook37a_null and skips call 0xe45360.
        let site37a_rva: usize = 0x142723f;
        let tramp37a_rva: usize = 0x67e1dc0;
        let skip37a_rva: usize = 0x142724b;
        let call37a_rva: usize = 0xe45360;
        let fn_hook37a_cb = on_hook37a_null as *const () as usize;

        let mut tramp37a = Vec::with_capacity(64);
        // mov rdx, [r14 + 0x20]: 49 8b 56 20
        tramp37a.extend_from_slice(&[0x49, 0x8b, 0x56, 0x20]);
        // mov r10, rdx: 49 89 d2
        tramp37a.extend_from_slice(&[0x49, 0x89, 0xd2]);
        // shr r10, 47: 49 c1 ea 2f
        tramp37a.extend_from_slice(&[0x49, 0xc1, 0xea, 0x2f]);
        // jnz null_skip
        tramp37a.push(0x75);
        let jnz37a_pos = tramp37a.len();
        tramp37a.push(0x00);
        // cmp rdx, 0x10000: 48 81 fa 00 00 01 00
        tramp37a.extend_from_slice(&[0x48, 0x81, 0xfa, 0x00, 0x00, 0x01, 0x00]);
        // jb null_skip
        tramp37a.push(0x72);
        let jb37a_pos = tramp37a.len();
        tramp37a.push(0x00);
        // valid: mov rcx, rsi: 48 89 f1
        tramp37a.extend_from_slice(&[0x48, 0x89, 0xf1]);
        // call call37a_rva: e8 rel32
        let pos37a_call = tramp37a.len();
        let rel37a_call = (call37a_rva as isize - (tramp37a_rva as isize + pos37a_call as isize + 5)) as i32;
        tramp37a.push(0xe8);
        tramp37a.extend_from_slice(&rel37a_call.to_le_bytes());
        // jmp skip37a_rva: e9 rel32
        let pos37a_skip1 = tramp37a.len();
        let rel37a_skip1 = (skip37a_rva as isize - (tramp37a_rva as isize + pos37a_skip1 as isize + 5)) as i32;
        tramp37a.push(0xe9);
        tramp37a.extend_from_slice(&rel37a_skip1.to_le_bytes());
        // null_skip:
        let null37a_pos = tramp37a.len();
        tramp37a[jnz37a_pos] = (null37a_pos - (jnz37a_pos + 1)) as u8;
        tramp37a[jb37a_pos] = (null37a_pos - (jb37a_pos + 1)) as u8;
        // mov rcx, rsi: 48 89 f1
        tramp37a.extend_from_slice(&[0x48, 0x89, 0xf1]);
        // mov rdx, r14: 4c 89 f2
        tramp37a.extend_from_slice(&[0x4c, 0x89, 0xf2]);
        // sub rsp, 0x20: 48 83 ec 20
        tramp37a.extend_from_slice(&[0x48, 0x83, 0xec, 0x20]);
        // mov r11, fn_hook37a_cb: 49 bb <8 bytes>
        tramp37a.extend_from_slice(&[0x49, 0xbb]);
        tramp37a.extend_from_slice(&(fn_hook37a_cb as u64).to_le_bytes());
        // call r11: 41 ff d3
        tramp37a.extend_from_slice(&[0x41, 0xff, 0xd3]);
        // add rsp, 0x20: 48 83 c4 20
        tramp37a.extend_from_slice(&[0x48, 0x83, 0xc4, 0x20]);
        // jmp skip37a_rva: e9 rel32
        let pos37a_skip2 = tramp37a.len();
        let rel37a_skip2 = (skip37a_rva as isize - (tramp37a_rva as isize + pos37a_skip2 as isize + 5)) as i32;
        tramp37a.push(0xe9);
        tramp37a.extend_from_slice(&rel37a_skip2.to_le_bytes());

        patch_exec(tramp37a_rva, &tramp37a)?;

        let mut patch37a = [0x90u8; 12];
        let rel37a_site = (tramp37a_rva as isize - (site37a_rva as isize + 5)) as i32;
        patch37a[0] = 0xe9;
        patch37a[1..5].copy_from_slice(&rel37a_site.to_le_bytes());
        patch(site37a_rva, &patch37a)?;

        // B. Callee Defense at 0xe45360 in IntersectAABB:
        //    Directly validates both rcx and rdx upon entry. If either rcx < 0x10000 or rdx < 0x10000
        //    (or non-canonical), returns immediately via `ret` without allocating a stack frame or
        //    dereferencing [rcx + 0x30] or [rdx + 0x30].
        let site37b_rva: usize = 0xe45360;
        let tramp37b_rva: usize = 0x67e1df0;
        let body37b_rva: usize = 0xe4536a;

        let mut tramp37b = Vec::with_capacity(56);
        // mov r10, rcx: 49 89 ca
        tramp37b.extend_from_slice(&[0x49, 0x89, 0xca]);
        // shr r10, 47: 49 c1 ea 2f
        tramp37b.extend_from_slice(&[0x49, 0xc1, 0xea, 0x2f]);
        // jnz early_ret
        tramp37b.push(0x75);
        let jnz37b_1 = tramp37b.len();
        tramp37b.push(0x00);
        // cmp rcx, 0x10000: 48 81 f9 00 00 01 00
        tramp37b.extend_from_slice(&[0x48, 0x81, 0xf9, 0x00, 0x00, 0x01, 0x00]);
        // jb early_ret
        tramp37b.push(0x72);
        let jb37b_1 = tramp37b.len();
        tramp37b.push(0x00);
        // mov r10, rdx: 49 89 d2
        tramp37b.extend_from_slice(&[0x49, 0x89, 0xd2]);
        // shr r10, 47: 49 c1 ea 2f
        tramp37b.extend_from_slice(&[0x49, 0xc1, 0xea, 0x2f]);
        // jnz early_ret
        tramp37b.push(0x75);
        let jnz37b_2 = tramp37b.len();
        tramp37b.push(0x00);
        // cmp rdx, 0x10000: 48 81 fa 00 00 01 00
        tramp37b.extend_from_slice(&[0x48, 0x81, 0xfa, 0x00, 0x00, 0x01, 0x00]);
        // jb early_ret
        tramp37b.push(0x72);
        let jb37b_2 = tramp37b.len();
        tramp37b.push(0x00);
        // sub rsp, 0x28: 48 83 ec 28
        tramp37b.extend_from_slice(&[0x48, 0x83, 0xec, 0x28]);
        // vmovaps [rsp + 0x10], xmm7: c5 f8 29 7c 24 10
        tramp37b.extend_from_slice(&[0xc5, 0xf8, 0x29, 0x7c, 0x24, 0x10]);
        // jmp body37b_rva: e9 rel32
        let pos37b_body = tramp37b.len();
        let rel37b_body = (body37b_rva as isize - (tramp37b_rva as isize + pos37b_body as isize + 5)) as i32;
        tramp37b.push(0xe9);
        tramp37b.extend_from_slice(&rel37b_body.to_le_bytes());
        // early_ret:
        let ret37b_pos = tramp37b.len();
        tramp37b[jnz37b_1] = (ret37b_pos - (jnz37b_1 + 1)) as u8;
        tramp37b[jb37b_1] = (ret37b_pos - (jb37b_1 + 1)) as u8;
        tramp37b[jnz37b_2] = (ret37b_pos - (jnz37b_2 + 1)) as u8;
        tramp37b[jb37b_2] = (ret37b_pos - (jb37b_2 + 1)) as u8;
        tramp37b.push(0xc3);

        patch_exec(tramp37b_rva, &tramp37b)?;

        let mut patch37b = [0x90u8; 10];
        let rel37b_site = (tramp37b_rva as isize - (site37b_rva as isize + 5)) as i32;
        patch37b[0] = 0xe9;
        patch37b[1..5].copy_from_slice(&rel37b_site.to_le_bytes());
        patch(site37b_rva, &patch37b)?;

        Ok(())
    }
}
