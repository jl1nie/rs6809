//! Debug Adapter Protocol server: editors such as VS Code drive the same ICE
//! as the browser UI. Each TCP connection is one session; requests are
//! handled on the emulator thread like HTTP requests, so both views always
//! agree. Messages use `Content-Length` framing and JSON bodies.
use super::debuginfo::{Place, encode};
use super::frames::VarRef;
use super::json::{Json, base64, obj, unbase64};
use super::osmon::Regs;
use super::symbols::Target;
use super::{BpSpec, Goal, Ice, Inbox, Stop};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Sender};
use std::thread;

const THREAD: i64 = 1;
const REGISTERS: i64 = 1;
const STACK: i64 = 2;
const PROCESS: i64 = 3;
/// Variable tree nodes are numbered from here.
const DYNAMIC: i64 = 1000;

struct Session {
    id: u32,
    out: Sender<String>,
    seq: i64,
    /// Last verified state reported per breakpoint id.
    reported: HashMap<u32, bool>,
}

/// Connected sessions and events waiting to be sent after a response.
#[derive(Default)]
pub struct Hub {
    sessions: Vec<Session>,
    pending: Vec<(Option<u32>, Json)>,
    output: Vec<u8>,
}

impl Hub {
    fn event(&mut self, to: Option<u32>, name: &str, body: Json) {
        if !self.sessions.is_empty() {
            self.pending.push((
                to,
                obj([
                    ("type", "event".into()),
                    ("event", name.into()),
                    ("body", body),
                ]),
            ));
        }
    }
    pub fn stopped(&mut self, s: &Stop, description: &str, hits: Vec<u32>) {
        let reason = match s {
            Stop::Pause => "pause",
            Stop::Step => "step",
            Stop::Breakpoint(_) => "breakpoint",
            Stop::Watch { .. } => "data breakpoint",
            Stop::SysCall { .. } | Stop::SysReturn(_) | Stop::Switch { .. } => {
                "function breakpoint"
            }
            Stop::Error(_) => "exception",
        };
        self.flush_output_into_pending();
        self.event(
            None,
            "stopped",
            obj([
                ("reason", reason.into()),
                ("description", description.into()),
                ("text", description.into()),
                ("threadId", THREAD.into()),
                ("allThreadsStopped", true.into()),
                (
                    "hitBreakpointIds",
                    if hits.is_empty() {
                        Json::Null
                    } else {
                        hits.into()
                    },
                ),
            ]),
        );
    }
    pub fn continued(&mut self) {
        self.event(
            None,
            "continued",
            obj([
                ("threadId", THREAD.into()),
                ("allThreadsContinued", true.into()),
            ]),
        );
    }
    pub fn output(&mut self, bytes: &[u8]) {
        if !self.sessions.is_empty() {
            self.output.extend_from_slice(bytes);
        }
    }
    fn flush_output_into_pending(&mut self) {
        if self.output.is_empty() {
            return;
        }
        let text: String = String::from_utf8_lossy(&std::mem::take(&mut self.output))
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        self.event(
            None,
            "output",
            obj([("category", "stdout".into()), ("output", text.into())]),
        );
    }
    fn send(&mut self, to: u32, mut msg: Json) {
        if let Some(s) = self.sessions.iter_mut().find(|s| s.id == to) {
            s.seq += 1;
            if let Json::Obj(m) = &mut msg {
                m.insert("seq".into(), s.seq.into());
            }
            let _ = s.out.send(msg.to_string());
        }
    }
    pub fn flush(&mut self) {
        self.flush_output_into_pending();
        for (to, msg) in std::mem::take(&mut self.pending) {
            let ids: Vec<u32> = self
                .sessions
                .iter()
                .map(|s| s.id)
                .filter(|id| to.is_none_or(|t| t == *id))
                .collect();
            for id in ids {
                self.send(id, msg.clone());
            }
        }
    }
    pub fn active(&self) -> bool {
        !self.sessions.is_empty()
    }
}

fn hex_ref(a: u16) -> Json {
    format!("0x{a:04X}").into()
}
fn parse_ref(v: &Json) -> Option<u16> {
    let s = v.str()?.trim();
    let h = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .or_else(|| s.strip_prefix('$'))?;
    u32::from_str_radix(h, 16).ok().map(|v| v as u16)
}
fn parse_value(s: &str) -> Option<u16> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix('$').or_else(|| s.strip_prefix("0x")) {
        return u32::from_str_radix(h, 16).ok().map(|v| v as u16);
    }
    s.parse::<i32>().ok().map(|v| v as u16)
}
fn cc_flags(cc: u8) -> String {
    "EFHINZVC"
        .chars()
        .enumerate()
        .map(|(i, c)| if cc & (0x80 >> i) != 0 { c } else { '-' })
        .collect()
}
fn source_ref(table: usize, src: usize) -> i64 {
    (table as i64) * 65536 + src as i64 + 1
}

impl Ice {
    /// Handle one DAP message from session `id`; replies go to `out`.
    pub fn dap_message(&mut self, id: u32, msg: &Json, out: Sender<String>) {
        if !self.dap.sessions.iter().any(|s| s.id == id) {
            self.dap.sessions.push(Session {
                id,
                out,
                seq: 0,
                reported: HashMap::new(),
            });
        }
        if msg.get("type").str() != Some("request") {
            return;
        }
        let command = msg.get("command").str().unwrap_or("").to_string();
        let args = msg.get("arguments");
        let result = self.dap_request(id, &command, args);
        let (success, body, message) = match result {
            Ok(body) => (true, body, Json::Null),
            Err(e) => (false, Json::Null, Json::from(e)),
        };
        let response = obj([
            ("type", "response".into()),
            ("request_seq", msg.get("seq").int().unwrap_or(0).into()),
            ("success", success.into()),
            ("command", command.as_str().into()),
            ("message", message),
            ("body", body),
        ]);
        // Responses go out before the events the request caused.
        self.dap.send(id, response);
        if command == "initialize" {
            self.dap.event(Some(id), "initialized", obj([]));
        }
        if command == "configurationDone" && !self.running {
            let desc = self
                .stop
                .as_ref()
                .map(|s| s.describe(&self.tracer, &*self.os))
                .unwrap_or_default();
            self.dap.event(
                Some(id),
                "stopped",
                obj([
                    ("reason", "pause".into()),
                    ("description", desc.into()),
                    ("threadId", THREAD.into()),
                    ("allThreadsStopped", true.into()),
                ]),
            );
        }
        self.dap_sync();
    }

    pub fn dap_closed(&mut self, id: u32) {
        self.dap.sessions.retain(|s| s.id != id);
        self.remove_sym_bps(|b| b.owner != id);
    }

    /// Flush events and report breakpoints whose resolution changed.
    pub fn dap_sync(&mut self) {
        if !self.dap.active() {
            return;
        }
        let mut changes = Vec::new();
        for s in &mut self.dap.sessions {
            for b in self.sym_bps.iter().filter(|b| b.owner == s.id) {
                let verified = !b.targets.is_empty();
                if s.reported.insert(b.id, verified) != Some(verified) {
                    changes.push((s.id, b.id, verified, b.line));
                }
            }
        }
        for (sid, bid, verified, line) in changes {
            let bp = obj([
                ("id", bid.into()),
                ("verified", verified.into()),
                ("line", line.into()),
            ]);
            self.dap.event(
                Some(sid),
                "breakpoint",
                obj([("reason", "changed".into()), ("breakpoint", bp)]),
            );
        }
        self.dap.flush();
    }

    /// Breakpoint ids that stop at this PC.
    pub(super) fn hit_ids(&self, pc: u16) -> Vec<u32> {
        let phys = self.machine.translate(pc);
        self.sym_bps
            .iter()
            .filter(|b| {
                b.targets.iter().any(|t| match *t {
                    Target::Logical(a) => a == pc,
                    Target::Phys(p) => Some(p) == phys,
                })
            })
            .map(|b| b.id)
            .collect()
    }

    fn dap_source(&self, table: usize, src: usize) -> Json {
        let s = &self.symbols.tables[table].sources[src];
        let name = s.name.rsplit('/').next().unwrap_or(&s.name).to_string();
        match &s.path {
            Some(p) => {
                let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
                obj([
                    ("name", name.into()),
                    ("path", abs.to_string_lossy().into_owned().into()),
                ])
            }
            None => obj([
                ("name", name.into()),
                ("sourceReference", source_ref(table, src).into()),
                ("origin", s.name.as_str().into()),
            ]),
        }
    }

    /// Source location of an address, as (source, line).
    fn dap_location(&self, addr: u16) -> Option<(Json, u32)> {
        let (ti, l) = self.symbols.line(&self.machine, addr)?;
        Some((self.dap_source(ti, l.src), l.line))
    }

    fn dap_request(&mut self, id: u32, command: &str, args: &Json) -> Result<Json, String> {
        match command {
            "initialize" => Ok(obj([
                ("supportsConfigurationDoneRequest", true.into()),
                ("supportsFunctionBreakpoints", true.into()),
                ("supportsInstructionBreakpoints", true.into()),
                ("supportsDisassembleRequest", true.into()),
                ("supportsReadMemoryRequest", true.into()),
                ("supportsWriteMemoryRequest", true.into()),
                ("supportsSteppingGranularity", true.into()),
                ("supportsSetVariable", true.into()),
                ("supportsEvaluateForHovers", true.into()),
                ("supportsLoadedSourcesRequest", true.into()),
                (
                    "exceptionBreakpointFilters",
                    {
                        // Labels come from the OS layer.
                        let info = self.os.info();
                        let label = |k: &str, d: &str| info.get(k).str().unwrap_or(d).to_string();
                        Json::Arr(vec![
                            obj([
                                ("filter", "oserror".into()),
                                ("label", label("errorLabel", "エラー復帰").into()),
                                ("default", false.into()),
                            ]),
                            obj([
                                ("filter", "osswitch".into()),
                                ("label", label("switchLabel", "プロセス切り替え").into()),
                                ("default", false.into()),
                            ]),
                        ])
                    },
                ),
            ])),
            "attach" | "launch" => {
                let symbols: Vec<String> = match args.get("symbols") {
                    Json::Str(s) => vec![s.clone()],
                    v => v
                        .arr()
                        .iter()
                        .filter_map(|x| x.str().map(str::to_string))
                        .collect(),
                };
                for d in args.get("sourceDirs").arr().iter().filter_map(Json::str) {
                    self.symbols.add_source_dir(d.into());
                }
                for s in &symbols {
                    self.load_symbols(s)?;
                }
                if args.get("stopOnEntry").bool() == Some(true) && self.running {
                    self.halt(Stop::Pause);
                }
                Ok(Json::Null)
            }
            "configurationDone" | "cancel" => Ok(Json::Null),
            "disconnect" | "terminate" => {
                self.remove_sym_bps(|b| b.owner != id);
                Ok(Json::Null)
            }
            "setBreakpoints" => {
                let source = args.get("source");
                let file = source
                    .get("path")
                    .str()
                    .or_else(|| source.get("name").str())
                    .ok_or("source")?
                    .to_string();
                let lines: Vec<u32> = if args.get("breakpoints").arr().is_empty() {
                    args.get("lines")
                        .arr()
                        .iter()
                        .filter_map(Json::int)
                        .map(|l| l as u32)
                        .collect()
                } else {
                    args.get("breakpoints")
                        .arr()
                        .iter()
                        .filter_map(|b| b.get("line").int())
                        .map(|l| l as u32)
                        .collect()
                };
                self.remove_sym_bps(|b| {
                    !(b.owner == id
                        && matches!(&b.spec, BpSpec::Line { file: f, .. } if *f == file))
                });
                let mut out = Vec::new();
                for line in lines {
                    let bid = self.add_sym_bp(
                        id,
                        BpSpec::Line {
                            file: file.clone(),
                            line,
                        },
                    );
                    out.push(self.dap_bp(bid, Some(line)));
                }
                Ok(obj([("breakpoints", Json::Arr(out))]))
            }
            "setFunctionBreakpoints" => {
                self.remove_sym_bps(|b| !(b.owner == id && matches!(b.spec, BpSpec::Symbol(_))));
                let mut out = Vec::new();
                for name in args
                    .get("breakpoints")
                    .arr()
                    .iter()
                    .filter_map(|b| b.get("name").str())
                {
                    let bid = self.add_sym_bp(id, BpSpec::Symbol(name.trim().to_string()));
                    out.push(self.dap_bp(bid, None));
                }
                Ok(obj([("breakpoints", Json::Arr(out))]))
            }
            "setInstructionBreakpoints" => {
                self.remove_sym_bps(|b| !(b.owner == id && matches!(b.spec, BpSpec::Addr(_))));
                let mut out = Vec::new();
                for b in args.get("breakpoints").arr() {
                    let Some(a) = parse_ref(b.get("instructionReference")) else {
                        continue;
                    };
                    let a = a.wrapping_add(b.get("offset").int().unwrap_or(0) as u16);
                    let bid = self.add_sym_bp(id, BpSpec::Addr(a));
                    out.push(self.dap_bp(bid, None));
                }
                Ok(obj([("breakpoints", Json::Arr(out))]))
            }
            "setExceptionBreakpoints" => {
                let filters: Vec<&str> = args
                    .get("filters")
                    .arr()
                    .iter()
                    .filter_map(Json::str)
                    .collect();
                self.break_errors = filters.contains(&"oserror");
                self.break_switch = filters.contains(&"osswitch");
                Ok(obj([]))
            }
            "threads" => {
                let mut name = "6809".to_string();
                if let Some(os) = self.os() {
                    let cur = os.current(&self.machine);
                    let osname = os.info().get("name").str().unwrap_or("OS").to_string();
                    if let Some(p) = os.processes(&self.machine).into_iter().find(|p| p.key == cur) {
                        name = format!("6809 ({osname} プロセス{} {})", p.id, p.name);
                    }
                }
                Ok(obj([(
                    "threads",
                    Json::Arr(vec![obj([("id", THREAD.into()), ("name", name.into())])]),
                )]))
            }
            "stackTrace" => {
                self.refresh_symbols();
                let mut frames = Vec::new();
                let pcs: Vec<u16> = self.frames(32).iter().map(|f| f.pc).collect();
                for (i, &pc) in pcs.iter().enumerate() {
                    // A return address belongs to the line of its call.
                    let at = if i == 0 { pc } else { pc.wrapping_sub(1) };
                    let name = self
                        .symbols
                        .label(&self.machine, at)
                        .unwrap_or_else(|| format!("${pc:04X}"));
                    let (source, line) = self.dap_location(at).unwrap_or((Json::Null, 0));
                    frames.push(obj([
                        ("id", i.into()),
                        ("name", name.into()),
                        ("source", source),
                        ("line", line.into()),
                        ("column", if line > 0 { 1 } else { 0 }.into()),
                        ("instructionPointerReference", hex_ref(pc)),
                        (
                            "presentationHint",
                            if line > 0 {
                                Json::Null
                            } else {
                                "subtle".into()
                            },
                        ),
                    ]));
                }
                let start = args.get("startFrame").int().unwrap_or(0).max(0) as usize;
                let levels = args
                    .get("levels")
                    .int()
                    .filter(|&n| n > 0)
                    .map_or(usize::MAX, |n| n as usize);
                let total = frames.len();
                let frames: Vec<Json> = frames.into_iter().skip(start).take(levels).collect();
                Ok(obj([
                    ("stackFrames", Json::Arr(frames)),
                    ("totalFrames", total.into()),
                ]))
            }
            "scopes" => {
                let frame = args.get("frameId").int().unwrap_or(0).max(0) as usize;
                let mut scopes = Vec::new();
                let frames = self.frames(frame + 1);
                if let Some(f) = frames.get(frame)
                    && let Some((ti, _)) = f.func
                {
                    let r = self.var_ref(VarRef::Locals(frame));
                    scopes.push(obj([
                        ("name", "ローカル".into()),
                        ("variablesReference", r.into()),
                        ("expensive", false.into()),
                        ("presentationHint", "locals".into()),
                    ]));
                    let g = self.var_ref(VarRef::Globals(ti));
                    scopes.push(obj([
                        ("name", "大域".into()),
                        ("variablesReference", g.into()),
                        ("expensive", false.into()),
                    ]));
                }
                let Json::Obj(mut fixed) = self.dap_fixed_scopes() else {
                    unreachable!()
                };
                let mut list = scopes;
                if let Some(Json::Arr(v)) = fixed.remove("scopes") {
                    list.extend(v);
                }
                Ok(obj([("scopes", Json::Arr(list))]))
            }
            "variables" => {
                let r = args.get("variablesReference").int().unwrap_or(0);
                let vars = if r >= DYNAMIC {
                    self.dap_var_children(r)
                } else {
                    self.dap_variables(r)
                };
                Ok(obj([("variables", Json::Arr(vars))]))
            }
            "setVariable"
                if args
                    .get("variablesReference")
                    .int()
                    .is_some_and(|r| r >= DYNAMIC) =>
            {
                let r = args.get("variablesReference").int().unwrap_or(0);
                let name = args.get("name").str().unwrap_or("").to_string();
                let value = args.get("value").str().unwrap_or("").to_string();
                let child = self
                    .dap_var_nodes(r)
                    .into_iter()
                    .find(|(n, ..)| *n == name)
                    .ok_or("変数")?;
                let (_, table, ty, place) = child;
                let place = place.map_err(|e| e.to_string())?;
                let info = self.symbols.tables[table].debug.as_ref().ok_or("変数")?;
                let bytes = encode(info, &ty, &value)
                    .ok_or("整数・文字・ポインタの値を指定してください")?;
                match place {
                    Place::Mem(a) => {
                        for (i, b) in bytes.iter().enumerate() {
                            self.machine.poke(a.wrapping_add(i as u16), *b);
                        }
                    }
                    Place::Value(_) => {
                        // A register variable of the current frame.
                        let v = bytes.iter().fold(0u16, |acc, b| acc << 8 | u16::from(*b));
                        self.cpu.u = v;
                    }
                }
                let shown = self
                    .with_ctx(table, |c| {
                        c.format(
                            &ty,
                            match place {
                                Place::Value(_) => Place::Value(self.cpu.u),
                                p => p,
                            },
                        )
                    })
                    .unwrap_or_default();
                Ok(obj([("value", shown.into())]))
            }
            "setVariable" => {
                if args.get("variablesReference").int() != Some(REGISTERS) {
                    return Err("レジスタのみ変更できます".into());
                }
                let name = args.get("name").str().unwrap_or("").to_ascii_lowercase();
                let v = args.get("value").str().and_then(parse_value).ok_or("値")?;
                let c = &mut self.cpu;
                match name.as_str() {
                    "a" => c.a = v as u8,
                    "b" => c.b = v as u8,
                    "d" => c.set_d(v),
                    "dp" => c.dp = v as u8,
                    "cc" => c.cc = v as u8,
                    "x" => c.x = v,
                    "y" => c.y = v,
                    "u" => c.u = v,
                    "s" => c.s = v,
                    "pc" => c.pc = v,
                    _ => return Err("レジスタ名".into()),
                }
                let shown = self.dap_variables(REGISTERS).into_iter().find(|x| {
                    x.get("name").str().map(str::to_ascii_lowercase) == Some(name.clone())
                });
                Ok(obj([(
                    "value",
                    shown.map_or(Json::Null, |x| x.get("value").clone()),
                )]))
            }
            "continue" => {
                if !self.running {
                    self.resume(Goal::None);
                }
                Ok(obj([("allThreadsContinued", true.into())]))
            }
            "next" | "stepIn" => {
                let instruction = args.get("granularity").str() == Some("instruction");
                match (command, instruction) {
                    ("next", true) => self.step_over(),
                    ("stepIn", true) => self.single_step(),
                    (_, false) => self.step_line(command == "stepIn"),
                    _ => unreachable!(),
                }
                Ok(Json::Null)
            }
            "stepOut" => {
                let goal = Goal::Out {
                    s: self.cpu.s,
                    proc_: self.current_proc(),
                };
                self.resume(goal);
                Ok(Json::Null)
            }
            "pause" => {
                if self.running {
                    self.halt(Stop::Pause);
                } else {
                    self.dap.stopped(&Stop::Pause, "一時停止", Vec::new());
                }
                Ok(Json::Null)
            }
            "evaluate" => {
                let frame = args.get("frameId").int().unwrap_or(0).max(0) as usize;
                let expr = args
                    .get("expression")
                    .str()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                self.refresh_symbols();
                // C variables and expressions first (`n`, `s.x`, `p->next`, `arr[1]`).
                if !expr.starts_with('>')
                    && let Some((table, ty, place)) = self.eval_var(frame, &expr)
                {
                    let value = self
                        .with_ctx(table, |c| c.format(&ty, place))
                        .unwrap_or_default();
                    let expandable = self.with_ctx(table, |c| c.expandable(&ty)).unwrap_or(false);
                    let r = if expandable {
                        self.var_ref(VarRef::Node {
                            table,
                            ty: ty.clone(),
                            place,
                        })
                    } else {
                        0
                    };
                    let tname = self.symbols.tables[table]
                        .debug
                        .as_ref()
                        .map(|d| d.type_name(&ty));
                    let mem = match place {
                        Place::Mem(a) => hex_ref(a),
                        Place::Value(_) => Json::Null,
                    };
                    return Ok(obj([
                        ("result", value.into()),
                        ("type", tname.into()),
                        ("variablesReference", r.into()),
                        ("memoryReference", mem),
                    ]));
                }
                self.dap_evaluate(&expr, args.get("context").str().unwrap_or(""))
            }
            "disassemble" => {
                self.refresh_symbols();
                let base = parse_ref(args.get("memoryReference")).ok_or("memoryReference")?;
                let mut a = base.wrapping_add(args.get("offset").int().unwrap_or(0) as u16);
                let skip = args.get("instructionOffset").int().unwrap_or(0);
                let count = args
                    .get("instructionCount")
                    .int()
                    .unwrap_or(0)
                    .clamp(0, 4096) as usize;
                let m = &self.machine;
                if skip < 0 {
                    a = self.resync(a, (-skip).min(4096) as u16);
                } else {
                    for _ in 0..skip {
                        a = a.wrapping_add(self.dis(a).len.max(1));
                    }
                }
                let mut out = Vec::new();
                let mut last_line = None;
                for _ in 0..count {
                    let l = self.dis(a);
                    let bytes: Vec<String> = (0..l.len)
                        .map(|i| format!("{:02X}", m.peek(a.wrapping_add(i))))
                        .collect();
                    let mut text = l.text.clone();
                    if let Some(t) = l.target.filter(|_| !l.svc)
                        && let Some(name) = self.symbols.label(m, t)
                    {
                        text.push_str(&format!("  <{name}>"));
                    }
                    let loc = self.dap_location(a);
                    let mut fields = vec![
                        ("address", hex_ref(a)),
                        ("instructionBytes", bytes.join(" ").into()),
                        ("instruction", text.into()),
                        (
                            "symbol",
                            self.symbols.exact(m, a).map(str::to_string).into(),
                        ),
                    ];
                    if let Some((src, line)) = loc
                        && last_line != Some(line)
                    {
                        fields.push(("location", src));
                        fields.push(("line", line.into()));
                        last_line = Some(line);
                    }
                    out.push(Json::Obj(
                        fields
                            .into_iter()
                            .filter(|(_, v)| *v != Json::Null)
                            .map(|(k, v)| (k.to_string(), v))
                            .collect(),
                    ));
                    a = a.wrapping_add(l.len.max(1));
                }
                Ok(obj([("instructions", Json::Arr(out))]))
            }
            "readMemory" => {
                let base = parse_ref(args.get("memoryReference")).ok_or("memoryReference")?;
                let a = base.wrapping_add(args.get("offset").int().unwrap_or(0) as u16);
                let n = args.get("count").int().unwrap_or(0).clamp(0, 65536) as usize;
                let bytes: Vec<u8> = (0..n)
                    .map(|i| self.machine.peek(a.wrapping_add(i as u16)))
                    .collect();
                Ok(obj([
                    ("address", hex_ref(a)),
                    ("data", base64(&bytes).into()),
                ]))
            }
            "writeMemory" => {
                let base = parse_ref(args.get("memoryReference")).ok_or("memoryReference")?;
                let a = base.wrapping_add(args.get("offset").int().unwrap_or(0) as u16);
                let data = unbase64(args.get("data").str().unwrap_or("")).ok_or("data")?;
                for (i, b) in data.iter().enumerate() {
                    self.machine.poke(a.wrapping_add(i as u16), *b);
                }
                Ok(obj([("bytesWritten", data.len().into())]))
            }
            "source" => {
                let r = args
                    .get("sourceReference")
                    .int()
                    .or_else(|| args.get("source").get("sourceReference").int())
                    .unwrap_or(0)
                    - 1;
                let (ti, si) = ((r / 65536) as usize, (r % 65536) as usize);
                let src = self
                    .symbols
                    .tables
                    .get(ti)
                    .and_then(|t| t.sources.get(si))
                    .ok_or("source")?;
                let last = src.text.keys().last().copied().unwrap_or(0);
                let content: Vec<&str> = (1..=last)
                    .map(|n| src.text.get(&n).map_or("", String::as_str))
                    .collect();
                Ok(obj([("content", content.join("\n").into())]))
            }
            "loadedSources" => {
                let mut out = Vec::new();
                for (ti, t) in self.symbols.tables.iter().enumerate() {
                    for si in 0..t.sources.len() {
                        if t.lines.iter().any(|l| l.src == si) {
                            out.push(self.dap_source(ti, si));
                        }
                    }
                }
                Ok(obj([("sources", Json::Arr(out))]))
            }
            _ => Err(format!("unsupported request: {command}")),
        }
    }

    fn var_ref(&mut self, r: VarRef) -> i64 {
        self.var_refs.push(r);
        DYNAMIC + self.var_refs.len() as i64 - 1
    }

    fn dap_fixed_scopes(&self) -> Json {
        obj([(
            "scopes",
            Json::Arr(vec![
                obj([
                    ("name", "レジスタ".into()),
                    ("variablesReference", REGISTERS.into()),
                    ("expensive", false.into()),
                    ("presentationHint", "registers".into()),
                ]),
                obj([
                    ("name", "スタック".into()),
                    ("variablesReference", STACK.into()),
                    ("expensive", false.into()),
                ]),
                obj([
                    ("name", self.os.info().get("name").str().unwrap_or("OS").to_string().into()),
                    ("variablesReference", PROCESS.into()),
                    ("expensive", false.into()),
                ]),
            ]),
        )])
    }

    /// (name, table, type, place) for the members of a variables node.
    #[allow(clippy::type_complexity)]
    fn dap_var_nodes(
        &self,
        r: i64,
    ) -> Vec<(
        String,
        usize,
        super::debuginfo::Type,
        Result<Place, &'static str>,
    )> {
        let Some(node) = self.var_refs.get((r - DYNAMIC) as usize).cloned() else {
            return Vec::new();
        };
        match node {
            VarRef::Locals(frame) => {
                let frames = self.frames(frame + 1);
                let Some(f) = frames.get(frame) else {
                    return Vec::new();
                };
                let Some((ti, _)) = f.func else {
                    return Vec::new();
                };
                self.frame_vars(frame, f)
                    .into_iter()
                    .map(|v| (v.name.clone(), ti, v.ty.clone(), self.place_of(f, v)))
                    .collect()
            }
            VarRef::Globals(ti) => {
                let dummy = super::frames::Frame {
                    pc: 0,
                    s: 0,
                    cfa: None,
                    u: 0,
                    func: None,
                };
                let Some(d) = self.symbols.tables.get(ti).and_then(|t| t.debug.as_ref()) else {
                    return Vec::new();
                };
                d.globals
                    .iter()
                    .filter(|g| !matches!(g.loc, super::debuginfo::Loc::Unknown(_)))
                    .map(|g| (g.name.clone(), ti, g.ty.clone(), self.place_of(&dummy, g)))
                    .collect()
            }
            VarRef::Node { table, ty, place } => self
                .with_ctx(table, |c| c.children(&ty, place))
                .unwrap_or_default()
                .into_iter()
                .map(|(n, t, p)| (n, table, t, Ok(p)))
                .collect(),
        }
    }

    fn dap_var_children(&mut self, r: i64) -> Vec<Json> {
        let mut out = Vec::new();
        for (name, table, ty, place) in self.dap_var_nodes(r) {
            let tname = self.symbols.tables[table]
                .debug
                .as_ref()
                .map(|d| d.type_name(&ty));
            let (value, child, mem) = match place {
                Ok(p) => {
                    let value = self
                        .with_ctx(table, |c| c.format(&ty, p))
                        .unwrap_or_default();
                    let expandable = self.with_ctx(table, |c| c.expandable(&ty)).unwrap_or(false);
                    let child = if expandable {
                        self.var_ref(VarRef::Node {
                            table,
                            ty: ty.clone(),
                            place: p,
                        })
                    } else {
                        0
                    };
                    let mem = match p {
                        Place::Mem(a) => hex_ref(a),
                        Place::Value(_) => Json::Null,
                    };
                    (value, child, mem)
                }
                Err(why) => (format!("（{why}）"), 0, Json::Null),
            };
            out.push(obj([
                ("name", name.into()),
                ("value", value.into()),
                ("type", tname.into()),
                ("variablesReference", child.into()),
                ("memoryReference", mem),
                ("evaluateName", Json::Null),
            ]));
        }
        out
    }

    fn dap_bp(&self, id: u32, requested: Option<u32>) -> Json {
        let Some(b) = self.sym_bps.iter().find(|b| b.id == id) else {
            return obj([("verified", false.into())]);
        };
        let verified = !b.targets.is_empty();
        let addr = b
            .targets
            .iter()
            .find_map(|t| self.symbols.logical(&self.machine, *t));
        obj([
            ("id", id.into()),
            ("verified", verified.into()),
            ("line", b.line.or(requested).into()),
            (
                "instructionReference",
                addr.map(hex_ref).unwrap_or(Json::Null),
            ),
            (
                "message",
                if verified {
                    Json::Null
                } else if matches!(b.spec, BpSpec::Line { .. }) && b.line.is_none() {
                    "この行に対応するコードがありません".into()
                } else {
                    "モジュールがロードされると有効になります".into()
                },
            ),
        ])
    }

    fn dap_variables(&self, reference: i64) -> Vec<Json> {
        let var = |name: &str, value: String, mem: Option<u16>| {
            obj([
                ("name", name.into()),
                ("value", value.into()),
                ("variablesReference", 0.into()),
                ("memoryReference", mem.map(hex_ref).unwrap_or(Json::Null)),
            ])
        };
        let m = &self.machine;
        let label = |a: u16| {
            self.symbols
                .label(m, a)
                .map_or(String::new(), |l| format!("  <{l}>"))
        };
        match reference {
            REGISTERS => {
                let r = Regs::of(&self.cpu);
                vec![
                    var("A", format!("${:02X}", r.a), None),
                    var("B", format!("${:02X}", r.b), None),
                    var("D", format!("${:04X}", r.d()), Some(r.d())),
                    var("X", format!("${:04X}{}", r.x, label(r.x)), Some(r.x)),
                    var("Y", format!("${:04X}{}", r.y, label(r.y)), Some(r.y)),
                    var("U", format!("${:04X}{}", r.u, label(r.u)), Some(r.u)),
                    var("S", format!("${:04X}", r.s), Some(r.s)),
                    var("PC", format!("${:04X}{}", r.pc, label(r.pc)), Some(r.pc)),
                    var("DP", format!("${:02X}", r.dp), None),
                    var("CC", format!("${:02X} {}", r.cc, cc_flags(r.cc)), None),
                ]
            }
            STACK => (0..32u16)
                .map(|i| {
                    let at = self.cpu.s.wrapping_add(i * 2);
                    let w = m.peek16(at);
                    let ret = if self.is_return_address(w) {
                        " ↩"
                    } else {
                        ""
                    };
                    var(
                        &format!("S+${:02X}", i * 2),
                        format!("${w:04X}{}{ret}", label(w)),
                        Some(at),
                    )
                })
                .collect(),
            PROCESS => {
                let Some(os) = self.os() else {
                    return vec![var("OS", "未検出".into(), None)];
                };
                let show = |v: &Json| v.str().map_or_else(|| v.to_string(), str::to_string);
                let mut out: Vec<Json> = os.summary(m).iter().map(|(k, v)| var(k, show(v), None)).collect();
                let cur = os.current(m);
                if let Some(p) = os.processes(m).into_iter().find(|p| p.key == cur) {
                    out.push(var("プロセスID", p.id.to_string(), None));
                    out.push(var("名前", p.name.clone(), None));
                    out.push(var("親", p.parent.to_string(), None));
                    for (k, v) in &p.fields {
                        out.push(var(k, show(v), None));
                    }
                }
                if let Some(c) = self.tracer.calls.values().rev().find(|c| !c.system) {
                    out.push(var(
                        "直近のシステムコール",
                        format!("{} {} → {}", os.service_name(c.code), c.args, c.result),
                        None,
                    ));
                }
                out
            }
            _ => Vec::new(),
        }
    }

    fn dap_evaluate(&mut self, expr: &str, context: &str) -> Result<Json, String> {
        let e = expr.trim();
        // In the debug console, `>text` types a line into the guest console.
        if context == "repl"
            && let Some(line) = e.strip_prefix('>')
        {
            self.pending_input.extend(line.trim_start().bytes());
            self.pending_input.push_back(13);
            return Ok(obj([
                (
                    "result",
                    format!("→ コンソール: {}", line.trim_start()).into(),
                ),
                ("variablesReference", 0.into()),
            ]));
        }
        self.refresh_symbols();
        let m = &self.machine;
        let r = Regs::of(&self.cpu);
        let reg = match e.to_ascii_lowercase().as_str() {
            "a" => Some(u16::from(r.a)),
            "b" => Some(u16::from(r.b)),
            "d" => Some(r.d()),
            "x" => Some(r.x),
            "y" => Some(r.y),
            "u" => Some(r.u),
            "s" => Some(r.s),
            "pc" => Some(r.pc),
            "dp" => Some(u16::from(r.dp)),
            "cc" => Some(u16::from(r.cc)),
            _ => None,
        };
        // `[expr]` reads the word at an address.
        let (value, indirect) =
            if let Some(inner) = e.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
                let a = self.symbols.resolve(m, inner).ok_or("式を評価できません")?;
                (m.peek16(a), Some(a))
            } else if let Some(v) = reg {
                (v, None)
            } else if let Some(v) = self.symbols.resolve(m, e) {
                (v, None)
            } else {
                return Err(format!("評価できません: {e}"));
            };
        let label = self
            .symbols
            .label(m, value)
            .map_or(String::new(), |l| format!("  <{l}>"));
        let text = match indirect {
            Some(a) => format!("${value:04X} ({value}){label}  ← [${a:04X}]"),
            None => format!("${value:04X} ({value}){label}"),
        };
        Ok(obj([
            ("result", text.into()),
            ("variablesReference", 0.into()),
            ("memoryReference", hex_ref(value)),
        ]))
    }
}

fn read_message(r: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            length = v.trim().parse::<usize>().ok().filter(|&n| n <= 16 << 20);
        }
    }
    let mut body = vec![0; length.unwrap_or(0)];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

fn serve(stream: TcpStream, id: u32, inbox: Sender<Inbox>) -> io::Result<()> {
    let (tx, rx) = mpsc::channel::<String>();
    let mut writer = stream.try_clone()?;
    thread::spawn(move || {
        for msg in rx {
            let frame = format!("Content-Length: {}\r\n\r\n{msg}", msg.len());
            if writer
                .write_all(frame.as_bytes())
                .and_then(|_| writer.flush())
                .is_err()
            {
                break;
            }
        }
    });
    let mut reader = BufReader::new(stream);
    while let Some(body) = read_message(&mut reader)? {
        let Ok(msg) = Json::parse(&String::from_utf8_lossy(&body)) else {
            continue;
        };
        if inbox
            .send(Inbox::Dap {
                id,
                msg,
                out: tx.clone(),
            })
            .is_err()
        {
            break;
        }
    }
    Ok(())
}

/// Accept DAP connections in the background.
pub fn start(listener: TcpListener, inbox: Sender<Inbox>) {
    thread::spawn(move || {
        for (n, stream) in listener.incoming().flatten().enumerate() {
            let inbox = inbox.clone();
            let id = n as u32 + 1;
            let _ = stream.set_nodelay(true);
            thread::spawn(move || {
                let _ = serve(stream, id, inbox.clone());
                let _ = inbox.send(Inbox::DapClosed(id));
            });
        }
    });
}
