//! NitrOS-9 awareness for the ICE: service names, kernel structures and a
//! system-call tracer. Offsets follow `defs/os9.d` of the reference kit.
use super::tables::{ERRORS, SERVICES, STATUS_CODES};
use crate::{Cpu, Machine};
use std::collections::{BTreeMap, HashMap, VecDeque};

pub fn service_name(code: u8) -> String {
    SERVICES
        .iter()
        .find(|s| s.0 == code)
        .map_or_else(|| format!("${code:02X}"), |s| s.1.to_string())
}
pub fn service_code(name: &str) -> Option<u8> {
    SERVICES
        .iter()
        .find(|s| s.1.eq_ignore_ascii_case(name))
        .map(|s| s.0)
}
pub fn error_name(code: u8) -> String {
    ERRORS
        .iter()
        .find(|s| s.0 == code)
        .map_or_else(|| format!("E${code}"), |s| format!("{} ({})", s.1, s.2))
}
pub fn status_name(code: u8) -> String {
    STATUS_CODES
        .iter()
        .find(|s| s.0 == code)
        .map_or_else(|| format!("${code:02X}"), |s| s.1.to_string())
}

/// What the kernel does for a primitive, in Japanese, for the explainer pane.
pub fn explain(code: u8) -> &'static str {
    match code {
        0x00 => {
            "モジュールディレクトリから名前と種別が一致するモジュールを探し、リンク数を1増やして実行開始アドレス(Y)とヘッダ(U)を返す。Level 2では見つけたモジュールのブロックを呼び出し側のDATイメージへマップする。"
        }
        0x01 => {
            "パスリストのファイルを開いてモジュールを読み込み、CRCを検証してモジュールディレクトリへ登録し、F$Linkと同じくリンクする。"
        }
        0x02 => {
            "モジュールのリンク数を1減らす。0になったモジュールはディレクトリから外され、メモリが解放される。"
        }
        0x03 => {
            "新しいプロセス記述子を確保(F$AllPrc)し、モジュールをリンク、データ領域を確保して親子関係(P$PID/P$CID/P$SID)とパス0〜2を引き継ぎ、アクティブキューへ入れる。子は次のスケジューリングで実行を始める。"
        }
        0x04 => {
            "子プロセスの終了を待つ。終了済みの子がいれば即座に返し、いなければ自分を待機キュー(D.WProcQ)へ移して他プロセスへ切り替える。子のF$Exitで起こされ、A=子のID、B=終了ステータス。"
        }
        0x05 => {
            "現在のプロセスを保ったまま実行モジュールだけを入れ替える(exec相当)。旧モジュールをアンリンクし、新モジュールの先頭から再開するため呼び出し元へは戻らない。"
        }
        0x06 => {
            "プロセスを終了する。全パスを閉じ、モジュールをアンリンクしてメモリを返し、親がF$Waitしていれば起こす。記述子は親が回収するまで残る。呼び出し元へは戻らない。"
        }
        0x07 => "プロセスのデータ領域サイズを変更する。D=新しいサイズ(0なら現在値の問い合わせ)。",
        0x08 => {
            "プロセスAへシグナルBを送る。相手が眠っていれば起こしてアクティブキューへ戻す。シグナル0(S$Kill)は強制終了。"
        }
        0x09 => "シグナル受信時に呼ばれるインターセプトルーチン(X)とそのデータ領域(U)を登録する。",
        0x0A => {
            "Xティックだけ眠る(0なら起こされるまで)。プロセスは睡眠キュー(D.SProcQ)へ入り、時計割り込みかシグナルで起こされる。I/O待ちのドライバもこれを使う。"
        }
        0x0C => "現在のプロセスID(A)とユーザーID(Y)を返す。",
        0x0D => {
            "プロセスAの優先度をBに設定する。スケジューラは優先度とエージ(P$Age)で次に走るプロセスを選ぶ。"
        }
        0x0F => "パスAへエラー番号Bのメッセージを表示する。",
        0x10 => "パスリストから1要素(名前)を切り出す。B=名前の長さ、Y=次の要素。",
        0x15 => "現在時刻(年月日時分秒の6バイト)をXのバッファへ書く。",
        0x16 => "システム時刻を設定する。起動時の Time ? 入力はここへ渡される。",
        0x18 => "プロセスAの記述子をXのバッファへコピーする(procsなどが使う)。",
        0x1B => "DATイメージ(D)で示す別アドレス空間のXからYバイトをUへコピーする。",
        0x28 => "システムメモリをDバイト確保し、U=先頭を返す(カーネル・ドライバ用)。",
        0x29 => "F$SRqMemで得たシステムメモリ(U, Dバイト)を返却する。",
        0x2B => "I/Oキューに入ってプロセスAの入出力完了を待つ。デバイスが使用中のときに使われる。",
        0x2C => {
            "プロセス(X)をアクティブキューへ入れる。キューはエージ順に並び、待っている間にエージが上がる。"
        }
        0x2D => {
            "次に実行するプロセスをアクティブキューから選び、そのレジスタを復元して切り替える(ディスパッチ)。呼び出し元へは戻らない。"
        }
        0x2F => {
            "64バイト単位の表(X)からインデックスAのブロックを探す。プロセス/パス記述子の参照に使う。"
        }
        0x30 => "64バイト単位の表(X)に空きブロックを確保する。パス記述子の確保に使う。",
        0x31 => "64バイト単位の表(X)のブロックAを解放する。",
        0x34 => {
            "システム側(Level 2)のモジュールリンク。DATイメージYの空間へモジュールをマップする。"
        }
        0x38 => {
            "タスクAの空間のXからタスクBの空間のUへYバイト転送する(Level 2のアドレス空間間コピー)。"
        }
        0x39 => "物理RAMブロックを確保する(Level 2のブロックマップ)。",
        0x3A => "プロセスのDATイメージに新しいRAMブロックを割り当てる。",
        0x3F => "プロセス用のハードウェアタスク番号を割り当て、DATイメージをMMUへ書き込む。",
        0x4B => "プロセス記述子(Level 2では512バイト)を確保し、プロセス表D.PrcDBTへ登録する。",
        0x4C => "プロセス記述子を解放する。",
        0x80 => {
            "デバイス(ディスクリプタ名X)をデバイステーブルへ登録し、ドライバとファイルマネージャを初期化する。"
        }
        0x81 => "デバイスの使用数を減らし、0になればドライバを終了してテーブルから外す。",
        0x82 => {
            "パスAを複製し、空いている最小のパス番号を返す。シェルのリダイレクトでstdin/stdoutを付け替えるのに使う。"
        }
        0x83 => {
            "ファイルを新規作成して開く。ファイルマネージャ(RBF等)がディレクトリへ項目を追加する。A=新しいパス番号。"
        }
        0x84 => {
            "パスリストXを開く。IOManがデバイスをアタッチし、パス記述子を確保してファイルマネージャのOpenを呼ぶ。A=パス番号。"
        }
        0x85 => "ディレクトリを作成する。",
        0x86 => "カレント(データ/実行)ディレクトリを変更する。chdやchxの実体。",
        0x87 => "ファイルを削除する。",
        0x88 => "パスAのファイル位置をX:Uへ移動する。",
        0x89 => "パスAからYバイトをXへ読む(生データ)。Y=実際に読んだバイト数。",
        0x8A => "パスAへXのYバイトを書く(生データ)。",
        0x8B => "パスAから1行(CRまで、最大Yバイト)をXへ読む。SCFでは行編集・エコーが行われる。",
        0x8C => "パスAへ1行(CRまで、最大Yバイト)書く。SCFではCRの後にLFを補う。",
        0x8D => "パスAの状態を取得する。Bが機能コード(SS.xxx)。",
        0x8E => "パスAの状態を設定する。Bが機能コード(SS.xxx)。",
        0x8F => "パスAを閉じる。パス記述子を解放し、使われなくなったデバイスはデタッチする。",
        0x90 => "実行ディレクトリ基準でファイルを削除する。",
        _ => "",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    One,
    Two,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Regs {
    pub a: u8,
    pub b: u8,
    pub dp: u8,
    pub cc: u8,
    pub x: u16,
    pub y: u16,
    pub u: u16,
    pub s: u16,
    pub pc: u16,
}
impl Regs {
    pub fn of(c: &Cpu) -> Self {
        Self {
            a: c.a,
            b: c.b,
            dp: c.dp,
            cc: c.cc,
            x: c.x,
            y: c.y,
            u: c.u,
            s: c.s,
            pc: c.pc,
        }
    }
    pub fn d(&self) -> u16 {
        u16::from_be_bytes([self.a, self.b])
    }
}

/// Offsets that differ between Level 1 and Level 2 process descriptors.
struct ProcLayout {
    state: u16,
    queue: u16,
    prior: u16,
    age: u16,
    user: u16,
    pmodul: u16,
    signal: u16,
    path: u16,
    pagcnt: u16,
}
const PROC_L1: ProcLayout = ProcLayout {
    pagcnt: 0x08,
    user: 0x09,
    prior: 0x0b,
    age: 0x0c,
    state: 0x0d,
    queue: 0x0e,
    pmodul: 0x12,
    path: 0x26,
    signal: 0x36,
};
const PROC_L2: ProcLayout = ProcLayout {
    pagcnt: 0x07,
    user: 0x08,
    prior: 0x0a,
    age: 0x0b,
    state: 0x0c,
    queue: 0x0d,
    pmodul: 0x11,
    signal: 0x19,
    path: 0x30,
};

/// Direct-page system globals, by level.
struct Globals {
    proc_: u16,
    aq: u16,
    wq: u16,
    sq: u16,
    prcdbt: u16,
    moddir: u16,
    init: u16,
}
const DP_L1: Globals = Globals {
    proc_: 0x4b,
    aq: 0x4d,
    wq: 0x4f,
    sq: 0x51,
    prcdbt: 0x49,
    moddir: 0x26,
    init: 0x2a,
};
const DP_L2: Globals = Globals {
    proc_: 0x50,
    aq: 0x52,
    wq: 0x54,
    sq: 0x56,
    prcdbt: 0x48,
    moddir: 0x44,
    init: 0x24,
};

#[derive(Debug, Clone)]
pub struct Process {
    pub id: u8,
    pub addr: u16,
    pub parent: u8,
    pub sibling: u8,
    pub child: u8,
    pub sp: u16,
    pub task: Option<u8>,
    pub pages: u8,
    pub user: u16,
    pub prior: u8,
    pub age: u8,
    pub state: u8,
    pub queue: &'static str,
    pub module: String,
    pub signal: u8,
    pub paths: [u8; 16],
    pub dat: Vec<u16>,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub addr: u16,
    pub size: u16,
    pub kind: u8,
    pub revs: u8,
    pub links: u16,
    pub blocks: Vec<u16>,
}

/// Read-only view of OS-9 structures in a paused (or running) machine.
pub struct View<'a> {
    pub m: &'a Machine,
    pub level: Level,
    sysdat: [u16; 8],
}

fn module_ok(read: &dyn Fn(u16) -> u8, at: u16) -> bool {
    read(at) == 0x87 && read(at.wrapping_add(1)) == 0xcd
}
fn name_at(read: &dyn Fn(u16) -> u8, at: u16) -> String {
    let mut s = String::new();
    for i in 0..32u16 {
        let c = read(at.wrapping_add(i));
        let ch = c & 0x7f;
        if !(0x21..0x7f).contains(&ch) {
            break;
        }
        s.push(char::from(ch));
        if c & 0x80 != 0 {
            break;
        }
    }
    s
}

impl<'a> View<'a> {
    /// Detect the OS from the Init module pointer; `None` before OS-9 runs.
    pub fn detect(m: &'a Machine) -> Option<Self> {
        let phys16 = |a: usize| u16::from_be_bytes([m.ram[a], m.ram[a + 1]]);
        for level in [Level::One, Level::Two] {
            let v = Self::new(m, level);
            let init = phys16(usize::from(v.g().init));
            if module_ok(&|a| v.sys(a), init) && v.module_name(&|a| v.sys(a), init) == "Init" {
                return Some(v);
            }
        }
        None
    }
    pub fn new(m: &'a Machine, level: Level) -> Self {
        let mut v = Self {
            m,
            level,
            sysdat: [0x333e; 8],
        };
        if level == Level::Two {
            let ptr = u16::from_be_bytes([v.phys(0x4c), v.phys(0x4d)]);
            for (i, b) in v.sysdat.iter_mut().enumerate() {
                let a = usize::from(ptr.wrapping_add(i as u16 * 2));
                if a < 0x1fff {
                    *b = u16::from_be_bytes([m.ram[a], m.ram[a + 1]]);
                }
            }
        }
        v
    }
    fn g(&self) -> &'static Globals {
        match self.level {
            Level::One => &DP_L1,
            Level::Two => &DP_L2,
        }
    }
    fn p(&self) -> &'static ProcLayout {
        match self.level {
            Level::One => &PROC_L1,
            Level::Two => &PROC_L2,
        }
    }
    fn phys(&self, p: usize) -> u8 {
        self.m.ram.get(p).copied().unwrap_or(0xff)
    }
    /// Read through a DAT image: one 16-bit block number per 8 KiB slot.
    fn dat_read(&self, image: &[u16], addr: u16) -> u8 {
        match image.get(usize::from(addr >> 13)) {
            Some(&b) if b != 0x333e => self.phys(usize::from(b) * 8192 + usize::from(addr & 8191)),
            _ => 0xff,
        }
    }
    /// The system address space. Level 2 keeps system block 0 at physical 0.
    pub fn sys(&self, addr: u16) -> u8 {
        match self.level {
            Level::One => self.m.peek(addr),
            Level::Two => {
                if addr < 0x2000 {
                    return self.phys(usize::from(addr));
                }
                self.dat_read(&self.sysdat, addr)
            }
        }
    }
    pub fn sys16(&self, addr: u16) -> u16 {
        u16::from_be_bytes([self.sys(addr), self.sys(addr.wrapping_add(1))])
    }
    fn image_at(&self, ptr: u16) -> [u16; 8] {
        std::array::from_fn(|i| self.sys16(ptr.wrapping_add(i as u16 * 2)))
    }
    pub fn sys_dat(&self) -> [u16; 8] {
        self.sysdat
    }
    /// A process' own address space (Level 2 DAT image, flat in Level 1).
    pub fn proc_read(&self, desc: u16, addr: u16) -> u8 {
        match self.level {
            Level::One => self.m.peek(addr),
            Level::Two => {
                let img = self.image_at(desc.wrapping_add(0x40));
                self.dat_read(&img, addr)
            }
        }
    }
    fn module_name(&self, read: &dyn Fn(u16) -> u8, at: u16) -> String {
        let off = u16::from_be_bytes([read(at.wrapping_add(4)), read(at.wrapping_add(5))]);
        name_at(read, at.wrapping_add(off))
    }
    pub fn current(&self) -> u16 {
        self.sys16(self.g().proc_)
    }
    pub fn pid_of(&self, desc: u16) -> u8 {
        if desc == 0 { 0 } else { self.sys(desc) }
    }
    fn queue(&self, head: u16) -> Vec<u16> {
        let mut v = Vec::new();
        let mut p = self.sys16(head);
        while p != 0 && v.len() < 64 && !v.contains(&p) {
            v.push(p);
            p = self.sys16(p.wrapping_add(self.p().queue));
        }
        v
    }
    pub fn queues(&self) -> [Vec<u8>; 3] {
        let g = self.g();
        [g.aq, g.wq, g.sq].map(|h| self.queue(h).into_iter().map(|p| self.pid_of(p)).collect())
    }
    fn descriptor(&self, id: u8) -> Option<u16> {
        let dbt = self.sys16(self.g().prcdbt);
        if dbt == 0 || id == 0 {
            return None;
        }
        let addr = match self.level {
            Level::One => {
                let page = self.sys(dbt.wrapping_add(u16::from(id >> 2)));
                (page != 0).then(|| (u16::from(page) << 8) | (u16::from(id & 3) << 6))?
            }
            Level::Two => {
                let page = self.sys(dbt.wrapping_add(u16::from(id)));
                (page != 0).then(|| u16::from(page) << 8)?
            }
        };
        (self.sys(addr) == id).then_some(addr)
    }
    pub fn processes(&self) -> Vec<Process> {
        let current = self.current();
        let g = self.g();
        let [aq, wq, sq] = [g.aq, g.wq, g.sq].map(|h| self.queue(h));
        let l = self.p();
        let mut out = Vec::new();
        for id in 1..=255u8 {
            let Some(a) = self.descriptor(id) else {
                continue;
            };
            let r16 = |o: u16| self.sys16(a.wrapping_add(o));
            let r = |o: u16| self.sys(a.wrapping_add(o));
            let pmod = r16(l.pmodul);
            let module = if pmod == 0 {
                String::new()
            } else if module_ok(&|x| self.proc_read(a, x), pmod) {
                self.module_name(&|x| self.proc_read(a, x), pmod)
            } else if module_ok(&|x| self.sys(x), pmod) {
                self.module_name(&|x| self.sys(x), pmod)
            } else {
                format!("${pmod:04X}")
            };
            let queue = if a == current {
                "running"
            } else if aq.contains(&a) {
                "active"
            } else if wq.contains(&a) {
                "wait"
            } else if sq.contains(&a) {
                "sleep"
            } else {
                "-"
            };
            let mut paths = [0; 16];
            for (i, p) in paths.iter_mut().enumerate() {
                *p = r(l.path + i as u16);
            }
            let two = self.level == Level::Two;
            out.push(Process {
                id,
                addr: a,
                parent: r(1),
                sibling: r(2),
                child: r(3),
                sp: r16(4),
                task: two.then(|| r(6)),
                pages: r(l.pagcnt),
                user: r16(l.user),
                prior: r(l.prior),
                age: r(l.age),
                state: r(l.state),
                queue,
                module,
                signal: r(l.signal),
                paths,
                dat: if two {
                    self.image_at(a.wrapping_add(0x40)).to_vec()
                } else {
                    Vec::new()
                },
            });
        }
        out
    }
    pub fn modules(&self) -> Vec<Module> {
        let start = self.sys16(self.g().moddir);
        // Level 2 entries end at D.ModEnd; D.ModDir+2 bounds the DAT images
        // that grow down from the top of the same area.
        let end = match self.level {
            Level::One => self.sys16(self.g().moddir + 2),
            Level::Two => self.sys16(0x58),
        };
        let step = if self.level == Level::One { 4 } else { 8 };
        let mut out = Vec::new();
        let mut e = start;
        while e < end && out.len() < 256 {
            // Level 2: MD$MPDAT, MD$MBSiz, MD$MPtr (relative to that image), MD$Link.
            let (ptr, links, image) = match self.level {
                Level::One => (self.sys16(e), u16::from(self.sys(e + 2)), [0x333e; 8]),
                Level::Two => {
                    let img = self.sys16(e);
                    let image = if img == 0 {
                        [0x333e; 8]
                    } else {
                        self.image_at(img)
                    };
                    (self.sys16(e + 4), self.sys16(e + 6), image)
                }
            };
            e += step;
            if ptr == 0 && links == 0 {
                continue;
            }
            let read = |a: u16| match self.level {
                Level::One => self.m.peek(a),
                Level::Two => self.dat_read(&image, a),
            };
            if !module_ok(&read, ptr) {
                continue;
            }
            let size = u16::from_be_bytes([read(ptr + 2), read(ptr + 3)]);
            // The physical blocks the module itself occupies, not its whole image.
            let blocks = if self.level == Level::Two {
                let last = ptr.saturating_add(size.max(1) - 1);
                image[usize::from(ptr >> 13)..=usize::from(last >> 13)].to_vec()
            } else {
                Vec::new()
            };
            out.push(Module {
                name: self.module_name(&read, ptr),
                addr: ptr,
                size,
                kind: read(ptr + 6),
                revs: read(ptr + 7),
                links,
                blocks,
            });
        }
        out
    }
    /// Level 1: 256-byte page bitmap; Level 2: physical 8 KiB block flags.
    pub fn memory_map(&self) -> (u16, Vec<u8>) {
        match self.level {
            Level::One => {
                let start = self.sys16(0x20);
                let end = self.sys16(0x22);
                let n = end.saturating_sub(start).min(64);
                let mut v = Vec::new();
                for i in 0..n {
                    let byte = self.sys(start + i);
                    for bit in 0..8 {
                        v.push((byte >> (7 - bit)) & 1);
                    }
                }
                (256, v)
            }
            Level::Two => {
                let start = self.sys16(0x40);
                let end = self.sys16(0x42);
                let n = end.saturating_sub(start).min(256);
                (8192, (0..n).map(|i| self.sys(start + i)).collect())
            }
        }
    }
}

/// One traced service request, filled in on entry and completed on return.
#[derive(Debug, Clone)]
pub struct Call {
    pub id: u64,
    pub version: u64,
    pub cycle: u64,
    pub end: Option<u64>,
    pub pid: u8,
    pub code: u8,
    pub system: bool,
    pub depth: u8,
    /// Innermost unfinished call of the same process when this one started.
    pub parent: Option<u64>,
    /// Kernel-internal requests made while this user call ran, by code.
    /// Kept even after the individual internal calls leave the log.
    pub inner: Vec<(u8, u32)>,
    pub inp: Regs,
    pub out: Option<Regs>,
    pub args: String,
    pub result: String,
    pub error: Option<u8>,
    /// Never returns to the caller (F$Exit, F$Chain, F$NProc) or was abandoned.
    pub no_return: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Switch {
    pub cycle: u64,
    pub from: u8,
    pub to: u8,
}

struct Pending {
    index: u64,
    /// The outermost user-state call this request runs on behalf of.
    root: u64,
    ret_pc: u16,
    s: u16,
    pid: u8,
}

/// Records every SWI2 service request and context switch.
pub struct Tracer {
    pub calls: BTreeMap<u64, Call>,
    user_ids: VecDeque<u64>,
    system_ids: VecDeque<u64>,
    pub switches: VecDeque<Switch>,
    next_id: u64,
    pub version: u64,
    pending: Vec<Pending>,
    ret_hint: Vec<u8>,
    last_proc: u16,
    paths: HashMap<(u8, u8), String>,
    capacity: usize,
}
impl Default for Tracer {
    fn default() -> Self {
        Self {
            calls: BTreeMap::new(),
            user_ids: VecDeque::new(),
            system_ids: VecDeque::new(),
            switches: VecDeque::new(),
            next_id: 1,
            version: 0,
            pending: Vec::new(),
            ret_hint: vec![0; 65536],
            last_proc: 0,
            paths: HashMap::new(),
            capacity: 60000,
        }
    }
}

fn printable(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        match b {
            b'\r' => s.push_str("\\r"),
            b'\n' => s.push_str("\\n"),
            0x20..=0x7e => s.push(char::from(b)),
            _ => s.push_str(&format!("\\x{b:02X}")),
        }
    }
    s
}
fn path_string(m: &Machine, at: u16) -> String {
    let mut v = Vec::new();
    for i in 0..48u16 {
        let c = m.peek(at.wrapping_add(i));
        let ch = c & 0x7f;
        if ch <= 0x20 || ch == 0x7f {
            break;
        }
        v.push(ch);
        if c & 0x80 != 0 {
            break;
        }
    }
    String::from_utf8_lossy(&v).into_owned()
}
fn preview(m: &Machine, at: u16, len: u16) -> String {
    let n = len.min(64);
    let mut bytes: Vec<u8> = (0..n).map(|i| m.peek(at.wrapping_add(i))).collect();
    if let Some(cr) = bytes.iter().position(|&b| b == b'\r') {
        bytes.truncate(cr + 1);
        let mut s = printable(&bytes);
        if usize::from(len) > cr + 1 {
            s.push('…');
        }
        return s;
    }
    let mut s = printable(&bytes);
    if len > n {
        s.push('…');
    }
    s
}

impl Tracer {
    fn path_label(&self, pid: u8, path: u8) -> String {
        match self.paths.get(&(pid, path)) {
            Some(n) => format!("{path}({n})"),
            None => match path {
                0 => "0(stdin)".into(),
                1 => "1(stdout)".into(),
                2 => "2(stderr)".into(),
                _ => path.to_string(),
            },
        }
    }
    fn describe_entry(&self, m: &Machine, pid: u8, code: u8, r: &Regs) -> String {
        let path = |p| self.path_label(pid, p);
        match code {
            0x00 | 0x01 => format!("X→\"{}\" 種別A=${:02X}", path_string(m, r.x), r.a),
            0x02 => format!("U=${:04X}", r.u),
            0x03 | 0x05 => format!(
                "モジュール\"{}\" パラメータ\"{}\" データ{}ページ",
                path_string(m, r.x),
                preview(m, r.u, r.y),
                r.b
            ),
            0x04 => "子プロセスの終了を待つ".into(),
            0x06 => format!("終了ステータスB={}", r.b),
            0x0C => String::new(),
            0x10 | 0x11 => format!("\"{}\"", path_string(m, r.x)),
            0x15 => format!("→ X=${:04X}", r.x),
            0x07 => format!("D={}バイト", r.d()),
            0x08 => format!("プロセス{}へシグナル{}", r.a, r.b),
            0x09 => format!("ハンドラX=${:04X} データU=${:04X}", r.x, r.u),
            0x0A => {
                if r.x == 0 {
                    "起こされるまで".into()
                } else {
                    format!("{}ティック", r.x)
                }
            }
            0x0D => format!("プロセス{} 優先度{}", r.a, r.b),
            0x0F => format!("パス{} {}", path(r.a), error_name(r.b)),
            0x16 => {
                let t: Vec<u8> = (0..6).map(|i| m.peek(r.x.wrapping_add(i))).collect();
                format!(
                    "{}/{:02}/{:02} {:02}:{:02}:{:02}",
                    1900 + u32::from(t[0]),
                    t[1],
                    t[2],
                    t[3],
                    t[4],
                    t[5]
                )
            }
            0x18 => format!("プロセス{} → X=${:04X}", r.a, r.x),
            0x28 | 0x29 => format!("D={}バイト U=${:04X}", r.d(), r.u),
            0x38 => format!(
                "タスク{}:${:04X} → タスク{}:${:04X} {}バイト",
                r.a, r.x, r.b, r.u, r.y
            ),
            0x80 => format!("デバイス\"{}\" モードA=${:02X}", path_string(m, r.x), r.a),
            0x82 | 0x8F => format!("パス{}", path(r.a)),
            0x83 | 0x84 | 0x85 | 0x86 | 0x87 | 0x90 => {
                format!("\"{}\" モードA=${:02X}", path_string(m, r.x), r.a)
            }
            0x88 => format!("パス{} 位置${:04X}{:04X}", path(r.a), r.x, r.u),
            0x89 | 0x8B => format!("パス{} 最大{}バイト → X=${:04X}", path(r.a), r.y, r.x),
            0x8A | 0x8C => format!(
                "パス{} {}バイト \"{}\"",
                path(r.a),
                r.y,
                preview(m, r.x, r.y)
            ),
            0x8D | 0x8E => format!("パス{} {}", path(r.a), status_name(r.b)),
            _ => format!(
                "A=${:02X} B=${:02X} X=${:04X} Y=${:04X} U=${:04X}",
                r.a, r.b, r.x, r.y, r.u
            ),
        }
    }
    fn describe_exit(&self, m: &Machine, call: &Call, r: &Regs) -> String {
        if r.cc & 1 != 0 {
            return format!("エラー {}", error_name(r.b));
        }
        let i = &call.inp;
        match call.code {
            0x00 | 0x01 => format!("ヘッダU=${:04X} 実行Y=${:04X}", r.u, r.y),
            0x03 => format!("子プロセスID={}", r.a),
            0x04 => format!("子{}が終了 ステータス{}", r.a, r.b),
            0x07 => format!("サイズ{} 上限Y=${:04X}", r.d(), r.y),
            0x0A => format!("残りX={}", r.x),
            0x0C => format!("ID={} ユーザー{}", r.a, r.y),
            0x10 => format!("長さB={}", r.b),
            0x15 => {
                let t: Vec<u8> = (0..6).map(|k| m.peek(i.x.wrapping_add(k))).collect();
                format!(
                    "{}/{:02}/{:02} {:02}:{:02}:{:02}",
                    1900 + u32::from(t[0]),
                    t[1],
                    t[2],
                    t[3],
                    t[4],
                    t[5]
                )
            }
            0x28 => format!("U=${:04X}", r.u),
            0x80 => format!("デバイステーブルU=${:04X}", r.u),
            0x82..=0x84 => format!("パス番号A={}", r.a),
            0x89 | 0x8B => format!("{}バイト \"{}\"", r.y, preview(m, i.x, r.y)),
            0x8A | 0x8C => format!("{}バイト書いた", r.y),
            _ => "OK".into(),
        }
    }

    /// User calls and system-state calls are kept separately so the flood
    /// of kernel-internal requests never evicts a pending user request.
    fn push(&mut self, c: Call) {
        let (ids, cap) = if c.system {
            (&mut self.system_ids, self.capacity)
        } else {
            (&mut self.user_ids, self.capacity / 4)
        };
        if ids.len() >= cap
            && let Some(old) = ids.pop_front()
        {
            self.calls.remove(&old);
        }
        ids.push_back(c.id);
        self.calls.insert(c.id, c);
    }
    fn get_mut(&mut self, id: u64) -> Option<&mut Call> {
        self.calls.get_mut(&id)
    }
    pub fn last_id(&self) -> u64 {
        self.next_id - 1
    }
    /// Call before executing the instruction at `cpu.pc`. Returns the new
    /// call id when the instruction is an OS-9 service request.
    pub fn before(&mut self, m: &Machine, view: &View, cpu: &Cpu) -> Option<u64> {
        if m.peek(cpu.pc) != 0x10 || m.peek(cpu.pc.wrapping_add(1)) != 0x3f {
            return None;
        }
        let code = m.peek(cpu.pc.wrapping_add(2));
        let desc = view.current();
        let pid = view.pid_of(desc);
        let state_off = if view.level == Level::One { 0x0d } else { 0x0c };
        let system = desc != 0 && view.sys(desc.wrapping_add(state_off)) & 0x80 != 0;
        let inp = Regs::of(cpu);
        // Kernel-internal requests are frequent; their registers are formatted on demand.
        let args = if system {
            String::new()
        } else {
            self.describe_entry(m, pid, code, &inp)
        };
        if !system {
            // A process in user state has no unfinished kernel calls: any
            // leftovers left through another path (signals, process exit).
            let ret_hint = &mut self.ret_hint;
            let calls = &mut self.calls;
            self.pending.retain(|p| {
                if p.pid != pid {
                    return true;
                }
                ret_hint[usize::from(p.ret_pc)] -= 1;
                if let Some(c) = calls.get_mut(&p.index) {
                    c.no_return = true;
                }
                false
            });
        }
        let depth = self
            .pending
            .iter()
            .filter(|p| p.pid == pid)
            .count()
            .min(255) as u8;
        let outer = self.pending.iter().rev().find(|p| p.pid == pid);
        let parent = outer.map(|p| p.index);
        let id = self.next_id;
        let root = outer.map_or(id, |p| p.root);
        if root != id
            && let Some(r) = self.calls.get_mut(&root)
        {
            match r.inner.iter_mut().find(|(c, _)| *c == code) {
                Some((_, n)) => *n += 1,
                None => r.inner.push((code, 1)),
            }
        }
        self.next_id += 1;
        self.version += 1;
        let no_return = matches!(code, 0x05 | 0x06 | 0x2d);
        self.push(Call {
            id,
            version: self.version,
            cycle: cpu.cycles,
            end: None,
            pid,
            code,
            system,
            depth,
            parent,
            inner: Vec::new(),
            inp,
            out: None,
            args,
            result: if no_return {
                "(戻らない)".into()
            } else {
                String::new()
            },
            error: None,
            no_return,
        });
        if !no_return {
            let ret_pc = cpu.pc.wrapping_add(3);
            // A process re-entering the same frame abandoned its old call.
            if let Some(i) = self
                .pending
                .iter()
                .position(|p| p.ret_pc == ret_pc && p.s == cpu.s && p.pid == pid)
            {
                let old = self.pending.remove(i);
                self.ret_hint[usize::from(old.ret_pc)] -= 1;
                if let Some(c) = self.get_mut(old.index) {
                    c.no_return = true;
                }
            }
            if self.pending.len() >= 256 {
                let old = self.pending.remove(0);
                self.ret_hint[usize::from(old.ret_pc)] -= 1;
            }
            self.ret_hint[usize::from(ret_pc)] += 1;
            self.pending.push(Pending {
                index: id,
                root,
                ret_pc,
                s: cpu.s,
                pid,
            });
        }
        Some(id)
    }

    /// Call after each instruction. Returns a completed call id and any context switch.
    pub fn after(&mut self, m: &Machine, view: &View, cpu: &Cpu) -> (Option<u64>, Option<Switch>) {
        let mut switch = None;
        let desc = view.current();
        if desc != self.last_proc {
            let s = Switch {
                cycle: cpu.cycles,
                from: view.pid_of(self.last_proc),
                to: view.pid_of(desc),
            };
            self.last_proc = desc;
            if s.from != s.to {
                if self.switches.len() >= self.capacity {
                    self.switches.pop_front();
                }
                self.switches.push_back(s);
                switch = Some(s);
            }
        }
        if self.ret_hint[usize::from(cpu.pc)] == 0 {
            return (None, switch);
        }
        let pid = view.pid_of(desc);
        let Some(i) = self
            .pending
            .iter()
            .rposition(|p| p.ret_pc == cpu.pc && p.s == cpu.s && p.pid == pid)
        else {
            return (None, switch);
        };
        let p = self.pending.remove(i);
        self.ret_hint[usize::from(p.ret_pc)] -= 1;
        let out = Regs::of(cpu);
        let Some(call) = self.calls.get(&p.index) else {
            return (None, switch);
        };
        let result = if call.system && out.cc & 1 == 0 {
            String::new()
        } else {
            self.describe_exit(m, call, &out)
        };
        let (code, system, inp) = (call.code, call.system, call.inp);
        let opened = (matches!(code, 0x83 | 0x84) && !system)
            .then(|| call.args.split('"').nth(1).map(str::to_string))
            .flatten();
        let error = (out.cc & 1 != 0).then_some(out.b);
        // Learn path names so later I/O calls show what a path number refers to.
        if error.is_none() && !system {
            match code {
                0x83 | 0x84 => {
                    if let Some(name) = opened {
                        self.paths.insert((pid, out.a), name);
                    }
                }
                0x82 => {
                    if let Some(n) = self.paths.get(&(pid, inp.a)).cloned() {
                        self.paths.insert((pid, out.a), n);
                    }
                }
                0x8F => {
                    self.paths.remove(&(pid, inp.a));
                }
                0x03 => {
                    let inherited: Vec<_> = (0..3)
                        .filter_map(|k| self.paths.get(&(pid, k)).map(|n| (k, n.clone())))
                        .collect();
                    for (k, n) in inherited {
                        self.paths.insert((out.a, k), n);
                    }
                }
                _ => (),
            }
        }
        self.version += 1;
        let version = self.version;
        if let Some(c) = self.get_mut(p.index) {
            c.end = Some(cpu.cycles);
            c.out = Some(out);
            c.result = result;
            c.error = error;
            c.version = version;
        }
        (Some(p.index), switch)
    }
    pub fn call(&self, id: u64) -> Option<&Call> {
        self.calls.get(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names() {
        assert_eq!(service_name(0x03), "F$Fork");
        assert_eq!(service_name(0x8c), "I$WritLn");
        assert_eq!(service_code("i$open"), Some(0x84));
        assert!(error_name(216).starts_with("E$PNNF"));
        assert_eq!(status_name(0x26), "SS.ScSiz");
    }
}
