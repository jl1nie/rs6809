English | [日本語](ja/fork-compare.md)

# fork, exec and wait versus OS-9's F$Fork and F$Wait

The same ICE recorded NitrOS-9 (6809, Level 1) and FUZIX (68000) running a command from the shell, shown side by side here.
Both records are the contents of `/api/os/calls`, taken the same way as `tests/os_snapshot.rs` for OS-9 (running `echo traced`) and `tests/fuzix68k.rs` for FUZIX (running `ps`).

## The calls in order

**NitrOS-9**: the call that creates the child also runs the new program.

```text
#239 P2 F$Link   X→"Shell" type A=$11                 → header U=$B800 entry Y=$B843
#240 P2 F$Fork   module "Shell" parameters "-P X -p\r" data 3 pages
                                                       → child process ID=3   inner: F$All64, F$AProc
#273 P2 F$Wait   waits for a child to exit             (sleeps until the child ends)
#336 P3 F$Fork   module "echo" parameters "* NitrOS-9 L1 mc09 / BASIC09 *\r" data 2 pages
                                                       → child process ID=4   inner: F$All64, F$SRqMem, F$AProc
```

**FUZIX**: it is split into three: create the child (`_fork`), the child replaces its own contents (`execve`), and the parent waits (`waitpid`).

```text
P9  _fork    flags $0                             → child pid 11
P9  waitpid  pid -1 status $0329B6 options 0      (sleeps until the child ends)
P11 execve   "/bin/ps" [ps]                       (does not return on success)
P11 open     "/dev/proc" flags $0 mode 0          → fd 3
P11 write    fd 1 26 bytes "  PID    TTY     TIME CM…"
P11 _exit    exit code 0                          (does not return)
P9  waitpid  …                                    → pid 11 status $0000 (exit code 0)
```

## Differences

| | NitrOS-9 (`F$Fork`, `F$Wait`) | FUZIX (`fork`, `execve`, `waitpid`) |
|---|---|---|
| Creating the child | Starts the module given by name as a new process (fork and exec in one call) | Duplicates the whole calling process. The child continues in the same program, and `_fork` returns 0 to it |
| Naming the program | A module name (linked if in memory, otherwise loaded from the execution directory) and a parameter string | The child passes a path name and an argument list to `execve`, which replaces its contents |
| The child's memory | The module's code is shared; only a new data area is allocated (`F$SRqMem`; in Level 2, block allocation) | Memory is flat (no MMU), so the parent's whole memory is copied elsewhere (`pagemap_alloc`). `execve` loads the new program |
| Returned to the parent | The child's process ID (register A) | The child's pid (D0); 0 to the child |
| Waiting | `F$Wait`: sleeps until any child ends and returns the child's ID and exit status (B) | `waitpid(-1, &status, 0)`: likewise sleeps until a child ends and returns the pid and status (exit code in the upper 8 bits) |
| Ending | `F$Exit`: passes the status to the parent and frees the process descriptor | `_exit`: becomes a zombie and is reaped by the parent's `waitpid` |
| Inner work | Visible as system calls inside the kernel (`F$All64` for the process descriptor, `F$AProc` to make it ready) | Inside the kernel these are C functions (`dofork`, `ptab_alloc`, `pagemap_alloc`, `doexit`, `psleep`, `wakeup`). In the ICE, follow them with breakpoints on the `fuzix.sym` symbols |
| Catching in the ICE | `SWI2` + number (the number is in the instruction) | `TRAP #12`, number in D0 (taken from the registers just before) |

## Following it in the ICE

```sh
# NitrOS-9: the moment the shell starts a command
curl -X POST 'localhost:6809/api/os/break?code=F$Fork'
curl -X POST 'localhost:6809/api/os/break?code=F$Wait&when=return'

# FUZIX: the moments of fork and exec, and the parent receiving the child's exit
curl -X POST 'localhost:6809/api/os/break?code=_fork'
curl -X POST 'localhost:6809/api/os/break?code=execve'
curl -X POST 'localhost:6809/api/os/break?code=waitpid&when=return'
# inside the kernel: where the child's process table entry is made (--symbols fuzix.sym)
curl -X POST 'localhost:6809/api/bp?sym=dofork'
```

The OS tab's timeline shows, as bands, FUZIX switching to the child (pid 11) after `_fork` and back to the shell (pid 9) after `_exit`.
Stopped at `execve`, the process table shows the child (running), still named `-sh`, next to the parent sleeping in `waitpid`.
