// src/codegen/disasm.rs - x86-64 机器码 → AT&T 汇编文本（-S 输出）

use std::collections::{BTreeMap, HashMap};

const REG8: [&str; 16] = [
    "%al", "%cl", "%dl", "%bl", "%spl", "%bpl", "%sil", "%dil", "%r8b", "%r9b", "%r10b", "%r11b",
    "%r12b", "%r13b", "%r14b", "%r15b",
];
/// **没有 REX 前缀时**字段 4..7 的名字。%spl/%bpl/%sil/%dil 只在「存在 REX」时才可达，
/// 无 REX 的 4..7 是遗留的 %ah/%ch/%dh/%bh。这张表是「该发 REX 却没发」这类缺陷的唯一
/// 探测器：少了它，`80 c7 10`（真机 = `add $0x10,%bh`）会被读成 `add $0x10,%dil`，
/// 往返校验正好假绿 —— 而 objdump 会老实报出 %bh。
const REG8_LEGACY: [&str; 8] = ["%al", "%cl", "%dl", "%bl", "%ah", "%ch", "%dh", "%bh"];
const REG16: [&str; 16] = [
    "%ax", "%cx", "%dx", "%bx", "%sp", "%bp", "%si", "%di", "%r8w", "%r9w", "%r10w", "%r11w",
    "%r12w", "%r13w", "%r14w", "%r15w",
];
const REG32: [&str; 16] = [
    "%eax", "%ecx", "%edx", "%ebx", "%esp", "%ebp", "%esi", "%edi", "%r8d", "%r9d", "%r10d",
    "%r11d", "%r12d", "%r13d", "%r14d", "%r15d",
];
const REG64: [&str; 16] = [
    "%rax", "%rcx", "%rdx", "%rbx", "%rsp", "%rbp", "%rsi", "%rdi", "%r8", "%r9", "%r10", "%r11",
    "%r12", "%r13", "%r14", "%r15",
];
const XMM: [&str; 16] = [
    "%xmm0", "%xmm1", "%xmm2", "%xmm3", "%xmm4", "%xmm5", "%xmm6", "%xmm7", "%xmm8", "%xmm9",
    "%xmm10", "%xmm11", "%xmm12", "%xmm13", "%xmm14", "%xmm15",
];

#[derive(Default, Clone, Copy)]
struct Rex {
    w: bool,
    r: bool,
    x: bool,
    b: bool,
    /// 是否真的存在 REX 字节。与上面四个位**正交**：全 0 的 `0x40` 也是合法 REX，它
    /// 不搬任何字段编号，只把 8 位上下文里字段 4..7 的释义从 %ah..%bh 切到 %spl..%dil。
    /// 因此「为 %sil 置 REX.B」是错的（那会把字段搬到 %r14b），「不发 REX 而写 %sil」
    /// 同样是错的（那是 %dh）；这两类缺陷都只能靠这一位区分。
    present: bool,
}

#[derive(Default)]
struct Prefixes {
    lock: bool,
    rep: bool,
    repne: bool,
    o16: bool,
}

/// 条件码后缀, 索引 = 0x0F 后操作码低 4 位 (setcc/jcc 共用).
const CC_NAMES: [&str; 16] = [
    "o", "no", "b", "ae", "e", "ne", "be", "a", "s", "ns", "p", "np", "l", "ge", "le", "g",
];

struct Dec<'a> {
    code: &'a [u8],
    pos: usize,
    base: u64,
    calls: &'a HashMap<u64, String>,
    data: &'a HashMap<u64, String>,
    /// 「地址 → 标号名」命名表, 与 `disassemble_range` 打印标号**定义**用的是
    /// 同一份数据。缺陷 C 的根因就是定义侧查这张表、引用侧伪造名字, 两侧
    /// 永远对不上, 导致 `-S` 产物交给 GAS 后生成针对未定义 `.L*` 的重定位。
    labels: &'a BTreeMap<u64, Vec<&'a str>>,
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

    /// `66 B8 iw` / `66 C7 /0 iw` 这类 16 位立即数。不读它就不知道指令有多长，
    /// 后面的字节会被当成新指令的首字节 —— 整份反汇编从此错位。
    fn take_i16(&mut self) -> Option<i16> {
        if self.pos + 2 > self.code.len() {
            return None;
        }
        let v = i16::from_le_bytes(self.code[self.pos..self.pos + 2].try_into().ok()?);
        self.pos += 2;
        Some(v)
    }

    fn abs_off(&self) -> u64 {
        self.base + self.pos as u64
    }

    fn reg(&self, rex: &Rex, idx: u8, width: u8) -> &'static str {
        // idx 已由调用方按 REX.R / REX.B / REX.X 扩展为完整 0..15 索引.
        // 此处不能再叠加 REX.B, 否则会把 reg 字段 (用 REX.R 扩展) 错当成 r/m 字段
        // (用 REX.B 扩展), 导致反汇编里 rax 误显示为 r8, r8 误显示为 rax 等.
        let i = (idx & 15) as usize;
        match width {
            // 8 位命名取决于「有没有 REX」，而不只是 REX 的位值（见 REG8_LEGACY）.
            1 if !rex.present && i < 8 => REG8_LEGACY[i],
            1 => REG8[i],
            2 => REG16[i],
            4 => REG32[i],
            _ => REG64[i],
        }
    }

    fn xmm(&self, rex: &Rex, idx: u8) -> &'static str {
        XMM[(idx & 7) as usize + if rex.r { 8 } else { 0 }]
    }

    /// Compute effective operand width considering 0x66 prefix and REX.W
    fn eff_width(&self, rex: &Rex, pfx: &Prefixes) -> u8 {
        if rex.w {
            8
        } else if pfx.o16 {
            2
        } else {
            4
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
                present: true,
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
        let reg_name = self
            .reg(rex, reg | if rex.r { 8 } else { 0 }, width)
            .to_string();
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

    /// modrm，reg 字段为 XMM，r/m 字段为 GP 寄存器或内存（按 gp_width 命名）。
    /// 用于 movd/movq (0F 6E/7E) 的 XMM/GP 混合编码。
    fn modrm_xmm_gp(&mut self, rex: &Rex, gp_width: u8) -> Option<(String, String)> {
        let modrm = self.take()?;
        let mod_ = modrm >> 6;
        let reg = (modrm >> 3) & 7;
        let rm = modrm & 7;
        let xmm_name = self.xmm(rex, reg).to_string();
        let gp = if mod_ == 3 {
            self.reg(rex, rm | if rex.b { 8 } else { 0 }, gp_width)
                .to_string()
        } else {
            self.decode_mem(rex, mod_, rm, gp_width)?
        };
        Some((xmm_name, gp))
    }

    /// modrm，其中 reg 字段是宽目标 (dst_width), r/m 是窄源:
    /// mod=3 时用 sub_width (8/16 位) 寄存器名, 否则显示内存操作数.
    /// 用于 movzx/movsx (0F B6/B7/BE/BF) 的 reg=宽, r/m=窄 非对称编码.
    fn modrm_ext(&mut self, rex: &Rex, sub_width: u8, dst_width: u8) -> Option<(String, String)> {
        let modrm = self.take()?;
        let mod_ = modrm >> 6;
        let reg = (modrm >> 3) & 7;
        let rm = modrm & 7;
        let dst = self
            .reg(rex, reg | if rex.r { 8 } else { 0 }, dst_width)
            .to_string();
        let src = if mod_ == 3 {
            self.reg(rex, rm | if rex.b { 8 } else { 0 }, sub_width)
                .to_string()
        } else {
            self.decode_mem(rex, mod_, rm, sub_width)?
        };
        Some((src, dst))
    }

    fn decode_mem(&mut self, rex: &Rex, mod_: u8, rm: u8, width: u8) -> Option<String> {
        let rm_ext = rm | if rex.b { 8 } else { 0 };
        if mod_ == 3 {
            return Some(self.reg(rex, rm_ext, width).to_string());
        }
        // 64 位长模式下 base/index 恒为 64 位寻址寄存器,
        // 不继承操作数宽度 (否则 movzx/movsx 的窄源显示成 %al 而非 (%rax))
        const ADDR_W: u8 = 8;
        let mut disp = 0i32;
        let base: String;
        let mut index = String::new();
        let mut scale = 1u32;
        let mut rip_disp_off: Option<u64> = None; // disp32 起始偏移 (用于 data 符号查找)
        if rm == 4 {
            let sib = self.take()?;
            let scale_bits = sib >> 6;
            let index_reg = (sib >> 3) & 7;
            let base_reg = sib & 7;
            scale = 1u32 << scale_bits;
            if index_reg != 4 {
                index = self
                    .reg(rex, index_reg | if rex.x { 8 } else { 0 }, ADDR_W)
                    .to_string();
            }
            if mod_ == 0 && base_reg == 5 {
                rip_disp_off = Some(self.abs_off());
                disp = self.take_i32()?;
                base = "%rip".to_string();
            } else {
                base = self
                    .reg(rex, base_reg | if rex.b { 8 } else { 0 }, ADDR_W)
                    .to_string();
            }
        } else if mod_ == 0 && rm == 5 {
            rip_disp_off = Some(self.abs_off());
            disp = self.take_i32()?;
            base = "%rip".to_string();
        } else {
            base = self.reg(rex, rm_ext, ADDR_W).to_string();
        }
        match mod_ {
            0 => {},
            1 => disp = self.take_i8()? as i32,
            2 => disp = self.take_i32()?,
            _ => {},
        }
        // RIP-relative 时, data 符号在 disp32 起始位置; 其他情况在指令末尾
        let lookup_off = rip_disp_off.unwrap_or_else(|| self.abs_off());
        let sym = self.data.get(&lookup_off).cloned();
        let mut inner = String::new();
        if let Some(s) = sym {
            inner.push_str(&s);
        } else if disp != 0 {
            inner.push_str(&format!("{disp}"));
        }
        // 统一 AT&T 内存操作数格式: inner(base[,index,scale])
        // 之前的实现在 disp=0 时返回裸寄存器名 (丢掉括号), 把
        // movzbq (%rax),%rax 变成 movzbq %rax,%rax (语义损坏);
        // 带 index 时又写成 "idx,scale(base)" 缺左括号.
        if !index.is_empty() {
            inner.push_str(&format!(",{index},{scale}"));
        }
        Some(format!("{inner}({base})"))
    }

    /// 跳转 / 调用目标的唯一解析路径 (`jmp`、`jcc`、`call` 共用)。
    ///
    /// `site` 是 rel32/rel8 字段自身的地址 (即 `calls` 表的键), `insn_end` 是下一条
    /// 指令地址。优先级不可颠倒:
    ///
    /// 1. `calls`: 发射期就记下来的**符号名**。经重定位解析的 `call`/`jmp` 在字节里
    ///    只写了 addend 占位 (常为 0), 按地址算出来的「目标」是本指令的下一条,
    ///    那一处恰好有自己的标号 —— 先查地址会把 `call foo` 渲染成 `call .Lnext`。
    /// 2. `labels`: 与标号定义同一张表, 所以引用名与定义名不可能漂移。
    /// 3. 两者都查不到 -> [`Self::unresolved_target`], 不得伪造 GAS 合法名。
    fn jump_target(&self, site: u64, rel: i32, insn_end: u64) -> String {
        if let Some(sym) = self.calls.get(&site) {
            return sym.clone();
        }
        match insn_end.checked_add_signed(rel as i64) {
            Some(target) => match self.labels.get(&target).and_then(|names| names.first()) {
                Some(name) => (*name).to_string(),
                None => Self::unresolved_target(target),
            },
            // 回绕的目标地址不可能是本 TU 内的合法标号, 同样不得臆造名字。
            None => Self::unresolved_target(insn_end),
        }
    }

    /// 未解析目标的渲染。
    ///
    /// 旧实现是 `format!(".L{}", addr)`: 造出一个**语法合法但从未定义**的标号,
    /// GAS 把它当成未定义全局符号, 安静地发一条 `R_X86_64_PLT32/.L126` 重定位
    /// —— 产出的 `.o` 能链、能装、跑必挂。`<>` 不是 GAS 表达式合法 token,
    /// 因此汇编器会直接报错终止: 后端只能保证「不静默」, 不能保证猜对名字。
    fn unresolved_target(target: u64) -> String {
        format!("<dcc-unresolved-target-0x{:x}>", target)
    }

    fn prefix_str(pfx: &Prefixes) -> String {
        let mut parts = Vec::new();
        if pfx.lock {
            parts.push("lock");
        }
        if pfx.repne {
            parts.push("repne");
        } else if pfx.rep {
            parts.push("rep");
        }
        // 0x66 操作数大小前缀通过 16 位寄存器名（如 %ax）隐式表达，
        // 显式输出 data16 会导致 GAS 把它当作重复前缀而报错。
        if parts.is_empty() {
            String::new()
        } else {
            format!("{} ", parts.join(" "))
        }
    }

    fn finish(&self, start: usize, pfx: &Prefixes, body: &str) -> (usize, String) {
        let text = if pfx.lock || pfx.rep || pfx.repne {
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
                Some(0xF0) => {
                    pfx.lock = true;
                    self.take();
                },
                Some(0xF3) => {
                    pfx.rep = true;
                    self.take();
                },
                Some(0xF2) => {
                    pfx.repne = true;
                    self.take();
                },
                Some(0x66) => {
                    pfx.o16 = true;
                    self.take();
                },
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
            return Some((
                self.pos - start,
                format!("{}push {}", Self::prefix_str(&pfx), REG64[(b - 0x50) as usize]),
            ));
        }
        if (0x58..=0x5F).contains(&b) {
            self.take();
            return Some((
                self.pos - start,
                format!("{}pop {}", Self::prefix_str(&pfx), REG64[(b - 0x58) as usize]),
            ));
        }

        match b {
            0x90 if pfx.rep => {
                self.take();
                Some((self.pos - start, "pause".into()))
            },
            0x90 => {
                self.take();
                Some((1, "nop".into()))
            },
            0xC3 => {
                self.take();
                Some((self.pos - start, format!("{}ret", Self::prefix_str(&pfx))))
            },
            0xC9 => {
                self.take();
                Some((self.pos - start, "leave".into()))
            },
            0x55 => {
                self.take();
                Some((self.pos - start, "push %rbp".into()))
            },
            0x5D => {
                self.take();
                Some((self.pos - start, "pop %rbp".into()))
            },
            0x99 => {
                self.take();
                Some((self.pos - start, "cltd".into()))
            },
            0x9B => {
                self.take();
                Some((self.pos - start, "fwait".into()))
            },
            0xA4 if pfx.rep => {
                self.take();
                Some((self.pos - start, "rep movsb".into()))
            },
            0xA5 if pfx.rep => {
                self.take();
                let rex = self.parse_rex();
                let mn = if rex.w { "rep movsq" } else { "rep movsl" };
                Some((self.pos - start, mn.into()))
            },
            0xEB => {
                self.take();
                let rel = self.take_i8()? as i32;
                Some((
                    self.pos - start,
                    format!(
                        "{}jmp {}",
                        Self::prefix_str(&pfx),
                        self.jump_target(off + 1, rel, off + 2)
                    ),
                ))
            },
            0xE8 => {
                self.take();
                let rel = self.take_i32()?;
                let target = self.jump_target(off + 1, rel, off + 5);
                Some((self.pos - start, format!("{}call {target}", Self::prefix_str(&pfx))))
            },
            0xE9 => {
                self.take();
                let rel = self.take_i32()?;
                let target = self.jump_target(off + 1, rel, off + 5);
                Some((self.pos - start, format!("{}jmp {target}", Self::prefix_str(&pfx))))
            },
            0x31 if self.code.get(self.pos) == Some(&0xC0) => {
                self.take();
                self.take();
                Some((self.pos - start, "xor %eax, %eax".into()))
            },
            0x85 if self.code.get(self.pos) == Some(&0xC0) => {
                self.take();
                self.take();
                Some((self.pos - start, "test %eax, %eax".into()))
            },
            0xB8..=0xBF => {
                let reg = b - 0xB8;
                self.take();
                // 0x66 下立即数是 imm16 且目标是 16 位寄存器（`66 B8 iw` = mov $imm,%ax）。
                // 按 imm32 读会把长度算错，后缀的两个字节被当成新指令首字节。
                if pfx.o16 {
                    let imm = self.take_i16()?;
                    Some((
                        self.pos - start,
                        format!("mov ${imm}, {}", REG16[reg as usize]),
                    ))
                } else {
                    let imm = self.take_i32()?;
                    Some((
                        self.pos - start,
                        format!("mov ${imm}, {}", REG32[reg as usize]),
                    ))
                }
            },
            0xB0..=0xB7 => {
                // 走到这里说明没有 REX（有则整体交给 decode_rex），故 4..7 是 %ah..%bh.
                let reg = b - 0xB0;
                self.take();
                let imm = self.take()? as i32;
                Some((self.pos - start, format!("mov ${imm}, {}", REG8_LEGACY[reg as usize])))
            },
            0x0F => self.decode_two_byte(start, &pfx),
            0x40..=0x4F => self.decode_rex(start, &pfx),
            // x87 FPU instructions (0xD8-0xDF)
            0xD8..=0xDF => {
                let opcode = self.take()?; // 先取 opcode
                self.decode_x87(start, opcode)
            },
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
            },
            0x84..=0x8F => {
                let cc = match op - 0x80 {
                    0x04 => "je",
                    0x05 => "jne",
                    0x0C => "jl",
                    0x0D => "jge",
                    0x0E => "jle",
                    0x0F => "jg",
                    0x02 => "jb",
                    0x03 => "jae",
                    0x08 => "js",
                    0x09 => "jns",
                    _ => "jcc",
                };
                let rel = self.take_i32()?;
                let end = off + (self.pos - start) as u64;
                Some((
                    self.pos - start,
                    format!("{cc} {}", self.jump_target(off + 2, rel, end)),
                ))
            },
            0xAE if self.peek() == Some(0xF0) => {
                self.take();
                Some((self.pos - start, "mfence".into()))
            },
            0xB0 | 0xB1 => {
                let rex = self.parse_rex();
                let (src, dst) = self.modrm_gp(&rex, if rex.w { 8 } else { 4 })?;
                Some(self.finish(start, pfx, &format!("cmpxchg {src}, {dst}")))
            },
            // cmovcc r, r/m (non-REX)
            0x40..=0x4F => {
                let rex = self.parse_rex();
                let cc = CC_NAMES[(op - 0x40) as usize];
                let (src, dst) = self.modrm_gp(&rex, 4)?;
                Some(self.finish(start, pfx, &format!("cmov{cc} {src}, {dst}")))
            },
            0xC1 => {
                let rex = self.parse_rex();
                let (src, dst) = self.modrm_gp(&rex, if rex.w { 8 } else { 4 })?;
                Some(self.finish(start, pfx, &format!("xadd {src}, {dst}")))
            },
            // setcc r/m8 (无 REX 版本, dcc emit 0F 9x C0)
            0x90..=0x9F => {
                let rex = self.parse_rex();
                let modrm = self.take()?;
                if modrm >> 6 != 3 {
                    self.pos = start + 1;
                    return None;
                }
                let rm = modrm & 7;
                let r = self.reg(&rex, rm | if rex.b { 8 } else { 0 }, 1);
                Some(self.finish(start, pfx, &format!("set{} {r}", CC_NAMES[(op - 0x90) as usize])))
            },
            0x10 | 0x11 | 0x2A | 0x2C | 0x5A | 0x6E | 0x7E => {
                self.pos = start;
                self.decode_sse(start, pfx)
            },
            0xB6 | 0xB7 | 0xBE | 0xBF => {
                // movzx/movsx (无 REX): reg 字段是宽目标 (32 位), r/m 是窄源
                let rex = self.parse_rex();
                let (src, dst) =
                    self.modrm_ext(&rex, if op == 0xB6 || op == 0xBE { 1 } else { 2 }, 4)?;
                let mn = match (op, rex.w) {
                    (0xB6, true) => "movzbq",
                    (0xB6, false) => "movzbl",
                    (0xB7, true) => "movzwq",
                    (0xB7, false) => "movzwl",
                    (0xBE, true) => "movsbq",
                    (0xBE, false) => "movsbl",
                    (0xBF, true) => "movswq",
                    _ => "movswl",
                };
                Some(self.finish(start, pfx, &format!("{mn} {src}, {dst}")))
            },
            0xAF => {
                let rex = self.parse_rex();
                let (src, dst) = self.modrm_gp(&rex, if rex.w { 8 } else { 4 })?;
                Some(self.finish(start, pfx, &format!("imul {src}, {dst}")))
            },
            _ => {
                let len = decode_unknown_len(self.code, start);
                self.pos = start + len;
                // Output the full unknown instruction as .byte sequence
                let bytes: Vec<String> = self.code[start..self.pos]
                    .iter()
                    .map(|b| format!("0x{:02x}", b))
                    .collect();
                Some((len, format!("# UNKNOWN {}", bytes.join(", "))))
            },
        }
    }

    fn decode_sse(&mut self, start: usize, _pfx: &Prefixes) -> Option<(usize, String)> {
        let pfx = self.take_prefixes();
        let rex = self.parse_rex();
        let _ = self.take(); // 0x0F
        let op = self.take()?;
        // 0x66/0xF2/0xF3 在此是 SSE 指令编码的一部分，不是独立前缀助记符。
        let scalar = if pfx.repne {
            "sd"
        } else if pfx.rep {
            "ss"
        } else {
            "sd"
        };
        match op {
            0x10 => {
                let (dst, src) = self.modrm_xmm(&rex)?;
                let mn = if scalar == "ss" { "movss" } else { "movsd" };
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            },
            0x11 => {
                let (src, dst) = self.modrm_xmm(&rex)?;
                let mn = if scalar == "ss" { "movss" } else { "movsd" };
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            },
            0x2A => {
                let (xmm, src) = self.modrm_xmm(&rex)?;
                let mn = if scalar == "ss" { "cvtsi2ss" } else { "cvtsi2sd" };
                Some((self.pos - start, format!("{mn} {src}, {xmm}")))
            },
            0x2C => {
                let (dst, xmm) = self.modrm_xmm(&rex)?;
                let mn = if scalar == "ss" { "cvttss2si" } else { "cvttsd2si" };
                Some((self.pos - start, format!("{mn} {xmm}, {dst}")))
            },
            0x5A => {
                let (dst, src) = self.modrm_xmm(&rex)?;
                let mn = if scalar == "ss" { "cvtss2sd" } else { "cvtsd2ss" };
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            },
            0x6E | 0x7E => {
                // 66 0F 6E /r: movd r/m32, xmm (REX.W 时为 movq)
                // 66 0F 7E /r: movd xmm, r/m32 (REX.W 时为 movq)
                let gp_width = if rex.w { 8 } else { 4 };
                let (xmm, gp) = self.modrm_xmm_gp(&rex, gp_width)?;
                let mn = if rex.w { "movq" } else { "movd" };
                if op == 0x6E {
                    Some((self.pos - start, format!("{mn} {gp}, {xmm}")))
                } else {
                    Some((self.pos - start, format!("{mn} {xmm}, {gp}")))
                }
            },
            _ => {
                let len = decode_unknown_len(self.code, start);
                self.pos = start + len;
                // Output the full unknown instruction as .byte sequence
                let bytes: Vec<String> = self.code[start..self.pos]
                    .iter()
                    .map(|b| format!("0x{:02x}", b))
                    .collect();
                Some((len, format!("# UNKNOWN {}", bytes.join(", "))))
            },
        }
    }

    fn decode_rex(&mut self, start: usize, pfx: &Prefixes) -> Option<(usize, String)> {
        let rex = self.parse_rex();
        let width = self.eff_width(&rex, pfx);
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
            // --- MOV ---
            0x89 => {
                let (src, dst) = self.modrm_gp(&rex, width)?;
                Some(self.finish(start, pfx, &format!("mov {src}, {dst}")))
            },
            0x8B => {
                let (dst, src) = self.modrm_gp(&rex, width)?;
                Some(self.finish(start, pfx, &format!("mov {src}, {dst}")))
            },
            0x8A => {
                let (dst, src) = self.modrm_gp(&rex, 1)?;
                Some(self.finish(start, pfx, &format!("mov {src}, {dst}")))
            },
            0x88 => {
                let (src, dst) = self.modrm_gp(&rex, 1)?;
                Some(self.finish(start, pfx, &format!("mov {src}, {dst}")))
            },
            0xC6 => {
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                let imm = self.take()? as i32;
                Some(self.finish(start, pfx, &format!("mov ${imm}, {mem}")))
            },
            // --- XCHG / LEA ---
            0x87 => {
                let (src, dst) = self.modrm_gp(&rex, width)?;
                Some(self.finish(start, pfx, &format!("xchg {src}, {dst}")))
            },
            0x8D => {
                let (dst, src) = self.modrm_gp(&rex, width)?;
                Some(self.finish(start, pfx, &format!("lea {src}, {dst}")))
            },
            // --- ALU r/m, r (reg=src, r/m=dst) ---
            0x01 | 0x09 | 0x21 | 0x29 | 0x31 => {
                let (src, dst) = self.modrm_gp(&rex, width)?;
                let mn = match op {
                    0x01 => "add",
                    0x09 => "or",
                    0x21 => "and",
                    0x29 => "sub",
                    0x31 => "xor",
                    _ => "?",
                };
                Some(self.finish(start, pfx, &format!("{mn} {src}, {dst}")))
            },
            // --- ALU r, r/m (reg=dst, r/m=src) ---
            0x03 | 0x0B | 0x23 | 0x2B | 0x33 | 0x3B => {
                let (dst, src) = self.modrm_gp(&rex, width)?;
                let mn = match op {
                    0x03 => "add",
                    0x0B => "or",
                    0x23 => "and",
                    0x2B => "sub",
                    0x33 => "xor",
                    0x3B => "cmp",
                    _ => "?",
                };
                Some(self.finish(start, pfx, &format!("{mn} {src}, {dst}")))
            },
            // --- ALU 8-bit r/m, r ---
            0x00 | 0x08 | 0x20 | 0x28 | 0x30 => {
                let (src, dst) = self.modrm_gp(&rex, 1)?;
                let mn = match op {
                    0x00 => "add",
                    0x08 => "or",
                    0x20 => "and",
                    0x28 => "sub",
                    0x30 => "xor",
                    _ => "?",
                };
                Some(self.finish(start, pfx, &format!("{mn} {src}, {dst}")))
            },
            // --- ALU 8-bit r, r/m ---
            0x02 | 0x0A | 0x22 | 0x2A | 0x32 | 0x3A => {
                let (dst, src) = self.modrm_gp(&rex, 1)?;
                let mn = match op {
                    0x02 => "add",
                    0x0A => "or",
                    0x22 => "and",
                    0x2A => "sub",
                    0x32 => "xor",
                    0x3A => "cmp",
                    _ => "?",
                };
                Some(self.finish(start, pfx, &format!("{mn} {src}, {dst}")))
            },
            // --- ALU rax/eax, imm32 ---
            0x05 | 0x0D | 0x25 | 0x2D | 0x35 | 0x3D => {
                let mn = match op {
                    0x05 => "add",
                    0x0D => "or",
                    0x25 => "and",
                    0x2D => "sub",
                    0x35 => "xor",
                    0x3D => "cmp",
                    _ => "?",
                };
                let r = self.reg(&rex, if rex.b { 8 } else { 0 }, width);
                let imm = self.take_i32()?;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {r}")))
            },
            // --- CMP/TEST r/m, r ---
            0x39 | 0x85 => {
                let (a, b) = self.modrm_gp(&rex, width)?;
                let mn = if op == 0x39 { "cmp" } else { "test" };
                Some(self.finish(start, pfx, &format!("{mn} {b}, {a}")))
            },
            // --- TEST 8-bit ---
            0x84 => {
                let (a, b) = self.modrm_gp(&rex, 1)?;
                Some(self.finish(start, pfx, &format!("test {b}, {a}")))
            },
            // --- XCHG 8-bit ---
            0x86 => {
                let (src, dst) = self.modrm_gp(&rex, 1)?;
                Some(self.finish(start, pfx, &format!("xchg {src}, {dst}")))
            },
            // --- ALU r/m, imm32 ---
            0x81 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "add",
                    1 => "or",
                    2 => "adc",
                    3 => "sbb",
                    4 => "and",
                    5 => "sub",
                    6 => "xor",
                    7 => "cmp",
                    _ => "alu",
                };
                let (_, mem) = self.modrm_gp(&rex, width)?;
                let imm = self.take_i32()?;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {mem}")))
            },
            // --- ALU r/m, imm8 ---
            0x83 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "add",
                    1 => "or",
                    2 => "adc",
                    3 => "sbb",
                    4 => "and",
                    5 => "sub",
                    6 => "xor",
                    7 => "cmp",
                    _ => "alu",
                };
                let (_, mem) = self.modrm_gp(&rex, width)?;
                let imm = self.take_i8()? as i32;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {mem}")))
            },
            // --- ALU r/m8, imm8 ---
            0x80 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "add",
                    1 => "or",
                    2 => "adc",
                    3 => "sbb",
                    4 => "and",
                    5 => "sub",
                    6 => "xor",
                    7 => "cmp",
                    _ => "alu",
                };
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                let imm = self.take()? as i32;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {mem}")))
            },
            // --- MOV r/m, imm32 ---
            0xC7 => {
                let (_, mem) = self.modrm_gp(&rex, width)?;
                let imm = self.take_i32()?;
                Some(self.finish(start, pfx, &format!("mov ${imm}, {mem}")))
            },
            // --- MOV r, imm ---
            0xB8..=0xBF => {
                // `B8+rd` 的寄存器编号在 **opcode 低 3 位**，扩展它的是 REX.B 而不是 REX.R.
                let reg = op - 0xB8;
                let r = self.reg(&rex, reg | if rex.b { 8 } else { 0 }, width);
                let imm = if rex.w {
                    let v = i64::from_le_bytes(self.code[self.pos..self.pos + 8].try_into().ok()?);
                    self.pos += 8;
                    v
                } else if pfx.o16 {
                    // `66 41 B8 iw` = mov $imm,%r8w：16 位立即数只有 2 字节，
                    // 按 imm32 读会超出指令边界，整份反汇编错位。
                    i64::from(self.take_i16()?)
                } else {
                    self.take_i32()? as i64
                };
                Some(self.finish(start, pfx, &format!("mov ${imm}, {r}")))
            },
            // --- MOV r8, imm8 ---
            0xB0..=0xB7 => {
                let reg = op - 0xB0;
                let r = self.reg(&rex, reg | if rex.b { 8 } else { 0 }, 1);
                let imm = self.take()? as i32;
                Some(self.finish(start, pfx, &format!("mov ${imm}, {r}")))
            },
            // --- MOVSXD r64, r/m32 ---
            0x63 => {
                let modrm = self.take()?;
                let mod_ = modrm >> 6;
                let reg = (modrm >> 3) & 7;
                let rm = modrm & 7;
                let dst = self
                    .reg(&rex, reg | if rex.r { 8 } else { 0 }, width)
                    .to_string();
                let src = self.decode_mem(&rex, mod_, rm | if rex.b { 8 } else { 0 }, 4)?;
                Some(self.finish(start, pfx, &format!("movsxd {src}, {dst}")))
            },
            // --- IMUL r, r/m, imm32 ---
            0x69 => {
                let (src, dst) = self.modrm_gp(&rex, width)?;
                let imm = self.take_i32()?;
                Some(self.finish(start, pfx, &format!("imul ${imm}, {src}, {dst}")))
            },
            // --- IMUL r, r/m, imm8 ---
            0x6B => {
                let (src, dst) = self.modrm_gp(&rex, width)?;
                let imm = self.take_i8()? as i32;
                Some(self.finish(start, pfx, &format!("imul ${imm}, {src}, {dst}")))
            },
            // --- Shift/Rotate r/m, imm8 ---
            0xC0 | 0xC1 => {
                let sw = if op == 0xC1 { width } else { 1 };
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "rol",
                    1 => "ror",
                    2 => "rcl",
                    3 => "rcr",
                    4 => "shl",
                    5 => "shr",
                    6 => "sal",
                    7 => "sar",
                    _ => "shift",
                };
                let (_, mem) = self.modrm_gp(&rex, sw)?;
                let imm = self.take()? as i32;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {mem}")))
            },
            // --- Shift/Rotate r/m, 1 ---
            0xD0 | 0xD1 => {
                let sw = if op == 0xD1 { width } else { 1 };
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "rol",
                    1 => "ror",
                    2 => "rcl",
                    3 => "rcr",
                    4 => "shl",
                    5 => "shr",
                    6 => "sal",
                    7 => "sar",
                    _ => "shift",
                };
                let (_, mem) = self.modrm_gp(&rex, sw)?;
                Some(self.finish(start, pfx, &format!("{mn} $1, {mem}")))
            },
            // --- Shift/Rotate r/m, cl ---
            0xD2 | 0xD3 => {
                let sw = if op == 0xD3 { width } else { 1 };
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "rol",
                    1 => "ror",
                    2 => "rcl",
                    3 => "rcr",
                    4 => "shl",
                    5 => "shr",
                    6 => "sal",
                    7 => "sar",
                    _ => "shift",
                };
                let (_, mem) = self.modrm_gp(&rex, sw)?;
                Some(self.finish(start, pfx, &format!("{mn} %cl, {mem}")))
            },
            // --- TEST r/m, imm ---
            0xF6 => {
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                let imm = self.take()? as i32;
                Some(self.finish(start, pfx, &format!("test ${imm}, {mem}")))
            },
            // --- NOT/NEG/MUL/IMUL/DIV/IDIV ---
            0xF7 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let (_, mem) = self.modrm_gp(&rex, width)?;
                let mn = match reg {
                    0 => {
                        let imm = self.take_i32()?;
                        return Some(self.finish(start, pfx, &format!("test ${imm}, {mem}")));
                    },
                    2 => "not",
                    3 => "neg",
                    4 => "mul",
                    5 => "imul",
                    6 => "div",
                    7 => "idiv",
                    _ => "f7",
                };
                Some(self.finish(start, pfx, &format!("{mn} {mem}")))
            },
            // --- INC/DEC ---
            0xFE => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = if reg == 0 { "inc" } else { "dec" };
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                Some(self.finish(start, pfx, &format!("{mn} {mem}")))
            },
            // --- INC/DEC/ CALL/JMP/PUSH ---
            0xFF => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let (_, mem) = self.modrm_gp(&rex, width)?;
                let mn = match reg {
                    0 => "inc",
                    1 => "dec",
                    2 => "call",
                    4 => "jmp",
                    6 => "push",
                    _ => "ff",
                };
                Some(self.finish(start, pfx, &format!("{mn} {mem}")))
            },
            // --- Sign extension ---
            0x98 => {
                self.take();
                Some((self.pos - start, "cltq".into()))
            },
            0x99 => {
                self.take();
                Some((self.pos - start, "cqo".into()))
            },
            // --- PUSHF/POPF ---
            0x9C => {
                self.take();
                Some((self.pos - start, "pushfq".into()))
            },
            0x9D => {
                self.take();
                Some((self.pos - start, "popfq".into()))
            },
            // --- Fallback ---
            _ => {
                let len = decode_unknown_len(self.code, start);
                self.pos = start + len;
                // Output the full unknown instruction as .byte sequence
                let bytes: Vec<String> = self.code[start..self.pos]
                    .iter()
                    .map(|b| format!("0x{:02x}", b))
                    .collect();
                Some((len, format!("# UNKNOWN {}", bytes.join(", "))))
            },
        }
    }

    fn decode_two_byte_rex(
        &mut self,
        start: usize,
        pfx: &Prefixes,
        rex: &Rex,
    ) -> Option<(usize, String)> {
        let op = self.take()?;
        match op {
            // REX 前缀的 SSE 标量/vector 指令（0x66/0xF2/0xF3 为 opcode 选择符）
            // 统一交回 decode_sse 处理，避免把 F2/F3/66 误当成独立前缀助记符。
            0x10 | 0x11 | 0x2A | 0x2C | 0x5A => {
                self.pos = start;
                self.decode_sse(start, pfx)
            },
            0x6E | 0x7E => {
                // REX 前缀版本的 movd/movq：66 48 0F 6E/7E /r
                let gp_width = if rex.w { 8 } else { 4 };
                let (xmm, gp) = self.modrm_xmm_gp(rex, gp_width)?;
                let mn = if rex.w { "movq" } else { "movd" };
                let body = if op == 0x6E {
                    format!("{mn} {gp}, {xmm}")
                } else {
                    format!("{mn} {xmm}, {gp}")
                };
                Some((self.pos - start, body))
            },
            0xB0 | 0xB1 => {
                let (src, dst) = self.modrm_gp(rex, if rex.w { 8 } else { 4 })?;
                Some(self.finish(start, pfx, &format!("cmpxchg {src}, {dst}")))
            },
            0xC1 => {
                let (src, dst) = self.modrm_gp(rex, if rex.w { 8 } else { 4 })?;
                Some(self.finish(start, pfx, &format!("xadd {src}, {dst}")))
            },
            // movzx/movsx r64, r/m8|r16 (48 0F B6/B7/BE/BF)
            // dcc 用于窄类型加载/提升: *p (char/uchar), (u)short 提升
            0xB6 | 0xB7 | 0xBE | 0xBF => {
                let (src, dst) = self.modrm_ext(
                    rex,
                    if op == 0xB6 || op == 0xBE { 1 } else { 2 },
                    if rex.w { 8 } else { 4 },
                )?;
                let mn = match (op, rex.w) {
                    (0xB6, true) => "movzbq",
                    (0xB6, false) => "movzbl",
                    (0xB7, true) => "movzwq",
                    (0xB7, false) => "movzwl",
                    (0xBE, true) => "movsbq",
                    (0xBE, false) => "movsbl",
                    (0xBF, true) => "movswq",
                    _ => "movswl",
                };
                Some(self.finish(start, pfx, &format!("{mn} {src}, {dst}")))
            },
            // imul r64, r/m64 (48 0F AF)
            0xAF => {
                let (src, dst) = self.modrm_gp(rex, if rex.w { 8 } else { 4 })?;
                Some(self.finish(start, pfx, &format!("imul {src}, {dst}")))
            },
            // setcc r/m8 带 REX (41 0F 9x C7 → dil 等)
            0x90..=0x9F => {
                let modrm = self.take()?;
                if modrm >> 6 != 3 {
                    self.pos = start + 1;
                    return None;
                }
                let rm = modrm & 7;
                let r = self.reg(rex, rm | if rex.b { 8 } else { 0 }, 1);
                Some(self.finish(start, pfx, &format!("set{} {r}", CC_NAMES[(op - 0x90) as usize])))
            },
            // cmovcc r, r/m (REX)
            0x40..=0x4F => {
                let cc = CC_NAMES[(op - 0x40) as usize];
                let (src, dst) = self.modrm_gp(rex, if rex.w { 8 } else { 4 })?;
                Some(self.finish(start, pfx, &format!("cmov{cc} {src}, {dst}")))
            },
            _ => {
                let len = decode_unknown_len(self.code, start);
                self.pos = start + len;
                // Output the full unknown instruction as .byte sequence
                let bytes: Vec<String> = self.code[start..self.pos]
                    .iter()
                    .map(|b| format!("0x{:02x}", b))
                    .collect();
                Some((len, format!("# UNKNOWN {}", bytes.join(", "))))
            },
        }
    }

    fn decode_legacy(&mut self, start: usize, pfx: &Prefixes) -> Option<(usize, String)> {
        let rex = Rex::default();
        // 无 REX 的 legacy 形式同样要认 0x66：`66 89 /r` 是 16 位 mov，操作数得叫 %ax
        // 而不是 %eax。宽度看不到 0x66 的话，编码器漏发 0x66 的缺陷就会被读成合法编码。
        let w = if pfx.o16 { 2 } else { 4 };
        let op = self.take()?;
        match op {
            // ALU r/m32, r32
            0x01 | 0x09 | 0x21 | 0x29 | 0x31 => {
                let (src, dst) = self.modrm_gp(&rex, w)?;
                let mn = match op {
                    0x01 => "add",
                    0x09 => "or",
                    0x21 => "and",
                    0x29 => "sub",
                    0x31 => "xor",
                    _ => "?",
                };
                Some(self.finish(start, pfx, &format!("{mn} {src}, {dst}")))
            },
            // ALU r32, r/m32
            0x03 | 0x0B | 0x23 | 0x2B | 0x33 | 0x3B => {
                let (dst, src) = self.modrm_gp(&rex, w)?;
                let mn = match op {
                    0x03 => "add",
                    0x0B => "or",
                    0x23 => "and",
                    0x2B => "sub",
                    0x33 => "xor",
                    0x3B => "cmp",
                    _ => "?",
                };
                Some(self.finish(start, pfx, &format!("{mn} {src}, {dst}")))
            },
            // ALU 8-bit r/m, r
            0x00 | 0x08 | 0x20 | 0x28 | 0x30 => {
                let (src, dst) = self.modrm_gp(&rex, 1)?;
                let mn = match op {
                    0x00 => "add",
                    0x08 => "or",
                    0x20 => "and",
                    0x28 => "sub",
                    0x30 => "xor",
                    _ => "?",
                };
                Some(self.finish(start, pfx, &format!("{mn} {src}, {dst}")))
            },
            // ALU 8-bit r, r/m
            0x02 | 0x0A | 0x22 | 0x2A | 0x32 | 0x3A => {
                let (dst, src) = self.modrm_gp(&rex, 1)?;
                let mn = match op {
                    0x02 => "add",
                    0x0A => "or",
                    0x22 => "and",
                    0x2A => "sub",
                    0x32 => "xor",
                    0x3A => "cmp",
                    _ => "?",
                };
                Some(self.finish(start, pfx, &format!("{mn} {src}, {dst}")))
            },
            // MOV r/m32, r32
            0x89 => {
                let (src, dst) = self.modrm_gp(&rex, w)?;
                Some(self.finish(start, pfx, &format!("mov {src}, {dst}")))
            },
            // MOV r32, r/m32
            0x8B => {
                let (dst, src) = self.modrm_gp(&rex, w)?;
                Some(self.finish(start, pfx, &format!("mov {src}, {dst}")))
            },
            // MOV r8, r/m8
            0x8A => {
                let (dst, src) = self.modrm_gp(&rex, 1)?;
                Some(self.finish(start, pfx, &format!("mov {src}, {dst}")))
            },
            // MOV r/m8, r8
            0x88 => {
                let (src, dst) = self.modrm_gp(&rex, 1)?;
                Some(self.finish(start, pfx, &format!("mov {src}, {dst}")))
            },
            // MOV r/m32, imm32
            0xC7 => {
                let (_, mem) = self.modrm_gp(&rex, w)?;
                let imm = self.take_i32()?;
                Some(self.finish(start, pfx, &format!("mov ${imm}, {mem}")))
            },
            // MOV r/m8, imm8
            0xC6 => {
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                let imm = self.take()? as i32;
                Some(self.finish(start, pfx, &format!("mov ${imm}, {mem}")))
            },
            // LEA r32, mem
            0x8D => {
                let (dst, src) = self.modrm_gp(&rex, w)?;
                Some(self.finish(start, pfx, &format!("lea {src}, {dst}")))
            },
            // CMP/TEST r/m32, r32
            0x39 | 0x85 => {
                let (a, b) = self.modrm_gp(&rex, w)?;
                let mn = if op == 0x39 { "cmp" } else { "test" };
                Some(self.finish(start, pfx, &format!("{mn} {b}, {a}")))
            },
            // TEST r/m8, r8
            0x84 => {
                let (a, b) = self.modrm_gp(&rex, 1)?;
                Some(self.finish(start, pfx, &format!("test {b}, {a}")))
            },
            // ALU r/m32, imm32
            0x81 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "add",
                    1 => "or",
                    2 => "adc",
                    3 => "sbb",
                    4 => "and",
                    5 => "sub",
                    6 => "xor",
                    7 => "cmp",
                    _ => "alu",
                };
                let (_, mem) = self.modrm_gp(&rex, w)?;
                let imm = self.take_i32()?;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {mem}")))
            },
            // ALU r/m32, imm8
            0x83 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "add",
                    1 => "or",
                    2 => "adc",
                    3 => "sbb",
                    4 => "and",
                    5 => "sub",
                    6 => "xor",
                    7 => "cmp",
                    _ => "alu",
                };
                let (_, mem) = self.modrm_gp(&rex, w)?;
                let imm = self.take_i8()? as i32;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {mem}")))
            },
            // ALU r/m8, imm8
            0x80 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "add",
                    1 => "or",
                    2 => "adc",
                    3 => "sbb",
                    4 => "and",
                    5 => "sub",
                    6 => "xor",
                    7 => "cmp",
                    _ => "alu",
                };
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                let imm = self.take()? as i32;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {mem}")))
            },
            // Shift/Rotate r/m32, imm8
            0xC1 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "rol",
                    1 => "ror",
                    4 => "shl",
                    5 => "shr",
                    6 => "sal",
                    7 => "sar",
                    _ => "shift",
                };
                let (_, mem) = self.modrm_gp(&rex, w)?;
                let imm = self.take()? as i32;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {mem}")))
            },
            // Shift/Rotate r/m8, imm8
            0xC0 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "rol",
                    1 => "ror",
                    4 => "shl",
                    5 => "shr",
                    6 => "sal",
                    7 => "sar",
                    _ => "shift",
                };
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                let imm = self.take()? as i32;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {mem}")))
            },
            // Shift/Rotate r/m32, 1
            0xD1 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "rol",
                    1 => "ror",
                    4 => "shl",
                    5 => "shr",
                    6 => "sal",
                    7 => "sar",
                    _ => "shift",
                };
                let (_, mem) = self.modrm_gp(&rex, w)?;
                Some(self.finish(start, pfx, &format!("{mn} $1, {mem}")))
            },
            // Shift/Rotate r/m8, 1
            0xD0 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "rol",
                    1 => "ror",
                    4 => "shl",
                    5 => "shr",
                    6 => "sal",
                    7 => "sar",
                    _ => "shift",
                };
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                Some(self.finish(start, pfx, &format!("{mn} $1, {mem}")))
            },
            // Shift/Rotate r/m32, cl
            0xD3 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "rol",
                    1 => "ror",
                    4 => "shl",
                    5 => "shr",
                    6 => "sal",
                    7 => "sar",
                    _ => "shift",
                };
                let (_, mem) = self.modrm_gp(&rex, w)?;
                Some(self.finish(start, pfx, &format!("{mn} %cl, {mem}")))
            },
            // Shift/Rotate r/m8, cl
            0xD2 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = match reg {
                    0 => "rol",
                    1 => "ror",
                    4 => "shl",
                    5 => "shr",
                    6 => "sal",
                    7 => "sar",
                    _ => "shift",
                };
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                Some(self.finish(start, pfx, &format!("{mn} %cl, {mem}")))
            },
            // TEST r/m32, imm32
            0xF7 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let (_, mem) = self.modrm_gp(&rex, w)?;
                if reg == 0 {
                    let imm = self.take_i32()?;
                    Some(self.finish(start, pfx, &format!("test ${imm}, {mem}")))
                } else {
                    let mn = match reg {
                        2 => "not",
                        3 => "neg",
                        4 => "mul",
                        5 => "imul",
                        6 => "div",
                        7 => "idiv",
                        _ => "f7",
                    };
                    Some(self.finish(start, pfx, &format!("{mn} {mem}")))
                }
            },
            // TEST r/m8, imm8
            0xF6 => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                if reg == 0 {
                    let imm = self.take()? as i32;
                    Some(self.finish(start, pfx, &format!("test ${imm}, {mem}")))
                } else {
                    let mn = match reg {
                        2 => "not",
                        3 => "neg",
                        _ => "f6",
                    };
                    Some(self.finish(start, pfx, &format!("{mn} {mem}")))
                }
            },
            // INC/DEC/CALL/JMP/PUSH r/m32
            0xFF => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let (_, mem) = self.modrm_gp(&rex, w)?;
                let mn = match reg {
                    0 => "inc",
                    1 => "dec",
                    2 => "call",
                    4 => "jmp",
                    6 => "push",
                    _ => "ff",
                };
                Some(self.finish(start, pfx, &format!("{mn} {mem}")))
            },
            // INC/DEC r/m8
            0xFE => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = if reg == 0 { "inc" } else { "dec" };
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                Some(self.finish(start, pfx, &format!("{mn} {mem}")))
            },
            // XCHG r/m32, r32
            0x87 => {
                let (src, dst) = self.modrm_gp(&rex, w)?;
                Some(self.finish(start, pfx, &format!("xchg {src}, {dst}")))
            },
            // MOVSXD r32, r/m32 (without REX, same as MOV r32, r/m32 on 64-bit)
            0x63 => {
                let modrm = self.take()?;
                let mod_ = modrm >> 6;
                let reg = (modrm >> 3) & 7;
                let rm = modrm & 7;
                let dst = self.reg(&rex, reg, 4).to_string();
                let src = self.decode_mem(&rex, mod_, rm, w)?;
                Some(self.finish(start, pfx, &format!("movsxd {src}, {dst}")))
            },
            // IMUL r32, r/m32, imm32
            0x69 => {
                let (src, dst) = self.modrm_gp(&rex, w)?;
                let imm = self.take_i32()?;
                Some(self.finish(start, pfx, &format!("imul ${imm}, {src}, {dst}")))
            },
            // IMUL r32, r/m32, imm8
            0x6B => {
                let (src, dst) = self.modrm_gp(&rex, w)?;
                let imm = self.take_i8()? as i32;
                Some(self.finish(start, pfx, &format!("imul ${imm}, {src}, {dst}")))
            },
            // ALU eax, imm32
            0x05 | 0x0D | 0x25 | 0x2D | 0x35 | 0x3D => {
                let mn = match op {
                    0x05 => "add",
                    0x0D => "or",
                    0x25 => "and",
                    0x2D => "sub",
                    0x35 => "xor",
                    0x3D => "cmp",
                    _ => "?",
                };
                let imm = self.take_i32()?;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, %eax")))
            },
            _ => {
                let len = decode_unknown_len(self.code, start);
                self.pos = start + len;
                // Output the full unknown instruction as .byte sequence
                let bytes: Vec<String> = self.code[start..self.pos]
                    .iter()
                    .map(|b| format!("0x{:02x}", b))
                    .collect();
                Some((len, format!("# UNKNOWN {}", bytes.join(", "))))
            },
        }
    }

    /// 解码 x87 FPU 指令 (0xD8-0xDF)
    fn decode_x87(&mut self, start: usize, opcode: u8) -> Option<(usize, String)> {
        let modrm = self.take()?;
        let mod_ = modrm >> 6;
        let reg = (modrm >> 3) & 7;
        let rm = modrm & 7;

        // 特殊处理: 寄存器-寄存器 指令 (mod=3, 特定 opcode)
        if mod_ == 3 {
            // DE 前缀的寄存器运算指令
            if opcode == 0xDE {
                let mnemonic = match reg {
                    0 => "faddp",
                    1 => "fmulp",
                    4 => "fsubp",
                    5 => "fsubrp",
                    6 => "fdivp",
                    7 => "fdivrp",
                    _ => "f???",
                };
                return Some((self.pos - start, format!("{} %st({}), %st", mnemonic, rm)));
            }
            // DF E9: fucomip
            if opcode == 0xDF && modrm == 0xE9 {
                return Some((self.pos - start, "fucomip %st(1), %st".into()));
            }
            // D9 前缀的特殊指令
            if opcode == 0xD9 {
                match modrm {
                    0xE0 => return Some((self.pos - start, "fchs".into())),
                    0xE8 => return Some((self.pos - start, "fld1".into())),
                    0xEE => return Some((self.pos - start, "fldz".into())),
                    0xD0 => return Some((self.pos - start, "fnop".into())),
                    _ => {},
                }
            }
        }

        // 内存操作数解码 - 手动解码 modrm 的内存部分
        let mem = self.decode_x87_mem(mod_, rm)?;

        let mnemonic = match opcode {
            0xD8 => match reg {
                0 => "fadds",
                1 => "fmuls",
                4 => "fsubs",
                5 => "fsubrs",
                6 => "fdivs",
                7 => "fdivrs",
                _ => "f???",
            },
            0xD9 => match reg {
                0 => "flds",
                2 => "fstps",
                3 => "fstps",
                5 => "fldcw",
                7 => "fnstcw",
                _ => "f???",
            },
            0xDA => "fi???",
            0xDB => match reg {
                0 => "fildl",
                _ => "f???",
            },
            0xDC => match reg {
                0 => "faddl",
                1 => "fmull",
                4 => "fsubl",
                5 => "fsubrl",
                6 => "fdivl",
                7 => "fdivrl",
                _ => "f???",
            },
            0xDD => match reg {
                0 => "fldl",
                2 => "fstpl",
                3 => "fstpl",
                _ => "f???",
            },
            0xDE => match reg {
                0 => "faddp",
                1 => "fmulp",
                4 => "fsubp",
                5 => "fsubrp",
                6 => "fdivp",
                7 => "fdivrp",
                _ => "f???",
            },
            0xDF => match reg {
                5 => "fucomip",
                _ => "f???",
            },
            _ => "f???",
        };

        Some((self.pos - start, format!("{} {}", mnemonic, mem)))
    }

    /// 解码 x87 内存操作数 (简化版，专门处理 r13 基址)
    fn decode_x87_mem(&mut self, mod_: u8, rm: u8) -> Option<String> {
        // 处理 SIB 字节情况 (rm=4 表示有 SIB)
        if rm == 4 {
            let sib = self.take()?;
            let base = sib & 7;
            let _index = (sib >> 3) & 7;
            let _scale = sib >> 6;
            // 对于我们的用例，index=4 表示无索引
            let disp = match mod_ {
                0 => {
                    if base == 5 {
                        // [rip+disp32]
                        let d = self.take_i32()?;
                        return Some(format!("{}(%rip)", d));
                    } else {
                        0i32
                    }
                },
                1 => self.take_i8()? as i32,
                2 => self.take_i32()?,
                _ => 0,
            };
            let base_reg = if base == 5 { "%rbp" } else { REG64[base as usize] };
            if disp == 0 {
                return Some(format!("({})", base_reg));
            }
            return Some(format!("{}({})", disp, base_reg));
        }

        // 非 SIB 情况
        let disp = match mod_ {
            0 => {
                if rm == 5 {
                    // [rip+disp32]
                    let d = self.take_i32()?;
                    return Some(format!("{}(%rip)", d));
                } else {
                    0i32
                }
            },
            1 => self.take_i8()? as i32,
            2 => self.take_i32()?,
            _ => return Some(REG64[rm as usize].to_string()),
        };

        // rm=5 with mod!=0 表示 r13
        let base_reg = if rm == 5 { "%r13" } else { REG64[rm as usize] };
        if disp == 0 {
            Some(format!("({})", base_reg))
        } else {
            Some(format!("{}({})", disp, base_reg))
        }
    }

    /// 计算从 start 开始的未知指令应整体消费的字节数。
    /// 根据 prefix/opcode/ModRM/SIB/disp/imm 做最低限度的 x86 长度解析，
    /// 确保未覆盖指令不会只消费 1 字节而导致后续解码失步。
    fn unknown_len(&self, start: usize) -> usize {
        let code = self.code;
        let len = code.len();
        let mut pos = start;

        // 扫描 legacy prefixes
        while pos < len {
            match code[pos] {
                0xF0 | 0xF2 | 0xF3 | 0x66 | 0x67 | 0x2E | 0x36 | 0x3E | 0x26 | 0x64 | 0x65 => {
                    pos += 1
                },
                _ => break,
            }
        }

        // 扫描 REX
        if pos < len && (code[pos] & 0xF0) == 0x40 {
            pos += 1;
        }

        if pos >= len {
            return len - start;
        }

        let op1 = code[pos];
        pos += 1;

        // VEX/EVEX/XOP：至少完整消费前缀 payload 及后续 ModRM
        if matches!(op1, 0xC4 | 0xC5 | 0x62 | 0x8F) {
            return self.skip_vex_evex_xop_len(start);
        }

        let (has_modrm, mut imm_len) = if op1 == 0x0F {
            if pos >= len {
                return len - start;
            }
            let op2 = code[pos];
            pos += 1;
            match op2 {
                // syscall / ud2 / cpuid / rdmsr / wrmsr / rdtsc / etc.
                0x05 | 0x0B | 0x34 | 0x35 | 0xA0 | 0xA1 | 0xA2 | 0xA8 | 0xA9 => (false, 0),
                // 0F 38：三字节 opcode，通常带 ModRM，无立即数
                0x38 => {
                    if pos >= len {
                        return len - start;
                    }
                    pos += 1;
                    (true, 0)
                },
                // 0F 3A：三字节 opcode，通常带 ModRM + imm8
                0x3A => {
                    if pos >= len {
                        return len - start;
                    }
                    pos += 1;
                    (true, 1)
                },
                // jcc rel32 / near call/jmp rel32
                0x80..=0x8F | 0xE8 | 0xE9 => (false, 4),
                _ => (true, 0),
            }
        } else {
            Self::legacy_opcode_props(op1)
        };

        if has_modrm {
            if pos >= len {
                return len - start;
            }
            let modrm = code[pos];
            pos += 1;
            let mod_ = modrm >> 6;
            let reg = (modrm >> 3) & 7;
            let rm = modrm & 7;

            // 根据 opcode + reg 调整立即数长度
            match op1 {
                0x80 | 0x82 | 0x83 => imm_len = 1,
                0x81 => imm_len = 4,
                0xC0 | 0xC1 => imm_len = 1,
                0xC6 => imm_len = 1,
                0xC7 => imm_len = 4,
                0xF6 => {
                    if reg == 0 {
                        imm_len = 1;
                    }
                },
                0xF7 if reg == 0 => {
                    imm_len = 4;
                },
                _ => {},
            }

            // SIB
            if mod_ != 3 && rm == 4 {
                if pos >= len {
                    return len - start;
                }
                let sib = code[pos];
                pos += 1;
                let base = sib & 7;
                if mod_ == 0 && base == 5 {
                    pos = (pos + 4).min(len);
                }
            } else if mod_ == 0 && rm == 5 {
                // [rip+disp32] 或 disp32 绝对地址
                pos = (pos + 4).min(len);
            }

            // disp
            match mod_ {
                1 => pos = (pos + 1).min(len),
                2 => pos = (pos + 4).min(len),
                _ => {},
            }
        }

        pos = (pos + imm_len).min(len);
        (pos - start).max(1)
    }

    /// VEX/EVEX/XOP 指令的最低限度整条消费。
    fn skip_vex_evex_xop_len(&self, start: usize) -> usize {
        let code = self.code;
        let len = code.len();
        let mut pos = start;
        // 扫描 legacy prefixes（VEX 允许前面有 legacy prefixes，但少见）
        while pos < len {
            match code[pos] {
                0xF0 | 0xF2 | 0xF3 | 0x66 | 0x67 => pos += 1,
                _ => break,
            }
        }
        if pos >= len {
            return len - start;
        }
        match code[pos] {
            // 2-byte VEX: C5 + 1 payload + opcode + ModRM + ...
            0xC5 => pos = (pos + 2).min(len),
            // 3-byte VEX: C4 + 2 payload + opcode + ModRM + ...
            0xC4 | 0x8F => pos = (pos + 3).min(len),
            // EVEX: 62 + 3 payload + opcode + ModRM + ...
            0x62 => pos = (pos + 4).min(len),
            _ => return 1,
        }
        if pos >= len {
            return len - start;
        }
        // 假定存在 ModRM 并跳过 SIB/disp；imm 长度保守按 0 处理。
        // 这至少保证不会把 VEX/EVEX 多字节编码拆散。
        let modrm = code[pos];
        pos += 1;
        let mod_ = modrm >> 6;
        let rm = modrm & 7;
        if mod_ != 3 && rm == 4 && pos < len {
            pos += 1; // SIB
        }
        match mod_ {
            0 => {
                if rm == 5 {
                    pos = (pos + 4).min(len);
                }
            },
            1 => pos = (pos + 1).min(len),
            2 => pos = (pos + 4).min(len),
            _ => {},
        }
        (pos - start).max(1)
    }

    /// 单字节 opcode 的 (是否含 ModRM, 默认立即数长度) 属性表。
    fn legacy_opcode_props(op1: u8) -> (bool, usize) {
        match op1 {
            // 无 ModRM，无立即数
            0x90 | 0xC3 | 0xC9 | 0x99 | 0x9C | 0x9D | 0x98 | 0x37 | 0x3F | 0x27 | 0x2F | 0xF5
            | 0xF8 | 0xF9 | 0xFA | 0xFB | 0xFC | 0xFD | 0x06 | 0x07 | 0x0E | 0x16 | 0x17 | 0x1E
            | 0x1F | 0x60 | 0x61 | 0x9E | 0x9F => (false, 0),
            // 无 ModRM，imm8
            0x6A | 0xEB | 0xCD | 0xD4 | 0xD5 | 0xE4 | 0xE5 | 0xE6 | 0xE7 | 0x70..=0x7F => {
                (false, 1)
            },
            // 无 ModRM，imm32/rel32
            0x68 | 0xE8 | 0xE9 => (false, 4),
            // mov r, imm
            0xB0..=0xB7 => (false, 1),
            0xB8..=0xBF => (false, 4),
            // ALU rax, imm8
            0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x34 | 0x3C => (false, 1),
            // ALU rax, imm32
            0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D => (false, 4),
            // ret imm16 / int imm8 / in/out imm8 等已在上面覆盖
            // 默认：含 ModRM，立即数长度待定
            _ => (true, 0),
        }
    }
}

#[allow(clippy::too_many_arguments)]
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
        let mut dec = Dec {
            code,
            pos,
            base: base_offset,
            calls,
            data,
            labels,
        };
        match dec.decode_one() {
            Some((len, text)) if len > 0 => {
                out.push('\t');
                out.push_str(&text);
                out.push('\n');
                pos += len;
            },
            _ => {
                let len = dec.unknown_len(pos);
                let bytes: Vec<String> = code[pos..(pos + len).min(code.len())]
                    .iter()
                    .map(|b| format!("0x{:02x}", b))
                    .collect();
                out.push('\t');
                out.push_str(".byte ");
                out.push_str(&bytes.join(", "));
                out.push('\n');
                pos += len;
            },
        }
    }
    out
}

pub fn label_map<'a>(
    labels: impl IntoIterator<Item = (&'a u64, &'a str)>,
) -> BTreeMap<u64, Vec<&'a str>> {
    let mut m: BTreeMap<u64, Vec<&'a str>> = BTreeMap::new();
    for (off, name) in labels {
        m.entry(*off).or_default().push(name);
    }
    sort_labels_in_place(&mut m);
    m
}

/// 同一地址上多个别名的排列必须确定。
///
/// 上游传入的 `impl IntoIterator` 来自 `Codegen::labels` / `asm_directives_at`
/// 等 `HashMap` 迭代结果: `BTreeMap` 的 **key(地址) 有序，但每个 key 下的 `Vec`
/// 完全保留上游的随机迭代序**，导致 `-S` 输出里共用同一 PC 的标签行
/// (如 `.L4:` / `.L13:`) 逐进程跳动。按名字排序后，同地址别名的输出序固定。
fn sort_labels_in_place(m: &mut BTreeMap<u64, Vec<&str>>) {
    for names in m.values_mut() {
        names.sort_unstable();
    }
}

pub fn directive_map<'a>(
    items: impl IntoIterator<Item = (&'a u64, &'a str)>,
) -> BTreeMap<u64, Vec<&'a str>> {
    let mut m: BTreeMap<u64, Vec<&'a str>> = BTreeMap::new();
    for (off, line) in items {
        m.entry(*off).or_default().push(line);
    }
    sort_labels_in_place(&mut m);
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
            out.entry(off - base)
                .or_insert_with(Vec::new)
                .extend(names.iter().copied());
        }
    }
    out
}

pub fn slice_map<T: Clone>(map: &HashMap<u64, T>, base: u64, size: u64) -> HashMap<u64, T> {
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

/// Fallback helper: estimate length of unknown instruction
/// to prevent instruction pointer desync in disassembler.
/// Returns the estimated instruction length in bytes.
fn decode_unknown_len(code: &[u8], start: usize) -> usize {
    if start >= code.len() {
        return 1;
    }
    let mut pos = start;

    // Consume prefix bytes
    loop {
        match code.get(pos) {
            Some(0xF0) => {
                pos += 1;
            }, // lock
            Some(0xF3) => {
                pos += 1;
            }, // rep
            Some(0xF2) => {
                pos += 1;
            }, // repne
            Some(0x66) => {
                pos += 1;
            }, // data16
            Some(0x40..=0x4F) => {
                pos += 1;
            }, // REX
            _ => break,
        }
    }

    if pos >= code.len() {
        return pos - start + 1;
    }

    let opcode = code[pos];
    pos += 1;

    // Two-byte opcode (0x0F)
    if opcode == 0x0F {
        if pos >= code.len() {
            return pos - start + 1;
        }
        let op2 = code[pos];
        pos += 1;

        // Instructions with ModRM: most common SSE and general instructions
        // 0F 10-12, 20-2C, 58-5A, 6E, 7E: SSE scalar/vector load/store
        // 0F 40-4F: cmovcc
        // 0F 90-9F: setcc
        // 0F B0-B1: cmpxchg/xadd
        // 0F B6-BF: movzx/movsx
        // 0F AF: imul
        // 0F 80-8F: jcc (rel8)
        if matches!(op2, 0x10..=0x12 | 0x20..=0x2C | 0x58..=0x5A | 0x6E | 0x7E |
                       0x40..=0x4F | 0x90..=0x9F | 0xB0..=0xB1 | 0xB6..=0xBF | 0xAF | 0x80..=0x8F)
        {
            if pos >= code.len() {
                return pos - start + 1;
            }
            pos += consume_modrm_and_optional_parts(code, pos);
            return pos - start;
        }

        // Other extensions: VEX/EVEX would need longer analysis - conservative estimate
        return pos - start;
    }

    // Single-byte opcodes
    match opcode {
        // REG-MEM instructions: need ModRM (+ optional disp/imm)
        0x00..=0x09
        | 0x10..=0x1B
        | 0x20..=0x29
        | 0x30..=0x3B
        | 0x60..=0x69
        | 0x6B
        | 0x80..=0x8F
        | 0xA0..=0xA5
        | 0xA8..=0xAA
        | 0xB8..=0xBF
        | 0xC6..=0xC7
        | 0xD0..=0xD3
        | 0xF0..=0xFF => {
            if pos < code.len() {
                pos += 1; // ModRM
                pos += consume_optional_disp_imm(code, pos, opcode);
            }
        },
        // PUSH imm8/immm32, CALL/JMP near, RET imm16
        0x6A..=0x6F | 0xCA..=0xCF | 0xC8..=0xC9 => {
            pos += consume_immediate_for_opcode(opcode);
        },
        _ => {},
    }

    pos - start
}

/// Helper to consume ModRM byte and potential SIB/disp/imm parts
fn consume_modrm_and_optional_parts(code: &[u8], pos: usize) -> usize {
    if pos >= code.len() {
        return 0;
    }
    let mut len = 1; // already consumed ModRM
    let modrm = code[pos];
    let mod_ = modrm >> 6;
    let rm = modrm & 7;

    // Check for SIB byte
    if rm == 4 && mod_ != 0 && pos + 1 < code.len() {
        len += 1; // SIB
        let sib = code[pos + 1];
        let base = sib & 7;
        // If SIB has disp32 (base=5 and mod=0)
        if mod_ == 0 && base == 5 {
            len += 4;
        }
    }

    // Displacement
    if mod_ == 1 && pos + len < code.len() {
        len += 1; // disp8
    } else if mod_ == 2 && pos + len < code.len() {
        len += 4; // disp32
    } else if rm == 5 && mod_ == 0 && pos + len < code.len() {
        len += 4; // disp32 (RIP-relative)
    }

    len.min(code.len() - pos)
}

/// Estimate displacement and immediate based on opcode
fn consume_optional_disp_imm(code: &[u8], pos: usize, opcode: u8) -> usize {
    let mut len = 0;

    // Some ALU and MOV instructions have immediate operand
    if matches!(opcode, 0x81 | 0x83 | 0xC7) && pos + 4 <= code.len() {
        len += 4; // imm32
    }

    len.min(code.len() - pos)
}

/// Estimate immediate size for specific opcodes
fn consume_immediate_for_opcode(opcode: u8) -> usize {
    match opcode {
        0x6A => 1,        // PUSH imm8
        0x6B => 1,        // IMUL imm8
        0xC0..=0xC2 => 1, // ROL/ROR/etc imm8
        0xCA..=0xCF => 2, // RET/CALL/JMP near (imm16 in legacy mode, but usually handled differently)
        0xC8 => 6,        // LEA w/ imm16 (w/disp16 for lea)
        _ => 4,           // default: assume imm32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_mov_imm_and_ret() {
        let code = [0xB8, 0x05, 0x00, 0x00, 0x00, 0xC3];
        let out = disassemble_range(
            &code,
            0,
            code.len(),
            0,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(out.contains("mov $5, %eax"));
        assert!(out.contains("ret"));
    }

    #[test]
    fn decodes_lock_cmpxchg_and_movsd() {
        let code = [
            0xF0, 0x48, 0x0F, 0xB1, 0x17, // lock cmpxchg %rdx, (%rdi)
            0xF2, 0x0F, 0x10, 0x05, 0, 0, 0, 0, // movsd 0(%rip), %xmm0
        ];
        let out = disassemble_range(
            &code,
            0,
            code.len(),
            0,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(out.contains("lock cmpxchg"), "{out}");
        assert!(out.contains("movsd"), "{out}");
    }
}
