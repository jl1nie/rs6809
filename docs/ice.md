English | [日本語](ja/ice.md)

# ICE (in-circuit emulator) and OS-9 monitor

The emulator has ICE-style debugging built in, driven from a browser GUI and from editors (VS Code and others) over the Debug Adapter Protocol (DAP).
The same screen shows NitrOS-9 system calls (primitives), processes, the scheduler, modules and memory.
It uses no external crates: a standard-library HTTP and DAP server plus a single HTML file (`src/ice/ui.html`).
Loading LWASM and Microware RMA listings and link maps adds symbols to the disassembly and the stack, and lets you break and step by C or assembly source line.

## Starting

```sh
sh examples/nitros9-ice.sh                 # Level 1, http://127.0.0.1:6809/
sh examples/nitros9-level2.sh --ice        # Level 2 (after building the disk)
sh examples/nitros9-ice.sh 127.0.0.1:8080  # change the listen address
```

With `--ice [address]` the ICE runs instead of the normal execution loop. The default listen address is `127.0.0.1:6809`.
`--ice-halt` starts in the stopped state right after reset. Automatic input with `--boot` and `--script` works as before.
It cannot be combined with `--until`, `--max-cycles` or `--measure-from`.

The UI and the ICE's messages (stop reasons, system call descriptions and so on) are in English by default, and in Japanese when `LC_ALL`, `LC_MESSAGES` or `LANG` (the first one that is set) starts with `ja`.
`--lang ja|en` overrides the environment. The language selector in the header switches it while running (`POST /api/lang?lang=ja|en`).

The console (ACIA 1, `/term`) appears in the terminal on the right side of the browser. Click the terminal and keystrokes go straight to the guest.
The terminal screen is 80 columns × 24 rows by default, with the font scaled to fit the panel. Choosing "Fit the panel" in the header makes it as many columns × rows as fit in the panel, and rebuilds it when the panel size changes (contents are kept; rows pushed off the top go to the scrollback).
Exactly the screen's rows are displayed; the scrollback is visible when you scroll back up. The current size is shown in the header.
A serial terminal cannot tell the guest its size automatically, so the browser reports the size (`/api/console/size`) and the ICE writes it into the OS's terminal size
(for FUZIX, the `winsize` of the first terminal `ttydata[1]`; it is fixed again if `stty` in `/root/.profile` at login resets it).
Writing it does not raise `SIGWINCH`, so a running program does not redraw; programs started afterwards get the new size. Nothing is written for NitrOS-9 and CP/M-68K.
The terminal has 2000 lines of scrollback and interprets VT52 (FUZIX's `TERM`) and the main ANSI/xterm controls
(cursor movement and positioning, screen and line erase, color, bold, underline, reverse, scroll region, line and character insert/delete, cursor save and restore, position report).
The type is selected in the console header and remembered per OS (default VT52 for FUZIX, ANSI for the others). Arrow keys, Home, End, PageUp, PageDown, Insert and Delete are sent in the form of the selected type too.
The interpreter is tested with `node scripts/test-ui-term.js`.
The input field below sends one line with a CR. Combined with Telnet mode (`--telnet`), the additional ACIAs are on the Telnet side, and ACIA 1 is also visible in the browser.

## Layout

```text
 Browser (ui.html)   ──HTTP/JSON──  HTTP threads (ice/http.rs)
 VS Code etc.        ──DAP/TCP───  DAP threads  (ice/dap.rs)
                                        │ mpsc (Inbox: HTTP requests, DAP messages)
                                        ▼
                         Emulator thread (ice/mod.rs: Ice)
                          ├ target.rs   owns the CPU and machine (Target: Mc6809 for 6809, Mc68000 for 68000)
                          ├ execution control, break checks, instruction trace
                          ├ disasm.rs   6809 disassembler (OS9 F$xxx notation)
                          ├ osmon.rs    OS plug-in point (OsMonitor) and system call recording
                          ├ os9.rs      NitrOS-9 implementation (reading structures, descriptions)
                          ├ symbols.rs  loading listings and maps, placing them at module locations
                          └ dap.rs      DAP request handling and events (stopped, output)
```

- Only the emulator thread owns the CPU and machine. HTTP threads pass requests over a channel, so the display is always a consistent state at an instruction boundary.
- While running, it executes instructions in slices of about 8 ms and handles requests in between. While stopped, it waits for requests.
- Debugger reads use `Machine::peek`. They cause no I/O side effects such as taking received data out of the ACIA.
  Writes (`poke`) go to RAM only and do not reach device registers.
- Clock advancement was split out of `Machine::step` into `Machine::tick`. The normal execution path is unchanged.

## ICE features

| Feature | Description |
|---|---|
| Run / stop | F5 / F6. A speed limit can be chosen around the CPU's nominal clock (6809 1 MHz, 68000 10 MHz), or max; slowing down lets you follow the OS by eye. At max, a guest waiting for the next tick (NitrOS-9 with nothing to run, shortly after an `F$Sleep`) runs at its nominal clock instead: every instruction still runs in the same order, only the host sleeps (the 68000's `STOP` is skipped to the next timer event) |
| Step | F11 steps one instruction. When an interrupt is accepted, the branch to the vector is one step |
| Step over | F10. Executes `JSR`, `BSR`, `LBSR`, `SWI` and `OS9` as one instruction, stopping when S returns in the same process |
| Step out | Shift+F11. Stops when S returns above the current value by `RTS`, `RTI` or `PULS PC` |
| Step back, reverse | With "Record" on, each instruction keeps the CPU before it and the old values of the RAM it wrote (the last 200,000 instructions). "◀ Back" (Shift+F10) undoes one instruction and "◀◀ Reverse" (Shift+F5) runs backwards to a breakpoint or the start of the recording. Devices (console, timers, disk) are not undone: running forward again writes to them again, and interrupts may then come at other places. Editing registers or memory, reset and loading a state end the recording. Running is slower while recording |
| Run to line | Double-click a disassembly line |
| Breakpoints | PC match. Checked with a 64 KB bit table; on resume the current PC is skipped once |
| Conditions, hit counts, logpoints | Any breakpoint can carry a condition (`A==5 && [X].w!=0`), a hit count (`N` from the Nth time on, `=N` only the Nth, `%N` every Nth, counting the times the condition held) and a log message (`A={A} X={X}`: it logs and goes on). Set them in the fields under the breakpoint input or with ✎ on a listed breakpoint; DAP's `condition`, `hitCondition` and `logMessage` do the same. Conditions are evaluated only when the PC reaches the breakpoint. A condition that fails to evaluate stops and says why in the log |
| Disassembly navigation | ▲▼ move 4 instructions, ⇞⇟ a page (also ↑↓ PageUp PageDown with the panel focused, and the wheel at either end). → after a branch or call goes to its target, ← returns. Scrolling up retraces the way down; otherwise instruction starts are found from the trace and the labels |
| Watchpoints | Read, write or both on a range of CPU logical addresses. Stops after the instruction completes |
| Register / memory editing | While stopped, click a register value or memory byte to change it |
| Memory view | CPU logical space, OS-9 system space, first 64 KB of physical RAM; as bytes, words or longs. Search for hex bytes or a `"string"` from the shown address on ("Find next") |
| Watch expressions | Expressions evaluated at every stop (and every second while running): C variables of the stopped function, registers, `[address]` (`.b` `.w` `.l`), symbols and operators, as in breakpoint conditions. Changed values are highlighted; the list is kept in the browser |
| MMU view | The two task maps (8 banks each), the running task, write protection, fixed upper region |
| Execution trace | Registers and instruction bytes of the last 4096 instructions, disassembled when displayed |
| Symbols | The disassembly shows label lines, source lines and branch target names (`LBSR $AA0E <_stkcheck>`). Symbol names can be typed in the go-to field |
| Source line / symbol breaks | Given as `sum`, `hello.c:19` and so on. Automatically re-placed at the address where the module is loaded |
| Stack | 32 words from S. Values pointing just after a call instruction are shown as return addresses (↩), with symbols |
| Reset | "↺ Reset", `/api/reset`, DAP `restart`. Returns to the state at startup and replays the boot's automatic input (`--boot`, `--script`) from the start. Disk contents stay as they are. Breakpoints and symbols are kept; system call records, trace and OS detection start over. On the 6809, with a boot ROM it initializes the MMU and so on and starts from the reset vector; without one it returns to the RAM and registers as loaded |
| Saving / loading state | "Save", "Load", `/api/machine/save`, `load`, `--load-state` (68000 only). Puts the CPU, RAM, devices (DUART, IDE) and the contents of a disk whose writes are not kept in its file into one file. You can resume, for example after logging in, without waiting for the boot |
| Skipping waits | While the 68000 waits for an interrupt in `STOP`, it does not run instructions; it advances time to the next timer and sleeps the host for that real time (an idle guest uses almost no CPU) |

## Symbols and source lines

```sh
rs6809 --rom ... --disk ... --boot --ice --symbols artifacts/level2/echo.lst --symbols artifacts/cdebug/hello --source-dir examples
```

`--symbols` can be repeated and loads a file or a directory (its `.lst` files, and `.map` files without a matching `.lst`).
They can also be loaded from the browser's "Symbols" panel or the DAP `attach`.

| Input | Contents |
|---|---|
| LWASM listing (`lwasm --list`) | Labels, the code range of each line, source text. A module starting with `$87CD` is treated as module-relative offsets |
| LWLINK map (`Symbol: name (file) = XXXX`) | Absolute-address symbols |
| Microware RMA listing (`c.asm -l`) + `.map` of the same name (`c.link -m -s`) + `.m` (`c.prep -l`) | C function symbols, module offset and C line of each instruction |
| GNU as listing (`as -al`, starting with `68K GAS`) + `.map` of the same name (output of `nm -n`) | Code range and source text of each line of 68000 assembly. `.text` addresses are derived from the address of the same label in the `.map` (CP/M-68K BIOS) |
| `nm` output (`0001b000 T _init`) | Absolute-address symbols |
| CP/M-68K NM68 output (`CPM15000.MAP`, `_init 1B000 equ global abs`) | Absolute-address symbols of the CCP and BDOS |

- `file@modulename` places an LWLINK map or listing in that module; `file@abs` treats it as absolute addresses.
- OS-9 modules are position-independent, and where they load changes from run to run. A table of module offsets is placed by
  finding the module of the same name in the module directory and mapping it to its **physical RAM range**.
  Level 1 uses logical addresses as they are; Level 2 derives physical addresses from the blocks in the module's DAT image,
  so the same table works whichever process space it runs in. Placement is updated on return from `F$Link`, `F$Load`, `F$Fork`, `F$Chain`, `F$UnLink`,
  `F$Exit` and so on, on process switches, and on stop and resume.
- Source line and symbol breakpoints are checked with a bit table of module offsets converted to physical addresses.
  Set before the module loads, they become active when it loads (placed before the code runs).
- File names in LWASM listings are truncated on the left (`s/source-echo.asm`), so files matching the tail are searched for in the listing's directory, one level below it,
  and `--source-dir`. If none is found, the source is shown from the text in the listing.
- Local labels (`l@`, the C compiler's `_3` and so on) are shown only at their exact address; global labels are used as the base for "name+offset".

### Source-level debugging of C

Microware C's `.a` normally has no line number information. With `c.prep -l`, the preprocessor leaves each source line
in the assembly as a comment starting with `* *`, and writes records of "file name (`#7`) and source line (`#6`)" to `.m`.
The RMA listing (`c.asm -l`) has each instruction's section offset and those comments in the same order,
and the `c.link -m -s` map gives the section's (`hello_c`) position in the module and the function symbols.
Matching these three, each instruction is mapped to the source line before it.

```sh
python3 scripts/build-c-disk.py            # first time only (build-level2-disk.py for Level 2)
python3 scripts/c-debug-build.py examples/hello.c            # Level 1 C development disk
python3 scripts/c-debug-build.py examples/hello.c --level2 --out artifacts/cdebug/level2-hello
```

The script writes the host C file to `/dd/SOURCES` on the disk, boots the guest in the emulator, runs

```text
c.prep -l hello.c >hello.m → c.pass1 → c.pass2 → c.asm hello.a -l >hello.lst → c.link ... -m -s >hello.map
```

and extracts `hello.lst`, `hello.map` and `hello.m` to `artifacts/cdebug/hello/`.
The executable is `/dd/SOURCES/hello`. To keep lines and code matched, the optimizer (`c.opt`) is not run.
It writes to the disk image, so stop any emulator using the same image before running it.

```sh
./target/release/rs6809 --rom reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin \
  --disk artifacts/ccompiler/multicomp09-c.img --boot \
  --dap --symbols artifacts/cdebug/hello --source-dir examples
```

Running `/dd/SOURCES/hello` in the guest stops at breakpoints such as `hello.c:19`,
and you can step over, step in (to `sum`'s `{`) and step out by line.

- A line step stops on reaching an instruction of a different line in the same process and same MMU task.
  Step over skips deeper frames (S below its starting value), and does not stop inside libraries without line info (`printf` and so on).
  It also stops when an `RTS`/`PULS PC` in the debugged code returns to caller code without line info (`_cstart`).
- A function's epilogue (`leas`, `puls u,pc`) belongs to the line of the last statement. Constants such as `fcc` are not mapped to lines.

### C variables

When stopped at a breakpoint, arguments, local variables, `static` variables and globals are shown according to their types (the browser's "Variables" panel, DAP "Locals" and "Globals" scopes).
Selecting a caller frame shows that frame's variables too. Structures, arrays and pointer targets can be expanded,
and `char *` and `char` arrays are shown as strings. DAP `evaluate` (watch, hover, debug console) accepts expressions of the form
`n`, `s.x`, `p->next->val`, `arr[1]`, `*p`, and integer, character and pointer values can be changed from the variable list.

It is split into three parts. Only part 3 is compiler-specific.

| Part | File | Contents |
|---|---|---|
| 1. Describing and showing variables | `debuginfo.rs` | Types, locations (frame-relative, register, data-base-relative, absolute), frame rules (the frame base at each instruction, where the return address and saved registers are), value formatting, expression evaluation |
| 2. Parsing C declarations | `cdecl.rs` | Extracts globals, functions, arguments, per-block locals, structs and typedefs from the preprocessed source (contained in `.m`). K&R style and simple ANSI style |
| 3. Microware C adapter | `microware.rs` | Builds the descriptions of part 1 using the layout rules below |

Microware C leaves no symbol table, so the layout rules were confirmed from the compiled output of test programs and implemented.

- The frame base F is S right after the prologue's `pshs u`. The return address is at F+2, the caller's U (register variable) at F+0.
- Arguments are placed in order from F+4. `char`, `int` and pointers take 2 bytes (`char` is the low byte), `long` 4 bytes, `double` 8 bytes.
- Local variables are placed below F in declaration order without gaps. Variables in a block go further below the outer variables, and sibling blocks reuse the same space.
- Of the `register` variables, only the first one of `int` or pointer size goes in U (arguments first). The others (including `register char`) are ordinary locals.
- Globals and `static` variables are Y-relative. The address comes from the data symbol in the link map, or the section base (IDat/UDat) plus the offset within `vsect`.
- Variables are read relative to S, so the depth of S at each instruction of the function is derived from the listing's `pshs`, `puls`, `leas` and `,s++`.
  `bsr _4`, which places constants inline, is treated as pushing only 2 bytes, and runtime routines that clean up their own stack arguments (`_lmove`) are subtracted too.
- At run time it checks that "the value at F+2 of the derived base points just after an instruction that calls this function". If not, nearby depths are tried, so
  runtime routines with unknown stack effects do not put variables in the wrong place. Caller frames are walked the same way.
- The increment and condition code of `for`/`while` is emitted after the loop body, but is assigned to the loop's first line (the `for`/`while` line).
  A breakpoint on the statement right after a loop does not stop in the middle of the loop.

Not supported: floating-point values (shown in hex), struct arguments passed by value and the arguments after them, bit-field values,
`#define` constants. Local variables after a variable of unknown size are shown as "unknown".
`short` is 2 bytes, the same as `int`, and is shown as `int`.

## Editors (DAP / VS Code)

```sh
rs6809 ... --dap                 # 127.0.0.1:6810. Also enables --ice; usable together with the browser UI
rs6809 ... --dap 127.0.0.1:7000
```

The emulator itself is the DAP server. Each TCP connection is one session, and requests are handled on the emulator thread just like HTTP.
In the example scripts, `sh examples/cpm68k.sh --dap` and `sh examples/fuzix68k.sh --dap` also load the BIOS or kernel symbols and listen.
Stopping in the browser notifies the editor, and vice versa.

In VS Code, use the minimal extension in `editors/vscode` (no dependencies).

```sh
ln -s "$PWD/editors/vscode" ~/.vscode/extensions/rs6809.rs6809-debug-0.2.0   # restart VS Code
```

`.vscode/launch.json`:

```json
{
  "version": "0.2.0",
  "configurations": [
    {
      "type": "rs6809",
      "request": "attach",
      "name": "rs6809: attach",
      "port": 6810,
      "symbols": ["${workspaceFolder}/artifacts/cdebug/hello"],
      "sourceDirs": ["${workspaceFolder}/examples"]
    }
  ]
}
```

For the 68000, these can be created from the extension's configuration snippets ("rs6809: CP/M-68K", "rs6809: FUZIX/68000").

```json
{ "type": "rs6809", "request": "attach", "name": "rs6809: CP/M-68K", "port": 6810,
  "symbols": ["${workspaceFolder}/examples/cpm68k/bios.lst", "${workspaceFolder}/artifacts/cpm68k/dist/DISK8/CPM15000.MAP"],
  "sourceDirs": ["${workspaceFolder}/examples/cpm68k"] }
{ "type": "rs6809", "request": "attach", "name": "rs6809: FUZIX", "port": 6810,
  "symbols": ["${workspaceFolder}/artifacts/fuzix68k/fuzix.sym"] }
```

| DAP | Description |
|---|---|
| `attach` (`launch` is the same) | Loads `symbols` and `sourceDirs`. `stopOnEntry` pauses |
| `setBreakpoints` | Source lines. A line without code moves to the next line with code; for modules not yet loaded, a `breakpoint` event reports when they become active |
| `setFunctionBreakpoints` | Symbol name, or `$C000` |
| `setInstructionBreakpoints` | Addresses in the disassembly view |
| `restart` | Reset (same as the browser's "↺ Reset"). If stopped, stays stopped |
| `setExceptionBreakpoints` | "Error return" and "Process switch" (names come from the OS), and the conditional "System call" and "System call return" (names, comma-separated, in the `filterOptions` condition, e.g. `F_OPEN,P_LOAD`) |
| `stackTrace` | Walks with the frame rules if there are any; otherwise looks for return addresses on the stack (values pointing just after a call instruction that have a symbol). A return address shows the line of the call instruction |
| `scopes`/`variables`/`setVariable` | C locals and globals (expandable, editable), registers (editable), 32 stack words, OS-9 (process ID, module, state, latest system call) |
| `next`/`stepIn`/`stepOut` | By line. By instruction with `granularity: "instruction"`. By instruction where there is no line info |
| `stepBack`/`reverseContinue` | Back to the previous line (or instruction), or backwards to a breakpoint. Needs the recording: `"record": true` in launch/attach, or "Record" in the browser |
| `disassemble`/`readMemory`/`writeMemory` | CPU logical space. The disassembly carries symbols and source locations |
| `evaluate` | C expressions (`s.x`, `p->next`, `arr[1]`, `*p`), register names, symbols, `$1234`, `[expr]` (the word at that address). Typing `>text` in the debug console sends one line to the guest console |
| `source`/`loadedSources` | For sources not on the host, returns the text in the listing |
| `output` event | Sends the guest's console output (ACIA 1) to the debug console |

## Swapping the CPU

The ICE takes the `Target` trait from `target.rs` as a type parameter (`Ice<T: Target = Mc6809>`). Dispatch is static, so
per-instruction processing is as fast as calling the CPU directly (about 140 to 148 MHz on the 6809, both before and after the change).
The address, symbol and OS layers work with 32-bit addresses, truncated to the CPU's address width (`addr_bits`).

| Item | 6809 (`Mc6809`) | 68000 (`Mc68000`) |
|---|---|---|
| Machine | Multicomp or a bare 64 KiB (`Machine`) | Bare machine with RAM and console (`m68k::machine::Sim68k`, [68000 CPU core](m68k.md)) |
| Address width / return address | 16 bits / 2 bytes | 24 bits / 4 bytes |
| Call instruction length (return address check) | 2, 3, 4 bytes (`JSR`/`BSR`/`LBSR`) | 2, 4, 6 bytes (`BSR`/`JSR`; excluding `TRAP`) |
| Return instructions | `RTS`, `PULS PC`, `RTI` | `RTS`, `RTR`, `RTE` |
| Context (identity for line steps) | MMU task | Supervisor state |
| OS | NitrOS-9 (`Ice::new`) | None (`Ice::m68000`, `osmon::NoOs`) |

With `cpu` in `/api/state` (`"6809"`, `"68000"`), the screen switches the register panel, the number of address digits and the trace columns, and hides the MMU panel and OS tab when unused.
The 68000 registers are `d0` to `d7`, `a0` to `a7`, `sr`, `pc`, `usp`, `ssp`. `/api/reg` accepts `D0` to `D7`, `A0` to `A7`, `SP`, `PC`, `SR`, `CCR`, `USP`, `SSP`.
The DAP register view is built from the names, values and bit widths of `Target::reg_list` (address registers get symbols).

```sh
rs6809 --cpu 68000 --bin prog.s68 --ice --ice-halt   # S-record or raw (--load, --pc)
rs6809 --cpu 68000 --bin prog.s68 --dap             # from VS Code etc.
```

## Swapping the OS

The ICE's screen, HTTP API and DAP use OS-specific processing only through the `OsMonitor` trait in `osmon.rs`.
A second OS can be added by passing a type implementing this trait to `Ice::with_os` (6809) or `Ice::for_target` (`Ice::new` is NitrOS-9).
The CP/M-68K implementation is `cpm68k.rs` ([CP/M-68K](cpm68k.md)), and FUZIX is `fuzix.rs` ([FUZIX/68000](fuzix68k.md)).
On the 68000 it is selected with `--os cpm68k|fuzix|none`.

| Method | Description | NitrOS-9 |
|---|---|---|
| `use_symbols` | Receives the loaded absolute-address symbols (on each symbol load) | Not used (FUZIX: `udata_shadow`, `ptab`) |
| `detect` | Recognizes the OS from memory | `D.Init` points to the `Init` module |
| `info` | Name, title, queue names, memory map kind, exception break names (`/api/os/info`) | `OS-9`, `NitrOS-9 Level 2` and so on |
| `svc_at`, `svc_prefix`, `svc_mnemonic` | Whether an instruction is a system call, its number, length, and whether it returns. Disassembly notation | `SWI2` + number, `OS9 F$xxx` |
| `svc_regs` | For OSes that pass the number in a register, determines the number from the registers just before execution (default does nothing) | Not used (CP/M-68K: D0.W) |
| `internal` | Calls always treated as OS-internal, by number or caller address | Not used (CP/M-68K: BIOS calls, and BDOS calls from inside the BDOS) |
| `service_name` and others | Names and descriptions of services and errors | Tables generated from `defs/os9.d` |
| `current`, `current_slot`, `pid_of`, `in_system` | The running process and its ID, whether in system state | `D.Proc`, `P$ID`, `P$State` |
| `describe_entry`, `describe_exit`, `error_of`, `error_for`, `learn` | Describing arguments and results, error detection (`error_for` is per call kind; default `error_of`), learning path names and so on | See below |
| `processes`, `summary` | Process table (common columns + OS-specific columns) and queues | Process descriptors, `D.AProcQ` and so on |
| `images`, `image_calls` | Loaded images and their ranges in physical RAM (for symbol placement), and the calls that may change them | Module directory, `F$Link` and so on |
| `memory_map`, `read_space` | Memory usage, OS address spaces | `D.FMBM` / `D.BlkMap`, `sys`, `pidN` |

Call recording (the call tree, internal call counts, switch timeline) is done by `Tracer` in `osmon.rs` and does not depend on the OS.
To keep per-instruction processing light, it receives from the OS the first byte of system call instructions (`svc_prefix`) and the physical address of the running process's identifier (`current_slot`).
The process ID is computed only when a switch is detected.

The OS tab is drawn with the name and queue names from `/api/os/info`; the process table, modules and memory map use per-OS renderers (`OS_VIEWS` in `ui.html`).
An OS without a renderer gets a generic one that lists the fields as they are.

## The NitrOS-9 implementation

### Catching system calls

An OS-9 service request is `SWI2` followed by a one-byte function code (`OS9 F$Fork` = `10 3F 03`).

1. Before executing an instruction, if the instruction at PC is `10 3F`, it is recorded as a call.
   At this point the registers and arguments (path names, data to write, module names and so on) are read in the caller's space.
2. The kernel advances the PC on the stack past the function code and returns with `RTI`.
   The moment "PC = call address + 3, S = same as at the call, same process (`D.Proc` ID)" is taken as the return,
   and the output registers, the CC carry (error) and the error number in B are recorded.
3. If the same process has an unfinished call at the time of a call, that becomes the parent.
   This lets the display show a tree such as the `F$Sleep` a driver does inside the user's `I$ReadLn`, and the `F$NProc` inside that.
4. `F$Exit`, `F$Chain` and `F$NProc` are treated as calls that do not return. When a new call comes from user state,
   unfinished calls left for that process are discarded (a process in user state is not in the middle of a kernel call).
5. The mapping between path numbers and path names is learned from the results of `I$Open` and `I$Create`, and updated by `I$Dup`, `I$Close` and `F$Fork` (inheriting standard paths).
   Later `I$Write` calls and so on are shown as "path 1(stdout)", "path 3(echo)".

Calls in system state (inside the kernel and drivers) happen hundreds of thousands of times per second, so turning registers into text is deferred until display.
User calls and system calls are kept separately (the latest 15,000 and 60,000 respectively), so that many internal calls
do not push out the user call in progress.

### Reading kernel structures

Offsets follow `defs/os9.d` in the reference kit. Service names, error names and `SS.xxx` come from
`src/ice/tables.rs`, generated from that file by evaluating the Level conditions.

| Item | Level 1 | Level 2 |
|---|---|---|
| Detection | `D.Init` ($2A) points to the `Init` module | `D.Init` ($24) points to the `Init` module |
| System space | 64 KB flat | Block 0 is physical 0, the rest from the `D.SysDAT` DAT image |
| Running process | `D.Proc` ($4B) | `D.Proc` ($50) |
| Queues | `D.AProcQ`/`D.WProcQ`/`D.SProcQ` ($4D/$4F/$51) | Same ($52/$54/$56) |
| Process table | Page table in `D.PrcDBT`, 64-byte descriptors | `D.PrcDBT[ID]` is the high byte of a 512-byte descriptor |
| Modules | 4-byte entries in `D.ModDir` | 8-byte entries from `D.ModDir` to `D.ModEnd` ($58), read through the module's DAT image |
| Memory | Bitmap of 256-byte units in `D.FMBM` | 8 KB block flags in `D.BlkMap` |

The Multicomp Level 1 kernel also enables the MMU (identity map), so the Level is determined by the location of the `Init` module, not by the MMU state.

### Screen (OS-9 monitor)

- **Scheduler**: `D.Proc` and the active, waiting and sleeping queues. The timeline below shows run intervals derived from changes of `D.Proc`, and the user's system calls (▲).
- **Processes**: Parent-child relations as a tree, `P$State` flags (Sys, TSlp, TOut, Susp, Cond, Dead), queue, priority, age, path table, and on Level 2 the DAT image.
- **System calls**: Number, process, name, arguments, result, cycles taken. A call in progress is shown as "running…".
  Selecting a row shows what that primitive does in the kernel, the diff of input and output registers, and the tree of calls inside the kernel.
  From there you can set "Stop at this call" and "Stop on return".
- **Break conditions**: Before a particular service's call or after its return, error return, process switch, filtering by target process ID. Recording itself can be toggled too.
- **Module directory**, **memory map** (modules, process descriptors, in use and free, color-coded).

## Examples

Running `echo hello` shows the following sequence in the system call list (Level 1, process 2 is the Shell).

```text
P2 F$PrsNam   "echo"                 → length B=4
P2 F$Link     X→"echo"               → error E$MNF   not in memory, so
P2 I$Open     "echo"                 → path A=3      open the file in the execution directory
P2 I$Read     path 3(echo) max 13 bytes → "\x87\xCD…"   check the module header
P2 F$Load     X→"echo"               → header U=$B700  load and register it
P2 F$Fork     module "echo" params "hello\r" → child process ID=3
P3 I$WritLn   path 1(stdout) 6 bytes "hello\r"
P3 F$Exit     exit status B=0         (does not return)
P2 F$Wait     → child 3 exited, status 0
```

- Stop at the `F$Fork` call and step over, and you can watch process 3 being created and the scheduler view and process table change.
- With "Stop on error return" enabled, running `echo hi` first stops right after the Shell's `F$PrsNam` returns `E$BNam` at the end of the name.
  Resuming stops right after `F$Link` for `echo`, which is not in memory, returns `E$MNF`. In OS-9, error returns are part of normal control flow.
- Unchecking "User calls only" shows the driver repeating `F$Sleep` while `I$ReadLn` waits, the kernel switching to the next process with `F$NProc`,
  and the clock interrupt putting it back on the active queue with `F$AProc`.
- At 1 MHz (real-hardware speed) or below, you can follow process switches on the timeline by eye.

## HTTP API

Everything is JSON on `127.0.0.1`. Numbers default to decimal, addresses to hex (`$` and `0x` also accepted).

| Path | Description |
|---|---|
| `GET /api/state` | Run state, stop reason, registers, MMU, break settings, OS-9 summary, language (`lang`) |
| `POST /api/run` `pause` `step?n=` `over` `out` `runto?addr=` `speed?hz=` | Execution control |
| `POST /api/reset` | Reset (to the startup state; disk contents stay as they are) |
| `POST /api/machine/save?path=&disk=1\|0`, `POST /api/machine/load?path=` | Machine state file (path on the emulator side; `disk` defaults to 1 when disk writes are not kept in the file) |
| `POST /api/reg?name=&val=` | Change a register |
| `POST /api/bp?addr=&op=add\|del\|toggle\|clear` | Breakpoints |
| `POST /api/bp?addr=\|sym=\|id=&cond=&hits=&log=` | Condition, hit count and log message of a new breakpoint, or of an existing one (`addr`, or `id` for symbolic ones). Empty fields clear them; a wrong condition is refused with 400. `state` has `bpConds`, the same fields in `symBreakpoints` (with `count`) and `bpLog` (the latest log lines) |
| `POST /api/watch?addr=&len=&kind=r\|w\|rw`, `op=del&index=`, `op=clear` | Watchpoints |
| `POST /api/record?on=1\|0`, `stepback?n=`, `reverse` | Recording for stepping back, stepping back, running backwards (`record` in `state`: `on`, `depth`) |
| `GET /api/disasm?addr=&n=&back=` | Disassembly (`back` starts from an earlier instruction boundary). Branches and calls have `target` |
| `GET /api/mem?addr=&len=&space=cpu\|sys\|phys\|pidN`, `POST ...&val=hex` | Memory |
| `GET /api/mem/find?addr=&pat=hex` | The first address from `addr` holding the bytes (`{"addr":null}` if none) |
| `GET /api/eval?expr=&frame=` | A watch expression: `{"text":...}` for a C variable, `{"value":N}`, or `{"error":...}` |
| `GET /api/trace?n=` | Execution trace |
| `GET /api/console?since=`, `POST /api/console` (body to ACIA 1) | Console |
| `POST /api/os/break?code=&when=call\|return&op=del`, `errors=` `switch=` `pid=` `trace=` | OS break conditions (`osbreak` in `state`) |
| `GET /api/os/info` | OS name, title, queue names and so on (`OsMonitor::info`) |
| `GET /api/os/procs` `modules` `memmap` `services` | OS structures (summarized in `os` in `state`) |
| `GET /api/os/calls?since=&user=1&pid=&parent=&limit=` | System calls (`since` is an update number) |
| `GET /api/symbols`, `POST ...?path=`, `dir=`, `op=clear` | List and load symbol tables, source search directories |
| `POST /api/bp?sym=name\|file:line`, `symdel=ID` | Symbol and source line breakpoints (`symBreakpoints` in `state`) |
| `GET /api/resolve?expr=` | Resolve a symbol name or hex address |
| `GET /api/stack?n=` | Stack words, return address checks and labels, the call chain |
| `GET /api/vars?frame=` | The list of frames, and that frame's variables and globals (type, value, address, one level of expansion) |
| `GET /api/source?table=&src=` | Text of a loaded source |
| `GET /api/lang`, `POST /api/lang?lang=ja\|en` | The ICE's language (`{"lang":"en"}`); POST switches it |

Tests and scripts can call the same API directly with `Ice::command("GET", "/api/state", b"")`.

## Verification

```sh
cargo test --release --test ice
cargo test --release --test ice -- --ignored   # reference kit Level 1, built Level 2 disk
```

The `--ignored` Level 2 tests need a disk built with `python3 scripts/build-level2-disk.py`.
The C source-level tests additionally need the following debug builds.

```sh
python3 scripts/c-debug-build.py examples/hello.c
python3 scripts/c-debug-build.py examples/hello.c --level2 --out artifacts/cdebug/level2-hello
python3 scripts/c-debug-build.py examples/cvars.c
python3 scripts/c-debug-build.py examples/cvars.c --level2 --out artifacts/cdebug/level2-cvars
```

```sh
cargo test --release --test dap               # a DAP (TCP) sequence of operations
cargo test --release --test os_layer          # API and DAP with a toy OS (ToyOS)
cargo test --release --test os_snapshot -- --ignored --nocapture   # snapshots of the OS views and speed
cargo test --release --test m68k_ice          # HTTP API and DAP on the 68000
cargo test --release --test cpm68k -- --ignored   # CP/M-68K (needs scripts/fetch-cpm68k.py)
cargo test --release --test fuzix68k -- --ignored  # FUZIX (needs scripts/build-fuzix-tiny68k.sh)
```

- Disassembler, step over/out, breakpoints, watchpoints, memory and register operations.
- With a minimal Level 1 structure and a kernel-like handler that advances the PC on the stack and does `RTI`,
  checks stopping before an `I$WritLn` call and after its return, and the display of arguments and results.
- Boots the reference kit's NitrOS-9 Level 1 and checks `echo traced`'s `F$Fork`, the child process's `I$WritLn`, and the Shell and SysGo entries in the process table.
- Level 2: with synthetic memory laid out as defined in `os9.d`, checks reads through the system DAT, process descriptors and DAT images,
  the module directory, and the interpretation of the block map.
- Boots the built Level 2 disk, stops at the return of the `F$Fork` of startup's `echo`, and checks
  the SysGo→Shell→Shell→Echo process tree, the running process's task number, the child's active queue, the system process and system DAT,
  the location of Krn (offset $1000 in block 7), the blocks each module occupies, and the in-use flags of the block map.
- Symbols: LWASM module listings (`rmb` data offsets and `equ` are not made into labels), matching of RMA listing, map and `.m`.
- C variables (Level 1 and Level 2, `examples/cvars.c`): stops only once on the line right after a loop; checks the values of `char`, `int`, `long`, `unsigned`, arrays, structs,
  `register` (U), `char *`, globals, file-scope `static` and function-scope `static`, the scope of variables in blocks,
  an array in a caller frame and a pointer to it, expressions such as `nd.next->val`, and DAP scopes, expansion, evaluation and variable changes.
- C source level (Level 1 and Level 2): stops at `hello.c:19` set before loading, checks the `main+` label,
  the `LBSR` branch target `sum`, and the `_cstart+` return address on the stack. Steps in to `sum`'s `{`, steps out,
  steps over the `printf` line and checks the output, and steps by line from `return 0;` back to `_cstart`.
- OS plug-in point: with only a toy OS that treats `SWI` + number as a system call (ToyOS, `tests/os_layer.rs`), checks that
  OS info, system call breaks by name, disassembly notation and descriptions, call recording, the process table, images, memory map,
  OS address spaces, symbol placement in images, and DAP thread names, OS scope and exception breaks all work.
- Before and after building the OS layer, `tests/os_snapshot.rs` confirmed that the JSON of `procs`, `modules`, `memmap`, `calls`,
  `services`, the state summary and the disassembly at fixed stop points (return of `F$Fork`) on Level 1 and Level 2 had the same contents.
- Before and after swapping the CPU (stage 2b), confirmed that the JSON of `tests/os_snapshot.rs` above had the same contents and that speed did not drop.
- 68000 (`tests/m68k_ice.rs`): with small hand-assembled programs, checks step, step over/out (4-byte return addresses),
  breakpoints, watchpoints, register changes (switching stack pointers via SR), disassembly, memory above 64 KiB,
  trace and console output. Over DAP, checks instruction breakpoints, stack frames, register and stack display and changes, disassembly, evaluation,
  step out, memory reads and output events.
- DAP: connects over TCP to a small program assembled with LWASM and checks initialization, attach, source line (blank lines move to the next line) and function breakpoints,
  stopped events, stack frames and return address lines, register display and changes, step, step out, evaluation, disassembly and memory reads.

## Limitations

- The address of a Level 2 module (`MD$MPtr`) is a position within the module's DAT image. The screen shows it converted to a physical address, with the original value in a tooltip.
- Level 2 counters such as `P$UTicks` are not shown, because this kernel does not update them (for system processes, stack contents remain at the same location).
- Level 2 task numbers are assigned only while a process is running. Waiting and sleeping processes have 0 (unassigned).
- If you stop in the middle of `F$NProc`, a process waiting to run may not be on any queue.
  To look at a fixed point, stop with an OS-9 break condition (call or return).
- With recording enabled and the shell waiting, it runs at about 140 MHz equivalent (`ice_speed` in `tests/os_snapshot.rs`; instruction execution only, excluding HTTP handling; varies by host). That is dozens of times the speed of real hardware.
- Watchpoints are checked on CPU logical addresses. On Level 2 they also trigger on the same logical address in another process. Filtering by process ID is available only for OS-9 break conditions.
- By default it listens on localhost only, with no authentication. Do not expose it externally.
- Timers do not advance while stopped (the clock advances per instruction). A clock interrupt arriving during single-stepping is correct behavior.
- The return address check is heuristic (a value whose preceding 2 to 4 bytes decode as `JSR`/`BSR`/`LBSR`). When symbols are loaded,
  only values pointing into ranges with symbols become stack frames. Data can happen to meet the condition.
- When there are several modules of the same name, the symbols are placed in all of them. Whether the module contents match the listing is not checked.
  After changing the source, run `c-debug-build.py` again.
- DAP has no authentication either. By default it listens on localhost.

## Future work

- Conditional breakpoints (register and memory expressions), hit counts, log-only tracepoints.
- Cycle profiles per process and per module.
- Snapshots and reverse execution (rewinding with periodic state saves and the instruction trace).
- Adapters for other C compilers (CMOC uses U as a frame pointer, so the frame rules should be simpler).
- On Level 2, showing per-task MMU maps matched with process DAT images.
