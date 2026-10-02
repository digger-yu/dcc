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
    /// 段前缀字节（0x26/0x2E/0x36/0x3E/0x64/0x65）。它**必须被当成前缀收掉**：留在
    /// 流里会被当成一条指令的首字节（`0x26` 还带 ModRM），于是一条 `65 48 8b 05 …`
    /// 被读成两段残渣，percpu 那一族整页错位。
    ///
    /// 渲染位置不是我们定的，是 `as` + `objdump -d` 反读出来的（见 seg_operand/seg_word
    /// 的注释）：FS/GS 贴在内存操作数上，ES/CS/SS/DS 贴在助记符前。
    seg: Option<u8>,
}

/// 段前缀 → 贴在**内存操作数**前的字面量。objdump 只对 FS/GS 用这种拼法：
/// `65 48 8b 05 …` → `mov %gs:0x0(%rip),%rax`；`64 48 8b 04 25 10` → `mov %fs:0x10,%rax`。
/// ES/CS/SS/DS 返回空串，因为它们走 [`seg_word`]。
fn seg_operand(pfx: u8) -> &'static str {
    match pfx {
        0x64 => "%fs:",
        0x65 => "%gs:",
        _ => "",
    }
}

/// 段前缀 → 贴在**助记符**前的字面量。objdump 的两种情形用这一路：
/// ES/CS/SS/DS 覆盖（`26 48 8b 58 08` → `es mov 0x8(%rax),%rbx`、
/// `3e 48 8b 58 08` → `ds mov …`、`36 ff 20` → `ss jmp *(%rax)`），
/// 以及 FS/GS 但这条指令压根没有内存操作数（`65 90` → `gs nop`、
/// `65 0f ae f0` → `gs mfence`）。
fn seg_word(pfx: u8) -> &'static str {
    match pfx {
        0x26 => "es ",
        0x2e => "cs ",
        0x36 => "ss ",
        0x3e => "ds ",
        0x64 => "fs ",
        0x65 => "gs ",
        _ => "",
    }
}

/// 条件码后缀, 索引 = 0x0F 后操作码低 4 位 (setcc/jcc 共用).
const CC_NAMES: [&str; 16] = [
    "o", "no", "b", "ae", "e", "ne", "be", "a", "s", "ns", "p", "np", "l", "ge", "le", "g",
];

/// `66 0F xx /r` 打包整数算术族（SSE2）的助记词表。
///
/// **每一项都是本机 `as` + `objdump -d` 反读出来的**（不是记忆）：对每个 op 码组
/// `.byte 0x66,0x0f,0xXX,0xc1` 交给 objdump 读名字，再用 `as` 组装 `mn %xmm1,%xmm0`
/// 确认能回到同一串字节（双向定死）。AT&T 打印序是 `{src}, {dst}`：dst=ModRM.reg，
/// src=ModRM.r/m（mod=3 时也叫 %xmmN）。
pub(crate) fn packed_int_mnemonic(op: u8) -> Option<&'static str> {
    Some(match op {
        0x60 => "punpcklbw",
        0x61 => "punpcklwd",
        0x62 => "punpckldq",
        0x63 => "packsswb",
        0x64 => "pcmpgtb",
        0x65 => "pcmpgtw",
        0x66 => "pcmpgtd",
        0x67 => "packuswb",
        0x68 => "punpckhbw",
        0x69 => "punpckhwd",
        0x6a => "punpckhdq",
        0x6b => "packssdw",
        0x6c => "punpcklqdq",
        0x6d => "punpckhqdq",
        0x74 => "pcmpeqb",
        0x75 => "pcmpeqw",
        0x76 => "pcmpeqd",
        0xd1 => "psrlw",
        0xd2 => "psrld",
        0xd3 => "psrlq",
        0xd4 => "paddq",
        0xd5 => "pmullw",
        0xd8 => "psubusb",
        0xd9 => "psubusw",
        0xda => "pminub",
        0xdb => "pand",
        0xdc => "paddusb",
        0xdd => "paddusw",
        0xdf => "pandn",
        0xe0 => "pavgb",
        0xe1 => "psraw",
        0xe2 => "psrad",
        0xe3 => "pavgw",
        0xe4 => "pmulhuw",
        0xe5 => "pmulhw",
        0xe8 => "psubsb",
        0xe9 => "psubsw",
        0xea => "pminsw",
        0xeb => "por",
        0xec => "paddsb",
        0xed => "paddsw",
        0xee => "pmaxsw",
        0xef => "pxor",
        0xf1 => "psllw",
        0xf2 => "pslld",
        0xf3 => "psllq",
        0xf4 => "pmuludq",
        0xf5 => "pmaddwd",
        0xf6 => "psadbw",
        0xf8 => "psubb",
        0xf9 => "psubw",
        0xfa => "psubd",
        0xfb => "psubq",
        0xfc => "paddb",
        0xfd => "paddw",
        0xfe => "paddd",
        // 有意不入表的（objdump 读不回可被 `as` 重现的名字）：
        //   0xd6 movq / 0x6e,0x7e movd（已有专列）、0xd7 pmovmskb（dst 是 GP，
        //   `pmovmskb %xmm1,%xmm0` 被 GAS 拒）、0xf7 maskmovdqu、0xe6/0xe7（objdump
        //   对 0xe6 打 AVX 名 `vttpd2dq`，与 legacy 名不同）、0xf0/0xff（本机
        //   binutils 直报 `(bad)`/ud0）。
        _ => return None,
    })
}

/// 两条 0F 入口（非-REX 的 [`Dec::decode_two_byte`] 与 REX 的
/// [`Dec::decode_two_byte_rex`]）**共用**这一张 SSE 白名单。写成谓词而不是两份
/// 字面量列表，是因为这个项目已经踩过「只改了 `decode_sse`、入口白名单没加 →
/// 复测数字一动没动」的坑：两份列表迟早会漂。
pub(crate) fn sse_op(op: u8) -> bool {
    matches!(
        op,
        // load / store / 标量算术 / 转换（原有覆盖面）
        0x10 | 0x11 | 0x28 | 0x29 | 0x2A | 0x2C | 0x2E | 0x2F | 0x51 | 0x57 | 0x58
            | 0x59 | 0x5A | 0x5C | 0x5D | 0x5E | 0x5F | 0x6E | 0x6F | 0x7E | 0x7F | 0xD6
            // 打包整数算术族（只在 66 格有名字，见 packed_int_mnemonic）
            | 0x60..=0x6D | 0x74..=0x76
            | 0xD1..=0xD5 | 0xD8..=0xDD | 0xDF
            | 0xE0..=0xE5 | 0xE8..=0xEB | 0xEC..=0xEF
            | 0xF1..=0xF6 | 0xF8..=0xFE
            // UNPCKL/UNPCKH（无 66 = ps，66 = pd）
            | 0x14 | 0x15
            // 0F 3A 三字节转义（只有 66 格的 round* 有名字）
            | 0x3A
    )
}

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
    /// 本条指令的段前缀字节（由 [`Dec::take_prefixes`] 记下, `decode_mem` 据此把
    /// `%gs:` 贴到内存操作数上）。Dec 每条指令新建一次, 所以不会串到下一条。
    seg: Option<u8>,
    /// 段前缀是否已经贴在某个内存操作数上了。贴过就不再在助记符前重复贴
    /// （objdump 也是二选一：`mov %gs:0x8(%rax),%rbx` vs `gs nop`）。
    seg_shown: bool,
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

    /// 按**完整** 0..15 索引取 %xmm 名。REX 的哪一位扩展该字段由调用方决定
    /// （ModRM.reg 走 REX.R，ModRM.r/m 走 REX.B），所以这里刻意不碰 rex。
    fn xmm_at(&self, idx: u8) -> &'static str {
        XMM[(idx & 15) as usize]
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

    /// SSE 形 ModRM：reg 字段是 XMM 目的，r/m 字段是源（XMM 或内存）。
    ///
    /// mod=3 时 r/m 字段**也是** XMM 号，不是 GP 寄存器：`0f 58 c1` 的 objdump 读数是
    /// `addps %xmm1,%xmm0`（`as` 组装 `addps %xmm1,%xmm0` 正好得到这 3 个字节）。旧代码
    /// 这里调 `decode_mem(width=8)`，把 %xmm1 打成 `%rcx` —— 名字和号都错。
    fn modrm_xmm(&mut self, rex: &Rex) -> Option<(String, String)> {
        let modrm = self.take()?;
        let mod_ = modrm >> 6;
        let reg = (modrm >> 3) & 7;
        let rm = modrm & 7;
        let reg_name = self.xmm(rex, reg).to_string();
        let mem = if mod_ == 3 {
            self.xmm_at(rm | if rex.b { 8 } else { 0 }).to_string()
        } else {
            self.decode_mem(rex, mod_, rm, 8)?
        };
        Some((reg_name, mem))
    }

    /// `0F 2C` 的 F3/F2 格（cvttss2si / cvttsd2si）：reg 字段是**通用寄存器**目的，
    /// r/m 是 XMM 或内存源。`f3 0f 2c c1` → objdump `cvttss2si %xmm1,%eax`（as 组装
    /// 同一行得到同样字节），返回 `(src, dst)` 以便按 AT&T 序打印。
    fn modrm_cvttrunc(&mut self, rex: &Rex, gp_width: u8) -> Option<(String, String)> {
        let modrm = self.take()?;
        let mod_ = modrm >> 6;
        let reg = (modrm >> 3) & 7;
        let rm = modrm & 7;
        let dst = self
            .reg(rex, reg | if rex.r { 8 } else { 0 }, gp_width)
            .to_string();
        let src = if mod_ == 3 {
            self.xmm_at(rm | if rex.b { 8 } else { 0 }).to_string()
        } else {
            self.decode_mem(rex, mod_, rm, gp_width)?
        };
        Some((src, dst))
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
        // 段前缀贴在内存操作数上（objdump 反读确认：`65 48 0f b6 00` =
        // `movzbq %gs:(%rax),%rax`）。只有 FS/GS 走这一路，ES/CS/SS/DS 由
        // prefix_str 贴在助记符前；mod=3 的寄存器形式在上面已早退，不会到这里。
        let seg = match self.seg {
            Some(b) => seg_operand(b),
            None => "",
        };
        if !seg.is_empty() {
            self.seg_shown = true;
        }
        let mut inner = String::from(seg);
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

    fn prefix_str(&self, pfx: &Prefixes) -> String {
        let mut parts = Vec::new();
        // 段前缀。ES/CS/SS/DS 只能走这一路（objdump: `es mov 0x8(%rax),%rbx`）；
        // FS/GS 已经在内存操作数上贴过 `%gs:` 就不再重复（`gs mov %gs:(%rax)` 是错的）。
        if let Some(s) = pfx.seg {
            if !self.seg_shown {
                let w = seg_word(s);
                if !w.is_empty() {
                    parts.push(w);
                }
            }
        }
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
        let text = if pfx.lock || pfx.rep || pfx.repne || pfx.seg.is_some() {
            format!("{}{body}", self.prefix_str(pfx))
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
                // 段前缀。之前这一族**不被收集**，于是 `65 48 8b 05 …`（percpu 读）被
                // 从 0x65 开始当指令读：0x65 不在任何臂上 → 只吞它自己 → 后面的
                // `48 8b …` 又被重读一遍，读数里同时出现 `# UNKNOWN 0x65, 0x48` 与一条
                // “真” mov（两条加起来还是一行 UNKNOWN），整族 percpu 访问全部错位。
                Some(b @ (0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65)) => {
                    pfx.seg = Some(b);
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
        // 段前缀交给 decode_mem 贴到内存操作数上（objdump 的拼法），所以既记字节
        // 也记「还没贴过」。
        self.seg = pfx.seg;
        self.seg_shown = false;
        let b = self.peek()?;

        if (0x50..=0x57).contains(&b) {
            self.take();
            return Some((
                self.pos - start,
                format!("{}push {}", self.prefix_str(&pfx), REG64[(b - 0x50) as usize]),
            ));
        }
        if (0x58..=0x5F).contains(&b) {
            self.take();
            return Some((
                self.pos - start,
                format!("{}pop {}", self.prefix_str(&pfx), REG64[(b - 0x58) as usize]),
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
                Some((self.pos - start, format!("{}ret", self.prefix_str(&pfx))))
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
            // CBW/CWDE/CWT：宽度轴与 REX 路径同一份事实（as + objdump 实测：
            // `98`→cwtl、`66 98`→cbtw）。本函数的 arm 自己 take opcode。
            0x98 => {
                self.take();
                let mn = if pfx.o16 { "cbtw" } else { "cwtl" };
                Some((self.pos - start, mn.into()))
            },
            0xCC => {
                self.take();
                Some((self.pos - start, "int3".into()))
            },
            0x9B => {
                self.take();
                Some((self.pos - start, "fwait".into()))
            },
            // 字符串族（MOVS/STOS/CMPS/SCAS）：本路径无 REX，宽度由 66 定。
            // 拼写与 REX 路径共用 [`Self::string_op_text`]，不在两处各写一份 ——
            // 以前这里只有 `rep movs*` 两条且只念助记符，同一族指令在两条路径里
            // 有两种拼法，那就是下一份漂移的起点。
            op @ (0xA4 | 0xA5 | 0xA6 | 0xA7 | 0xAA | 0xAB | 0xAE | 0xAF) => {
                self.take();
                Some((self.pos - start, Self::string_op_text(op, &pfx, false)?))
            },
            0xEB => {
                self.take();
                let rel = self.take_i8()? as i32;
                Some((
                    self.pos - start,
                    format!(
                        "{}jmp {}",
                        self.prefix_str(&pfx),
                        self.jump_target(off + 1, rel, off + 2)
                    ),
                ))
            },
            0xE8 => {
                self.take();
                let rel = self.take_i32()?;
                let target = self.jump_target(off + 1, rel, off + 5);
                Some((self.pos - start, format!("{}call {target}", self.prefix_str(&pfx))))
            },
            0xE9 => {
                self.take();
                let rel = self.take_i32()?;
                let target = self.jump_target(off + 1, rel, off + 5);
                Some((self.pos - start, format!("{}jmp {target}", self.prefix_str(&pfx))))
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
            // RDTSC（不带 ModRM、不带立即数）。dcc 的 inline asm `rdtsc` 发射
            // `0f 31`（emit_lines.rs），没这条臂时它会落 fallback 并把后面切碎。
            0x31 => Some((self.pos - start, "rdtsc".into())),
            0x0B => Some((self.pos - start, "ud2".into())),
            0x1F => {
                let _ = self.take();
                Some((self.pos - start, "nop".into()))
            },
            0x80..=0x8F => {
                // 全 16 个条件码都有名字（`0f 80/81/8a/8b` → objdump `jo/jno/jp/jnp`，
                // `as` 组装同一行得回同串字节）。旧表只写了 10 个，其余归到
                // 「jcc」这个不存在的助记词上 —— 而 `0f 82`（jb）连进都不进这一臂。
                let cc = format!("j{}", CC_NAMES[(op - 0x80) as usize]);
                let rel = self.take_i32()?;
                let end = off + (self.pos - start) as u64;
                Some((
                    self.pos - start,
                    format!("{cc} {}", self.jump_target(off + 2, rel, end)),
                ))
            },
            0xAE => {
                let rex = self.parse_rex();
                self.decode_group_ae(start, &rex)
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
            // SSE 族的入口白名单。**与 REX 入口共用 [`sse_op`]**：上一版这里是一张
            // 字面量列表，改了 `decode_sse` 而忘在这里加 op 码的话完全不起作用
            // （实测全语料未读数 2046 → 2046，一动没动）。
            op if sse_op(op) => {
                self.pos = start;
                self.decode_sse(start, pfx)
            },
            // 0F 18 / 0F 0D：hint-nop 与 prefetch 族（非-REX 入口）。
            0x18 | 0x0D => {
                let rex = Rex::default();
                self.decode_group_hint(start, pfx, &rex, op)
            },
            // BSWAP（非-REX 入口）。寄存器号在 opcode 低 3 位，**没有 ModRM**。
            0xC8..=0xCF => {
                let rex = Rex::default();
                self.decode_bswap(start, pfx, &rex, op)
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
            // BSF / BSR（`0F BC` / `0F BD`）：扫位起点/终点，dcc 在 __builtin_ctz/clz
            // 路径上会发。宽度由 REX.W 定，其他与 imul 同构。
            0xBC | 0xBD => {
                let rex = self.parse_rex();
                let (rm, r) = self.modrm_gp(&rex, if rex.w { 8 } else { 4 })?;
                let mn = if op == 0xBC { "bsf" } else { "bsr" };
                Some(self.finish(start, pfx, &format!("{mn} {rm}, {r}")))
            },
            // SHLD / SHRD（双入口之一）。本路径的开头 `peek() == 0x0F` 已经保证了
            // 没有 REX（带 REX 的整体走 decode_rex → decode_two_byte_rex），所以这里
            // 用 Rex::default() 而不是再 parse_rex()：0F 后面的字节是 ModRM，它落在
            // 0x40..=0x4F（`shld %cl,%eax,0x8(%rax)` = `0f a5 40 08`）时会被误当成 REX 吞掉。
            0xA4 | 0xA5 | 0xAC | 0xAD => {
                let rex = Rex::default();
                self.decode_shld_shrd(start, pfx, &rex, op)
            },
            // 唯一兜底出口：整条长度由 [`Dec::unknown_len`] 算完再报 UNKNOWN（方法论
            // ③：不消费完 ModRM+disp+imm 就会把一条指令切成两段残渣）。
            _ => Some(self.unknown_from(start)),
        }
    }

    fn decode_sse(&mut self, start: usize, _pfx: &Prefixes) -> Option<(usize, String)> {
        let pfx = self.take_prefixes();
        let rex = self.parse_rex();
        let _ = self.take(); // 0x0F
        let op = self.take()?;
        // 0x66/0xF2/0xF3 在此是 SSE 指令编码的一部分，不是独立前缀助记符。
        //
        // 前缀决定「打包/标量 × 单/双精度」四格：无=ps、66=pd、F3=ss、F2=sd。
        // 大多数算术助记词就是「词根 + 该后缀」（addps/addpd/addss/addsd、
        // xorps/xorpd/xorss/xorsd …）；只有 load/store 一类例外（movups/movupd/
        // movss/movsd、movaps/movapd）需要逐条列表。名字必须与**发射端同一拼写**：
        // 本解码器只用于自家 `-S` 读数与矩阵断言，名字猜错比报 UNKNOWN 更糟（后者
        // 会被「读不出来就判红」接住，前者会默地让断言跟错误的读数对齐）。
        let sfx = if pfx.repne {
            "sd"
        } else if pfx.rep {
            "ss"
        } else if pfx.o16 {
            "pd"
        } else {
            "ps"
        };
        // 比较类用的是 legacy SSE2 的 66 前缀而不是 F2：`0F 2E` = UCOMISS、
        // `66 0F 2E` = UCOMISD（同样 `0F 2F` = COMISS、`66 0F 2F` = COMISD）。
        let wide = pfx.o16 || pfx.repne;
        match op {
            0x10 => {
                let (dst, src) = self.modrm_xmm(&rex)?;
                let mn = match (pfx.repne, pfx.rep, pfx.o16) {
                    (true, _, _) => "movsd",
                    (_, true, _) => "movss",
                    (_, _, true) => "movupd",
                    _ => "movups",
                };
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            },
            0x11 => {
                let (src, dst) = self.modrm_xmm(&rex)?;
                let mn = match (pfx.repne, pfx.rep, pfx.o16) {
                    (true, _, _) => "movsd",
                    (_, true, _) => "movss",
                    (_, _, true) => "movupd",
                    _ => "movups",
                };
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            },
            0x28 => {
                // MOVAPS/MOVAPD（打包对齐 load）、MOVSS（F3）。
                let (dst, src) = self.modrm_xmm(&rex)?;
                let mn = if pfx.rep {
                    "movss"
                } else if pfx.repne {
                    "movsd"
                } else if pfx.o16 {
                    "movapd"
                } else {
                    "movaps"
                };
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            },
            0x29 => {
                let (src, dst) = self.modrm_xmm(&rex)?;
                let mn = if pfx.rep {
                    "movss"
                } else if pfx.repne {
                    "movsd"
                } else if pfx.o16 {
                    "movapd"
                } else {
                    "movaps"
                };
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            },
            0x2E | 0x2F => {
                let (dst, src) = self.modrm_xmm(&rex)?;
                let root = if op == 0x2E { "ucomi" } else { "comi" };
                let mn = format!("{root}{}", if wide { "sd" } else { "ss" });
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            },
            // UNPCKL/UNPCKH：无 66 格 = ps，66 格 = pd（F2/F3 格 objdump 直报
            // `(bad)`，所以不猜）。`0f 14 c1` → `unpcklps %xmm1,%xmm0`、
            // `66 0f 14 c1` → `unpcklpd %xmm1,%xmm0`、内存形 `0f 14 48 10` →
            // `unpcklps 0x10(%rax),%xmm1`（都是 as+objdump 反读）。
            0x14 | 0x15 => {
                if pfx.rep || pfx.repne {
                    return Some(self.unknown_from(start));
                }
                let (dst, src) = self.modrm_xmm(&rex)?;
                let root = if op == 0x14 { "unpckl" } else { "unpckh" };
                let mn = format!("{root}{}", if pfx.o16 { "pd" } else { "ps" });
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            },
            // 0F 3A 三字节转义。只补 `as` 能明确反读的那四格：66 + op3 ∈ {08,09,0A,0B}
            // = roundps/roundpd/roundss/roundsd（imm8 在 ModRM(+disp) **之后**）。
            // 其余 op3（palignr/pshufb/pcmpeqq/extractps/pdep/pext/packusdw…）需要
            // 逐格验证，且很多在不带 66 的格子上；本语料零命中，不猜。
            0x3A => {
                if !pfx.o16 || pfx.rep || pfx.repne {
                    return Some(self.unknown_from(start));
                }
                let op3 = self.take()?;
                let mn = match op3 {
                    0x08 => "roundps",
                    0x09 => "roundpd",
                    0x0A => "roundss",
                    0x0B => "roundsd",
                    _ => return Some(self.unknown_from(start)),
                };
                let (dst, src) = self.modrm_xmm(&rex)?;
                let imm = self.take()? as i32;
                Some((self.pos - start, format!("{mn} ${imm}, {src}, {dst}")))
            },
            // `66 0F xx` 打包整数算术族（表见 [`packed_int_mnemonic`]）。
            op if packed_int_mnemonic(op).is_some() => {
                let mn = match packed_int_mnemonic(op) {
                    Some(m) => m,
                    None => return Some(self.unknown_from(start)),
                };
                if !pfx.o16 || pfx.rep || pfx.repne {
                    // 不带 66 的是 MMX 格子（`0f fe c1` → `paddd %mm1,%mm0`），那是
                    // 另一张表（需 %mm 命名且多个 op 码在 MMX 里语义不同），本语料
                    // 零命中 ⇒ 整条消费后照实报 UNKNOWN，不当“读对了”。
                    return Some(self.unknown_from(start));
                }
                let (dst, src) = self.modrm_xmm(&rex)?;
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            },
            0x2A => {
                // CVTSI2SS/CVTSI2SD：源是**通用寄存器**（或内存），所以不能用
                // modrm_xmm（它把 r/m 也命名成 %xmm）。而且只有 F3/F2 两格是
                // cvtsi2*：无 66 与 66 格的 `0F 2A` 是 MMX 的 CVTPI2PS/CVTPI2PD
                // （`0f 2a c1` → objdump `cvtpi2ps %mm1,%xmm0`）。旧代码给这四个格子
                // 统一拼成 `cvtsi2{sfx}`，其中的 `cvtsi2ps`/`cvtsi2pd` 在本机
                // binutils 的读数里根本不存在 —— 那是“猜名字”的典型。
                if !(pfx.rep || pfx.repne) {
                    return Some(self.unknown_from(start));
                }
                let gp_w = if rex.w { 8 } else { 4 };
                let (xmm, src) = self.modrm_xmm_gp(&rex, gp_w)?;
                let mn = if pfx.rep { "cvtsi2ss" } else { "cvtsi2sd" };
                Some((self.pos - start, format!("{mn} {src}, {xmm}")))
            },
            0x2C => {
                // 同 0x2A：只有 F3/F2 格是 cvttss2si/cvttsd2si，**目的是 GP 寄存器**
                // （`f3 0f 2c c1` → `cvttss2si %xmm1,%eax`）。旧代码把两个操作数都
                // 用 modrm_xmm 命名，读出来是 `cvttss2si %rcx,%xmm0` —— 两边都错。
                // 无 66 / 66 格是 MMX 的 cvttps2pi/cvttpd2pi（目的叫 %mm0），不补。
                if !(pfx.rep || pfx.repne) {
                    return Some(self.unknown_from(start));
                }
                let mn = if pfx.rep { "cvttss2si" } else { "cvttsd2si" };
                let (src, dst) = self.modrm_cvttrunc(&rex, if rex.w { 8 } else { 4 })?;
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            },
            0x5A => {
                let (dst, src) = self.modrm_xmm(&rex)?;
                let mn = match (pfx.rep, pfx.repne, pfx.o16) {
                    (true, _, _) => "cvtss2sd",
                    (_, true, _) => "cvtsd2ss",
                    (_, _, true) => "cvtpd2ps",
                    _ => "cvtps2pd",
                };
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            },
            0x51 | 0x57 | 0x58 | 0x59 | 0x5C | 0x5D | 0x5E | 0x5F => {
                // 算术族：sqrtps/xorps/addps/mulps/subps/minps/divps/maxps 及其 pd/ss/sd
                // 变体，全部等于「词根 + 前缀后缀」。
                // 旧解码表完全没这些条目，所以 `f2 0f 58`（addsd）、`66 0f 57`（xorpd）、
                // `0f 57`（xorps）在 `-S` 里整批报 UNKNOWN（实测全语料 3.3% 行读不出来，
                // 这就是最大的几块）。`0x5E`（div）一度被漏：只补了 51/57/58/59/5C/5D/5F，
                // 复测时 `f2 0f 5e`/`f3 0f 5e` 仍在残留里 —— 同一个族里漏一项就会留在清单上。
                let (dst, src) = self.modrm_xmm(&rex)?;
                let root = match op {
                    0x51 => "sqrt",
                    0x57 => "xor",
                    0x58 => "add",
                    0x59 => "mul",
                    0x5C => "sub",
                    0x5D => "min",
                    0x5E => "div",
                    _ => "max",
                };
                let mn = format!("{root}{sfx}");
                Some((self.pos - start, format!("{mn} {src}, {dst}")))
            },
            0xD6 => {
                // `66 0F D6 /r` = MOVQ m64 ← xmm（存回），与 `0F D6` 的 legacy MMX
                // 形式同拼写；方向与 0x11（store）一致：先寄存器后内存。
                let (src, dst) = self.modrm_xmm(&rex)?;
                Some((self.pos - start, format!("movq {src}, {dst}")))
            },
            0x6F | 0x7F => {
                // `F3 0F 6F /r` = movdqu xmm ← r/m，`F3 0F 7F /r` = movdqu r/m ← xmm；
                // 0x66 同形但是 movdqa。后端存/取 SSE 临时值就走这两条，解码器不认它们
                // 的话，整个 vector 帧通路在 `-S` 里是一片 UNKNOWN。
                let (reg_name, mem) = self.modrm_xmm(&rex)?;
                let mn = if pfx.rep {
                    "movdqu"
                } else if pfx.o16 {
                    "movdqa"
                } else {
                    "movq"
                };
                if op == 0x6F {
                    Some((self.pos - start, format!("{mn} {mem}, {reg_name}")))
                } else {
                    Some((self.pos - start, format!("{mn} {reg_name}, {mem}")))
                }
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
            // 唯一兜底出口：整条长度由 [`Dec::unknown_len`] 算完再报 UNKNOWN（方法论
            // ③：不消费完 ModRM+disp+imm 就会把一条指令切成两段残渣）。
            _ => Some(self.unknown_from(start)),
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
            // 本路径的 `op` 已在函数开头 take 过：下面四条以前各带一句 `self.take()`，
            // 那等于**多吞下一条指令的首字节**并把长度报大 1 —— 从这一条起整页错位
            // （残留清单里那批 `4d 11 d8`、`4d c8 49 63 4d`、`4d 19 df`、`4d a0/a8` 零碎
            // 字节都是 REX 开头的前缀被前面多吃一字节后重读的产物）。名字与宽度都取自
            // `as` + `objdump -d` 实测：`98`→cwtl、`66 98`→cbtw、`48 98`→cltq。
            0x98 => {
                let mn = if rex.w {
                    "cltq"
                } else if pfx.o16 {
                    "cbtw"
                } else {
                    "cwtl"
                };
                Some((self.pos - start, mn.into()))
            },
            0x99 => Some((self.pos - start, "cqo".into())),
            // --- INT3 ---
            // inline asm 的 `int3`（`emit_asm_*` 里有发射点）与陷阱填充都走这条。
            0xCC => Some((self.pos - start, "int3".into())),
            // --- PUSHF/POPF ---
            0x9C => Some((self.pos - start, "pushfq".into())),
            0x9D => Some((self.pos - start, "popfq".into())),
            // 字符串族：`f3 48 ab`（dcc 自己发射的 rep stos %rax 清零循环）以前没臂，
            // 落 fallback 只吞 2 字节 ⇒ 从这里起整页错位。拼写与 legacy 路径共用一条
            // （包括 `rep movsb/movsl/movsq` —— 它们以前在本文件里是另一套只念助记符的
            // 写法，同一族指令两种拼法就是下一次漂移的起点）。
            // 注意 `op` 已在函数开头 take 过，这里不能再 take。
            op @ (0xA4 | 0xA5 | 0xA6 | 0xA7 | 0xAA | 0xAB | 0xAE | 0xAF) => {
                Some((self.pos - start, Self::string_op_text(op, &pfx, rex.w)?))
            },
            0xD8..=0xDF => self.decode_x87(start, op),
            0xF8 => Some((self.pos - start, "clc".into())),
            0xF9 => Some((self.pos - start, "stc".into())),
            0xFA => Some((self.pos - start, "cli".into())),
            0xFB => Some((self.pos - start, "sti".into())),
            0xFC => Some((self.pos - start, "cld".into())),
            0xFD => Some((self.pos - start, "std".into())),
            // --- Fallback ---
            // 唯一兜底出口：整条长度由 [`Dec::unknown_len`] 算完再报 UNKNOWN（方法论
            // ③：不消费完 ModRM+disp+imm 就会把一条指令切成两段残渣）。
            _ => Some(self.unknown_from(start)),
        }
    }

    /// 字符串指令（MOVS / STOS / CMPS / SCAS）的 objdump 拼写。
    ///
    /// 这些编码**不带 ModRM 也带不了立即数**，长度就是「前缀 + 1 字节 opcode」，所以
    /// 两条解码路径（legacy 与 REX）只共用这份**文本**知识，不在两处各写一遍。
    ///
    /// 拼写全部取自 `as` + `objdump -d` 实测（本文件的规矩：名字拿不准就继续报
    /// UNKNOWN，猜错的名字会让自家断言与错误读数对齐）：
    ///   `aa`→`stos %al,%es:(%rdi)`   `f3 aa`→`rep stos %al,%es:(%rdi)`
    ///   `48 ab`→`stos %rax,...`  `66 ab`→`stos %ax,...`
    ///   `ae`→`scas %es:(%rdi),%al`  `f3 ae`→`repz scas ...`（注意是 repz 不是 rep）
    ///   `a6`→`cmpsb %es:(%rdi),%ds:(%rsi)`  `f3 a6`→`repz cmpsb ...`
    ///   `a4`→`movsb %ds:(%rsi),%es:(%rdi)`  `f3 a4`→`rep movsb ...`
    ///
    /// 为什么必须有：旧解码器只有 `rep movs*` 两条，于是 **dcc 自己发射的** `f3 48 ab`
    /// （栈帧里那个 `rep stos %rax` 清零循环，见 `emit/func_setup.rs`）落进 fallback，
    /// 只吞 2 字节不消费后续 ⇒ 从这一条起整份反汇编错位；`check_disasm_coverage.sh` 的
    /// 残留清单里那些 `ae`、`a0`、`cb`、`45 90` 零碎字节就是同一条指令被切碎的重读产物。
    fn string_op_text(op: u8, pfx: &Prefixes, w: bool) -> Option<String> {
        // 尺寸后缀（movs/cmps 用）与寄存器写法（stos/scas 用），按 REX.W / 66 / 默认三档
        let (sfx, reg) = if w {
            ("q", "%rax")
        } else if pfx.o16 {
            ("w", "%ax")
        } else {
            ("l", "%eax")
        };
        let (repz, rep) = if pfx.rep { ("repz ", "rep ") } else { ("", "") };
        match op {
            0xA4 => Some(format!("{rep}movsb  %ds:(%rsi),%es:(%rdi)")),
            0xA5 => Some(format!("{rep}movs{sfx}  %ds:(%rsi),%es:(%rdi)")),
            0xA6 => Some(format!("{repz}cmpsb  %es:(%rdi),%ds:(%rsi)")),
            0xA7 => Some(format!("{repz}cmps{sfx}  %es:(%rdi),%ds:(%rsi)")),
            0xAA => Some(format!("{rep}stos   %al,%es:(%rdi)")),
            0xAB => Some(format!("{rep}stos   {reg},%es:(%rdi)")),
            0xAE => Some(format!("{repz}scas   %es:(%rdi),%al")),
            0xAF => Some(format!("{repz}scas   %es:(%rdi),{reg}")),
            _ => None,
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
            // RDTSC 的 REX 入口（与非-REX 入口同一拼写；`rdtsc` 不受 REX 影响）。
            0x31 => Some((self.pos - start, "rdtsc".into())),
            // REX 前缀的 SSE 标量/vector 指令（0x66/0xF2/0xF3 为 opcode 选择符）
            // 统一交回 decode_sse 处理，避免把 F2/F3/66 误当成独立前缀助记符。
            // **与非-REX 入口共用 [`sse_op`]**：只补一张表而漏另一入口的话，带 REX 的
            // 那一批（如 `66 41 0f 28`、`66 0f fe` 的 r13 形）仍会落进 fallback。
            op if sse_op(op) => {
                self.pos = start;
                self.decode_sse(start, pfx)
            },
            // 0F 18 / 0F 0D：hint-nop 与 prefetch 族（REX 入口：dcc 对 %r8- 基址
            // 会带 REX.B）。rex 已由 decode_rex 解好传入，不能再 parse_rex 一次。
            0x18 | 0x0D => self.decode_group_hint(start, pfx, rex, op),
            // BSWAP r32/r64（REX 入口）。`48 0f c8` → `bswap %rax`、
            // `41 0f c8` → `bswap %r8d`（as+objdump 反读，都不带注记）。
            0xC8..=0xCF => self.decode_bswap(start, pfx, rex, op),
            // 串行化 / mxcsr 族与非-REX 路径共用同一实现（旧代码只在非-REX 路径上
            // 有 mfence 字节序列特判，带 REX 的同类一律落进只吞 2 字节的 fallback）。
            0xAE => self.decode_group_ae(start, rex),
            // BSF / BSR 与非-REX 路径同构：旧表只在非-REX 路径上有臂，所以
            // `48 0f bd c0`（bsrq %eax,%eax）仍报 UNKNOWN。两路径同表同一实现，
            // 不再按字节序列特判。
            0xBC | 0xBD => {
                let (rm, r) = self.modrm_gp(rex, if rex.w { 8 } else { 4 })?;
                let mn = if op == 0xBC { "bsf" } else { "bsr" };
                Some(self.finish(start, pfx, &format!("{mn} {rm}, {r}")))
            },
            // SHLD / SHRD（双入口的另一条：带 REX 的那一批）。实测未读数里最大两堆
            // 就是 `48 0f a5`/`48 0f ad`（int128 移位路径发射 shld/shrd），只补非-REX
            // 入口它们一条也不会少。rex 由调用方（decode_rex）已解好传入。
            0xA4 | 0xA5 | 0xAC | 0xAD => self.decode_shld_shrd(start, pfx, rex, op),
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
            // 唯一兜底出口：整条长度由 [`Dec::unknown_len`] 算完再报 UNKNOWN（方法论
            // ③：不消费完 ModRM+disp+imm 就会把一条指令切成两段残渣）。
            _ => Some(self.unknown_from(start)),
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
            0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D => {
                let mn = match op {
                    0x05 => "add",
                    0x0D => "or",
                    0x15 => "adc",
                    0x1D => "sbb",
                    0x25 => "and",
                    0x2D => "sub",
                    0x35 => "xor",
                    _ => "cmp",
                };
                let imm = self.take_i32()?;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, %eax")))
            },
            // ALU accumulator, imm8：`04/0C/14/1C/24/2C/34/3C ib`。legacy 路径只有上面
            // 那个 `%eax` 的 imm32 族，没有 imm8 兄弟 ⇒ `34 01`（adc $0x1,%al）、
            // `14 24`（adc $0x24,%al）这类全报 UNKNOWN（实测两类共 30+ 处，且只吞
            // 1 字节不会错位，但仍算「读不出来」）。操作数寄存器是 %al（0x66 时 %ax），
            // 与 imm32 臂同一张 op 表。
            0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x34 | 0x3C => {
                let mn = match op {
                    0x04 => "add",
                    0x0C => "or",
                    0x14 => "adc",
                    0x1C => "sbb",
                    0x24 => "and",
                    0x2C => "sub",
                    0x34 => "xor",
                    _ => "cmp",
                };
                let acc = if pfx.o16 { "%ax" } else { "%al" };
                let imm = self.take_i8()?;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {acc}")))
            },
            // ret imm16（`C2 iw`）与 retf imm16（`CA iw`）：props 表认识这一族而解码臂
            // 没有 ⇒ `c2` 开头的 14 处报 UNKNOWN。
            0xC2 | 0xCA => {
                let mn = if op == 0xC2 { "ret" } else { "retf" };
                let imm = self.take_i16()?;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}")))
            },
            // jcc rel8：短条件跳转在 legacy 路径上**根本没有臂**（长形式 `0F 80..=8F`
            // 只存在于 two-byte 路径），所以 `74 07`（je）先被读成一条 UNKNOWN，
            // 紧跟的 `07, 0x48` 又是下一句被切碎的重读 —— 一条缺失会连带造成整段
            // 错位，这就是为什么未读数不能只看条数要看锤点。长度与 rel8 的
            // site/end 算法照 `0xEB` 同一惯例。
            0x70..=0x7F => {
                let cc = match op - 0x70 {
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
                let rel = self.take_i8()? as i32;
                let off = self.base + start as u64;
                let target = self.jump_target(off + 1, rel, off + 2);
                Some(self.finish(start, pfx, &format!("{cc} {target}")))
            },
            // 字符串 / 标志位族：之前只补了 REX 路径，legacy 路径仍缺 ⇒ `fc`、`a4`、
            // `a5` 落进下面这个只吞 2 字节的 fallback，后面字节序列全线错位（实测剩余
            // 未读数里 `fc, 0x48`、`fc, 0xf3`、`a4, 0x49` 就是这批残渣）。`op` 已在
            // 函数开头 take 过，这里不能再 take。
            0xA4 => {
                let mn = if pfx.rep { "rep movsb" } else { "movsb" };
                Some(self.finish(start, pfx, mn))
            },
            0xA5 => {
                let mn = match (pfx.rep, pfx.o16) {
                    (true, true) => "rep movsd",
                    (true, false) => "rep movsl",
                    (false, true) => "movsd",
                    (false, false) => "movsl",
                };
                Some(self.finish(start, pfx, mn))
            },
            0xF8 => Some(self.finish(start, pfx, "clc")),
            0xF9 => Some(self.finish(start, pfx, "stc")),
            0xFA => Some(self.finish(start, pfx, "cli")),
            0xFB => Some(self.finish(start, pfx, "sti")),
            0xFC => Some(self.finish(start, pfx, "cld")),
            0xFD => Some(self.finish(start, pfx, "std")),
            // 唯一兜底出口：整条长度由 [`Dec::unknown_len`] 算完再报 UNKNOWN（方法论
            // ③：不消费完 ModRM+disp+imm 就会把一条指令切成两段残渣）。
            _ => Some(self.unknown_from(start)),
        }
    }

    /// `0F 18 /digit` 与 `0F 0D /digit`： prefetch 族与多字节 hint-nop。两族形状一样
    /// （ModRM 的 reg 字段是 /digit，r/m 是内存；mod=3 在 64 位下只有 0F 18 还有意义）。
    ///
    /// 名字全部来自本机 `as` + `objdump -d` 反读（不是记的）：
    ///   `0f 18 00/08/10/18` → `prefetchnta/prefetcht0/prefetcht1/prefetcht2 (%rax)`，
    ///   `0f 18 20/28/30/38` → `nopl (%rax)`（/4../7 是 hint-nop，不是 prefetch），
    ///   `0f 18 c0/c1/c8` → `nop %eax`/`nop %ecx`（mod=3 时操作数名**取 rm 字段**），
    ///   `66 0f 18 c1` → `nop %cx`；`0f 0d 00/08/10/18/20` → `prefetch/prefetchw/
    ///   prefetchwt1/prefetch/prefetch (%rax)`（/3../7 也被读成 `prefetch`）。
    /// 不补的形状及原因：
    ///   * `48 0f 18 00` / `44 0f 18 00` —— objdump 对多余位打的是字面注记
    ///     `rex.W prefetchnta (%rax)` / `rex.R …`，那不是 GAS 合法文本（-S 输出要能回炸）；
    ///   * `0f 0d c1` —— objdump 报 `(bad)`；
    ///   * `f3 0f 18 c1` —— objdump 打 `repz nop %ecx`（前缀被当独立助记词），
    ///     与我们的 `rep` 拼法不一致，不赌。
    /// 以上都走 [`Self::unknown_from`]：整条消费完再报 UNKNOWN。
    fn decode_group_hint(
        &mut self,
        start: usize,
        pfx: &Prefixes,
        rex: &Rex,
        op: u8,
    ) -> Option<(usize, String)> {
        if rex.w || rex.r {
            return Some(self.unknown_from(start));
        }
        let modrm = *self.code.get(self.pos)?;
        let mod_ = modrm >> 6;
        let digit = (modrm >> 3) & 7;
        if mod_ == 3 {
            // 只有 0F 18 的寄存器形有名字（`nop {rm}`）；0F 0D 的 mod=3 是 (bad)。
            if op != 0x18 || pfx.rep || pfx.repne || pfx.lock || rex.w {
                return Some(self.unknown_from(start));
            }
            self.take()?;
            let w = if pfx.o16 { 2 } else { 4 };
            let r = self.reg(rex, (modrm & 7) | if rex.b { 8 } else { 0 }, w);
            return Some(self.finish(start, pfx, &format!("nop {r}")));
        }
        // 内存形：上面反读的那些都发生在「除 REX.B 外无前缀」的格子上。
        if pfx.o16 || pfx.rep || pfx.repne || pfx.lock {
            return Some(self.unknown_from(start));
        }
        self.take()?;
        let mem = self.decode_mem(rex, mod_, modrm & 7, 4)?;
        let mn = match (op, digit) {
            (0x18, 0) => "prefetchnta",
            (0x18, 1) => "prefetcht0",
            (0x18, 2) => "prefetcht1",
            (0x18, 3) => "prefetcht2",
            (0x18, 4..=7) => "nopl",
            (0x0D, 1) => "prefetchw",
            (0x0D, 2) => "prefetchwt1",
            // 0F 0D 的 /0 与 /3../7 都读回 `prefetch`（binutils 对未分配的 digit
            // 仍按 AMD 的 3DNow! prefetch 打），这一条是实测不是推论。
            (0x0D, 0) | (0x0D, 3..=7) => "prefetch",
            _ => return Some(self.unknown_from(start)),
        };
        Some((self.pos - start, format!("{mn} {mem}")))
    }

    /// `0F C8+r` = BSWAP r。寄存器号在 opcode 低 3 位（+REX.B），**没有 ModRM**。
    /// `0f c8` → `bswap %eax`、`0f cf` → `bswap %edi`、`48 0f cf` → `bswap %rdi`、
    /// `41 0f c8` → `bswap %r8d`、`66 0f c8` → `bswap %ax`（均为 as+objdump 反读）。
    fn decode_bswap(
        &mut self,
        start: usize,
        pfx: &Prefixes,
        rex: &Rex,
        op: u8,
    ) -> Option<(usize, String)> {
        self.take()?;
        let w = self.eff_width(rex, pfx);
        let r = self.reg(rex, (op & 7) | if rex.b { 8 } else { 0 }, w);
        Some(self.finish(start, pfx, &format!("bswap {r}")))
    }

    /// `0F A4`/`0F A5`/`0F AC`/`0F AD` = SHLD/SHRD。`0F A4`/`0F AC` 的计数是紧随其后的
    /// imm8，`0F A5`/`0F AD` 的计数是 `%cl`；ModRM 的 **reg 字段就是第二个操作数**
    /// （源寄存器），不是 `/digit`。
    ///
    /// 助记词与操作数次序都是 `as` 组装 + `objdump -d` 反读定的（本机 binutils）：
    ///   `0f a4 c3 03` → `shld $0x3,%eax,%ebx`、`48 0f a5 c3` → `shld %cl,%rax,%rbx`、
    ///   `48 0f ac c3 03` → `shrd $0x3,%rax,%rbx`、`48 0f ad c3` → `shrd %cl,%rax,%rbx`、
    ///   `66 0f a4 c3 03` → `shld $0x3,%ax,%bx`（16 位形式不带后缀）。
    /// 还把 reg 字段 0..7 逐个扫了一遍（`48 0f a5 c0/c1/…/ff`）：binutils 对每个 reg
    /// 值都读回 `shld`，源操作数就是 reg 字段那个寄存器。所以过去那条「reg=/4 时是
    /// 另一条 SHL/SHR」的说法在本机读不出来，按 /digit 分发就会念错名字。
    fn decode_shld_shrd(
        &mut self,
        start: usize,
        pfx: &Prefixes,
        rex: &Rex,
        op: u8,
    ) -> Option<(usize, String)> {
        let width = self.eff_width(rex, pfx);
        let (src, dst) = self.modrm_gp(rex, width)?;
        let mn = if op == 0xA4 || op == 0xA5 { "shld" } else { "shrd" };
        if op == 0xA4 || op == 0xAC {
            // imm8 在 ModRM 之后（`0f a4 c3 03`），漏读它会把计数吐给下一条指令。
            let imm = self.take()? as i32;
            return Some(self.finish(start, pfx, &format!("{mn} ${imm}, {src}, {dst}")));
        }
        Some(self.finish(start, pfx, &format!("{mn} %cl, {src}, {dst}")))
    }

    /// `0F AE /digit`，按 ModRM 的 reg 字段选形。**映射不是靠记，是用 GAS assemble
    /// + objdump 反读定的**（`0f ae 07/0f/17/1f/e8/f0/f8` 分别 = fxsave/fxrstor/
    /// ldmxcsr/stmxcsr/lfence/mfence/sfence）：
    ///   /0 fxsave、/1 fxrstor、/2 ldmxcsr、/3 stmxcsr、/5 lfence、/6 mfence、/7 sfence。
    /// 第一版按记忆写成 3=ldmxcsr、4=stmxcsr，那会把 stmxcsr 念成 ldmxcsr —— 比报
    /// UNKNOWN 危险得多（猜错的名字会让自家断言与错误读数对齐）。/4 是 AMD 重叠编码
    /// (lzcnt/sse)，无法单从字节定形，照实报 UNKNOWN。
    ///
    /// 旧代码只写了 `0xAE if peek == 0xF0` 一条按字节组合的特判（实际只认 mfence），
    /// 其余 /digit 全落进「吞 2 字节、不消费 ModRM」的 fallback，从这一条起整份错位。
    fn decode_group_ae(&mut self, start: usize, rex: &Rex) -> Option<(usize, String)> {
        let modrm = self.take()?;
        let mn = match (modrm >> 3) & 7 {
            0 => "fxsave",
            1 => "fxrstor",
            2 => "ldmxcsr",
            3 => "stmxcsr",
            5 => "lfence",
            6 => "mfence",
            7 => "sfence",
            // 认不得时也必须把整条指令（ModRM + 可能的 disp/imm）消费完再报 UNKNOWN：
            // 从 start 重算长度会把 ModRM 吐回去，于是一条指令被读成两段残渣
            // （实测残留清单里 `0f ae` 与 `e0` 成对出现就是这个错位）。
            _ => {
                let at = self.pos - 1;
                self.pos = at + consume_modrm_and_optional_parts(self.code, at);
                let bytes: Vec<String> = self.code[start..self.pos]
                    .iter()
                    .map(|b| format!("0x{b:02x}"))
                    .collect();
                return Some((self.pos - start, format!("# UNKNOWN {}", bytes.join(", "))));
            },
        };
        if modrm >> 6 == 3 {
            // 串行化指令的 ModRM 是 mod=11、rm=000，无操作数
            return Some((self.pos - start, mn.into()));
        }
        let mem = self.decode_mem(rex, modrm >> 6, modrm & 7, 4)?;
        Some((self.pos - start, format!("{mn} {mem}")))
    }

    /// 把 `start` 处按「读不出来」发射，并把游标推到该序列之后。
    /// 抽出来是因为三条路径（非-REX / REX / SSE）都需要在「认不得但不猜」时复用它。
    fn unknown_from(&mut self, start: usize) -> (usize, String) {
        let len = self.unknown_len(start);
        self.pos = start + len;
        let bytes: Vec<String> = self.code[start..self.pos]
            .iter()
            .map(|b| format!("0x{b:02x}"))
            .collect();
        (len, format!("# UNKNOWN {}", bytes.join(", ")))
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
    ///
    /// 每一条都用 `as` 组 `.byte …` + `objdump -d` 看过它**实际吃掉几字节**（本机
    /// binutils，不是凭记忆抄表）。这张表错一格的后果不是「少读一条」而是「从这一条
    /// 起整份错位」：实测残留里 `cc 48 c7`、`cb 48 8b`、`c8 48 63 c0 41 89 45` 都是
    /// 0xCC/0xCB/0xC8 被当成「有 ModRM」切出来的残渣。
    /// 几条形与直觉相反（逐条验过）：
    ///   * `cc` = int3、1 字节；`c3` ret 1 字节，但 `c2` 是 `ret $imm16`（3 字节），
    ///     `ca` = `lret $imm16`（3 字节），`cb` = `lret`（objdump 只吃 1 字节）；
    ///   * `c8` = `enter $imm16,$imm8`，共 4 字节；
    ///   * `a0..=a3` 是 moffs：无 ModRM，后面跟 **8 字节绝对地址**（`a1 …` 共 9 字节，
    ///     objdump 打 `movabs 0x…,%eax`）；
    ///   * 字符串族 `a4..=af` 一律**无 ModRM**（`a6` → `cmpsb %es:(%rdi),%ds:(%rsi)`，
    ///     `ae` → `scas %es:(%rdi),%al`），只有 `a8`(imm8) / `a9`(imm32) 带立即数；
    ///   * `90..=97` 是 `xchg eax, r`（无 ModRM）；`e0..=e3` 是 loopne/loope/loop/jecxz
    ///     的 rel8（2 字节）。
    fn legacy_opcode_props(op1: u8) -> (bool, usize) {
        match op1 {
            // 无 ModRM、无立即数
            0x06 | 0x07 | 0x0E | 0x16 | 0x17 | 0x1E | 0x1F | 0x27 | 0x2F | 0x37 | 0x3F => {
                (false, 0)
            },
            0x60 | 0x61 | 0x6C | 0x6D | 0x6E | 0x6F => (false, 0),
            // xchg eax,r（90..=97）与标志/跳变族（98..=9F，9A 在 64 位下 invalid）
            0x90..=0x97 | 0x98..=0x99 | 0x9B..=0x9F => (false, 0),
            // 字符串族（除 a8/a9）、ret/retf 裸形、leave、int3、xlat、in/out 隐式形、
            // hlt/cmc/clc..std
            0xA4..=0xA7 | 0xAA..=0xAF => (false, 0),
            0xC3 | 0xC9 | 0xCB | 0xCC | 0xD7 | 0xEC..=0xEF | 0xF4 | 0xF5 | 0xF8..=0xFD => {
                (false, 0)
            },
            // 无 ModRM、imm8（含 ALU acc、短跳、loop 一族、enter 之外的一族）
            0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x34 | 0x3C | 0x6A | 0xA8 => (false, 1),
            0xB0..=0xB7 | 0xCD | 0xCE | 0xD4 | 0xD5 | 0xE0..=0xE7 | 0x70..=0x7F | 0xEB => {
                (false, 1)
            },
            // 无 ModRM、imm16（ret/retf 带计数形）
            0xC2 | 0xCA => (false, 2),
            // 无 ModRM、enter 的 imm16 + imm8
            0xC8 => (false, 3),
            // 无 ModRM、imm32 / rel32
            0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D | 0x68 | 0xA9 | 0xE8 | 0xE9 => {
                (false, 4)
            },
            // mov r, imm64：`48 B8+r` 的 imm 是 8 字节，但那一形由 `decode_rex` 直接
            // 认（本表只在兜底路径上用，那里取 4 不会错到越过 REX.W 形）。
            0xB8..=0xBF => (false, 4),
            // moffs：无 ModRM，跟 8 字节绝对地址
            0xA0..=0xA3 => (false, 8),
            // 默认：含 ModRM，立即数长度由 [`Self::unknown_len`] 里的分组修正项定
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
            seg: None,
            seg_shown: false,
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

    /// 字符串指令族（MOVS/STOS/CMPS/SCAS）。字节与期望文本都是 `as` + `objdump -d`
    /// 反读出来的，不是凭记忆写的。
    ///
    /// 背后两件事都要钉住：
    ///   1. **名字** —— `repz scas` 不是 `rep scas`，`stos`/`scas` 不带尺寸字母；
    ///   2. **长度** —— 这些 opcode 不带 ModRM，所以下一条必须紧接开头。后半段的
    ///      `c3` 就是专门用来抓错位的：`f3 48 ab` 一旦落进只吞 2 字节的 fallback，
    ///      后面的 `mov $0,%eax; ret` 会被重读成残渣（那正是残留清单里那些
    ///      `ae`/`a0`/`cb` 零碎字节的来源）。
    #[test]
    fn decodes_string_instructions_and_keeps_following_stream_aligned() {
        let one = |bytes: &[u8]| -> String {
            let mut code = bytes.to_vec();
            code.push(0xC3); // ret：错位时它不会以 `ret` 出现
            disassemble_range(
                &code,
                0,
                code.len(),
                0,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &HashMap::new(),
                &HashMap::new(),
            )
        };

        let cases: &[(&[u8], &str)] = &[
            (&[0xA4], "movsb  %ds:(%rsi),%es:(%rdi)"),
            (&[0xF3, 0xA4], "rep movsb  %ds:(%rsi),%es:(%rdi)"),
            (&[0x48, 0xA5], "movsq  %ds:(%rsi),%es:(%rdi)"),
            (&[0xF3, 0x48, 0xA5], "rep movsq  %ds:(%rsi),%es:(%rdi)"),
            (&[0xA6], "cmpsb  %es:(%rdi),%ds:(%rsi)"),
            (&[0xF3, 0x48, 0xA7], "repz cmpsq  %es:(%rdi),%ds:(%rsi)"),
            (&[0xAA], "stos   %al,%es:(%rdi)"),
            (&[0x66, 0xAB], "stos   %ax,%es:(%rdi)"),
            (&[0xF3, 0x48, 0xAB], "rep stos   %rax,%es:(%rdi)"),
            (&[0xAE], "scas   %es:(%rdi),%al"),
            (&[0xF3, 0x48, 0xAF], "repz scas   %es:(%rdi),%rax"),
        ];
        for (bytes, want) in cases {
            let out = one(bytes);
            assert!(out.contains(want), "{bytes:02x?} 应念成 {want:?}，实得:\n{out}");
            // 反向对照：不能因为长度算错而多出一条 UNKNOWN。
            assert!(!out.contains("UNKNOWN"), "{bytes:02x?} 后续流被错位切碎:\n{out}");
            assert!(out.contains("ret"), "{bytes:02x?} 后面那条 ret 没读回来:\n{out}");
        }
    }

    /// `0F AE /digit` 族。这些字节序列不是凭记忆写的，而是用 `as` 组装
    /// `fxsave/fxrstor/ldmxcsr/stmxcsr/lfence/mfence/sfence (%rdi)` 后从 `objdump -d`
    /// 反读得到的。把它们固定下来是因为第一版按记忆把 /2、/3 错记成 /3、/4，
    /// 那会把 stmxcsr 念成 ldmxcsr —— 比一条 UNKNOWN 危险得多，因为猜错的名字会让
    /// 依赖本解码器的矩阵判据与错误读数对齐。/4 是 AMD 重叠编码（lzcnt/sse），
    /// 单从字节无法定形，所以它必须是 UNKNOWN，不能赌一个名字。
    #[test]
    fn decodes_group_ae_by_modrm_reg_field() {
        let code = [
            0x0F, 0xAE, 0x07, // fxsave  (%rdi)
            0x0F, 0xAE, 0x0F, // fxrstor (%rdi)
            0x0F, 0xAE, 0x17, // ldmxcsr (%rdi)
            0x0F, 0xAE, 0x1F, // stmxcsr (%rdi)
            0x0F, 0xAE, 0xE8, // lfence
            0x0F, 0xAE, 0xF0, // mfence
            0x0F, 0xAE, 0xF8, // sfence
            0x0F, 0xAE, 0xE0, // /4: AMD 重叠编码，不得赌名字
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
        for want in ["fxsave", "fxrstor", "ldmxcsr", "stmxcsr", "lfence", "mfence", "sfence"] {
            assert!(out.contains(want), "缺少 {want}：\n{out}");
        }
        // 前七条各占 3 字节，第八条必须整条报 UNKNOWN而不是被当某一条误读
        assert!(out.contains("UNKNOWN"), "AMD 重叠编码 /4 必须报 UNKNOWN：\n{out}");
        assert_eq!(
            out.matches("0xae, 0xe0").count(),
            1,
            "残余字节只能出现一次（多份 = 不消费 ModRM 后重读造成错位）：\n{out}"
        );
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
