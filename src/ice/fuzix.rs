//! FUZIX (68000, flat memory) for the OS layer: system calls (`TRAP #12`
//! in each program's stub, call number in D0.W, arguments in D1, A0, A1,
//! A2; result in D0, or -1 with the error number in D1.W), the process
//! table and the running process.
//!
//! The kernel's symbols (`--symbols fuzix.sym`, nm output of `fuzix.elf`)
//! give `udata_shadow` (the running process's u_data), `ptab` and
//! `ptab_end`. Structure offsets are those of the kernel built from
//! FUZIX 776bfc9 for Tiny68K (`scripts/fuzix-offsets.c` prints them).
use super::json::{Json, obj};
use super::osmon::{Call, CpuRegs, Image, Mem, OsMonitor, PathNames, Process, Regs68k, Service, Svc};

/// `struct p_tab` and `struct u_data` offsets (m68k-elf-gcc, 2-byte alignment).
pub mod off {
    pub const PTAB_SIZE: u32 = 94;
    pub const P_STATUS: u32 = 0;
    pub const P_TTY: u32 = 2;
    pub const P_PID: u32 = 4;
    pub const P_UID: u32 = 6;
    pub const P_PPTR: u32 = 8;
    pub const P_EXITVAL: u32 = 16;
    pub const P_WAIT: u32 = 18;
    pub const P_UDATA: u32 = 26;
    pub const P_PRIORITY: u32 = 30;
    pub const P_NAME: u32 = 48;
    pub const P_TIME: u32 = 56;
    pub const P_PGRP: u32 = 76;
    pub const P_NICE: u32 = 78;
    pub const P_SIZE: u32 = 84;
    pub const U_PTAB: u32 = 0;
    pub const U_INSYS: u32 = 8;
    pub const U_CALLNO: u32 = 9;
    pub const U_TOP: u32 = 46;
    pub const U_CODEBASE: u32 = 54;
    pub const U_NAME: u32 = 212;
    pub const TTY_SIZE: u32 = 32;
    pub const TTY_WINSIZE: u32 = 24;
}
use off::*;

/// Kernel/include/syscall_name.h.
const NAMES: [&str; 80] = [
    "_exit", "open", "close", "rename", "mknod", "link", "unlink", "read", "write", "_lseek", "chdir", "sync",
    "access", "chmod", "chown", "_stat", "_fstat", "dup", "getpid", "getppid", "getuid", "umask", "_statfs",
    "execve", "_getdirent", "setuid", "setgid", "_time", "_stime", "ioctl", "brk", "sbrk", "_fork", "mount",
    "_umount", "signal", "dup2", "_pause", "_alarm", "kill", "pipe", "getgid", "times", "utime", "geteuid",
    "getegid", "chroot", "fcntl", "fchdir", "fchmod", "fchown", "mkdir", "rmdir", "setpgrp", "_uname", "waitpid",
    "_profil", "uadmin", "nice", "_sigdisp", "flock", "getpgrp", "yield", "acct", "memalloc", "memfree",
    "__netcall", "_ftruncate", "_nosys68", "_nosys69", "_nosys70", "_nosys71", "_select", "setgroups",
    "getgroups", "getrlimit", "setrlimit", "setpgid", "setsid", "getsid",
];

/// (call, summary, explanation) for the calls worth explaining.
const EXPLAIN: &[(&str, &str, &str)] = &[
    ("_exit", "プロセスの終了", "終了コードを親に残してプロセスを終える。ゾンビになり、親のwaitpidで回収される。戻らない。"),
    ("open", "ファイルを開く", "パス名を開き、空いている最小のファイル記述子を返す。"),
    ("close", "ファイル記述子を閉じる", "ファイル記述子を解放する。"),
    ("read", "読み込み", "ファイル記述子からバッファへ最大nバイト読む。端末では1行そろうまで眠ることがある。"),
    ("write", "書き込み", "バッファのnバイトをファイル記述子へ書く。"),
    ("execve", "プログラムの実行", "今のプロセスの中身を、パス名のプログラムに置き換える。成功すると呼び出し元へは戻らず、新しいプログラムの先頭から動く。"),
    ("_fork", "プロセスの複製", "今のプロセスを複製する。親には子のプロセスIDが、子には0が返る。平坦なメモリのFUZIXでは、子のメモリを丸ごと写す。"),
    ("waitpid", "子の終了待ち", "子プロセスの終了を待ち、そのIDと終了状態を返す。子がまだ動いていれば眠る。"),
    ("kill", "シグナル送信", "プロセスにシグナルを送る。"),
    ("signal", "シグナルの処理の設定", "シグナルを受けたときの動作（無視・既定・関数）を設定する。"),
    ("pipe", "パイプの作成", "読み書き2つのファイル記述子をつないだパイプを作る。"),
    ("dup", "記述子の複製", "ファイル記述子を複製する。"),
    ("dup2", "記述子の複製（番号指定）", "ファイル記述子を指定の番号に複製する。"),
    ("brk", "データ領域の終わりの設定", "プログラムのデータ領域（ヒープ）の終わりを設定する。"),
    ("sbrk", "データ領域の伸縮", "データ領域をnバイト伸ばし、元の終わりを返す。"),
    ("chdir", "作業ディレクトリの変更", "カレントディレクトリを変える。"),
    ("_stat", "ファイルの情報", "パス名のファイルのi-node情報を読む。"),
    ("_fstat", "ファイルの情報（記述子）", "ファイル記述子のi-node情報を読む。"),
    ("ioctl", "装置の制御", "端末の設定などを読み書きする。"),
    ("getpid", "プロセスID", "自分のプロセスIDを返す。"),
    ("_pause", "シグナル待ち", "シグナルが来るまで眠る。"),
    ("_getdirent", "ディレクトリの読み込み", "ディレクトリの項目を読む。"),
    ("_select", "入出力待ち", "複数のファイル記述子のどれかが読み書きできるまで待つ。"),
    ("setsid", "セッションの作成", "新しいセッションとプロセスグループを作る。"),
    ("_time", "時刻", "現在時刻を読む。"),
];

const ERRNO: [&str; 60] = [
    "", "EPERM", "ENOENT", "ESRCH", "EINTR", "EIO", "ENXIO", "E2BIG", "ENOEXEC", "EBADF", "ECHILD", "EAGAIN",
    "ENOMEM", "EACCES", "EFAULT", "ENOTBLK", "EBUSY", "EEXIST", "EXDEV", "ENODEV", "ENOTDIR", "EISDIR", "EINVAL",
    "ENFILE", "EMFILE", "ENOTTY", "ETXTBSY", "EFBIG", "ENOSPC", "ESPIPE", "EROFS", "EMLINK", "EPIPE", "EDOM",
    "ERANGE", "ENOLOCK", "ENOTEMPTY", "ENAMETOOLONG", "EAFNOSUPPORT", "EALREADY", "EADDRINUSE", "EADDRNOTAVAIL",
    "ENOSYS", "EPFNOSUPPORT", "EOPNOTSUPP", "ECONNRESET", "ENETDOWN", "EMSGSIZE", "ETIMEDOUT", "ECONNREFUSED",
    "EHOSTUNREACH", "EHOSTDOWN", "ENETUNREACH", "ENOTCONN", "EINPROGRESS", "ESHUTDOWN", "EISCONN",
    "EDESTADDRREQ", "ENOBUFS", "EPROTONOSUPPORT",
];

const STATUS: [&str; 9] = ["空き", "実行中", "実行待ち", "睡眠", "I/O待ち", "停止", "fork中", "ゾンビ", "NOSLEEP"];
const QUEUE: [&str; 9] = ["-", "running", "ready", "sleep", "sleep", "stopped", "ready", "zombie", "running"];

/// Placeholder before D0 is known (disassembly).
const ANY: u8 = 0xff;

#[derive(Default, Clone, Copy)]
struct Syms {
    udata_shadow: u32,
    ptab: u32,
    /// Address of the `ptab_end` pointer.
    ptab_end: u32,
    kernel_end: u32,
    trap12: u32,
    /// `struct tty ttydata[]` (index = minor; the console is tty1).
    ttydata: u32,
}

#[derive(Default)]
pub struct Fuzix {
    syms: Option<Syms>,
}

fn regs(r: &CpuRegs) -> Regs68k {
    match r {
        CpuRegs::M68k(r) => *r,
        CpuRegs::M6809(_) => Regs68k::default(),
    }
}

fn cstr(m: &dyn Mem, at: u32, max: u32) -> String {
    let mut s = String::new();
    for i in 0..max {
        let c = m.peek(at.wrapping_add(i));
        match c {
            0 => return s,
            0x20..=0x7e => s.push(char::from(c)),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            b'\t' => s.push_str("\\t"),
            _ => s.push_str(&format!("\\x{c:02X}")),
        }
    }
    s.push('…');
    s
}

fn bytes_preview(m: &dyn Mem, at: u32, n: u32) -> String {
    let mut s = String::new();
    for i in 0..n.min(24) {
        let c = m.peek(at.wrapping_add(i));
        match c {
            0x20..=0x7e => s.push(char::from(c)),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            _ => s.push_str(&format!("\\x{c:02X}")),
        }
    }
    if n > 24 {
        s.push('…');
    }
    s
}

fn name8(m: &dyn Mem, at: u32) -> String {
    (0..8)
        .map(|i| m.peek(at + i))
        .take_while(|&c| c != 0)
        .map(|c| if (0x20..0x7f).contains(&c) { char::from(c) } else { '?' })
        .collect()
}

impl Fuzix {
    fn s(&self) -> Option<Syms> {
        self.syms
    }
    /// Live process table entries.
    fn entries(&self, m: &dyn Mem) -> Vec<u32> {
        let Some(s) = self.s() else { return Vec::new() };
        // `ptab_end` is a pointer to the end of the table.
        let end = m.peek32(s.ptab_end);
        let end = if end > s.ptab { end.min(s.ptab + 256 * PTAB_SIZE) } else { s.ptab + 125 * PTAB_SIZE };
        (0..)
            .map(|i| s.ptab + i * PTAB_SIZE)
            .take_while(|&p| p + PTAB_SIZE <= end)
            .filter(|&p| m.peek(p + P_STATUS) != 0)
            .collect()
    }
    fn fd(&self, paths: &PathNames, pid: u8, fd: u32) -> String {
        match paths.get(&(pid, fd as u8)) {
            Some(p) => format!("{fd}（{p}）"),
            None => format!("{fd}"),
        }
    }
}

impl OsMonitor for Fuzix {
    fn use_symbols(&mut self, find: &dyn Fn(&str) -> Option<u32>) {
        if let (Some(u), Some(p), Some(e)) = (find("udata_shadow"), find("ptab"), find("ptab_end")) {
            self.syms = Some(Syms {
                udata_shadow: u,
                ptab: p,
                ptab_end: e,
                kernel_end: find("_end").unwrap_or(0),
                trap12: find("e_trap12").unwrap_or(0),
                ttydata: find("ttydata").unwrap_or(0),
            });
        }
    }
    fn detect(&mut self, m: &dyn Mem) -> bool {
        let Some(s) = self.s() else { return false };
        // The syscall trap is installed and a process runs.
        m.peek32(0xb0) == s.trap12 && m.peek32(s.udata_shadow) != 0
    }
    fn info(&self) -> Json {
        let q = |key: &str, label: &str| obj([("key", key.into()), ("label", label.into())]);
        obj([
            ("os", "fuzix".into()),
            ("name", "FUZIX".into()),
            ("title", "FUZIX 0.5（68000）".into()),
            ("current", "実行中 udata_shadow".into()),
            (
                "queues",
                Json::Arr(vec![
                    q("ready", "実行待ち（P_READY）"),
                    q("sleep", "睡眠（P_SLEEP・P_IOWAIT）"),
                    q("zombie", "ゾンビ（親のwaitpid待ち）"),
                ]),
            ),
            ("image", "プロセスのメモリ".into()),
            ("memmap", "regions".into()),
            ("errorLabel", "FUZIXのエラー復帰（-1とerrno）".into()),
            ("switchLabel", "FUZIXのプロセス切り替え".into()),
            ("imagesTitle", "カーネルとプロセスのメモリ".into()),
            ("internalLabel", "カーネル内部の呼び出し".into()),
            ("breakExample", "_fork".into()),
        ])
    }
    fn summary(&self, m: &dyn Mem) -> Vec<(&'static str, Json)> {
        let mut ready = Vec::new();
        let mut sleep = Vec::new();
        let mut zombie = Vec::new();
        for p in self.entries(m) {
            let pid = Json::from(u32::from(m.peek16(p + P_PID) as u8));
            match m.peek(p + P_STATUS) {
                2 | 6 => ready.push(pid),
                3 | 4 => sleep.push(pid),
                7 => zombie.push(pid),
                _ => {}
            }
        }
        vec![("ready", Json::Arr(ready)), ("sleep", Json::Arr(sleep)), ("zombie", Json::Arr(zombie))]
    }
    fn svc_at(&self, read: &dyn Fn(u32) -> u8, pc: u32) -> Option<Svc> {
        (read(pc) == 0x4e && read(pc.wrapping_add(1)) == 0x4c).then_some(Svc {
            code: ANY,
            len: 2,
            no_return: false,
        })
    }
    fn svc_regs(&self, svc: Svc, r: &CpuRegs) -> Svc {
        let n = regs(r).d[0] as u16;
        let code = if n < 80 { n as u8 } else { ANY };
        Svc {
            code,
            len: svc.len,
            no_return: code == 0,
        }
    }
    fn svc_prefix(&self) -> &'static [u8] {
        &[0x4e]
    }
    fn svc_mnemonic(&self) -> &'static str {
        "TRAP"
    }
    fn service_name(&self, code: u8) -> String {
        match code {
            ANY => "#12 (FUZIX)".into(),
            c => NAMES.get(c as usize).map_or_else(|| format!("sys{c}"), |n| n.to_string()),
        }
    }
    fn service_code(&self, name: &str) -> Option<u8> {
        let n = name.trim();
        NAMES
            .iter()
            .position(|x| *x == n || x.trim_start_matches('_') == n.trim_start_matches('_'))
            .map(|i| i as u8)
            .or_else(|| n.parse::<u8>().ok().filter(|&c| c < 80))
    }
    fn explain(&self, code: u8) -> &'static str {
        let name = NAMES.get(code as usize).copied().unwrap_or("");
        EXPLAIN.iter().find(|e| e.0 == name).map_or("", |e| e.2)
    }
    fn error_name(&self, code: u8) -> String {
        ERRNO.get(code as usize).filter(|n| !n.is_empty()).map_or_else(|| format!("errno {code}"), |n| n.to_string())
    }
    fn services(&self) -> Vec<Service> {
        NAMES
            .iter()
            .enumerate()
            .map(|(i, n)| Service {
                code: i as u8,
                name: n.to_string(),
                desc: EXPLAIN.iter().find(|e| e.0 == *n).map_or("", |e| e.1).into(),
                explain: EXPLAIN.iter().find(|e| e.0 == *n).map_or("", |e| e.2),
            })
            .collect()
    }
    fn image_calls(&self) -> &'static [u8] {
        &[23, 32, 0]
    }
    fn current(&self, m: &dyn Mem) -> u32 {
        self.s().map_or(0, |s| m.peek32(s.udata_shadow))
    }
    fn pid_of(&self, m: &dyn Mem, key: u32) -> u8 {
        if key == 0 {
            return 0;
        }
        let p = m.peek32(key + U_PTAB);
        if p == 0 { 0 } else { m.peek16(p + P_PID) as u8 }
    }
    fn in_system(&self, _: &dyn Mem, _: u32) -> bool {
        false
    }
    fn describe_entry(&self, m: &dyn Mem, paths: &PathNames, pid: u8, code: u8, r: &CpuRegs) -> String {
        let r = regs(r);
        let (a1, a2, a3, a4) = (r.d[1], r.a[0], r.a[1], r.a[2]);
        let path = |a: u32| format!("\"{}\"", cstr(m, a, 64));
        let name = NAMES.get(code as usize).copied().unwrap_or("");
        match name {
            "_exit" => format!("終了コード{}", a1 as i32),
            "open" => format!("{} フラグ${:X} モード{:o}", path(a1), a2, a3),
            "close" | "dup" | "_fstat" | "fchdir" => format!("fd {}", self.fd(paths, pid, a1)),
            "read" => format!("fd {} ${:06X} {}バイト", self.fd(paths, pid, a1), a2, a3),
            "write" => format!("fd {} {}バイト \"{}\"", self.fd(paths, pid, a1), a3, bytes_preview(m, a2, a3)),
            "_lseek" => format!("fd {} 位置${:X} 基準{}", self.fd(paths, pid, a1), a2, a3),
            "chdir" | "unlink" | "rmdir" | "chroot" | "_stat" | "access" | "chmod" | "chown" | "mkdir" | "_statfs" => {
                path(a1)
            }
            "rename" | "link" => format!("{} → {}", path(a1), path(a2)),
            "execve" => {
                let mut argv = Vec::new();
                for i in 0..8 {
                    let p = m.peek32(a2 + 4 * i);
                    if p == 0 {
                        break;
                    }
                    argv.push(cstr(m, p, 32));
                }
                format!("{} [{}]", path(a1), argv.join(" "))
            }
            "_fork" => format!("フラグ${a1:X}"),
            "waitpid" => format!("pid {} 状態 ${:06X} オプション{}", a1 as i32, a2, a3),
            "kill" => format!("pid {} シグナル{}", a1 as i32, a2),
            "signal" => format!("シグナル{} 処理 ${:06X}", a1, a2),
            "dup2" => format!("fd {} → {}", self.fd(paths, pid, a1), a2),
            "brk" => format!("${a1:06X}"),
            "sbrk" => format!("{}バイト", a1 as i32),
            "ioctl" => format!("fd {} 要求${:X} ${:06X}", self.fd(paths, pid, a1), a2, a3),
            "pipe" => format!("配列 ${a1:06X}"),
            "_getdirent" => format!("fd {} ${:06X} {}バイト", self.fd(paths, pid, a1), a2, a3),
            "_select" => format!("{}個", a1),
            _ => {
                let _ = a4;
                String::new()
            }
        }
    }
    fn describe_exit(&self, m: &dyn Mem, call: &Call, r: &CpuRegs) -> String {
        let out = regs(r);
        let d0 = out.d[0] as i32;
        if d0 == -1 {
            let e = out.d[1] as u16;
            return format!("-1 {}", self.error_name(e as u8));
        }
        let inp = regs(&call.inp);
        match NAMES.get(call.code as usize).copied().unwrap_or("") {
            "open" | "dup" | "dup2" => format!("fd {d0}"),
            "_fork" => {
                if d0 == 0 {
                    "0（子）".into()
                } else {
                    format!("子のpid {d0}")
                }
            }
            "waitpid" => {
                let st = m.peek16(inp.a[0]);
                format!("pid {d0} 状態${st:04X}（終了コード{}）", st >> 8)
            }
            "read" => format!("{}バイト \"{}\"", d0, bytes_preview(m, inp.a[0], d0.max(0) as u32)),
            "write" | "_getdirent" => format!("{d0}バイト"),
            "pipe" => format!("fd {} と {}", m.peek32(inp.d[1]) as i32, m.peek32(inp.d[1] + 4) as i32),
            "brk" | "sbrk" => format!("${:06X}", out.d[0]),
            _ => format!("{d0}"),
        }
    }
    fn error_of(&self, r: &CpuRegs) -> Option<u8> {
        let r = regs(r);
        (r.d[0] as i32 == -1).then_some(r.d[1] as u8)
    }
    fn learn(&self, call: &Call, out: &CpuRegs, pid: u8, paths: &mut PathNames) {
        let d0 = regs(out).d[0] as i32;
        let inp = regs(&call.inp);
        match NAMES.get(call.code as usize).copied().unwrap_or("") {
            "open" if d0 >= 0 => {
                let name = call.args.split('"').nth(1).unwrap_or("").to_string();
                paths.insert((pid, d0 as u8), name);
            }
            "close" => {
                paths.remove(&(pid, inp.d[1] as u8));
            }
            "dup" | "dup2" if d0 >= 0 => {
                if let Some(p) = paths.get(&(pid, inp.d[1] as u8)).cloned() {
                    paths.insert((pid, d0 as u8), p);
                }
            }
            _ => {}
        }
    }
    fn processes(&self, m: &dyn Mem) -> Vec<Process> {
        self.entries(m)
            .into_iter()
            .map(|p| {
                let st = m.peek(p + P_STATUS);
                let pptr = m.peek32(p + P_PPTR);
                let udata = m.peek32(p + P_UDATA);
                let wait = m.peek32(p + P_WAIT);
                let id = m.peek16(p + P_PID) as u8;
                // init is its own parent.
                let parent = if pptr == 0 || pptr == p { 0 } else { m.peek16(pptr + P_PID) as u8 };
                Process {
                    id,
                    parent,
                    key: udata,
                    name: name8(m, p + P_NAME),
                    queue: QUEUE.get(st as usize).copied().unwrap_or("-"),
                    fields: vec![
                        ("name", name8(m, p + P_NAME).into()),
                        ("status", STATUS.get(st as usize).copied().unwrap_or("?").into()),
                        ("pid", u32::from(m.peek16(p + P_PID)).into()),
                        ("uid", u32::from(m.peek16(p + P_UID)).into()),
                        ("tty", u32::from(m.peek(p + P_TTY)).into()),
                        ("wait", if wait == 0 { Json::Null } else { format!("${wait:06X}").into() }),
                        ("priority", u32::from(m.peek16(p + P_PRIORITY)).into()),
                        ("nice", u32::from(m.peek(p + P_NICE)).into()),
                        ("size", format!("{}K", m.peek32(p + P_SIZE)).into()),
                        ("ticks", m.peek32(p + P_TIME).into()),
                        ("exit", u32::from(m.peek16(p + P_EXITVAL)).into()),
                        ("pgrp", u32::from(m.peek16(p + P_PGRP)).into()),
                        ("ptab", format!("${p:06X}").into()),
                        ("udata", format!("${udata:06X}").into()),
                    ],
                }
            })
            .collect()
    }
    fn images(&self, m: &dyn Mem) -> Vec<Image> {
        let Some(s) = self.s() else { return Vec::new() };
        let mut v = vec![Image {
            name: "kernel".into(),
            fields: vec![("kind", "カーネル".into()), ("addr", "$000400".into())],
            chunks: vec![(0x400, s.kernel_end.saturating_sub(0x400) as usize, 0x400)],
        }];
        for p in self.entries(m) {
            let u = m.peek32(p + P_UDATA);
            if u == 0 || m.peek(p + P_STATUS) == 7 {
                continue;
            }
            let base = m.peek32(u + U_CODEBASE);
            let top = m.peek32(u + U_TOP);
            if base == 0 || top <= base {
                continue;
            }
            let pid = m.peek16(p + P_PID);
            v.push(Image {
                name: format!("{}[{pid}]", name8(m, u + U_NAME)),
                fields: vec![
                    ("kind", "プロセス".into()),
                    ("addr", format!("${base:06X}").into()),
                    ("size", format!("{}K", (top - base) / 1024).into()),
                    ("insys", (m.peek(u + U_INSYS) != 0).into()),
                    ("call", if m.peek(u + U_INSYS) != 0 { self.service_name(m.peek(u + U_CALLNO)).into() } else { Json::Null }),
                ],
                chunks: vec![(base as usize, (top - base) as usize, 0)],
            });
        }
        v
    }
    fn memory_map(&self, m: &dyn Mem) -> Json {
        let mut regions = vec![obj([("name", "例外ベクタ".into()), ("kind", "sys".into()), ("start", 0.into()), ("end", 0x400.into())])];
        for img in self.images(m) {
            let (start, len, _) = img.chunks[0];
            let kind = if img.name == "kernel" { "sys" } else { "prog" };
            regions.push(obj([
                ("name", img.name.clone().into()),
                ("kind", kind.into()),
                ("start", start.into()),
                ("end", (start + len).into()),
            ]));
        }
        obj([("regions", Json::Arr(regions)), ("ram", m.ram().len().into())])
    }
    fn read_space(&self, _: &dyn Mem, _: &str, _: u32) -> Option<u8> {
        None
    }
    fn console_size_writes(&self, m: &dyn Mem, rows: u16, cols: u16) -> Vec<(u32, u8)> {
        let Some(s) = self.s().filter(|s| s.ttydata != 0) else { return Vec::new() };
        // ws_row, ws_col of ttydata[1] (no SIGWINCH: programs started later see it).
        let at = s.ttydata + TTY_SIZE + TTY_WINSIZE;
        if m.peek16(at) == rows && m.peek16(at + 2) == cols {
            return Vec::new();
        }
        let [r0, r1] = rows.to_be_bytes();
        let [c0, c1] = cols.to_be_bytes();
        vec![(at, r0), (at + 1, r1), (at + 2, c0), (at + 3, c1)]
    }
}
