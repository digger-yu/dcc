// src/codegen/x86_64/asm/emit.rs
// Assembly emission functions for x86_64 backend
// Contains main entry points for inline assembly code generation
// Extracted from src/codegen/x86_64/mod.rs (PR-A2 Phase)

use crate::ast::Expr;
use crate::codegen::x86_64::asm::AsmOpBinding;
use crate::codegen::x86_64::{AsmDirectiveExpander, AsmLocalLabels, Codegen};

/// Context for ASM operand binding
#[derive(Debug, Clone)]
pub struct Binding {
    pub reg: u8,
    pub xmm_width: u16,
    pub is_output: bool,
    pub is_mem: bool,
}

impl Codegen {
    /// P12 Phase 5.2a：本条 asm 会毁掉、而**此刻正被本函数持有值**的池寄存器。
    ///
    /// clobber 名单说的是「这些号的内容在本条 asm 之后不保证」。而现在可以住在那儿的
    /// 有两类东西：某个变量的家（`RegBank::claim`）与某个表达式中间值的溅出槽
    /// （`RegBank::alloc_temp`）。两侧此前都不认识对方，两个都是本轮实测出的静默错值：
    ///   * 自动绑到 %rbx 的循环变量跨一条 `::: "rbx"` ⇒ `f(10)` 得 632360（gcc 3050）；
    ///   * `(q+r)` 的溅出槽在 %rbx 上跨同一条 asm ⇒ 得 38885（gcc 25），-O0 就坏。
    ///
    /// 两个读入口都用这份名单，但用法不同（`asm_guard_enter` 与 `asm_unguardable`）：
    ///   * 本条 asm 的**操作数**正拿着值的号在这里被排除 —— 那些号由写回路径负责，弹回
    ///     旧值会把 asm 的输出抹掉（规划器本来会把已占用号排除在发号之外，固定约束如
    ///     `"=b"` 是例外，所以这道门必须显式设在这里）；
    ///   * 跳得出的 `asm goto` 不能靠守护保值（跳出路径不经过恢复），所以那些号在函数
    ///     入口就被移出溅出池（`Codegen::ban_escaping_asm_regs`）；自动绑定的家不会出现在
    ///     这里，因为带 `Label`/`AsmGoto` 的函数根本进不了 SSA（见
    ///     `ssa::regalloc::tests::escaping_asm_functions_do_not_reach_the_allocator`）。
    ///     于是走到这条名单上的只剩**用户显式绑定**的家 —— 那既不能拒（用户写的）也不能守
    ///     （守不住），只能报错。
    ///
    /// naked 函数不算：那里没有帧也没有序言，栈是用户在管的，而且这条路径上
    /// `reset_reg_tracker()` 没跑、`allocated` 还是上一个函数的残值。
    fn asm_clobber_held_regs(&self, clobbers: &[String], bindings: &[AsmOpBinding]) -> Vec<u8> {
        if self.cur_func_is_naked || clobbers.is_empty() {
            return Vec::new();
        }
        // 解析只走现有那一份（与规划器拿到的 clobber 名单同一个口径），不新建解析器。
        let (clobber_gp, _, _) = Self::parse_clobber_registers(clobbers);
        if clobber_gp.is_empty() {
            return Vec::new();
        }
        // GP 形态的绑定：0 = 64 位 GP，205..208 = 显式子寄存器（reg 字段仍是 GP 索引）。
        // XMM/x87/段/条件码的 `reg` 字段与 GP 不同域，不得进来参与排除（否则会把一个
        // 同数的 GP 误会成「操作数正拿着值」而漏守护）。
        let mut op_gp: Vec<u8> = bindings
            .iter()
            .filter(|b| b.xmm_width == 0 || matches!(b.xmm_width, 205..=208))
            .map(|b| b.reg)
            .collect();
        op_gp.sort_unstable();
        op_gp.dedup();
        let mut held = self.reg_bank.held_pool_regs();
        // 延伸池家（r8/r9，caller-saved）：这些号不在 `reg_bank` 的 `allocated` 位掩码里
        // （位宽只有 4 位，对应 callee-saved POOL），但它们也是「正被本函数持有值」的号。
        // 普通 call 由 `persist_ext_homes`/`restore_ext_homes` 在栈槽里保值；内联 asm
        // 走的是另一条路（直接发射用户模板，不经 `emit_sysv_call`），所以这里必须把
        // 被本条 asm 毁掉的延伸池号一并纳入 push/pop 守护，否则循环计数器之类的家
        // 会在 asm 体内被悄悄抹掉（实测 -O2 `one_guarded`/`two_guarded` 死循环：r8
        // 持有的循环计数器被 `"r8"` clobber 毁掉，下一轮读回 0）。
        for entry in &self.ext_home_saves {
            held.push(entry.reg);
        }
        // 帧基址与帧指针也是「正被持有值」的号：全部局部量都是 `%r13+disp`，帧基址被毁
        // 就是整个帧被毁。实测（本轮）`::: "r13"` 不守护也不报错，-O0/-O2 都是 SIGSEGV(139)，
        // 而 gcc 同一源得 9 —— 它把 r13 当成普通 callee-saved，而本端把它当帧基址。
        // 守它不需新增机制：同一个 push/pop 对就能把帧基址原样还回来（奇数时会连同下面
        // 那道对齐补偿一起算）。`-fno-omit-frame-pointer` 时 rbp 也在用（序言 push 它、
        // 尾声 pop 它），所以一并守。
        if clobber_gp.contains(&13u8) {
            held.push(13u8);
        }
        if self.target.frame_pointer && clobber_gp.contains(&5u8) {
            held.push(5u8);
        }
        held.sort_unstable();
        held.dedup();
        held.into_iter()
            .filter(|r| clobber_gp.contains(r) && !op_gp.contains(r))
            .collect()
    }

    /// `%rsp` 不能作为 clobber：它是这对守护自己的基座（守无可守），也是帧与返回地址的
    /// 定位根据。这不是本后端的保守，是 GCC 自己也拒绝的写法（clobber 名单里不允许 sp），
    /// 所以直接报错而不是静默弄坏帧。
    ///
    /// 注意与 `emit/func_setup.rs::reserved_reg_reason` 的分工：那边拒的是「把保留号当
    /// 变量的家」（全程不可恢复），这边只拒「守不了的号」—— `%r13`/`%rbp` 作为家不行，
    /// 但作为一次性的 clobber 是**守得住**的（push/pop 原样还回来），所以这里不能报错；
    /// 而跳得出的 `asm goto` 守不住它们，那边已由 E0317（守护名单非空即点名）接住。
    fn asm_reject_reserved_clobber(&mut self, clobbers: &[String]) {
        use crate::diagnostics::{Diagnostic, Severity, Span};
        let (clobber_gp, _, _) = Self::parse_clobber_registers(clobbers);
        if !clobber_gp.contains(&4u8) {
            return;
        }
        let (file, line) = self.asm_src_pos();
        self.diagnostics.push(Diagnostic {
            sev: Severity::Error,
            code: Self::DIAG_RESERVED_REG,
            msg: "asm clobber 了 %rsp：栈指针是局部寻址、序言保存与内联 asm 守护的基座，守无可守".to_string(),
            span: Span::point(&file, line.max(1), 1),
            hint: Some(
                "要改栈请在模板里自己 push/pop 平衡回去；GCC 同样不允许 clobber %rsp".to_string(),
            ),
            source_line: None,
        });
    }

    /// 进守护区间：每个被毁的号 `push` 一次；**奇数个 `push` 会把 `%rsp` 从本后端自己
    /// 写死的不变式上推离 16 字节**（体内每个 call 点都依赖它，见
    /// `emit/stmt.rs` 里 pr44575 那条注释：少这道对齐实测就是 libc 的 `movaps` 崩），
    /// 所以下面补一次 `sub rsp, 8`，与 `asm_guard_leave` 的 `add rsp, 8` 成对。
    /// 返的正是「有没有补那道对齐」，调用方把它原样交给 `leave`，不存在「只记得一边」。
    fn asm_guard_enter(&mut self, guard: &[u8]) -> bool {
        let padded = guard.len() % 2 == 1;
        if padded {
            self.emit_sub_rsp(8);
        }
        for &r in guard {
            self.push(r);
        }
        padded
    }

    /// 出守护区间：`enter` 的反操作（弹回逆序 + 把对齐还回去）。区间外 `%rsp` 与今天一致。
    fn asm_guard_leave(&mut self, guard: &[u8], padded: bool) {
        for &r in guard.iter().rev() {
            self.pop(r);
        }
        if padded {
            self.emit_add_rsp(8);
        }
    }

    /// 跳得出的 asm 守不住一个正被它毁掉的号 ⇒ 点名报错，而不是给一个错值或一个
    /// 永远不执行的 `pop`（那会把 `%rsp` 永久下推，后续每个 call 点都错位）。
    ///
    /// 为什么今天能把它当**用户代码错误**（E03xx，与 E0316 同族）而不是后端不变量破坏：
    /// 溅出槽已由函数入口的 `ban_escaping_asm_regs` 移出池，自动绑定的家已由「这类函数
    /// 不进 SSA」挡在外面，所以还能出现在这份名单里的只有用户自己写的
    /// `register long v asm("rbx")` 与同一函数里 `::: "rbx"` 的 `asm goto` —— 两个宣告互相
    /// 矛盾，没有一种解释能同时满足它们（gcc 对这种写法也是直接让 `v` 被毁）。
    fn asm_unguardable(&mut self, regs: &[u8]) {
        use crate::diagnostics::{Diagnostic, Severity, Span};
        const DIAG_NO_GUARD: &str = "E0317";
        let names: Vec<String> = regs
            .iter()
            .map(|&r| {
                // 号→名只有一份表（`abi/regnames.rs`）。诊断文本里宁可显示一个看得见
                // 的坏号：本函数已经在报 E0317，这里再报一次内部错误会把真正的
                // 诊断顶掉（而旧 `gp_reg_name` 的静默折回会把它报成 `%rax`）。
                crate::codegen::x86_64::abi::regnames::att_name_of_num(r, 64)
                    .unwrap_or_else(|| format!("<bad-reg-{r}>"))
            })
            .collect();
        let (file, line) = self.asm_src_pos();
        self.diagnostics.push(Diagnostic {
            sev: Severity::Error,
            code: DIAG_NO_GUARD,
            msg: format!(
                "asm goto 会把控制流跳出守护区间，无法保住它同时 clobber 的寄存器: {}",
                names.join(", ")
            ),
            span: Span::point(&file, line.max(1), 1),
            hint: Some(
                "这些号此刻正被某个 `register asm` 变量拿着；要么去掉该 clobber，\
                 要么不要把变量绑到它们上（跳出路径上不经过恢复）"
                    .to_string(),
            ),
            source_line: None,
        });
    }

    /// 发射内联汇编模板
    ///
    /// This function extracts the complete emit_inline_asm implementation
    /// originally at line 32455-32543 in mod.rs (~90 lines of code)
    pub fn emit_inline_asm(
        &mut self,
        asm_str: &str,
        outputs: &[Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        clobbers: &[String],
        volatile: bool,
    ) {
        let mut bindings = Vec::new();
        let op_map = self.build_asm_operand_map(
            outputs,
            output_names,
            output_constraints,
            inputs,
            input_names,
            input_constraints,
            clobbers,
            &mut bindings,
        );
        let s = if asm_str.starts_with('"') && asm_str.ends_with('"') && asm_str.len() >= 2 {
            &asm_str[1..asm_str.len() - 1]
        } else {
            asm_str
        };
        // 先展开高级指令 (.rept/.macro/.if 等)
        let raw_lines: Vec<String> = s
            .split(['\n', ';'])
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        // 合并独立成行的前缀 (lock/rep/repz/repnz/repe) 与下一条指令
        let raw_lines = Self::merge_standalone_prefixes(raw_lines);
        let mut expander = AsmDirectiveExpander::new();
        let expanded_lines = expander.expand(&raw_lines);
        // 再解析操作数占位符
        let resolved_lines = AsmLocalLabels::expand_label_insn_lines(
            expanded_lines
                .iter()
                .map(|line| {
                    self.resolve_asm_template(
                        line.trim(),
                        outputs,
                        output_names,
                        inputs,
                        input_names,
                        &op_map,
                    )
                })
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>(),
        );
        // volatile asm: 完整屏障，防止优化器重排
        // 若代码尾部已是 mfence (前一 volatile asm 的后屏障), 跳过冗余前屏障
        if volatile {
            let code_len = self.code.len();
            let already_fenced = code_len >= 3
                && self.code[code_len - 3] == 0x0F
                && self.code[code_len - 2] == 0xAE
                && self.code[code_len - 1] == 0xF0;
            if !already_fenced {
                self.emit(&[0x0F, 0xAE, 0xF0]); // mfence
            }
        }
        // 记录 asm 块起始 PC (用于 DWARF 行号表)
        // Phase 5.2a：守护区间从这一行开始，弹回也算在内 —— 这几条指令就是「实现本条 asm
        // 的语义」所需的全部代码，行号归属应当跟着整块走。奇数个 push 会同时带走一次
        // 8 字节的对齐补偿（见 `asm_guard_enter`）。
        let guard = self.asm_clobber_held_regs(clobbers, &bindings);
        // `%rsp` 既守不了也不能当家用：先点名，再决定是否插守护（插了也守不住栈本身）。
        self.asm_reject_reserved_clobber(clobbers);
        let padded = self.asm_guard_enter(&guard);
        let asm_pc_start = self.code.len() as u64;
        let asm_src_line = self.current_asm_src_line;
        self.emit_resolved_asm_lines(&resolved_lines, false);
        self.asm_guard_leave(&guard, padded);
        // 记录 asm 块 DWARF 行号条目
        if self.debug_level > 0 && asm_src_line > 0 {
            let asm_pc_end = self.code.len() as u64;
            if asm_pc_end > asm_pc_start {
                self.asm_line_entries
                    .push((asm_pc_start, asm_pc_end, asm_src_line));
            }
        }
        for (i, binding) in bindings.iter().enumerate() {
            if binding.is_output && !binding.is_mem && i < outputs.len() {
                self.writeback_asm_operand(&outputs[i], binding.reg, binding.xmm_width);
            }
        }
        if clobbers.iter().any(|c| c == "memory") || volatile {
            // memory clobber 或 volatile: 编译器屏障，防止指令重排越过 asm
            self.emit(&[0x0F, 0xAE, 0xF0]); // mfence
        }
    }

    /// asm goto 专用：与 emit_inline_asm 相同，但输出写回仅 fall-through 路径生效。
    /// GCC 语义：asm goto 输出约束写回仅 fall-through 路径生效;
    /// 跳转分支保留原值。实现方式：asm 执行后直接写回输出;
    /// 若 asm 内部跳转到 goto label，控制流跳过写回代码，变量保持初值。
    pub fn emit_inline_asm_goto(
        &mut self,
        asm_str: &str,
        outputs: &[Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        clobbers: &[String],
    ) {
        let mut bindings = Vec::new();
        let op_map = self.build_asm_operand_map(
            outputs,
            output_names,
            output_constraints,
            inputs,
            input_names,
            input_constraints,
            clobbers,
            &mut bindings,
        );
        let s = if asm_str.starts_with('"') && asm_str.ends_with('"') && asm_str.len() >= 2 {
            &asm_str[1..asm_str.len() - 1]
        } else {
            asm_str
        };
        // 先展开高级指令 (.rept/.macro/.if 等)
        let raw_lines: Vec<String> = s
            .split(['\n', ';'])
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        // 合并独立成行的前缀 (lock/rep/repz/repnz/repe) 与下一条指令
        let raw_lines = Self::merge_standalone_prefixes(raw_lines);
        let mut expander = AsmDirectiveExpander::new();
        let expanded_lines = expander.expand(&raw_lines);
        // 再解析操作数占位符
        let resolved_lines = AsmLocalLabels::expand_label_insn_lines(
            expanded_lines
                .iter()
                .map(|line| {
                    self.resolve_asm_template(
                        line.trim(),
                        outputs,
                        output_names,
                        inputs,
                        input_names,
                        &op_map,
                    )
                })
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>(),
        );
        // 记录 asm goto 块起始 PC (用于 DWARF 行号表)
        // 这里**不**插守护：跳出去的那条路不经过恢复，那个 `pop` 可能永远不执行（%rsp 就此
        // 永久下推）。溅出槽已在函数入口被移出池，所以名单里剩下的只能是用户自己绑定的家，
        // 守不住就只能点名（E0317），不能静默给错值。判据与守护用的是同一份名单。
        let unguardable = self.asm_clobber_held_regs(clobbers, &bindings);
        self.asm_reject_reserved_clobber(clobbers);
        if !unguardable.is_empty() {
            self.asm_unguardable(&unguardable);
        }
        let asm_pc_start = self.code.len() as u64;
        let asm_src_line = self.current_asm_src_line;
        self.emit_resolved_asm_lines(&resolved_lines, false);
        // 记录 asm goto 块 DWARF 行号条目
        if self.debug_level > 0 && asm_src_line > 0 {
            let asm_pc_end = self.code.len() as u64;
            if asm_pc_end > asm_pc_start {
                self.asm_line_entries
                    .push((asm_pc_start, asm_pc_end, asm_src_line));
            }
        }
        // asm goto: fall-through 路径执行输出写回; 若 asm 内部跳转到 goto label,
        // 控制流跳过此处的写回代码，变量保持初值 (GCC 语义)。
        for (i, binding) in bindings.iter().enumerate() {
            if binding.is_output && !binding.is_mem && i < outputs.len() {
                self.writeback_asm_operand(&outputs[i], binding.reg, binding.xmm_width);
            }
        }
        if clobbers.iter().any(|c| c == "memory") {
            self.emit(&[0x0F, 0xAE, 0xF0]); // mfence
        }
    }
}
