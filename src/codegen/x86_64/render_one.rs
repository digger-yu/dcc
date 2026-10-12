// src/codegen/x86_64/render_one.rs - 单条 x86-64 机器码 → AT&T 汇编文本渲染.
//
// P9 方案 C 收尾: 本文件由 src/codegen/disasm.rs 迁移而来. 生产路径只需要
// `disassemble_one_at` (单指令解码) 与 label_map / directive_map (纯 BTreeMap
// 工具) 两组函数. `disassemble_range` 老入口已从生产路径撤下 (render_byte_range
// 直接调用 disassemble_one_at), 文件内 `#[cfg(test)]` 模块在本迁移中一并删除 ——
// 生产路径上 `render_byte_range` 已等价覆盖 disassemble_range 的循环语义.

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

    /// Shift/Rotate `/digit` 的名字（`C0/C1/D0/D1/D2/D3` 六条路径共用一张表）。
    ///
    /// 为什么是一张表而不是六份清单：本文件曾在 legacy 与 REX 两条路径上各写三份
    /// `match reg`，而 REX 那三份**同时漏了 reg=2/3**，兜底臂把它们读成一个假助记词
    /// `shift`（既不是 objdump 的名字，也不是 GAS 的，更不是 `UNKNOWN`，所以
    /// `# UNKNOWN` 计数看不见它）。与 `alu_rm_r` 是同一个形状的错误：同一张表抄四遍，
    /// 少抄的那一份永远不会自己报错。名字本身是 `as` 组装 + `objdump -d` 反读钉的：
    /// ```text
    ///   c1 10 02 -> rcll $0x2,(%rax)     c1 18 02 -> rcrl $0x2,(%rax)
    ///   48 c1 10 02 -> rclq $0x2,(%rax)  48 c1 18 02 -> rcrq $0x2,(%rax)
    ///   d0 10 -> rclb $1,(%rax)          d2 18 -> rcrb %cl,(%rax)
    ///   c0 38 02 -> sarb $0x2,(%rax)     （reg=7 是 sar，不是 shr）
    /// ```
    fn shift_name(reg: u8) -> &'static str {
        match reg {
            0 => "rol",
            1 => "ror",
            2 => "rcl",
            3 => "rcr",
            4 => "shl",
            5 => "shr",
            6 => "sal",
            _ => "sar",
        }
    }

    /// `0xFE`/`0xFF` 两组 `/digit` 的助记词，REX 与 legacy 两条路径共用一张表
    /// （本文件被「同一张表抄四遍、少抄的那份永远不会自己报错」坑过两次：ALU `/r` 漏
    /// adc/sbb、shift `/digit` 漏 rcl/rcr）。`None` = 64 位模式下**没有编码**，
    /// 调用方必须报 UNKNOWN，不得猜名字。
    ///
    /// 哪几格是 `None` 由 `as` 组 `.byte` + `objdump -d` 逐格钉（每个形状单独一个函数，
    /// 否则前一条非法字节会让后面全部同步错位）：
    /// ```text
    ///   fe c0 -> inc %al       fe d0 -> (bad)      fe e0 -> (bad)   fe f0 -> (bad)
    ///   ff c0 -> inc %eax      ff c8 -> dec %eax   ff f0 -> push %rax
    ///   ff d8 -> lcall (bad)   ff e8 -> ljmp (bad)                 ff f8 -> (bad)
    /// ```
    /// 旧实现把这些格子打成**有名字**的假读数（`ff` 这个不存在的助记词、以及把 0xFE 的
    /// /2../7 一律念成 `dec`），于是 `# UNKNOWN` 计数看不见它们 —— 与 `shift`、`f???` 是
    /// 同一个形状的盲区。全语料实测 0 行命中（`grep -cP '^\t(ff|f6|f7) '` = 0），所以
    /// 这一改不会把 `disasm-cov` 门弄红。
    fn group_fe_ff_name(op: u8, reg: u8) -> Option<&'static str> {
        match (op, reg) {
            (0xFE, 0) => Some("inc"),
            (0xFE, 1) => Some("dec"),
            (0xFF, 0) => Some("inc"),
            (0xFF, 1) => Some("dec"),
            (0xFF, 2) => Some("call"),
            (0xFF, 4) => Some("jmp"),
            (0xFF, 6) => Some("push"),
            _ => None,
        }
    }

    /// 已经把整条消费干净（ModRM/SIB/位移都读掉、`self.pos` 在指令末尾）之后，按本文件
    /// 的兜底出口格式报 UNKNOWN。与 [`Self::unknown_from`] 的差别：那条自己算长度，
    /// 这一条用调用方已经走到的位置，只负责把 `start..pos` 打成 `# UNKNOWN 0x.., 0x..`。
    fn unknown_consumed(&self, start: usize) -> String {
        let bytes: Vec<String> = self.code[start..self.pos]
            .iter()
            .map(|b| format!("0x{b:02x}"))
            .collect();
        format!("# UNKNOWN {}", bytes.join(", "))
    }

    /// `FF /2`(call)、`/4`(jmp)、`/6`(push) 的操作数渲染，REX 与 legacy 两条路径共用
    /// 一张实现（`inc`/`dec` 不走这里，它们跟着操作数宽度走）。
    ///
    /// 两条规则都是上面那份 `as` + `objdump -d` 实测钉的，缺一条 `-S` 文本就被 GAS 拒：
    ///   * **间接**转移必须带 `*`：`ff d0`→`call *%rax`、`ff e0`→`jmp *%rax`、
    ///     `ff 10`→`call *(%rax)`、`ff 20`→`jmp *(%rax)`、`ff 91 08 00 00 00`→
    ///     `call *0x8(%rcx)`、`ff 14 8b`→`call *(%rbx,%rcx,4)`。少一个星，GAS 把
    ///     `call (%rax)` 当成「直接转移的表达式」，报 `operand size mismatch for 'call'`
    ///     —— 实测本仓库 626 个可编译夹具×档位里 24 行间接转移**全部**少星，是 `-S`
    ///     文本回炸失败 30 个中的最大单一类（16 个）。`push` 不带星：`ff 30`→`push (%rax)`。
    ///   * mod=3 时寄存器名**恒取 64 位**（`0x66` 前缀才是 16 位），不跟 REX.W 的操作数
    ///     宽度：`ff d0`→`call *%rax`（不是 `%eax`）、`41 ff e2`→`jmp *%r10`、
    ///     `ff f0`→`push %rax`、`66 ff f0`→`push %ax`、`66 ff d0`→`call *%ax`。旧实现按
    ///     `width`（无 REX.W 就是 32 位）取名，读出 `call %r10d`：长度对、UNKNOWN 数不到，
    ///     但 GAS 既不接受它、也把它组装成**另一串字节**（`66 ff f0`），回炸不等价。
    fn indirect_operand(
        &self,
        rex: &Rex,
        o16: bool,
        reg: u8,
        mod_: u8,
        modrm: u8,
        mem: String,
    ) -> String {
        let operand = if mod_ == 3 {
            let rm = modrm & 7;
            self.reg(rex, rm | if rex.b { 8 } else { 0 }, if o16 { 2 } else { 8 })
                .to_string()
        } else {
            mem
        };
        let star = if matches!(reg, 2 | 4) { "*" } else { "" };
        format!("{star}{operand}")
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
        // base 必须是 `Option`：SIB 的 `mod=0/base=101` 是**没有基址**（只剩 disp32），
        // 它和「RIP 相对」不是一回事。旧实现把两者合并成同一个 `%rip`，于是
        // `48 8b 04 0d 40 00 00 00` 被念成 `0x0(%rip)`，而 objdump 的真值是
        // `mov 0x40(,%rcx,1),%rax`（绝对基址 + 变址：括号里 base 那一格是空的）。
        let mut base: Option<String> = None;
        let mut index = String::new();
        let mut scale = 1u32;
        let mut disp32_off: Option<u64> = None; // disp32 起始偏移 (用于 data 符号查找)
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
                // 无基址形式：只读 disp32，**不**贴 `%rip`（它不是 RIP 相对寻址）。
                disp32_off = Some(self.abs_off());
                disp = self.take_i32()?;
            } else {
                base = Some(
                    self.reg(rex, base_reg | if rex.b { 8 } else { 0 }, ADDR_W)
                        .to_string(),
                );
            }
        } else if mod_ == 0 && rm == 5 && !rex.b {
            // 无 REX.B: rm=5 mod=00 是 RIP-relative. 有 REX.B 时 rm_ext=13=r13,
            // 是绝对基址 (不是 RIP) — 走 else 分支.
            disp32_off = Some(self.abs_off());
            disp = self.take_i32()?;
            base = Some("%rip".to_string());
        } else {
            base = Some(self.reg(rex, rm_ext, ADDR_W).to_string());
        }
        match mod_ {
            0 => {},
            1 => disp = self.take_i8()? as i32,
            2 => disp = self.take_i32()?,
            _ => {},
        }
        // RIP-relative 时, data 符号在 disp32 起始位置; 其他情况在指令末尾
        let lookup_off = disp32_off.unwrap_or_else(|| self.abs_off());
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
        // AT&T 内存操作数的形状恒为 `[seg][disp](base[,index[,scale]])`：**段前缀与
        // 位移/符号名落在左括号之前，base/index/scale 全部落在括号之内**。
        // 三条真值（`as` 组装 + `objdump -d` 反读，不是凭记忆）：
        //   `49 63 04 8b`                 -> `movslq (%r11,%rcx,4),%rax`
        //   `49 63 44 8b f8`              -> `movslq -0x8(%r11,%rcx,4),%rax`
        //   `65 49 63 04 8b`              -> `movslq %gs:(%r11,%rcx,4),%rax`
        // 旧实现把 `,{index},{scale}` 追加到 inner（disp/符号那一串）末尾，最后才
        // `format!("{inner}({base})")`，变址因此掉到左括号**外面**：
        //   `movsxd ,%rcx,4(%r11), %rax` / `movsxd -8,%rcx,4(%r11), %rax`
        // 长度是对的（所以 UNKNOWN 计数看不见它），但文本不是 GAS 合法操作数 ——
        // `-S` 产物回炸必挂。修前实测：全语料 626 个可编译夹具×档位里 75 行是这个
        // 形状（逐行数 `grep -cP ',%[a-z0-9]+,[0-9]+\('`，同一批行在新二进制上为 0）。
        // 另外两条不能退化的既有约定：RIP 相对的符号名（`arr(%rip)`）、段前缀贴在
        // 内存操作数上（`%gs:(%rax)`）；两者都在括号外那一截，与本次改动正交。
        let parens = base.is_some() || !index.is_empty();
        let mut head = String::from(seg);
        if let Some(s) = sym {
            head.push_str(&s);
        } else if disp != 0 || base.is_none() {
            // base 缺席时位移**就是地址本身**，0 也必须写出来：无变址就是裸绝对
            // 形式，有变址就是 `(,%index,%scale)`，两者都不能把位移默掉
            // （objdump：`mov 0x0,%rax` / `mov 0x40(,%rcx,1),%rax`）。
            head.push_str(&format!("{disp}"));
        }
        if !parens {
            return Some(head);
        }
        let mut operand = String::from("(");
        if let Some(b) = base {
            operand.push_str(&b);
        }
        if !index.is_empty() {
            operand.push_str(&format!(",{index},{scale}"));
        }
        operand.push(')');
        Some(format!("{head}{operand}"))
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
            // （此处原有两条 `0x31 / 0x85 if self.code.get(self.pos) == Some(&0xC0)` 的特例臂，
            // 已删：`peek()` 不推进位置，所以它们比的是 **opcode 自己**，0x31/0x85 与
            // 0xC0 永不相等 —— 两条永不火的结构。`xor %eax,%eax` 与 `test %eax,%eax`
            // 本来就由 `decode_rex` / `decode_legacy` 的 ALU 表给出，删它不丢形状。）
            0xB8..=0xBF => {
                let reg = b - 0xB8;
                self.take();
                // 0x66 下立即数是 imm16 且目标是 16 位寄存器（`66 B8 iw` = mov $imm,%ax）。
                // 按 imm32 读会把长度算错，后缀的两个字节被当成新指令首字节。
                if pfx.o16 {
                    let imm = self.take_i16()?;
                    Some((self.pos - start, format!("mov ${imm}, {}", REG16[reg as usize])))
                } else {
                    let imm = self.take_i32()?;
                    Some((self.pos - start, format!("mov ${imm}, {}", REG32[reg as usize])))
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
                let rex = Rex::default();
                self.decode_x87(start, &pfx, &rex, opcode)
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
                Some((self.pos - start, format!("{cc} {}", self.jump_target(off + 2, rel, end))))
            },
            0xAE => {
                let rex = self.parse_rex();
                self.decode_group_ae(start, &rex)
            },
            0xB0 | 0xB1 => {
                let rex = self.parse_rex();
                // cmpxchg r/m, reg：reg 字段是源（见 [`two_byte_reg_role`]）。
                // 0xB0 恒为 8 位形式；0xB1 宽度才随 0x66 / REX.W 走（as+objdump 反读）。
                let width = if op == 0xB0 {
                    1
                } else {
                    if rex.w { 8 } else { 4 }
                };
                let (reg_field, rm_field) = self.modrm_gp(&rex, width)?;
                let ops = Self::two_byte_att_operands(
                    &reg_field,
                    &rm_field,
                    Self::two_byte_reg_role(op)?,
                );
                Some(self.finish(start, pfx, &format!("cmpxchg {ops}")))
            },
            // cmovcc r, r/m (non-REX)：reg 字段是目的
            0x40..=0x4F => {
                let rex = self.parse_rex();
                let cc = CC_NAMES[(op - 0x40) as usize];
                let (reg_field, rm_field) = self.modrm_gp(&rex, 4)?;
                let ops = Self::two_byte_att_operands(
                    &reg_field,
                    &rm_field,
                    Self::two_byte_reg_role(op)?,
                );
                Some(self.finish(start, pfx, &format!("cmov{cc} {ops}")))
            },
            0xC1 => {
                let rex = self.parse_rex();
                // xadd r/m, reg：reg 字段是源
                let (reg_field, rm_field) =
                    self.modrm_gp(&rex, if rex.w { 8 } else { 4 })?;
                let ops = Self::two_byte_att_operands(
                    &reg_field,
                    &rm_field,
                    Self::two_byte_reg_role(op)?,
                );
                Some(self.finish(start, pfx, &format!("xadd {ops}")))
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
                // imul r, r/m：reg 字段是目的
                let (reg_field, rm_field) =
                    self.modrm_gp(&rex, if rex.w { 8 } else { 4 })?;
                let ops = Self::two_byte_att_operands(
                    &reg_field,
                    &rm_field,
                    Self::two_byte_reg_role(op)?,
                );
                Some(self.finish(start, pfx, &format!("imul {ops}")))
            },
            // BSF / BSR（`0F BC` / `0F BD`）：reg 字段是目的（扫描结果写入它），
            // dcc 在 __builtin_ctz/clz 路径上会发。宽度由 REX.W 定。
            0xBC | 0xBD => {
                let rex = self.parse_rex();
                let (reg_field, rm_field) =
                    self.modrm_gp(&rex, if rex.w { 8 } else { 4 })?;
                let ops = Self::two_byte_att_operands(
                    &reg_field,
                    &rm_field,
                    Self::two_byte_reg_role(op)?,
                );
                let mn = if op == 0xBC { "bsf" } else { "bsr" };
                Some(self.finish(start, pfx, &format!("{mn} {ops}")))
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

    /// ALU `/r` 一族（`00..3B`：八个运算 × {8 位, 操作数尺寸} × {r/m,r / r,r/m}）的
    /// 助记词、方向与宽度。返回 `(mnemonic, reg 是目的, 8 位形)`；非本族形状返回
    /// `None`（交回各自的臂，最终由兜底出口处理）。
    ///
    /// 这张表存在的理由是「同一个事实不许在四处各写一份清单」：本文件原先在 REX 与
    /// legacy 两条路径上各有四条臂，四条清单全都只列 add/or/and/sub/xor(/cmp)，于是
    /// **adc 与 sbb 两个运算同时缺席**（实测残留 `4d 11 d8` = objdump `adc %r11,%r8`、
    /// `4d 19 df` = `sbb %r11,%r15`）。兜底把长度算对，所以它只表现为「读不出来」，
    /// 永远不会有名字 —— 一张按位算的表把这八种运算一次补全。
    ///
    /// 编码规则（Intel SDM Vol.2 的 ALU 表，与本机 `as`+`objdump -d` 反读一致）：
    ///   * `op >> 3` = 运算：0 add / 1 or / 2 adc / 3 sbb / 4 and / 5 sub / 6 xor / 7 cmp；
    ///   * `op & 7` 的低两位是形状：0 `ALU r/m8,r8` / 1 `ALU r/m,r` / 2 `ALU r8,r/m8` /
    ///     3 `ALU r,r/m`；+4/+5 是 accumulator+imm 形、+6/+7 是 IN/OUT，都不带 ModRM，
    ///     所以必须排除（`0f` 的转义也正落在被排除的 +7 上，由 `decode_one` 提前拦掉）。
    fn alu_rm_r(op: u8) -> Option<(&'static str, bool, bool)> {
        if op >= 0x40 {
            return None;
        }
        let form = op & 7;
        if form > 3 {
            return None;
        }
        let mn = ["add", "or", "adc", "sbb", "and", "sub", "xor", "cmp"][(op >> 3) as usize];
        // bit1 定方向、bit0 定宽度（见上表的形状编号）。
        Some((mn, form & 2 != 0, form & 1 == 0))
    }

    /// 两字节（`0F xx`）操作数次序的唯一真源。
    ///
    /// 返回 ModRM 的 `reg` 字段在 AT&T 读数里是**源**还是**目的**。两字节指令没有
    /// 单字节那种「方向位」，次序只能按 opcode 逐族钉死 —— 靠读代码猜方向必然出错，
    /// 本轮用 `as` + `objdump -d` 产物级反读核对（探针见 §11.54）：
    ///
    /// `reg` 字段是**目的**（AT&T 打印 `rm, reg`）：
    ///   * `0F AF` imul r, r/m
    ///   * `0F 40..4F` cmovcc r, r/m
    ///   * `0F BC` bsf r, r/m、`0F BD` bsr r, r/m
    ///
    /// `reg` 字段是**源**（AT&T 打印 `reg, rm`）：
    ///   * `0F B0` cmpxchg r/m, reg、`0F B1` cmpxchg{8} r/m8, reg
    ///   * `0F C1` xadd r/m, reg
    ///
    /// 落在这两族之外的 opcode 返回 None：调用方只对自己认得的指令取方向，不靠
    /// 「默认值」蒙混 —— 一个新的两字节指令接进来，次序必须先在这张表里登记。
    fn two_byte_reg_role(op: u8) -> Option<bool> {
        match op {
            0xAF | 0x40..=0x4F | 0xBC | 0xBD => Some(true),
            0xB0 | 0xB1 | 0xC1 => Some(false),
            _ => None,
        }
    }

    /// 把 [`Self::modrm_gp`] 读到的 `(reg 字段, r/m 字段)` 按
    /// [`Self::two_byte_reg_role`] 钉死的次序排成 AT&T 操作数串 `src, dst`。
    ///
    /// `reg_is_dst` 必须直接取自 [`Self::two_byte_reg_role`]，不在调用处再猜一次
    /// 方向 —— 这正是本轮 imul/cmov（reg 是目的）与 cmpxchg/xadd（reg 是源）
    /// 一度整族印反的那一位。集中到这一个函数后，六条臂只声明 opcode 与宽度，
    /// 不再各自手写解构顺序。
    fn two_byte_att_operands(reg_field: &str, rm_field: &str, reg_is_dst: bool) -> String {
        if reg_is_dst {
            format!("{rm_field}, {reg_field}")
        } else {
            format!("{reg_field}, {rm_field}")
        }
    }

    /// `mov r/m, imm` 共用的一小段（`C6` = imm8、`C7` = imm32，REX 与 legacy 两条路径同用）。
    ///
    /// ModRM 的 /digit 为 7 时这条编码**不是 mov**：`C6 /7` 是 XABORT、`C7 /7` 是 XBEGIN
    /// （都要求带 XACQUIRE/`lock` 前缀）。本文件确实会收到 `f0 c7 f8 …` 这串字节（后端为
    /// inline asm 的 `xbegin` 就发它，见 `emit_lines.rs`），而拿它当 `mov` 打出一条看起来
    /// 合法的读数，比一条诚实的 UNKNOWN 危险得多 —— 「读回来的名字必须是意图那个」这类
    /// 矩阵判据会与错误读数对齐。名字为何不赌：本机 GAS 直接拒 `lock xabort $3` 与
    /// `lock xbegin 1f` 两种写法（"expecting lockable instruction after `lock'"），无法用
    /// `as` + `objdump -d` 反读出拼法。长度仍由兜底算完整，不会把后面的指令流切碎。
    fn decode_mov_rm_imm(
        &mut self,
        start: usize,
        pfx: &Prefixes,
        rex: &Rex,
        width: u8,
        imm8: bool,
    ) -> Option<(usize, String)> {
        if self.code.get(self.pos).map(|m| (m >> 3) & 7) == Some(7) {
            return Some(self.unknown_from(start));
        }
        let (_, mem) = self.modrm_gp(rex, width)?;
        let text = if imm8 {
            format!("mov ${}, {mem}", self.take()? as i32)
        } else {
            format!("mov ${}, {mem}", self.take_i32()?)
        };
        Some(self.finish(start, pfx, &text))
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
            0xC6 => self.decode_mov_rm_imm(start, pfx, &rex, 1, true),
            // --- XCHG / LEA ---
            0x87 => {
                let (src, dst) = self.modrm_gp(&rex, width)?;
                Some(self.finish(start, pfx, &format!("xchg {src}, {dst}")))
            },
            0x8D => {
                let (dst, src) = self.modrm_gp(&rex, width)?;
                Some(self.finish(start, pfx, &format!("lea {src}, {dst}")))
            },
            // --- ALU `/r` 全族（见 [`Self::alu_rm_r`]）---
            // AT&T 打印序恒为 `{src},{dst}`，所以由方向位决定 reg 字段落在哪一侧：
            // `ALU r/m,r` 时 reg 是源，`ALU r,r/m` 时 reg 是目的。
            op @ 0x00..=0x3B if Self::alu_rm_r(op).is_some() => {
                let (mn, reg_is_dst, is8) = Self::alu_rm_r(op)?;
                let (reg_name, rm_name) = self.modrm_gp(&rex, if is8 { 1 } else { width })?;
                let text = if reg_is_dst {
                    format!("{mn} {rm_name}, {reg_name}")
                } else {
                    format!("{mn} {reg_name}, {rm_name}")
                };
                Some(self.finish(start, pfx, &text))
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
            // --- TEST r/m, r（CMP 的 39 形已由上面的 ALU 表覆盖）---
            // 操作数序按 objdump 反读：`48 85 c8` → `test %rcx,%rax`，即 **reg 在前**。
            // 这里曾与 `0x39` 共用一条臂并把两侧打印成 `{rm},{reg}`，那是把 reg 当目的
            // 的 `ALU r,r/m` 拼法 —— 名字对、两侧互换，属于「有名字但读错」那一类。
            0x85 => {
                let (reg_name, rm_name) = self.modrm_gp(&rex, width)?;
                Some(self.finish(start, pfx, &format!("test {reg_name}, {rm_name}")))
            },
            // --- TEST 8-bit ---
            0x84 => {
                let (reg_name, rm_name) = self.modrm_gp(&rex, 1)?;
                Some(self.finish(start, pfx, &format!("test {reg_name}, {rm_name}")))
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
            0xC7 => self.decode_mov_rm_imm(start, pfx, &rex, width, false),
            // --- MOV r, imm ---
            0xB8..=0xBF => {
                // `B8+rd` 的寄存器编号在 **opcode 低 3 位**，扩展它的是 REX.B 而不是 REX.R.
                let reg = op - 0xB8;
                let r = self.reg(&rex, reg | if rex.b { 8 } else { 0 }, width);
                let imm = if rex.w {
                    let buf: [u8; 8] = self.code.get(self.pos..self.pos + 8)?.try_into().ok()?;
                    self.pos += 8;
                    i64::from_le_bytes(buf)
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
                // rm 必须传**未扩展**的原值：[`Self::decode_mem`] 内部自己做 REX.B 扩展，
                // 并按**原始** rm 判定 rm==4（SIB）/ rm==5（RIP 相对）。这里先扩展一次
                // 就让 `4`(SIB) 变成 `12`，于是既不读 SIB 也不读位移 —— 长度少算，后面
                // 整条指令流被切碎重读（实测残渣 `4d, 0xa0, 0x48, …`、`0xa0, 0x41, 0x8b,
                // …`、`0x45, 0x90`、`0xcb` 全是这一处造成的，而错位后读出来的是一些
                // **有名字**的假指令，`# UNKNOWN` 计数完全看不见它们）。
                let rm = modrm & 7;
                let dst = self
                    .reg(&rex, reg | if rex.r { 8 } else { 0 }, width)
                    .to_string();
                let src = self.decode_mem(&rex, mod_, rm, 4)?;
                Some(self.finish(start, pfx, &format!("movsxd {src}, {dst}")))
            },
            // --- IMUL r, r/m, imm32：reg 字段是目的
            0x69 => {
                let (dst, src) = self.modrm_gp(&rex, width)?;
                let imm = self.take_i32()?;
                Some(self.finish(start, pfx, &format!("imul ${imm}, {src}, {dst}")))
            },
            // --- IMUL r, r/m, imm8：reg 字段是目的
            0x6B => {
                let (dst, src) = self.modrm_gp(&rex, width)?;
                let imm = self.take_i8()? as i32;
                Some(self.finish(start, pfx, &format!("imul ${imm}, {src}, {dst}")))
            },
            // --- Shift/Rotate r/m, imm8 ---
            0xC0 | 0xC1 => {
                let sw = if op == 0xC1 { width } else { 1 };
                let modrm = self.code.get(self.pos)?;
                let mn = Self::shift_name((modrm >> 3) & 7);
                let (_, mem) = self.modrm_gp(&rex, sw)?;
                let imm = self.take()? as i32;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {mem}")))
            },
            // --- Shift/Rotate r/m, 1 ---
            0xD0 | 0xD1 => {
                let sw = if op == 0xD1 { width } else { 1 };
                let modrm = self.code.get(self.pos)?;
                let mn = Self::shift_name((modrm >> 3) & 7);
                let (_, mem) = self.modrm_gp(&rex, sw)?;
                Some(self.finish(start, pfx, &format!("{mn} $1, {mem}")))
            },
            // --- Shift/Rotate r/m, cl ---
            0xD2 | 0xD3 => {
                let sw = if op == 0xD3 { width } else { 1 };
                let modrm = self.code.get(self.pos)?;
                let mn = Self::shift_name((modrm >> 3) & 7);
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
            // /2../7 在 64 位模式下没有编码（见 [`Self::group_fe_ff_name`]）。
            0xFE => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = Self::group_fe_ff_name(0xFE, reg);
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                match mn {
                    Some(mn) => Some(self.finish(start, pfx, &format!("{mn} {mem}"))),
                    None => Some(self.finish(start, pfx, &self.unknown_consumed(start))),
                }
            },
            // --- INC/DEC/ CALL/JMP/PUSH ---
            0xFF => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mod_ = modrm >> 6;
                let mn = Self::group_fe_ff_name(0xFF, reg);
                let (_, mem) = self.modrm_gp(&rex, width)?;
                match mn {
                    None => Some(self.finish(start, pfx, &self.unknown_consumed(start))),
                    // inc/dec 的目标宽度跟着操作数走（objdump：`ff c0`→`inc %eax`），
                    // 不套 [`Self::indirect_operand`] 那条「恒 64 位 + 星号」的规则。
                    Some(mn) if !matches!(reg, 2 | 4 | 6) => {
                        Some(self.finish(start, pfx, &format!("{mn} {mem}")))
                    },
                    Some(mn) => Some(self.finish(
                        start,
                        pfx,
                        &format!(
                            "{mn} {}",
                            self.indirect_operand(&rex, pfx.o16, reg, mod_, *modrm, mem)
                        ),
                    )),
                }
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
            0xD8..=0xDF => self.decode_x87(start, &pfx, &rex, op),
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
                let (reg_field, rm_field) =
                    self.modrm_gp(rex, if rex.w { 8 } else { 4 })?;
                let ops = Self::two_byte_att_operands(
                    &reg_field,
                    &rm_field,
                    Self::two_byte_reg_role(op)?,
                );
                let mn = if op == 0xBC { "bsf" } else { "bsr" };
                Some(self.finish(start, pfx, &format!("{mn} {ops}")))
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
                // cmpxchg r/m, reg：reg 字段是源。0xB0 恒 8 位（REX 只用于扩展
                // 字节寄存器号），0xB1 才随 REX.W 定宽（as+objdump 反读）。
                let width = if op == 0xB0 {
                    1
                } else {
                    if rex.w { 8 } else { 4 }
                };
                let (reg_field, rm_field) = self.modrm_gp(rex, width)?;
                let ops = Self::two_byte_att_operands(
                    &reg_field,
                    &rm_field,
                    Self::two_byte_reg_role(op)?,
                );
                Some(self.finish(start, pfx, &format!("cmpxchg {ops}")))
            },
            0xC1 => {
                let (reg_field, rm_field) =
                    self.modrm_gp(rex, if rex.w { 8 } else { 4 })?;
                let ops = Self::two_byte_att_operands(
                    &reg_field,
                    &rm_field,
                    Self::two_byte_reg_role(op)?,
                );
                Some(self.finish(start, pfx, &format!("xadd {ops}")))
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
            // imul r, r/m (48 0F AF)：reg 字段是目的
            0xAF => {
                let (reg_field, rm_field) =
                    self.modrm_gp(rex, if rex.w { 8 } else { 4 })?;
                let ops = Self::two_byte_att_operands(
                    &reg_field,
                    &rm_field,
                    Self::two_byte_reg_role(op)?,
                );
                Some(self.finish(start, pfx, &format!("imul {ops}")))
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
            // cmovcc r, r/m (REX)：reg 字段是目的
            0x40..=0x4F => {
                let cc = CC_NAMES[(op - 0x40) as usize];
                let (reg_field, rm_field) =
                    self.modrm_gp(rex, if rex.w { 8 } else { 4 })?;
                let ops = Self::two_byte_att_operands(
                    &reg_field,
                    &rm_field,
                    Self::two_byte_reg_role(op)?,
                );
                Some(self.finish(start, pfx, &format!("cmov{cc} {ops}")))
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
            // ALU `/r` 全族（与 REX 路径共用 [`Self::alu_rm_r`] 一份事实，原因同彼：
            // 旧的四条清单里 adc/sbb 两个运算在本路径上同样整体缺席）。
            op @ 0x00..=0x3B if Self::alu_rm_r(op).is_some() => {
                let (mn, reg_is_dst, is8) = Self::alu_rm_r(op)?;
                let (reg_name, rm_name) = self.modrm_gp(&rex, if is8 { 1 } else { w })?;
                let text = if reg_is_dst {
                    format!("{mn} {rm_name}, {reg_name}")
                } else {
                    format!("{mn} {reg_name}, {rm_name}")
                };
                Some(self.finish(start, pfx, &text))
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
            0xC7 => self.decode_mov_rm_imm(start, pfx, &rex, w, false),
            // MOV r/m8, imm8
            0xC6 => self.decode_mov_rm_imm(start, pfx, &rex, 1, true),
            // LEA r32, mem
            0x8D => {
                let (dst, src) = self.modrm_gp(&rex, w)?;
                Some(self.finish(start, pfx, &format!("lea {src}, {dst}")))
            },
            // TEST r/m32, r32（CMP 的 39 形已由上面的 ALU 表覆盖）
            // 两侧序按 objdump：`85 c8` → `test %ecx,%eax`，reg 字段在前。
            0x85 => {
                let (reg_name, rm_name) = self.modrm_gp(&rex, w)?;
                Some(self.finish(start, pfx, &format!("test {reg_name}, {rm_name}")))
            },
            // TEST r/m8, r8
            0x84 => {
                let (reg_name, rm_name) = self.modrm_gp(&rex, 1)?;
                Some(self.finish(start, pfx, &format!("test {reg_name}, {rm_name}")))
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
                let mn = Self::shift_name((modrm >> 3) & 7);
                let (_, mem) = self.modrm_gp(&rex, w)?;
                let imm = self.take()? as i32;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {mem}")))
            },
            // Shift/Rotate r/m8, imm8
            0xC0 => {
                let modrm = self.code.get(self.pos)?;
                let mn = Self::shift_name((modrm >> 3) & 7);
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                let imm = self.take()? as i32;
                Some(self.finish(start, pfx, &format!("{mn} ${imm}, {mem}")))
            },
            // Shift/Rotate r/m32, 1
            0xD1 => {
                let modrm = self.code.get(self.pos)?;
                let mn = Self::shift_name((modrm >> 3) & 7);
                let (_, mem) = self.modrm_gp(&rex, w)?;
                Some(self.finish(start, pfx, &format!("{mn} $1, {mem}")))
            },
            // Shift/Rotate r/m8, 1
            0xD0 => {
                let modrm = self.code.get(self.pos)?;
                let mn = Self::shift_name((modrm >> 3) & 7);
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                Some(self.finish(start, pfx, &format!("{mn} $1, {mem}")))
            },
            // Shift/Rotate r/m32, cl
            0xD3 => {
                let modrm = self.code.get(self.pos)?;
                let mn = Self::shift_name((modrm >> 3) & 7);
                let (_, mem) = self.modrm_gp(&rex, w)?;
                Some(self.finish(start, pfx, &format!("{mn} %cl, {mem}")))
            },
            // Shift/Rotate r/m8, cl
            0xD2 => {
                let modrm = self.code.get(self.pos)?;
                let mn = Self::shift_name((modrm >> 3) & 7);
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
            // INC/DEC/CALL/JMP/PUSH r/m32（与 REX 路径同表同实现）
            0xFF => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mod_ = modrm >> 6;
                let mn = Self::group_fe_ff_name(0xFF, reg);
                let (_, mem) = self.modrm_gp(&rex, w)?;
                match mn {
                    None => Some(self.finish(start, pfx, &self.unknown_consumed(start))),
                    Some(mn) if !matches!(reg, 2 | 4 | 6) => {
                        Some(self.finish(start, pfx, &format!("{mn} {mem}")))
                    },
                    Some(mn) => Some(self.finish(
                        start,
                        pfx,
                        &format!(
                            "{mn} {}",
                            self.indirect_operand(&rex, pfx.o16, reg, mod_, *modrm, mem)
                        ),
                    )),
                }
            },
            // INC/DEC r/m8
            0xFE => {
                let modrm = self.code.get(self.pos)?;
                let reg = (modrm >> 3) & 7;
                let mn = Self::group_fe_ff_name(0xFE, reg);
                let (_, mem) = self.modrm_gp(&rex, 1)?;
                match mn {
                    Some(mn) => Some(self.finish(start, pfx, &format!("{mn} {mem}"))),
                    None => Some(self.finish(start, pfx, &self.unknown_consumed(start))),
                }
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
            // IMUL r, r/m, imm32：reg 字段是目的
            0x69 => {
                let (dst, src) = self.modrm_gp(&rex, w)?;
                let imm = self.take_i32()?;
                Some(self.finish(start, pfx, &format!("imul ${imm}, {src}, {dst}")))
            },
            // IMUL r, r/m, imm8：reg 字段是目的
            0x6B => {
                let (dst, src) = self.modrm_gp(&rex, w)?;
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

    /// 解码 x87 FPU 指令 (0xD8-0xDF)。
    ///
    /// 两张助记符表（内存形看 `(opcode, reg)`、寄存器形看 `(opcode, reg, rm)`）都是
    /// `as` 组 `.byte` + `objdump -d` 逐字节钉下来的原文，不是凭 ISA 手册的记忆写的。
    /// 为什么要钉全而不是「先写认识的几条」：旧实现只认十几条，其余一律打成 `f???` /
    /// `fi???`，而 mod=3 的分支还会 fallthrough 到「内存操作数」那条路上去，把
    /// `%st(rm)` 打成 `%rdx`。于是实测语料里 **1013 条读数**（18 个夹具×档位，全部是
    /// long double / fenv 一族）长成这样：
    ///
    /// ```text
    /// f??? 0(%rip)        f??? (%rsp)         f??? -120(%r13)
    /// f??? %rdx           f??? %rax
    /// ```
    ///
    /// 它们**有名字**（虽然是个假名字），所以 `# UNKNOWN` 那条计数一条都不报；同时
    /// `as` 直接拒绝这种 token（实测：`错误： invalid character '?' in mnemonic`），
    /// 所以「-S 文本可被 GAS 回炸」也一并漏过。两条门都被同一个占位符绕过，这就是
    /// 本函数改成「认得就叫出 objdump 的真名，不认得就报 UNKNOWN」的原因 —— 占位符
    /// 不是兜底出口，是第二个兜底出口。
    ///
    /// objdump（binutils 2.42）真值样例（逐条实测，不是记忆）：
    /// ```text
    /// db 05 00 00 00 00 -> fildl    0x0(%rip)      （旧读数：f??? 0(%rip)）
    /// db 3c 24          -> fstpt    (%rsp)         （旧读数：f??? (%rsp)）
    /// db e2             -> fnclex                    （旧读数：f??? %rdx）
    /// d9 e0             -> fchs                      （旧臂只认这一条 mod=3）
    /// d8 05 22 00 00 00 -> fadds   0x22(%rip)
    /// dd 08             -> fisttpll (%rax)          （不是 fldl：fldl 是 DD/0）
    /// de c1             -> faddp    %st,%st(1)
    /// d9 08             -> (bad)    (%rax)          ⇒ 本解码器报 UNKNOWN，不猜名字
    /// ```
    fn decode_x87(
        &mut self,
        start: usize,
        pfx: &Prefixes,
        rex: &Rex,
        opcode: u8,
    ) -> Option<(usize, String)> {
        let modrm = self.take()?;
        let mod_ = modrm >> 6;
        let reg = (modrm >> 3) & 7;
        let rm = modrm & 7;
        if mod_ == 3 {
            // 寄存器形：操作数只可能来自 `%st(rm)`，既没有 SIB 也没有位移。旧实现
            // 认不出时**不返回**，继续往下走内存路径，把 `%st(2)` 念成 `%rdx`。
            return match Self::x87_reg_text(opcode, reg, rm) {
                Some(text) => Some(self.finish(start, pfx, &text)),
                // objdump 自己也只能读 `(bad)`（或 `fneni(8087 only)` 这种带注记、
                // GAS 不接受的名字）：编码未定义，不许给它编一个看着合法的名字。
                None => Some(self.unknown_from(start)),
            };
        }
        // 内存形交给 decode_mem：括号怎么拼（base/index/scale、RIP 符号名、段前缀）
        // 全仓库只有那一份事实。旧的 `decode_x87_mem` 是自己另写的一份，丢 SIB 变址、
        // 把 REX 硬编成 `%r13`/`%rbp`、也没有 data 符号名 —— 长度对但文本不是 GAS
        // 合法操作数（`%gs:16(%r13)` 那类还会把段前缀整个吞掉）。
        let Some(mn) = Self::x87_mem_mnemonic(opcode, reg) else {
            // 内存形的未定义格（`D9/1`、`DB/4`、`DB/6`、`DD/5`，objdump 读 `(bad)`）同样
            // 报 UNKNOWN，不拿占位符充当名字。
            return Some(self.unknown_from(start));
        };
        let mem = self.decode_mem(rex, mod_, rm, 8)?;
        Some(self.finish(start, pfx, &format!("{mn} {mem}")))
    }

    /// x87 内存形助记符。`None` = objdump 读作 `(bad)`（该组合未定义）。
    ///
    /// 真值由 `.byte D8..DF, <reg><<3>`（mod=0/rm=0 ⇒ `(%rax)`）64 条一次性钉出，
    /// 原文照抄：未定义的四格是 `D9/1`、`DB/4`、`DB/6`、`DD/5`。
    /// 两条与直觉相反的拼法也照 objdump 写，不做「顺手规范化」：
    /// `DA/0` 是 `fiaddl`（int32）而 `DE/0` 是 `fiadds`（int16），`DB/1` 是 `fisttpl`。
    fn x87_mem_mnemonic(opcode: u8, reg: u8) -> Option<&'static str> {
        Some(match (opcode, reg) {
            (0xD8, 0) => "fadds",
            (0xD8, 1) => "fmuls",
            (0xD8, 2) => "fcoms",
            (0xD8, 3) => "fcomps",
            (0xD8, 4) => "fsubs",
            (0xD8, 5) => "fsubrs",
            (0xD8, 6) => "fdivs",
            (0xD8, 7) => "fdivrs",
            (0xD9, 0) => "flds",
            (0xD9, 2) => "fsts",
            (0xD9, 3) => "fstps",
            (0xD9, 4) => "fldenv",
            (0xD9, 5) => "fldcw",
            (0xD9, 6) => "fnstenv",
            (0xD9, 7) => "fnstcw",
            (0xDA, 0) => "fiaddl",
            (0xDA, 1) => "fimull",
            (0xDA, 2) => "ficoml",
            (0xDA, 3) => "ficompl",
            (0xDA, 4) => "fisubl",
            (0xDA, 5) => "fisubrl",
            (0xDA, 6) => "fidivl",
            (0xDA, 7) => "fidivrl",
            (0xDB, 0) => "fildl",
            (0xDB, 1) => "fisttpl",
            (0xDB, 2) => "fistl",
            (0xDB, 3) => "fistpl",
            (0xDB, 5) => "fldt",
            (0xDB, 7) => "fstpt",
            (0xDC, 0) => "faddl",
            (0xDC, 1) => "fmull",
            (0xDC, 2) => "fcoml",
            (0xDC, 3) => "fcompl",
            (0xDC, 4) => "fsubl",
            (0xDC, 5) => "fsubrl",
            (0xDC, 6) => "fdivl",
            (0xDC, 7) => "fdivrl",
            (0xDD, 0) => "fldl",
            (0xDD, 1) => "fisttpll",
            (0xDD, 2) => "fstl",
            (0xDD, 3) => "fstpl",
            (0xDD, 4) => "frstor",
            (0xDD, 6) => "fnsave",
            (0xDD, 7) => "fnstsw",
            (0xDE, 0) => "fiadds",
            (0xDE, 1) => "fimuls",
            (0xDE, 2) => "ficoms",
            (0xDE, 3) => "ficomps",
            (0xDE, 4) => "fisubs",
            (0xDE, 5) => "fisubrs",
            (0xDE, 6) => "fidivs",
            (0xDE, 7) => "fidivrs",
            (0xDF, 0) => "filds",
            (0xDF, 1) => "fisttps",
            (0xDF, 2) => "fists",
            (0xDF, 3) => "fistps",
            (0xDF, 4) => "fbld",
            (0xDF, 5) => "fildll",
            (0xDF, 6) => "fbstp",
            (0xDF, 7) => "fistpll",
            _ => return None,
        })
    }

    /// x87 寄存器形（mod=3）读数。`None` = objdump 读 `(bad)`（该编码未定义）。
    ///
    /// 这张表不是按 ISA 手册抄的，是把 `D8..DF × mod=3` 的**全部 512 个组合**交给
    /// `as` + `objdump -d` 跑一遍再逐格对齐的结果（原始读数见 `docs/wp_disasm.md`，
    /// 常驻的对账判据是 `decodes_all_x87_space_against_objdump_sweep`）。
    /// 两处与手册记忆不同的地方，都以 objdump 为准：
    ///   * `DD E0` 是 `fucom %st(0)`（`ffree` 在 `DD C0`，即 DD/0）；
    ///   * `DF E0` 是 `fnstsw %ax`，只有 rm=0 有定义（rm=1..7 是 `(bad)`）。
    /// `DB/4` 一族 objdump 打成 `fneni(8087 only)` / `fnsetpm(287 only)`：GAS 解析不了
    /// 那个括号注记尾巴，但认得裸助记词（实测 `fneni` 组装回 `db e0`、`frstpm` 组装回
    /// `db e5`），所以这里打印裸名 —— 既保住读数，也保住 `-S` 文本可被 GAS 回炸。
    ///
    /// 操作数形状只有四种，全部照 objdump 的 `%st(N)` 写法（逗号后的空格是本文件的
    /// 打印惯例，与 objdump 的紧凑写法不同；**两侧顺序**与 objdump 一致）：
    /// ```text
    /// d8 01  -> fadd %st(1), %st        dc 01  -> fadd %st, %st(1)
    /// d8 21  -> fcom %st(1)             de 01  -> faddp %st, %st(1)
    /// d9 c1  -> fld %st(1)              da 01  -> fcmovb %st(1), %st
    /// db e2  -> fnclex                  d9 e0  -> fchs
    /// ```
    fn x87_reg_text(opcode: u8, reg: u8, rm: u8) -> Option<String> {
        let st = format!("%st({rm})");
        Some(match (opcode, reg) {
            (0xD8, r @ (0 | 1 | 4 | 5 | 6 | 7)) => {
                format!("f{} {st}, %st", Self::x87_arith_name(r)?)
            },
            (0xD8, 2) => format!("fcom {st}"),
            (0xD8, 3) => format!("fcomp {st}"),
            (0xD9, 0) => format!("fld {st}"),
            (0xD9, 1) => format!("fxch {st}"),
            (0xD9, 2) if rm == 0 => "fnop".to_string(),
            (0xD9, 4) => match rm {
                0 => "fchs",
                1 => "fabs",
                4 => "ftst",
                5 => "fxam",
                _ => return None,
            }
            .to_string(),
            (0xD9, 5) => match rm {
                0 => "fld1",
                1 => "fldl2t",
                2 => "fldl2e",
                3 => "fldpi",
                4 => "fldlg2",
                5 => "fldln2",
                6 => "fldz",
                _ => return None,
            }
            .to_string(),
            (0xD9, 6) => match rm {
                0 => "f2xm1",
                1 => "fyl2x",
                2 => "fptan",
                3 => "fpatan",
                4 => "fxtract",
                5 => "fprem1",
                6 => "fdecstp",
                _ => "fincstp",
            }
            .to_string(),
            (0xD9, 7) => match rm {
                0 => "fprem",
                1 => "fyl2xp1",
                2 => "fsqrt",
                3 => "fsincos",
                4 => "frndint",
                5 => "fscale",
                6 => "fsin",
                _ => "fcos",
            }
            .to_string(),
            (0xDA, r @ 0..=3) => {
                format!("{} {st}, %st", Self::x87_cmov_name(false, r)?)
            },
            (0xDA, 5) if rm == 1 => "fucompp".to_string(),
            (0xDB, r @ 0..=3) => {
                format!("{} {st}, %st", Self::x87_cmov_name(true, r)?)
            },
            (0xDB, 4) => {
                // rm=0/1/4/5 是 objdump 带 `(8087 only)`/`(287 only)` 注记的四条；
                // rm=2/3（fnclex/fninit）是 80287 之后对前两格的改名，两套名字都留。
                match rm {
                    0 => "fneni",
                    1 => "fndisi",
                    2 => "fnclex",
                    3 => "fninit",
                    4 => "fnsetpm",
                    5 => "frstpm",
                    _ => return None,
                }
                .to_string()
            },
            (0xDB, 5) => format!("fucomi {st}, %st"),
            (0xDB, 6) => format!("fcomi {st}, %st"),
            (0xDC, r @ (0 | 1 | 4 | 5 | 6 | 7)) => {
                format!("f{} %st, {st}", Self::x87_arith_name(r)?)
            },
            (0xDD, 0) => format!("ffree {st}"),
            (0xDD, 2) => format!("fst {st}"),
            (0xDD, 3) => format!("fstp {st}"),
            (0xDD, 4) => format!("fucom {st}"),
            (0xDD, 5) => format!("fucomp {st}"),
            (0xDE, r @ (0 | 1 | 4 | 5 | 6 | 7)) => {
                format!("f{}p %st, {st}", Self::x87_arith_name(r)?)
            },
            (0xDE, 3) if rm == 1 => "fcompp".to_string(),
            (0xDF, 0) => format!("ffreep {st}"),
            (0xDF, 4) if rm == 0 => "fnstsw %ax".to_string(),
            (0xDF, 5) => format!("fucomip {st}, %st"),
            (0xDF, 6) => format!("fcomip {st}, %st"),
            _ => return None,
        })
    }

    /// D8 / DC / DE 三族共用的六个运算词根（objdump 的名字都是 `f` + 词根，DE 再加 `p`；
    /// `reg` 的 2/3 是 fcom/fcomp，不属于这一族，
    /// 而且只在 D8 上有定义 —— 所以这里当未定义处理）。
    fn x87_arith_name(reg: u8) -> Option<&'static str> {
        Some(match reg {
            0 => "add",
            1 => "mul",
            4 => "sub",
            5 => "subr",
            6 => "div",
            7 => "divr",
            _ => return None,
        })
    }

    /// `fcmov*` 一族：DA 是「条件成立则移动」，DB 是反条件（objdump 的拼法带 `n`）。
    fn x87_cmov_name(inverted: bool, reg: u8) -> Option<&'static str> {
        Some(match (inverted, reg) {
            (false, 0) => "fcmovb",
            (false, 1) => "fcmove",
            (false, 2) => "fcmovbe",
            (false, 3) => "fcmovu",
            (true, 0) => "fcmovnb",
            (true, 1) => "fcmovne",
            (true, 2) => "fcmovnbe",
            (true, 3) => "fcmovnu",
            _ => return None,
        })
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

/// P9 方案 C: 解码**单条**指令, 返回 (字节长度, GAS 文本).
///
/// 与 `disassemble_range` 的差别:
/// - 不打印 labels / directives (那些由 v2 渲染器按 pc 自行查表);
/// - 不追加 `\t` 前缀与 `\n` 后缀 (v2 渲染器自己加);
/// - 不处理 `.byte` 兜底 (发射点发出的指令必须是合法 x86_64).
///
/// 用途: `emit_insn` 发射后立即调用, 把这条指令的 GAS 文本缓存到
/// `Codegen::insn_render_cache`, 供 `build_asm_output_v2` 直接遍历输出.
/// 这是 v2 渲染器在生产路径上调用 disasm 解码表的**唯一**入口.
///
/// `calls` / `data` / `labels` 与 `disassemble_range` 同语义: 供跳转目标
/// 与 RIP 相对符号名解析. 这些表在发射期不完整 (PendingJump 还没回填),
/// 所以 v2 渲染器必须在 `resolve()` 之后再跑一次渲染, 或者直接读发射点
/// 显式给出的 `gas_text` (优先通道).
pub fn disassemble_one_at(
    code: &[u8],
    pos: usize,
    base_offset: u64,
    calls: &HashMap<u64, String>,
    data: &HashMap<u64, String>,
    labels: &BTreeMap<u64, Vec<&str>>,
) -> Option<(usize, String)> {
    if pos >= code.len() {
        return None;
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
    dec.decode_one()
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

/// 读取 ModRM 并跟随其 SIB / disp8 / disp32 / RIP-relative disp32, 返回整条指令的
/// 长度偏移 (从 pos 开始, ModRM 自身 +1). decode_group_ae 认不得时用它把游标推到
/// 指令末尾再报 UNKNOWN; 与 Dec::unknown_len 同源但只处理最常见的一条路径.
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

