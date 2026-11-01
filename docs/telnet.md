English | [日本語](ja/telnet.md)

# Connecting to OS-9 Level 2 over Telnet

One 6809 CPU, OS-9 kernel and disk are shared, and each connection gets its own MC6850 ACIA and OS-9 login shell.
OS-9 manages authentication, user IDs, home directories and process scheduling.
A free ACIA is chosen automatically on connection. Listening is limited to localhost, and the OS picks a free TCP port.

## Preparing and starting

You need Rust 1.89 or later and the tools the README lists for building the Level 2 disk.

```sh
python3 scripts/build-level2-disk.py --multi-user --acia-count 3
sh examples/nitros9-telnet.sh
```

OS-9 is set up with three terminals, `/term`, `/t0` and `/t1`, each with its own `tsmon`.
User names and random initial passwords are saved in the generated `artifacts/level2-multiuser/accounts.txt`.
User names default to `user1`–`user3`, with user IDs 1–3. The output goes in a different directory from the single-user version.

On start-up the actual listening address is printed to standard error.

```text
rs6809: Telnet listening on 127.0.0.1:49152 (4 ACIA hardware ports; OS-9 controls active sessions)
```

Connect to the printed port from another terminal.

```sh
telnet 127.0.0.1 49152
```

Press Enter and OS-9 shows `User name?:` and `Password:`.
After logging in you can run, at the `OS9:` prompt, for example:

```text
mfree
procs
sleep 30000 &
echo hello >memo
list memo
```

OS-9 turns echo off for password input. Telnet option negotiation, CRLF/CR NUL and IAC escapes are handled.
In Telnet mode the host's standard input is not passed to the UART. The host's standard output shows only the boot log up to the start of listening;
after that, `/term` (ACIA 1) output goes only to the connected user.

## Terminals and the connection limit

The emulator provides 4 ACIAs at the existing Multicomp I/O addresses.
Only ACIAs for which OS-9's `tsmon` has signalled it is ready for a login are candidates for a connection.
The boot ROM or ordinary driver initialisation alone does not make an ACIA accept connections.
If the OS has opened 2 terminals, 2 connections can be open at once; with 4 terminals, 4.
A connection beyond the limit gets a busy message and is closed; existing sessions continue.

| OS-9 terminal | ACIA status/control | ACIA data |
|---|---|---|
| `/term` | `$FFD0` | `$FFD1` |
| `/t0` | `$FFD2` | `$FFD3` |
| `/t1` | `$FFD4` | `$FFD5` |
| `/t2` | `$FFD6` | `$FFD7` |

The disk build option `--acia-count 1|2|3|4` sets the terminal descriptors included in OS-9 and the number of monitor processes started.
No separate session limit is needed on the host.
When changing the OS-9 configuration on an existing disk, keep the terminal descriptors and the `tsmon` start-up settings in step.

An example giving your own accounts at build time:

```sh
python3 scripts/build-level2-disk.py --multi-user --acia-count 2 \
  --user alice:initial-password --user bob:another-password
```

The number of accounts and the number of simultaneous connections are independent. OS-9 also handles logging into several terminals with the same account.
The authentication data is in the guest's `/dd/SYS/password`, and homes are `/dd/username`.
Each home gets the matching OS-9 user ID, with read and write permissions for other users removed.

## Disconnecting and reconnecting

A TCP disconnect is passed on as loss of the MC6850's DCD. The emulator does not end guest processes directly.
When the guest's ACIA driver sees DCD loss while waiting to read, it sends `S$HUP` with OS-9's `F$Send`.
The existing Login/Shell signal handlers deal with it, and the parent `tsmon` prepares the next login.

Before handing the same ACIA to the next connection, it checks that `tsmon` has reopened the terminal and done two master resets in a row with `SS.Reset`, then reconfigured it.
This sequence is used as the signal that a login is ready, which keeps input from reaching a UART that is still starting up.
The check keeps the next connection from inheriting the previous user's shell or input.
The kernel also holds `/term`, so instead of relying only on the last file close, the explicit reset is checked.

If the guest is in a long operation, or a background process holds the terminal, when the cleanup happens is up to OS-9.
The host never forces an end by timeout or removes process trees.

## Keeping files

In Telnet mode SD writes are kept by default. Each completed 512-byte sector is written to the SD image
and counted as done after `sync_data`. After a file is closed and the command finishes, its contents survive restarting the emulator.
To prevent two writers on the same image, it is exclusively locked while running. A read-only file or a write error is a start-up or run-time error.

```text
artifacts/level2-multiuser/multicomp09-level2.img
```

This SD image is what is updated persistently. The source `nitros9-level2.dsk` is not updated automatically.
The ordinary local console mode can also keep writes with `--persist-disk`.
For experiments where changes should be discarded as before, give `--volatile-disk`.

If a multi-user disk already exists, the build script refuses to rebuild it.
Rebuilding with `--force` resets the saved files and accounts.
To try another configuration, use `--output other-directory`.
The SD image, the source disk and the account information are created on the host with mode 0600.

## Listening port

The default `--telnet` binds to `127.0.0.1:0` and gets a free port automatically. The number may change on restart.
To fix it, give for example `--telnet 127.0.0.1:6809`. Binding to non-loopback addresses is refused.

## Verification

```sh
cargo test
python3 scripts/test-telnet.py
python3 scripts/test-telnet.py --acia-count 4
```

Using the real ROM and OS-9, this checks simultaneous logins, rejection of wrong passwords, hidden passwords, separated input and output,
multiple processes, restricted access to other users' homes, the connection limit, re-authentication after disconnecting and files kept after restarting.
The tests build their own disk in a temporary directory and do not change the disks or accounts in use.

The reference kit needed further changes. `nitros9-telnet.patch` adjusts the MC6850 DCD and terminal reset,
Tsmon reopening the terminal, SysGo starting the login monitors, where Login reads the terminal options, and the T2 descriptor.
Also, the old Multicomp kernel did not register `F$CpyMem` for the 6809, so a compatible service was added to KrnP2,
which also lets `procs` get process names. The emulator itself runs on the Rust standard library alone.
