English | [日本語](ja/level2.md)

# Multicomp NitrOS-9 Level 2

## Reproducing

The reference kit is used at commit `bf60370cfa180636cde3ee305d089d94872de82a`, as given in the README.
The build script checks this revision and applies patches to external sources only to copies in `artifacts/level2/sources/`.
Kernel and ROM binaries are not added to the repository.

```sh
python3 scripts/build-level2-disk.py
sh examples/nitros9-level2.sh
cargo test --release --test nitros9_level2 -- --ignored
```

The verified path is CamelForth → Level 2 kernel → SysGo → startup Shell → echo → interactive Shell → mfree → echo → interactive Shell.
`mfree` showed the physical memory top `$7FFFF` and 440 KB free (the default 512 KB configuration).
With `--ram-kb 1024` the same disk shows `$FFFFF` and 952 KB.
These are results with the minimal disk; the free amount depends on the loaded modules and running processes.

```text
NitrOS-9/6809 Level 2 V03.3.0
...
LEVEL2-STARTED
...
OS9:mfree

 Blk Begin   End   Blks  Size
 --- ------ ------ ---- ------
   9  12000  7FFFF   37   440k
                   ==== ======
            Total:   37   440k

OS9:echo LEVEL2-DONE
LEVEL2-DONE

OS9:
```

## The emulator's MMU

CPU addresses stay 16 bits. The physical RAM of `Machine::multicomp()` is extended to 512 KB,
and 64 banks of 8 KB can be selected. `multicomp_with_ram()` / `--ram-kb` select
64, 128, 256, 512 or 1024 KB; the standard flat machine stays at 64 KB.

- `$FFDE`: bit 7 = ROMDIS, bit 6 = running task, bit 5 = MMUEN, bits 3–0 = the map number to change. Control writes with bit 4 set are ignored.
- `$FFDF`: bit 7 = write protect, bits 6–0 = physical bank number. Each task holds 8 maps.
- With the MMU disabled, the low 64 KB is addressed directly. Banks that are not fitted read `$FF` and ignore writes.
- From the state with both MMUEN and ROMDIS set to 1, a write that keeps MMUEN and sets ROMDIS to 0 enables the fixed top area.
  The ROM stays disabled. Disabling the MMU releases the fixed top area latch.
- The fixed top area `$FE00–$FFFF` is the last 512 bytes of physical bank 7.
  It can be read and written regardless of the task, slot 7's protection or a missing bank, and I/O takes priority.

References: the kit's `exec09/miscsbc.c`, `defs/mc09.d`, `level1/coco1/modules/rel.asm`.
The implementation recomputes the mapping table only when the registers change.

## Changes needed for the Level 2 boot image

Extending the RAM alone did not make the reference kit's Level 2 sources boot as they are.
`scripts/nitros9-level2.patch` collects the following.

1. Restores REL's padding to 304 bytes, making BOOT 464 bytes and KRN's load address `$F000`.
2. Matches symbol names changed by updates to the reference sources (`InstallSvc` / `FSFreeLB`) and branch distances.
3. Replaces the CoCo DAT accesses left in `F$LDDXY` / `LDAXY`, task setup and `F$Move`
   with Multicomp `$FFDE–$FFDF` accesses. Two-byte reads can cross an 8 KB boundary.
4. `F$Move` does not use the stack while the source and destination are mapped.
   The kernel's register frame transfers use this routine too.
   When a process descriptor or the system stack was allocated in `$A000–$DFFF`, the old transfer
   unmapped its own source, and starting child processes and returning from system calls failed.
5. The SD driver's LSN0 test is changed from an addition to an OR, so a non-zero LSN that overflows is not mistaken for LSN0.
   This is the same fix as on the existing C development disk.
6. The mc09 kernel fixed the end of the block map at `ldd #$0240` (64 blocks, 512 KB).
   The build script replaces this with detection at boot. It maps block `$40`, just past 512 KB, into task 0's map 5 (`$A000`),
   writes a marker at `$A200`, and only if it reads back and does not appear at the same place in block 0 (`$0200`, the block map)
   uses `#$0280` (128 blocks, 1 MB). Missing banks read `$FF`, and with too few address lines the same memory
   appears twice so the marker shows in block 0; both cases give 512 KB. Finally map 5 is set back to 1:1.
   MMU block numbers are 7 bits (bit 7 of `$FFDF` is write protect), so 1 MB is the hardware limit.
   The block map is at `$0200–$02FF` and holds 128 blocks; the kernel block `KrnBlk=$07` does not depend on the size.
   In the 1 MB configuration, `cc1` and C programs were run with a `#56k` process holding blocks above the 512 KB boundary.

At assembly time, the sources are processed in a generated directory holding the Level 2 `defsfile`.
Using Level 1's source-relative includes as they are would mix in Level 1's system variable offsets.
The Level 2 RBF is used. The minimal disk's commands are Shell 2.1, echo and mfree.

## What is verified

`tests/memory_map.rs` checks every physical bank at every RAM size, missing banks, write protection,
two independent tasks and shared RAM, the fixed vector area and the ROM switching conditions.
The existing Level 1 → BASIC09 → `42` boot test also runs in the 512 KB configuration.

This is NitrOS-9 Level 2 for the Multicomp. The CoCo 3's GIME, screen, keyboard and disk formats are out of scope.
The Level 2 disk holds the Microware C `cc1` set and BASIC09.
In the default local console mode, disk changes are discarded at exit; `--persist-disk` or Telnet mode keeps them.
For the multi-user setup see [Telnet connections](telnet.md).
