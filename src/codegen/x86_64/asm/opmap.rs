//! asm/opmap.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::Codegen;
use crate::codegen::x86_64::*;
use crate::const_eval::{self, ConstValue};
use crate::debug::*;
use crate::diagnostics::{Diagnostic, Severity, Span};
use crate::ir::{self, *};
use crate::semantics::{self, *};
use crate::target::Target;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// 一个 GP 操作数装载的**延迟动作**：在全部操作数求值完成之后、模板发射之前才真正
/// 写进目标寄存器。
///
/// 为什么必须延迟：`prepare_asm_binding` 走 `gen_expr`，而 `gen_expr` 把 %rax 当求值
/// 基址（`EVAL_BASE`），部分子路径还硬写 %rcx/%rdx/%rsi/%rdi。在操作数循环里逐条
/// 装载，**后面**操作数一求值，前面已就位的固定寄存器就被踩掉。实测内核
/// `arch/x86/kernel/cpu/scattered.c` 的 `native_cpuid`：
/// `"0"(*eax) / "2"(*ecx)` 两个输入，第二个经 %rax 中转后把 EAX 里的 cpuid leaf
/// 冲成 0（读回厂商串），guest 在 `init_scattered_cpuid_features` 就地 #PF。
#[derive(Debug)]
enum DeferredGpLoad {
    /// 表达式的值直接装进目标（只收「单条装载指令」的形态，见
    /// `asm_gp_load_is_single_insn`）。
    Direct { target: u8, expr: Expr },
    /// 值已经求到 callee-saved 驻足号，flush 时一条 `mov` 进目标。
    FromScratch { target: u8, scratch: u8 },
    /// 从 `base` 指向的内存按 `size` 装载进目标（`"+r"(*p)` 的旧值）。
    FromPtr { target: u8, base: u8, size: usize },
    /// `base` 是 sret 帧槽里的地址（驻足池耗尽时的 `"+r"(*p)`）：先把地址装进
    /// 目标，再用目标自己当基址取旧值（`load_stack` + `mov t, [t]`）。
    FromAddrSlot { target: u8, slot: i64, size: usize },
    /// 全局符号地址装进目标（`"=r"(global)` 的写回地址）。
    GlobalAddr { target: u8, name: String },
}

impl DeferredGpLoad {
    fn target(&self) -> u8 {
        match *self {
            DeferredGpLoad::Direct { target, .. }
            | DeferredGpLoad::FromScratch { target, .. }
            | DeferredGpLoad::FromPtr { target, .. }
            | DeferredGpLoad::FromAddrSlot { target, .. }
            | DeferredGpLoad::GlobalAddr { target, .. } => target,
        }
    }
}

impl Codegen {
    /// 报障窗口：`DCC_DBG_ASM=1` 在 stderr 逐条打印内联 asm 的操作数绑定
    /// （约束、名字、解析出的槽/寄存器、最终替换表）。只读不改：不进任何判据、
    /// 不进产物字节。
    ///
    /// 为什么必须有它：§5.11 那一族缺陷的现场是「模板里 `%[var]` 换错了对象」，
    /// 而从产物上只能看到一个错值，从 AST 上又看不出是上游改了 `outputs` 还是
    /// 本函数绑错了 —— 两侧一打就能分。`OnceLock` 只读一次 env：本函数每条 asm
    /// 调两次，一个内核 TU 就是几百次 getenv。
    fn asm_operand_map_dbg() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("DCC_DBG_ASM").is_some())
    }

    fn asm_format_subst(val: &str) -> String {
        // 缺失操作数占位符会被保留为 `%[name]` / `%N`; 这些不是
        // mini-asm 可编码操作数, 让指令整体走 warning + nop fallback。
        if val.starts_with("%[") || val.starts_with('%') {
            return val.to_string();
        }
        // 内存操作数 (含 '('), 立即数 (以 $ 开头), 偏移 (以 - 开头),
        // 纯数字或符号立即数: 直接使用, 不加 % 前缀
        if val.contains('(')
            || val.starts_with('$')
            || val.starts_with('-')
            || val.chars().next().is_some_and(|c| c.is_ascii_digit())
            || Self::asm_symbolic_operand(val)
        {
            val.to_string()
        } else {
            format!("%{val}")
        }
    }
    pub(crate) fn asm_format_subst_safe(val: &str) -> String {
        let base = Self::asm_format_subst(val);
        if val.starts_with('$') && Self::asm_imm_needs_parens(val) {
            // $(expr) 形式: 把 $ 移到括号外
            format!("$({})", &val[1..])
        } else {
            base
        }
    }
    pub(crate) fn asm_has_unresolved_operand(line: &str) -> bool {
        line.contains("%[")
            || line
                .as_bytes()
                .windows(2)
                .any(|w| w[0] == b'%' && w[1].is_ascii_digit())
    }
    fn asm_load_address_to_reg(&mut self, expr: &Expr, reg: u8) {
        // p 约束: 把指针值加载到寄存器 (而不是 lea 变量地址)
        // 对于指针变量, 加载其值; 对于其他表达式, 按值求值
        self.gen_expr(expr, reg);
    }

    /// 约束里的固定 GP 号（`a/b/c/d/S/D` 或 `{eax}` 这类显式寄存器）。
    ///
    /// 与规划器 `alloc_gp_for_constraint` 的字母表一致；`r/R/g`、`q/Q`、`@cc` 不钉号。
    /// 约束文本 → 被固定点名的 GP 号（字母约束 `"b"`→rbx 与显式名 `{r12}` 同一口径）。
    ///
    /// 可见性从私有提到 `pub(crate)`：帧规划期（`Codegen::compute_callee_plan`）要用
    /// 同一个判据算「哪些池号被 asm 钉死」，不能在那里再写一份字母表 —— 两份表
    /// 一定会漂移，而漂移的方向是「计划漏了号 ⇒ 序言少一条保存」。
    pub(crate) fn fixed_gp_of_constraint(raw: &str) -> Option<u8> {
        if let Some(eri) = Self::asm_parse_explicit_reg(raw) {
            return match eri.kind {
                ExplicitRegKind::Gp => Some(eri.reg),
                ExplicitRegKind::Seg => None,
            };
        }
        let core = raw.trim_start_matches(['=', '+', '&', '%']);
        if core.contains('{') || core.starts_with("@cc") {
            return None;
        }
        let letters: Vec<char> = core.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        if letters.iter().any(|c| matches!(c, 'r' | 'R' | 'g')) {
            return None;
        }
        if letters.contains(&'a') {
            Some(0)
        } else if letters.contains(&'b') {
            Some(3)
        } else if letters.contains(&'c') {
            Some(1)
        } else if letters.contains(&'d') {
            Some(2)
        } else if letters.contains(&'S') {
            Some(6)
        } else if letters.contains(&'D') {
            Some(7)
        } else {
            None
        }
    }

    /// 单条装载即可入寄存器：局部标量 Ident（栈家或 `register asm` 家）与整型常量。
    /// 全局 Ident 不算 —— TLS 形态走 `__tls_get_addr` 调用，不是一条指令。
    fn asm_gp_load_is_single_insn(&self, op: &Expr) -> bool {
        match op {
            Expr::Int { .. } | Expr::Char { .. } => true,
            Expr::Ident { name, ty, .. } => {
                if self.is_global_not_local(&name.name) {
                    return false;
                }
                ty.size() <= 8
                    && matches!(
                        ty,
                        Type::Bool
                            | Type::Char
                            | Type::SChar
                            | Type::UChar
                            | Type::Char8
                            | Type::Char16
                            | Type::Char32
                            | Type::Short
                            | Type::UShort
                            | Type::Int
                            | Type::UInt
                            | Type::Long
                            | Type::ULong
                            | Type::LongLong
                            | Type::ULongLong
                            | Type::Ptr(_)
                            | Type::Enum(..)
                    )
            },
            _ => false,
        }
    }

    /// 在 callee-saved 池里挑一个驻足号：跳过 `avoid` 与规划器已占用的号。
    ///
    /// 必须是 callee-saved：caller-saved 号是各条求值子路径的固定暂存，驻足到那里
    /// 会被后续操作数的求值踩掉；也必须是规划器台账里没发出去的号，否则本句 asm 的
    /// 某个操作数会与驻足值共用同一寄存器。
    fn alloc_asm_park_reg(&mut self, planner: &AsmRegisterPlanner, avoid: u8) -> Option<u8> {
        for r in [3u8, 12, 14, 15] {
            if r == avoid || planner.is_used_gp(r) {
                continue;
            }
            if self.reg_bank.alloc_temp_named(r) {
                return Some(r);
            }
        }
        None
    }

    /// 延迟装载一个 GP 输入（或 inout 输出的旧值）。理由与策略见 `DeferredGpLoad`。
    fn defer_gp_load(
        &mut self,
        op: &Expr,
        target: u8,
        planner: &mut AsmRegisterPlanner,
        deferred: &mut Vec<DeferredGpLoad>,
    ) {
        if self.asm_gp_load_is_single_insn(op) {
            deferred.push(DeferredGpLoad::Direct {
                target,
                expr: op.clone(),
            });
            return;
        }
        if let Some(scratch) = self.alloc_asm_park_reg(planner, target) {
            planner.exclude_gp(scratch);
            self.prepare_asm_binding(op, scratch, false);
            deferred.push(DeferredGpLoad::FromScratch { target, scratch });
            return;
        }
        // 驻足池耗尽：退回「flush 时直接求值」。此时只剩 %rax 目标最后装载这一条
        // 顺序保障（flush 的排序），与旧行为同风险。
        deferred.push(DeferredGpLoad::Direct {
            target,
            expr: op.clone(),
        });
    }

    /// 从 `base` 指向的内存按 `sz` 零扩展装载到 `dst`（`"+r"(*p)` 的旧值装载）。
    fn load_reg_from_ptr_sized(&mut self, dst: u8, base: u8, sz: usize) {
        let mut rex = 0x40u8;
        if dst >= 8 {
            rex |= 0x04;
        }
        if base >= 8 {
            rex |= 0x01;
        }
        match sz {
            1 => {
                if rex != 0x40 {
                    self.emit_insn(&Insn::raw_bytes(&[rex]));
                }
                self.emit_insn(&Insn::raw_bytes(&[0x0F, 0xB6]));
            },
            2 => {
                self.emit_insn(&Insn::raw_bytes(&[0x66]));
                if rex != 0x40 {
                    self.emit_insn(&Insn::raw_bytes(&[rex]));
                }
                self.emit_insn(&Insn::raw_bytes(&[0x0F, 0xB7]));
            },
            4 => {
                if rex != 0x40 {
                    self.emit_insn(&Insn::raw_bytes(&[rex]));
                }
                self.emit_insn(&Insn::raw_bytes(&[0x8B]));
            },
            _ => {
                self.emit_insn(&Insn::raw_bytes(&[rex | 0x08]));
                self.emit_insn(&Insn::raw_bytes(&[0x8B]));
            },
        }
        self.emit_reg_indirect_modrm(dst, base);
    }

    /// 执行延迟装载。%rax 目标最后：它同时是所有残余求值路径的固定暂存
    /// (`EVAL_BASE`)，放在前面会被紧随其后的求值踩掉。
    fn flush_deferred_gp_loads(&mut self, deferred: Vec<DeferredGpLoad>) {
        let mut rax_loads: Vec<DeferredGpLoad> = Vec::new();
        let mut others: Vec<DeferredGpLoad> = Vec::new();
        for d in deferred {
            if d.target() == Self::EVAL_BASE {
                rax_loads.push(d);
            } else {
                others.push(d);
            }
        }
        for d in others.into_iter().chain(rax_loads) {
            match d {
                DeferredGpLoad::Direct { target, expr } => {
                    self.prepare_asm_binding(&expr, target, false);
                },
                DeferredGpLoad::FromScratch { target, scratch } => {
                    if target != scratch {
                        self.mov(target, scratch);
                    }
                    self.free_callee_saved(scratch);
                },
                DeferredGpLoad::FromPtr { target, base, size } => {
                    self.load_reg_from_ptr_sized(target, base, size);
                },
                DeferredGpLoad::FromAddrSlot { target, slot, size } => {
                    self.load_stack(target, slot);
                    self.load_reg_from_ptr_sized(target, target, size);
                },
                DeferredGpLoad::GlobalAddr { target, name } => {
                    self.emit_global_addr(target, &name);
                },
            }
        }
    }

    /// 该约束是否要求 x86-64 的 legacy 寄存器类（`q` / `Q`）。
    /// 只服务于「规划器返回 None 之后要不要报 impossible constraint」这一个判定，
    /// 不参与分配本身，所以不必与规划器的字母表严格同步。
    fn constraint_demands_legacy_gp(raw: &str) -> bool {
        let digits = ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];
        let core = raw
            .trim_start_matches(['=', '+', '&', '%'])
            .trim_start_matches(digits);
        !core.contains('{') && core.chars().any(|c| c == 'q' || c == 'Q')
    }

    /// 本条 asm 实际用过的 callee-saved 号必须登记回占用真源（P12 Phase 6.1/6.2）。
    ///
    /// GP 池现在含 rbx/r12/r14/r15：操作数多于 9 个 GP 号时，过去会**静默复用同一个号**
    /// （实测十个 `"=r"` 输出得 `s=64`，gcc 是 55：第十个输出落在了第一个的家上）。
    /// 而 callee-saved 拿来做操作数，有两件事必须同时成立：
    ///   (a) 序言那条保存不得被 `patch_callee_saves` 改成 nop，否则本函数毁掉调用者存在
    ///       %rbx 里的值（§7.15 实测过同一族：修前 `caller_holds_rbx` 得 1500，gcc 是 500）；
    ///   (b) 表达式溅出池（`alloc_temp`）不得再把同一个号发出去，否则会盖掉已就位的操作数。
    /// `RegBank::claim` 一次做完这两件（它置的就是那两个位）。
    ///
    /// caller-saved 的号不进 claim：它们本来就不在溅出池里，钉住它们只会让后续每条 asm
    /// 可选的号逐步耗空（规划器已在 `finish` 内按池内/池外分流）。
    ///
    /// 历史路径：旧实现靠调用点在「输出分配完后」「整张表建完后」各调一次
    /// `own_asm_operand_gp`（幂等）来登记。Phase 6.2 之后这件事由
    /// `AsmRegisterPlanner::finish(&mut reg_bank)` 一次性完成，旧入口已删除。

    /// 模板里作为**间接跳转目标**出现的操作数标识（`*%[name]` 的名字或 `*%N` 的编号）。
    ///
    /// 只认 `*%` 形态：它只会在间接跳转/间接寻址里出现（`call *%0`、`jmp *%[x]`），
    /// 无需再解析前一条助记符（`call`/`callq`/`jmp`/`jmpq` 别名各异）。返回集合里
    /// 同时装下名字与编号，调用方按「操作数名或操作数号」匹配。
    fn asm_indirect_target_tokens(asm_str: &str) -> HashSet<String> {
        let mut out = HashSet::new();
        let b = asm_str.as_bytes();
        let mut i = 0usize;
        while i < b.len() {
            if b[i] != b'*' {
                i += 1;
                continue;
            }
            let mut j = i + 1;
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            if j >= b.len() || b[j] != b'%' {
                i += 1;
                continue;
            }
            j += 1;
            // 修饰字母（`%c`、`%k`、`%q`、`%a` 等）
            while j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            if j < b.len() && b[j] == b'[' {
                let start = j + 1;
                let mut k = start;
                while k < b.len() && b[k] != b']' {
                    k += 1;
                }
                if k < b.len() {
                    out.insert(asm_str[start..k].to_string());
                    i = k + 1;
                    continue;
                }
            } else if j < b.len() && b[j].is_ascii_digit() {
                let start = j;
                let mut k = j;
                while k < b.len() && b[k].is_ascii_digit() {
                    k += 1;
                }
                out.insert(asm_str[start..k].to_string());
                i = k;
                continue;
            }
            i = j.max(i + 1);
        }
        out
    }

    /// 模板里该操作数的**每一次**出现都处在可静态化的位置吗（名字与编号两种写法都认）。
    ///
    /// 与 [`Self::asm_indirect_target_tokens`] 分工：那份只收集 `*%N` 目标（间接跳转必须
    /// 静态化才与 GAS 同形）；这份回答另一半问题——**普通** `"m"` 操作数的地址静态可求时，
    /// 能不能直接印 `sym+off(%rip)`。
    ///
    /// 为什么需要它：`-O0` 下 `"m"(全局对象)` 走 `Ident` 分支一直印 `sym(%rip)`，SSA 把
    /// 操作数降级成 `*&sym`（`Deref{AddrOf}`）后却退回 scratch 寄存器
    /// （`lea sym(%rip),%rX; insn (%rX)`）——同一个源程序在两个优化级别得到两种字节形状，
    /// 且 -O2 的 7+3 字节比 GAS 的 6 字节贵（实测 `verw ds(%rip)`：GAS `0f 00 2d`，
    /// 后端 `0f 00 2b`，§5.39-e 的 x87/描述符用例）。
    ///
    /// 仍然拒绝的两种位置：
    ///   * 段前缀（`mov %1,%%gs:%0`）：rip 相对会把段基址加在符号的**链接地址**上，
    ///     语义错（per-cpu 现场）；
    ///   * `*%N` 间接目标：由 `indirect_targets` 单独放行，这里一律不放（避免两套判据）。
    ///
    /// 操作数在模板里一次都没出现（宏展开才用）时返回 false：宁可不改形状。
    fn asm_memory_token_plain(asm_str: &str, name: Option<&str>, idx: usize) -> bool {
        let b = asm_str.as_bytes();
        let mut i = 0usize;
        let mut seen = false;
        while i < b.len() {
            if b[i] != b'%' {
                i += 1;
                continue;
            }
            // `%%` 是转义的字面 `%`（`%%gs` 的那个），不是操作数占位符
            if i + 1 < b.len() && b[i + 1] == b'%' {
                i += 2;
                continue;
            }
            let mut j = i + 1;
            // 修饰字母（`%k0`、`%q1` …）
            while j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            let hit = if j < b.len() && b[j] == b'[' {
                let start = j + 1;
                let mut k = start;
                while k < b.len() && b[k] != b']' {
                    k += 1;
                }
                k < b.len() && name.is_some_and(|n| &asm_str[start..k] == n)
            } else if j < b.len() && b[j].is_ascii_digit() {
                let start = j;
                let mut k = j;
                while k < b.len() && b[k].is_ascii_digit() {
                    k += 1;
                }
                asm_str[start..k].parse::<usize>() == Ok(idx)
            } else {
                false
            };
            if hit {
                seen = true;
                // 占位符之前的第一个非空白字节：`:`（段前缀/标号）与 `*`（间接）都不算 plain。
                let mut p = i;
                while p > 0 && (b[p - 1] == b' ' || b[p - 1] == b'\t') {
                    p -= 1;
                }
                if p > 0 && (b[p - 1] == b':' || b[p - 1] == b'*') {
                    return false;
                }
            }
            i = j.max(i + 1);
        }
        seen
    }

    pub(crate) fn build_asm_operand_map(
        &mut self,
        asm_str: &str,
        outputs: &[Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        clobbers: &[String],
        bindings: &mut Vec<AsmOpBinding>,
    ) -> HashMap<String, String> {
        let mut map = HashMap::new();
        if Self::asm_operand_map_dbg() {
            eprintln!(
                "[DBG-ASM] fn={} outs={} constraints={} names={} ins={}",
                self.current_func_name,
                outputs.iter().map(|e| format!("{}", crate::const_eval::ExprDebug(e))).collect::<Vec<_>>().join(" | "),
                output_constraints.join("|"),
                output_names.iter().map(|n| n.clone().unwrap_or_default()).collect::<Vec<_>>().join("|"),
                inputs.iter().map(|e| format!("{}", crate::const_eval::ExprDebug(e))).collect::<Vec<_>>().join(" | "),
            );
        }
        let mut idx = 0usize;
        // 允许「静态地址直接展开成 `sym+off(%rip)`」的操作数标识：只有作为
        // `call *%N` / `jmp *%N` **目标**的才算（内核 paravirt 的
        // `"m"(pv_ops.op)`，§5.32）。
        //
        // 为什么必须限定：per-cpu 访问的 `"m"` 操作数（`mov %1,%%gs:%0`、
        // `xadd %%q1,%0`）一旦展开成 RIP 相对，`%gs:` 前缀就作用到符号的**链接
        // 地址**上（语义错），且 xadd/mov 的编码分支只认寄存器/内存形态（实测
        // `arch/x86/mm/tlb.c` 报 `unsupported xadd operand pattern`）。
        let indirect_targets = Self::asm_indirect_target_tokens(asm_str);
        // 写回地址槽占用的 sret 水位。槽必须在模板之前分配、写回之后才能还原，
        // 所以把标记留给发射侧（见 `Codegen::asm_pending_sret_mark`）。
        let addr_slot_mark = self.save_sret_watermark();
        // x87 FPU 栈加载需要按 st() 从高到低顺序执行 (fld 推栈,
        // 最后一次 fld 才是 st(0)). 收集所有 x87 operand, 统一在
        // outputs+inputs 都处理完后再排序加载.
        let mut pending_x87: Vec<(u8, &Expr)> = Vec::new();
        // GP 操作数的延迟装载队列（理由与四种形态见 `DeferredGpLoad`）。
        // `deferred_out` 是输出侧的条目：它们在全部输入求值完成后才具体化，
        // 但必须排在输入条目**之前** —— `"0"`/`"2"` 这类匹配输入要用自己的值
        // 覆盖输出预装载的值，顺序反了就是把旧值又装回去。
        let mut deferred: Vec<DeferredGpLoad> = Vec::new();
        let mut deferred_out: Vec<DeferredGpLoad> = Vec::new();
        // GP 输出里需要延迟装载的项：(输出下标, 是否 inout)。
        // 目标寄存器在 early-clobber 重分配之后才能定，所以收尾时按 `slots[i]` 取。
        let mut pending_output_gp: Vec<(usize, bool)> = Vec::new();

        // 内部占位: 每个操作数分配状态
        enum Slot {
            /// 立即数 (模板内联)
            Imm(String),
            /// 内存操作数 (模板直接出现)
            Mem(String),
            /// 寄存器 (GP 或 XMM)
            Reg(u8, &'static str),
        }
        let mut slots: Vec<Slot> = Vec::new();
        let mut meta: Vec<(u8, u8, bool, bool, u8)> = Vec::new(); // (reg, xmm_width, is_output, is_mem, flags_raw)
                                                                  // flags_raw: bit 0 = early_clobber
        // 非 Ident 输出左值的写回地址：(输出下标, 驻足号, sret 帧槽)。
        // 地址在模板之前算好；写回只 store（+ 必要时 load 一次地址），不再求值。
        let mut saved_addrs: Vec<(usize, Option<u8>, Option<i64>)> = Vec::new();

        // 解析 clobber 列表, 把被 clobber 的寄存器加入统一规划器的排除集合。
        let (clobber_gp, clobber_xmm, _x87_count) = Self::parse_clobber_registers(clobbers);
        // 内存操作数的地址暂存号预扫描。
        //
        // `"m"(s->f)` / `"m"(a[i])` / `"m"(*p)` 的地址必须在整条 asm 期间活着，
        // 模板里会展开成 `(%reg)`，所以每个这类操作数都要一个专属的 callee-saved 号。
        // 必须**先于**下面的 `owned_gp` 快照分配：快照里有它，规划器之后就不会把同号
        // 再发给别的寄存器操作数；`alloc_callee_saved` 置的 used_by_self 同时让序言
        // 保存/恢复调用者的值。
        //
        // 旧实现硬编码 `%r12` 却从不登记：不保存调用者的 r12，别的操作数也照样能分到
        // %r12 把地址踩掉；更要紧的是 `Member`/`Index` 走的是 `gen_expr`（右值/取值），
        // `"+m"(v->counter)` 于是把**计数器的值**当地址去 `lock incl`（内核 online_css
        // 实测 r12=0、CR2=0，启动期 triple fault）。
        let mem_needs_scratch = |op: &Expr, raw: &str| {
            let (_o, is_inout, _e, _m) = Self::parse_asm_constraint_flags(raw);
            Self::asm_constraint_prefers_memory(raw, is_inout)
                && !matches!(op, Expr::Ident { .. })
        };
        // 固定寄存器约束（`a/b/c/d/S/D` 与 `{eax}` 这类显式寄存器）必须**先于**任何
        // 分配成集：除了下面给规划器 `reserve_gp`，`"m"` 操作数的地址 scratch 也
        // 要避开它们。旧序里 scratch 先分配、固定约束后预留，于是地址挑中 `"b"`
        // 要钉死的 `%rbx`：`mov ptr,%rbx` 后紧接 `mov n_lo,%rbx`，内存操作数在
        // n_lo（内核里是 0）上寻址 —— §5.20 的 `cmpxchg16b (%rbx)` 读地址 0。
        // 号码与输入/输出侧一次性算完，下面建规划器与 scratch 两处共用同一张表。
        let fixed_regs: Vec<(u8, bool)> = output_constraints
            .iter()
            .map(|c| (c, true))
            .chain(input_constraints.iter().map(|c| (c, false)))
            .filter_map(|(c, is_out)| Self::fixed_gp_of_constraint(c).map(|r| (r, is_out)))
            .collect();
        let fixed_gp: HashSet<u8> = fixed_regs.iter().map(|&(r, _)| r).collect();
        let mut mem_scratch: Vec<Option<(u8, bool)>> =
            Vec::with_capacity(outputs.len() + inputs.len());
        for (i, op) in outputs.iter().enumerate() {
            let raw = output_constraints.get(i).map(String::as_str).unwrap_or("=r");
            mem_scratch.push(if mem_needs_scratch(op, raw) {
                // 第二项 = 是否"借号"（池内四个号全被活值占着时压栈保存后借用）。
                Some(self.alloc_asm_mem_scratch(&clobber_gp, &fixed_gp))
            } else {
                None
            });
        }
        for (i, op) in inputs.iter().enumerate() {
            let raw = input_constraints.get(i).map(String::as_str).unwrap_or("r");
            mem_scratch.push(if mem_needs_scratch(op, raw) {
                Some(self.alloc_asm_mem_scratch(&clobber_gp, &fixed_gp))
            } else {
                None
            });
        }
        // 寄存器所有权只有一个真源：`RegBank`。构造时一次性把「本条 asm 之外已有主人」
        // 的 GP 号（函数级 pinned + 溅出池正占的号）快照进规划器的排除集合；
        // 规划期不再持有 `&mut RegBank`，因此操作数求值（`prepare_asm_binding` 等
        // `&mut self` 方法）可以与规划器同时活跃。规划结束后由 `planner.finish(&mut reg_bank)`
        // 一次性把 callee-saved 号 claim 回占用真源。
        //
        // §5.42：帧头只为**计划内**的 callee-saved 号留保存槽（``cs_slots =
        // callee_plan.count_ones()``），所以计划外的池号一律不得发给 asm —— 发了就是
        // 「序言没保存、asm 却毁掉它」。把它们并进排除集（不是 clobber 集：asm 结束
        // 后它们仍属于调用方）。固定约束（`"b"`/`{r12}`）的号已由
        // `compute_callee_plan` 收进计划，不会出现在这里。
        let mut owned_gp: Vec<u8> = self.reg_bank.owned_gp();
        for &pool_reg in &[3u8, 12, 14, 15] {
            if !self.reg_bank.is_in_plan(pool_reg) {
                owned_gp.push(pool_reg);
            }
        }
        let mut planner = AsmRegisterPlanner::new(&owned_gp, &clobber_gp, &clobber_xmm);

        // 预扫描固定寄存器约束：先于任何通用分配预留，避免前面的 `"r"` 操作数抢占
        // 后面固定约束要的号（同一句 asm 两个操作数共享一个号 = 静默错值）。
        // 号码表已在 scratch 分配前算好（`fixed_regs`），与那边同一真源。
        for &(reg, is_out) in &fixed_regs {
            planner.reserve_gp(
                reg,
                if is_out {
                    AsmRegUse::Output
                } else {
                    AsmRegUse::Input
                },
            );
        }

        // ===== 处理 outputs =====
        for (op_idx, op) in outputs.iter().enumerate() {
            let raw = output_constraints
                .get(op_idx)
                .map(String::as_str)
                .unwrap_or("=r");
            let (is_output, is_inout, early, match_digit) = Self::parse_asm_constraint_flags(raw);
            let letter = Self::asm_pick_sub_constraint(raw, is_inout);
            let _ = is_output; // 已经在 flags 派生

            // match-digit: 复用先前操作数 (仅当 N < 当前全局 idx)
            if let Some(n) = match_digit {
                if (n as usize) < slots.len() {
                    let slot = &slots[n as usize];
                    // `meta` 与 `slots` 必须同长：它按**下标**就是操作数号（写回、
                    // early-clobber 重分配、`bindings` 与下面的碰撞检查都按下标取）。
                    // 旧写法只在 `Slot::Reg` 臂 push，`"0"`/`"1"` 匹配到一个立即数/内存
                    // 操作数时后面每个操作数的 meta 条目整体前移一位 —— 与 §5.11 同一族
                    // 的静默错位，只是发生在输出侧。
                    match slot {
                        Slot::Imm(s) => {
                            map.insert(format!("%{idx}"), s.clone());
                            meta.push((0, 0, true, false, 0));
                        },
                        Slot::Mem(s) => {
                            map.insert(format!("%{idx}"), s.clone());
                            meta.push((0, 0, true, true, 0));
                        },
                        Slot::Reg(r, name) => {
                            let nreg = *r;
                            map.insert(format!("%{idx}"), (*name).to_string());
                            planner.reserve_gp(nreg, AsmRegUse::Output);
                            meta.push((nreg, 0, true, false, 0));
                        },
                    }
                    slots.push(match slot {
                        Slot::Imm(s) => Slot::Imm(s.clone()),
                        Slot::Mem(s) => Slot::Mem(s.clone()),
                        Slot::Reg(r, n) => Slot::Reg(*r, n),
                    });
                    idx += 1;
                    continue;
                }
            }

            // GCC 条件码约束 (@ccz/@ccs/@ccc 等): 不可见操作数, 不在模板中替换
            // 写回时通过 setcc 指令捕获条件标志
            if let Some(cc_letters) = Self::asm_parse_cc_constraint(raw) {
                if let Some(setcc_op) = Self::cc_letter_to_setcc_opcode(cc_letters) {
                    slots.push(Slot::Imm(String::new())); // 占位
                    meta.push((setcc_op, 204, true, false, if early { 1 } else { 0 }));
                    idx += 1;
                    continue;
                }
            }

            // 显式寄存器约束 ({eax}/{cx}/{cl}/{gs}/{fs} 等)
            if let Some(eri) = Self::asm_parse_explicit_reg(raw) {
                let name = eri.att_name;
                match eri.kind {
                    ExplicitRegKind::Seg => {
                        // 段寄存器输出: 模板里用 %gs/%fs 等
                        // 写回时通过 mov %seg, [r13+off] 存储
                        if is_inout {
                            self.prepare_asm_seg_binding(op, eri.reg);
                        }
                        map.insert(format!("%{idx}"), name.to_string());
                        slots.push(Slot::Reg(eri.reg, name));
                        meta.push((eri.reg, 203, true, false, if early { 1 } else { 0 }));
                        idx += 1;
                        continue;
                    },
                    ExplicitRegKind::Gp => {
                        // 显式 GP 子寄存器: 模板里用 %eax/%cx/%cl 等
                        let xmm_w = match eri.size {
                            8 => 205u8,
                            16 => 206,
                            32 => 207,
                            64 => 208,
                            _ => 0,
                        };
                        // +r (读写): 加载变量值; =r (只写): 无需预装载。
                        // 装载一律延迟到全部操作数求值之后（见 `DeferredGpLoad`）。
                        pending_output_gp.push((op_idx, is_inout));
                        planner.reserve_gp(eri.reg, AsmRegUse::Output);
                        map.insert(format!("%{idx}"), name.to_string());
                        slots.push(Slot::Reg(eri.reg, name));
                        meta.push((eri.reg, xmm_w, true, false, if early { 1 } else { 0 }));
                        idx += 1;
                        continue;
                    },
                }
            }

            // x87 FPU 约束 (f/t/u)
            if let Some(st_idx) = Self::asm_constraint_wants_x87(raw) {
                let xmm_w = if st_idx == 0 { 200u8 } else { 201 };
                let name: &'static str = if st_idx == 0 { "st" } else { "%st(1)" };
                if is_inout {
                    pending_x87.push((st_idx, op));
                }
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(st_idx, name));
                meta.push((st_idx, xmm_w, true, false, if early { 1 } else { 0 }));
                idx += 1;
                continue;
            }

            // MMX 约束 (y)
            if Self::asm_constraint_wants_mmx(raw) {
                let mi = planner.alloc_mmx(idx);
                let name = Self::asm_mmx_name(mi as usize);
                if is_inout {
                    self.prepare_asm_mmx_binding(op, mi);
                }
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(mi, name));
                meta.push((mi, 202, true, false, if early { 1 } else { 0 }));
                idx += 1;
                continue;
            }

            // 立即数约束且操作数可常量求值
            if matches!(letter, "i") && Self::asm_constraint_is_immediate(raw) {
                if let Some(val) = self.eval_asm_operand(op) {
                    // 输出侧走到这里说明 `asm_pick_sub_constraint` 选的就是 `i`
                    // （约束里没有 `r`/`m`/`g` 备选），越界在 gcc 里是
                    // `impossible constraint in 'asm'`，不能默默截成 imm8。
                    if !crate::codegen::shared::asm_constraints::immediate_text_fits(raw, &val) {
                        self.asm_reject(&format!(
                            "impossible constraint in 'asm': immediate {val} out of range for \
                             constraint \"{raw}\" (operand {idx}, in function {})",
                            if self.current_func_name.is_empty() {
                                "<unknown>"
                            } else {
                                &self.current_func_name
                            }
                        ));
                    }
                    // 字符串字面量立即数: eval_asm_operand 返回 .L.str.const.<hash> label,
                    // 但 codegen 之前没把这个字符串落进 string_pool (g_expr_literal 才会
                    // 落池; 内联汇编里 "%c" 把 label 直接嵌入自定义节, 不经 gen_expr).
                    // 这里补落一份 rodata, 保证自定义节里对 .L.str.const.N 的引用能
                    // 反向定位到 rodata 偏移 (resolve.rs lookup_str_const_label_offset).
                    if let Expr::String { data, .. } = op {
                        if !self.string_pool.contains_key(data) {
                            let o = self.rodata.len() as u64;
                            self.rodata.extend_from_slice(data);
                            self.string_pool.insert(data.clone(), o);
                        }
                    }
                    let att_val = if val.starts_with('$') {
                        val.clone()
                    } else if val.starts_with('-')
                        || val.chars().next().is_some_and(|c| c.is_ascii_digit())
                    {
                        format!("${val}")
                    } else {
                        val.clone()
                    };
                    map.insert(format!("%{idx}"), att_val.clone());
                    slots.push(Slot::Imm(att_val));
                    meta.push((0, 0, true, false, 0));
                    idx += 1;
                    continue;
                }
            }

            // 内存操作数 (m 优先)
            if Self::asm_constraint_prefers_memory(raw, is_inout) {
                let scratch = mem_scratch
                    .get(idx)
                    .copied()
                    .flatten()
                    .map(|(r, _)| r)
                    .unwrap_or(12);
                let op_name = output_names.get(op_idx).and_then(|n| n.as_deref());
                let allow_static = op_name.is_some_and(|n| indirect_targets.contains(n))
                    || indirect_targets.contains(&idx.to_string())
                    || Self::asm_memory_token_plain(asm_str, op_name, idx);
                if let Some(mem) = self.asm_memory_operand_str(op, scratch, allow_static) {
                    map.insert(format!("%{idx}"), mem.clone());
                    slots.push(Slot::Mem(mem));
                    meta.push((0, 0, true, true, if early { 1 } else { 0 }));
                    idx += 1;
                    continue;
                }
            }

            // XMM 寄存器约束
            if let Some(xmm_bits) = Self::asm_constraint_wants_xmm(raw) {
                if !Self::asm_type_allows_xmm(&op.ty()) {
                    self.asm_reject(
                        "inconsistent operand constraints in an 'asm': long double needs an x87 constraint (t/u/f)",
                    );
                    map.insert(format!("%{idx}"), String::new());
                    slots.push(Slot::Reg(0, "st"));
                    meta.push((0, 200, true, false, 0));
                    idx += 1;
                    continue;
                }
                let xi = planner.alloc_xmm(idx, AsmRegUse::Output);
                let name = Self::asm_xmm_name(xi as usize);
                if is_inout {
                    self.prepare_asm_xmm_binding(op, xi, true);
                }
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(xi, name));
                meta.push((xi, xmm_bits, true, false, if early { 1 } else { 0 }));
                idx += 1;
                continue;
            }

            // 固定 GP 字母约束 (a/b/c/d/S/D/q/Q)
            if let Some((chosen, _fixed)) =
                planner.alloc_gp_for_constraint(raw, idx, AsmRegUse::Output)
            {
                let name = self.asm_reg_name_or_report(chosen);
                pending_output_gp.push((op_idx, is_inout));
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(chosen, name));
                meta.push((chosen, 0, true, false, if early { 1 } else { 0 }));
                idx += 1;
                continue;
            } else if Self::constraint_demands_legacy_gp(raw) {
                // 与输入侧同一条判据：legacy 类耗尽时报 impossible constraint，
                // 不静默换成别的寄存器（那会丢写回，实测第 7 个输出全是 0）。
                self.asm_reject("impossible constraint: 'q' has no free legacy register");
            }

            // 内存地址 (p): lea 到一个 GP 寄存器, 模板里就是 %reg
            if Self::asm_constraint_is_address(raw) {
                let chosen = planner.alloc_gp(idx, AsmRegUse::Output);
                pending_output_gp.push((op_idx, is_inout));
                let name = self.asm_reg_name_or_report(chosen);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(chosen, name));
                meta.push((chosen, 0, true, false, if early { 1 } else { 0 }));
                idx += 1;
                continue;
            }

            // 默认: GP 寄存器
            // `register T v asm("rN")` 的变量：约束只是把它的「住处」写进操作数表，
            // 另分配一个号既读不到值（值在那个固定号里），也白吃一个号。
            // 见 `pinned_gp_of_operand`。
            let chosen = match self.pinned_gp_of_operand(op) {
                Some(reg) => {
                    planner.reserve_gp(reg, AsmRegUse::Output);
                    reg
                },
                None => planner.alloc_gp(idx, AsmRegUse::Output),
            };
            let name = self.asm_reg_name_or_report(chosen);
            // 非 Ident 输出（含 `+r`）的写回地址统一在 early-clobber 重分配之后、
            // 模板之前由下面的预计算段算好（驻足号或 sret 帧槽）。
            map.insert(format!("%{idx}"), name.to_string());
            slots.push(Slot::Reg(chosen, name));
            meta.push((chosen, 0, true, false, if early { 1 } else { 0 }));
            pending_output_gp.push((op_idx, is_inout));
            idx += 1;
        }

        // ===== 处理 inputs =====
        for (op_idx, op) in inputs.iter().enumerate() {
            let raw = input_constraints
                .get(op_idx)
                .map(String::as_str)
                .unwrap_or("r");
            let (_is_output, is_inout, _early, match_digit) = Self::parse_asm_constraint_flags(raw);
            let _letter = Self::asm_pick_sub_constraint(raw, is_inout);

            // match-digit: 复用先前操作数的寄存器, 但仍需 prepare_asm_binding
            // 把当前操作数 (作为输入) 的值加载到该寄存器, 不能只复制 slot.
            // 例如 "=r"(b) : "0"(a): 输出准备把 &b 加载到 rax, 但输入 a 应当把
            // a 的值 (42) 重新加载到 rax, 覆盖 &b, 这样 asm "add $1, %0" 才能
            // 在 rax 上做正确算术 (rax = 42 + 1 = 43), 再写回 b.
            if let Some(n) = match_digit {
                if (n as usize) < slots.len() {
                    let slot = &slots[n as usize];
                    if let Slot::Reg(r, name) = slot {
                        let nreg = *r;
                        let nname = *name;
                        let mwidth = meta.get(n as usize).map(|m| m.1).unwrap_or(0);
                        let is_xmm = mwidth != 0 && mwidth < 200;
                        let is_x87 = mwidth == 200 || mwidth == 201;
                        if is_x87 {
                            // x87 栈寄存器复用: defer fld, 等所有 operand 处理
                            // 完后再按 st() 从高到低统一加载.
                            pending_x87.push((nreg, op));
                        } else if is_xmm {
                            self.prepare_asm_xmm_binding(op, nreg, is_inout);
                        } else {
                            // 对当前操作数 (输入) 调 prepare 把值加载到复用寄存器；
                            // 装载延迟到全部操作数求值之后（见 `DeferredGpLoad`）。
                            self.defer_gp_load(op, nreg, &mut planner, &mut deferred);
                        }
                        map.insert(format!("%{idx}"), nname.to_string());
                        if is_xmm {
                            planner.reserve_xmm(nreg, AsmRegUse::Input);
                        } else if !is_x87 {
                            planner.reserve_gp(nreg, AsmRegUse::Input);
                        }
                        meta.push((nreg, mwidth, false, false, 0));
                        slots.push(Slot::Reg(nreg, nname));
                        idx += 1;
                        continue;
                    }
                    // 非 Reg slot (Imm/Mem): 直接复制即可（`meta` 同样必须同长，
                    // 理由见输出侧的 match-digit 臂）
                    match slot {
                        Slot::Imm(s) => {
                            map.insert(format!("%{idx}"), s.clone());
                            meta.push((0, 0, false, false, 0));
                        },
                        Slot::Mem(s) => {
                            map.insert(format!("%{idx}"), s.clone());
                            meta.push((0, 0, false, true, 0));
                        },
                        Slot::Reg(_, _) => unreachable!(),
                    }
                    slots.push(match slot {
                        Slot::Imm(s) => Slot::Imm(s.clone()),
                        Slot::Mem(s) => Slot::Mem(s.clone()),
                        Slot::Reg(r, n) => Slot::Reg(*r, n),
                    });
                    idx += 1;
                    continue;
                }
            }

            // 显式寄存器约束 ({eax}/{cx}/{cl}/{gs}/{fs} 等)
            if let Some(eri) = Self::asm_parse_explicit_reg(raw) {
                let name = eri.att_name;
                match eri.kind {
                    ExplicitRegKind::Seg => {
                        self.prepare_asm_seg_binding(op, eri.reg);
                        map.insert(format!("%{idx}"), name.to_string());
                        slots.push(Slot::Reg(eri.reg, name));
                        meta.push((eri.reg, 203, false, false, 0));
                        idx += 1;
                        continue;
                    },
                    ExplicitRegKind::Gp => {
                        let xmm_w = match eri.size {
                            8 => 205u8,
                            16 => 206,
                            32 => 207,
                            64 => 208,
                            _ => 0,
                        };
                        self.defer_gp_load(op, eri.reg, &mut planner, &mut deferred);
                        map.insert(format!("%{idx}"), name.to_string());
                        slots.push(Slot::Reg(eri.reg, name));
                        planner.reserve_gp(eri.reg, AsmRegUse::Input);
                        meta.push((eri.reg, xmm_w, false, false, 0));
                        idx += 1;
                        continue;
                    },
                }
            }

            // x87 FPU 约束 (f/t/u) — 延迟加载, 等全部 operand 处理完
            // 后按 st() 从高到低顺序统一 fld.
            if let Some(st_idx) = Self::asm_constraint_wants_x87(raw) {
                let xmm_w = if st_idx == 0 { 200u8 } else { 201 };
                let name: &'static str = if st_idx == 0 { "st" } else { "%st(1)" };
                pending_x87.push((st_idx, op));
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(st_idx, name));
                meta.push((st_idx, xmm_w, false, false, 0));
                idx += 1;
                continue;
            }

            // MMX 约束 (y)
            if Self::asm_constraint_wants_mmx(raw) {
                let mi = planner.alloc_mmx(idx);
                let name = Self::asm_mmx_name(mi as usize);
                self.prepare_asm_mmx_binding(op, mi);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(mi, name));
                meta.push((mi, 202, false, false, 0));
                idx += 1;
                continue;
            }

            // 立即数约束且操作数可常量求值
            if Self::asm_constraint_is_immediate(raw) {
                if let Some(val) = self.eval_asm_operand(op) {
                    // 立即数**备选**要走一遍字母的取值范围：`"Ir"` 的 `I` 只接 0..31，
                    // 超范围时 gcc 会改用备选 `r`（寄存器），而 CPU 的
                    // `bts/btr mem,reg` 才会按 `nr/64*8` 调整地址。旧写法无视范围
                    // 当 `i` 代成 `$100`，而 `btsq $100,(mem)` 由 CPU 按
                    // `imm & 63` 取位、不做地址调整 —— 内核 `init_rt_rq` 的
                    // delimiter 位（MAX_RT_PRIO=100）就这样落在 bit 36。
                    if crate::codegen::shared::asm_constraints::immediate_text_fits(raw, &val) {
                        if let Expr::String { data, .. } = op {
                            if !self.string_pool.contains_key(data) {
                                let o = self.rodata.len() as u64;
                                self.rodata.extend_from_slice(data);
                                self.string_pool.insert(data.clone(), o);
                            }
                        }
                        let att_val = if val.starts_with('$') {
                            val.clone()
                        } else if val.starts_with('-')
                            || val.chars().next().is_some_and(|c| c.is_ascii_digit())
                        {
                            format!("${val}")
                        } else {
                            val.clone()
                        };
                        map.insert(format!("%{idx}"), att_val.clone());
                        slots.push(Slot::Imm(att_val));
                        meta.push((0, 0, false, false, 0));
                        idx += 1;
                        continue;
                    }
                    // 值超出受限字母的范围：只有「全立即数」约束才是 gcc 的
                    // impossible constraint；带 `r`/`m` 备选的落到下面的寄存器分支，
                    // 与 gcc 的备选选择一致（这正是 `"Ir"(100)` 该走的路）。
                    if Self::asm_constraint_is_immediate_only(raw) {
                        self.asm_reject(&format!(
                            "impossible constraint in 'asm': immediate {val} out of range for \
                             constraint \"{raw}\" (operand {idx}, in function {})",
                            if self.current_func_name.is_empty() {
                                "<unknown>"
                            } else {
                                &self.current_func_name
                            }
                        ));
                        map.insert(format!("%{idx}"), String::new());
                        slots.push(Slot::Imm(String::new()));
                        meta.push((0, 0, false, false, 0));
                        idx += 1;
                        continue;
                    }
                }
                // 走到这里：约束只能接立即数，操作数却不是编译期常量（内联没能把实参传进来）。
                // GCC 对这一形状直接报 `impossible constraint in 'asm'`；往下掉去分配寄存器
                // 则把 `%c[feature]` 换成裸寄存器名，数据伪指令 `.4byte rax` 就当符号引用发
                // 一条解不开的重定位（实测 `vmlinux.o` 的 `.altinstructions` 特性字段上 98 条
                // `→ rax`，objtool 报 `special: can't find orig instruction`）。
                //
                // 硬失败而不只报警：当前要求内核全树由 dcc 编出，不允许拿 gcc 兼掉，所以这里
                // 必须把「dcc 还缺什么」当场钉在编译步骤上（也让全树一轮就能把待补站点列齐），
                // 而不是交出一个能编过但值错的产物。先决条件是 `try_stmt_inline_call` 能
                // 展开带 asm 的 `__always_inline` 体（包含 void 体与表达式位置）。
                if Self::asm_constraint_is_immediate_only(raw) {
                    self.warn_asm_immediate_only_operand_runtime(raw, idx);
                    // 把「到底是哪棵表达式没能折成常量」直接写在诊断里：全树跑一轮就能
                    // 把待补的常量求值形态列齐，不必一个个 TU 去回放猜。
                    self.asm_reject(&format!(
                        "impossible constraint in 'asm': operand {idx} with immediate-only \
                         constraint \"{raw}\" has no compile-time constant value \
                         (expression: {}, in function {})",
                        crate::const_eval::ExprDebug(op),
                        if self.current_func_name.is_empty() {
                            "<unknown>"
                        } else {
                            &self.current_func_name
                        }
                    ));
                    map.insert(format!("%{idx}"), String::new());
                    slots.push(Slot::Imm(String::new()));
                    meta.push((0, 0, false, false, 0));
                    idx += 1;
                    continue;
                }
            }

            // 内存操作数
            if Self::asm_constraint_prefers_memory(raw, is_inout) {
                let scratch = mem_scratch
                    .get(idx)
                    .copied()
                    .flatten()
                    .map(|(r, _)| r)
                    .unwrap_or(12);
                let op_name = input_names.get(op_idx).and_then(|n| n.as_deref());
                let allow_static = op_name.is_some_and(|n| indirect_targets.contains(n))
                    || indirect_targets.contains(&idx.to_string())
                    || Self::asm_memory_token_plain(asm_str, op_name, idx);
                if let Some(mem) = self.asm_memory_operand_str(op, scratch, allow_static) {
                    map.insert(format!("%{idx}"), mem.clone());
                    slots.push(Slot::Mem(mem));
                    meta.push((0, 0, false, true, 0));
                    idx += 1;
                    continue;
                }
            }

            // XMM
            if let Some(xmm_bits) = Self::asm_constraint_wants_xmm(raw) {
                if !Self::asm_type_allows_xmm(&op.ty()) {
                    self.asm_reject(
                        "inconsistent operand constraints in an 'asm': long double needs an x87 constraint (t/u/f)",
                    );
                    map.insert(format!("%{idx}"), String::new());
                    slots.push(Slot::Reg(0, "st"));
                    meta.push((0, 200, false, false, 0));
                    idx += 1;
                    continue;
                }
                let xi = planner.alloc_xmm(idx, AsmRegUse::Input);
                let name = Self::asm_xmm_name(xi as usize);
                if matches!(op.ty(), Type::Float | Type::Double | Type::LongDouble) {
                    self.prepare_asm_xmm_binding(op, xi, is_inout);
                } else {
                    // W0005 兼容路径：约束要求 XMM 但表达式是整数/指针。
                    // 为保持 warning-only 语义，按整数位型加载到 GP 后
                    // movq/movd 进 XMM，避免把 GP 当 XMM 直接使用而崩溃。
                    self.prepare_asm_binding(op, 0, false);
                    if op.ty().size() <= 4 {
                        self.emit_insn(&Insn::raw(&[0x66]));
                        if xi >= 8 {
                            self.emit_insn(&Insn::raw(&[0x44]));
                        }
                        self.emit_insn(&Insn::raw(&[0x0F, 0x6E, 0xC0 | ((xi & 7) << 3)]));
                    } else {
                        self.emit_insn(&Insn::raw(&[0x66]));
                        let rex = 0x48 | if xi >= 8 { 0x44 } else { 0 };
                        self.emit_insn(&Insn::raw(&[rex, 0x0F, 0x6E, 0xC0 | ((xi & 7) << 3)]));
                    }
                }
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(xi, name));
                meta.push((xi, xmm_bits, false, false, 0));
                idx += 1;
                continue;
            }

            if matches!(op.ty(), Type::Float | Type::Double | Type::LongDouble) {
                // 浮点值绑到 GP 约束 (如 "r"(1.5F)): 兼容路径.
                // GCC 允许此类绑定 (仅告警), 将浮点位型加载到 GP 寄存器.
                // 920520-1.c: asm("%0"::"r"(1.5F)) 期望 %0 展开为 GP 寄存器名.
                if Self::asm_constraint_is_gp(raw) {
                    // 将浮点位型加载到 GP 寄存器 (而非 XMM).
                    // 常量浮点值: 直接 mov 位型; 非变量: 从内存加载位型.
                    let chosen = planner.alloc_gp(idx, AsmRegUse::Input);
                    let name = self.asm_reg_name_or_report(chosen);
                    // 尝试直接求值浮点常量位型
                    if let Expr::Float { c, ty, .. } = op {
                        // 浮点常量的位型加载到 GP 寄存器。
                        // long double 无法整体放入 GP 寄存器，此处为 GCC
                        // 兼容降级路径：显式转 f64 取其 64 位位型。
                        let bits: u64 = if matches!(ty, Type::Float) {
                            // float: 32 位
                            (c.as_f64() as f32).to_bits() as u64
                        } else {
                            // double / long double 降级: 64 位
                            c.as_f64().to_bits()
                        };
                        self.mov_imm(chosen, bits as i128);
                    } else if let Expr::Ident {
                        name: vname,
                        ty: vty,
                        ..
                    } = op
                    {
                        // 变量: 从内存加载位型 (按类型大小)
                        let off = self.var_offset(&vname.name);
                        let sz = vty.size();
                        if sz <= 4 {
                            self.load_stack_sized(chosen, off, sz);
                        } else {
                            self.gen_expr(op, chosen);
                        }
                    } else {
                        // 其他表达式: 走 gen_expr (可能加载到 xmm0, 再 mov 到 gp)
                        // 对非常量表达式, 从临时区加载位型
                        self.gen_expr(op, Self::EVAL_BASE);
                        // xmm0 → GP (按类型大小复制位型)
                        if matches!(op.ty(), Type::Float) {
                            self.emit_insn(&Insn::raw(&[
                                0x66,
                                0x0F,
                                0x7E,
                                0xC0 | ((chosen & 7) << 3) | (chosen & 7),
                            ]));
                        } else {
                            self.emit_insn(&Insn::raw(&[
                                0x66,
                                0x48,
                                0x0F,
                                0x7E,
                                0xC0 | ((chosen & 7) << 3) | (chosen & 7),
                            ]));
                        }
                    }
                    map.insert(format!("%{idx}"), name.to_string());
                    slots.push(Slot::Reg(chosen, name));
                    meta.push((chosen, 0, false, false, 0));
                    idx += 1;
                    continue;
                } else {
                    let xi = planner.alloc_xmm(idx, AsmRegUse::Input);
                    let name = Self::asm_xmm_name(xi as usize);
                    self.prepare_asm_xmm_binding(op, xi, is_inout);
                    map.insert(format!("%{idx}"), name.to_string());
                    slots.push(Slot::Reg(xi, name));
                    meta.push((
                        xi,
                        if matches!(op.ty(), Type::Float) { 32 } else { 64 },
                        false,
                        false,
                        0,
                    ));
                    idx += 1;
                    continue;
                }
            }

            // 固定 GP 字母约束 (a/b/c/d/S/D/q/Q)
            if let Some((chosen, _fixed)) =
                planner.alloc_gp_for_constraint(raw, idx, AsmRegUse::Input)
            {
                let name = self.asm_reg_name_or_report(chosen);
                self.defer_gp_load(op, chosen, &mut planner, &mut deferred);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(chosen, name));
                meta.push((chosen, 0, false, false, 0));
                idx += 1;
                continue;
            } else if Self::constraint_demands_legacy_gp(raw) {
                // legacy 类（rax/rbx/rcx/rdx/rsi/rdi）里已经没有空闲号 —— 这就是约束
                // 满足不了。绝不退成「换一个别的寄存器」：实测那样会丢最后一个操作数
                // 的写回，只是换一种错值。报 impossible constraint，出路是既有的
                // --asm-fallback=gas（整 TU 交给 GAS 编码）。
                self.asm_reject("impossible constraint: 'q' has no free legacy register");
            }

            // 内存地址 (p)
            if Self::asm_constraint_is_address(raw) {
                let chosen = planner.alloc_gp(idx, AsmRegUse::Input);
                self.defer_gp_load(op, chosen, &mut planner, &mut deferred);
                let name = self.asm_reg_name_or_report(chosen);
                map.insert(format!("%{idx}"), name.to_string());
                slots.push(Slot::Reg(chosen, name));
                meta.push((chosen, 0, false, false, 0));
                idx += 1;
                continue;
            }

            // GP 寄存器
            let chosen = match self.pinned_gp_of_operand(op) {
                Some(reg) => {
                    planner.reserve_gp(reg, AsmRegUse::Input);
                    reg
                },
                None => planner.alloc_gp(idx, AsmRegUse::Input),
            };
            let name = self.asm_reg_name_or_report(chosen);
            self.defer_gp_load(op, chosen, &mut planner, &mut deferred);
            map.insert(format!("%{idx}"), name.to_string());
            slots.push(Slot::Reg(chosen, name));
            meta.push((chosen, 0, false, false, 0));
            idx += 1;
        }

        // ===== early-clobber: 输出寄存器不能与任何输入寄存器重叠 =====
        let num_outputs = outputs.len();
        for i in 0..num_outputs {
            let (reg, xmm_w, is_out, is_mem, flags) = meta[i];
            if !is_out || is_mem || (flags & 1) == 0 {
                continue;
            }
            // 跳过非 GP/XMM 类型 (x87, MMX, 段寄存器, 条件码, 显式子寄存器)
            // 这些类型不参与 GP/XMM 寄存器池的 early-clobber 重分配
            if xmm_w >= 200 {
                continue;
            }
            if xmm_w != 0 {
                // XMM 输出 + early-clobber: 找一个未被 XMM 输入占用的 XMM
                if !planner.input_xmm_contains(reg) {
                    continue;
                }
                if let Some(cand) = planner.reassign_early_xmm(reg, i) {
                    let new_name = Self::asm_xmm_name(cand as usize);
                    let key = format!("%{i}");
                    if let Some(op) = outputs.get(i) {
                        let raw = output_constraints
                            .get(i)
                            .map(String::as_str)
                            .unwrap_or("=r");
                        let (_, is_inout, _, _) = Self::parse_asm_constraint_flags(raw);
                        if is_inout {
                            self.prepare_asm_xmm_binding(op, cand, true);
                        }
                    }
                    map.insert(key, new_name.to_string());
                    slots[i] = Slot::Reg(cand, new_name);
                    meta[i] = (cand, xmm_w, true, false, flags);
                }
                continue;
            }
            if !planner.input_gp_contains(reg) {
                continue;
            }
            // 找一个不冲突的 GP
            if let Some(cand) = planner.reassign_early_gp(reg, i) {
                let new_name = self.asm_reg_name_or_report(cand);
                let key = format!("%{i}");
                // 预装载已经登记在 `pending_output_gp` 里，收尾时按 `slots[i]`
                // 取新号发射；这里不再重新求值（否则 `+r"(*bar())` 会把 bar() 调两次）。
                map.insert(key, new_name.to_string());
                slots[i] = Slot::Reg(cand, new_name);
                meta[i] = (cand, 0, true, false, flags);
            }
        }

        // ===== 非 Ident 输出的写回地址：模板之前算好 =====
        //
        // 写回过去在 asm 之后才 `gen_lvalue_addr`，那时有两个致命前提：
        //   (a) `alloc_callee_saved` 还能发出一个号 —— 实测 `native_cpuid`
        //       （`"=a"(*eax)/"=b"(*ebx)/"=c"(*ecx)/"=d"(*edx)`）里 r12/r14/r15 是
        //       变量的家、rbx 又被本句 `"=b"` 钉住 ⇒ 返回 None ⇒ 四个输出全部被静默丢弃，
        //       `regs[]` 永远是旧值；
        //   (b) 重算地址时 %rax/%rcx/%rdx 可以自由当暂存 —— 可它们正住着还没写回的输出。
        // 所以地址必须在模板之前定下来：优先停到 callee-saved 驻足号（整条 asm 期间持有），
        // 驻足池满则落一个 sret 帧槽（写回时只需一次 `load_stack`）。
        if !self.current_func_name.is_empty() {
            for i in 0..num_outputs {
                let (reg, xmm_w, is_out, is_mem, _flags) = meta[i];
                if !is_out || is_mem || !matches!(xmm_w, 0 | 32 | 64 | 128) {
                    continue;
                }
                let Some(op) = outputs.get(i) else {
                    continue;
                };
                if matches!(op, Expr::Ident { .. }) {
                    continue;
                }
                let avoid = if xmm_w == 0 { reg } else { u8::MAX };
                if let Some(park) = self.alloc_asm_park_reg(&planner, avoid) {
                    if self.gen_lvalue_addr(op, park) {
                        planner.exclude_gp(park);
                        saved_addrs.push((i, Some(park), None));
                        continue;
                    }
                    self.free_callee_saved(park);
                }
                let off = self.alloc_sret_slot(8);
                if self.gen_lvalue_addr(op, Self::EVAL_BASE) {
                    self.store_stack(Self::EVAL_BASE, off);
                    saved_addrs.push((i, None, Some(off)));
                }
            }
        }

        // ===== fail-closed：两个输出不得被绑到同一块存储 =====
        //
        // §5.11 的现场：命名 `"+m"(*v)` 与模板里不可见的 `"=@ccz"(c)` 解析到同一个
        // `-64(%r13)` —— 比较交换打到了条件码变量自己的帧槽上，`v` 永不写回、`c`
        // 读回垃圾，而整条链**一个警告都没有**。绑定错了就是产物错了，所以这里把它
        // 钉成编译错误而不是继续生成。
        //
        // 只比较**静态可判定**的形态：内存操作数解析出的地址串，与 Ident 输出写回的
        // 帧槽 / `符号(%rip)`（`@cc`/x87/MMX/段寄存器/显式子寄存器的写回也是 Ident 的槽位，
        // 同一个式子算）。驻足在寄存器里的非 Ident 输出（`"=r"(a[i])`）目的地址各自独占
        // 一个号，静态无从判定，不在此列。
        //
        // 不报的情况：带匹配数字的输出（`"0"`/`"1"` …）—— 它**天生**就是与它引用的那个
        // 操作数同一块存储，那就是匹配约束的含义。
        //
        // 这里**不能**拿「两个操作数写法相同」当豁免条件去对齐 gcc：§5.11 的错位恰好
        // 会把 `"+m"(*v)` 改写成 `Ident{c}`，于是两侧看起来都是同一个 `c` —— 错位自己的
        // 指纹就是「两个输出指向同一变量」。`"=m"(x), "=r"(x)` 这类写法在 gcc 里也是
        // 谁后写谁赢的歧义式，真实代码不会依赖它；把它一起钉住换来的是这一族静默
        // 错绑不再可能漏网（全量 gcc-c-torture 与内核树已建部分实测无命中站点）。
        if !self.current_func_name.is_empty() {
            let mut dests: Vec<(usize, String, String)> = Vec::new();
            for i in 0..num_outputs {
                let raw = output_constraints.get(i).map(String::as_str).unwrap_or("=r");
                if Self::parse_asm_constraint_flags(raw).3.is_some() {
                    continue;
                }
                let Some(&(_, _, is_out, is_mem, _)) = meta.get(i) else {
                    continue;
                };
                if !is_out {
                    continue;
                }
                let Some(op) = outputs.get(i) else {
                    continue;
                };
                let dest = if is_mem {
                    match slots.get(i) {
                        Some(Slot::Mem(s)) if !s.is_empty() => Some(s.clone()),
                        _ => None,
                    }
                } else if let Expr::Ident { name, .. } = op {
                    if self.is_global_not_local(&name.name) {
                        Some(format!("{}(%rip)", self.resolve_global_key(&name.name)))
                    } else {
                        let off = self.var_offset(&name.name);
                        Some(format!("{off}(%r13)"))
                    }
                } else {
                    None
                };
                if let Some(d) = dest {
                    dests.push((i, d, format!("{}", crate::const_eval::ExprDebug(op))));
                }
            }
            for a in 0..dests.len() {
                for b in (a + 1)..dests.len() {
                    if dests[a].1 != dests[b].1 {
                        continue;
                    }
                    self.diag_asm_operand_conflict(&format!(
                        "inline asm output operands {} and {} are both bound to storage '{}' \
                         (operands: {} and {}) — two outputs must not share one destination; \
                         use a matching constraint if they are meant to be the same operand \
                         (in function {})",
                        dests[a].0,
                        dests[b].0,
                        dests[a].1,
                        dests[a].2,
                        dests[b].2,
                        self.current_func_name,
                    ));
                }
            }
        }

        // x87 FPU 栈加载: 每 fld 都会把栈顶 (st(0)) 压向 st(1), 因此
        // **目标 st 号大的最后加载** 才能让 st(0) = st_idx=0 的操作数.
        // 先加载 st_idx=N (N>=1), 最后加载 st_idx=0; 这样 st(0) 是最后一次
        // fld 的结果 (对应 st_idx=0), st(1)/st(N) 是先前 fld 压下来的旧顶.
        if !pending_x87.is_empty() {
            pending_x87.sort_by(|a, b| b.0.cmp(&a.0)); // st_idx 降序: 大的先 fld
            for (st_idx, expr) in pending_x87 {
                self.prepare_asm_x87_binding(expr, st_idx);
            }
        }

        // ===== 延迟装载：所有操作数求值完成，模板发射前把值装进最终寄存器 =====
        // 输出侧条目必须排在输入侧之前（`"0"`/`"2"` 匹配输入要用自己的值覆盖输出
        // 预装载的值）；具体理由与四种形态见 `DeferredGpLoad`。
        for (i, is_inout) in pending_output_gp {
            let target = match slots.get(i) {
                Some(Slot::Reg(r, _)) => *r,
                _ => continue,
            };
            let Some(op) = outputs.get(i) else {
                continue;
            };
            let saved = saved_addrs
                .iter()
                .find(|(idx, _, _)| *idx == i)
                .map(|&(_, r, s)| (r, s))
                .unwrap_or((None, None));
            if is_inout {
                match saved {
                    (Some(park), _) => deferred_out.push(DeferredGpLoad::FromPtr {
                        target,
                        base: park,
                        size: op.ty().size(),
                    }),
                    (_, Some(slot)) => deferred_out.push(DeferredGpLoad::FromAddrSlot {
                        target,
                        slot,
                        size: op.ty().size(),
                    }),
                    _ => self.defer_gp_load(op, target, &mut planner, &mut deferred_out),
                }
            } else if let Expr::Ident { name, .. } = op {
                if self.is_global_not_local(&name.name) {
                    // `=r`(全局) 的写回地址：asm 之后要用，必须在模板前就位。
                    deferred_out.push(DeferredGpLoad::GlobalAddr {
                        target,
                        name: name.name.clone(),
                    });
                } else {
                    // 本地变量：写回按栈槽偏移落值，这条 lea 实际不被使用；保留原行为。
                    self.prepare_asm_binding(op, target, true);
                }
            }
        }
        deferred.splice(0..0, deferred_out);
        self.flush_deferred_gp_loads(deferred);

        // 把 meta 转成 AsmOpBinding
        bindings.clear();
        // 发射侧写完回后按这个标记把 sret 水位还原（见 `asm_pending_sret_mark`）。
        // 这里才写：操作数求值里可能嵌内层 asm，它们已经各自 push/pop 完毕。
        self.asm_pending_sret_mark = Some(addr_slot_mark);
        // 本条 asm 收尾：规划期所有 callee-saved 号（rbx/r12/r14/r15）在这里一次性
        // claim 进 RegBank（置 allocated+used_by_self）；caller-saved 号不在溅出池里，
        // 无需登记。取 planner 所有权，因此这是「规划期结束」的类型边界。
        let exhausted = planner.gp_exhausted();
        planner.finish(&mut self.reg_bank);
        // GP 池（含 callee-saved 四个）全被占时 `alloc_gp` 只能复用已发出去的号，那是静默
        // 错值；点名它（与 legacy `q` 池耗空那条同一口径），而不是把一个确定会盖值的
        // 分配交出去。
        if exhausted {
            self.asm_reject(&format!(
                "impossible constraint: inline asm has more GP operands than free registers \
                 ({} outputs + {} inputs, {} already owned, {} clobbered, in function {}; \
                 outs=[{}] ins=[{}])",
                outputs.len(),
                inputs.len(),
                owned_gp.len(),
                clobber_gp.len(),
                if self.current_func_name.is_empty() {
                    "<unknown>"
                } else {
                    &self.current_func_name
                },
                output_constraints.join(","),
                input_constraints.join(",")
            ));
        }
        for (i, (reg, xmm_w, is_out, is_mem, flags)) in meta.iter().enumerate() {
            // 地址条目按**输出下标**登记（`i` 在 meta 里与输出下标一致到 `num_outputs`
            // 为止），不再用平行的 push 顺序对齐：以前那种下标一旦有前置分支
            // （xmm/固定字母/内存）走掉就会错位到别的操作数上。
            let saved = saved_addrs.iter().find(|(idx, _, _)| *idx == i).copied();
            bindings.push(AsmOpBinding {
                reg: *reg,
                is_output: *is_out,
                is_mem: *is_mem,
                early_clobber: (flags & 1) != 0,
                xmm_width: *xmm_w,
                saved_addr_reg: saved.and_then(|(_, r, _)| r),
                saved_addr_slot: saved.and_then(|(_, _, s)| s),
                mem_addr_reg: mem_scratch.get(i).copied().flatten().map(|(r, _)| r),
                mem_addr_borrowed: mem_scratch
                    .get(i)
                    .copied()
                    .flatten()
                    .is_some_and(|(_, borrowed)| borrowed),
            });
        }
        // name aliases
        for (i, name) in output_names.iter().enumerate() {
            if let Some(n) = name {
                if let Some(reg) = map.get(&format!("%{}", i)) {
                    map.insert(format!("%[{n}]"), reg.clone());
                }
            }
        }
        for (i, name) in input_names.iter().enumerate() {
            if let Some(n) = name {
                let key = format!("%{}", outputs.len() + i);
                if let Some(reg) = map.get(&key) {
                    map.insert(format!("%[{n}]"), reg.clone());
                }
            }
        }
        if Self::asm_operand_map_dbg() {
            let mut kv: Vec<_> = map.iter().collect::<Vec<_>>();
            kv.sort();
            eprintln!(
                "[DBG-ASM]   slots={} bindings={} map={}",
                slots.iter().map(|s| match s {
                    Slot::Imm(t) => format!("Imm({t:?})"),
                    Slot::Mem(t) => format!("Mem({t:?})"),
                    Slot::Reg(r, n) => format!("Reg({r},{n})"),
                }).collect::<Vec<_>>().join(" | "),
                bindings.iter().map(|b| format!("reg={} out={} mem={} mm={} sa={:?}/{:?}", b.reg, b.is_output, b.is_mem, b.xmm_width, b.saved_addr_reg, b.saved_addr_slot)).collect::<Vec<_>>().join(" | "),
                kv.iter().map(|(k, v)| format!("{k}=>{v}")).collect::<Vec<_>>().join(" "),
            );
        }
        map
    }

    /// 同一条 asm 的两个输出被绑到同一块存储 —— 硬失败（判据与理由见
    /// `build_asm_operand_map` 里的 fail-closed 段）。
    ///
    /// 为什么不走 `asm_reject`：那条通道的语义是「这条指令 dcc 编不出来」，
    /// 拼出来的文本与 `--asm-fallback=gas` 提示对「绑定错了」全是误导；而绑定错
    /// 正是 §5.11 那种能编过、能链接、开机 0.15 s 才 #GP 的静默产物缺陷。
    fn diag_asm_operand_conflict(&mut self, msg: &str) {
        use crate::diagnostics::{Diagnostic, Severity, Span};
        let (file, line) = self.asm_src_pos();
        self.diagnostics.push(Diagnostic {
            sev: Severity::Error,
            code: Self::DIAG_ASM_OPERAND_CONFLICT,
            msg: msg.to_string(),
            span: Span::point(if file.is_empty() { "<asm>" } else { &file }, line.max(1), 1),
            hint: Some(
                "两个输出要写同一个操作数请用匹配约束（如 \"=r\"(x) 与 \"1\"），要写不同的对象请换变量；\
                 若你确信这段 asm 是合法的，那就是 dcc 把操作数绑错了 —— 请附上本条报错与最小复现件"
                    .to_string(),
            ),
            source_line: None,
        });
    }

    /// 取一个可在本条 asm 里当内存操作数地址的 callee-saved 号。
    ///
    /// 两个约束同时成立才可用：
    ///   * `RegBank` 里空闲；
    ///   * 不在本条 asm 的 GP clobber 名单里 —— 地址必须在整条 asm 期间有效，
    ///     模板自己会毁掉的号不能拿来存地址。
    ///
    /// 按号逐个试（`alloc_temp_named`）而不是先拿 `alloc_temp` 再退让：退让只能靠
    /// `claim` 把号永久钉住，那会让同一个函数里后续每条 asm 可用的号越用越少。
    /// 返回值第二项表示这个号是**借来**的（见下）。
    fn alloc_asm_mem_scratch(
        &mut self,
        clobber_gp: &HashSet<u8>,
        fixed_gp: &HashSet<u8>,
    ) -> (u8, bool) {
        let candidates = |clobber_gp: &HashSet<u8>, fixed_gp: &HashSet<u8>| {
            [3u8, 12, 14, 15]
                .into_iter()
                .filter(|reg| !clobber_gp.contains(reg) && !fixed_gp.contains(reg))
                .collect::<Vec<u8>>()
        };
        for reg in candidates(clobber_gp, fixed_gp) {
            // 被本条 clobber 的号不能用；被固定约束（`"b"`→rbx 等）钉住的号同样
            // 不能用：地址先装进去、随后被约束值覆盖就是 §5.20 的 cmpxchg16b。
            if self.reg_bank.alloc_temp_named(reg) {
                return (reg, false);
            }
        }
        // 池里四个号全被活值占着。旧实现「退回 r12」是**静默错值**：r12 正握着还要
        // 用的值，模板把地址写进去就把它盖掉了 —— 内核实测 `__blk_mq_alloc_disk` 的
        // `set_bit(GD_OWNS_QUEUE, &disk->state)` 因此让函数返回 `disk + 0x160`，
        // `add_disk_fwnode` 读到 `disk->queue == NULL`，init 崩在地址 0x10。
        //
        // 借一个「正持有值」的号，但在借出前把原值压栈：`sub rsp,8` + `push` 合计
        // 16 字节，`%rsp` 的 call 点对齐不变式不受影响；asm 主体之后由
        // `release_asm_mem_scratch` 按 LIFO `pop` 回来（借几个就弹几个，顺序取反）。
        for reg in candidates(clobber_gp, fixed_gp) {
            if self.reg_bank.is_allocated(reg) && !self.reg_bank.is_banned(reg) {
                self.emit_sub_rsp(8);
                self.push(reg);
                return (reg, true);
            }
        }
        // 一个都借不到（全被 clobber/固定约束排除，或全被禁用）：没有正确产物可发，
        // 记错误让本 TU 失败，而不是再静默盖掉一个活值。
        self.asm_reject(
            "inline asm 的内存操作数地址找不到可用寄存器（callee-saved 池四个号全被 clobber/固定约束排除）",
        );
        (12, false)
    }
    /// Prepare an asm operand.  `is_pure_output` is true when the constraint is
    /// `=r` (write-only — asm will completely overwrite, no need to pre-load
    /// the current value).  It is false for `+r` (inout — must pre-load) and
    /// for pure inputs.
    ///
    /// Returns `Some(addr_reg)` only when the operand is a non-Ident inout
    /// lvalue and the caller should remember this callee-saved scratch across
    /// the asm so writeback can reuse the address instead of re-evaluating
    /// side-effecting sub-expressions (e.g. `"+r"(*bar())` calling `bar()`
    /// twice — see 990130-1.c).
    fn prepare_asm_binding(&mut self, expr: &Expr, reg: u8, is_pure_output: bool) -> Option<u8> {
        if let Expr::Ident { name, .. } = expr {
            if self.is_global_not_local(&name.name) {
                if is_pure_output {
                    self.emit_global_addr(reg, &name.name);
                } else {
                    self.gen_expr(expr, reg);
                }
            } else if is_pure_output {
                let off = self.var_offset(&name.name);
                self.emit_lea_r13(reg, off);
            } else {
                self.gen_expr(expr, reg);
            }
            return None;
        }
        // 非 Ident 表达式
        if is_pure_output {
            // `=r`: asm 完全覆盖, 无需预加载, 也不需要保存地址 (writeback 时再算一次即可).
            return None;
        }
        // inout (`+r`) 或纯输入: 求值当前值到绑定寄存器.
        self.gen_expr(expr, reg);
        // 对 inout (由输出侧调用) 我们再单独在 caller 里计算一次地址并溅到 scratch.
        None
    }
    fn prepare_asm_mmx_binding(&mut self, expr: &Expr, mm: u8) {
        if let Expr::Ident { name, ty, .. } = expr {
            let off = self.var_offset(&name.name);
            let sz = ty.size();
            // movd/movq [r13+off], mmN
            // movd m32, mm = 0F 7E ; movq m64, mm = 0F 7F
            // 注意: reg=mm_index, r/m=5 (r13)
            // 但方向是内存→寄存器: movd mm, [r13+off] = 0F 6E
            // movq mm, [r13+off] = 0F 6F
            if sz <= 4 {
                self.emit_insn(&Insn::raw(&[0x41, 0x0F, 0x6E]));
            } else {
                self.emit_insn(&Insn::raw(&[0x41, 0x0F, 0x6F]));
            }
            if off.abs() < 128 {
                self.emit_insn(&Insn::raw(&[0x45 | ((mm & 7) << 3), off as i8 as u8]));
            } else {
                self.emit_insn(&Insn::raw(&[0x85 | ((mm & 7) << 3)]));
                self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
            }
            return;
        }
        // 非变量: gen_expr 到 rax, 再 movd/mm
        self.gen_expr(expr, Self::EVAL_BASE);
        self.emit_insn(&Insn::raw(&[0x0F, 0x6E, 0xC0 | ((mm & 7) << 3)])); // movd mm, eax
    }
    fn prepare_asm_seg_binding(&mut self, expr: &Expr, seg_code: u8) {
        if let Expr::Ident { name, .. } = expr {
            // 局部优先 (同上): 同名遮蔽时取栈地址而非全局地址
            if self.is_global_not_local(&name.name) {
                // 全局: lea 到 rax, 再 mov Sreg, [rax]
                self.emit_global_addr(0, &name.name);
                // mov Sreg, [rax] = 8E /r (mod=00, reg=seg, r/m=0)
                self.emit_insn(&Insn::raw(&[0x8E, (seg_code & 7) << 3]));
            } else {
                let off = self.var_offset(&name.name);
                // mov Sreg, [r13+off] = 8E /r (REX.B, reg=seg, r/m=5)
                self.emit_insn(&Insn::raw(&[0x41, 0x8E]));
                if off.abs() < 128 {
                    self.emit_insn(&Insn::raw(&[0x45 | ((seg_code & 7) << 3), off as i8 as u8]));
                } else {
                    self.emit_insn(&Insn::raw(&[0x85 | ((seg_code & 7) << 3)]));
                    self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
                }
            }
            return;
        }
        self.gen_expr(expr, Self::EVAL_BASE);
        self.emit_insn(&Insn::raw(&[0x8E, (seg_code & 7) << 3]));
    }
    fn prepare_asm_x87_binding(&mut self, expr: &Expr, st_idx: u8) {
        let _ = st_idx; // 仅作注释, 不影响本函数行为
        if let Expr::Ident { name, ty, .. } = expr {
            // _Imaginary: 按内型
            let effective_ty = if let Type::Imaginary(ref inner) = ty {
                inner.base.clone()
            } else {
                ty.clone()
            };
            // 局部优先: 与全局同名的参数/局部变量必须走栈槽 fld
            if self.is_global_not_local(&name.name) {
                // 全局: RIP 相对 fld，float=D9 /5 double=DD /5 ld=DB /5
                let (opcode, modrm) = match effective_ty {
                    Type::Float => (0xD9u8, 0x05u8),
                    Type::LongDouble => (0xDBu8, 0x2Du8),
                    _ => (0xDDu8, 0x05u8),
                };
                // 先发射再按 `pos()-4` 取 disp32（发射入口可能在本条之前插字节，
                // 发射前算的绝对位置会整条失效）。与 `mem/access.rs` 同一口径。
                self.emit_insn(&Insn::raw(&[opcode, modrm, 0, 0, 0, 0]));
                self.push_global_float_rel(self.pos() - 4, &name.name);
            } else {
                let off = self.var_offset(&name.name);
                self.emit_x87_fld_at(13, off as i32, &effective_ty);
            }
            return;
        }
        // 非变量表达式: gen_expr 按通道约定求值（ld 在 st0、f32/f64 在
        // xmm0、整数在 rax），再统一搬到 st0；旧代码无条件发 fld m64
        // [rax]，把整数/指针位型当 double 且漏掉 xmm0/st0 两种来源。
        self.gen_expr(expr, Self::EVAL_BASE);
        self.emit_operand_to_st0(&expr.ty(), 0);
    }
    fn prepare_asm_xmm_binding(&mut self, expr: &Expr, xmm: u8, _is_inout: bool) {
        if let Expr::Ident { name, ty, .. } = expr {
            if matches!(ty, Type::Float | Type::Double | Type::LongDouble)
                || matches!(ty, Type::Imaginary(_))
            {
                // 局部优先: 与全局同名的参数/局部变量 (如 double b 与
                // static float b[]) 必须走栈槽加载, 不得误入全局 rip 路径
                if self.is_global_not_local(&name.name) {
                    self.emit_global_float_to_xmm_typed(xmm, &name.name, ty);
                } else {
                    let off = self.var_offset(&name.name);
                    let effective_ty = if let Type::Imaginary(ref inner) = ty {
                        inner.base.clone()
                    } else {
                        ty.clone()
                    };
                    self.emit_mem_float_to_xmm(13, off as i32, &effective_ty, xmm);
                }
                return;
            }
        }
        self.gen_expr(expr, Self::EVAL_BASE);
        let ty = expr.ty();
        if self.float_in_st0(&ty) {
            // long double 表达式结果在 st0：经栈 scratch 单步舍入到 f64
            self.emit_x87_to_xmm0();
            if xmm != 0 {
                self.mov_xmm_to_xmm(xmm, 0);
            }
        } else if matches!(ty, Type::Float | Type::Double) {
            self.mov_xmm_to_xmm(xmm, 0);
        }
    }
    /// 「只能接立即数」的约束拿到了运行时值：当场点名，不静默。
    ///
    /// GCC 对这一形状直接报 `impossible constraint in 'asm'`。dcc 先把内联的
    /// `__always_inline` 展开试一遍（`opt.rs::try_stmt_inline_call`），只有它也没能
    /// 把实参传进来时才会走到这里，此时操作数会落成寄存器，模板里 `%c[op]`/`%P[op]`
    /// 会被换成裸寄存器名，数据伪指令就会发一条指向不存在符号的重定位（实测
    /// `vmlinux.o` 的 `.altinstructions` 特性字段上有 98 条 `→ rax`，objtool 报
    /// `special: can't find orig instruction`）。报而不拒是因为那些体带 `asm goto` 的
    /// 内核分派当前还不能在表达式位置展开，退给宿主编译器会让大半棵树不再由 dcc 编；
    /// 但风险必须在日志里看得见，不能只到链接后才露出来。
    pub(crate) fn warn_asm_immediate_only_operand_runtime(&mut self, constraint: &str, idx: usize) {
        use crate::diagnostics::{Diagnostic, Span};
        let (file, line) = self.asm_src_pos();
        let ordinal = self.current_asm_line;
        let mut diag = Diagnostic::warn(
            "W1016",
            format!(
                "inline asm operand {idx} has an immediate-only constraint \"{constraint}\" but no \
                 compile-time constant value (not inlined); `%c`/`%P` references to it will emit an \
                 unresolved reference"
            ),
            Span::point(if file.is_empty() { "<asm>" } else { &file }, line, 1),
        );
        diag.hint = Some(format!(
            "asm 语句第 {ordinal} 行：把被调函数改成能在表达式位置内联，或给该操作数加上 \
             \"r\" 备选约束"
        ));
        self.diagnostics.push(diag);
    }

    pub(crate) fn warn_unresolved_asm_operand(&mut self, line: &str, asm_line: usize) {
        self.asm_reject(&format!(
            "unsupported inline asm operand pattern '{}' (asm line {})",
            line, asm_line
        ));
    }

    /// 操作数是 `register T v asm("rN")`（含文件作用域）的变量时，返回它绑定的号。
    ///
    /// `"r"`/`"+r"` 约束写在 GCC 里的含义是「把它的**住处**写进操作数表」，不是「请给
    /// 它分配一个号」：另分配一个号既拿不到变量的值（值在固定号里），又白吃一个号。
    /// 实测 `call_on_stack`（`arch/x86/include/asm/irq_stack.h`）的 `tos` 在 output
    /// （`"+r"`）与 input（`"r"`）各出现一次，两次各自分配就把 13 个号里最后两个
    /// 挤掉，报 `more GP operands than free registers`（`arch/x86/mm/fault.o`）；
    /// 而 GCC 在这里只需 5 个号（r11 + 3 个实参 + `current_stack_pointer`）。
    fn pinned_gp_of_operand(&mut self, op: &Expr) -> Option<u8> {
        let name = match op {
            Expr::Ident { name, .. } => name.name.clone(),
            _ => return None,
        };
        // 只查**已有**的家，绝不能走 `home_for`：那条路的兜底是 `var_offset()`，会给一个
        // 还没家的名字现场登记栈槽（`fxsave_area` 这种全局数组名就是这幺被登进晚分配带的）——
        // 静态扫描 `LateNeed` 不知道这次登记，于是凭空多出一槽、触发 E1133。
        // 探测未命中时返回 None，调用方继续走原来的 `alloc_gp`，行为与不打这张表一致。
        let loc = if let Some((loc, _)) = self.global_homes.get(&name) {
            *loc
        } else if let Some(loc) = self.home_overrides.get(&name) {
            *loc
        } else if let Some((loc, _)) = self.homes.get(&name) {
            *loc
        } else {
            return None;
        };
        match loc {
            crate::location::Location::Reg(r)
                if r.bank == crate::location::RegBank::Gp && self.reg_bank.is_pinned(r.n) =>
            {
                Some(r.n)
            },
            _ => None,
        }
    }
    /// 把 asm 输出寄存器写回一个**非 Ident** 左值（`a[i]` / `*p` / `s.f` / `a[i].f`）。
    ///
    /// 为什么需要它：`writeback_asm_operand` 的每个分支都以
    /// `let Expr::Ident { .. } = expr else { return }` 开头，于是这类输出被**静默丢掉**。
    /// 实测 `__asm__ volatile("movq $5, %0" : "=q"(a[1]))` 之后 `a[1]` 仍是 0（gcc 是 5），
    /// `"=r"(a[2])`、`"=q"(gs[0])`（全局数组）同样丢 —— 而「把结果取到数组元素」是
    /// 内核与库代码里的常见写法。
    ///
    /// 返回 `false` 表示这条路走不了（地址算不出，或宽度不是一条 store 能表达的：
    /// 聚合 / 向量 / long double / 128 位整数）。调用方保留原有行为：宁可维持旧状态，
    /// 也不把值写到一个算错的位置。
    ///
    /// scratch 的来源分三层（顺序即优先级）：
    ///   1. `saved_addr_reg` —— 模板之前算好的地址驻足号（callee-saved），不再求值；
    ///   2. `saved_addr_slot` —— 驻足池耗尽时落的 sret 帧槽，只 `load_stack` 一次；
    ///   3. 都没有（地址预计算失败/文件作用域）：现算，scratch 优先 callee-saved，
    ///      池满退到 caller-saved（避开 `out_mask` 里还没写回的输出与钉住的家）。
    fn writeback_asm_general_lvalue(
        &mut self,
        expr: &Expr,
        src: u8,
        xmm_width: u8,
        saved_addr_reg: Option<u8>,
        saved_addr_slot: Option<i64>,
        out_mask: u64,
    ) -> bool {
        let ty = expr.ty();
        let effective_ty = match &ty {
            Type::Imaginary(inner) => inner.base.clone(),
            other => other.clone(),
        };
        let excluded = matches!(
            &effective_ty,
            Type::Void
                | Type::LongDouble
                | Type::Complex(_)
                | Type::Imaginary(_)
                | Type::Array(..)
                | Type::Vector(..)
                | Type::Struct(..)
                | Type::Union(..)
                | Type::Func(..)
                | Type::Named(_)
                | Type::Int128
                | Type::UInt128
        );
        let sz = effective_ty.size();
        if excluded || sz == 0 || sz > 8 {
            return false;
        }
        // float / double 的输出寄存器是 xmm，存法不同（movss / movsd）。注意
        // `"=x"` 的 `xmm_width` 一律是 128（见 `asm_constraint_wants_xmm`），所以
        // 宽度必须按**类型**判，而不是照抄 xmm_width。这一条现在才敢做：
        // `movsd %xmm1, %xmm0` 以前被编成字符串搬移 0xA5（见 emit_lines.rs 里
        // `movsd` 的守卫），写回还没走到程序就已经崩了。
        let float_bits = match &effective_ty {
            Type::Float => Some(32u8),
            Type::Double => Some(64u8),
            _ => None,
        };
        if xmm_width != 0 && float_bits.is_none() {
            // 操作数在 xmm 里但类型不是 float/double：不知道该怎么存，保持旧行为。
            return false;
        }
        // 调用方已在模板之前算好 lvalue 地址：priority 1/2 直接复用，避免重跑
        // `gen_lvalue_addr` —— 既会二次求值副作用子表达式（`"+r"(*bar())` 把 bar()
        // 调两次，见 990130-1.c），也会踩掉还没写回的输出寄存器（求值固定用
        // %rax/%rcx/%rdx 当暂存）。
        let scratch = match saved_addr_reg {
            Some(r) => r,
            None => match self.alloc_writeback_scratch(out_mask) {
                Some(r) => r,
                None => return false,
            },
        };
        let addr_ready = if saved_addr_reg.is_some() {
            true
        } else if let Some(off) = saved_addr_slot {
            // 约定：地址在模板之前已按 8 字节落进该帧槽。
            self.load_stack(scratch, off);
            true
        } else {
            self.gen_lvalue_addr(expr, scratch)
        };
        if !addr_ready {
            if saved_addr_reg.is_none() {
                self.free_callee_saved(scratch);
            }
            return false;
        }
        match float_bits {
            Some(bits) => self.store_xmm_to_ptr(src, scratch, bits),
            None => self.store_reg_to_ptr(src, scratch, sz),
        }
        if saved_addr_reg.is_none() {
            self.free_callee_saved(scratch);
        }
        true
    }

    /// 写回期的地址暂存号。
    ///
    /// 优先 `alloc_callee_saved`（池内号会进序言保存）；池满退到 caller-saved ——
    /// 此时模板已经执行完，输入寄存器全部死亡，唯一要避开的是 `out_mask` 里
    /// **还没写回的输出寄存器**（它们的值还得落盘）与 `register asm` 钉住的家。
    fn alloc_writeback_scratch(&mut self, out_mask: u64) -> Option<u8> {
        if let Some(r) = self.alloc_callee_saved() {
            if r < 64 && out_mask & (1u64 << r) == 0 {
                return Some(r);
            }
            self.free_callee_saved(r);
        }
        for r in [0u8, 1, 2, 6, 7, 8, 9, 10, 11] {
            if out_mask & (1u64 << r) != 0 || self.reg_bank.is_pinned(r) {
                continue;
            }
            return Some(r);
        }
        None
    }

    pub(crate) fn writeback_asm_operand(
        &mut self,
        expr: &Expr,
        reg: u8,
        xmm_width: u8,
        saved_addr_reg: Option<u8>,
        saved_addr_slot: Option<i64>,
        out_mask: u64,
    ) {
        // 非 Ident 左值先走通用写回（原因与实测见 `writeback_asm_general_lvalue`）。
        // 必须放在最前面：下面每个分支都以 `Expr::Ident` 为前提，非 Ident 落进去就是
        // 静默丢值。x87 / MMX / 段寄存器 / 条件码（`xmm_width` 的 2xx 编码）仍走各自
        // 的分支 —— 它们的宽度语义与 GP/SSE 两条不同，混进来只会让诊断变模糊。
        if !matches!(expr, Expr::Ident { .. })
            && matches!(xmm_width, 0 | 32 | 64 | 128)
            && self.writeback_asm_general_lvalue(
                expr,
                reg,
                xmm_width,
                saved_addr_reg,
                saved_addr_slot,
                out_mask,
            )
        {
            return;
        }
        // x87 FPU 栈寄存器写回 (f/t/u 约束): fstp 存储 st(0) 到内存
        if xmm_width == 200 || xmm_width == 201 {
            let Expr::Ident { name, ty, .. } = expr else {
                return;
            };
            let effective_ty = if let Type::Imaginary(ref inner) = ty {
                inner.base.clone()
            } else {
                ty.clone()
            };
            if self.is_global_not_local(&name.name) {
                // 全局: RIP 相对 fstp
                // float=D9 /3 double=DD /3 long double=DB /7
                let (opcode, modrm) = match effective_ty {
                    Type::Float => (0xD9u8, 0x1Du8),
                    Type::LongDouble => (0xDBu8, 0x3Du8),
                    _ => (0xDDu8, 0x1Du8),
                };
                // 同上：先发射再取 disp32。
                self.emit_insn(&Insn::raw(&[opcode, modrm, 0, 0, 0, 0]));
                self.push_global_float_rel(self.pos() - 4, &name.name);
            } else {
                let off = self.var_offset(&name.name);
                self.emit_x87_fstp_at(13, off as i32, &effective_ty);
            }
            return;
        }
        // MMX 寄存器写回 (y 约束): movq/movd 存储 mm 到内存
        if xmm_width == 202 {
            let Expr::Ident { name, ty, .. } = expr else {
                return;
            };
            let off = self.var_offset(&name.name);
            let sz = ty.size();
            // movq mm, [r13+off] = 0F 7F ; movd mm, [r13+off] = 0F 7E
            // reg 字段 = mm 索引, r/m = 5 (r13)
            if sz <= 4 {
                self.emit_insn(&Insn::raw(&[0x41, 0x0F, 0x7E]));
            } else {
                self.emit_insn(&Insn::raw(&[0x41, 0x0F, 0x7F]));
            }
            if off.abs() < 128 {
                self.emit_insn(&Insn::raw(&[0x45 | ((reg & 7) << 3), off as i8 as u8]));
            } else {
                self.emit_insn(&Insn::raw(&[0x85 | ((reg & 7) << 3)]));
                self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
            }
            return;
        }
        // 段寄存器写回 ({gs}/{fs} 约束): mov %seg, [r13+off] (16-bit)
        if xmm_width == 203 {
            let Expr::Ident { name, .. } = expr else {
                return;
            };
            let off = self.var_offset(&name.name);
            // mov [r13+off], Sreg = 8C /r (16-bit)
            // REX.B for r13, reg = seg code, r/m = 5
            self.emit_insn(&Insn::raw(&[0x41, 0x8C]));
            if off.abs() < 128 {
                self.emit_insn(&Insn::raw(&[0x45 | ((reg & 7) << 3), off as i8 as u8]));
            } else {
                self.emit_insn(&Insn::raw(&[0x85 | ((reg & 7) << 3)]));
                self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
            }
            return;
        }
        // 条件码写回 (@cc 约束): setcc + movzx + store
        // 使用 push/pop rax 保护其他输出操作数的寄存器值
        if xmm_width == 204 {
            let Expr::Ident { name, .. } = expr else {
                return;
            };
            let off = self.var_offset(&name.name);
            // reg 字段 = setcc opcode (0F 90+off)
            self.emit_insn(&Insn::raw(&[0x50])); // push %rax (preserve across setcc)
            self.emit_insn(&Insn::raw(&[0x0F, reg, 0xC0])); // setcc %al
            self.emit_insn(&Insn::raw(&[0x0F, 0xB6, 0xC0])); // movzx %al, %eax
            self.emit_insn(&Insn::raw(&[Self::rex(true, 0, 0, 13), 0x89, 0x85])); // mov %rax, [r13+off]
            self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
            self.emit_insn(&Insn::raw(&[0x58])); // pop %rax (restore)
            return;
        }
        // 显式子寄存器写回 ({ecx}/{cx}/{cl} 约束)
        if (205..=208).contains(&xmm_width) {
            let Expr::Ident { name, .. } = expr else {
                return;
            };
            let off = self.var_offset(&name.name);
            let size_bits = (xmm_width - 204) * 8; // 8, 16, 32, 64
            match size_bits {
                8 => {
                    self.emit_insn(&Insn::raw(&[0x41, 0x88, 0x85 | ((reg & 7) << 3)]));
                    self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
                },
                16 => {
                    self.emit_insn(&Insn::raw(&[0x66, 0x41, 0x89, 0x85 | ((reg & 7) << 3)]));
                    self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
                },
                32 => {
                    self.emit_insn(&Insn::raw(&[
                        Self::rex(true, reg, 0, 13),
                        0x89,
                        0x85 | ((reg & 7) << 3),
                    ]));
                    self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
                },
                _ => {
                    self.store_stack(reg, off);
                },
            }
            return;
        }
        if xmm_width != 0 {
            let Expr::Ident { name, ty, .. } = expr else {
                return;
            };
            let effective_ty = if let Type::Imaginary(ref inner) = ty {
                inner.base.clone()
            } else {
                ty.clone()
            };
            if self.is_global_not_local(&name.name) {
                if self.float_in_st0(&effective_ty) {
                    // xmm 约束写回 long double 全局：拓宽后 fstp tbyte
                    let src_ty = if matches!(effective_ty, Type::Float) {
                        Type::Float
                    } else {
                        Type::Double
                    };
                    self.emit_xmm_to_global_float_typed(reg, &name.name, &src_ty, &effective_ty);
                } else {
                    self.emit_global_float_store_from_xmm(reg, &name.name, &effective_ty);
                }
            } else {
                let off = self.var_offset(&name.name);
                if self.float_in_st0(&effective_ty) {
                    let src_ty = if matches!(effective_ty, Type::Float) {
                        Type::Float
                    } else {
                        Type::Double
                    };
                    self.emit_xmm_to_mem_float(reg, &src_ty, 13, off as i32, &effective_ty);
                } else if matches!(effective_ty, Type::Float) || xmm_width == 32 {
                    self.store_xmm32_stack(reg, off);
                } else {
                    self.store_xmm64_stack(reg, off);
                }
            }
            return;
        }
        let Expr::Ident { name, ty, .. } = expr else {
            return;
        };
        // 全局变量写回：使用 RIP-relative store，不得走 var_offset 栈槽路径
        // (pr98096.c 里 "+r"(i) 全局触发 E1134/E1133)。
        if self.is_global_not_local(&name.name) {
            self.emit_global_store(reg, &name.name);
            return;
        }
        // 变量绑在寄存器里（用户的 `register asm`，或 P12 的自动分配）时，它的家
        // 不是栈槽。旧代码无条件按 `[r13+off]` 写回，于是 asm 的结果落进一个再没
        // 人读的槽里：实测 `register long v asm("rbx")` +
        // `__asm__ volatile("movq $5, %0" : "=r"(v))` 之后 `v` 仍是 0，gcc 是 5。
        // 走 `mov_to_stack` 顺带把宽度口径也统一掉（见 `var_access_shape`）。
        if self.homes.contains_key(&name.name) {
            if xmm_width == 0 {
                self.mov_to_stack(reg, &name.name);
                return;
            }
            // SSE 的输出值在 xmm 里，而变量的家是 GP 寄存器：先按宽度存进栈槽，
            // 再从槽读进一个临时 callee-saved 号，最后仍由 `mov_to_stack` 进绑定
            // 寄存器。这个组合（`register double v asm("rbx")` + `"=x"(v)`）偏到
            // 角落，但没有任何理由让它变成静默错值。
            let off = self.var_offset(&name.name);
            let sz = ty.size();
            if matches!(ty, Type::Float) {
                self.store_xmm32_stack(reg, off);
            } else {
                self.store_xmm64_stack(reg, off);
            }
            if let Some(tmp) = self.alloc_callee_saved() {
                self.load_stack_sized(tmp, off, sz);
                self.mov_to_stack(tmp, &name.name);
                self.free_callee_saved(tmp);
            }
            return;
        }
        if matches!(ty, Type::Float | Type::Double | Type::LongDouble)
            || matches!(ty, Type::Imaginary(_))
        {
            let effective_ty = if let Type::Imaginary(ref inner) = ty {
                inner.base.clone()
            } else {
                ty.clone()
            };
            let off = self.var_offset(&name.name);
            if self.float_in_st0(&effective_ty) {
                // 该兜底分支不可达：long double 输出必走 xmm/x87 绑定
                // （xmm_width != 0），不会落到 GP 写回路径。
                unreachable!("long double writeback must use xmm/x87 binding");
            } else if matches!(effective_ty, Type::Float) {
                self.store_xmm32_stack(reg, off);
            } else {
                self.store_xmm64_stack(reg, off);
            }
            return;
        }
        let off = self.var_offset(&name.name);
        let sz = ty.size();
        if sz <= 1 {
            self.emit_insn(&Insn::raw(&[0x41, 0x88, 0x85 | ((reg & 7) << 3)]));
            self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
        } else if sz <= 2 {
            self.emit_insn(&Insn::raw(&[0x66, 0x41, 0x89, 0x85 | ((reg & 7) << 3)]));
            self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
        } else if sz <= 4 {
            // 32-bit 整型写回需零扩展到 64 位槽, 否则后续 64-bit 重载读到栈残留
            // 旧实现用 `mov %eax, [r13+off]` (4 字节), C codegen 后续用 64-bit 读
            // (mov -off(%r13), %rax) 会读到未初始化高 32 位.
            // 改为 `mov %rax, [r13+off]` (REX.W + REX.B, 8 字节), 保证 64-bit 槽整体更新.
            self.emit_insn(&Insn::raw(&[
                Self::rex(true, reg, 0, 13),
                0x89,
                0x85 | ((reg & 7) << 3),
            ]));
            self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
        } else {
            self.store_stack(reg, off);
        }
    }
}
