// src/codegen/disasm.rs - x86-64 机器码 → AT&T 汇编文本（-S 输出）

use std::collections::{BTreeMap, HashMap};

const REG8: [&str; 16] = [
    "%al", "%cl", "%dl", "%bl", "%spl", "%bpl", "%sil", "%dil",
    "%r8b", "%r9b", "%r10b", "%r11b", "%r12b", "%r13b", "%r14b", "%r15b",
];
const REG32: [&str; 16] = [
    "%eax", "%ecx", "%edx", "%ebx", "%esp", "%ebp", "%esi", "%edi",
    "%r8d", "%r9d", "%r10d", "%r11d", "%r12d", "%r13d", "%r14d", "%r15d",
];
const REG64: [&str; 16] = [
    "%rax", "%rcx", "%rdx", "%rbx", "%rsp", "%rbp", "%rsi", "%rdi",
    "%r8", "%r9", "%r10", "%r11", "%r12", "%r13", "%r14", "%r15",
];
const XMM: [&str; 16] = [
    "%xmm0", "%xmm1", "%xmm2", "%xmm3", "%xmm4", "%xmm5", "%xmm6", "%xmm7",
    "%xmm8", "%xmm9", "%xmm10", "%xmm11", "%xmm12", "%xmm13", "%xmm14", "%xmm15",
];

#[derive(Default, Clone, Copy)]
struct Rex {
    w: bool,
    r: bool,
    x: bool,
    b: bool,
}

#[derive(Default)]
struct Prefixes {
    lock: bool,
    rep: bool,
    repne: bool,
    o16: bool,
}

struct Dec<'a> {
    code: &'a [u8],
    pos: usize,
    base: u64,
    calls: &'a HashMap<u64, String>,
    data: &'a HashMap<u64, String>,
}

impl<'a> Dec<'a> {
    fn peek(&self) -> Option<u8> {
        self.code.get(self.pos).copied()
    }

    fn take(&mut self) -> Option<u8> {
        if self.pos >= self.code.len() {
            return None;
        }
        let b = self.code[self.pos];
        self.pos += 1;
        Some(b)
    }

    fn take_i32(&mut self) -> Option<i32> {
        if self.pos + 4 > self.code.len() {
            return None;
        }
        let v = i32::from_le_bytes(self.code[self.pos..self.pos + 4].try_into().ok()?);
        self.pos += 4;
        Some(v)
    }

    fn take_i8(&mut self) -> Option<i8> {
        Some(self.take()? as i8)
    }

    fn abs_off(&self) -> u64 {
        self.base + self.pos as u64
    }

    fn reg(&self, _rex: &Rex, idx: u8, width: u8) -> &'static str {
        // idx 已由调用方按 REX.R / REX.B / REX.X 扩展为完整 0..15 索引.
        // 此处不能再叠加 REX.B, 否则会把 reg 字段 (用 REX.R 扩展) 错当成 r/m 字段
        // (用 REX.B 扩展), 导致反汇编里 rax 误显示为 r8, r8 误显示为 rax 等.
        let i = (idx & 15) as usize;
        match width {
            1 => REG8[i],
            4 => REG32[i],
            _ => REG64[i],
        }
    }

    fn xmm(&self, rex: &Rex, idx: u8) -> &'static str {
        XMM[(idx & 7) as usize + if rex.r { 8 } else { 0 }]
    }

    fn parse_rex(&mut self) -> Rex {
        let b = self.peek().unwrap_or(0);
        if (b & 0xF0) == 0x40 {
            let _ = self.take();
            Rex {
                w: (b & 0x08) != 0,
                r: (b & 0x04) != 0,
                x: (b & 0x02) != 0,
                b: (b & 0x01) != 0,
            }
        } else {
            Rex::default()
        }
    }

    fn modrm_gp(&mut self, rex: &Rex, width: u8) -> Option<(String, String)> {
        let modrm = self.take()?;
        let mod_ = modrm >> 6;
        let reg = (modrm >> 3) & 7;
        let rm = modrm & 7;
        let reg_name = self.reg(rex, reg | if rex.r { 8 } else { 0 }, width).to_string();
        let mem = self.decode_mem(rex, mod_, rm, width)?;
        Some((reg_name, mem))
    }

    fn modrm_xmm(&mut self, rex: &Rex) -> Option<(String, String)> {
        let modrm = self.take()?;
        let mod_ = modrm >> 6;
        let reg = (modrm >> 3) & 7;
        let rm = modrm & 7;
        let reg_name = self.xmm(rex, reg).to_string();
        let mem = self.decode_mem(rex, mod_, rm, 8)?;
        Some((reg_name, mem))
    }

    fn decode_mem(&mut self, rex: &Rex, mod_: u8, rm: u8, width: u8) -> Option<String> {
        let rm_ext = rm | if rex.b { 8 } else { 0 };
        if mod_ == 3 {
            return Some(self.reg(rex, rm_ext, width).to_string());
        }
        let mut disp = 0i32;
        let base: String;
        let mut index = String::new();
        let mut scale = 1u32;
        if rm == 4 {
            let sib = self.take()?;
            let scale_bits = sib >> 6;
            let index_reg = (sib >> 3) & 7;
            let base_reg = sib & 7;
            scale = 1u32 << scale_bits;
            if index_reg != 4 {
                index = self.reg(rex, index_reg | if rex.x { 8 } else { 0 }, width).to_string();
            }
            if mod_ == 0 && base_reg == 5 {
                disp = self.take_i32()?;
                base = "rip".to_string();
            } else {
                base = self.reg(rex, base_reg | if rex.b { 8 } else { 0 }, width).to_string();
            }
        } else if mod_ == 0 && rm == 5 {
            disp = self.take_i32()?;
            base = "rip".to_string();
        } else {
            base = self.reg(rex, rm_ext, width).to_string();
        }
        match mod_ {
            0 => {}
            1 => disp = self.take_i8()? as i32,
            2 => disp = self.take_i32()?,
            _ => {}
        }
        let sym = self.data.get(&self.abs_off()).cloned();
        let mut inner = String::new();
        if let Some(s) = sym {
            inner.push_str(&s);
        } else if disp != 0 {
            inner.push_str(&format!("{disp}"));
        }
        if !index.is_empty() {
            if !inner.is_empty() {
                inner.push(',');
            }
            inner.push_str(&format!("{index},{scale}"));
        }
        if inner.is_empty() {
            Some(base)
        } else {
            Some(format!("{inner}({base})"))
        }
    }

    fn rel_target(&self, rel: i32, insn_end: u64) -> String {
        format!(".L{}", (insn_end as i64 + rel as i64) as u64)
    }

    fn prefix_str(pfx: &Prefixes) -> String {
        let mut parts = Vec::new();
        if pfx.lock { parts.push("lock"); }
        if pfx.repne { parts.push("repne"); }
        else if pfx.rep { parts.push("rep"); }
        if pfx.o16 { parts.push("data16"); }
        if parts.is_empty() { String::new() } else { format!("{} ", parts.join(" ")) }
    }

    fn finish(&self, start: usize, pfx: &Prefixes, body: &str) -> (usize, String) {
        let text = if pfx.lock || pfx.rep || pfx.repne || pfx.o16 {
            format!("{}{body}", Self::prefix_str(pfx))
        } else {
            body.to_string()
        };
        (self.pos - start, text)
    }

    fn take_prefixes(&mut self) -> Prefixes {
        let mut pfx = Prefixes::default();
        loop {
            match self.peek() {
                Some(0xF0) => { pfx.lock = true; self.take(); }
                Some(0xF3) => { pfx.rep = true; self.take(); }
                Some(0xF2) => { pfx.repne = true; self.take(); }
                Some(0x66) => { pfx.o16 = true; self.take(); }
                _ => break,
            }
        }
        pfx
    }

    fn decode_one(&mut self) -> Option<(usize, String)> {
        let start = self.pos;
        let off = self.base + start as u64;
        let pfx = self.take_prefixes();
        let b = self.peek()?;

        if (0x50..=0x57).contains(&b) {
            self.take();
            return Some((self.pos - start, format!("{}push {}", Self::prefix_str(&pfx), REG64[(b - 0x50) as usize])));
        }
        if (0x58..=0x5F).contains(&b) {
            self.take();
            return Some((self.pos - start, format!("{}pop {}", Self::prefix_str(&pfx), REG64[(b - 0x58) as usize])));
        }

        match b {
            0x90 if pfx.rep => {
                self.take();
                Some((self.pos - start, "pause".into()))
            }
            0x90 => { self.take(); Some((1, "nop".into())) }
            0xC3 => { self.take(); Some((self.pos - start, format!("{}ret", Self::prefix_str(&pfx)))) }
            0xC9 => { self.take(); Some((self.pos - start, "leave".into())) }
            0x55 => { self.take(); Some((self.pos - start, "push %rbp".into())) }
            0x5D => { self.take(); Some((self.pos - start, "pop %rbp".into())) }
            0x99 => { self.take(); Some((self.pos - start, "cltd".into())) }
            0xA4 if pfx.rep => { self.take(); Some((self.pos - start, "rep movsb".into())) }
            0xA5 if pfx.rep => {
                self.take();
                let rex = self.parse_rex();
                let mn = if rex.w { "rep movsq" } else { "rep movsl" };
                Some((self.pos - start, mn.into()))
            }
            0xEB => {
                self.take();
                let rel = self.take_i8()? as i32;
                Some((self.pos - start, format!("{}jmp {}", Self::prefix_str(&pfx), self.rel_target(rel, off + 2))))
            }
            0xE8 => {
                self.take();
                let rel = self.take_i32()?;
                let sym = self.calls.get(&(off + 1)).cloned();
                let target = sym.unwrap_or_else(|| self.rel_target(rel, off + 5));
                Some((self.pos - start, format!("{}call {target}", Self::prefix_str(&pfx))))
            }
            0xE9 => {
                self.take();
                let rel = self.take_i32()?;
                Some((self.pos - start, format!("{}jmp {}", Self::prefix_str(&pfx), self.rel_target(rel, off + 5))))
            }
            0x31 if self.code.get(self.pos) == Some(&0xC0) => {
                self.take(); self.take();
                Some((self.pos - start, "xor %eax, %eax".into()))
            }
            0x85 if self.code.get(self.pos) == Some(&0xC0) => {
                self.take(); self.take();
                Some((self.pos - start, "test %eax, %eax".into()))
            }
            0xB8..=0xBF => {
                let reg = b - 0xB8;
                self.take();
                let imm = self.take_i32()?;
                Some((self.pos - start, format!("mov ${imm}, {}", REG32[reg as usize])))
            }
            0xB0..=0xB7 => {
                let reg = b - 0xB0;
                self.take();
                let imm = self.take()? as i32;
                Some((self.pos - start, format!("mov ${imm}, {}", REG8[reg as usize])))
            }
            0x0F => self.decode_two_byte(start, &pfx),
            0x41 | 0x49 | 0x4C | 0x48 => self.decode_rex(start, &pfx),
            _ => self.decode_legacy(start, &pfx),
        }
    }

    fn decode_two_byte(&mut self, start: usize, pfx: &Prefixes) -> Option<(usize, String)> {
        let off = self.base + start as u64;
        let _ = self.take();
        let op = self.take()?;
        match op {
            0x05 => Some((self.pos - start, "syscall".into())),
            0x0B => Some((self.pos - start, "ud2".into())),
            0x1F => {
                let _ = self.take();
                Some((self.pos - start, "nop".into()))
            }
            0x84..=0x8F => {
                let cc = match op - 0x80 {
                    0x04 => "je", 0x05 => "jne", 0x0C => "jl", 0x0D => "jge",
                    0x0E => "jle", 0x0F => "jg", 0x02 => "jb", 0x03 => "jae",
                    0x08 => "js", 0x09 => "jns",
                    _ => "jcc",
                };
                let rel = self.take_i32()?;
                let end = off + (self.pos - start) as u64;
                Some((self.pos - start, format!("{cc} {}", self.rel_target(rel, end))))
            }
            0xAE if self.peek() == Some(0xF0) => {
                self.take();
                Some((self.pos - start, "mfence".into()))
            }
            0xB0 | 0xB1 => {
                let rex = self.parse_rex();
                let (src, dst) = self.modrm_gp(&rex, if rex.w { 8 } else { 4 })?;
                let mn = if op == 0xB0 { "cmpxchg" } else { "cmpxchg" };
                let _ = mn;
                Some(self.finish(start, pfx, &format!("cmpxchg {src}, {dst}")))
            }
            0xC1 => {
                let rex = self.parse_rex();
                let (src, dst) = self.modrm_gp(&rex, if rex.w { 8 } else { 4 })?;
                Some(self.finish(start, pfx, &format!("xadd {src}, {dst}")))
            }
            0x10 | 0x11 | 0x2A | 0x2C | 0x5A => {
                self.pos = start;
                self.decode_sse(start, pfx)
            }
            0xB6 | 0xB7 | 0xBE | 0xBF | 0xAF | 0xB8 => {
                self.pos = start;
                self.decode_rex(start, pfx)
            }
            _ => { self.pos = start + 1; None }
        }
    }

    fn decode_sse(&mut self, start: usize, _pfx: &Prefixes) -> Option<(usize, String)> {
        let pfx = self.take_prefixes();
        let rex = self.parse_rex();
        let _ = self.take(); // 0x0F
        let op = self.take()?;
        let scalar = if pfx.repne { "sd" } else if pfx.rep { "ss" } else { "sd" };
        match op {
            0x10 => {
                let (dst, src) = self.modrm_xmm(&rex)?;
                let mn = if scalar == "ss" { "movss" } else { "movsd" };
                Some(self.finish(start, &pfx, &format!("{mn} {src}, {dst}")))
            }
            0x11 => {
                let (src, dst) = self.modrm_xmm(&rex)?;
                let mn = if scalar == "ss" { "movss" } else { "movsd" };
                Some(self.finish(start, &pfx, &format!("{mn} {src}, {dst}")))
            }
            0x2A => {
                let (xmm, src) = self.modrm_xmm(&rex)?;
                let mn = if scalar == "ss" { "cvtsi2ss" } else { "cvtsi2sd" };
                Some(self.finish(start, &pfx, &format!("{mn} {src}, {xmm}")))
            }
            0x2C => {
                let (dst, xmm) = self.modrm_xmm(&rex)?;
                let mn = if scalar == "ss" { "cvttss2si" } else { "cvttsd2si" };
                Some(self.finish(start, &pfx, &format!("{mn} {xmm}, {dst}")))
            }
            0x5A => {
                let (dst, src) = self.modrm_xmm(&rex)?;
                let mn = if scalar == "ss" { "cvtss2sd" } else { "cvtsd2ss" };
                Some(self.finish(start, &pfx, &format!("{mn} {src}, {dst}")))
            }
            _ => { self.pos = start + 1; None }
        }
    }

    fn decode_rex(&mut self, start: usize, pfx: &Prefixes) -> Option<(usize, String)> {
        let rex = self.parse_rex();
        let width = if rex.w { 8 } else { 4 };
        let op = self.take()?;
        if (0x50..=0x5F).contains(&op) {
            let reg = (op & 7) as usize + if rex.b { 8 } else { 0 };
            let mn = if op >= 0x58 { "pop" } else { "push" };
            return Some(self.finish(start, pfx, &format!("{mn} {}", REG64[reg])));
        }
        if op == 0x0F {
            return self.decode_two_byte_rex(start, pfx, &rex);
        }
        match op {
            0x89 => {
                let (src, dst) = self.modrm_gp(&rex, width)?;
                Some(self.finish(start, pfx, &format!("mov {src}, {dst}")))
            }
            0x8B => {
                let (dst, src) = self.modrm_gp(&rex, width)?;
                Some(self.finish(start, pfx, &format!("mov {src}, {dst}")))
            }
            0x87 => {
                let (src, dst) = self.modrm_gp(&rex, width)?;
                Some(self.finish(start, pfx, &format!("xchg {src}, {dst}")))
            }
            0x8D => {
                let (dst, src) = self.modrm_gp(&rex, width)?;
                Some(self.finish(start, pfx, &format!("lea {dst}, {src}")))
            }
            0x01 | 0x09 | 0x21 | 0x29 | 0x31 => {
                let (dst, src) = self.modrm_gp(&rex, width)?;
                let mn = match op { 0x01 => "add", 0x09 => "or", 0x21 => "and", 0x29 => "sub", 0x31 => "xor", _ => "?" };
                Some(self.finish(start, pfx, &format!("{mn} {src}, {dst}")))
            }
            0x39 | 0x85 => {
                let (a, b) = self.modrm_gp(&rex, width)?;
                let mn = if op == 0x39 { "cmp" } else { "test" };
                Some(self.finish(start, pfx, &format!("{mn} {b}, {a}")))
            }
            0x81 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg { 0 => "add", 5 => "sub", 7 => "cmp", _ => "alu" };
                let (_, mem) = self.modrm_gp(&rex, width)?;
                let imm = self.take_i32()?;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {mem}")))
            }
            0x83 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg { 0 => "add", 5 => "sub", 7 => "cmp", _ => "alu" };
                let (_, mem) = self.modrm_gp(&rex, width)?;
                let imm = self.take_i8()? as i32;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {mem}")))
            }
            0xC7 => {
                let (_, mem) = self.modrm_gp(&rex, 4)?;
                let imm = self.take_i32()?;
                Some(self.finish(start, pfx, &format!("mov ${imm}, {mem}")))
            }
            0xB8..=0xBF => {
                let reg = op - 0xB8;
                let r = self.reg(&rex, reg | if rex.r { 8 } else { 0 }, width);
                let imm = if rex.w {
                    let v = i64::from_le_bytes(self.code[self.pos..self.pos + 8].try_into().ok()?);
                    self.pos += 8;
                    v
                } else {
                    self.take_i32()? as i64
                };
                Some(self.finish(start, pfx, &format!("mov ${imm}, {r}")))
            }
            0x98 => Some((self.pos - start, "cltq".into())),
            0x99 => Some((self.pos - start, "cqo".into())),
            0xFF => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let (_, mem) = self.modrm_gp(&rex, width)?;
                let mn = match reg { 2 => "call", 4 => "jmp", 6 => "push", _ => "ff" };
                Some(self.finish(start, pfx, &format!("{mn} {mem}")))
            }
            _ => { self.pos = start + 1; None }
        }
    }

    fn decode_two_byte_rex(&mut self, start: usize, pfx: &Prefixes, rex: &Rex) -> Option<(usize, String)> {
        let op = self.take()?;
        match op {
            0x10 | 0x11 => {
                let (a, b) = self.modrm_xmm(rex)?;
                if op == 0x10 {
                    Some(self.finish(start, pfx, &format!("movups {b}, {a}")))
                } else {
                    Some(self.finish(start, pfx, &format!("movups {a}, {b}")))
                }
            }
            0xB0 | 0xB1 => {
                let (src, dst) = self.modrm_gp(rex, if rex.w { 8 } else { 4 })?;
                Some(self.finish(start, pfx, &format!("cmpxchg {src}, {dst}")))
            }
            0xC1 => {
                let (src, dst) = self.modrm_gp(rex, if rex.w { 8 } else { 4 })?;
                Some(self.finish(start, pfx, &format!("xadd {src}, {dst}")))
            }
            _ => { self.pos = start + 1; None }
        }
    }

    fn decode_legacy(&mut self, start: usize, pfx: &Prefixes) -> Option<(usize, String)> {
        let rex = Rex::default();
        let op = self.take()?;
        match op {
            0x01 | 0x09 | 0x21 | 0x29 | 0x31 => {
                let (dst, src) = self.modrm_gp(&rex, 4)?;
                let mn = match op { 0x01 => "add", 0x09 => "or", 0x21 => "and", 0x29 => "sub", 0x31 => "xor", _ => "?" };
                Some(self.finish(start, pfx, &format!("{mn} {src}, {dst}")))
            }
            _ => { self.pos = start + 1; None }
        }
    }
}

pub fn disassemble_range(
    code: &[u8],
    start: usize,
    end: usize,
    base_offset: u64,
    labels: &BTreeMap<u64, Vec<&str>>,
    directives: &BTreeMap<u64, Vec<&str>>,
    calls: &HashMap<u64, String>,
    data: &HashMap<u64, String>,
) -> String {
    let mut out = String::new();
    let mut pos = start;
    while pos < end && pos < code.len() {
        let off = base_offset + pos as u64;
        if let Some(names) = labels.get(&off) {
            for n in names {
                out.push_str(n);
                out.push_str(":\n");
            }
        }
        if let Some(lines) = directives.get(&off) {
            for line in lines {
                out.push('\t');
                out.push_str(line);
                out.push('\n');
            }
        }
        let mut dec = Dec { code, pos, base: base_offset, calls, data };
        match dec.decode_one() {
            Some((len, text)) if len > 0 => {
                out.push('\t');
                out.push_str(&text);
                out.push('\n');
                pos += len;
            }
            _ => {
                out.push('\t');
                out.push_str(&format!(".byte 0x{:02x}", code[pos]));
                out.push('\n');
                pos += 1;
            }
        }
    }
    out
}

pub fn label_map<'a>(labels: impl IntoIterator<Item = (&'a u64, &'a str)>) -> BTreeMap<u64, Vec<&'a str>> {
    let mut m: BTreeMap<u64, Vec<&'a str>> = BTreeMap::new();
    for (off, name) in labels {
        m.entry(*off).or_default().push(name);
    }
    m
}

pub fn directive_map<'a>(items: impl IntoIterator<Item = (&'a u64, &'a str)>) -> BTreeMap<u64, Vec<&'a str>> {
    let mut m: BTreeMap<u64, Vec<&'a str>> = BTreeMap::new();
    for (off, line) in items {
        m.entry(*off).or_default().push(line);
    }
    m
}

pub fn slice_labels<'a>(
    labels: &BTreeMap<u64, Vec<&'a str>>,
    base: u64,
    size: u64,
) -> BTreeMap<u64, Vec<&'a str>> {
    let mut out = BTreeMap::new();
    let end = base + size;
    for (off, names) in labels {
        if *off >= base && *off < end {
            out.entry(off - base).or_insert_with(Vec::new).extend(names.iter().copied());
        }
    }
    out
}

pub fn slice_map<T: Clone>(
    map: &HashMap<u64, T>,
    base: u64,
    size: u64,
) -> HashMap<u64, T> {
    let end = base + size;
    map.iter()
        .filter(|(off, _)| **off >= base && **off < end)
        .map(|(off, v)| (off - base, v.clone()))
        .collect()
}

pub fn slice_directives<'a>(
    directives: &BTreeMap<u64, Vec<&'a str>>,
    base: u64,
    size: u64,
) -> BTreeMap<u64, Vec<&'a str>> {
    slice_labels(directives, base, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_mov_imm_and_ret() {
        let code = [0xB8, 0x05, 0x00, 0x00, 0x00, 0xC3];
        let out = disassemble_range(&code, 0, code.len(), 0, &BTreeMap::new(), &BTreeMap::new(), &HashMap::new(), &HashMap::new());
        assert!(out.contains("mov $5, %eax"));
        assert!(out.contains("ret"));
    }

    #[test]
    fn decodes_lock_cmpxchg_and_movsd() {
        let code = [
            0xF0, 0x48, 0x0F, 0xB1, 0x17, // lock cmpxchg %rdx, (%rdi)
            0xF2, 0x0F, 0x10, 0x05, 0, 0, 0, 0, // movsd 0(%rip), %xmm0
        ];
        let out = disassemble_range(&code, 0, code.len(), 0, &BTreeMap::new(), &BTreeMap::new(), &HashMap::new(), &HashMap::new());
        assert!(out.contains("lock cmpxchg"), "{out}");
        assert!(out.contains("movsd"), "{out}");
    }
}
