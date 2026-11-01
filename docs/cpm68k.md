English | [日本語](ja/cpm68k.md)

# CP/M-68K and the BDOS monitor

CP/M-68K 1.3 runs on the Tiny68K-compatible machine ([68000 CPU core](m68k.md)) with our own BIOS, and the ICE watches its BDOS and BIOS calls.

```sh
sh examples/cpm68k.sh            # the console is this terminal (^C to quit)
sh examples/cpm68k.sh --ice      # ICE: http://127.0.0.1:6809/ (add --dap for VS Code and others)
```

The first run fetches the distribution with `scripts/fetch-cpm68k.py` and builds the disk `artifacts/cpm68k/a.img` with `scripts/cpm68k-disk.py`.
Writes to the disk are kept (delete the file to rebuild it).

```text
A>DIR *.SUB
A: RELOC1   SUB : RELOC2   SUB : CLINK    SUB : CLINKE   SUB : CLINKF   SUB
...
A>AS68 -I AS68INIT          ← first time only: builds the assembler's symbol table
A>SUBMIT C HELLO            ← CP68 → C068 → C168 → AS68
A>SUBMIT CLINK HELLO        ← links with LO68 into HELLO.68K
A>HELLO
Hello from Alcyon C on CP/M-68K
1+2+...+10 = 55
```

## Distribution and layout

| Item | Source | In the repository |
|---|---|---|
| CP/M-68K 1.3 (`68kv1_3.zip`, checked by SHA-256) | [The Unofficial CP/M Web Site](http://www.cpm.z80.de/), [license](http://www.cpm.z80.de/license.html) | no (fetch script) |
| The system `CPM15000.SR` (no BIOS, `$15000`–`$1ACC1`) | `DISK8` of the distribution | no |
| BIOS `examples/cpm68k/bios.s` and the assembled `bios.sr`, `bios.lst`, `bios.map` | our own | yes |
| Disk image | built by `scripts/cpm68k-disk.py` | no |

It is not stated whether Alcyon C is covered by the CP/M license, so it is handled by the fetch script like the system itself.

Memory layout:

| Address | Contents |
|---|---|
| `$000000`–`$0003FF` | Exception vectors (TRAP #2 is the BDOS, TRAP #3 the BIOS) |
| `$015000`–`$017321` | CCP (entry at `$15000`; `_ccp` = `$150BC` is the warm boot target) |
| `$017322`–`$01AFFF` | BDOS (from the TRAP #2 entry `_traphnd`) |
| `$01B000`– | BIOS (`_init`) |
| `$020000`–`$FDFFFF` | TPA (programs are relocated and loaded here) |

There is no monitor ROM: the emulator loads the two S-records and starts at `$15000` in supervisor mode.
The system sets up its stack, calls `JSR $1B000` (the BIOS `_init`), initialises the BDOS and shows the CCP prompt.

## BIOS

`examples/cpm68k/bios.s` (GNU as, Motorola syntax). Assemble it with `python3 scripts/build-cpm68k-bios.py`
(needs Ubuntu's `binutils-m68k-linux-gnu`; the assembled output is in the repository, so this is not needed just to run it).

- `_init`: sets the TRAP #3 vector, initialises DUART channel A and returns D0 = 0 (drive A:, user 0).
- TRAP #3: dispatches on the function number in D0.W (0–22). D1–D7 and A0–A6 are preserved.
- The console is DUART channel A; LIST, PUNCH and READER are channel B.
  `CONIN` does not spin: it enables the receive interrupt (level 2, vector 64) and waits in `STOP`. The interrupt handler only sets IMR back to 0
  (the request stays up until a byte is read).
- Disk A: is the IDE. The BDOS reads and writes 128-byte sectors, so the BIOS keeps one 512-byte IDE sector;
  reads copy from it, and writes change it and write it to the IDE at once (write-through). IDE words are read and written with their bytes swapped.
- `GETMRT` returns a table with the TPA at `$20000`–`$FE0000`. `SETEXC` changes any vector except TRAP #3.

## Disk format

8 MiB (16384 × 512 bytes). One track = 32 × 128 bytes, 2048 tracks, 1 reserved track, 4 KiB blocks (2047 of them), 512 directory entries (blocks 0–3);
the directory allocation map uses 16-bit block numbers. They are little-endian, the same order as 8080 CP/M (even though the BDOS runs on a 68000).

```sh
python3 scripts/cpm68k-disk.py dist a.img [FILE[=NAME]]...   # the distribution's tools plus files
python3 scripts/cpm68k-disk.py create a.img FILE...          # an empty disk plus files
python3 scripts/cpm68k-disk.py add a.img FILE...
python3 scripts/cpm68k-disk.py ls a.img
python3 scripts/cpm68k-disk.py get a.img X.SUB -
```

The distribution's commands are `.REL` (with relocation information), but the BDOS program load (function 59) relocates them, so they are written under `.68K` names and RELOC is not used.
Text files (`.C`, `.H`, `.SUB` and so on) get CR LF line ends and are padded with ^Z.

## BDOS monitor (`src/ice/cpm68k.rs`)

The CP/M-68K version of the ICE's OS layer (`OsMonitor`). It recognises the system by the `CP/M-68K` string at its start and by TRAP #2 pointing into the BDOS.

| Item | Contents |
|---|---|
| Catching | `TRAP #2` (BDOS, function number in D0.W, argument D1) and `TRAP #3` (BIOS, D0.W, D1, D2). The function number is taken from the registers just before the trap (`svc_regs`) |
| Names | Common CP/M names for the BDOS such as `F_OPEN`, `C_WRITESTR`, `P_LOAD`, and `BIOS_CONOUT` and so on for the BIOS, with summaries and explanations |
| Inner calls | BIOS calls, and BDOS calls from inside the BDOS (`F_READ` inside `P_LOAD`, for example), are counted as inner calls under the original BDOS call |
| Arguments and results | Characters, `$`-terminated strings, FCB file names (`A:HELLO.C`), random record numbers, DMA addresses, the load parameter block (TPA, base page, text, data, BSS) |
| Errors | `$FF` from directory operations; failures of sequential writes, random reads and writes and program loads; failed BIOS reads and writes. `$FF` from `F_SFIRST`/`F_SNEXT` (end of entries) is not an error |
| State | The BDOS's internal layout is not published, so the current drive, user number, DMA address, open FCBs, TPA and loaded programs are learnt from call arguments and results |
| Programs | The CCP is shown as 1 and programs loaded by `P_LOAD` as 2 onwards; `P_TERMCPM` and warm boot return to the CCP |
| Memory | Regions for the exception vectors, CCP, BDOS, BIOS, TPA, programs and base pages |

The OS tab shows system information (running program, drive and user, DMA, TPA, open files), the program table, programs and system in memory,
memory regions, and the log of BDOS and BIOS calls (select a call to see the input and output of registers D0–D2, A0, A1, A7 and SR, and the tree of inner calls).

```sh
# stop by name (HTTP API)
curl -X POST 'localhost:6809/api/os/break?code=F_OPEN'                 # before the call
curl -X POST 'localhost:6809/api/os/break?code=P_LOAD&when=return'     # after it returns
curl -X POST 'localhost:6809/api/os/break?code=BIOS_READ'              # BIOS too, by name or as "BIOS 13"
curl -X POST 'localhost:6809/api/os/break?errors=1'                    # error returns
```

In DAP, put names, comma-separated, in the conditions of the "system call" and "system call return" exception breakpoints ([ICE](ice.md)).

## Symbols and source

`examples/cpm68k.sh` loads the BIOS listing (`bios.lst` + `bios.map`) and the system's address map (`CPM15000.MAP`).
The disassembly and the stack show CCP and BDOS function names (`_traphnd`, `_pgmld` and so on, NM68's 8 characters) and BIOS labels,
and the BIOS can be broken and stepped by source line (in VS Code you can put line breakpoints in `bios.s`).

## Verification

```sh
python3 scripts/fetch-cpm68k.py
cargo test --release --test cpm68k -- --ignored
cargo test --release --lib tiny68k            # DUART and IDE
```

- Boots and checks the output of `DIR`, `TYPE`, `STAT` and `PIP X.SUB=C.SUB`, and behind them the `F_OPEN`, `F_MAKE` (temporary name `X.$$$`), `F_RENAME`, `P_LOAD` and `F_READ` calls,
  the log of inner BIOS calls, and the learnt drive and TPA.
- Stops before `F_OPEN` (D0 = 15, D1 the CCP's FCB `STAT    68K`), after `P_LOAD` returns (base page `$020000` in the program table, image `STAT`),
  and at the error return of `F_OPEN` for `TYPE NOSUCH.TXT`.
- Compiles, links and runs `examples/cpm68k/hello.c` with Alcyon C.
- DAP: stops at an `F_SFIRST` call and checks that the thread name shows `CP/M-68K`; stops at a breakpoint on the `conout:` line of `bios.s`
  and checks the frame name `conout`, the line number and line stepping.

## Limitations

- CP/M-68K runs only one program at a time, so there is no scheduler or queue as in OS-9. The timeline only shows switches between the CCP and programs.
- The BDOS's internal state is learnt from calls, so when the ICE is enabled part way through, for example, it shows "—" until the next relevant call.
- Only drive A: exists. Interrupts are used only while waiting in `CONIN`.
- Source-level debugging of Alcyon C is out of scope.
