//! CP/M-68K for the OS layer: BDOS calls (`TRAP #2`, function in D0.W,
//! parameter in D1) and the BIOS calls under them (`TRAP #3`, function in
//! D0.W, parameters in D1/D2).
//!
//! CP/M-68K runs one program at a time and keeps its state inside the BDOS,
//! whose layout is not published. The current drive, user number, DMA
//! address, open files, TPA and the loaded program are therefore learned
//! from the calls themselves as they return.
use super::json::{Json, obj};
use super::osmon::{Call, CpuRegs, Image, Mem, OsMonitor, PathNames, Process, Regs68k, Service, Svc};
use std::cell::RefCell;
use std::collections::BTreeMap;

/// Service codes: BDOS functions are 0–63; BIOS functions are 0x80 + n.
pub const BIOS: u8 = 0x80;
/// Placeholder codes before the registers are known (disassembly).
const ANY_BDOS: u8 = 0xfe;
const ANY_BIOS: u8 = 0xff;

/// (function, name, summary, explanation)
const BDOS_FUNCS: &[(u8, &str, &str, &str)] = &[
    (0, "P_TERMCPM", "システムリセット", "プログラムを終え、CCPへ戻る（ウォームブート）。呼び出し元へは戻らない。"),
    (1, "C_READ", "コンソール入力", "コンソールから1文字読み、エコーしてD0に返す。"),
    (2, "C_WRITE", "コンソール出力", "D1の文字をコンソールへ出す。タブは展開し、^Sで一時停止できる。"),
    (3, "A_READ", "リーダー入力", "補助入力（BIOSのREADER）から1文字読む。"),
    (4, "A_WRITE", "パンチ出力", "補助出力（BIOSのPUNCH）へ1文字出す。"),
    (5, "L_WRITE", "リスト出力", "プリンタ（BIOSのLIST）へ1文字出す。"),
    (6, "C_RAWIO", "直接コンソール入出力", "D1=$FFで入力（なければ0）、$FEで状態、それ以外はその文字を加工せずに出す。"),
    (7, "A_GETIOB", "I/Oバイトの取得", "論理装置と物理装置の対応（I/Oバイト）を返す。"),
    (8, "A_SETIOB", "I/Oバイトの設定", "I/Oバイトを変える。"),
    (9, "C_WRITESTR", "文字列出力", "D1が指す、$で終わる文字列をコンソールへ出す。"),
    (10, "C_READSTR", "1行入力", "D1が指すバッファ（先頭に最大長）へ、編集付きで1行読む。"),
    (11, "C_STAT", "コンソール状態", "入力があれば$01、なければ0を返す。"),
    (12, "S_BDOSVER", "版数", "CP/Mの版数を返す（CP/M-68Kは$2022）。"),
    (13, "DRV_ALLRESET", "ディスク系のリセット", "全ドライブを未ログインにし、Aを選び、DMAを既定に戻す。"),
    (14, "DRV_SET", "ドライブ選択", "D1のドライブ（0=A:）を既定にする。"),
    (15, "F_OPEN", "ファイルを開く", "FCBの名前のファイルを探して開く。D0=$FFなら見つからない。"),
    (16, "F_CLOSE", "ファイルを閉じる", "FCBの内容をディレクトリへ書き戻す。"),
    (17, "F_SFIRST", "最初の検索", "FCBの名前（?で任意）に合う最初の項目をDMAへ読む。D0=0〜3は位置、$FFは無し。"),
    (18, "F_SNEXT", "次の検索", "F_SFIRSTの続きの項目を探す。"),
    (19, "F_DELETE", "ファイル削除", "FCBの名前に合うファイルを消す。"),
    (20, "F_READ", "順読み", "現在のレコード（128バイト）をDMAへ読み、次へ進む。D0=1はファイルの終わり。"),
    (21, "F_WRITE", "順書き", "DMAの128バイトを現在のレコードへ書き、次へ進む。"),
    (22, "F_MAKE", "ファイル作成", "ディレクトリに空のファイルを作って開く。"),
    (23, "F_RENAME", "名前の変更", "FCBの前半の名前を、後半（+16）の名前に変える。"),
    (24, "DRV_LOGINVEC", "ログイン済みドライブ", "ログイン済みのドライブを1ドライブ1ビットで返す。"),
    (25, "DRV_GET", "現在のドライブ", "既定のドライブ（0=A:）を返す。"),
    (26, "F_DMAOFF", "DMA番地の設定", "ディスクの読み書きに使う128バイトのバッファの番地をD1にする。"),
    (28, "DRV_SETRO", "書き込み禁止", "既定のドライブを一時的に読み出し専用にする。"),
    (29, "DRV_ROVEC", "読み出し専用ドライブ", "読み出し専用のドライブを1ビットずつ返す。"),
    (30, "F_ATTRIB", "属性の設定", "ファイル名の上位ビット（読み出し専用・システム）を書き換える。"),
    (31, "DRV_DPB", "ディスクパラメータ", "既定のドライブのディスクパラメータブロックをD1の番地へ写す。"),
    (32, "F_USERNUM", "ユーザー番号", "D1=$FFで現在のユーザー番号を返し、それ以外はその番号にする。"),
    (33, "F_READRAND", "ランダム読み", "FCBのランダムレコード番号（r0〜r2）の128バイトをDMAへ読む。"),
    (34, "F_WRITERAND", "ランダム書き", "ランダムレコード番号の位置へDMAの128バイトを書く。"),
    (35, "F_SIZE", "ファイルの大きさ", "ファイルのレコード数をFCBのランダムレコード番号に入れる。"),
    (36, "F_RANDREC", "ランダムレコード番号", "順アクセスの位置をランダムレコード番号に写す。"),
    (37, "DRV_RESET", "ドライブのリセット", "D1のビットのドライブを未ログインに戻す。"),
    (40, "F_WRITEZF", "ランダム書き（0埋め）", "新しく割り当てたブロックを0で埋めてから書く。"),
    (46, "DRV_SPACE", "空き容量", "D1のドライブの空きレコード数をDMAへ返す。"),
    (47, "P_CHAIN", "プログラムの連鎖", "DMAのコマンド行をCCPに渡して実行する。戻らない。"),
    (48, "DRV_FLUSH", "バッファのフラッシュ", "BIOSのFLUSHを呼び、書き込みを確定する。"),
    (50, "S_BIOS", "BIOSの直接呼び出し", "D1が指すブロック（機能番号.W、D1.L、D2.L）でBIOSを呼ぶ。"),
    (59, "P_LOAD", "プログラムの読み込み", "D1が指すロードパラメータブロックの開いたファイルを、TPAへ読み込み再配置する。ベースページの番地を返す。"),
    (61, "S_SETEXC", "例外ベクタの設定", "D1が指すブロックで例外ベクタを置き換え、元の番地を返す。"),
    (62, "S_SUPER", "スーパーバイザ状態へ", "呼び出し元をスーパーバイザ状態で戻す（SRのSビットを立てる）。"),
    (63, "S_TPA", "TPAの取得・設定", "D1が指すブロックへTPAの下限と上限を返す（フラグで設定も）。"),
];

const BIOS_FUNCS: &[(u8, &str, &str, &str)] = &[
    (0, "BIOS_INIT", "コールドブート", "装置を初期化し、TRAP #3のベクタを設定する。"),
    (1, "BIOS_WBOOT", "ウォームブート", "CCPへ戻る。"),
    (2, "BIOS_CONST", "コンソール状態", "入力があれば$FF、なければ0。"),
    (3, "BIOS_CONIN", "コンソール入力", "1文字を待って返す。"),
    (4, "BIOS_CONOUT", "コンソール出力", "D1の文字を出す。"),
    (5, "BIOS_LIST", "リスト出力", "プリンタへ1文字。"),
    (6, "BIOS_PUNCH", "補助出力", "補助出力へ1文字。"),
    (7, "BIOS_READER", "補助入力", "補助入力から1文字。"),
    (8, "BIOS_HOME", "トラック0へ", "現在のトラックを0にする。"),
    (9, "BIOS_SELDSK", "ディスク選択", "D1のドライブを選び、ディスクパラメータヘッダの番地を返す（0は無し）。"),
    (10, "BIOS_SETTRK", "トラック設定", "次の読み書きのトラックをD1にする。"),
    (11, "BIOS_SETSEC", "セクタ設定", "次の読み書きのセクタ（128バイト単位）をD1にする。"),
    (12, "BIOS_SETDMA", "DMA設定", "読み書きのバッファをD1にする。"),
    (13, "BIOS_READ", "セクタ読み", "選んだセクタをDMAへ読む。0で成功。"),
    (14, "BIOS_WRITE", "セクタ書き", "DMAの128バイトを選んだセクタへ書く。"),
    (15, "BIOS_LISTST", "リスト状態", "プリンタが受け付けられれば$FF。"),
    (16, "BIOS_SECTRAN", "セクタ変換", "論理セクタを物理セクタに変換する（スキュー）。"),
    (18, "BIOS_GETMRT", "メモリ領域表", "TPAの領域を示す表の番地を返す。"),
    (19, "BIOS_GETIOB", "I/Oバイトの取得", "I/Oバイトを返す。"),
    (20, "BIOS_SETIOB", "I/Oバイトの設定", "I/Oバイトを変える。"),
    (21, "BIOS_FLUSH", "フラッシュ", "書き込みバッファをディスクへ書き出す。"),
    (22, "BIOS_SETEXC", "例外ベクタ", "D1の番号のベクタをD2に変え、元の値を返す。"),
];

fn lookup(code: u8) -> Option<&'static (u8, &'static str, &'static str, &'static str)> {
    if code >= BIOS {
        BIOS_FUNCS.iter().find(|f| f.0 == code - BIOS)
    } else {
        BDOS_FUNCS.iter().find(|f| f.0 == code)
    }
}

/// Learned from completed calls.
#[derive(Default, Clone)]
struct State {
    drive: u8,
    user: u8,
    dma: Option<u32>,
    /// Open FCBs: address → name.
    files: BTreeMap<u32, String>,
    tpa: Option<(u32, u32)>,
    /// The program loaded by P_LOAD: (name, base page) while it runs.
    program: Option<(String, u32)>,
    loads: u32,
}

#[derive(Default)]
pub struct Cpm68k {
    /// System origin: $15000 (CPM15000) or $400 (CPM400).
    origin: Option<u32>,
    /// The BDOS (from its TRAP #2 entry) and the BIOS (from its TRAP #3
    /// entry's 4 KiB page).
    bdos: u32,
    bios: u32,
    state: RefCell<State>,
}

fn regs(r: &CpuRegs) -> Regs68k {
    match r {
        CpuRegs::M68k(r) => *r,
        CpuRegs::M6809(_) => Regs68k::default(),
    }
}

fn ch(c: u8) -> String {
    match c {
        0x20..=0x7e => format!("'{}'", char::from(c)),
        13 => "CR".into(),
        10 => "LF".into(),
        8 => "BS".into(),
        9 => "TAB".into(),
        _ => format!("${c:02X}"),
    }
}

/// `A:NAME.EXT` of an FCB (drive 0 = the default drive).
pub fn fcb_name(m: &dyn Mem, at: u32, default: u8) -> String {
    let dr = m.peek(at);
    let drive = if dr == 0 { default } else { dr - 1 };
    let part = |from: u32, n: u32| -> String {
        (0..n).map(|i| char::from(m.peek(at + from + i) & 0x7f)).collect::<String>().trim_end().to_string()
    };
    let (name, ext) = (part(1, 8), part(9, 3));
    let drive = char::from(b'A' + (drive & 15));
    if ext.is_empty() { format!("{drive}:{name}") } else { format!("{drive}:{name}.{ext}") }
}

fn dollar_string(m: &dyn Mem, at: u32) -> String {
    let mut s = String::new();
    for i in 0..60 {
        let c = m.peek(at + i);
        if c == b'$' {
            return format!("\"{s}\"");
        }
        match c {
            0x20..=0x7e => s.push(char::from(c)),
            13 => s.push_str("\\r"),
            10 => s.push_str("\\n"),
            _ => s.push_str(&format!("\\x{c:02X}")),
        }
    }
    format!("\"{s}…\"")
}

fn random_record(m: &dyn Mem, fcb: u32) -> u32 {
    u32::from(m.peek(fcb + 33)) | u32::from(m.peek(fcb + 34)) << 8 | u32::from(m.peek(fcb + 35)) << 16
}

impl Cpm68k {
    fn st(&self) -> std::cell::Ref<'_, State> {
        self.state.borrow()
    }
}

impl OsMonitor for Cpm68k {
    fn forget(&mut self) {
        *self.state.borrow_mut() = State::default();
    }
    fn detect(&mut self, m: &dyn Mem) -> bool {
        // "CP/M-68K V1." after the two jumps at the start of the system, and
        // the BDOS installed on TRAP #2.
        let found = [0x15000u32, 0x400].into_iter().find(|&o| {
            m.peek16(o) == 0x4ef9 && (0..8).map(|i| m.peek(o + 13 + i)).eq(b"CP/M-68K".iter().copied())
        });
        let Some(o) = found else { return false };
        let trap2 = m.peek32(0x88);
        if trap2 < o || trap2 >= o + 0x10000 {
            return false;
        }
        self.origin = Some(o);
        self.bdos = trap2;
        self.bios = m.peek32(0x8c) & !0xfff;
        true
    }
    fn info(&self) -> Json {
        obj([
            ("os", "cpm68k".into()),
            ("name", "CP/M-68K".into()),
            ("title", "CP/M-68K 1.3".into()),
            ("current", "実行中のプログラム".into()),
            ("queues", Json::Arr(Vec::new())),
            ("image", "プログラム".into()),
            ("memmap", "regions".into()),
            ("errorLabel", "BDOSのエラー復帰（$FFなど）".into()),
            ("switchLabel", "プログラムの切り替え".into()),
            ("procsTitle", "プログラム".into()),
            ("imagesTitle", "メモリ上のプログラムとシステム".into()),
            ("callsTitle", "BDOS・BIOSの呼び出し".into()),
            ("internalLabel", "BDOS内部の呼び出し（BIOS・BDOS）".into()),
            ("schedTitle", "システム".into()),
            ("breakExample", "F_OPEN".into()),
        ])
    }
    fn summary(&self, _: &dyn Mem) -> Vec<(&'static str, Json)> {
        let s = self.st();
        vec![
            ("drive", char::from(b'A' + (s.drive & 15)).to_string().into()),
            ("user", u32::from(s.user).into()),
            ("dma", s.dma.map_or(Json::Null, Json::from)),
            ("tpa", s.tpa.map_or(Json::Null, |(l, h)| Json::Arr(vec![l.into(), h.into()]))),
            ("program", s.program.as_ref().map_or("CCP".into(), |p| p.0.clone().into())),
            (
                "files",
                Json::Arr(
                    s.files
                        .iter()
                        .map(|(a, n)| obj([("fcb", (*a).into()), ("name", n.clone().into())]))
                        .collect(),
                ),
            ),
        ]
    }
    fn svc_at(&self, read: &dyn Fn(u32) -> u8, pc: u32) -> Option<Svc> {
        if read(pc) != 0x4e {
            return None;
        }
        let code = match read(pc.wrapping_add(1)) {
            0x42 => ANY_BDOS,
            0x43 => ANY_BIOS,
            _ => return None,
        };
        Some(Svc {
            code,
            len: 2,
            no_return: false,
        })
    }
    fn svc_regs(&self, svc: Svc, r: &CpuRegs) -> Svc {
        let f = regs(r).d[0] as u16;
        let code = match svc.code {
            ANY_BDOS if f < 0x80 => f as u8,
            ANY_BIOS if f < 0x7f => BIOS + f as u8,
            c => c,
        };
        Svc {
            code,
            len: svc.len,
            no_return: matches!(code, 0 | 47) || code == BIOS + 1,
        }
    }
    fn internal(&self, code: u8, pc: u32) -> bool {
        code >= BIOS || (self.bdos..self.bios.max(self.bdos)).contains(&pc)
    }
    fn svc_prefix(&self) -> &'static [u8] {
        &[0x4e]
    }
    fn svc_mnemonic(&self) -> &'static str {
        "TRAP"
    }
    fn service_name(&self, code: u8) -> String {
        match code {
            ANY_BDOS => "#2 (BDOS)".into(),
            ANY_BIOS => "#3 (BIOS)".into(),
            c => lookup(c).map_or_else(
                || if c >= BIOS { format!("BIOS_{}", c - BIOS) } else { format!("BDOS_{c}") },
                |f| f.1.to_string(),
            ),
        }
    }
    fn service_code(&self, name: &str) -> Option<u8> {
        let n = name.trim().to_ascii_uppercase();
        if let Some(f) = BDOS_FUNCS.iter().find(|f| f.1 == n) {
            return Some(f.0);
        }
        if let Some(f) = BIOS_FUNCS.iter().find(|f| f.1 == n || f.1.strip_prefix("BIOS_") == Some(&n)) {
            return Some(BIOS + f.0);
        }
        // A function number: "15" (BDOS) or "BIOS 4".
        if let Some(k) = n.strip_prefix("BIOS").map(str::trim) {
            return k.parse::<u8>().ok().filter(|&k| k < 0x7f).map(|k| BIOS + k);
        }
        n.parse::<u8>().ok().filter(|&k| k < BIOS)
    }
    fn explain(&self, code: u8) -> &'static str {
        lookup(code).map_or("", |f| f.3)
    }
    fn error_name(&self, code: u8) -> String {
        match code {
            0xff => "$FF（見つからない・失敗）".into(),
            c => format!("${c:02X}"),
        }
    }
    fn services(&self) -> Vec<Service> {
        BDOS_FUNCS
            .iter()
            .map(|f| (f.0, f))
            .chain(BIOS_FUNCS.iter().map(|f| (BIOS + f.0, f)))
            .map(|(code, f)| Service {
                code,
                name: f.1.into(),
                desc: f.2.into(),
                explain: f.3,
            })
            .collect()
    }
    fn image_calls(&self) -> &'static [u8] {
        &[59]
    }
    fn current(&self, _: &dyn Mem) -> u32 {
        // One program at a time: the CCP (1) or the loaded program (its load count + 1).
        let s = self.st();
        if s.program.is_some() { s.loads + 1 } else { 1 }
    }
    fn pid_of(&self, _: &dyn Mem, key: u32) -> u8 {
        if key == 0 { 0 } else { (key.min(255)) as u8 }
    }
    fn in_system(&self, _: &dyn Mem, _: u32) -> bool {
        false
    }
    fn describe_entry(&self, m: &dyn Mem, _: &PathNames, _: u8, code: u8, r: &CpuRegs) -> String {
        let r = regs(r);
        let d1 = r.d[1];
        let d1b = d1 as u8;
        let drive = self.st().drive;
        let fcb = |a: u32| format!("{} (FCB ${a:06X})", fcb_name(m, a, drive));
        match code {
            0 => {
                self.state.borrow_mut().program = None;
                String::new()
            }
            2 | 4 | 5 => ch(d1b),
            6 => match d1b {
                0xff => "入力".into(),
                0xfe => "状態".into(),
                c => format!("出力 {}", ch(c)),
            },
            8 => format!("${d1b:02X}"),
            9 => dollar_string(m, d1),
            10 => format!("バッファ ${d1:06X}（最大{}文字）", m.peek(d1)),
            14 => format!("{}:", char::from(b'A' + (d1b & 15))),
            15 | 16 | 17 | 19 | 20 | 21 | 22 | 30 | 35 | 36 => fcb(d1),
            23 => format!("{} → {}", fcb_name(m, d1, drive), fcb_name(m, d1 + 16, drive)),
            33 | 34 | 40 => format!("{} レコード{}", fcb(d1), random_record(m, d1)),
            26 => format!("${d1:06X}"),
            32 => {
                if d1b == 0xff {
                    "取得".into()
                } else {
                    format!("ユーザー{d1b}")
                }
            }
            37 => format!("ドライブ ${:04X}", d1 as u16),
            46 => format!("{}:", char::from(b'A' + (d1b & 15))),
            50 => {
                let f = m.peek16(d1);
                format!("{} D1=${:X} D2=${:X}", self.service_name(BIOS + f as u8), m.peek32(d1 + 2), m.peek32(d1 + 6))
            }
            59 => {
                let lo = m.peek32(d1 + 4);
                let hi = m.peek32(d1 + 8);
                format!("{} TPA ${lo:06X}–${hi:06X}", fcb_name(m, m.peek32(d1), drive))
            }
            61 => format!("ベクタ{} → ${:06X}", m.peek16(d1), m.peek32(d1 + 2)),
            63 => if m.peek16(d1) & 1 != 0 { "設定" } else { "取得" }.into(),
            c if c == BIOS + 1 => {
                self.state.borrow_mut().program = None;
                String::new()
            }
            c if c == BIOS + 4 || c == BIOS + 5 || c == BIOS + 6 => ch(d1b),
            c if c == BIOS + 9 => format!("{}:", char::from(b'A' + (d1b & 15))),
            c if c == BIOS + 10 => format!("トラック{}", d1 as u16),
            c if c == BIOS + 11 => format!("セクタ{}", d1 as u16),
            c if c == BIOS + 12 => format!("${d1:06X}"),
            c if c == BIOS + 16 => format!("{}", d1 as u16),
            c if c == BIOS + 22 => format!("ベクタ{} → ${:06X}", d1 as u16, r.d[2]),
            _ => String::new(),
        }
    }
    fn describe_exit(&self, m: &dyn Mem, call: &Call, r: &CpuRegs) -> String {
        let out = regs(r);
        let inp = regs(&call.inp);
        let d0 = out.d[0];
        let d1 = inp.d[1];
        match call.code {
            1 | 3 | 6 => {
                if call.code == 6 && inp.d[1] as u8 >= 0xfe {
                    format!("${:02X}", d0 as u8)
                } else {
                    ch(d0 as u8)
                }
            }
            7 | 11 | 24 | 29 => format!("${:04X}", d0 as u16),
            12 => format!("${:04X}", d0 as u16),
            25 => format!("{}:", char::from(b'A' + (d0 as u8 & 15))),
            10 => {
                let n = m.peek(d1 + 1);
                let text: String = (0..u32::from(n)).map(|i| char::from(m.peek(d1 + 2 + i))).collect();
                format!("\"{text}\"")
            }
            15 | 16 | 17 | 18 | 19 | 22 | 23 | 30 => {
                if d0 as u8 == 0xff {
                    "$FF（見つからない）".into()
                } else if call.code == 17 || call.code == 18 {
                    let dma = self.st().dma.unwrap_or(0x80);
                    let at = dma + 32 * (d0 & 3);
                    format!("{}（項目{}）", fcb_name(m, at, self.st().drive), d0 & 3)
                } else {
                    "OK".into()
                }
            }
            20 | 21 | 33 | 34 | 40 => match d0 as u8 {
                0 => "OK".into(),
                1 => "ファイルの終わり／未書き込み".into(),
                2 => "ディスクが一杯".into(),
                c => format!("${c:02X}"),
            },
            35 => format!("{}レコード", random_record(m, d1)),
            32 => format!("ユーザー{}", d0 as u8),
            59 => {
                if d0 as u16 == 0 {
                    let bp = m.peek32(d1 + 12);
                    format!(
                        "ベースページ ${bp:06X} テキスト ${:06X}+{} データ ${:06X}+{} BSS ${:06X}+{}",
                        m.peek32(bp + 8),
                        m.peek32(bp + 12),
                        m.peek32(bp + 16),
                        m.peek32(bp + 20),
                        m.peek32(bp + 24),
                        m.peek32(bp + 28)
                    )
                } else {
                    format!(
                        "エラー{}（{}）",
                        d0 as u16,
                        match d0 as u16 {
                            1 => "TPAに入らない",
                            2 => "読み込みエラー",
                            3 => "再配置の誤り",
                            _ => "不明",
                        }
                    )
                }
            }
            63 => {
                let (lo, hi) = (m.peek32(d1 + 2), m.peek32(d1 + 6));
                self.state.borrow_mut().tpa = Some((lo, hi));
                format!("TPA ${lo:06X}–${hi:06X}")
            }
            61 => format!("元 ${d0:06X}"),
            c if c == BIOS + 3 => ch(d0 as u8),
            c if c == BIOS + 2 || c == BIOS + 15 => format!("${:02X}", d0 as u8),
            c if c == BIOS + 9 => {
                if d0 == 0 { "無し".into() } else { format!("DPH ${d0:06X}") }
            }
            c if c == BIOS + 13 || c == BIOS + 14 => {
                if d0 == 0 { "OK".into() } else { format!("エラー{d0}") }
            }
            c if c == BIOS + 16 => format!("{}", d0 as u16),
            c if c == BIOS + 18 || c == BIOS + 22 => format!("${d0:06X}"),
            _ => String::new(),
        }
    }
    fn error_of(&self, r: &CpuRegs) -> Option<u8> {
        // The function is not known here: only the common failure value.
        (regs(r).d[0] as u8 == 0xff).then_some(0xff)
    }
    fn error_for(&self, call: &Call, r: &CpuRegs) -> Option<u8> {
        let d0 = regs(r).d[0];
        match call.code {
            15 | 16 | 17 | 19 | 22 | 23 | 30 => (d0 as u8 == 0xff).then_some(0xff),
            33 | 34 | 40 => (d0 as u8 > 1).then_some(d0 as u8),
            21 => (d0 as u8 != 0).then_some(d0 as u8),
            59 => (d0 as u16 != 0).then_some(d0 as u8),
            c if c == BIOS + 13 || c == BIOS + 14 => (d0 != 0).then_some(d0 as u8),
            _ => None,
        }
    }
    fn learn(&self, call: &Call, out: &CpuRegs, _: u8, _: &mut PathNames) {
        let inp = regs(&call.inp);
        let d1 = inp.d[1];
        let d0 = regs(out).d[0];
        let mut s = self.state.borrow_mut();
        match call.code {
            13 => {
                s.drive = 0;
                s.dma = Some(0x80);
            }
            14 => s.drive = d1 as u8 & 15,
            26 => s.dma = Some(d1),
            32 if d1 as u8 != 0xff => s.user = d1 as u8 & 15,
            15 | 22 if d0 as u8 != 0xff => {
                let name = call.args.split(" (FCB").next().unwrap_or("").to_string();
                s.files.insert(d1, name);
            }
            16 => {
                s.files.remove(&d1);
            }
            59 if d0 as u16 == 0 => {
                s.loads += 1;
                let name = call.args.split(" TPA").next().unwrap_or("").to_string();
                // The base page address is in the parameter block; read by images().
                s.program = Some((name, d1));
            }
            _ => {}
        }
    }
    fn processes(&self, m: &dyn Mem) -> Vec<Process> {
        let s = self.st();
        let mut v = vec![Process {
            id: 1,
            parent: 0,
            key: 1,
            name: "CCP".into(),
            queue: if s.program.is_none() { "running" } else { "-" },
            fields: vec![("name", "CCP".into()), ("drive", format!("{}:", char::from(b'A' + s.drive)).into()), ("user", u32::from(s.user).into())],
        }];
        if let Some((name, lpb)) = &s.program {
            let bp = m.peek32(lpb + 12);
            v.push(Process {
                id: (s.loads + 1).min(255) as u8,
                parent: 1,
                key: s.loads + 1,
                name: name.clone(),
                queue: "running",
                fields: vec![
                    ("name", name.clone().into()),
                    ("basepage", format!("${bp:06X}").into()),
                    ("text", format!("${:06X}+{}", m.peek32(bp + 8), m.peek32(bp + 12)).into()),
                    ("data", format!("${:06X}+{}", m.peek32(bp + 16), m.peek32(bp + 20)).into()),
                    ("bss", format!("${:06X}+{}", m.peek32(bp + 24), m.peek32(bp + 28)).into()),
                    ("stack", format!("${:06X}", m.peek32(lpb + 16)).into()),
                ],
            });
        }
        v
    }
    fn images(&self, m: &dyn Mem) -> Vec<Image> {
        let Some(o) = self.origin else { return Vec::new() };
        let s = self.st();
        let bios = self.bios;
        let mut v = vec![
            Image {
                name: "CCP".into(),
                fields: vec![("kind", "CCP".into()), ("addr", format!("${o:06X}").into())],
                chunks: vec![(o as usize, self.bdos.saturating_sub(o) as usize, 0)],
            },
            Image {
                name: "BDOS".into(),
                fields: vec![("kind", "BDOS".into()), ("addr", format!("${:06X}", self.bdos).into())],
                chunks: vec![(self.bdos as usize, bios.saturating_sub(self.bdos) as usize, 0)],
            },
        ];
        if bios > o {
            // The BIOS: from its TRAP #3 entry's page to the TPA (or 4 KiB).
            let start = bios;
            let end = s.tpa.map_or(start + 0x1000, |t| t.0.max(start));
            v.push(Image {
                name: "BIOS".into(),
                fields: vec![("kind", "BIOS".into()), ("addr", format!("${start:06X}").into())],
                chunks: vec![(start as usize, (end - start) as usize, 0)],
            });
        }
        if let Some((name, lpb)) = &s.program {
            let bp = m.peek32(lpb + 12);
            let text = m.peek32(bp + 8);
            let total = m.peek32(bp + 24) + m.peek32(bp + 28) - text;
            let stem = name.split(':').nth(1).unwrap_or(name).split('.').next().unwrap_or("").to_string();
            v.push(Image {
                name: stem,
                fields: vec![("kind", "プログラム".into()), ("addr", format!("${text:06X}").into()), ("basepage", format!("${bp:06X}").into())],
                chunks: vec![(text as usize, total.min(0x100_0000) as usize, 0)],
            });
        }
        v
    }
    fn memory_map(&self, m: &dyn Mem) -> Json {
        let s = self.st();
        let mut regions: Vec<Json> = Vec::new();
        let mut add = |name: &str, kind: &str, start: u32, end: u32| {
            if end > start {
                regions.push(obj([("name", name.into()), ("kind", kind.into()), ("start", start.into()), ("end", end.into())]));
            }
        };
        add("例外ベクタ", "sys", 0, 0x400);
        if let Some((lo, hi)) = s.tpa {
            add("TPA", "free", lo, hi);
        }
        for img in self.images(m) {
            let (start, len, _) = img.chunks[0];
            let kind = if matches!(img.name.as_str(), "CCP" | "BDOS" | "BIOS") { "sys" } else { "prog" };
            add(&img.name, kind, start as u32, (start + len) as u32);
        }
        if let Some((_, lpb)) = &s.program {
            let bp = m.peek32(lpb + 12);
            add("ベースページ", "prog", bp, bp + 0x100);
        }
        obj([("regions", Json::Arr(regions)), ("ram", m.ram().len().into())])
    }
    fn read_space(&self, _: &dyn Mem, _: &str, _: u32) -> Option<u8> {
        None
    }
}
