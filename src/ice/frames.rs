//! Call frames and variable values for the front ends (DAP, browser).
//!
//! Frames come from the frame rules in [`DebugInfo`]. A frame base computed
//! from the static stack depth is accepted only if the word at its return
//! slot really follows a call to the function; otherwise nearby depths are
//! searched, so an unknown helper's stack effect cannot misplace variables.
use super::debuginfo::{Ctx, Loc, Place, Type, Var};
use super::disasm::{Flow, disassemble};
use super::{Ice, json_str};

#[derive(Debug, Clone)]
pub struct Frame {
    pub pc: u16,
    /// S as seen by this frame.
    pub s: u16,
    /// Frame base, when the function is described.
    pub cfa: Option<u16>,
    /// The register variable (U) of this frame.
    pub u: u16,
    /// (table, function index) when variables are known.
    pub func: Option<(usize, usize)>,
}

/// A node of the variables tree, valid until execution resumes.
#[derive(Debug, Clone)]
pub enum VarRef {
    Locals(usize),
    Globals(usize),
    Node {
        table: usize,
        ty: Type,
        place: Place,
    },
}

impl Ice {
    /// Is `w` the address after a call to `target` (any call when `None`)?
    fn returns_from_call_to(&self, w: u16, target: Option<u16>) -> bool {
        let m = &self.machine;
        (2..=4u16).any(|k| {
            let l = disassemble(&|a| m.peek(a), w.wrapping_sub(k));
            l.len == k
                && l.flow == Flow::Call
                && !l.text.starts_with("SWI")
                && !l.svc
                && (target.is_none() || l.target.is_none() || l.target == target)
        })
    }

    /// Frames from the current PC outward.
    pub fn frames(&self, max: usize) -> Vec<Frame> {
        let m = &self.machine;
        let mut out = Vec::new();
        let (mut pc, mut s, mut u) = (self.cpu.pc, self.cpu.s, self.cpu.u);
        while out.len() < max {
            let described = self.symbols.locate(m, pc).and_then(|(ti, off)| {
                let d = self.symbols.tables[ti].debug.as_ref()?;
                let fi = d
                    .functions
                    .iter()
                    .position(|f| off >= f.start && off < f.end)?;
                Some((ti, off, fi))
            });
            let Some((ti, off, fi)) = described else {
                // Without frame rules, fall back to scanning for return addresses.
                out.push(Frame {
                    pc,
                    s,
                    cfa: None,
                    u,
                    func: None,
                });
                if out.len() == 1 {
                    for (at, w) in self.call_chain_from(s, max - 1) {
                        out.push(Frame {
                            pc: w,
                            s: at.wrapping_add(2),
                            cfa: None,
                            u,
                            func: None,
                        });
                    }
                }
                break;
            };
            let f = &self.symbols.tables[ti].debug.as_ref().unwrap().functions[fi];
            let start = pc.wrapping_sub(off - f.start);
            let (Some(ret_at), Some(depth)) = (f.ret_at, f.depth_at(off)) else {
                out.push(Frame {
                    pc,
                    s,
                    cfa: None,
                    u,
                    func: Some((ti, fi)),
                });
                break;
            };
            let valid = |d: i16| {
                let cfa = s.wrapping_add(d as u16);
                let ret = m.peek16(cfa.wrapping_add(ret_at as u16));
                self.returns_from_call_to(ret, Some(start))
                    .then_some((cfa, d))
            };
            let found = valid(depth).or_else(|| {
                (1..=64i16).find_map(|k| valid(depth + k).or_else(|| valid(depth - k)))
            });
            let Some((cfa, d)) = found else {
                out.push(Frame {
                    pc,
                    s,
                    cfa: None,
                    u,
                    func: Some((ti, fi)),
                });
                break;
            };
            out.push(Frame {
                pc,
                s,
                cfa: Some(cfa),
                u,
                func: Some((ti, fi)),
            });
            let slot = cfa.wrapping_add(ret_at as u16);
            // The caller's U is saved once the prologue ran (depth >= 0).
            if d >= 0
                && let Some(&(_, o)) = f.saved.iter().find(|r| r.0 == "U")
            {
                u = m.peek16(cfa.wrapping_add(o as u16));
            }
            pc = m.peek16(slot);
            s = slot.wrapping_add(2);
        }
        out
    }

    /// Return addresses on the stack from `s`, innermost first.
    pub fn call_chain_from(&self, s: u16, max: usize) -> Vec<(u16, u16)> {
        let m = &self.machine;
        let labelled = !self.symbols.tables.is_empty();
        let mut out = Vec::new();
        for i in 0..512u16 {
            if out.len() >= max {
                break;
            }
            let at = s.wrapping_add(i);
            if at < s {
                break;
            }
            let w = m.peek16(at);
            if self.is_return_address(w) && (!labelled || self.symbols.locate(m, w).is_some()) {
                out.push((at, w));
            }
        }
        out
    }

    /// The source line of a frame (callers: the line of the call).
    pub fn frame_line(&self, index: usize, f: &Frame) -> Option<(usize, usize, u32)> {
        let at = if index == 0 {
            f.pc
        } else {
            f.pc.wrapping_sub(1)
        };
        self.symbols
            .line(&self.machine, at)
            .map(|(ti, l)| (ti, l.src, l.line))
    }

    pub fn place_of(&self, f: &Frame, v: &Var) -> Result<Place, &'static str> {
        let y = self.cpu.y;
        match &v.loc {
            Loc::Frame(o) => f
                .cfa
                .map(|c| Place::Mem(c.wrapping_add(*o as u16)))
                .ok_or("フレームの位置が分かりません"),
            Loc::Reg("U") => Ok(Place::Value(f.u)),
            Loc::Reg(_) => Err("レジスタ"),
            Loc::Data(o) => Ok(Place::Mem(y.wrapping_add(*o))),
            Loc::Abs(a) => Ok(Place::Mem(*a)),
            Loc::Unknown(why) => Err(why),
        }
    }

    /// Variables visible in a frame: parameters, then locals in scope.
    pub fn frame_vars(&self, index: usize, f: &Frame) -> Vec<&Var> {
        let Some((ti, fi)) = f.func else {
            return Vec::new();
        };
        let func = &self.symbols.tables[ti].debug.as_ref().unwrap().functions[fi];
        let line = self.frame_line(index, f).map(|x| x.2);
        let mut vars: Vec<&Var> = Vec::new();
        for v in &func.vars {
            if let (Some((a, b)), Some(l)) = (v.lines, line)
                && !(a..=b).contains(&l)
            {
                continue;
            }
            // An inner declaration hides an outer one of the same name.
            vars.retain(|x| x.name != v.name);
            vars.push(v);
        }
        vars
    }

    pub fn with_ctx<R>(&self, table: usize, f: impl FnOnce(&Ctx) -> R) -> Option<R> {
        let info = self.symbols.tables.get(table)?.debug.as_ref()?;
        let m = &self.machine;
        let read = |a: u16| m.peek(a);
        let label = |a: u16| self.symbols.label(m, a);
        Some(f(&Ctx {
            read: &read,
            label: &label,
            info,
        }))
    }

    /// Resolve a C expression in a frame (or, without one, the globals).
    pub fn eval_var(&self, frame: usize, expr: &str) -> Option<(usize, Type, Place)> {
        let frames = self.frames(frame + 1);
        let f = frames.get(frame);
        let table = f
            .and_then(|f| f.func.map(|x| x.0))
            .or_else(|| self.symbols.tables.iter().position(|t| t.debug.is_some()))?;
        let lookup = |name: &str| -> Option<(Type, Place)> {
            if let Some(f) = f
                && let Some(v) = self
                    .frame_vars(frame, f)
                    .into_iter()
                    .find(|v| v.name == name)
            {
                return self.place_of(f, v).ok().map(|p| (v.ty.clone(), p));
            }
            let d = self.symbols.tables[table].debug.as_ref()?;
            let g = d.globals.iter().find(|g| g.name == name)?;
            let dummy = Frame {
                pc: 0,
                s: 0,
                cfa: None,
                u: 0,
                func: None,
            };
            self.place_of(f.unwrap_or(&dummy), g)
                .ok()
                .map(|p| (g.ty.clone(), p))
        };
        let (t, p) = self.with_ctx(table, |c| c.eval(expr, &lookup))??;
        Some((table, t, p))
    }

    fn var_json(
        &self,
        table: usize,
        name: &str,
        ty: &Type,
        place: Result<Place, &str>,
        depth: u8,
    ) -> String {
        let info = self.symbols.tables[table].debug.as_ref().unwrap();
        let tname = info.type_name(ty);
        let (value, at, children) = match place {
            Ok(p) => self
                .with_ctx(table, |c| {
                    let kids: Vec<String> = if depth == 0 && c.expandable(ty) {
                        c.children(ty, p)
                            .into_iter()
                            .take(64)
                            .map(|(n, t, cp)| self.var_json(table, &n, &t, Ok(cp), depth + 1))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let at = match p {
                        Place::Mem(a) => format!("${a:04X}"),
                        Place::Value(_) => "U".into(),
                    };
                    (c.format(ty, p), at, kids)
                })
                .unwrap_or_default(),
            Err(why) => (format!("（{why}）"), String::new(), Vec::new()),
        };
        format!(
            "{{\"name\":{},\"type\":{},\"value\":{},\"at\":{},\"children\":[{}]}}",
            json_str(name),
            json_str(&tname),
            json_str(&value),
            json_str(&at),
            children.join(",")
        )
    }

    /// `/api/vars?frame=N`: frames, the frame's variables and the globals.
    pub fn vars_json(&self, frame: usize) -> String {
        let frames = self.frames(32);
        let names: Vec<String> = frames
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let name = self
                    .symbols
                    .label(&self.machine, f.pc)
                    .unwrap_or_else(|| format!("${:04X}", f.pc));
                let line = self.frame_line(i, f).map(|(ti, si, l)| {
                    let s = &self.symbols.tables[ti].sources[si];
                    format!("{}:{l}", s.name.rsplit('/').next().unwrap_or(&s.name))
                });
                format!(
                    "{{\"pc\":{},\"name\":{},\"line\":{},\"vars\":{}}}",
                    f.pc,
                    json_str(&name),
                    line.map_or("null".into(), |l| json_str(&l)),
                    f.func.is_some()
                )
            })
            .collect();
        let mut locals = Vec::new();
        let mut table = None;
        if let Some(f) = frames.get(frame)
            && let Some((ti, _)) = f.func
        {
            table = Some(ti);
            for v in self.frame_vars(frame, f) {
                locals.push(self.var_json(ti, &v.name, &v.ty, self.place_of(f, v), 0));
            }
        }
        let table = table.or_else(|| self.symbols.tables.iter().position(|t| t.debug.is_some()));
        let mut globals = Vec::new();
        if let Some(ti) = table {
            let dummy = Frame {
                pc: 0,
                s: 0,
                cfa: None,
                u: 0,
                func: None,
            };
            for g in &self.symbols.tables[ti].debug.as_ref().unwrap().globals {
                if matches!(g.loc, Loc::Unknown(_)) {
                    continue;
                }
                globals.push(self.var_json(ti, &g.name, &g.ty, self.place_of(&dummy, g), 0));
            }
        }
        format!(
            "{{\"frame\":{frame},\"frames\":[{}],\"locals\":[{}],\"globals\":[{}]}}",
            names.join(","),
            locals.join(","),
            globals.join(",")
        )
    }
}
