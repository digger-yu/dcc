// src/codegen/shared/asm_source.rs - 内联汇编源行的单一真源
//
// 第一性原理: 一条已经过约束解析/操作数替换的 asm 文本行, 只有四种归宿:
//   1. 是 dcc 能编码的指令            -> 直接进 .text 字节
//   2. 是 dcc 编不出来的指令          -> 必须原样透传给 GAS (TU 级降级判据)
//   3. 是 assembler directive (.globl 等) -> 不是指令, 不产字节
//   4. 是标签定义/引用                -> 由后端的 label 机制处理, 不产指令字节
// 目标: 把「词法切分 -> 分类 -> 编码 -> 透传判据」收敛到本模块一处实现, 5 个后端
//   只消费 AsmSnippet 结果, 不再各自实现, 从根源消除 -c 与 -S 两条路径的行为分叉。
//
// 本模块同时消灭两个历史缺陷:
//   * 词法 (注释符) 曾被硬编码为 x86 的 `#`, 而 aarch64/ARM 的 `#` 是**立即数前缀**
//     (`add x0, x0, #1`), 会被静默截断成 `add x0, x0,` -> 操作数丢失。注释符按 ISA 表化。
//   * 编码失败曾被表达成 `Err(Vec<String>)`, 丢失原文/大小写/行号, 且整批字节一起作废;
//     现在逐行给出 AsmLineOutcome, 透传行结构化上报, 供 TU 级降级与精确诊断共用一份数据。

use crate::cli::Arch;

/// 指令集标识: 显式枚举, 取代 `arch: &str` —— 字符串拼写错误会让 `_ => None` 分支
/// 把整个 TU 的所有指令误判成"不支持", 且没有任何编译期提示。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsmIsa {
    X86_64,
    AArch64,
    RiscV64,
    RiscV32,
    Arm,
    Mips32,
    Mips64,
}

impl AsmIsa {
    /// 由目标架构构造。`is_64bit` 仅用于 MIPS (Mips/Mips64 在 Arch 上已分开,
    /// 但 mini_asm 的编码表按 bool 参数化, 此处收敛为一个真源)。
    pub fn from_arch(arch: Arch, is_64bit: bool) -> Option<Self> {
        Some(match arch {
            Arch::X86_64 => AsmIsa::X86_64,
            Arch::AArch64 => AsmIsa::AArch64,
            Arch::RiscV64 => AsmIsa::RiscV64,
            Arch::RiscV32 => AsmIsa::RiscV32,
            Arch::Arm => AsmIsa::Arm,
            Arch::Mips if is_64bit => AsmIsa::Mips64,
            Arch::Mips => AsmIsa::Mips32,
            Arch::Mips64 => AsmIsa::Mips64,
        })
    }

    /// 该 ISA 的内联汇编编码表由共享 mini_asm 提供; x86_64 有自研完整汇编器,
    /// 其编码经 `analyze_with` 的注入点完成。
    pub fn uses_mini_asm(self) -> bool {
        self != AsmIsa::X86_64
    }

    /// 交给 mini_asm `encode_*` 的架构键 (保持既有编码表签名不变)。
    fn mini_asm_key(self) -> Option<&'static str> {
        match self {
            AsmIsa::AArch64 => Some("aarch64"),
            AsmIsa::RiscV64 | AsmIsa::RiscV32 => Some("riscv64"),
            AsmIsa::Arm => Some("arm"),
            AsmIsa::Mips32 | AsmIsa::Mips64 => Some("mips"),
            AsmIsa::X86_64 => None,
        }
    }

    /// GAS 行注释符。注意 ARM 还有 `;` 作为语句分隔符, 但 `;` 在 ARM GAS 中亦作
    /// 行内注释起始, 这里一并按行尾截断。
    pub fn line_comment_tokens(self) -> &'static [&'static str] {
        match self {
            // x86 AT&T/GAS: `#`; RISC-V、MIPS: `#`
            AsmIsa::X86_64
            | AsmIsa::RiscV64
            | AsmIsa::RiscV32
            | AsmIsa::Mips32
            | AsmIsa::Mips64 => &["#"],
            // AArch64 GAS: `//` (而 `#` 是立即数前缀, 绝不能截断)
            AsmIsa::AArch64 => &["//"],
            // ARM GAS: `@` (同样 `#` 是立即数前缀)
            AsmIsa::Arm => &["@", ";"],
        }
    }

    /// 立即数前缀 (透传文本必须原样保留, 不参与截断)。
    pub fn immediate_prefixes(self) -> &'static [&'static str] {
        match self {
            AsmIsa::AArch64 | AsmIsa::Arm => &["#", "$"],
            AsmIsa::Mips32 | AsmIsa::Mips64 => &["0x"],
            _ => &[],
        }
    }

    /// 降级粒度 (P7 基线 6 的 ISA 属性):
    ///   * 定长 ISA (aarch64/A32 arm/mips/x86_64) 逐语句分流,
    ///     已知行仍直发字节、未知行单独透传;
    ///   * 变长 ISA (riscv64 含 16/32 位压缩指令) 必须整 TU 分流,
    ///     否则两条路径在同一函数内的 PC 偏移对不齐。
    /// x86_64 虽变长但其 `-S` 走自研完整汇编器、不降级, 返回 WholeTu
    /// 只是把"整 TU 渲染"这一事实显式化, 不影响直发路径。
    pub fn degrade_granularity(self) -> DegradeGranularity {
        match self {
            AsmIsa::AArch64 | AsmIsa::Arm | AsmIsa::Mips32 | AsmIsa::Mips64 => {
                DegradeGranularity::PerStatement
            }
            AsmIsa::RiscV64 | AsmIsa::RiscV32 | AsmIsa::X86_64 => DegradeGranularity::WholeTu,
        }
    }
}

/// 降级分流粒度 (定长 vs 变长 ISA 的结构性差异)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DegradeGranularity {
    /// 已知行直发字节, 未知行单独透传 (定长 ISA)。
    PerStatement,
    /// 整 TU 改走 GAS 文本 (变长 ISA 或 x86_64 自研汇编器)。
    WholeTu,
}

/// 一行 asm 源文本的词法分类。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AsmLineKind {
    /// 空白或纯注释
    Empty,
    /// 标签定义 (`foo:`), 保留符号名
    LabelDef { name: String },
    /// assembler directive (以 `.` 起始)
    Directive { text: String },
    /// 指令
    Instruction {
        mnemonic: String,
        operands: Vec<String>,
    },
}

/// 一条源行的原文与其分类结果。`text` 保留 strip 注释后的原文
/// (不 lowercase、不重排空白), 用于透传给 GAS 与精确诊断。
#[derive(Clone, Debug)]
pub struct AsmSourceLine {
    /// 在该 asm 语句内的行序 (0 起), 用于把诊断锚回 asm 模板的哪一行
    pub ordinal: usize,
    pub text: String,
    pub kind: AsmLineKind,
}

/// 单行不可编码的原因。它决定 TU 级降级的措辞与是否可降级。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsmPassthroughReason {
    /// 该 ISA 的指令表里没有这个助记符
    UnknownMnemonic,
    /// 助记符认识, 但操作数形态超出表的能力
    UnsupportedOperandForm,
    /// 该 ISA 不由共享表编码 (x86_64 自研汇编器), 由后端自己判定后上报
    BackendReported,
}

impl AsmPassthroughReason {
    pub fn as_str(self) -> &'static str {
        match self {
            AsmPassthroughReason::UnknownMnemonic => "mnemonic not in dcc's instruction table",
            AsmPassthroughReason::UnsupportedOperandForm => {
                "operand form not supported by dcc's instruction table"
            },
            AsmPassthroughReason::BackendReported => "not encodable by dcc",
        }
    }
}

/// 一行的处理结果。
#[derive(Clone, Debug)]
pub enum AsmLineOutcome {
    /// 已编码为机器码
    Encoded { bytes: Vec<u8> },
    /// 不是指令 (directive/label/空), 不产指令字节, 但文本必须出现在透传输出里
    NonCode,
    /// dcc 编不出来, 必须交给 GAS
    Passthrough { reason: AsmPassthroughReason },
}

/// 一行透传记录的完整信息: 后端把它挂到 CommonImage 上, driver / 诊断 / 文本
/// 渲染三方共用同一份数据, 不再靠扫 diagnostic 字符串判断。
#[derive(Clone, Debug)]
pub struct AsmPassthroughLine {
    pub isa: AsmIsa,
    /// asm 语句内的行序
    pub ordinal: usize,
    /// 源行原文 (已去注释)
    pub text: String,
    pub reason: AsmPassthroughReason,
}

/// 一条 asm 语句 (可含多行) 的完整分析结果。
#[derive(Clone, Debug)]
pub struct AsmSnippet {
    pub isa: AsmIsa,
    pub lines: Vec<AsmSourceLine>,
    pub outcomes: Vec<AsmLineOutcome>,
    /// 所有可编码行按原顺序拼出的机器码
    pub bytes: Vec<u8>,
    /// 需要 GAS 接手的所有行 (为空 == 整个 TU 可直发)
    pub passthrough: Vec<AsmPassthroughLine>,
}

impl AsmSnippet {
    /// TU 级降级的唯一判据。
    pub fn needs_gas_text(&self) -> bool {
        !self.passthrough.is_empty()
    }

    /// 是否所有行都不是指令 (例如只写了 `.globl foo`)。
    pub fn all_non_code(&self) -> bool {
        self.outcomes
            .iter()
            .all(|o| matches!(o, AsmLineOutcome::NonCode))
    }

    /// 把透传行渲染成用户可见的清单 (诊断 hint 与 driver 报告共用)。
    pub fn passthrough_listing(&self) -> String {
        let mut s = String::new();
        for p in &self.passthrough {
            s.push_str(&format!("\n- {}: {}", p.text, p.reason.as_str()));
        }
        s
    }
}

/// ISA 无关的词法切分: 按 ISA 注释符去注释, 切出非空行, 保留原文。
pub fn split_source_lines(isa: AsmIsa, text: &str) -> Vec<(usize, String)> {
    let toks = isa.line_comment_tokens();
    let mut out = Vec::new();
    for (i, raw) in text.split('\n').enumerate() {
        let mut line = raw;
        for t in toks {
            if let Some(pos) = line.find(t) {
                line = &line[..pos];
            }
        }
        // `\t` 续行与首尾空白归一, 但行内空白原样保留 (透传要求逐字)
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        out.push((i, trimmed));
    }
    out
}

/// 把一行分类成 Empty / Directive / LabelDef / Instruction。
///
/// 判定顺序与旧 `mini_asm::parse_instruction` 一致 (directive 优先于标签),
/// 否则 `".asciz \"a:b\""` 这类含冒号的 directive 会被误判成标签。
pub fn classify_line(text: &str) -> AsmLineKind {
    if text.is_empty() {
        return AsmLineKind::Empty;
    }
    if text.starts_with('.') {
        return AsmLineKind::Directive {
            text: text.to_string(),
        };
    }
    if let Some(colon) = text.find(':') {
        let head = text[..colon].trim();
        // 标签头必须是一个不含空白、不是指令的 token。asm goto 解析后的
        // `%0:` / `1:` / `$L:` 都走这一支 —— 它们是**标签定义**, 不是指令。
        let is_label_head =
            !head.is_empty() && !head.chars().any(|c| c.is_whitespace()) && !head.starts_with('#');
        if is_label_head {
            let rest = text[colon + 1..].trim();
            if rest.is_empty() {
                return AsmLineKind::LabelDef {
                    name: head.to_string(),
                };
            }
            // label + 同一行的指令: 编码对象是指令部分, label 由后端单独记账
            return classify_line(rest);
        }
    }
    let mut it = text.split_whitespace();
    let mnemonic = match it.next() {
        Some(m) => m,
        None => return AsmLineKind::Empty,
    };
    // 操作数按逗号切分; 逗号内的空格归一, 但每个操作数字符串内部保持原样
    let operands_str = text[mnemonic.len()..].trim().to_string();
    let operands: Vec<String> = if operands_str.is_empty() {
        Vec::new()
    } else {
        operands_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };
    AsmLineKind::Instruction {
        mnemonic: mnemonic.to_lowercase(),
        operands,
    }
}

/// 分析一条 asm 语句, 编码走目标后端的默认编码表 (mini_asm)。
pub fn analyze(isa: AsmIsa, resolved: &str) -> AsmSnippet {
    analyze_with(isa, resolved, &mut |mnem, ops| default_encode(isa, mnem, ops))
}

/// 分析一条 asm 语句, 编码逻辑由后端注入 —— x86_64 用它接入自研完整汇编器,
/// 从而让"哪些行需要透传给 GAS"的判据在所有后端只有一份实现。
pub fn analyze_with<F>(isa: AsmIsa, resolved: &str, encode: &mut F) -> AsmSnippet
where
    F: FnMut(&str, &[String]) -> Option<Vec<u8>>,
{
    let mut snippet = AsmSnippet {
        isa,
        lines: Vec::new(),
        outcomes: Vec::new(),
        bytes: Vec::new(),
        passthrough: Vec::new(),
    };
    for (src_line, text) in split_source_lines(isa, resolved) {
        let kind = classify_line(&text);
        let ordinal = snippet.lines.len();
        let outcome = match &kind {
            AsmLineKind::Empty | AsmLineKind::LabelDef { .. } | AsmLineKind::Directive { .. } => {
                AsmLineOutcome::NonCode
            },
            AsmLineKind::Instruction { mnemonic, operands } => match encode(mnemonic, operands) {
                Some(bytes) => AsmLineOutcome::Encoded { bytes },
                None => {
                    let reason = if isa.uses_mini_asm() && !mnemonic_in_table(isa, mnemonic) {
                        AsmPassthroughReason::UnknownMnemonic
                    } else {
                        AsmPassthroughReason::UnsupportedOperandForm
                    };
                    snippet.passthrough.push(AsmPassthroughLine {
                        isa,
                        ordinal,
                        text: text.clone(),
                        reason,
                    });
                    AsmLineOutcome::Passthrough { reason }
                },
            },
        };
        if let AsmLineOutcome::Encoded { bytes } = &outcome {
            snippet.bytes.extend_from_slice(bytes);
        }
        snippet.lines.push(AsmSourceLine {
            ordinal: src_line,
            text,
            kind,
        });
        snippet.outcomes.push(outcome);
    }
    snippet
}

/// 后端在自身编码路径里判定"这一行编不出来"时, 用同一份记录类型上报,
/// 使降级判据保持单一真源 (x86_64 的 `_ =>` 兜底分支走这里)。
pub fn record_backend_passthrough(isa: AsmIsa, text: &str, ordinal: usize) -> AsmPassthroughLine {
    AsmPassthroughLine {
        isa,
        ordinal,
        text: text.to_string(),
        reason: AsmPassthroughReason::BackendReported,
    }
}

/// 默认编码表入口: 共享 mini_asm。x86_64 无表可查, 返回 None 由注入点负责。
pub fn default_encode(isa: AsmIsa, mnemonic: &str, operands: &[String]) -> Option<Vec<u8>> {
    match isa.mini_asm_key()? {
        "aarch64" => crate::codegen::shared::mini_asm::encode_aarch64(mnemonic, operands),
        "riscv64" => crate::codegen::shared::mini_asm::encode_riscv64(mnemonic, operands),
        "arm" => crate::codegen::shared::mini_asm::encode_arm(mnemonic, operands),
        "mips" => {
            crate::codegen::shared::mini_asm::encode_mips(mnemonic, operands, isa == AsmIsa::Mips64)
        },
        _ => None,
    }
}

/// 助记符是否在编码表里 (用于把"不认识助记符"与"操作数形态不支持"区分开)。
/// 表本身是 match 分支, 这里给出与之等价的名单, 由
/// `mnemonic_table_matches_mini_asm_dispatch` 测试钉住不与编码表漂移。
fn mnemonic_in_table(isa: AsmIsa, mnemonic: &str) -> bool {
    known_mnemonics(isa).iter().any(|m| *m == mnemonic)
}

/// 各 ISA 已支持助记符名单 (单一真源, 供诊断与能力矩阵使用)。
pub fn known_mnemonics(isa: AsmIsa) -> &'static [&'static str] {
    match isa {
        // `msr`/`mrs` 曾以静默 NOP 冒充支持, 已移除 (见 known_mnemonics 一致性测试)。
        // `ld`/`st` 曾被错列为已支持, 但表里没有这两条臂 (grep 命中的是 dmb/dsb
        // 的 domain 选项值 0xd/0xe), 已移除, 见 behavior 护栏 test。
        AsmIsa::AArch64 => &[
            "add", "blr", "br", "clrex", "dbg", "dmb", "dsb", "fadd", "fmul", "fsub", "isb",
            "ld1", "ld1w", "mov", "movz", "mul", "nop", "ptrue", "ret", "sev", "sevl", "st1",
            "st1w", "sub", "svc", "wfe", "wfi", "whilelt", "yield",
        ],
        AsmIsa::RiscV64 | AsmIsa::RiscV32 => &[
            "add",
            "addi",
            "ebreak",
            "ecall",
            "fence",
            "fence.i",
            "nop",
            "pause",
            "ret",
            "sfence.vma",
            "sub",
            "wfi",
        ],
        AsmIsa::Arm => &[
            "add", "bx", "clrex", "dmb", "dsb", "isb", "mov", "nop", "sev", "sevl", "sub", "svc",
            "swi", "wfe", "wfi", "yield",
        ],
        // `cache` 曾以静默 NOP 冒充支持, 已移除。
        AsmIsa::Mips32 | AsmIsa::Mips64 => &[
            "add", "addu", "break", "daddu", "jalr", "jr", "move", "nop", "ssnop", "sub", "subu",
            "sync", "syscall",
        ],
        // x86_64 由自研汇编器负责, 名单不在此表
        AsmIsa::X86_64 => &[],
    }
}

/// `codegen::emit()` 的结果。旧返型只有 `Vec<u8>`, 无法告知 driver
/// “我发的到底是 ELF 还是 GAS 文本”, 导致文本被写进 `.o`、权限位算错。
/// 现在这个信息在类型上不可省略。
#[derive(Debug)]
pub enum Emitted {
    /// 目标文件字节 (ELF ET_REL / ET_EXEC / COFF)
    Object(Vec<u8>),
    /// GAS 汇编文本 (TU 级降级的产物)
    Text(String),
}

impl Emitted {
    pub fn as_bytes(&self) -> Vec<u8> {
        match self {
            Emitted::Object(b) => b.clone(),
            Emitted::Text(s) => s.as_bytes().to_vec(),
        }
    }
    pub fn is_text(&self) -> bool {
        matches!(self, Emitted::Text(_))
    }
}

/// 带源位置的透传记录: `CommonImage` 承载的形态。后端只负责记录事实,
/// “报错还是降级”由 `codegen::emit()` 统一决定 —— 降级策略因此只有一份实现。
#[derive(Clone, Debug)]
pub struct AsmPassthroughRecord {
    /// 包名的源文件 (诊断定位)
    pub file: String,
    pub line: u32,
    pub isa: AsmIsa,
    /// 在该 asm 语句内的行序
    pub ordinal: usize,
    pub text: String,
    pub reason: AsmPassthroughReason,
}

impl AsmSnippet {
    /// 把本次分析产生的透传行以带源位置的形式交给后端记账。
    pub fn collect_records(&self, file: &str, line: u32) -> Vec<AsmPassthroughRecord> {
        self.passthrough
            .iter()
            .map(|p| AsmPassthroughRecord {
                file: file.to_string(),
                line,
                isa: self.isa,
                ordinal: p.ordinal,
                text: p.text.clone(),
                reason: p.reason,
            })
            .collect()
    }

    /// 本次语句的 GAS 文本行 (去注释/去空行, 保留原文其字)。
    /// 文本侧与字节侧均出自同一次 `analyze`, 因此不可能出现两模式分叉。
    pub fn gas_lines(&self) -> Vec<String> {
        self.lines.iter().map(|l| l.text.clone()).collect()
    }
}

/// 后端接入点 (基础部分): 字节流与透传记录。
///
/// 为什么用 trait 注入而不是每个后端自己写一遍: 旧架构里 5 个后端各有一份
/// `encode_and_emit_asm`, 每份都在“出错还是透传”“记不记文本”上微妙不一致 ——
/// 那正是本方案要消灭的重复。策略只保留一份 (`record_asm_stmt`),
/// 后端只提供数据容器。
pub trait AsmOutcomeSink {
    /// 当前 .text 写入位置 (字节)
    fn pc(&self) -> u64;
    /// 追加机器码
    fn push_code(&mut self, bytes: &[u8]);
    /// 记录一段“无字节、只有文本”的透传区域
    fn save_passthrough_region(&mut self, pc: u64, lines: Vec<String>);
    /// 累加透传记录 (TU 级降级判据)
    fn add_passthrough_records(&mut self, recs: Vec<AsmPassthroughRecord>);
}

/// 后端接入点 (完整部分): 额外能保存内联 asm 的 GAS 原文。
///
/// 分开两个 trait 而不是合并: x86_64 的 `-S` 文本来自反汇编自己的字节, 本身
/// 不保留用户原文容器; 强行要求它实现 `save_asm_text` 只会诱导一个静默 no-op。
/// 能力差异因此留在类型层面, 而不是藏在空实现里。
pub trait AsmStmtSink: AsmOutcomeSink {
    /// 保存该语句的 GAS 文本行 (供 -S 与降级使用)
    fn save_asm_text(&mut self, key: &str, lines: Vec<String>);
}

/// 处理一条已解析的 asm 语句 —— 全仓唯一入口。
///
/// 不变量:
///   1. 无论何种目标模式, 文本与字节都来自同一次 `analyze` (一次分配、多处复用);
///   2. 一条语句要么全部可编码 (入字节流), 要么整体透传 (不产字节);
///      不允许“一半进字节流一半丢报错” —— 那会让后续 PC 相对偏移静默错位;
///   3. 本函数**不报错也**不降级: 它只记录事实, “E0311 还是改出文本”
///      由 `codegen::emit()` 根据 `--asm-fallback` 统一判定。
pub fn record_asm_stmt<S: AsmStmtSink + ?Sized>(
    sink: &mut S,
    isa: AsmIsa,
    key: &str,
    resolved: &str,
    span_file: &str,
    span_line: u32,
) {
    let snippet = analyze(isa, resolved);
    let lines = snippet.gas_lines();
    if !lines.is_empty() {
        sink.save_asm_text(key, lines.clone());
    }
    if snippet.needs_gas_text() {
        // 整体透传: 不产字节, 但把位置与文本记下, 降级时按位插入。
        let pc = sink.pc();
        sink.save_passthrough_region(pc, lines);
        sink.add_passthrough_records(snippet.collect_records(span_file, span_line));
    } else {
        let bytes = snippet.bytes.clone();
        sink.push_code(&bytes);
    }
}

/// 后端自带编码器判定“这一行我编不出来”时走这里。
///
/// 为什么需要它: x86_64 有自研完整汇编器 (不走 mini_asm 表), 它的失败只能在
/// 自己的发射路径里发现。但“失败 == 整 TU 需要 GAS 接手”这个结论必须全仓
/// 一致, 所以它不得自己发诊断/自己决定降级, 而是用同一个记录类型上报。
///
/// 注意: 调用前可能已写入指令前缀字节 (lock/rep/段前缀)。因此本函数只适用于
/// “要么整 TU 报错、要么整 TU 改出文本”的语境: 报错时字节不被使用,
/// 降级时文本 (含前缀) 是权威的 —— 两个出口都不会消费那几字节前缀。
pub fn note_backend_reject<S: AsmOutcomeSink + ?Sized>(
    sink: &mut S,
    isa: AsmIsa,
    text: &str,
    ordinal: usize,
    file: &str,
    line: u32,
) {
    let rec = AsmPassthroughRecord {
        file: file.to_string(),
        line,
        isa,
        ordinal,
        text: text.to_string(),
        reason: AsmPassthroughReason::BackendReported,
    };
    let pc = sink.pc();
    sink.save_passthrough_region(pc, vec![text.to_string()]);
    sink.add_passthrough_records(vec![rec]);
}

/// 由透传记录生成诊断 —— 全仓唯一出口。后端不得自己 push E0311,
/// 否则会出现“同一个缺陷在 5 个后端有 5 种措辞/行为”的分叉。
///
/// `supported_hint` 为该架构的已支持助记符列表 (供用户判断该改写法还是该降级)。
pub fn passthrough_diagnostics(
    records: &[AsmPassthroughRecord],
) -> Vec<crate::diagnostics::Diagnostic> {
    use crate::diagnostics::{Diagnostic, Span};
    if records.is_empty() {
        return Vec::new();
    }
    // 按 asm 语句分组: 同一条语句内的多行失败合并为一条诊断,
    // 避免一个 100 行的 barrier 块刷出 100 条重复错误。
    let mut out = Vec::new();
    let mut seen: Vec<(&str, u32)> = Vec::new();
    for r in records {
        if seen
            .iter()
            .any(|(f, l)| *f == r.file.as_str() && *l == r.line)
        {
            continue;
        }
        seen.push((&r.file, r.line));
        let group: Vec<&AsmPassthroughRecord> = records
            .iter()
            .filter(|x| x.file == r.file && x.line == r.line)
            .collect();
        let mut msg = if group.len() == 1 {
            // 单行: 直接把那行写进主句, 不再刷一条重复的列表项
            format!(
                "unsupported inline asm instruction on {} target: '{}' ({})",
                isa_name(group[0].isa),
                group[0].text,
                group[0].reason.as_str()
            )
        } else {
            format!(
                "{} inline asm instructions cannot be encoded by dcc on {} target",
                group.len(),
                isa_name(group[0].isa)
            )
        };
        if group.len() > 1 {
            for g in &group {
                msg.push_str(&format!("\n- {}: {}", g.text, g.reason.as_str()));
            }
        }
        let mut diag = Diagnostic::err("E0311", msg, Span::point(&r.file, r.line, 1));
        diag.hint = Some(
            "enable --asm-fallback=gas to emit this TU as GAS text and let the assembler \
             encode these instructions, or add the mnemonics to dcc's instruction table"
                .to_string(),
        );
        out.push(diag);
    }
    out
}

fn isa_name(isa: AsmIsa) -> &'static str {
    match isa {
        AsmIsa::X86_64 => "x86_64",
        AsmIsa::AArch64 => "aarch64",
        AsmIsa::RiscV64 => "riscv64",
        AsmIsa::RiscV32 => "riscv32",
        AsmIsa::Arm => "arm",
        AsmIsa::Mips32 => "mips",
        AsmIsa::Mips64 => "mips64",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aarch64_immediate_hash_is_not_treated_as_comment() {
        // 回归: 旧实现无条件按 `#` 截断, 把 `add x0, x0, #1` 削成 `add x0, x0,`
        let lines = split_source_lines(AsmIsa::AArch64, "add x0, x0, #1\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].1, "add x0, x0, #1");
    }

    #[test]
    fn aarch64_double_slash_is_the_comment_token() {
        let lines = split_source_lines(AsmIsa::AArch64, "nop // keep the pipeline idle\n");
        assert_eq!(lines[0].1, "nop");
    }

    #[test]
    fn arm_at_sign_is_comment_but_hash_is_immediate() {
        let lines = split_source_lines(AsmIsa::Arm, "mov r0, #1 @ load one\n");
        assert_eq!(lines[0].1, "mov r0, #1");
    }

    #[test]
    fn riscv_and_x86_use_hash_comments() {
        assert_eq!(split_source_lines(AsmIsa::RiscV64, "nop # idle")[0].1, "nop");
        assert_eq!(split_source_lines(AsmIsa::X86_64, "nop # idle")[0].1, "nop");
    }

    #[test]
    fn classify_distinguishes_directive_label_and_instruction() {
        assert_eq!(
            classify_line(".globl foo"),
            AsmLineKind::Directive {
                text: ".globl foo".to_string()
            }
        );
        assert_eq!(
            classify_line("1:"),
            AsmLineKind::LabelDef {
                name: "1".to_string()
            }
        );
        assert_eq!(
            classify_line("dmb ish"),
            AsmLineKind::Instruction {
                mnemonic: "dmb".to_string(),
                operands: vec!["ish".to_string()],
            }
        );
        // label 与指令同行: 编码对象是指令部分
        assert_eq!(
            classify_line("bar: nop"),
            AsmLineKind::Instruction {
                mnemonic: "nop".to_string(),
                operands: vec![],
            }
        );
    }

    #[test]
    fn analyze_returns_bytes_when_every_line_is_encodable() {
        let sn = analyze(AsmIsa::AArch64, "nop\nret\n");
        assert!(!sn.needs_gas_text());
        assert_eq!(sn.bytes.len(), 8, "two 4-byte instructions");
        assert_eq!(sn.bytes[0..4], crate::codegen::shared::mini_asm::encode_aarch64_nop());
    }

    #[test]
    fn analyze_records_passthrough_and_keeps_good_line_bytes() {
        // 旧实现: 任一行失败 -> 整个 Vec<u8> 作废, 好的行字节也一起丢
        let sn = analyze(AsmIsa::AArch64, "nop\nudf #0\n");
        assert!(sn.needs_gas_text());
        assert_eq!(sn.passthrough.len(), 1);
        assert_eq!(sn.passthrough[0].text, "udf #0");
        assert_eq!(
            sn.passthrough[0].reason,
            AsmPassthroughReason::UnknownMnemonic,
            "udf is not in the table at all"
        );
        assert_eq!(sn.bytes.len(), 4, "the encodable nop must still be accounted");
    }

    #[test]
    fn known_system_register_moves_are_passthrough_not_silent_nop() {
        // 回归: msr/mrs 曾被静默编码成 NOP —— 在内核 barrier/系统寄存器代码里
        // 这是最危险的 silent bug。现在它们必须被标记为透传。
        for src in ["msr daif, x0", "mrs x0, sp_el0"] {
            let sn = analyze(AsmIsa::AArch64, src);
            assert!(sn.needs_gas_text(), "{src} must not be silently encoded");
            assert!(sn.bytes.is_empty(), "{src} must contribute no bytes");
        }
    }

    #[test]
    fn mips_cache_is_passthrough_not_silent_nop() {
        let sn = analyze(AsmIsa::Mips32, "cache 0, 0($8)");
        assert!(sn.needs_gas_text());
        assert!(sn.bytes.is_empty());
    }

    #[test]
    fn asm_goto_label_after_resolution_is_a_label_not_an_instruction() {
        // 回归: `%0:` 这类解析后的标签定义不得被当成指令而报 E0311。
        for head in ["%0:", "1:", "$L:", "foo:"] {
            let sn = analyze(AsmIsa::AArch64, head);
            assert!(!sn.needs_gas_text(), "'{head}' is a label definition, not an instruction");
            assert!(sn.bytes.is_empty());
        }
        // 含冒号的 directive 仍按 directive 处理
        let sn = analyze(AsmIsa::AArch64, ".asciz \"a:b\"");
        assert!(!sn.needs_gas_text());
    }

    #[test]
    fn directives_and_labels_produce_no_instruction_bytes() {
        let sn = analyze(AsmIsa::AArch64, ".globl foo\nnop\nbar:\n");
        assert!(!sn.needs_gas_text());
        assert_eq!(sn.bytes.len(), 4);
        assert!(sn.all_non_code() == false);
    }

    #[test]
    fn unknown_isa_string_cannot_silently_reject_everything() {
        // AsmIsa 是枚举, 不存在 `arch: &str` 拼错导致全 TU 落入 `_ => None` 的路径
        assert!(!AsmIsa::X86_64.uses_mini_asm());
        assert_eq!(AsmIsa::Mips64.mini_asm_key(), Some("mips"));
    }

    #[test]
    fn degrade_granularity_is_length_class_anchored() {
        use DegradeGranularity::*;
        // 定长 ISA: 逐语句分流
        assert_eq!(AsmIsa::AArch64.degrade_granularity(), PerStatement);
        assert_eq!(AsmIsa::Arm.degrade_granularity(), PerStatement);
        assert_eq!(AsmIsa::Mips32.degrade_granularity(), PerStatement);
        assert_eq!(AsmIsa::Mips64.degrade_granularity(), PerStatement);
        // 变长 ISA (RISC-V 含 16/32 位压缩): 整 TU 分流
        assert_eq!(AsmIsa::RiscV64.degrade_granularity(), WholeTu);
        assert_eq!(AsmIsa::RiscV32.degrade_granularity(), WholeTu);
        // x86_64 自研完整汇编器, 不存在"半分流"; 事实就是整 TU 渲染
        assert_eq!(AsmIsa::X86_64.degrade_granularity(), WholeTu);
    }

    #[test]
    fn encoder_injection_point_drives_the_passthrough_decision() {
        // x86_64 风格: 后端自带编码器; 判定"需透传"仍由本模块统一产出
        let mut calls = 0;
        let sn = analyze_with(AsmIsa::X86_64, "lock cmpxchg %rax, (%rbx)\n", &mut |mnem, _ops| {
            calls += 1;
            // 模拟后端只认识 nop
            if mnem == "nop" {
                Some(vec![0x90])
            } else {
                None
            }
        });
        assert_eq!(calls, 1);
        assert!(sn.needs_gas_text());
        assert_eq!(sn.passthrough[0].reason, AsmPassthroughReason::UnsupportedOperandForm);
    }

    /// 四个 ISA 都必须把未知助记符判为透传 (接手已删除的
    /// `mini_asm::encode_asm_lines_for_arch` 的 unrecognized 系测试)。
    #[test]
    fn unrecognized_mnemonic_is_rejected_on_every_isa() {
        for isa in [
            AsmIsa::AArch64,
            AsmIsa::RiscV64,
            AsmIsa::Arm,
            AsmIsa::Mips32,
            AsmIsa::Mips64,
        ] {
            let sn = analyze(isa, "xyzzy_not_an_instruction x0");
            assert!(sn.needs_gas_text(), "{isa:?}: 未知助记符必须透传");
            assert!(sn.bytes.is_empty(), "{isa:?}: 未知助记符不得产字节");
            assert_eq!(
                sn.passthrough[0].reason,
                AsmPassthroughReason::UnknownMnemonic,
                "{isa:?}: 应被归类为助记符不在表内"
            );
        }
    }

    /// 每条名单助记符的一条合法 GAS 操作数样本 (该样本下 analyze() 必须产字节)。
    /// 样本形状覆盖该臂的实际判据 (寄存器对/三寄存器/向量/谓词 domain 选项),
    /// 因此它本身就是可达性证明 —— 名单漂移、臂被删或静默退化都会立即失败。
    ///
    /// 返回 None 表示「该 ISA 上此条名单项连一条合法操作数都产不出字节」
    /// (目前仅 MIPS32 的 daddu: 表臂要求 `is_64bit`, Mips32 上恒 None)。
    /// 对这类项, analyze() 必须报 UnsupportedOperandForm 而非 UnknownMnemonic。
    fn sample_operands(isa: AsmIsa, m: &str) -> Option<&'static str> {
        match (isa, m) {
            // aarch64 (29)
            (AsmIsa::AArch64, "add") => Some("x0, x1, x2"),
            (AsmIsa::AArch64, "blr") => Some("x9"),
            (AsmIsa::AArch64, "br") => Some("x9"),
            (AsmIsa::AArch64, "clrex") => Some(""),
            (AsmIsa::AArch64, "dbg") => Some("#0"),
            (AsmIsa::AArch64, "dmb") => Some("ish"),
            (AsmIsa::AArch64, "dsb") => Some("ish"),
            (AsmIsa::AArch64, "fadd") => Some("v0.2s, v1.2s, v2.2s"),
            (AsmIsa::AArch64, "fmul") => Some("v0.2s, v1.2s, v2.2s"),
            (AsmIsa::AArch64, "fsub") => Some("v0.2s, v1.2s, v2.2s"),
            (AsmIsa::AArch64, "isb") => Some(""),
            (AsmIsa::AArch64, "ld1") => Some("v0, x1"),
            (AsmIsa::AArch64, "ld1w") => Some("z0.s, x3"),
            (AsmIsa::AArch64, "mov") => Some("x0, x1"),
            (AsmIsa::AArch64, "movz") => Some("x0, #0x1ff"),
            (AsmIsa::AArch64, "mul") => Some("v0.16b, v1.16b, v2.16b"),
            (AsmIsa::AArch64, "nop") => Some(""),
            (AsmIsa::AArch64, "ptrue") => Some("p0"),
            (AsmIsa::AArch64, "ret") => Some(""),
            (AsmIsa::AArch64, "sev") => Some(""),
            (AsmIsa::AArch64, "sevl") => Some(""),
            (AsmIsa::AArch64, "st1") => Some("v3, x5"),
            (AsmIsa::AArch64, "st1w") => Some("z0.s, x3"),
            (AsmIsa::AArch64, "sub") => Some("x0, x1, x2"),
            (AsmIsa::AArch64, "svc") => Some("#0"),
            (AsmIsa::AArch64, "wfe") => Some(""),
            (AsmIsa::AArch64, "wfi") => Some(""),
            (AsmIsa::AArch64, "whilelt") => Some("p0.d, x0, x1"),
            (AsmIsa::AArch64, "yield") => Some(""),
            // riscv64 / riscv32 (12)
            (AsmIsa::RiscV64 | AsmIsa::RiscV32, "add") => Some("x0, x1, x2"),
            (AsmIsa::RiscV64 | AsmIsa::RiscV32, "addi") => Some("x0, x1, 8"),
            (AsmIsa::RiscV64 | AsmIsa::RiscV32, "ebreak") => Some(""),
            (AsmIsa::RiscV64 | AsmIsa::RiscV32, "ecall") => Some(""),
            (AsmIsa::RiscV64 | AsmIsa::RiscV32, "fence") => Some(""),
            (AsmIsa::RiscV64 | AsmIsa::RiscV32, "fence.i") => Some(""),
            (AsmIsa::RiscV64 | AsmIsa::RiscV32, "nop") => Some(""),
            (AsmIsa::RiscV64 | AsmIsa::RiscV32, "pause") => Some(""),
            (AsmIsa::RiscV64 | AsmIsa::RiscV32, "ret") => Some(""),
            (AsmIsa::RiscV64 | AsmIsa::RiscV32, "sfence.vma") => Some(""),
            (AsmIsa::RiscV64 | AsmIsa::RiscV32, "sub") => Some("x0, x1, x2"),
            (AsmIsa::RiscV64 | AsmIsa::RiscV32, "wfi") => Some(""),
            // arm (16; svc|swi 同臂)
            (AsmIsa::Arm, "add") => Some("r0, r1, r2"),
            (AsmIsa::Arm, "bx") => Some("lr"),
            (AsmIsa::Arm, "clrex") => Some(""),
            (AsmIsa::Arm, "dmb") => Some("ish"),
            (AsmIsa::Arm, "dsb") => Some("ish"),
            (AsmIsa::Arm, "isb") => Some(""),
            (AsmIsa::Arm, "mov") => Some("r0, r1"),
            (AsmIsa::Arm, "nop") => Some(""),
            (AsmIsa::Arm, "sev") => Some(""),
            (AsmIsa::Arm, "sevl") => Some(""),
            (AsmIsa::Arm, "sub") => Some("r0, r1, r2"),
            (AsmIsa::Arm, "svc") => Some("#0"),
            (AsmIsa::Arm, "swi") => Some("#0"),
            (AsmIsa::Arm, "wfe") => Some(""),
            (AsmIsa::Arm, "wfi") => Some(""),
            (AsmIsa::Arm, "yield") => Some(""),
            // mips (13; nop|ssnop 同臂)
            (AsmIsa::Mips32 | AsmIsa::Mips64, "add") => Some("$0, $1, $2"),
            (AsmIsa::Mips32 | AsmIsa::Mips64, "addu") => Some("$0, $1, $2"),
            (AsmIsa::Mips32 | AsmIsa::Mips64, "break") => Some(""),
            (AsmIsa::Mips32 | AsmIsa::Mips64, "daddu") if isa == AsmIsa::Mips64 => {
                Some("$0, $1, $2")
            }
            (AsmIsa::Mips32 | AsmIsa::Mips64, "jalr") => Some("$t1"),
            (AsmIsa::Mips32 | AsmIsa::Mips64, "jr") => Some("$ra"),
            (AsmIsa::Mips32 | AsmIsa::Mips64, "move") => Some("$2, $3"),
            (AsmIsa::Mips32 | AsmIsa::Mips64, "nop") => Some(""),
            (AsmIsa::Mips32 | AsmIsa::Mips64, "ssnop") => Some(""),
            (AsmIsa::Mips32 | AsmIsa::Mips64, "sub") => Some("$0, $1, $2"),
            (AsmIsa::Mips32 | AsmIsa::Mips64, "subu") => Some("$0, $1, $2"),
            (AsmIsa::Mips32 | AsmIsa::Mips64, "sync") => Some(""),
            (AsmIsa::Mips32 | AsmIsa::Mips64, "syscall") => Some(""),
            _ => None,
        }
    }

    /// 名单与编码表的一致性护栏: `known_mnemonics` 是诊断措辞与能力矩阵的数据源。
    ///
    /// 旧实现对 mini_asm.rs 源码做 `body.contains("\"ld\"")` 子串匹配, 前一项对
    /// `split_once("pub fn encode_aarch64(")` 直到文件尾的**整段 body** (含另三张表
    /// 与测试模块) 做匹配, 于是 "ld"/"st" 靠 dmb/dsb 臂的 domain 选项值 (0xd/0xe)
    /// 假绿通过, 且不验可达性。现改为行为判据: 对名单每条构造一条合法 GAS 操作数
    /// 文本走 analyze(), 断言产出字节非空 (即该臂真能把操作数编出来); 不在名单者
    /// 一律断言 UnknownMnemonic。
    #[test]
    fn mnemonic_table_matches_mini_asm_dispatch() {
        // 各 encode_* 函数体里被明确移除的静默 NOP 兜底, 不得回到名单里
        let removed_silent_nop = ["msr", "mrs", "cache", "pref"];
        for isa in [
            AsmIsa::AArch64,
            AsmIsa::RiscV64,
            AsmIsa::Arm,
            AsmIsa::Mips32,
            AsmIsa::Mips64,
        ] {
            // 1) 名单内每条: 合法样本必须产字节, 且不触发透传
            for m in known_mnemonics(isa) {
                let Some(sample) = sample_operands(isa, m) else {
                    // 该 ISA 上无合法样本 (MIPS32 daddu), 留待步骤 2 单独判定
                    continue;
                };
                let src = if sample.is_empty() {
                    m.to_string()
                } else {
                    format!("{m} {sample}")
                };
                let sn = analyze(isa, &src);
                assert!(
                    !sn.needs_gas_text(),
                    "known_mnemonics({isa:?}) lists '{m}' but '{src}' fails to encode: {:?}",
                    sn.passthrough.first().map(|p| p.reason)
                );
                assert!(
                    !sn.bytes.is_empty(),
                    "'{src}' on {isa:?} must emit bytes, not silently succeed with none"
                );
            }
            // 2) 名单内但该 ISA 连合法样本都产不出 (MIPS32 daddu):
            //    必须报 UnsupportedOperandForm, 而不是 UnknownMnemonic
            if let Some(m) = known_mnemonics(isa)
                .iter()
                .find(|m| sample_operands(isa, m).is_none())
            {
                let sn = analyze(isa, &format!("{m} $0, $1, $2"));
                assert!(sn.needs_gas_text());
                assert_eq!(
                    sn.passthrough[0].reason,
                    AsmPassthroughReason::UnsupportedOperandForm,
                    "'{m}' on {isa:?} is a known mnemonic with unsupported operand form, not unknown"
                );
            }
            // 3) 不在名单者: 一律 UnknownMnemonic, 绝不默默产字节
            for m in removed_silent_nop {
                assert!(
                    !known_mnemonics(isa).contains(&m),
                    "'{m}' must not be advertised: it used to be a silent NOP"
                );
                let sn = analyze(isa, &format!("{m} x0, x1"));
                assert!(
                    sn.needs_gas_text(),
                    "'{m}' on {isa:?} must be flagged for GAS passthrough"
                );
                assert!(sn.bytes.is_empty(), "'{m}' on {isa:?} must emit no bytes");
            }
        }
        // 4) 曾被假绿名单钉住、实际无臂的 aarch64 "ld"/"st" (domain 选项值):
        //    现在必须归为 UnknownMnemonic 透传, 不得假称支持。
        for m in ["ld", "st"] {
            let sn = analyze(AsmIsa::AArch64, &format!("{m} x0, x1"));
            assert_eq!(
                sn.passthrough[0].reason,
                AsmPassthroughReason::UnknownMnemonic,
                "aarch64 '{m}' has no table arm; it must be passthrough"
            );
            assert!(sn.bytes.is_empty());
        }
    }

    /// 交叉后端内联 asm 负向用例: 以下 GAS 语法都必须透传 (不产字节),
    /// 因为 mini_asm 尚不解析内存操作数括号与符号/标签目标。
    #[test]
    fn gas_syntax_beyond_table_passthroughs() {
        // aarch64: 花括号 + 方括号的 NEON 访存形式 (表里 ld1/st1 臂只认裸寄存器对,
        // 而 GAS 文本是 `{v0.16b}, [x0]`)。这是全仓唯一正向断言可达性的用例:
        // 裸 `["v0","x1"]` 走 ld1 能产字节 (见上一条护栏), 带括号的 GAS 文本必须透传。
        let sn = analyze(AsmIsa::AArch64, "ld1 {v0.16b}, [x0]");
        assert!(sn.needs_gas_text());
        assert!(sn.bytes.is_empty());
        // mips: 内存操作数 `4($t1)` 与分支标签 `.L`
        let sn = analyze(AsmIsa::Mips32, "lw $t0, 4($t1)");
        assert!(sn.needs_gas_text());
        assert!(sn.bytes.is_empty());
        let sn = analyze(AsmIsa::Mips32, "ble $t0, .L");
        assert!(sn.needs_gas_text());
        assert!(sn.bytes.is_empty());
        // 任意 ISA 的 `bl foo`: 表无符号/重定位表达位, 必然 UnknownMnemonic 透传
        let sn = analyze(AsmIsa::AArch64, "bl foo");
        assert!(sn.needs_gas_text());
        assert!(sn.bytes.is_empty());
        assert_eq!(sn.passthrough[0].reason, AsmPassthroughReason::UnknownMnemonic);
    }
}
