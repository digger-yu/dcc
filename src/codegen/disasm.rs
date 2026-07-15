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

struct Rex {
    w: bool,
    r: bool,
    x: bool,
    b: bool,
}

struct Dec<'a> {
    code: &'a [u8],
    pos: usize,
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
        self.pos as u64
    }

    fn reg(&self, rex: &Rex, idx: u8, width: u8) -> &'static str {
        let i = (idx & 7) as usize + if rex.b { 8 } else { 0 };
        match width {
            1 => REG8[i],
            4 => REG32[i],
            _ => REG64[i],
        }
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
            Rex { w: false, r: false, x: false, b: false }
        }
    }

    fn modrm(&mut self, rex: &Rex, width: u8) -> Option<(String, String)> {
        let modrm = self.take()?;
        let mod_ = modrm >> 6;
        let reg = (modrm >> 3) & 7;
        let rm = modrm & 7;
        let reg_name = self.reg(rex, reg | if rex.r { 8 } else { 0 }, width).to_string();
        let mem = self.decode_mem(rex, mod_, rm, width)?;
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

    fn decode_one(&mut self) -> Option<(usize, String)> {
        let start = self.pos;
        let off = start as u64;
        let b = self.peek()?;

        if (0x50..=0x57).contains(&b) {
            self.take();
            return Some((1, format!("push {}", REG64[(b - 0x50) as usize])));
        }
        if (0x58..=0x5F).contains(&b) {
            self.take();
            return Some((1, format!("pop {}", REG64[(b - 0x58) as usize])));
        }

        match b {
            0x90 => { self.take(); Some((1, "nop".into())) }
            0xC3 => { self.take(); Some((1, "ret".into())) }
            0xC9 => { self.take(); Some((1, "leave".into())) }
            0x55 => { self.take(); Some((1, "push %rbp".into())) }
            0x5D => { self.take(); Some((1, "pop %rbp".into())) }
            0x99 => { self.take(); Some((1, "cltd".into())) }
            0xEB => {
                self.take();
                let rel = self.take_i8()? as i32;
                Some((2, format!("jmp {}", self.rel_target(rel, off + 2))))
            }
            0xE8 => {
                self.take();
                let rel = self.take_i32()?;
                let sym = self.calls.get(&(off + 1)).cloned();
                let target = sym.unwrap_or_else(|| self.rel_target(rel, off + 5));
                Some((5, format!("call {target}")))
            }
            0xE9 => {
                self.take();
                let rel = self.take_i32()?;
                Some((5, format!("jmp {}", self.rel_target(rel, off + 5))))
            }
            0x31 if self.code.get(self.pos) == Some(&0xC0) => {
                self.take(); self.take();
                Some((2, "xor %eax, %eax".into()))
            }
            0x85 if self.code.get(self.pos) == Some(&0xC0) => {
                self.take(); self.take();
                Some((2, "test %eax, %eax".into()))
            }
            0xB8..=0xBF => {
                let reg = b - 0xB8;
                self.take();
                let imm = self.take_i32()?;
                Some((5, format!("mov ${imm}, {}", REG32[reg as usize])))
            }
            0xB0..=0xB7 => {
                let reg = b - 0xB0;
                self.take();
                let imm = self.take()? as i32;
                Some((2, format!("mov ${imm}, {}", REG8[reg as usize])))
            }
            0x0F => self.decode_two_byte(start),
            0x41 | 0x49 | 0x4C | 0x48 => self.decode_rex(start),
            _ => self.decode_legacy(start),
        }
    }

    fn decode_two_byte(&mut self, start: usize) -> Option<(usize, String)> {
        let _ = self.take();
        let op = self.take()?;
        match op {
            0x05 => Some((self.pos - start, "syscall".into())),
            0x0B => Some((self.pos - start, "ud2".into())),
            0x84..=0x8F => {
                let cc = match op - 0x80 {
                    0x04 => "je", 0x05 => "jne", 0x0C => "jl", 0x0D => "jge",
                    0x0E => "jle", 0x0F => "jg", 0x02 => "jb", 0x03 => "jae",
                    _ => "jcc",
                };
                let rel = self.take_i32()?;
                let end = start as u64 + (self.pos - start + 4) as u64;
                Some((self.pos - start, format!("{cc} {}", self.rel_target(rel, end))))
            }
            0xB6 | 0xB7 | 0xBE | 0xBF | 0xAF => {
                self.pos = start;
                self.decode_rex(start)
            }
            _ => { self.pos = start + 1; None }
        }
    }

    fn decode_rex(&mut self, start: usize) -> Option<(usize, String)> {
        let rex = self.parse_rex();
        let width = if rex.w { 8 } else { 4 };
        let op = self.take()?;
        if (0x50..=0x5F).contains(&op) {
            let reg = (op & 7) as usize + if rex.b { 8 } else { 0 };
            let mn = if op >= 0x58 { "pop" } else { "push" };
            return Some((self.pos - start, format!("{mn} {}", REG64[reg])));
        }
        match op {
            0x89 => {
                let (src, dst) = self.modrm(&rex, width)?;
                Some((self.pos - start, format!("mov {src}, {dst}")))
            }
            0x8B => {
                let (dst, src) = self.modrm(&rex, width)?;
                Some((self.pos - start, format!("mov {src}, {dst}")))
            }
            0x8D => {
                let (dst, src) = self.modrm(&rex, width)?;
                Some((self.pos - start, format!("lea {dst}, {src}")))
            }
            0x01 | 0x09 | 0x21 | 0x29 | 0x31 => {
                let (dst, src) = self.modrm(&rex, width)?;
                let mn = match op { 0x01 => "add", 0x09 => "or", 0x21 => "and", 0x29 => "sub", 0x31 => "xor", _ => "?" };
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            }
            0x39 | 0x85 => {
                let (a, b) = self.modrm(&rex, width)?;
                let mn = if op == 0x39 { "cmp" } else { "test" };
                Some((self.pos - start, format!("{mn} {b}, {a}")))
            }
            0x81 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg { 0 => "add", 5 => "sub", 7 => "cmp", _ => "alu" };
                let (_, mem) = self.modrm(&rex, width)?;
                let imm = self.take_i32()?;
                Some((self.pos - start, format!("{mn} ${imm}, {mem}")))
            }
            0x83 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg { 0 => "add", 5 => "sub", 7 => "cmp", _ => "alu" };
                let (_, mem) = self.modrm(&rex, width)?;
                let imm = self.take_i8()? as i32;
                Some((self.pos - start, format!("{mn} ${imm}, {mem}")))
            }
            0xC7 => {
                let (_, mem) = self.modrm(&rex, 4)?;
                let imm = self.take_i32()?;
                Some((self.pos - start, format!("mov ${imm}, {mem}")))
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
                Some((self.pos - start, format!("mov ${imm}, {r}")))
            }
            0x98 => Some((self.pos - start, "cltq".into())),
            0x99 => Some((self.pos - start, "cqo".into())),
            0xFF => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let (_, mem) = self.modrm(&rex, width)?;
                let mn = match reg { 2 => "call", 4 => "jmp", 6 => "push", _ => "ff" };
                Some((self.pos - start, format!("{mn} {mem}")))
            }
            0x05..=0x3D => {
                self.pos = start;
                None
            }
            _ => { self.pos = start + 1; None }
        }
    }

    fn decode_legacy(&mut self, start: usize) -> Option<(usize, String)> {
        let rex = Rex { w: false, r: false, x: false, b: false };
        let op = self.take()?;
        match op {
            0x01 | 0x09 | 0x21 | 0x29 | 0x31 => {
                let (dst, src) = self.modrm(&rex, 4)?;
                let mn = match op { 0x01 => "add", 0x09 => "or", 0x21 => "and", 0x29 => "sub", 0x31 => "xor", _ => "?" };
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            }
            _ => { self.pos = start + 1; None }
        }
    }
}

pub fn disassemble_range(
    code: &[u8],
    start: usize,
    end: usize,
    labels: &BTreeMap<u64, Vec<&str>>,
    directives: &BTreeMap<u64, Vec<&str>>,
    calls: &HashMap<u64, String>,
    data: &HashMap<u64, String>,
) -> String {
    let mut out = String::new();
    let mut pos = start;
    while pos < end && pos < code.len() {
        let off = pos as u64;
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
        let mut dec = Dec { code, pos, calls, data };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_mov_imm_and_ret() {
        let code = [0xB8, 0x05, 0x00, 0x00, 0x00, 0xC3];
        let out = disassemble_range(&code, 0, code.len(), &BTreeMap::new(), &BTreeMap::new(), &HashMap::new(), &HashMap::new());
        assert!(out.contains("mov $5, %eax"));
        assert!(out.contains("ret"));
    }
}
