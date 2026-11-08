English | [日本語](ja/fuzix68k.md)

# FUZIX/68000 and the process monitor

The Tiny68K port of FUZIX 0.5 (a small Unix-like OS by Alan Cox and others) runs on the Tiny68K-compatible machine ([68000 CPU core](m68k.md)),
and the ICE watches its system calls, process table and process switches. A comparison with OS-9 is in [fork, exec and wait versus OS-9's F$Fork and F$Wait](fork-compare.md).

```sh
sh scripts/build-m68k-elf.sh          # first time only: m68k-elf binutils and gcc (tens of minutes)
sh scripts/build-fuzix-tiny68k.sh     # FUZIX kernel, commands and disk
sh examples/fuzix68k.sh               # a shell logged in as root (^C to quit; disk writes are not kept)
sh examples/fuzix68k.sh --ice         # ICE: http://127.0.0.1:6809/
sh examples/fuzix68k.sh --persist     # keep writes in artifacts/fuzix68k/hd.img (run telinit 6 before quitting)
sh examples/fuzix68k.sh --tty2        # a second terminal: log in with telnet 127.0.0.1 6812
```

```text
FUZIX version 0.5
...
Motorola 68000 processor detected.
16352KiB total RAM, 16254KiB available to processes (125 processes max)
Enabling interrupts ... ok.
IDE drive 0: hda: hda1 hda2
bootdev: hda1
...
login: root

Welcome to FUZIX.
# ps
  PID    TTY     TIME CMD
    9    tty1 00:00:22 -sh
   11    tty1 00:00:00 ps
```

## Toolchain

The FUZIX 68000 port expects `m68k-elf-gcc -m68000` and links user programs with a 68000 libgcc (helper functions for 32-bit multiply and divide).
FUZIX's `README.68000.md` warns against using m68k-linux-gnu's gcc and names gcc 10.3.0 (`--with-cpu=68000`) as the author's setup.
`scripts/build-m68k-elf.sh` fetches binutils 2.42 and gcc 10.3.0 (C only, `--with-cpu=68000`) from GNU's sites, checks their SHA-256 and
builds them into `artifacts/tools/m68k-elf`. No sudo is needed. Disassembly confirmed that the built libgcc has no 68020-or-later instructions.

`scripts/build-fuzix-tiny68k.sh` runs `make TARGET=tiny68k diskimage` in `reference/FUZIX` (commit `776bfc9`) and
puts `disk.img`, `fuzix.elf`, `fuzix.sym` (nm output) and `offsets.txt` in `artifacts/fuzix68k`. Two places needed changes:

- `as.c` in `Applications/as09`: gcc 10.3's `-Os` gives a `switch` table the wrong size (`value -684 out of range`; README.68000.md describes this as a gcc 11 problem),
  so it is built first with `-fno-jump-tables`.
- `Applications/MWC/cmd`: two grammars are generated into the same `y.tab.c`, so with a parallel make one's output becomes the other's `.c`. This directory alone is built first, not in parallel.

## 8 MiB (the Tang Nano 20K machine)

The kernel has the size of RAM in its source: `Kernel/platform/platform-tiny68k/main.c` gives the memory from the end of the kernel to `$FF8000` to the processes (`kmemaddblk`), and `p68000.S` sets `ramsize` to 16352 KiB.
`sh scripts/build-fuzix-tiny68k.sh --tn20k` builds a kernel for 8 MiB (`$800000` and 8192 KiB), puts the two files back afterwards, and writes the results to `artifacts/fuzix68k-tn20k`.
`./setup.sh` fetches the same build from the release (`fuzix68k-tn20k-disk.img.gz`, `fuzix-tn20k.sym`, `fuzix-tn20k-usersyms.tar.gz`) into `artifacts/fuzix68k-tn20k`, so no toolchain is needed.
It runs in the emulator with `--ram-kb 8192`, the machine of [rs6809-fpga](https://github.com/jl1nie/rs6809-fpga):

```sh
sh scripts/rs6809 --cpu 68000 --machine tiny68k --ram-kb 8192 --os fuzix \
  --disk artifacts/fuzix68k-tn20k/disk.img --boot-sector --script examples/fuzix68k/boot.script
```

```text
8192KiB total RAM, 8094KiB available to processes (125 processes max)
```

`kmemaddblk` is the only place the kernel takes memory for processes from, and the boot loader (at `$1F0000`) and the kernel (from `$400`) are inside 8 MiB, so nothing else changes. The disk is a different one from the 16 MiB build's (the kernel is on it).

## Booting

There is no monitor ROM. With `--boot-sector` the emulator places LBA 1 of the disk (512 bytes) at `$15000` and starts there.
As with the monitor's "CP/M boot loader", the boot loader (`loader.S`, built for `$15000`) moves itself to `$1F0000`, reads the kernel from LBA 2 onwards to `$400` and starts it.
The disk has its bytes swapped, as on the board, so the boot sector is also placed with each word swapped.

- Clock: the kernel sets the DUART timer (initial value `$480`, vector `$81`) and the interrupt mask, and the terminal setup writes the timer mode (`$70`/`$F0`) to ACR.
  The emulator's ACR stays at its reset value 0, and the 100 Hz clock runs.
- There is no IDE drive 1 (slave) (with the DEV bit set the status is 0). The kernel prints `IDE drive 1: ide error, status=0000`.
- `examples/fuzix68k/boot.script` answers the boot-time questions (`bootdev:`, date, time, `login:`).
  In the script, lines starting with `#` are comments, so a line waiting for the prompt `# ` is written `\#`.
  Lines starting with `?` answer whenever their string appears, in any order (`y` to fsck's `Continue? `, `Fix? `, `Zap? ` and `Rebuild free list? `).

## Disk writes, terminals and state

- The terminal type is `TERM=vt52` (set by `init`, and present in `/etc/termcap`). The ICE console interprets FUZIX output as VT52 by default, so full-screen programs such as `less`, `vile` and `fleamacs` work.
  FUZIX terminals have no default size (full-screen programs think there are more than 200 lines), so the build script writes `stty rows 24 cols 80` into `/root/.profile`.
  Inside the ICE, the ICE writes the size of the browser's console into the first terminal's `winsize` (and corrects it again after `.profile`).

- By default each run boots from the freshly built `disk.img`, and writes stay in memory (however it is stopped, the next run starts from the same state).
- `--persist` keeps writes in `artifacts/fuzix68k/hd.img` (copied from `disk.img` the first time). Type `telinit 6` before quitting:
  runlevel 6's `/etc/rc.halt` runs `umount -a` and switches the root to read-only before halting (`halt` alone leaves the file system dirty).
  If it was stopped dirty, fsck checks and repairs it on the next boot (the boot script answers its questions).
- `--tty2 [127.0.0.1:6812]` listens for TCP on DUART channel B (`/dev/tty2`). `telnet 127.0.0.1 6812` gives a second login.
  The build script changes `getty /dev/tty2` in `/etc/inittab` from its original `off` to `respawn` (editing the file system with FUZIX's `ucp`).
  The last 2 KiB of output before a connection are kept and sent when one connects.
- The ICE's "Save" writes the current state (after logging in, for example, including the disk in memory) to a file, and `--load-state` or "Load" resumes from it.
  "↺ Reset" goes back to the state at start-up (from the boot sector).

## Program symbols

The build links every program through `scripts/fuzix-ld68k-syms`, FUZIX's `ld68k` that also keeps the ELF's symbols as `artifacts/fuzix68k/usersyms/NAME.sym` (the programs themselves come out the same, byte for byte).
With `--ice` or `--dap`, `examples/fuzix68k.sh` loads them (`--symbols artifacts/fuzix68k/usersyms`): each file is placed on the processes running that program,
at the process's `u_codebase` (the symbols are offsets in the image). After `execve` they are placed when the new program first runs in user state,
so a breakpoint on `listfiles` (in `ls`) or `main` waits for the program and stops in it, and the stack shows `main+$182` and so on.

## FUZIX monitor (`src/ice/fuzix.rs`)

The OS layer used with `--os fuzix`. It takes `udata_shadow`, `ptab`, `ptab_end` and `e_trap12` from the kernel's symbols (`--symbols artifacts/fuzix68k/fuzix.sym`)
(`OsMonitor::use_symbols`), and becomes active when the TRAP #12 vector points at `e_trap12` and a process is running.

| Item | Contents |
|---|---|
| Catching | The `TRAP #12` in the entry stub copied to the start of each program. The number is in D0.W, the arguments in D1, A0, A1, A2 (the order in which `e_trap12` copies them to `u_argn`) |
| Return | D0 is the result. On error D0 = -1 and D1.W is the error number (`ENOENT` and so on) |
| Names | The 80 in `Kernel/include/syscall_name.h` (`_fork`, `execve`, `waitpid`, `_exit`, `open`, `read`, `write` and others), with summaries and explanations for the main ones |
| Arguments and results | Path names, `execve`'s argument list, file descriptors with the path they were opened with (`fd 9 (/dev/tty1)`), what `write` writes, what `read` read, `_fork`'s child pid, `waitpid`'s pid and exit code |
| Running process | `udata_shadow` (the running process's `u_data`), and `p_pid` through its `u_ptab` |
| Process table | Used entries from `ptab` up to the end `ptab_end` points to: pid, parent, name, state (running, ready, sleeping, I/O wait, stopped, forking, zombie), uid, terminal, what it waits on, priority, nice, size, time |
| Queues | Ready (`P_READY`), sleeping (`P_SLEEP`, `P_IOWAIT`), zombies |
| Memory | The kernel (`$400`–`_end`) and each process's `u_codebase`–`u_top`, and whether that process is now inside a system call |

The structure offsets (`struct p_tab` is 94 bytes, `p_pid` at 4, `p_pptr` at 8, `p_name` at 48 and so on) depend on the 68000's 2-byte alignment,
so they are found by having m68k-elf-gcc read the real kernel headers with `scripts/fuzix-offsets.c`, and a test compares `offsets.txt` with the monitor's constants.

## Verification

```sh
cargo test --release --test fuzix68k -- --ignored
```

- Resuming from a saved state in another ICE, a command runs in the shell at once. Reset starts again from `bootdev:`.
- `login:` appears on the second terminal (channel B), and after logging in a second `-sh` appears in the process table.
- The output of `ls /`, `ps` and `echo` after booting. The shell's `_fork` (child pid), the child's `execve "/bin/ps" [ps]`, `open "/dev/proc"` and `_exit`, and the parent's `waitpid` receiving the child's pid;
  the path learnt from `open` is attached to later `write`s; the timeline switches to the child and back to the shell.
- Stops before the shell child's `execve` (D0 = 23; in the process table the child is running and the parent shell sleeping), after `waitpid` returns (D0 is the child's pid),
  and at the error return of `open` for `cat /nosuch` (-1 `ENOENT`).
- DAP: stops with the exception breakpoint conditions `_fork` and `waitpid`, and the thread name is `68000 (FUZIX process 9 -sh)`.
- The monitor's structure offsets match those found from the kernel headers.

## Limitations

- The structure offsets are for this FUZIX version (`776bfc9`) and the Tiny68K configuration. After changing versions, check them with `offsets.txt` and the tests.
- Process IDs are handled as 8 bits (256 and above use the low 8 bits).
- Program symbols come from the link (`usersyms/NAME.sym`), placed on every process whose name is NAME (`-sh`, `/bin/ls`). A program renamed on the disk, or run under another name, gets none.
- A system call interrupted by a signal is marked "no return" when the same process makes its next system call.
