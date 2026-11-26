# VIctory Diagnostics & Guarded Texture Refresh Change

Generated: 2026-11-25  
Branch: `ztrx-baseline` (`90b529f`)

---

## 1. `dstorage: a destination's physical pages could not be written`
- **Observed in `run_20261125_115916.log`** (`03:58:27.759Z..760Z`): 5 failures, all with `pages=1` (`offset=0` / `32768`, `len=24576` / `28672`).
- **Code path**: `crates/victory-hle/src/modules/dstorage.rs` (`into_destination`, lines 960–990) calls `xmem::page_window` and then `xmem::write_physical` (`crates/victory-hle/src/modules/xmem.rs`, lines 519–558).
- **Root cause**: Not a missing HLE physical-page API or scatter-gather bug (`XMemAllocatePhysicalPages` / `XMemMapPhysicalPages` already populate `physical().pages`). For `pages=1`, `runs()` / `View::map()` only fails when:
  1. `pages[0]` is `0` or unknown in `physical().pages` (e.g., the title enqueued a `PAGE_DESTINATION` read before writing the page number into `request.destination`, or freed the page while in flight — `ztrx`'s `dstorage.rs` has no retry loop for `pages.contains(&0)`), or
  2. `View::map` (`MapViewOfFile`) fails on the host.
- **Impact**: Non-fatal; those 5 24–28 KiB destination writes are skipped, leaving prior page contents intact.

---

## 2. `0x80070057` `host pipeline refused`
- **Observed in `run.log`** (`04:10:21Z`, `04:10:39Z`):
  - `shaders=[compute 2da4beb386509b77]`
  - `shaders=[compute 369cbe906ea72764]`
- **Code path**: `crates/victory-gpu/src/pipeline/host.rs` (lines 507–529), inside `CreateComputePipelineState(&desc)`.
- **Root cause**: The host D3D12 runtime rejected the translated DXIL compute shader or its root-signature binding with `E_INVALIDARG` (`0x80070057`). (`host.log_messages()` is called on line 526; running with `VICTORY_D3D12_DEBUG=1` prints the exact D3D12 InfoQueue message.)
- **Impact**: Those pipelines return `None`, so `draw.rs` logs `a dispatch without a host pipeline dropped` and skips dispatches using them.

---

## 3. GFX10 `"a lane mask exchanged where not every lane runs"`
- **Code path**: `crates/victory-shader/src/emit/lower.rs` (lines 2985–2998 for `waveActiveBallot`, lines 3636–3647 for `waveAnyTrue` on `scc`).
- **Root cause**: Console GFX10 shaders execute in **wave64**, while host PC GPUs (e.g. NVIDIA) execute in **wave32**. For groups of `> 32` threads, `victory-shader` emulates 64-lane wave ops by pairing two 32-lane host waves and exchanging halves (`self.exchange_halves`) via group-shared memory.
- **Why it errors**: `exchange_halves` requires every lane in the block to reach the exchange together. When the lane-mask read (line 2992) or `scc` test (line 3639) occurs in a divergent block `b` where `!self.all_lanes(b)` (and `!self.exchange.1`), the translator refuses the shader with `"a lane mask exchanged where not every lane runs"`.

---

## 4. `VICTORY_PIPELINE_WAIT_MS` Handling
- **Code path**: `crates/victory-gpu/src/pipeline/make.rs` (lines 98–214).
- **Behavior**:
  - `wait_budget()` reads `VICTORY_PIPELINE_WAIT_MS` (default **`500` ms** per title frame).
  - With `VICTORY_PIPELINE_WAIT_SHARED=1` (default), `SPENT: Mutex<(u64, Duration)>` shares that 500 ms budget across all recording threads for the current frame, waiting on `DONE` and refunding `left - waited` when a worker finishes early.
  - Setting `VICTORY_PIPELINE_WAIT_MS=0` causes `wait()` to return immediately at line 165; pipelines still compile asynchronously on background workers (`start(p, REQUESTED)`), eliminating the 500 ms/frame stall during first-run shader translation.

---

## 5. Guarded-View Texture Hot-Frame Hashing (`crates/victory-gpu/src/texture.rs`)
- **Problem**: Guarded-view textures whose writes happen through another view were only hashed once every `HASH_EVERY = 8` frames, which can leave intro/cutscene video frames stale.
- **Changes in `crates/victory-gpu/src/texture.rs`**:
  - Added `const HOT_FRAMES: u64 = 16;` and `hot_until: u64` on `Last` with helper `Last::update_hash(&mut self, hash: blake3::Hash) -> bool`, which sets `self.hot_until = crate::frame::current().saturating_add(HOT_FRAMES)` whenever `hash` changes.
  - Updated the guarded-view skip check (`if unwritten && !turn && frame >= last.hot_until { return 0; }`) so any texture whose hash changed is checked every frame for 16 frames before returning to the staggered 8-frame schedule.
  - Wired `last.update_hash(hash)` into all four hash sites (partial `copy_mapped`, `through_copy()`, direct `tiled` slice, and `refresh_unlocked`).
- **Build Verification**: `cargo build --release` succeeded in `22.81s` with 0 warnings.

---

## 6. Abrupt Exit / Silent Termination Paths
Several runtime paths can terminate `victory.exe` without a clean shutdown or Event Viewer entry:
- **Title `abort()` / `ExitProcess` / `TerminateProcess`** (`crates/victory-hle/src/modules/kernel.rs`, lines 240–255, 479–486, 573–581): Calls UCRT `abort()` or `TerminateProcess` directly.
- **Host GPU failure (`host_gpu_failed`)** (`crates/victory-hle/src/modules/d3d12x.rs`, lines 30–37) & **`host.rs` assert** (`crates/victory-gpu/src/host.rs`, line 2274): `host_gpu_failed` calls `TerminateProcess(GetCurrentProcess(), DXGI_ERROR_DEVICE_REMOVED)`.
- **Lifecycle `end()`** (`crates/victory-hle/src/modules/lifecycle.rs`, lines 288–294): Saves the pipeline library and calls `TerminateProcess(GetCurrentProcess(), 0)`.
- **Windows `__fastfail` (`0xc0000409`) / stack overflow (`0xc00000fd`)**: `__fastfail` bypasses both Vectored Exception Handlers and `SetUnhandledExceptionFilter` in `crates/victory-loader/src/crash.rs`, and a stack overflow (e.g., in `victory-pipelines-*` workers in `crates/victory-gpu/src/pipeline/make.rs`, lines 302–319) can fault inside the crash handler itself before flushing output.

---

## 7. Logging Flush & Crash Log Path
- **Tracing & PowerShell pipe**: `crates/victory-core/src/logging.rs` (lines 30–33) writes to `std::io::stderr`, which `run-game.ps1` pipes via `*>&1 | Tee-Object -FilePath "run.log"`. Abrupt process termination (`TerminateProcess` / `__fastfail`) can cause `Tee-Object` to drop or truncate the final partial line.
- **Persistent crash log**: `apps/victory/src/run.rs` (line 110) calls `victory_loader::log_crashes_to(temp.with_file_name("crashes.log"))` -> `C:\Users\Ree\AppData\Local\victory\data\7952987A\crashes.log`, which appends panics, unhandled exceptions, and `host_gpu_failed` reports directly to disk.
- **Diagnostic improvement**: Redirect stdout/stderr directly (`cmd /c "... > run.log 2>&1"` or `Start-Process -RedirectStandardError`) instead of `*>&1 | Tee-Object`, and check `C:\Users\Ree\AppData\Local\victory\data\7952987A\crashes.log` after any abrupt exit.

---

## 8. 2D Overlays Visible vs. Black 3D Scene
1. **`VICTORY_PIPELINE_WAIT_MS=0`** (`crates/victory-gpu/src/pipeline/make.rs`, lines 163–167): Skips waiting for the `~700` new 3D mesh/graphics/compute pipelines (`being made`), so `crates/victory-gpu/src/draw.rs` (lines 1686 and 2289) drops ~600–2,100 draws and ~130–3,000 indirect calls per frame while 2D UI/subtitle pipelines (already compiled) draw normally.
2. **Failed pixel & compute shaders**:
   - **3 pixel shaders** (`e5b2121a02f4f741`, `2fd01fca16993001`, `6b6db9f1a526daeb`) fail at `crates/victory-shader/src/emit/lower.rs:5581` (`"a descriptor read only as data"`), dropping their graphics/mesh draws.
   - **4 compute PSOs** (`2da4beb386509b77`, `369cbe906ea72764`, `2cc4c197ee29e291`, `02384a60fda6f2d0`) fail `CreateComputePipelineState` (`0x80070057`) at `crates/victory-gpu/src/pipeline/host.rs:520` + **1 compute shader** (`f133e35efed2a48a`) fails at `crates/victory-shader/src/emit/lower.rs:2995` (`"a lane mask exchanged where not every lane runs"`), dropping those compute dispatches.

---

## 9. Intro Cutscene Streaming Crashes, Shader/PSO Drops & View-Heap Cap (`run.log` / `crashes.log`)

### 9.1 Crash Sites & Why the Process Self-Terminated
Disassembly of `victory-image-b99f876b74d4a77f.dll` (`ImageBase = 0x140000000`) shows all four `MapDataStore` / chunk-header / pool dispatch sites are indirect/virtual calls on streaming worker threads (`+0x257035e` -> `+0x221c29c` -> `+0x2215eca`):
- **`+0x4e4f47d`** (`0x144e4f47b: call rax`): Guarded indirect call checking `rax` against `0x14003c490` and `0x144e6ff70` before falling back to `call rax`; corrupted `rax` (`0x103`, etc.) jumps to low/unmapped memory.
- **`+0x258b7fe`** (`0x14258b7fb: call qword ptr [rax + 0x68]`): Virtual method call on `rbx` (fault target `0x14d0`).
- **`+0x2593e8e`** (`0x142593e8e: call qword ptr [rax + 0x18]`): Virtual method call on `r15`.
- **`+0x9d9ebd`** (`0x1409d9ebd: call qword ptr [r10 + 0x20]`): Loops over `0x10`-byte entries in `r12`, resolves `rbx = [rax]` via `0x144da24a0`, and calls `[vtable + 0x20]`.
- **Breakpad self-termination (`+0x5149aec`)**: In the latest run (PID `3692`), `crashes.log` ends with 5 consecutive `0x80000003` (`int3`) faults at `+0x5149aec` (`0x145149ac0: mov rdi, qword ptr [rcx]; test rdi, rdi; je 0x145149aec; ... int3`). When `[rcx]` is null, the title triggers `int3`, which invoked `BacktraceBreakpad` (`run.log` at `05:34:28`), wrote a minidump, attempted to upload to `submit.backtrace.io:443` (blocked), and exited.

### 9.2 Shader, PSO, and View-Heap Drops (`run.log`, `victory-shader`, `victory-gpu`)
- **Shader translation failures (9 unique shaders)**:
  - `compute a929109552ce1e84` (`0xd00`): `v_permlanex16_b32 is not supported`. `v_permlanex16_b32` is only matched inside `wave::Section` reduction trees (`crates/victory-shader/src/emit/wave.rs:438-459`); outside of those, VALU lowering falls through to `lower.rs:8506` (`_ => return Err(inst.name())`).
  - `compute 5dc3b25986622949` (`0x7e8`) & `6f225b641c5b0708` (`0x7f8`): `v_cvt_f32_f64 is not supported` (same fallback at `lower.rs:8506`).
  - `compute ceb5eb68cd99cb07` (`0x38`): `a descriptor's address or format read as data is not supported` (`lower.rs:5480, 5573`).
  - `compute f133e35efed2a48a` (`0x1eb4`): `a lane mask exchanged where not every lane runs is not supported`.
  - `pixel e5b2121a02f4f741`, `2fd01fca16993001`, `6b6db9f1a526daeb`: `a descriptor read only as data is not supported` (`lower.rs:5581`).
  - `pixel 38bf0814e721a1b3` (`0x1464`): `the first half of an interpolation is not supported` (`lower.rs:8541`).
- **Host pipeline refusals (`0x80070057`) & unimplemented device slots**:
  - Numerous compute PSOs and 5 vertex+pixel PSOs failed `CreateComputePipelineState` / `CreateGraphicsPipelineState` (`crates/victory-gpu/src/pipeline/host.rs:520, 651`) with `0x80070057`. No InfoQueue details appeared under `VICTORY_D3D12_DEBUG=1` because the Windows *Graphics Tools* optional feature is not installed.
  - `crates/victory-gpu/src/device.rs:349-400` (`unknown_pipeline::<0x1c8>` / `<0x218>`) returns `E_NOTIMPL` for Xbox-specific pipeline creation slots `0x1c8` and `0x218`.
  - Combined with `VICTORY_PIPELINE_WAIT_MS=0`, `run.log` reports `dropped 400 without a host pipeline` per frame at the end.
- **View heap cap & texture queue saturation**:
  - `crates/victory-gpu/src/descriptor/host.rs:510-587` caps the host view heap at `PORTABLE_VIEWS = 1_000_000` (with `DYNAMIC = 1 << 18` = `262,144` dynamic slots, leaving `737,855` title slots) when the title requests `1,245,184` views; views past the cap read the null slot.
  - By `05:34:29`, `textures picked by index` had `12,445` waiting slots (`10,606` due, oldest `678` frames ago) and `0.2 fps` (`4124.9 ms/frame`).

---

## 10. D3D12 Debug Findings & Targeted Fixes (`run_5.log` – `run_7.log`)

### 10.1 D3D12 Debug Layer Activation & Findings
- **Gating fix**: `VICTORY_D3D12_DEBUG=1` in `crates/victory-gpu/src/host.rs:502-509` is gated behind `cfg!(feature = "diagnostics")`. Installing `Tools.Graphics.DirectX~~~~0.0.1.0` (`d3d12SDKLayers.dll`) and building with `cargo build --release --features diagnostics` enabled `ID3D12InfoQueue` output (`43` messages in `run.log`).
- **Root causes surfaced**:
  - `id=622` (`CreateComputeShader`): Compute PSOs (`2da4beb386509b77`, `369cbe906ea72764`, `2cc4c197ee29e291`, `02384a60fda6f2d0`) emitted `WaveSize(64)` on a `WaveLaneCountMax = 32` GPU.
  - `id=676` (`CreateGraphicsPipelineState`): Slot 2 format `DXGI_FORMAT_R8_UINT` had `BlendEnable = true` inherited from slot 0 with `IndependentBlendEnable = false`.
  - `id=645` (`CreateDescriptorHeap`): `probe_view_limit()` (`1000001`) and `HostHeap::new()` (`1507328`) exceeded the Tier 1/2 `1,000,000` limit before clamping.

### 10.2 Targeted Fixes Applied & Verified
1. **`v_permlane16_b32` & `v_permlanex16_b32` lowering** (`crates/victory-shader/src/emit/lower.rs:1266-1272, 7264-7316`):
   - Added general VALU lowering via `waveReadLaneAt.i32` (with `bound_ctrl:1` via `waveActiveBallot`), keeping shaders in 32-lane halves.
   - Compute shader `a929109552ce1e84` now translates and creates its host pipeline; unit test `permlanex16_reads_other_row_in_32_lane_half` passed on hardware GPU.
2. **Integer render-target blend sanitization** (`crates/victory-gpu/src/pipeline/host.rs:608, 764, 793-844`):
   - Added `is_integer_format` and `sanitize_blend` to promote `IndependentBlendEnable` and clear `BlendEnable` on any bound `*_UINT` / `*_SINT` RT slot in `HostPipeline::new` and `mesh_pipeline`.
   - Eliminated `id=676` in subsequent runs; unit test `integer_rt_disables_blend` passed.
3. **Shader-visible view heap pre-clamping** (`crates/victory-gpu/src/descriptor/host.rs:508-639`):
   - Updated `probe_view_limit()` to check `D3D12_FEATURE_D3D12_OPTIONS.ResourceBindingTier` first (avoiding `CreateDescriptorHeap(1000001)` on Tier 1/2 GPUs) and `HostHeap::new()` to pre-clamp `NumDescriptors` to `PORTABLE_VIEWS` (`1_000_000`) when `VIEW_LIMIT_CAPPED` is set.
   - Eliminated `id=645` errors; `descriptor::host` unit tests (`9 passed`) passed.
4. **`WAVE64-*` diagnostic gating & summarization** (`crates/victory-shader/src/emit/mod.rs:300-348, 459-488`, `lower.rs:1344-1345, 1520-1522`):
   - Added `wave_why()` (`cfg!(feature = "diagnostics") && VICTORY_WAVE_WHY == "1"`) and summarized `WAVE64-RETRY` into a single line only when a shader stays stuck at `wave = 64`.
   - `victory-shader` `wave64` tests (`8 passed`) and `cargo build --release --features diagnostics` (`36.22s`) passed.

### 10.3 `copystat` & Remaining Items
- **`texture uploads: ... left out` (`TexSkipped`)**: Confirmed in `crates/victory-gpu/src/texture.rs:548-555` and `copystat.rs:86-88` — skips redundant `CopyTextureRegion` uploads when `uploaded && !dirty`; no texture data is lost.
- **Remaining items**:
  - Convert `HostPipeline::new` (`crates/victory-gpu/src/pipeline/host.rs:107-120`) to a `D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL1` stream when `DepthBoundsTestEnable` is enabled.
  - Reverse-engineer `unknown_pipeline::<0x1c8>` / `<0x218>` (`crates/victory-gpu/src/device.rs:349-400`).
  - Address remaining `WaveSize(64)` compute shaders (`id=622`) and `ceb5eb68cd99cb07` (`0x38`).

### 10.4 `WaveSize(64)` Refusal on 32-Lane Hosts & `ceb5eb68cd99cb07`
- **Targeted fix (`crates/victory-shader/src/emit/mod.rs:495-521`)**: Added a `finish` check in `emit()` returning `ShaderError::Unsupported { offset: 0, what: "64-lane waves on a 32-lane host" }` whenever a compute shader still finishes with `wave == Some(64)` on a `host_max_wave() == 32` device (due to LDS read/write inside a lane-branch region where `VICTORY_WAVE_WIDE` is off to avoid GPU hangs). This prevents emitting unusable `WaveSize(64)` DXIL and avoids `CreateComputePipelineState` `0x80070057` (`id=622`) errors.
- **`ceb5eb68cd99cb07` (`0x38`)**: Hits `Val::Missing("a descriptor's address or format read as data")` (`lower.rs:5486, 5569-5580`) because `view_address` only populates the 64-bit address (`dwords 0..1`) and not the console's hardware format/stride/size bits (`dwords 2..3`). Left untouched without a shader dump to avoid miscompiling bounds/format checks.
- **Verification**: `cargo test -p victory-shader --lib wave64` (`8 passed, 0 failed`) and `cargo build --release --features diagnostics` (`31.92s`, 0 warnings) passed.

### 10.5 Shader Dump & Depth Bounds Findings
- **Built-in shader dump (`crates/victory-gpu/src/pipeline.rs:139-149`)**: Setting `VICTORY_SHADER_SAVE=<dir>` already saves every shader's raw console container as `<hash>.<stage>.cdxil` (and translated `<hash>.<stage>.dxil` on success) — e.g. `ceb5eb68cd99cb07.compute.cdxil` can be dumped directly without code changes and disassembled with `victory inspect`.
- **Depth bounds test (`crates/victory-gpu/src/pipeline/host.rs:107-131` & `list.rs:1440-1445`)**: `GraphicsState::read` drops `DepthBoundsTestEnable` from `D3D12_DEPTH_STENCIL_DESC1` (subobject `21`) and `list.rs:1440` (`depth_bounds`, slot `68` / `0x220`) is currently a no-op stub. Wiring `DepthBoundsTestEnable` requires both converting `HostPipeline::new` / `mesh_pipeline` to `D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL1` streams when enabled *and* forwarding `OMSetDepthBounds(min, max)` on `ID3D12GraphicsCommandList1`.

### 10.6 Depth Bounds Test & `OMSetDepthBounds` Wired
- **`crates/victory-gpu/src/pipeline/host.rs`**:
  - Added `depth_bounds: bool` to `GraphicsState` (set from subobject `21` `D3D12_DEPTH_STENCIL_DESC1.DepthBoundsTestEnable` and included in `describe()`).
  - Added `graphics_bounds_pipeline` and `D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL1` stream support in `HostPipeline::new` and `mesh_pipeline` when `g.depth_bounds` is `true` (keeping existing non-bounds pipelines unchanged).
- **`crates/victory-gpu/src/draw.rs` & `list.rs`**:
  - Added `depth_bounds: Option<(f32, f32)>` to `State` (replayed on list cuts in `State::replay`), `draw::depth_bounds` calling `ID3D12GraphicsCommandList1::OMSetDepthBounds(min, max)`, and wired `list::depth_bounds` (slot `68` / `0x220`).
- **Verification**: `cargo test -p victory-gpu --lib pipeline::host` (`2 passed, 0 failed`) and `cargo build --release --features diagnostics` (`33.19s`, 0 warnings) passed.

### 10.7 Frame Pacing (`30.0 fps` Cap) & Texture Refresh Inspection
- **Root cause of `30.0 fps` pacing**: `host.rs:5975` calls `p.swap_chain.Present(0, DXGI_PRESENT(0))` (no host vsync cap), so the `33.333ms` pacing comes from the title calling `frame::set_interval(..., length = 33333)` (`crates/victory-gpu/src/frame.rs:45-68`), plus synchronous CPU/GPU submission waits (`host.rs:4-5`, unless `VICTORY_LATE_WAIT` is enabled).
- **Targeted fix (`crates/victory-gpu/src/frame.rs:42-71, 260-270`)**: Added `parse_interval_override` / `interval_override()` reading `VICTORY_FRAME_INTERVAL_US` once (`OnceLock`) and capping `set_interval` to `requested.min(cap)` (preserving exact default behavior when unset).
- **Texture refresh (`crates/victory-gpu/src/texture.rs:86-96`)**: `HASH_EVERY = 8` (1/8 of unwritten guarded textures per frame) and `RECENTLY_CHANGED = 16` (`2 * HASH_EVERY`) are already tuned to avoid stale video/cutscene frames; left unchanged.
- **Verification**: `cargo test -p victory-gpu --lib frame` (`2 passed, 0 failed`) and `cargo build --release --features diagnostics` (`23.21s`, 0 warnings) passed.

### 10.8 3D Frame-Time Degradation Analysis (`run.log:7002–8584`)
When 3D world content (`core_restofworld`) loads in, frame time in `run.log` degrades from `33.2 ms` (`30.1 fps`) to `1913–2488 ms/frame` (`0.4–0.5 fps`) due to four measured bottlenecks:
1. **CPU buffer hashing/snapshotting (`copies`: `773–1549 ms`, `150–220 MB/frame`) + `heap buffers` (`335–835 ms`) + `copying back` (`149–371 ms`)**: CPU compares hundreds of MB of title buffers per frame because `VICTORY_CARD_BUFFERS_MIN_MB=1` and `VICTORY_CARD_REHASH_RUNS=1` (`crates/victory-gpu/src/shadow.rs:145-155`, `host.rs:1492-1507`) are off by default.
2. **Texture refresh lock contention (`textures`: up to `1323–1668 ms/frame`)**: `waited(2, crate::watch::title_memory_reading)` (`crates/victory-gpu/src/texture.rs:702`) blocks up to `1577.4 ms` waiting for streaming threads to finish remapping title memory, alongside `23,222` queued `textures picked by index`.
3. **Synchronous CPU/GPU waits (`VICTORY_LATE_WAIT=0`)**: Every submission waits on the GPU before the next list is prepared (`crates/victory-gpu/src/host.rs:4-5, 1623-1690`).
4. **First-run pipeline compilation**: Up to `438 being made` pipelines compile during initial 3D entry (subsequent runs load from the pipeline library).

### 10.9 `VICTORY_RELAY=2` & `VICTORY_CARD_BUFFERS_MIN_MB=1` Spike Analysis (`run.log:8839–9066`)
- **`(VICTORY_RELAY=1)` log label (`crates/victory-gpu/src/host.rs:2499`)**: `VICTORY_RELAY=2` (`relay_exact()`) and `VICTORY_LATE_WAIT=2` were active, but `host.rs:2499` hardcoded `"(VICTORY_RELAY=1)"` in the `tracing::info!` format string (fixed to print `1` or `2` dynamically).
- **Why `copies` (`613.7 ms`, `168.4 MB`) & `361.5 MB` CPU checks still happen**: `crates/victory-gpu/src/shadow.rs:148-155, 211-219` caps card-side copies at `VICTORY_CARD_BUFFERS_MB` (default `512` MiB) and 80% of VRAM budget; once full, additional `>= 1 MiB` buffers fall back to CPU snapshotting/hashing.
- **Why `textures` (`1205.3 ms`) spikes**: `1146.7 ms` is spent in `waited(2, crate::watch::title_memory_reading)` (`texture.rs:702`) waiting for streaming threads to finish remapping physical pages while `20,573` tabled textures are queued.
- **Script adjustment**: Remove `$env:VICTORY_PIPELINE_WAIT_MS = "0"` in `run-game.ps1` (which caused `dropped 2722 without a host pipeline`), and optionally test `$env:VICTORY_CARD_BUFFERS_MB = "1024"` (if VRAM permits).

### 10.10 Heavy 3D (~0.1–2.3 fps) VRAM Budget & Copy-Path Analysis (`run.log:10090–11300`)
- **`7,319 title textures have no host copy yet (8,072 MB)` (`crates/victory-gpu/src/resource.rs:1365, 1469-1479`)**: Normal on-demand behavior for placed/undrawn textures (`passes that made none for lack of room: 0`) plus `15,254` waiting in `textures picked by index` (`12,597` uploaded = `3,945 MB`, `5,013` let go = `2,831 MB`, `344` made again = `400 MB`).
- **Why multi-hundred-ms to multi-second spikes occur (`copies: 213–5637 ms`, `heap buffers: 265–1198 ms`, `copying back: 146–295 ms`)**:
  - **VRAM oversubscription (`run.log:11300`)**: `6197 MB` in use of a `6862 MB` budget, with `5412 MB` spilled into shared PC memory, causing WDDM PCIe paging during copies/heap-buffer passes and tripping `shadow.rs:211-213`'s 80%-budget cap (`5489 MB`) for `card_for()`.
  - **D3D12 Debug Layer overhead**: `VICTORY_D3D12_DEBUG=1` validates every draw (`4,162 draws/frame`), command list, and resource barrier on CPU.
  - **`VICTORY_PIPELINE_WAIT_MS=0`** in `run-game.ps1`: Still dropping `1,322–3,122` draws/frame without a host pipeline.

### 10.11 `0.4 fps` / `2492.9 ms` Stall Breakdown (`run.log:10141–10148`)
At `3,949–4,275 draws/frame` (`run.log:10141-10148`), stalls are driven by:
1. **Per-submission & heap-buffer refreshes (`copies: 1654.4 ms`, `heap buffers 1.0 passes 1114.3 ms`)**: `59.3` submissions/frame trigger `22,485` buffer refreshes (`23,020.5 MB` checked, `moves 1357.91 ms`), with `3,231` write-watch checks scanning `15,475 MB` (`15,320 MB` unchanged).
2. **Card-copy piece fetches (`copying back: 260.8 ms`)**: `53,699` pieces compared on the card, `1,315` fetched (`85.5 MB`) with `22.5` extra waits (`217.7 ms`).
3. **Command-list recording (`recording per frame: 36540 list calls 2421.1 ms`)**: CPU list recording overhead across threads while `VICTORY_D3D12_DEBUG=1` is active.
4. **Texture refresh & queue**: `textures: 116.5 ms (48.2 MB)` with `13,591` slots waiting in `textures picked by index`.

### 10.12 Non-Debug-Layer 3D Bottlenecks (`run.log:10263–10395`)
With `VICTORY_D3D12_DEBUG` and `VICTORY_WAVE_WHY` disabled, `run.log:10263-10395` (`4,488–4,590 draws/frame`) improved from `2492.9 ms` (`0.4 fps`) to `647.0–1238.8 ms/frame` (`0.8–1.5 fps`), with remaining time dominated by:
1. **Command-list recording (`recording per frame: 43,203–45,249 list calls`, `1,454–1,458 ms`)**: High per-draw list/state recording + `#[cfg(feature = "diagnostics")]` `copystat` instrumentation (`crates/victory-gpu/src/copystat.rs:1089-1096`).
2. **Heap-buffer & copy-back passes**: `heap buffers 1.0 passes 321.9–349.3 ms` (`host.rs:5724-5726, 5798`) + `copying back 278.8–303.3 ms (72–82 MB)`.
3. **Buffer refreshes & write-watch scans**: `23,975–25,150 refreshes` (`24.4–25.0 GB` checked across `61.5–63.3` submissions/frame, `moves 88.2–121.5 ms`, `15.3–15.5 GB` unchanged in `3,206–3,278` watches, `20,797–25,574` page faults).
4. **GPU execution**: `229.0–267.3 ms on the GPU`.

### 10.13 `ceb5eb68cd99cb07` (`0x38`) Fixed & Verified
- **Disassembly (`shader_dumps/ceb5eb68cd99cb07.compute.cdxil`)**:
  - `0x28: s_load_dwordx4 s[0:3], s[4:5], null` loads a 4-dword buffer SRV descriptor (`table_offset_dwords: 0`, `stride: 16`) and `0x34: s_cmp_eq_u32 s2, 0` checks `dword 2` (`num_records == 0`) to branch to `0x4c0: s_endpgm` when empty/unbound.
- **Fix (`crates/victory-shader/src/emit/lower.rs:5566-5610`)**:
  - In `descriptor_data`'s `view_address` fallback (`count >= 3`), added `getDimensions` (`dx.op 72`, divided by `stride` when `stride > 1`) via `self.handle(site, &src, false, (11 | u, 0))` to populate `dword 2` (`num_records`), keeping `dword 3` as `Val::Missing`.
- **Verification**:
  - `victory.exe translate --pipelines shader_dumps\ceb5eb68cd99cb07.compute.cdxil`: `1 emitted of 1 compute programs; 1 pipelines`.
  - `cargo test -p victory-shader --lib lower`: `13 passed; 0 failed; 1 ignored`.
  - `cargo build --release --features diagnostics`: succeeded (`35.33s`, 0 warnings).

### 10.14 Shader Dump Fixes (`38bf0814e721a1b3`, 5 Descriptor-Dimension Pixel Shaders, `f64` Compute Shaders)
- **Bit 1 — `38bf0814e721a1b3.pixel.cdxil` (`"the first half of an interpolation"` at `0x1464`)**:
  - **Root cause**: `v_interp_p1ll_f16 v8, v0, attr3.w` (`0x00c0`) wrote `Val::Missing("the first half of an interpolation")` into `v8` while `v_interp_p2_f16 v27, v1, attr3.w, v8` (`0x0108`) wrote its result into `v27`, leaving `v8` dead. Later `v_fma_mixlo_f16 v8, ...` (`0x0290`) read `old = inst.read(Reg::V(reg))` to preserve the high 16 bits, propagating `Val::Missing("the first half of an interpolation")` to `0x1464: exp mrt1`.
  - **Fix (`crates/victory-shader/src/emit/lower.rs`)**: In partial-register writes (`v_fma_mixlo_f16` / `v_fma_mixhi_f16` at line 7082 and `v_permlane16_b32` / `v_permlanex16_b32` at line 7290), treat `Err("the first half of an interpolation")` on `old` as `0` (`self.s.m.i32(0)`).
  - **Verification**: `victory.exe translate --pipelines shader_dumps\38bf0814e721a1b3.pixel.cdxil` → `1 emitted of 1 vertex and pixel programs; 1 pipelines`.
- **Bit 2 — 5 Pixel Shaders (`"a descriptor read only as data"`): `2fd01fca16993001`, `e5b2121a02f4f741`, `6b6db9f1a526daeb`, `abc6a80d46edc294`, `7d0f14ce44746d7e`**:
  - **Root cause**: Each shader loads an 8-dword texture SRV (`s_load_dwordx8`) solely to extract width/height/mip dimensions (`GetDimensions`) from `T#` dwords `1..=3` without ever passing that loaded descriptor to an `image_*` instruction, so `descriptor_use(site)` returned `None`.
  - **Fix (`crates/victory-shader/src/emit/lower.rs:5619-5622, 5764-5815`)**: Added `image_dimensions_only()` for `s_load_dwordx8` loads tracked in `self.scalar`, emitting `dx.op 72` (`getDimensions`) for a 2D texture handle and populating `T#` dwords `1..=4` while marking address/format dwords as missing.
  - **Verification**: `victory.exe translate --pipelines` across all 5 pixel `.cdxil` files → `5 emitted of 5 vertex and pixel programs; 5 pipelines`.
- **Bit 3 — `f64` Compute Shaders (`5dc3b25986622949`, `6f225b641c5b0708`, `926c46bab3b86bd6`) & `WaveSize(64)` on 32-Lane Host**:
  - **Root cause**: All three shaders perform a 64-lane `v_add_f64` wave reduction followed by `v_cvt_f32_f64`.
  - **Fix (`crates/victory-shader/src/emit/bitcode.rs`, `shader.rs`, `wave.rs`, `lower.rs`)**:
    - Added `TypeDef::Double`, `Const::Double`, `Cast::FpTrunc`, `flags::DOUBLES` (`1 << 2`, `SFI0` bit `0x1`), `v_add_f64`, and `v_cvt_f32_f64`.
    - Added `Op::AddDLo` / `Op::AddDHi` and `Shape::Active64` in `wave.rs` + `lower.rs` (plus `s_mov_b32` scalar copy propagation inside whole-wave sections).
    - Guarded `lower.rs:1388` with `halves.is_none()` so `WaveSize(64)` is not forced when `halves` is allocated.
  - **Verification**: `5dc3b25986622949` and `6f225b641c5b0708` now emit DXIL (`2 emitted of 3 compute programs`, with `926c46bab3b86bd6` hitting `a lane mask holding lanes elsewhere read as data` at `0x13c8`); on this 32-lane GPU (`WaveLaneCountMax = 32`), `CreateComputePipelineState` still rejects `5dc3b25986622949`, `6f225b641c5b0708`, and `f133e35efed2a48a` with `Shader wants wave lane count of 64 but maximum wave lane count for device is 32` via the `with_local_waves` fallback path in `emit/mod.rs`.
- **Unit tests & release build**: `cargo test -p victory-shader --lib` → `211 passed; 0 failed; 2 ignored`.

### 10.15 Heavy-Draw Recording & Heap-Pass Optimizations (`4,000–5,000` Draws/Frame)
- **Root-cause findings**:
  - **`heap buffers` (`draw.rs:1314-1414`, `321.9–349.3 ms`)**:
    1. `State::heap_buffers` called `self.heap_textures(list, heap, host)` inside `HEAP_BUFFER_MICROS` (`draw.rs:1412-1414`), where `heap_textures` had a `250 ms` per-pass time cap (`const SPAN = 250 ms` at `draw.rs:1511`) on the recording thread.
    2. `State::heap_buffers` rebuilt `spans` from all ~83,000 `ranges.values()` on every frame even when `host.buffer_changes()` (`slots`) was empty.
  - **`recording per frame` (`list.rs:145-154`, `draw.rs:931-955`, `43,203–45,249` list calls)**:
    - Every `with_state` call runs on recording threads (`list.rs:145-154`), and `State::bind` (`draw.rs:932`) allocated `vec![0u32; dwords as usize]` on the heap for every stage of every draw (`~9,000+` allocations/frame at `4,500+` draws/frame).
  - **`copying back` (`host.rs:3480-3555`, `278.8–303.3 ms`)**:
    - `Host::card_back` records `CopyBufferRegion`s and synchronously waits on `self.event` (`WaitForSingleObject`) for GPU→CPU readback (`CARD_STATS[6]` extra waits); with `VICTORY_RELAY=2` this runs on the relay thread (`host.rs:2385`), overlapping with submission unless `settle` waits on it (`"settles"`).
  - **`926c46bab3b86bd6.compute.cdxil` (`0x13c8`, `MASK_ELSEWHERE`)**:
    - `0x13c8..0x1450` sits inside a loop (`0x13a8` / `0x42a0: s_cbranch_scc0 label_13B0`) using `s_cselect_b64` / `v_writelane_b32` / `v_cmpx_gt_f32`; the shader also contains `s_or_saveexec_b64` + `v_add_f64` wave64 reductions (`0x42b8`), so even past `0x13c8` it hits the 32-lane host `WaveSize(64)` limit.
- **Changes (`crates/victory-gpu/src/draw.rs`)**:
  - `State::bind` (`draw.rs:931-940`): Replaced per-stage `vec![0u32; dwords as usize]` with a stack `[0u32; 64]` buffer (heap fallback only when `dwords > 64`).
  - `State::heap_buffers` (`draw.rs:1314-1410`): Cached `spans` and `bytes` alongside `ranges` in `RANGES`, rebuilding only on `full_rescan` (`*passes % 16 == 0`) or when `!slots.is_empty()`.
  - `State::heap_textures` (`draw.rs:1521-1535`): Replaced hardcoded `250 ms` `SPAN` with `VICTORY_HEAP_TEXTURES_MS` defaulting to `16 ms`.
- **Verification**:
  - `cargo test -p victory-gpu --lib`: `114 passed; 0 failed; 1 ignored`.
  - `cargo build --release --features diagnostics`: succeeded (`31.66s`, 0 warnings).

### 10.16 Series S Black Screen (`1920x1080`) & `VICTORY_DISPLAY_MODE` Inspection
- **What `--console series-s` changes**:
  - `crates/victory-gpu/src/console.rs:22-27` (`Console::title_memory`): Switches from Series X `(10 GiB GPU-optimal, 3.5 GiB standard)` to Series S `(0 GPU-optimal, 7,776 MiB standard)`.
  - `crates/victory-gpu/src/dxgi.rs:146-156` (`adapter_description`): Reports `dedicated_video_memory = 0` and `shared_system_memory = 7,776 MiB` (no other GPU cap differences in `console.rs` or `dxgi.rs`).
  - `crates/victory-hle/src/modules/xmem.rs:874-881` (`XMemGetWorkingSetStatistics`): Reports `title_limit = 7,776 MiB - TRANSFERRED`, `gpu_optimal_limit = 0`, and `title_used = counters.PagefileUsage` (which includes host/emulator commit).
  - `crates/victory-hle/src/modules/xsystem.rs:110, 257`: Reports `"Xbox Series S"` and Series S `console_mode`.
- **Why `amd_fidelityfx_upscaler_xbox.dll` logs as refused (`run.log:2545`)**:
  - `crates/victory-hle/src/modules/kernel.rs:297-348` (`find_module(path, load)`): The initial `GetModuleHandle` probe (`load = false`) skips `runtime::load_title_dll` (`line 307`) and logs `WARN a host DLL outside the host policy: refused` (`line 335`). Immediately afterward (`run.log:2547-2549`), `LoadLibrary` (`load = true`) maps it at `0x7ffff99d0000` (`7 hle, 79 host, 0 title, 0 missing`), so the DLL loaded cleanly.
- **Why Series S stays black (`11 draws/frame`, `run.log:2550, 4331-4598`)**:
  - `XMemGetWorkingSetStatistics` reports `limit_mb=7776 used_mb=5895` (`75.8%` of budget consumed at boot by `PagefileUsage`), and the title remains stuck in its `11 draws/frame` boot/UI loop (`0` `textures picked by index`, `0` world buffer copies, `black_frames=1`). Unblocking Series S requires more than `console.rs`/`dxgi.rs` tweaks.
- **Why Series X ignores `VICTORY_DISPLAY_MODE=1920x1080`**:
  - `dxgi.rs:227-278` (`display_size()`) only changes `IDXGIOutput::GetDisplayModeList`, which Series X ignores when choosing its `3840x2160` render target.
  - `crates/victory-gpu/src/host.rs:5880-5903, 5947` (`Host::present`): Sizes `IDXGISwapChain3` to the presented title texture's `(Width, Height)` and blits via `CopyResource(&back, texture)` (requiring identical dimensions). Capping the swapchain alone under Series X would require a fullscreen downscale blit in `Host::present`, and the title would still render `3840x2160` internally.

### 10.17 `XMemGetWorkingSetStatistics` Title-Owned `title_used` Tracking
- **Root cause (`crates/victory-hle/src/modules/xmem.rs:874-881`)**: `XMemGetWorkingSetStatistics` previously set `title_used = counters.PagefileUsage`, reporting Victory's host commit (`5895 MB` at boot against Series S's `7776 MB` `title_limit`).
- **Fix (`crates/victory-hle/src/modules/xmem.rs:50-54, 162-164, 310, 334, 874-891`)**:
  - Added `VIRTUAL_COMMITTED` (`XMemVirtualAlloc` commits with `MEM_COMMIT`) and `HEAP_BYTES` (`XMemAllocDefault`/`XMemFreeDefault` non-`ALLOC_32BIT` blocks) alongside `physical().pages.len() * PAGE`.
  - Set `title_used = tracked.max(PAGE).min(title_limit.saturating_sub(PAGE))` and removed `K32GetProcessMemoryInfo` (`PagefileUsage`).
- **Verification**: `cargo test -p victory-hle --lib` (`95 passed; 0 failed`) and `cargo build --release -p victory` succeeded with 0 warnings.

### 10.18 Series S Null RTV (`id=613`) & `R9G9B9E5_SHAREDEXP` Render Target Fix
- **Root cause (`run.log:8246, 8260, 13718-14160`)**:
  - `crates/victory-gpu/src/resource.rs:253` (`target_format`): Only `86` (`B5G5R5A1_UNORM`) and `115` (`B4G4R4A4_UNORM`) had stand-ins, so `1920x1080` `format: 67` (`DXGI_FORMAT_R9G9B9E5_SHAREDEXP`) with `flags: 5` (`ALLOW_RENDER_TARGET | ALLOW_UNORDERED_ACCESS`) failed `CreateCommittedResource` with `0x80070057` (`E_INVALIDARG`).
  - `crates/victory-gpu/src/draw.rs:524-538` (`State::set_targets`): Bound typed null RTVs (`host.rtv(None, Some(&desc))`) whenever `host_resource(record.resource)` was `None`, and `State::prepare_for` still issued `DrawInstanced`, hitting D3D12 `id=613` (`0x887A0001`).
- **Fixes**:
  - `crates/victory-gpu/src/resource.rs:253-260`: Added `(67, 10)` (`R9G9B9E5_SHAREDEXP` → `R16G16B16A16_FLOAT`) to `STAND_INS` in `target_format`.
  - `crates/victory-gpu/src/pipeline/host.rs:300, 538, 567-582, 684-696`: Added `rt_mask: u8` to `HostPipeline` (bits for non-`UNKNOWN` `g.targets.RTFormats`).
  - `crates/victory-gpu/src/draw.rs:161, 517-529, 786-790`: Tracked `null_targets: u8` in `State::set_targets` and skipped draws in `State::prepare_for` when `self.null_targets & p.rt_mask != 0`.
- **Verification**: `cargo test -p victory-gpu --lib` and `cargo build --release -p victory --features diagnostics` succeeded.

### 10.19 Series S `PresentX` / Null-RT Check & `xmem` `VIRTUAL_COMMITTED` Fix
- **Series S `PresentX` & `draw.rs` findings (`run.log:145-226, 7270, 15976`)**:
  - Section 10.18's `null_targets & p.rt_mask != 0` guard (`crates/victory-gpu/src/draw.rs:786-790`) dropped **0** draws in `run.log` (both during boot/UI and in 3D at `frame=3774`, `5712 draws/frame`).
  - `[vertex dfafaf385b2f94e8, pixel 06295fb1d1aefb78]` was bound later on Series S (`frame=3361` at `run.log:7270-7271`), while `[vertex 0602c2384acbb3b2, pixel a78d088fa55aa495]` was never bound.
  - `PresentX` (`crates/victory-gpu/src/queue.rs:818-837`) only reads `plane_parameters[0]` and `*plane.resources`, and `Host::present` (`crates/victory-gpu/src/host.rs:5873-5976`) copies that single texture (`resource=0x145668567b0`, `1920x1080 format=28`, `state=0x4`) to the swapchain. Added a one-shot diagnostic log in `crates/victory-gpu/src/queue.rs:827-839` whenever `planes > 1 || plane.count > 1` so multi-plane presents are surfaced immediately on the next run.
- **`xmem` `used_mb=7700 / 7776` root cause (`crates/victory-hle/src/modules/xmem.rs`)**:
  - `XMemVirtualAlloc` previously incremented `VIRTUAL_COMMITTED` unconditionally on every `MEM_COMMIT`, while `XMemFreeDefault` (`ALLOC_32BIT` calling `VirtualFree(..., MEM_RELEASE)`) and direct `VirtualFree(..., MEM_DECOMMIT / MEM_RELEASE)` calls never decremented it, and re-commits double-counted.
- **Fix (`crates/victory-hle/src/modules/xmem.rs:52-144, 253, 420-431, 984-1015, 1444-1463`)**:
  - Replaced `VIRTUAL_COMMITTED` with sorted, merged `[start, end)` intervals (`VIRTUAL_COMMITS`) via `record_virtual_commit`, `remove_virtual_range` (in `XMemFreeDefault` for `ALLOC_32BIT`), and `virtual_committed_bytes()` (pruning non-`MEM_COMMIT` sub-ranges via `VirtualQueryEx`).
  - Logged `physical_mb`, `virtual_mb`, and `heap_mb` in `XMemGetWorkingSetStatistics` and added `virtual_committed_tracks_recommits_and_frees`.
- **Verification**:
  - `cargo test -p victory-gpu --lib -- --test-threads=1`: `114 passed; 0 failed; 1 ignored`.
  - `cargo test -p victory-hle --lib`: `96 passed; 0 failed`.
  - `cargo build --release -p victory`: succeeded in `21.54s` with 0 warnings.

### 10.20 Multi-Plane `PresentX` Compositing for Series S
- **Root cause (`run.log`)**:
  - `PresentX` logged `planes=2 plane0_count=1 resource=0x217da6936d0` on `frame=1`. `crates/victory-gpu/src/queue.rs` previously read only `plane_parameters[0]` and `crates/victory-gpu/src/host.rs` copied only that single texture via `CopyResource`, discarding `plane_parameters[1]` (where the title renders UI/overlay/video).
- **Fixes**:
  - `crates/victory-gpu/src/queue.rs:753-919` (`detect_plane_stride`, `present`): Detects the `D3D12XBOX_PRESENT_PLANE_PARAMETERS` array stride across candidate 8-byte-aligned sizes (`24..=128`) by validating `token`, `count`, aligned `resources` pointer, and `host_resource`, collects all active plane resources (`0..planes.min(8)`), logs each plane's resource, dimensions, format, and `color_space` on first multi-plane sight, and calls `Host::present_planes`.
  - `crates/victory-gpu/src/host.rs:840-989, 6020-6239` (`CompositePass`, `Host::present_planes`): Keeps the fast `CopyResource` path (`Host::present`) when `planes.len() == 1`, and for `planes.len() >= 2` renders a fullscreen triangle pass with bilinear filtering (`COMPOSITE_VS` / `COMPOSITE_PS`) that draws plane 0 opaque and blends planes `1..` with premultiplied alpha (`ONE` / `INV_SRC_ALPHA`) onto the swapchain backbuffer.
- **Verification**:
  - `cargo test -p victory-gpu --lib -- --test-threads=1`: `114 passed; 0 failed; 1 ignored`.
  - `cargo build --release -p victory`: succeeded in `22.67s` with 0 warnings.

### 10.21 Series S 1080p Swapchain Sizing, 3D Plane Probe & Dropped Pipeline Inspection
- **Swapchain sizing fix (`crates/victory-gpu/src/host.rs:6153-6156`)**:
  - `run.log:134-136` showed `plane 0` is `1920x1080` (`format=28`) and `plane 1` is `3840x2160` (`format=28`). Updated `Host::present_planes` to size the swapchain to `plane 0` (`(desc0.Width as u32, desc0.Height)`) instead of `max` across all planes, so `plane 1` downscales onto the `1920x1080` backbuffer.
- **3D plane probe (`crates/victory-gpu/src/host.rs:989-1097, 6202-6255, 6364-6366`)**:
  - Added `record_plane_probe` / `log_plane_probe` to sample 5 pixels from `plane 0` and `plane 1` on the first >100-draw frame (`frame_draws > 100`) and log `plane0_max_rgba` vs `plane1_max_rgba` (including alpha) in both release and diagnostics builds.
- **3D `dropped 620..4330 without a host pipeline` findings (`run.log:100-114, 7499-7560, 9946`)**:
  - `run.log:7499-7560` logged 20 compute shaders failing translation with `malformed shader: no DXIL part` (`6a6c7b7fde59134f`, `f46117391dd81461`, `8cbd7f5b9ce7530c`, `f5ab5dcf95536794`, `85dea520c893a3d9`, `32aa8384586e0f12`, `8fb5e4aa9c542c3f`, `47d8c225abfcb246`, `55c46e739fa0796a`, `80a0178d3a9d8d12`, `477b52e73ddeb6f2`, `17af85f170e66f30`, `8b437ab61d8008fe`, `8e614708143ef917`, `3d147e8190574516`, `c99e30956be0deaf`, `f7fc6c984427fef3`, `528278df1274cb33`, `fdce2a0a9fb1521a`, `ba578b59c8c28a57`), alongside unimplemented `slot=0x218`/`0x1c8` pipeline creation calls (`run.log:100-114`) and the disabled `XboxBvhBuilder.dll` ray-tracing structure builder (`crates/victory-gpu/src/pipeline.rs:200-220`).
  - Note: per-reason `dropped {:.1}/frame without a host pipeline: {why}` logging (`crates/victory-gpu/src/host.rs:5971-5979`) is gated behind `#[cfg(feature = "diagnostics")]`, so non-diagnostics runs only print the aggregate count.
- **Verification**:
  - `cargo test -p victory-gpu --lib -- --test-threads=1`: `114 passed; 0 failed; 1 ignored`.
  - `cargo build --release -p victory`: succeeded in `25.54s` with 0 warnings.

### 10.22 `VIctory-7900XTX-ref` CPU Speedup Audit & Safe Port (NVIDIA Wave32 / 16 GB RAM / 8 GB VRAM)
- **(a) Audited in `C:\Users\Ree\Documents\VIctory-7900XTX-ref`**:
  - `README.md`, `docs/architecture.md`, and `crates/victory-gpu/src/` (`shadow.rs`, `host.rs`, `watch.rs`, `list.rs`, `draw.rs`, `texture.rs`, `queue.rs`).
  - Confirmed `GTAVI Port` already had `first_difference`/`last_difference` (`DIFF_CHUNK = 32` SIMD compares), `Batch::index` `HashMap`, `VICTORY_ASK_ONCE`, `VICTORY_LIST_POOL`, `VICTORY_SCRATCH_POOL`, `VICTORY_JOIN_COPIES`, and `VICTORY_CARD_REHASH_RUNS`, while `VICTORY_WIDE_REFRESH`, `VICTORY_WIDE_BACK`, and `VICTORY_RELAY` were present but **off by default**.
- **(b) Skipped for NVIDIA Wave32 / 16 GB RAM / 8 GB VRAM safety**:
  - `VICTORY_SNAPSHOT_POOL=1024` (kept `GTAVI Port`'s `8` cap to avoid multi-GB private RAM commit).
  - `VICTORY_CARD_ALL=1`, `VICTORY_CARD_WINDOWS_MB`, `VICTORY_WINDOWS_BAR_MB`, `VICTORY_KEEP_SNAPSHOTS=1` (avoid overflowing 8 GB RTX 2060 Super VRAM / 16 GB RAM).
  - `VICTORY_ASYNC_SUBMIT` (`host/aside.rs`, `host/send.rs`), `VICTORY_THIN` (`list/thin.rs`), `upload.rs` / `host/upq.rs` (high-RAM multi-frame staging / large architectural rewrites), and title-specific `VICTORY_PORT_*` memory patches (`port.rs`, `port_descriptors.rs`).
- **(c) Ported changes (exact files/lines)**:
  - `crates/victory-gpu/src/host.rs:5417-5505, 5582-5607`: Enabled `VICTORY_WIDE_REFRESH` by default on `>= 4` logical CPUs (`wide_refresh_enabled`, `VICTORY_WIDE_REFRESH=0` disables), and added `VICTORY_WIDE_TEXTURES` (`wide_textures()`, `Batch::due`, `Batch::refresh_textures()`) to refresh due textures in parallel via `shadow::wide_run` (`VICTORY_WIDE_TEXTURES=0` disables).
  - `crates/victory-gpu/src/shadow.rs:471-477, 962-966`: Enabled `VICTORY_RELAY` by default in non-test builds (`VICTORY_RELAY=0` disables) and `VICTORY_WIDE_BACK` by default (`VICTORY_WIDE_BACK=0` disables).
  - `crates/victory-gpu/src/watch.rs:26-108, 281, 336-352, 430-450` and `crates/victory-gpu/src/shadow.rs:884-949, 2417-2476`: Ported `VICTORY_PAGE_PARTS` (4 KiB sub-page dirty masks inside 64 KiB pages via `Journal::note`, `parts_written`, and `by_parts` in `Shadow::copy_stale`, `VICTORY_PAGE_PARTS=0` disables).
- **(d) `VICTORY_*` env flags for `run-game.ps1`**:
  - All ported optimizations (`VICTORY_WIDE_REFRESH`, `VICTORY_WIDE_TEXTURES`, `VICTORY_WIDE_BACK`, `VICTORY_RELAY`, `VICTORY_PAGE_PARTS`) are **enabled by default** on 4+ core CPUs with `=0` opt-out overrides if needed for A/B testing (`VICTORY_WIDE_REFRESH=0`, `VICTORY_WIDE_TEXTURES=0`, `VICTORY_WIDE_BACK=0`, `VICTORY_RELAY=0`, `VICTORY_PAGE_PARTS=0`). Optional: `VICTORY_WIDE_MERGE=1` can also be tested in `run-game.ps1` for parallel copy-back merges.
- **Verification**:
  - `cargo test -p victory-gpu --lib -- --test-threads=1`: `116 passed; 0 failed; 1 ignored` (`host::batch::texture_checks_per_submission`).
  - `cargo build --release -p victory`: succeeded in `24.43s` with 0 warnings.

### 10.23 NVIDIA-Safe Rendering, Shader & CPU Audit + Port (`VIctory-7900XTX-ref`)
- **(a) Rendering & Shader Fixes Audited, Ported vs. Skipped**:
  - **Ported**:
    1. **Cube-map explicit gradient `log2` & min-LOD clamp** (`crates/victory-shader/src/emit/lower.rs:9738-9744`, `emit/mod.rs:1777-1848`): Removed the erroneous `LOG2_E` multiplier on DXIL `op 23 Log` (which is already base 2, previously inflating cube mip levels by `1.44x`) and applied `has_cl` min-LOD clamp via `op 35`. Fixes blurred cube reflections.
    2. **Exact UNORM/SNORM buffer load precision** (`crates/victory-shader/src/emit/lower.rs:6022-6240`, `norm_quotient` & `exact_norm`): Replaced plain float division with exact integer/bit quotient rounding + `at_top` / `at_bottom` endpoint clamping so `65535` loads as `1.0` and integer material indices don't truncate one ULP low.
    3. **Pixel-shader helper-lane waterfall fix** (`crates/victory-shader/src/emit/mod.rs:162-167`, `emit/lower.rs:4189-4211`): Added `helper_own()` (`VICTORY_HELPER_OWN=0` disables, ON by default) and `isHelperLane.i1` (`dx.op 221`) in `first_lane()` so helper lanes keep their own value and don't stall waterfall loops or miss quad derivatives along triangle edges.
    4. **Compute-written texture readback (`texback`)** (`crates/victory-gpu/src/texture.rs:1229-1501`, `resource.rs:1783, 1811, 2145-2177`, `watch.rs:726`, `host.rs:1872-1875, 2921-2923, 4691-4716`, `draw.rs:2737`): Added `texture::back` (`VICTORY_TEXTURE_BACK`, default `256x256f41,256x256f16,64x64f16,128x128f36`, `=0` disables) so compute-written ocean height/displacement maps copy back and tile into title memory with allocation/remap invalidation.
    5. **Indirect-light stand-in (`VICTORY_AMBIENT`)** (`crates/victory-gpu/src/draw.rs:193, 927-930, 1765-1840, 2822-3383`, `pipeline.rs:64, 117-119`): Ported screen-space ambient stand-in (`ambient::role`, `ambient::after`, OFF unless `VICTORY_AMBIENT` is set) for dark interiors/faces when ray tracing is off (~765 KB GPU buffers, zero RT dependency).
    6. **Series S 1080p swapchain & multi-plane probe** (`crates/victory-gpu/src/host.rs:6153-6156, 6202-6255` from Sections 10.20–10.21): Preserved `Host::present_planes` sizing to `plane 0` (`1920x1080`) with bilinear compositing of `plane 1` over `plane 0` and the one-shot 3D plane RGBA probe (`log_plane_probe`).
  - **Skipped for NVIDIA Wave32 / Stability Safety**:
    - `VICTORY_WAVE_WIDE=1` default (kept OFF by default; barrier insertion inside branches/loops caused driver hangs on GPU).
    - `VICTORY_INDIRECT_WIDE` `1 << 20` groups (AMD-only path in `ref/indirect.rs:281`; NVIDIA remains clamped at `65_535`).
    - `VICTORY_BVH_BUILDER=1` / `-RayTracing` traversal & DXIL BVH intersection paths (`docs/fork-ray-tracing.md`).
    - Unimplemented `0x1c8`/`0x218` slots and `no DXIL part` compute shaders (`ref` likewise logs and drops them).
- **(b) CPU & Copy Speedups Audited (Ported vs. Skipped for 16 GB RAM Safety)**:
  - **Ported** (Sections 10.22–10.23): `VICTORY_WIDE_REFRESH` (default ON on 4+ logical CPUs), `VICTORY_WIDE_TEXTURES` (`Batch::due` + parallel `Batch::refresh_textures`), `VICTORY_RELAY` (default ON in non-test builds), `VICTORY_WIDE_BACK` (default ON), `VICTORY_PAGE_PARTS` (4 KiB sub-page dirty tracking inside 64 KiB pages), plus `DIFF_CHUNK = 32` SIMD compares & `Batch::index` already present.
  - **Skipped for 16 GB RAM / 8 GB VRAM Safety**: `VICTORY_SNAPSHOT_POOL=1024` (kept `8` cap), `VICTORY_KEEP_SNAPSHOTS=1`, `VICTORY_CARD_ALL=1`, `VICTORY_CARD_WINDOWS_MB`, `VICTORY_WINDOWS_BAR_MB`, `VICTORY_ASYNC_SUBMIT` / `VICTORY_THIN` / `upload.rs` copy-queue rewrite, and `VICTORY_PORT_*` title memory patches.
- **(c) Preserved**: `crates/victory-game/src/lib.rs` (and all `lib.rs` files) untouched; Sections 10.17 (`xmem`), 10.18 (`R9G9B9E5` / null RTV guard), and 10.20–10.22 preserved.
- **(d) Recommended `VICTORY_*` Flags for `run-game.ps1` (RTX 2060 Super 8 GB / Ryzen 5 5600 / 16 GB RAM)**:
  - **Defaults now ON (no flag needed)**: `VICTORY_WIDE_REFRESH`, `VICTORY_WIDE_TEXTURES`, `VICTORY_WIDE_BACK`, `VICTORY_RELAY`, `VICTORY_PAGE_PARTS`, `VICTORY_TEXTURE_BACK`, `VICTORY_HELPER_OWN`.
  - **Recommended to test in `run-game.ps1`**:
    - `$env:VICTORY_SHADER_FRESH = "1"` (or delete old shader cache once) so the new cube `log2`, UNORM/SNORM, and helper-lane shader fixes re-translate cleanly.
    - `$env:VICTORY_AMBIENT = "0.8"` (or `"constant:0.15"`) + optionally `$env:VICTORY_AMBIENT_AIR = "1.5"` to light dark interiors/faces with RT off.
    - `$env:VICTORY_WIDE_MERGE = "1"` (parallel copy-back merges into title memory).
    - Keep `$env:VICTORY_CARD_BUFFERS_MB = "1024"` and `$env:VICTORY_TEXTURE_BUDGET_MB = "1536"` for 8 GB VRAM headroom.
- **Verification**:
  - `cargo test -p victory-shader --lib`: `212 passed; 0 failed; 2 ignored`.
  - `cargo test -p victory-gpu --lib -- --test-threads=1`: `120 passed; 0 failed; 1 ignored` (`host::batch::texture_checks_per_submission`).
  - `cargo build --release -p victory`: succeeded in `32.98s` with 0 warnings.










