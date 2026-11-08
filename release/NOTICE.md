# rs6809 release images: contents, licences and sources

These files are what `setup.sh` downloads from the GitHub release. Each part
keeps its own licence.

| File | What | Licence and source |
|---|---|---|
| `rs6809-linux-x86_64`, `rs6809-macos-arm64` | The emulator for Linux x86-64 (statically linked with musl, any distribution) and macOS on Apple Silicon, built and uploaded by the `release-binaries` GitHub workflow (`.github/workflows/release-binaries.yml`) | MIT (this repository) |
| `CPM15000.SR`, `CPM15000.MAP` | CP/M-68K 1.3 without a BIOS, from `68kv1_3.zip` (DISK8) | Digital Research CP/M-68K, offered under the CP/M licence of 2022 (DRDOS, Inc.): http://www.cpm.z80.de/license.html — distribution: http://www.cpm.z80.de/download/68kv1_3.zip |
| `cpm68k-a.img.gz` | CP/M-68K disk A: made with `scripts/cpm68k-disk.py dist --no-alcyon` (the distribution's CP/M utilities, and `examples/cpm68k/hello.c`) | As above. The Alcyon C compiler suite is **not** included: whether the 2022 licence covers it is not stated, so `setup.sh` adds it from the original distribution on your machine |
| `fuzix68k-disk.img.gz`, `fuzix.sym`, `fuzix-usersyms.tar.gz` | FUZIX 0.5 for Tiny68K: the IDE disk (boot loader, kernel, file system), the kernel's symbols, and each program's symbols (`usersyms/NAME.sym`, kept at link time by `scripts/fuzix-ld68k-syms`), built by `scripts/build-fuzix-tiny68k.sh` | FUZIX: GNU GPL v2 (kernel) and the licences of its applications. Source: https://github.com/EtchedPixels/FUZIX at commit `776bfc9`, built with m68k-elf gcc 10.3.0 (`scripts/build-m68k-elf.sh`); the only changes are build options (`as09` without jump tables, `MWC/cmd` built serially) `/etc/inittab` enabling `getty /dev/tty2` and `stty rows 24 cols 80` in root's `.profile`, all in `scripts/build-fuzix-tiny68k.sh` |
| `fuzix68k-tn20k-disk.img.gz`, `fuzix-tn20k.sym`, `fuzix-tn20k-usersyms.tar.gz` | The same FUZIX built for 8 MiB of RAM (`scripts/build-fuzix-tiny68k.sh --tn20k`), for the Tang Nano 20K machine of rs6809-fpga and `--ram-kb 8192` | As above; the additional change is the RAM size (`kmemaddblk` up to `$800000`, `ramsize` 8192 in `Kernel/platform/platform-tiny68k`). Built with m68k-elf gcc 10.3.0 and GNU binutils 2.42 (m68k) |

Not in the release: NitrOS-9 (the Multicomp boot ROM and SD image). It carries
no explicit licence, so `setup.sh` fetches it from
https://github.com/kyo-ta04/nitros9-basic09-starter-kit at commit `bf60370`.

`release/images.sha256` in the repository lists the SHA-256 of every file;
`setup.sh` checks downloads against it.
