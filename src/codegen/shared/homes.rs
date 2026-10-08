// src/codegen/shared/homes.rs —— 「这个后端的家（home）能力面」的**唯一**声明处。
//
// 为什么必须有一份显式声明（`docs/p12-physreg-spill-design.md` §7 Phase 6.4）：
// P12 的位置表（`crate::location::Location`）与 `register asm("...")` 的用户文本请求
// 都是**前端产物**，四个非 x86_64 后端的变量模型今天只有栈（它们的 emit 目录里对
// `Location` 与 `register_name` 的引用数实测为 **0**）。那带来两种静默：
//
//   1. 分配器（只有 x86_64 跑）给出一个 `Reg`/`Spill`/`Imm` 家，别的后端读不到就当
//      没有 —— 那是「家/属性在某条通路上静默消失」那一族（§11.14 的 F3/F4 同族）。
//   2. 用户写 `register int p asm("rbx")` 编到 aarch64 上，声明被静默忽略，变量住在
//      栈槽里、读出来是未初始化的垃圾；而 gcc 在这里报
//      `error: invalid register name for ‘p’`（实测：aarch64/riscv64 交叉 gcc 对
//      `asm("rbx")` 都是硬错误，对 `asm("x1")`/`asm("sp")` 接受）。
//
// 本模块把这两件事变成**显式判据 + 显式诊断**：能力面按 `Arch` 穷举声明（新增架构
// 必须在这里表态，否则编不过），名字表按 target 各一张，非法名硬错误、合法名给出
// 「本后端尚未实现寄存器家」的硬错误（不再静默产出错的机器码）。
//
// x86_64 由 `codegen::x86_64::abi::regnames` 那一份表管（它有 `%` 前缀、位宽形态与
// 保留号规则，比这里的一层复杂得多），所以 `check_program_homes` 对 x86_64 直接返回空。

use crate::cli::Arch;
use crate::ast::Program;
use crate::ast::{Block, BlockItem, Decl, Stmt, VarDecl};
use crate::diagnostics::Diagnostic;
use crate::diagnostics::Severity;

/// 一个后端的「家」能力面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeSupport {
    /// 栈 + 物理寄存器（今天只有 x86_64：P12 的分配器、序言保存、asm 规划器都认号）。
    StackAndRegister,
    /// **只有栈**。收到任何非栈家都必须硬错误，不允许静默降级成栈槽。
    StackOnly,
}

/// 能力面按架构穷举声明。这里**不留 `_`**：新增 `Arch` 变体时必须在这里表态，
/// 否则就是「新后端默认静默忽略家」——那正是本文件要消除的形状。
pub fn home_support(arch: Arch) -> HomeSupport {
    match arch {
        Arch::X86_64 => HomeSupport::StackAndRegister,
        Arch::AArch64
        | Arch::Arm
        | Arch::RiscV64
        | Arch::RiscV32
        | Arch::Mips
        | Arch::Mips64 => HomeSupport::StackOnly,
    }
}

/// 该架构 `register asm("...")` 可写的**寄存器基名**（小写，不含 `%`/`$` 前缀）。
///
/// 表的口径 = 「gcc 会不会接受这个写法」，不是「后端能不能实现它」：能不能实现由
/// `HomeSupport` 与下面的诊断决定，而**名字合不合法**必须与 gcc 同判据，否则
/// 「dcc 拒了 gcc 接受的」与「dcc 收了 gcc 拒的」两种分歧都会出现。
/// 因此每张表都按该 target 的 ABI 别名展开（aarch64 的 x0-x30/sp、riscv 的
/// 双列名 x0-x31 与 abi 名、arm 的 r0-r15 与 a1-a4/v1-v8、mips 的 $0-$31 与 abi 名）。
pub fn target_reg_names(arch: Arch) -> &'static [&'static str] {
    match arch {
        // x86_64 由 `abi/regnames.rs` 那张 132 条的表管（带位宽形态与 % 规则）。
        Arch::X86_64 => &[],
        Arch::AArch64 => &[
            "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12",
            "x13", "x14", "x15", "x16", "x17", "x18", "x19", "x20", "x21", "x22", "x23", "x24",
            "x25", "x26", "x27", "x28", "x29", "x30", "sp",
        ],
        Arch::RiscV64 | Arch::RiscV32 => &[
            "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12",
            "x13", "x14", "x15", "x16", "x17", "x18", "x19", "x20", "x21", "x22", "x23", "x24",
            "x25", "x26", "x27", "x28", "x29", "x30", "x31",
            // riscv ABI 别名（实测 gcc 同样接受）
            "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "s0",
            "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "a0", "a1",
            "a2", "a3", "a4", "a5", "a6", "a7", "fp",
        ],
        Arch::Arm => &[
            "r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "r12",
            "r13", "r14", "r15",
            // arm AAPCS 别名（gcc 的 arm 手册拿 `asm ("a5")` 举例）
            "a1", "a2", "a3", "a4", "a5", "a6", "a7", "a8", "v1", "v2", "v3", "v4", "v5",
            "v6", "v7", "v8", "ip", "fp", "sp", "lr", "pc",
        ],
        Arch::Mips | Arch::Mips64 => &[
            // 数字号与 ABI 名两套都收（内核与 gcc 文档两边都有人用）。
            // 实测部分：本机只装了 mips o32 的 gcc（它拒 `a5`/`s8`/`lr`/`pc`/`r12`，
            // 收 `at`/`v1`/`fp`/`sp`/`ra`/`28`），mips64 与 arm 的交叉 gcc 本机没装。
            // 对只支持栈的后端而言，这张表**只影响诊断措辞**（两侧都是错误），
            // 所以取两张 ABI 的并集不会误拒合法程序，也不会保留静默。
            "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14",
            "15", "16", "17", "18", "19", "20", "21", "22", "23", "24", "25", "26", "27",
            "28", "29", "30", "31",
            "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7", "t0",
            "t1", "t2", "t3", "t4", "t5", "t6", "t7", "s0", "s1", "s2", "s3", "s4", "s5",
            "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp", "fp", "s8", "ra",
        ],
    }
}

/// 用户文本名在该架构是否**是一个寄存器名**。接受 `$`/`%` 前缀与 mips 的 `$28`
/// 形态，但**不接受大写**（与 gcc 一致：x86_64 那边实测 `asm("RBX")` 也是硬错误）。
pub fn reg_name_is_known(arch: Arch, text: &str) -> bool {
    let bare = text.trim().trim_start_matches('%').trim_start_matches('$');
    if bare.is_empty() || !bare.is_ascii() {
        return false;
    }
    // 大小写敏感：gcc 在这里是 invalid register name。
    if bare.chars().any(|c| c.is_ascii_uppercase()) {
        return false;
    }
    if arch == Arch::X86_64 {
        // x86_64 走它自己那一张表（带位宽形态与 `%` 规则）：这里不重写一遍判据，
        // 否则同一件事又出现两套口径。
        return crate::codegen::x86_64::is_user_register_name(text);
    }
    target_reg_names(arch).iter().any(|n| *n == bare)
}

/// 诊断码：本后端没有寄存器家，却收到了非栈家（后端内部不变量被违反）。
pub const DIAG_HOME_NOT_REPRESENTABLE: &str = "E1130";
/// 诊断码：`register asm("...")` 在该 target 上不是合法的寄存器名，或本后端实现不了
/// 这个家（两种情况都不允许静默）。
pub const DIAG_REGISTER_HOME_UNSUPPORTED: &str = "E1131";

/// 扫描整个程序里所有 `VarDecl`（含文件级、块内、嵌套函数体内、语句子树里的声明），
/// 检查它们的「家」是否落在该后端的能力面上。
pub fn check_program_homes(arch: Arch, prog: &Program) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if home_support(arch) == HomeSupport::StackAndRegister {
        // x86_64 有自己的两份判据（名字表 + 保留号），不在这里重复一套。
        return out;
    }
    for d in &prog.decls {
        check_decl(arch, d, &mut out);
    }
    out
}

fn check_decl(arch: Arch, d: &Decl, out: &mut Vec<Diagnostic>) {
    match d {
        Decl::Var(v) => {
            check_var(arch, v, out);
            // 初始化式里也能嵌着语句表达式块（`int t = ({ register int j asm("rbx"); j; })`），
            // 那里的声明同样是声明：不递归就是「家在某条通路上静默消失」。
            if let Some(init) = &v.init {
                check_expr(arch, init, out);
            }
        },
        Decl::Func(f) => {
            // 形参不携带家（`Param` 没有 register_name/home 字段：用户写在形参上的
            // `register` 只能在变量声明处表达，实测见 `src/ast.rs::Param`）
            if let Some(b) = &f.body {
                check_block(arch, b, out);
            }
        },
        // 类型定义与 asm 声明不携带家
        Decl::StructDef(_) | Decl::UnionDef(_) | Decl::EnumDef(_) | Decl::Typedef { .. }
        | Decl::GlobalAsm { .. } => {},
    }
}

fn check_block(arch: Arch, b: &Block, out: &mut Vec<Diagnostic>) {
    for item in &b.items {
        match item {
            BlockItem::Decl(d) => check_decl(arch, d, out),
            BlockItem::Stmt(s) => check_stmt(arch, s, out),
        }
    }
}

/// 语句子树里的声明也要扫（`for (register int i asm("rbx") ...)`、`({...})`、
/// 带标签的块都实测过会漏 —— 漏一处就是「家在某条通路上静默消失」）。
/// 这里**不留 `_`**：新增 `Stmt` 变体会直接编不过。
fn check_stmt(arch: Arch, s: &Stmt, out: &mut Vec<Diagnostic>) {
    match s {
        Stmt::Expr { e, .. } => check_expr(arch, e, out),
        Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Goto { .. }
        | Stmt::Fallthrough { .. } => {},
        Stmt::GotoIndirect { expr, .. } => check_expr(arch, expr, out),
        Stmt::Return { val: Some(e), .. } => check_expr(arch, e, out),
        Stmt::Return { val: None, .. } => {},
        Stmt::Block { b, .. } => check_block(arch, b, out),
        Stmt::If { cond, then_, else_, .. } => {
            check_expr(arch, cond, out);
            check_stmt(arch, then_, out);
            if let Some(e) = else_ {
                check_stmt(arch, e, out);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
            check_expr(arch, cond, out);
            check_stmt(arch, body, out);
        },
        Stmt::For { init, cond, step, body, .. } => {
            if let Some(i) = init {
                check_stmt(arch, i, out);
            }
            if let Some(c) = cond {
                check_expr(arch, c, out);
            }
            if let Some(s) = step {
                check_expr(arch, s, out);
            }
            check_stmt(arch, body, out);
        },
        Stmt::Switch { expr, cases, .. } => {
            check_expr(arch, expr, out);
            for c in cases {
                check_block(arch, &c.body, out);
            }
        },
        Stmt::Label { stmt, .. } => check_stmt(arch, stmt, out),
        Stmt::AsmGoto {
            outputs, inputs, ..
        } => {
            for x in outputs.iter().chain(inputs.iter()) {
                check_expr(arch, x, out);
            }
        },
        // 受保护调用里也可能有声明（C++ 的 `try` 块在降级后就是语句）
        Stmt::Invoke { call, .. } => check_stmt(arch, call, out),
        Stmt::EhLandingPad {
            object, selector, ..
        } => {
            check_expr(arch, object, out);
            check_expr(arch, selector, out);
        },
    }
}

/// 表达式树里的语句表达式块 `({...})` 也带声明，所以整棵树都要走。
/// 这里**不留 `_`**：新增 `Expr` 变体会直接编不过，而不是静默漏一个子树
/// （§11.14 F4 与 `opt.rs::expr_has_unsafe_ssa_pattern` 是同一条纪律）。
fn check_expr(arch: Arch, e: &crate::ast::Expr, out: &mut Vec<Diagnostic>) {
    use crate::ast::Expr as E;
    match e {
        E::StmtExpr { block, .. } => check_block(arch, block, out),
        E::Int { .. }
        | E::Float { .. }
        | E::ComplexLit { .. }
        | E::ComplexIntLit { .. }
        | E::String { .. }
        | E::Char { .. }
        | E::Ident { .. }
        | E::SizeofTy { .. }
        | E::AlignofTy { .. }
        | E::LabelAddr { .. } => {},
        E::Offsetof { indices, .. } => {
            for idx in indices {
                check_expr(arch, idx, out);
            }
        },
        E::Unary { e: x, .. }
        | E::Cast { e: x, .. }
        | E::AddrOf { e: x, .. }
        | E::Deref { e: x, .. }
        | E::Real { e: x, .. }
        | E::Imag { e: x, .. }
        | E::SizeofExpr { e: x, .. }
        | E::AlignofExpr { e: x, .. }
        | E::VaArg { ap: x, .. }
        | E::PostInc { e: x, .. }
        | E::PostDec { e: x, .. }
        | E::PreInc { e: x, .. }
        | E::PreDec { e: x, .. } => check_expr(arch, x, out),
        E::Binop { l, r, .. }
        | E::Assign { l, r, .. }
        | E::Comma { l, r, .. }
        | E::Index { arr: l, idx: r, .. } => {
            check_expr(arch, l, out);
            check_expr(arch, r, out);
        },
        E::Cond { cond, then_, else_, .. } => {
            check_expr(arch, cond, out);
            check_expr(arch, then_, out);
            check_expr(arch, else_, out);
        },
        E::Member { expr, .. } => check_expr(arch, expr, out),
        E::Call { callee, args, .. } => {
            check_expr(arch, callee, out);
            for a in args {
                check_expr(arch, a, out);
            }
        },
        E::CompoundLit { inits, .. } | E::InitList { inits, .. } => {
            for it in inits {
                match it {
                    crate::ast::InitItem::Expr(x) => check_expr(arch, x, out),
                    crate::ast::InitItem::Designated { designator, value } => {
                        check_designator(arch, designator, out);
                        check_expr(arch, value, out);
                    },
                }
            }
        },
        E::AsmExpr {
            outputs, inputs, ..
        } => {
            for x in outputs.iter().chain(inputs.iter()) {
                check_expr(arch, x, out);
            }
        },
    }
}

fn check_designator(arch: Arch, d: &crate::ast::Designator, out: &mut Vec<Diagnostic>) {
    use crate::ast::Designator as D;
    match d {
        D::Field(_) => {},
        D::Index(x) => check_expr(arch, x, out),
        D::Range(a, b) => {
            check_expr(arch, a, out);
            check_expr(arch, b, out);
        },
        D::Chain(items) => {
            for sub in items {
                check_designator(arch, sub, out);
            }
        },
    }
}

fn check_var(arch: Arch, v: &VarDecl, out: &mut Vec<Diagnostic>) {
    // 1) 非栈家：后端只有栈 ⇒ 硬错误，不静默降级（今天没有生产者走到这里，
    //    所以这条由用例强制构造，见下面 `non_stack_home_is_a_hard_error_*`）。
    if let Some(loc) = v.home {
        if !matches!(loc, crate::location::Location::Stack(_)) {
            out.push(Diagnostic {
                sev: Severity::Error,
                code: DIAG_HOME_NOT_REPRESENTABLE,
                msg: format!(
                    "target '{}' 的变量模型只有栈，但变量 '{}' 被给了非栈的家 {:?}；\
                     静默降级成栈槽会让「谁持有这个值」变成两套事实",
                    arch_name(arch),
                    v.name.name,
                    loc
                ),
                span: v.span.clone(),
                source_line: None,
                hint: Some(
                    "该后端没有物理寄存器家：要么在分配器侧就不要产出这个家，\
                     要么给这个后端补 Home 抽象与序言保存通道"
                        .to_string(),
                ),
            });
        }
    }
    // 2) 用户文本请求：名字不合法 ⇒ 与 gcc 同判据的 invalid register name；
    //    名字合法但本后端没有寄存器家 ⇒ 同样是错误（旧行为是静默忽略，变量
    //    留在栈槽里、读出来是未初始化的垃圾）。
    if let Some(text) = v.register_name.as_deref() {
        if !reg_name_is_known(arch, text) {
            out.push(Diagnostic {
                sev: Severity::Error,
                code: DIAG_REGISTER_HOME_UNSUPPORTED,
                msg: format!(
                    "invalid register name for ‘{}’：'{}' 不是 target '{}' 的寄存器名",
                    v.name.name,
                    text,
                    arch_name(arch)
                ),
                span: v.span.clone(),
                source_line: None,
                hint: Some("该 target 可用的寄存器名见 gcc 的 global register variables 一节".to_string()),
            });
        } else {
            // 名字是本 target 的寄存器，但本后端实现不了这个家：报**警告**而不是错误。
            //
            // 为什么不硬拒（§11.6.4 自己定的约束：「新增的非法名报错只针对本来就不是
            // 寄存器名的输入（gcc 也报错），不得把原本能编的合法程序变成编不了」）：
            // `register unsigned long current_stack_pointer asm("sp")` 是内核在用的
            // 写法，riscv64/arm 的 gcc 都接受它；在这里改成错误就是把能编的源码
            // 弄红。但也不允许继续静默：诊断里写清「变量留在栈槽里、读出来是
            // 未初始化的值」与要补什么，同一形状的处理先例是 §11.14 F5 对 `%rsp` 的
            // 分寸（只读的别名保留，真正危到的才拒）。
            out.push(Diagnostic {
                sev: Severity::Warning,
                code: DIAG_REGISTER_HOME_UNSUPPORTED,
                msg: format!(
                    "target '{}' 尚不支持把变量 '{}' 绑到寄存器 '{}'：本后端的变量模型只有栈，\
                     该声明现在留在栈槽里（读出来是未初始化的值）",
                    arch_name(arch),
                    v.name.name,
                    text
                ),
                span: v.span.clone(),
                source_line: None,
                hint: Some(
                    "要支持它，需要给该后端补 Home 抽象、序言保存通道与干涉表；\
                     见 docs/p12-physreg-spill-design.md §7 Phase 6.4"
                        .to_string(),
                ),
            });
        }
    }
}

fn arch_name(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x86_64",
        Arch::AArch64 => "aarch64",
        Arch::RiscV64 => "riscv64",
        Arch::RiscV32 => "riscv32",
        Arch::Mips64 => "mips64",
        Arch::Mips => "mips",
        Arch::Arm => "arm",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::location::{Location, RegBank, RegId};

    fn parse_ok(src: &str) -> Program {
        let (prog, diags) = crate::parser::parse(src.as_bytes(), "homes_test.c");
        let hard: Vec<String> = diags
            .iter()
            .filter(|d| d.sev == Severity::Error)
            .map(|d| d.msg.clone())
            .collect();
        assert!(hard.is_empty(), "夹具本身必须能解析: {:?}", hard);
        prog
    }

    /// 把文件级第一个变量的家改掉：今天只有 x86_64 的分配器会产家，所以「其他后端
    /// 收到非栈家」这个状态只能强制构造 —— 而这正是本判据存在的理由（它必须已经是
    /// 硬的，生产者一出现就不会静默）。
    fn set_home(prog: &mut Program, home: Location) {
        if let Some(Decl::Var(v)) = prog.decls.first_mut() {
            v.home = Some(home);
        } else {
            panic!("夹具的第一个声明必须是变量");
        }
    }

    /// 能力面必须穷举声明：七个架构各有结论，且只有 x86_64 认寄存器家。
    #[test]
    fn only_x86_64_declares_register_homes() {
        assert_eq!(home_support(Arch::X86_64), HomeSupport::StackAndRegister);
        for a in [
            Arch::AArch64,
            Arch::Arm,
            Arch::RiscV64,
            Arch::RiscV32,
            Arch::Mips,
            Arch::Mips64,
        ] {
            assert_eq!(
                home_support(a),
                HomeSupport::StackOnly,
                "{:?} 必须显式声明只支持栈",
                a
            );
        }
    }

    /// 名字表按 target 走：同一份源在 x86_64 合法的名字，在 aarch64/riscv64 必须是
    /// 非法名（实测交叉 gcc 就是这样：aarch64 对 `asm("rbx")` 报
    /// `invalid register name for ‘p’`，对 `asm("x1")`/`asm("sp")` 接受）。
    #[test]
    fn register_names_are_resolved_per_target_not_globally() {
        assert!(reg_name_is_known(Arch::AArch64, "x1"));
        assert!(reg_name_is_known(Arch::AArch64, "sp"));
        assert!(!reg_name_is_known(Arch::AArch64, "rbx"));
        assert!(!reg_name_is_known(Arch::AArch64, "X1"), "大写不认（与 gcc 同判据）");
        assert!(reg_name_is_known(Arch::RiscV64, "a0"));
        assert!(reg_name_is_known(Arch::RiscV64, "x2"));
        assert!(reg_name_is_known(Arch::Mips, "sp"));
        assert!(reg_name_is_known(Arch::Mips, "28"));
        assert!(reg_name_is_known(Arch::Mips, "$28"), "mips 的 $ 前缀形态");
        assert!(!reg_name_is_known(Arch::Mips, "x1"));
        assert!(reg_name_is_known(Arch::Arm, "a5"), "gcc arm 手册就用这个写法举例");
        assert!(!reg_name_is_known(Arch::Arm, ""));
        assert!(!reg_name_is_known(Arch::Arm, "nosuchreg"));
        assert!(reg_name_is_known(Arch::X86_64, "%rbx"), "x86_64 用它自己那张表");
        assert!(!reg_name_is_known(Arch::X86_64, "RBX"));
        assert!(!reg_name_is_known(Arch::X86_64, "r16"), "全局寄存器变量不认 APX 号");
    }

    /// 非栈家在只支持栈的后端必须是错误；栈家一条都不许报；x86_64 由它自己的
    /// 判据管，这里必须整体沉默（否则同一件事两套诊断，谁赢取决于调用顺序）。
    #[test]
    fn non_stack_home_is_a_hard_error_on_stack_only_backends() {
        let reg_home = Location::Reg(RegId {
            bank: RegBank::Gp,
            n: 3,
        });
        for arch in [Arch::AArch64, Arch::RiscV64, Arch::Arm, Arch::Mips] {
            let mut prog = parse_ok("int gv;\nint main(void){return gv;}\n");
            set_home(&mut prog, reg_home);
            let diags = check_program_homes(arch, &prog);
            assert_eq!(diags.len(), 1, "{:?} 应点名一个非栈家", arch);
            assert_eq!(diags[0].code, DIAG_HOME_NOT_REPRESENTABLE);
            assert_eq!(diags[0].sev, Severity::Error, "非栈家不能是警告");
            assert!(diags[0].msg.contains("gv"), "诊断里要带变量名: {}", diags[0].msg);
        }

        // 反向对照：栈家（含「后端定槽」的占位形态）不得报任何东西
        for arch in [Arch::AArch64, Arch::RiscV64, Arch::Arm, Arch::Mips, Arch::X86_64] {
            let mut prog = parse_ok("int gv;\nint main(void){return gv;}\n");
            set_home(&mut prog, Location::Stack(-16));
            assert!(
                check_program_homes(arch, &prog).is_empty(),
                "{:?} 上栈家是能力面内的事",
                arch
            );
            set_home(&mut prog, Location::backend_assigned_stack());
            assert!(check_program_homes(arch, &prog).is_empty());
        }

        // x86_64 侧的非栈家由它自己的判据管，本模块必须沉默
        let mut prog = parse_ok("int gv;\nint main(void){return gv;}\n");
        set_home(&mut prog, reg_home);
        assert!(check_program_homes(Arch::X86_64, &prog).is_empty());
    }

    /// 用户写的 `register asm(...)` 在任何只支持栈的后端都不允许静默，但分寸不同：
    /// 非法名（该 target 没这个寄存器）是**错误**，与 gcc 同判据；合法名但本后端
    /// 实现不了是**警告**，不改变更能编的事实（§11.6.4 的约束：不得把原本能编的
    /// 合法程序变成编不了）。
    #[test]
    fn register_asm_request_is_never_silently_dropped() {
        for (arch, name) in [
            (Arch::AArch64, "rbx"),
            (Arch::Mips, "x1"),
            (Arch::Arm, "nosuchreg"),
        ] {
            let prog = parse_ok(&format!(
                "register unsigned long p asm(\"{}\");\nunsigned long f(void){{return p;}}\n",
                name
            ));
            let diags = check_program_homes(arch, &prog);
            assert_eq!(diags.len(), 1, "{:?} 上的非法名 asm(\"{}\") 不该静默", arch, name);
            assert_eq!(diags[0].code, DIAG_REGISTER_HOME_UNSUPPORTED);
            assert_eq!(diags[0].sev, Severity::Error, "非法名与 gcc 同样是错误");
            assert!(diags[0].msg.contains("p"), "诊断里要带变量名: {}", diags[0].msg);
        }
        for (arch, name) in [
            (Arch::AArch64, "sp"),
            (Arch::RiscV64, "sp"),
            (Arch::Mips, "k0"),
            (Arch::Arm, "a5"),
        ] {
            let prog = parse_ok(&format!(
                "register unsigned long p asm(\"{}\");\nunsigned long f(void){{return p;}}\n",
                name
            ));
            let diags = check_program_homes(arch, &prog);
            assert_eq!(diags.len(), 1, "{:?} 上的合法名也要给出诊断", arch);
            assert_eq!(diags[0].code, DIAG_REGISTER_HOME_UNSUPPORTED);
            assert_eq!(
                diags[0].sev,
                Severity::Warning,
                "合法名不能改成错误：那会把 gcc 能编的内核写法弄红"
            );
        }
        // 两种情形的 msg 要能分开（否则用户看不出该改名还是该换后端）
        let bad = check_program_homes(
            Arch::AArch64,
            &parse_ok("register unsigned long p asm(\"rbx\");\n"),
        );
        let unsup = check_program_homes(
            Arch::AArch64,
            &parse_ok("register unsigned long p asm(\"x1\");\n"),
        );
        assert_ne!(bad[0].msg, unsup[0].msg);
        assert!(bad[0].msg.contains("invalid register name"));
        assert!(unsup[0].msg.contains("尚不支持"));
        assert!(unsup[0].msg.contains("栈槽"), "要说清现在实际发生的是什么");
    }

    /// 声明可能藏在**语句子树**里：`for` 的 init、语句表达式块、嵌套函数体。
    /// 少一条通路就是「家在某条通路上静默消失」的同族形状（§11.14 的 F3/F4）。
    #[test]
    fn declarations_inside_statement_subtrees_are_scanned() {
        let prog = parse_ok(
            "int m(void){\n\
             \x20 for (register int i asm(\"rbx\"); i < 2; i++) { }\n\
             \x20 int t = ({ register int j asm(\"rbx\"); j; });\n\
             \x20 auto int nf(void){ register int k asm(\"rbx\"); return k; }\n\
             \x20 return t + nf() + i;\n\
             }\n",
        );
        let diags = check_program_homes(Arch::AArch64, &prog);
        assert_eq!(
            diags.len(),
            3,
            "for-init / 语句表达式 / 嵌套函数体各一条，实际 {} 条: {:?}",
            diags.len(),
            diags.iter().map(|d| d.msg.clone()).collect::<Vec<_>>()
        );
    }

    /// 反向对照：不带任何家请求、也没有非栈家的普通程序，在**所有**后端上都必须
    /// 零诊断 —— 否则这条判据就成了「所有交叉编译都报错」的假绿防护反面。
    #[test]
    fn plain_programs_get_no_diagnostics_on_any_target() {
        let prog = parse_ok(
            "struct S { int a; char b[4]; };\n\
             static int g;\n\
             int f(struct S s, int *p) { register int r; for (int i = 0; i < 4; i++) r += s.b[i]; return r + *p + g; }\n\
             int main(void){ struct S s; s.a = 1; return f(s, &g); }\n",
        );
        for arch in [
            Arch::X86_64,
            Arch::AArch64,
            Arch::Arm,
            Arch::RiscV64,
            Arch::RiscV32,
            Arch::Mips,
            Arch::Mips64,
        ] {
            assert!(
                check_program_homes(arch, &prog).is_empty(),
                "{:?} 上普通程序不该产生任何诊断",
                arch
            );
        }
    }
}
