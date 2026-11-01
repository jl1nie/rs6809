English | [日本語](README.ja.md)

# rs6809

A Motorola 6809 and 68000 emulator in pure Rust. It uses no external crates, no C code and no FFI; it runs on the standard library alone.

- **6809**: boots NitrOS-9 Level 1 / BASIC09 and Level 2 on a Multicomp-compatible machine. Level 2 supports several simultaneous users over Telnet, and there is a demo of the C compiler running on the guest.
- **68000**: a core that implements every instruction, exception and interrupt, verified against the public instruction tests (SingleStepTests, about one million cases).
  On a Tiny68K-compatible machine it runs CP/M-68K 1.3 (including compiling with Alcyon C) and the Unix-like FUZIX 0.5 (two terminals).
- **ICE**: from a browser debugger and from VS Code (DAP) you can use breakpoints, stepping, source lines and C variables.
  The OS monitor follows NitrOS-9, CP/M-68K and FUZIX system calls from call to return, and shows the process table, scheduler and memory with explanations.
  It can also reset the machine and save and load its state.

| Document | Contents |
|---|---|
| [ICE and OS monitor](docs/ice.md) | Debugger, OS plug-in interface, HTTP API, DAP |
| [68000 CPU core](docs/m68k.md) | 68000 implementation and verification, machine configuration |
| [CP/M-68K](docs/cpm68k.md) / [FUZIX/68000](docs/fuzix68k.md) | BIOS, disks, BDOS monitor / building FUZIX and the process monitor |
| [fork, exec, wait and OS-9's F$Fork, F$Wait](docs/fork-compare.md) | A comparison of the two OSes using traces recorded with the same ICE |

## Quick setup

NitrOS-9 (6809), CP/M-68K and FUZIX (68000) can be set up and run with a single shell command each.

```sh
git clone https://github.com/jl1nie/rs6809.git && cd rs6809
./setup.sh                 # fetch everything needed (on later runs, only what is missing)
./run.sh nitros9           # NitrOS-9 Level 1 / BASIC09 (6809, Multicomp)
./run.sh cpm68k            # CP/M-68K 1.3 (68000, Tiny68K). hello.c can be compiled with Alcyon C
./run.sh fuzix             # FUZIX 0.5 (68000, Tiny68K). A shell already logged in as root
./run.sh fuzix --ice       # any of these: --ice for the debugger (http://127.0.0.1:6809/), --dap for VS Code
```

`setup.sh` does the following, and checks every download against the SHA-256 sums in the repository (`release/images.sha256` and others).

| Stage | Contents |
|---|---|
| Emulator | With Rust 1.89 or later, `cargo build --release`. Without it (or if it is too old), fetches the executable from [Releases](https://github.com/jl1nie/rs6809/releases): Linux x86-64 (statically linked, works on any distribution) or macOS (Apple Silicon). Installs nothing on the system |
| NitrOS-9 | Fetches the boot ROM and SD image from commit `bf60370` of [nitros9-basic09-starter-kit](https://github.com/kyo-ta04/nitros9-basic09-starter-kit) (not redistributed, as there is no explicit license) |
| CP/M-68K, FUZIX | Fetches prebuilt images from Releases (the CP/M-68K system and disk, the FUZIX disk and kernel symbols) |
| Alcyon C | Fetches it from the original CP/M-68K 1.3 distribution (cpm.z80.de), writes it to the disk and runs `AS68 -I AS68INIT` (not redistributed, as its license terms are not stated. If it cannot be fetched, the system runs without C) |

It needs `curl`, `gzip` and `sha256sum` (or `shasum`); Alcyon C also uses `python3` and `unzip`.
The contents, licenses and source locations of each image are in the Release's `NOTICE.md` (`release/NOTICE.md`).
You can also use `./run.sh fuzix --persist` to keep FUZIX's writes (run `telinit 6` before quitting), and `./run.sh fuzix --tty2` for a second terminal (`telnet 127.0.0.1 6812`).

## Build and minimal example

Uses a Rust 1.89 or later toolchain (edition 2024 support).

```sh
cargo build --release
cargo run --example hello
cargo test
```

`hello` runs 6809 machine code and prints `Hello, 6809!` through a memory-mapped ACIA.

## 6809 and NitrOS-9

### NitrOS-9 / BASIC09

Uses the boot ROM and SD image from [kyo-ta04/nitros9-basic09-starter-kit](https://github.com/kyo-ta04/nitros9-basic09-starter-kit).
The verified commit is `bf60370cfa180636cde3ee305d089d94872de82a`.
If you have already fetched them, no clone is needed. You do not need to build the reference kit's C emulator or OS.

```sh
git clone --recursive https://github.com/kyo-ta04/nitros9-basic09-starter-kit.git reference/nitros9-basic09-starter-kit
git -C reference/nitros9-basic09-starter-kit checkout bf60370cfa180636cde3ee305d089d94872de82a
cargo run --release -- \
  --rom reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin \
  --disk reference/nitros9-basic09-starter-kit/nitros9-runtime/multicomp09_sd.img \
  --boot
```

After fetching and building, `sh examples/nitros9.sh` starts the same configuration.

`--boot` types `NITROS9` into CamelForth. Answer `Time ?` with something like `2026/10/30 12:00:00` and `OS9:` appears.
Running `basic09` takes you to the `B:` prompt.
Standard input follows the host's terminal settings. In a normal terminal, type a line and press Enter to send it. Quit with Ctrl-C.

An example that edits and runs a BASIC09 program automatically and prints `42`:

```sh
cargo run --release -- \
  --rom reference/nitros9-basic09-starter-kit/nitros9-runtime/6809M.bin \
  --disk reference/nitros9-basic09-starter-kit/nitros9-runtime/multicomp09_sd.img \
  --boot --script examples/basic09.tsv --until 42 --max-cycles 300000000
```

Each line of the script is `output to wait for<TAB>input string`. A CR is appended to the input, which is sent one character at a time, paced by the virtual CPU's cycle count.
Do not type into standard input during an automated run.

Boot test using the real code:

```sh
cargo test --release --test nitros9 -- --ignored --nocapture
```

This test checks the whole path ROM → NitrOS-9 → shell → BASIC09 → edit mode → `RUN test` → `42` → `B:`.
It needs the reference kit, so it is ignored by a normal `cargo test`.

### NitrOS-9 Level 2 (512KB / 1MB)

From the same reference kit as above (commit `bf60370cfa180636cde3ee305d089d94872de82a`),
you can build a Level 2 kernel for Multicomp and a dedicated disk.
It needs Python 3.12 or later, make, a host C compiler, curl and patch.
LWTOOLS 4.25 and ToolShed are set up in the working directory by the same procedure as in "C compiler".

```sh
python3 scripts/build-level2-disk.py
sh examples/nitros9-level2.sh
```

After boot, `NitrOS-9/6809 Level 2 V03.3.0`, `LEVEL2-STARTED` and `OS9:` are shown.
`mfree` shows the upper limit of physical memory and the free memory. The default 512KB uses up to `$7FFFF`;
starting with `--ram-kb 1024` uses up to `$FFFFF`.
The disk contains `shell`, `echo`, `del`, `mfree`, the full set of Microware C's original driver `cc1`
(`c.prep` to `c.link`, headers in `/dd/DEFS`, libraries in `/dd/LIB`), and BASIC09 (`basic09`, `runb`, `inkey`, `syscall`).
BASIC09 is taken as the same modules from the reference kit's verified BASIC09 disk.
`/dd/SOURCES` holds `hello.c`, `mandel.c` and `mandel.b09`.

```text
OS9:chd /dd/SOURCES
OS9:cc1 hello.c -f=/dd/SOURCES/hello
OS9:/dd/SOURCES/hello
Hello from C: 42
```

`cc1` creates a working shell script `c.com` and intermediate files `ctmp.<process number>.*` in the current directory.
If `-f=` is omitted, the executable is written to the execution directory `/dd/CMDS`.

```text
OS9:chd /dd/SOURCES
OS9:basic09
B:load mandel.b09
B:run mandel
```
In Level 2 each process has its own 64KB space, so `cc1`, which ran out of memory on Level 1, works.
Changes needed in the reference sources for Level 2 are applied only to the generated output; the reference kit and the Level 1 disk are left as they are.
For the reasons behind the changes, see [Notes on Level 2 support](docs/level2.md).

```sh
cargo test --release --test nitros9_level2 -- --ignored
```

This test checks booting from ROM, running startup, `mfree` in the shell, running `echo` as a separate process and
returning to the shell, compiling and running `hello.c` with `cc1`, and running `6*7` and `mandel.b09` in BASIC09.

Multicomp RAM is 512KB by default, selectable with `--ram-kb 64|128|256|512|1024`.
The old 64KB configuration is reproduced with `--ram-kb 64`.
The Level 2 kernel detects 512KB or 1MB at boot, so the same disk works with either.
Level 2 needs 512KB or more. You can also add `--ram-kb 1024` to the start scripts (for example `sh examples/nitros9-telnet.sh --ram-kb 1024`).
The CoCo 3-specific ROM, disks and GIME are not supported.

### Telnet and multiple users

One to four simultaneous sessions are available, according to the OS-9 Level 2 terminal configuration.
Each connection is assigned to a free ACIA and authenticated by OS-9's `login`. Processes and SIGHUP handling are also managed by OS-9.
It listens on localhost only, and the TCP port number is chosen automatically from free ports. File writes in Telnet mode are persistent.

```sh
python3 scripts/build-level2-disk.py --multi-user --acia-count 3
sh examples/nitros9-telnet.sh
```

From another terminal, connect to the `127.0.0.1:port` shown at startup and press Enter to log in.
For example, if it shows `127.0.0.1:49152`:

```sh
telnet 127.0.0.1 49152
```

The initial configuration has three terminals: `/term`, `/t0` and `/t1`. With `--acia-count 1|2|3|4` at build time,
you can change the OS-9 terminal descriptors and the number of `tsmon` instances started. Only terminals that are ready are accepted as free ACIAs,
and the number of simultaneous connections is decided by the OS-9 configuration. In Telnet mode the host's standard input is not used.

The initial accounts are `user1` to `user3`; random initial passwords are saved in
`artifacts/level2-multiuser/accounts.txt`. Authentication is done by OS-9's `login`, and
a disconnect is signalled as loss of DCD on the ACIA. OS-9's driver and its existing SIGHUP handler do the cleanup.

Files are stored in `artifacts/level2-multiuser/multicomp09-level2.img`.
Changes made after a command completes survive restarting the emulator. An exclusive lock prevents two writers on the same image.
If a multi-user disk already exists, the build script refuses to rebuild it.
Rebuilding with `--force` resets the stored files and the accounts.

To verify simultaneous logins, the connection limit, session isolation, reconnection and file persistence on real OS-9:

```sh
python3 scripts/test-telnet.py
python3 scripts/test-telnet.py --acia-count 4
```

The test disk is created in a temporary directory.
Configuration, disconnect behavior, storage location and verification steps are described in [Telnet connections](docs/telnet.md).

#### Mandelbrot with several users at once

Several Telnet users share one 6809, and each compiles `mandel.c` with `cc1` in their home directory and runs it.
Prepare the reference kit and the tools required for "C compiler", and run `cargo build --release`, beforehand.

```sh
python3 scripts/demo-mandelbrot.py --users 3
```

The script recreates a multi-user disk in `artifacts/level2-mandel/` and runs the following in order.

1. One user runs `cc1 mandel.c -f=/dd/username/mandel` and `/dd/username/mandel`, recording the reference image and time.
2. All users compile at the same time.
3. All users run their own `mandel` at the same time, and each image is checked against the reference.

The terminal shows each user's compile stage and number of rows drawn, and at the end compares the time taken alone and when run simultaneously.
`--users 1|2|3|4` sets the number of users, and `--skip-build` reuses an existing disk.
In the multi-user configuration, `hello.c`, `mandel.c` and `mandel.b09` are placed in each user's home.
Use `-f=` to direct output to your own home. If omitted, everyone writes to the shared `/dd/CMDS`.

To try it by hand, start it after the demo has created the disk, and connect from separate terminals.

```sh
sh examples/nitros9-mandel-telnet.sh
telnet 127.0.0.1 port
```

The accounts are in `artifacts/level2-mandel/accounts.txt`.
The emulator's standard output shows only the boot log, so output does not get mixed even if you use telnet in the same terminal.

### C compiler

Uses a disk with Microware C's stages, headers and standard library, plus a small `cc` driver for 64KB.
The original BASIC09 image is kept. To create the C development image:

```sh
python3 scripts/build-c-disk.py
```

The first build needs Python 3.12 or later, make, a host C compiler and curl.
It fetches LWTOOLS 4.25, checks its SHA-256, and builds LWASM and the reference kit's ToolShed in the working directory.
Nothing is installed on the system. To use existing tools, set `LWASM` and `OS9_TOOL` to the executable paths.
The host C compiler is used for building these. Compiling C for the guest runs on the 6809.

```sh
sh examples/nitros9-c.sh
```

After entering the date and time, in the OS9 shell:

```text
chd /dd/SOURCES
cc hello
./hello
```

`Hello from C: 42` is shown. The sample is `examples/hello.c`.
It is K&R-style C and runs functions, loops, integer arithmetic and `printf`. Syntax and the standard library differ from modern ISO C.

`cc hello` or `cc hello.c` takes a single source in the current directory and runs the following in order.

```text
c.prep → c.pass1 → c.pass2 → c.opt → c.asm → c.link
```

`cc` accepts only a file name; paths, multiple sources and extra options are not supported.
The output executable is `hello` in the same directory, and the intermediate files are `hello.m/.i/.a/.o/.r`.
If a stage fails, it stops there. Microware's original `cc1` and `make` are also included, but
in this 64KB configuration running through `cc1` ran out of memory, so use the verified `cc`.

The preprocessor uses `/dd/DEFS`, and the linker uses `/dd/LIB/cstart.r` and `/dd/LIB/clib.l`.
If you change the sample sources on the host, rerun the disk build script.
Rebuilding the disk resets the C development image. With the current design, executables and changes made on the guest are lost on exit.

To compile and run automatically:

```sh
sh examples/nitros9-c.sh --script examples/ccompiler.tsv --until 'Hello from C: 42' --max-cycles 300000000
```

Tests for C compilation, recompilation and returning to the shell after a failure:

```sh
cargo test --release --test ccompiler -- --ignored
```

### Mandelbrot set C demo

From the repository root, build the emulator command, create the C development disk,
and compile and run the C benchmark on the guest, in that order.
First fetch the reference kit as above and prepare the tools required for "C compiler".

1. Build the emulator command on the host.

   ```sh
   cargo build --release
   ```

   The executable is `target/release/rs6809`.

2. Create a disk containing the C source and Microware C, and start the emulator.

   ```sh
   python3 scripts/build-c-disk.py
   sh examples/nitros9-c.sh
   ```

   Answer `Time ?` with a date and time such as `2026/10/31 12:00:00`.

3. At the guest's `OS9:` prompt, compile the C benchmark.

   ```text
   chd /dd/SOURCES
   cc mandel
   ```

   `c.prep` through `c.link` run, and the command `mandel` is created in the same directory.

4. Run it on the guest.

   ```text
   ./mandel
   ```

   A 79-column by 25-row Mandelbrot set is drawn and `MANDELBROT-DONE` is shown.
   Quit an interactive run with Ctrl-C.

To automate everything from disk creation through compiling, running and one speed measurement:

```sh
sh examples/mandelbrot.sh
```

`measured_cycles` on standard error is the number of virtual CPU cycles for the drawing section, and the following `host_seconds` is
the time taken on the host in seconds. Boot and compile time are not included in the drawing section.

To get the median of seven measured runs after one warm-up run, after creating the disk:

```sh
python3 scripts/benchmark-emulator.py --workload basic09 --runs 7 --output artifacts/benchmark/mandelbrot-c.json
```

This uses the already built `target/release/rs6809`.
Results are shown on screen and saved to the given JSON file. If you change the C source, recreate the disk.

It is a direct port to C of the BASIC09 `PROCEDURE mandel` given in an image.
79 columns by 25 rows, X=-39 to 39, Y=-12 to 12, floating-point arithmetic, up to 16 iterations; diverging points are shown as 0 to F and interior points as spaces.
The initial value z=c and the divergence check after the update also match the original BASIC09.
The source is [examples/mandel.c](examples/mandel.c), and the automated input is `examples/mandelbrot.tsv`.
Running and comparing the original BASIC09 and the C version confirmed that **all 1,975 characters match**.
The drawing time of this floating-point version was a median of **1.673355 seconds** over seven runs, 704,530,641 virtual cycles (equivalent to 421.0MHz).
Measured excluding boot and compile; [details and reproduction steps](docs/ja/mandelbrot-basic09-port.md#c移植版の速度ベンチマーク) (Japanese) are saved.

The original BASIC09 source is also included as `/dd/SOURCES/mandel.b09`.
Run the comparison again with `python3 scripts/compare-basic09-port.py`.
For the details of the port and the SD driver fix, see [Notes on the port](docs/ja/mandelbrot-basic09-port.md) (Japanese).

The earlier 76×25 fixed-point version is kept as `benchmarks/mandel_fixed.c`.
Emulator performance comparisons continue with the frozen old disk, and are not mixed with the current demo.

### 6809 machine configuration

| Item | Standard machine | Multicomp-compatible mode for NitrOS-9 |
|---|---|---|
| Physical RAM | 64KB | 512KB by default (64KB to 1MB selectable) |
| Boot ROM | None | `$E000–$FFFF`, 8KB, can be disabled |
| ACIA status/control | `$8018` | `$FFD0`, extra terminals at `$FFD2`, `$FFD4`, `$FFD6` |
| ACIA data | `$8019` | `$FFD1`, extra terminals at `$FFD3`, `$FFD5`, `$FFD7` |
| Timer | `$8010` | `$FFDD` |
| SD data/control/LBA | None | `$FFD8–$FFDC` |
| Mapping control | None | `$FFDE–$FFDF` |

- CPU: the defined 6809 instructions, all addressing modes, CC, IRQ/FIRQ/NMI, SWI/SWI2/SWI3, SYNC/CWAI, instruction cycle accounting.
- ACIA: up to four. Receive queue, transmit output, receive and transmit IRQs, DCD, master reset. Transmission completes immediately.
- Timer: every 20,000 cycles by default. 50Hz at 1MHz. There is no throttling to real time.
- SD: 512-byte blocks. Persistent with an exclusive lock in Telnet mode or with `--persist-disk`. The other default modes and `--volatile-disk` use an in-memory overlay.
- MMU: 8KB banks, two independent task maps, per-bank write protection. Unpopulated banks return `$FF` and ignore writes. The fixed upper area `$FE00–$FFFF` shares physical bank 7 and is always readable and writable, except for I/O.
- Emulation is per instruction. Bus-cycle-accurate timing, undefined instructions, 6309 extensions, and ACIA baud rate, parity and error flags are not handled.

To load an arbitrary binary into RAM:

```sh
cargo run --release -- --bin program.bin --load 0x1000 --pc 0x1000 --max-cycles 1000000
```

From the library, `Cpu` and `Bus` let you embed just the CPU. `Machine` is an implementation with RAM, console and timer.
`Machine::multicomp()` provides 512KB, and `Machine::multicomp_with_ram(rom, disk, ram_bytes)` provides RAM of the given size.
The public field `ram` is the physical memory as a `Box<[u8]>`. `load()` loads directly into the low 64KB, as before.
Instruction errors and disk read errors are reported as a failed run.
`--until` exits successfully when the output reaches the given string, and fails if the cycle limit is reached first.

## 68000, CP/M-68K and FUZIX

### 68000 CPU core

`src/m68k/` has a core that runs every 68000 instruction, exception and interrupt with per-instruction cycle counts, and a disassembler.
With `--cpu 68000` you can run programs on a bare machine with only RAM and a console, or on a Tiny68K-compatible machine (`--machine tiny68k`: 68681 DUART, IDE), and examine them with the ICE (browser, DAP).
It is verified against about one million cases from SingleStepTests/680x0 (including address error frames and cycle counts). For details, see [68000 CPU core](docs/m68k.md).

```sh
python3 scripts/fetch-68000-tests.py
cargo test --release --test m68k_sst -- --ignored --nocapture
rs6809 --cpu 68000 --bin prog.s68 --ice --ice-halt   # http://127.0.0.1:6809/
```

### CP/M-68K and the BDOS monitor

Runs CP/M-68K 1.3 on the Tiny68K-compatible machine with a custom BIOS (`examples/cpm68k/bios.s`).
In the ICE's OS tab you can observe BDOS and BIOS calls (name, arguments, result), the loaded program and memory regions, and stop on a call or return by name.
C can also be compiled and run with Alcyon C. For details, see [CP/M-68K](docs/cpm68k.md).

```sh
sh examples/cpm68k.sh            # first run fetches the distribution and creates the disk. A> appears
sh examples/cpm68k.sh --ice      # ICE: http://127.0.0.1:6809/
```

### FUZIX/68000 and the process monitor

Boots FUZIX 0.5 (Tiny68K version), a small Unix-like OS, from disk without a monitor ROM, and runs commands in the shell.
In the ICE's OS tab you can observe system calls (`_fork`, `execve`, `waitpid` and others, with name, arguments and result), the process table and a timeline of process switches.
For details, see [FUZIX/68000](docs/fuzix68k.md); for a comparison with OS-9, see [fork, exec, wait and OS-9's F$Fork, F$Wait](docs/fork-compare.md).

```sh
sh scripts/build-m68k-elf.sh          # first time only. m68k-elf binutils and gcc
sh scripts/build-fuzix-tiny68k.sh     # build FUZIX and its disk
sh examples/fuzix68k.sh --ice         # http://127.0.0.1:6809/
sh examples/fuzix68k.sh --tty2        # second terminal: telnet 127.0.0.1 6812
```

The ICE has reset ("↺ Reset", DAP's `restart`) and saving and loading the 68000 machine's state ("Save", "Load", `--load-state`).

## ICE (GUI debugger) and OS monitor

A debugger operated from the browser, equivalent to an in-circuit emulator. Without external crates, the emulator itself serves HTTP on localhost.

```sh
sh examples/nitros9-ice.sh            # NitrOS-9 Level 1 → http://127.0.0.1:6809/
sh examples/nitros9-level2.sh --ice   # Level 2 (after building the disk)
./run.sh cpm68k --ice                 # CP/M-68K (BDOS monitor)
./run.sh fuzix --ice                  # FUZIX (process monitor)
```

- **Debugger**: run, stop, step, step over / out, run to line, breakpoints, watchpoints,
  disassembly (in `OS9 F$Fork` form), memory (CPU logical, OS-9 system space, physical), register editing, MMU map, execution trace, speed limit.
- **OS-9 monitor**: traces every `OS9` system call from call to return, showing arguments, results and cycles taken.
  It shows an explanation of each primitive, the tree of calls made inside the kernel, the scheduler queues and timeline, the process table,
  the module directory and the memory map, and lets you set ICE break conditions such as "stop on a call to F$Fork" or "stop on an error return".
- **Symbols and source lines**: labels the disassembly and stack from LWASM and Microware RMA listings and link maps (`--symbols`),
  and lets you break and step by C or assembly source line. OS-9 modules are placed automatically at the address where they are loaded (Level 1 / 2).
  C is compiled on the guest with line information by `python3 scripts/c-debug-build.py examples/hello.c`,
  and arguments, local variables and globals can be shown and changed according to their type (expanding structs, arrays and pointers, expressions such as `p->next->val`).
- **CP/M-68K and FUZIX monitors**: show BDOS and BIOS calls, FUZIX system calls such as `fork`, `execve` and `waitpid`, the process table,
  the timeline and memory regions on the same screen ([CP/M-68K](docs/cpm68k.md), [FUZIX/68000](docs/fuzix68k.md)).
- **Reset and state**: "↺ Reset" returns to the state at startup, and on the 68000 "Save", "Load" and `--load-state` let you resume from a saved state.
- **Editor integration**: `--dap` provides the Debug Adapter Protocol, so VS Code (`editors/vscode`) and others can drive the same ICE.

The UI language is English by default, and Japanese when the locale (LC_ALL / LC_MESSAGES / LANG) starts with `ja`; `--lang ja|en` overrides it, and the language selector in the header switches it while running.

For details, internals and the API, see [ICE and OS-9 monitor](docs/ice.md).

## Speed

We measured how many guest cycles advance per second in CPU-bound work (AMD Ryzen 9 9900X, WSL2, rustc 1.99.1, November 2026).

| CPU | Workload | Average per instruction | Without ICE | Inside ICE | Ratio to real hardware (without ICE) |
|---|---|---|---|---|---|
| 6809 | Drawing the C Mandelbrot on NitrOS-9 Level 1 (`scripts/benchmark-emulator.py`, median of 3 runs) | 5.9 cycles | About 391MHz equivalent (about 66 million instructions/s) | About 175MHz equivalent (about 30 million instructions/s) | About 390x, taking real hardware as 1MHz |
| 68000 | Compiling, linking and running `hello.c` with Alcyon C on CP/M-68K (375.5 million cycles, 3 runs) | 13.7 cycles | About 750MHz equivalent (about 55 million instructions/s) | About 300MHz equivalent (about 22 million instructions/s) | About 75x, taking Tiny68K as 10MHz |

- "MHz equivalent" is the guest's clock count. The 68000 takes 4 clocks per memory read or write, and at least 4 clocks per instruction, so it needs far more clocks per instruction than the 6809 (1 clock per read or write), which gives the larger number.
  Compared by instruction count, the 68000, with its heavier effective addresses and 32-bit arithmetic, is about 20% slower. The average per instruction was measured while running, with the ICE's `/api/step?n=`.
- Inside the ICE it is slower, because each instruction checks breakpoints, records the trace and tracks system calls.
- While the 68000 is waiting for a guest interrupt (`STOP`), no instructions are run and time advances in real time (the host CPU is barely used while the guest is idle). The values above are for when the CPU is fully used.
- How the 6809's performance was investigated and the history of the optimizations are in "Version 0 and performance measurement" and "Optimizing for Rust" below.

## For developers

### Rebuilding the Release images

```sh
sh scripts/build-m68k-elf.sh && sh scripts/build-fuzix-tiny68k.sh && python3 scripts/fetch-cpm68k.py
sh scripts/make-release.sh   # creates artifacts/release and the image lines of release/images.sha256
gh release create images-v1 artifacts/release/* --title "rs6809 images v1" --notes-file release/NOTICE.md
gh workflow run release-binaries -f tag=images-v1   # build the executables (Linux x86-64, macOS arm64) and upload them to the Release
# when done, update the rs6809-* lines of release/images.sha256 and commit
```

### Version 0 and performance measurement

The current state is saved as version 0 (v0), together with the source, executable and input hashes.
The median of seven Mandelbrot measurements was 1.481785 seconds (equivalent to 331.2MHz).
The biggest hotspot is memory reads, about 34% of the latter half of the drawing.
For details and the order of improvements, see [Version 0 performance investigation](docs/ja/v0-performance.md) (Japanese); the machine-readable record is [v0-baseline.json](docs/v0-baseline.json).

```sh
python3 scripts/benchmark-emulator.py
```

Uses the existing release executable and the C development disk, and measures seven times after a warm-up.
The default is the current BASIC09 port.
Results are saved to `artifacts/benchmark/current.json`.
To compare with past measurements, specify `--workload fixed`. This mode needs
the inputs of the old fixed-point version recorded in `benchmarks/fixed-inputs.json` and saved in this working environment.

### Optimizing for Rust

The release build uses opt-level=3, ThinLTO and codegen-units=1.
Normal memory access is split into small functions, and I/O handling is moved into separate functions.
In addition, address translation and protection attributes under the MMU are precomputed, and that table is moved into a separate object.

Measuring the fixed-point version seven times, alternating before and after, the median went from 0.972 seconds to 0.878 seconds, about 1.11x from this change.
Separate workloads for normal RAM, the MMU and serial output were also compared.
For the latest investigation, see [Re-validating implementation patterns](docs/ja/pattern-validation.md) (Japanese); for the previous stage, see [Notes on Rust optimization](docs/ja/rust-optimization.md) (Japanese).

## License and references

The rs6809 code is under the MIT license. `reference/` is not part of the distribution; the fetched kit, ROM, OS and BASIC09 are subject to their own rights and licenses.
The contents, licenses and source locations of the CP/M-68K and FUZIX images distributed in Releases are in [release/NOTICE.md](release/NOTICE.md).

CPU specification: [Motorola MC6809/MC6809E Programming Manual](https://www.maddes.net/m6809pm/index.htm).
The Multicomp register specification and boot procedure were checked against the reference kit's `exec09/miscsbc.c`, `nitros9-mc09-build/defs/mc09.d` and `nitros9-runtime/`.
68000 specification: MC68000 User's Manual (instruction timing in chapter 8). Instruction tests: [SingleStepTests/680x0](https://github.com/SingleStepTests/680x0).
