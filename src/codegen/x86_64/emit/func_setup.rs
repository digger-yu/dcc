// src/codegen/x86_64/emit/func_setup.rs
// Per-function setup helpers: frame bookkeeping, local analysis walks and
// small register/stack helpers used by compile_func. Manually migrated from
// the pre-refactor monolithic x86_64.rs (commit 2ff718b) during solution v2
// phase 0.

use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::x86_64::abi::layout::LayoutMap;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::Codegen;
use std::collections::HashMap;

/// 用户写了不是寄存器的寄存器名（与 `report_internal_bug` 不同：这是**用户代码错误**，
/// 不是后端不变量破坏）。
const DIAG_BAD_REG_NAME: &str = "E0316";

/// 发射期才登记的那族帧内对象的**需求清单**（§11.23 步 2）。
///
/// 四族的名字都要到发射期才知道：`__compound_lit_<pos>` 带发射序、`__vmsize_<key>` 带类型
/// 标签、`va_start` 的 ap 槽带实参名、`<agg_member_tmp>` 是发射器自己的暂存名 —— 它们都不在
/// `pre_register_vars` 能预知的名字集合里，但与局部变量共用**同一条**向下生长的变量区水位。
///
/// 为什么这份清单现在是「可枚举」的：`var_offset` 的懒注册兜底已被 `E1134` 封死（源码里来的
/// 名字必须在规划期登记完），剩下的新名字 `register_var` 就只有这四族，而每一族都住在源码
/// AST 的某个站点上 ⇒ 上界可以静态算准；算少了就是 `E1133`（账面超出）。
#[derive(Default)]
pub struct LateNeed {
    /// 每个复合字面量站点一份（名字含发射序 ⇒ 站点之间不复用槽）。
    compound_slots: i64,
    /// VM 记录类型的尺寸元信息键（发射期按 `struct:<tag>` / `union:<tag>` 复用一槽）。
    vmsize_keys: Vec<String>,
    /// `va_start` 的 ap 候选名（按名字复用；全局 ap 在发射点走 rip 相对，不占帧槽）。
    ap_names: Vec<String>,
    /// 聚合成员赋值的一般 RHS 暂存（固定名，全函数共用一槽）。
    agg_tmp: bool,
}

impl LateNeed {
    /// 按 `gen_vm_type_bind` 的口径登记一个记录类型（两条判据都住在
    /// `Codegen::vm_size_key` / `Codegen::vm_record_needs_size_slot`，本函数不重答）。
    fn push_vmsize(&mut self, ty: &Type) {
        let Some(key) = Codegen::vm_size_key(ty) else {
            return;
        };
        if !Codegen::vm_record_needs_size_slot(ty) {
            return;
        }
        // 发射期同一个 key 只对应一槽（`vm_struct_size_slots` 命中就复用），所以这里去重
        let already = self.vmsize_keys.iter().any(|k| k == &key);
        if !already {
            self.vmsize_keys.push(key);
        }
    }

    /// 块级 struct/union 定义的那次绑定（`gen_vm_struct_bind` 把 `StructDef` 塑成
    /// `Type::Struct(名字, 字段, 对齐, 非 packed)` 再走同一个入口），键拼法与那里一致。
    fn push_vmsize_def(&mut self, tag: &str, fields: &[crate::ast::Field], is_union: bool) {
        let prefix = if is_union { "union" } else { "struct" };
        if !fields.iter().any(|f| f.ty.has_vla()) {
            return;
        }
        let key = format!("{}:{}", prefix, tag);
        if !self.vmsize_keys.iter().any(|k| k == &key) {
            self.vmsize_keys.push(key);
        }
    }

    /// `va_start` 的 ap 候选名（发射期按名字复用，多个 `va_start(ap,…)` 只一槽）。
    fn push_ap(&mut self, name: String) {
        if !self.ap_names.iter().any(|n| n == &name) {
            self.ap_names.push(name);
        }
    }
}

impl Codegen {
    pub fn reset_reg_tracker(&mut self) {
        // 占用真相只有一份：清的是 `RegBank` 里的两个位图与钉住集合。
        self.reg_bank.reset();
        // file-scope 的 `register asm` 绑定在 compile_func 开头就已经写进
        // reg_bindings, 而上面一行把占用清空。不重新 claim 就等于宣告
        // 「本函数不使用这些寄存器」, 序言里对应的保存会被 patch_callee_saves
        // 改成 nop —— 调用者存放在里面的值就此消失 (本地声明那条路径在绑定处
        // 当场 claim, 因为它的时机晚于本函数)。
        let bound: Vec<u8> = self
            .homes
            .values()
            .filter_map(|(l, _)| l.reg())
            .map(|r| r.n)
            .collect();
        for reg in bound {
            self.reg_bank.claim(reg);
        }
    }

    /// P12 Phase 5.2a：把「本函数里能把控制流带出保存/恢复区间的 asm goto」clobber 掉的
    /// 池号从临时值发号器里拿掉。
    ///
    /// 为什么不能只在发射点处理：守护是一对 `push`/`pop`，而 `asm goto` 的跳出路径上不
    /// 经过那个 `pop`。所以必须在函数入口就知道这件事（H1），不然第一个表达式已经把值
    /// 放进了那个号。
    ///
    /// 判据本身只有一份（`crate::ast_asm::escaping_asm_clobbers`）。分配器这一侧今天**不需要**
    /// 再读它：带 `Label`/`AsmGoto` 的函数根本进不了 SSA（`ssa::structure::is_structured`），
    /// 所以一个号也不会被自动绑到这些号上 —— 那条前提由用例
    /// `ssa::regalloc::tests::escaping_asm_functions_do_not_reach_the_allocator` 钉住；
    /// 它一变红，补判据的位置就只可能在本函数与 `plan_for` 这两处。两处各自写一遍
    /// 遍历就会漂移，而漂移的方向就是静默错值。
    pub fn ban_escaping_asm_regs(&mut self, body: Option<&crate::ast::Block>) {
        let Some(b) = body else { return };
        for name in crate::ast_asm::escaping_asm_clobbers(b) {
            // 名字→号走本后端已有的那一份解析（`register asm` 与 clobber 同一口径）；
            // `ban` 对池外的号（caller-saved）直接返 false，所以 `"rcx"` 这种最常见的
            // clobber 不会改变本函数能用的溅出号。
            if let Some(reg) = Self::parse_clobber_reg_name(&name) {
                self.reg_bank.ban(reg);
            }
        }
    }

    pub fn stack_reserve_bytes(&self) -> i64 {
        // 向量/bitcast 临时槽需要 3 个 16 字节槽: slot0 是
        // vector 结果约定, slot1/2 用于同尺寸重解释 cast。
        let vector_scratch = 48;
        if self.target.red_zone {
            vector_scratch
        } else {
            128 + vector_scratch
        }
    }

    /// 用户写的寄存器文本名 → 号。名字表只有一份（`abi/regnames.rs`）：本函数以前是
    /// 三张同事实表里的那一张（它额外认识 APX r16-r31，并接受 `%` 前缀与大写），
    /// 现在保留同样的接受口径，只把「哪个名字是哪个号」的事实交给唯一名表。
    /// 接受集的分寸（哪些写法有 `%` 形态）由表里的 `percent_ok` 逐条描述，
    /// 所以合并不扩大也不缩小语言。
    pub fn parse_reg_name(name: &str) -> Option<u8> {
        crate::codegen::x86_64::abi::regnames::lookup_reg_name(
            name,
            &crate::codegen::x86_64::abi::regnames::NAME_FORMS_USER,
        )
        .map(|e| e.num)
    }

    /// clobber 名单里的文本名 → 号。与上面同一张表，**只差 APX 号**：`-mapxf` 下
    /// gcc 确实接受 `::: "r16"`，而 `register asm("r16")` 它是硬错误（全局寄存器
    /// 变量不能占 REX2 编码的号）。两者共用一个口径必有一侧与 gcc 相反，
    /// 所以 `ban_escaping_asm_regs` 走本函数。
    pub fn parse_clobber_reg_name(name: &str) -> Option<u8> {
        crate::codegen::x86_64::abi::regnames::lookup_reg_name(
            name,
            &crate::codegen::x86_64::abi::regnames::NAME_FORMS_CLOBBER,
        )
        .map(|e| e.num)
    }

    /// 文件作用域 `register asm` 绑到「会被调用毁掉」的寄存器时的警告码。与
    /// `DIAG_RESERVED_REG` 同一家族（031x = 寄存器名/绑定），但不占 E0317：那是一个
    /// 号码本身在本 target 不存在的错误，不是一句「能不能存住值」的告警。
    pub(crate) const DIAG_CALL_CLOBBERED_GLOBAL_BINDING: &'static str = "W0319";

    /// 文件作用域的 `register asm` 绑定如果落在一个会被调用毁掉的寄存器上，按 gcc 的
    /// 原话警告（Phase 5.1 前置三的第一块：这个事实以前只有编译器知道）。
    ///
    /// 为什么不保存、也不升成错误：gcc 就是这个形状（警告完照样把值放在那个寄存器里，
    /// 调用会毁掉它）。实测本端 -O0 的产物与 gcc 逐条一致（`mov %rax,%r11` … `call sink`
    /// … `mov %r11,%rax`），缺的只有这句话。
    ///
    /// **只适用于文件作用域**：函数内的 `register asm` 在 gcc 文档里明说不保证跨调用存活，
    /// 实测 gcc 对零诊断；本函数也只由 `compile_rel` 里那个文件作用域登记点调用。
    /// 消息正文与 gcc 逐字相同（用例可以直接 `contains` 两侧输出），变量名与号进 hint。
    pub(crate) fn diag_call_clobbered_global_binding(
        &mut self,
        v: &VarDecl,
        home: &crate::location::Location,
    ) {
        let Some(reg) = home.reg() else { return };
        if !crate::codegen::x86_64::abi::regbank::is_call_clobbered(reg.n) {
            return;
        }
        // Phase 3.2: 文本从家反查（`gp_name_of_num` 是号→名唯一入口），不再独立读
        // `register_name` —— 与判据同源。
        let text = crate::codegen::x86_64::abi::regnames::gp_name_of_num(reg.n).unwrap_or("");
        self.diagnostics.push(crate::diagnostics::Diagnostic {
            sev: crate::diagnostics::Severity::Warning,
            code: Self::DIAG_CALL_CLOBBERED_GLOBAL_BINDING,
            msg: "call-clobbered register used for global register variable".to_string(),
            span: v.span.clone(),
            hint: Some(format!(
                "'{name}' 绑在 `{text}` 上，而它不是 callee-saved：任何函数调用都会毁掉它。\
                 要跨调用保活就换 rbx/r12-r15，或改回普通全局变量",
                name = v.name.name
            )),
            source_line: None,
        });
    }

    /// 回填本函数序言/尾声的 callee-saved 保存/恢复指令.
    /// 单遍编译下序言先于函数体发射, 无法预知函数体是否使用这些寄存器;
    /// 函数体生成完毕后 `RegBank::used_mask` 已完整, 将未使用寄存器的
    /// 4 字节 mov 改写为 4×0x90 (nop), 消除叶子函数的冗余保存/恢复.
    /// 尾声可能在每个 return 处内联发射, 统一在此处回填, 不依赖生成顺序.
    /// sites_base: compile_func 开头记录的 sites 起始下标 (嵌套函数隔离).
    pub fn patch_callee_saves(&mut self, sites_base: usize) {
        let used = self.reg_bank.used_mask();
        for (pos, bit) in self.callee_save_patch_sites.drain(sites_base..) {
            if used & bit == 0 {
                let p = pos as usize;
                self.code[p..p + 4].fill(0x90);
            }
        }
    }

    pub fn emit_store_rax_r13(&mut self, off: i64) {
        self.store_stack(0, off);
    }

    /// 用户文本名 → 家（后端事实：名字表只有 `parse_reg_name` 这一份）。
    /// 名字不是本目标已知的寄存器 ⇒ **报错**，而不是静默不绑。
    ///
    /// 实测过的静默后果（P12 Phase 4.1d 要消除的缺陷）：
    /// `register long gv asm("nosuchreg");` 在改前退出码 0，而产物里 `gv` 出现 **0 次** ——
    /// 全局变量因收集阶段 `continue` 不进 .data/.bss，又因 `rel_symbols` 判
    /// `register_name.is_some()` 不发符号，于是这个对象**整个消失**；而 gcc 在这里是
    /// `error: invalid register name for ‘gv’`。「一个名字没认出来」不可能比「声明不存在」
    /// 更接近用户意图，所以只能报错。
    pub(crate) fn resolve_asm_reg(&mut self, v: &VarDecl) -> Option<crate::location::Location> {
        let text = v.register_name.as_deref()?;
        if let Some(loc) = Self::parse_reg_home(text) {
            // 名字认得，但这个号是本端自己的帧基座：拿它当家会让所有 `%r13+disp` 寻址
            // 一起失效。而今天它不报错：实测同一个 `register long v asm("r13")` 在 -O0
            // 真发出 `mov %rax,%r13`，在 -O2 则被静默降级成栈槽（`mov %rax,-0x48(%r13)`）
            // —— 同一份源两个档位两个答案，那比报错坏得多。
            if let Some(why) = self.reserved_reg_reason(&loc) {
                self.diag_reserved_reg(&v.name.name, text, why, &v.span);
                return None;
            }
            return Some(loc);
        }
        self.diagnostics.push(crate::diagnostics::Diagnostic {
            sev: crate::diagnostics::Severity::Error,
            code: DIAG_BAD_REG_NAME,
            msg: format!("invalid register name for '{}': '{}'", v.name.name, text),
            span: v.span.clone(),
            hint: Some("register asm(\"rN\") 里的 N 必须是本目标已知的寄存器名".to_string()),
            source_line: None,
        });
        None
    }

    /// 保留号（栈指针 / 帧基址 / 帧指针）不得当变量之家：诊断码也只定在这里一处
    /// （`asm/emit.rs` 的 clobber 侧引用同一个常量）。
    pub(crate) const DIAG_RESERVED_REG: &'static str = "E0318";

    /// 这个家落在的号是不是本端保留号（帧基址 / 帧指针）。
    ///
    /// **只拒这两个，不拒 `%rsp`**：内核与 libm 依赖的
    /// `register unsigned long current_stack_pointer asm("rsp")` 是一个合法且常见的
    /// 「只读别名」（本轮实测：把它拒了直接弄坏 `parallel_codegen_keeps_file_scope_register_asm_binding`）。
    /// 家为 `Reg(4)` 时读侧发的就是 `mov %rsp,%rax`，正是用户要的东西；写侧也是用户自己要求的。
    /// 而 `%r13`/`%rbp` 不一样：它们是本端自己用来定位帧的，被当家用会让所有
    /// `%r13+disp` 寻址一起失效（或在另一个档位被静默降级）。
    /// clobber 侧的 `%rsp` 仍被拒（它守无可守），见 `asm/emit.rs::asm_reject_reserved_clobber`。
    fn reserved_reg_reason(&self, loc: &crate::location::Location) -> Option<&'static str> {
        let n = loc.reg()?.n;
        match n {
            13 => Some("%r13 是本后端的帧基址：所有局部量都按 `%r13+disp` 寻址"),
            5 if self.target.frame_pointer => {
                Some("%rbp 在 -fno-omit-frame-pointer 下是帧指针（序言 push、尾声 pop）")
            },
            _ => None,
        }
    }

    fn diag_reserved_reg(
        &mut self,
        name: &str,
        text: &str,
        why: &str,
        span: &crate::diagnostics::Span,
    ) {
        use crate::diagnostics::{Diagnostic, Severity};
        self.diagnostics.push(Diagnostic {
            sev: Severity::Error,
            code: Self::DIAG_RESERVED_REG,
            msg: format!("register '{}': '{}' 是保留寄存器 —— {}", name, text, why),
            span: span.clone(),
            hint: Some(
                "本后端可当变量之家的 callee-saved 号只有 rbx/r12/r14/r15（%r13 是帧基址、\n\
                 %rbp 在 -fno-omit-frame-pointer 下是帧指针，都不行；%rsp 当只读别名是可以的）"
                    .to_string(),
            ),
            source_line: None,
        });
    }

    /// 变量声明处的**家**写入的唯一入口（P12 Phase 4.1c）。
    ///
    /// 为什么要有它：同一件事（「这个声明要住哪个寄存器」）之前在 `compile_func` 与
    /// `gen_block_items` 各有一份拷贝（两处字面同形、改一处漏一处的形状）；而 §7.15/§7.19
    /// 反复实测到的结论是「消费者没读同一张表」就是静默错值的来源。家在这里只有一个
    /// 算式，插入后必然同步 `RegBank` 与钉住集合。
    ///
    /// 两个来源的优先级与今天一致：**用户写的文本名 > IR 位置表给出的分配结果**
    /// （`apply_bindings` 本来就不会覆盖已有的 `register_name`，而自动分配把
    /// `already-register-asm` 当拒因，所以两者不会同时出现在同一个变量上）。
    /// 返回是否真的绑上了。
    pub fn bind_decl_home(&mut self, v: &VarDecl) -> bool {
        // 地址被取走的对象不得绑到物理寄存器：绑定后那个栈槽再没人写过，`&v` 的读者
        // 拿到旧值/垃圾（GCC 遇到逸出同样是忽略绑定，而规划期总是先分好了槽，
        // 退回内存路径无风险）。
        if !self.register_binding_allowed(&v.name.name) {
            return false;
        }
        let home = match &v.register_name {
            // 用户文本名：非法就报错（见 `resolve_asm_reg`），不静默降级成内存变量。
            Some(_) => self.resolve_asm_reg(v),
            // 自动分配的结果：已是 `Location`，不再走名字这一圈。
            None => v.home,
        };
        // 「家从哪来」才是保存通道的判据，而不是「有没有寄存器名」：`apply_bindings` 为了
        // 与旧代码同源，会给自动绑定也补写一个 `register_name` 文本（只在原本为 None 时写），
        // 所以名字那一位区分不出两者。而 `home` 只有一个写入点（分配器），用户显式绑定的
        // `home` 恒为 None —— 于是 `home.is_some()` 就是「这是分配器选的」。把这条记到
        // `explicit` 里，而不是在下一次改动时重新推一遍。
        let explicit = v.home.is_none();
        match home {
            Some(loc) => {
                self.insert_home(&v.name.name, loc, v.ty.clone(), explicit);
                true
            },
            None => false,
        }
    }

    /// 登记一个家，并同步占用事实。**只给声明处用**：file-scope 表 (`global_homes`) 的
    /// 重填走直接 `insert`，因为那条路径上旧代码不在这里 claim（它在 `reset_reg_tracker`
    /// 里做），而 `naked` 函数根本不会走到 reset —— 改在这里 claim 就是新增行为。
    /// 登记一个家，并同步占用事实。**只给声明处用**：file-scope 表 (`global_homes`) 的
    /// 重填走直接 `insert`，因为那条路径上旧代码不在这里 claim（它在 `reset_reg_tracker`
    /// 里做），而 `naked` 函数根本不会走到 reset —— 改在这里 claim 就是新增行为。
    ///
    /// `explicit` 区分「用户写了 `register asm(...)`」与「分配器选的」（调用方知道，
    /// 它就是从那个字段分流的），因为两者的跳 call 语义不同：前者「会被调用毁掉」是
    /// GNU C 明文语义（实测 gcc/dcc 两侧同值），后者是优化，必须对程序正确。
    pub(super) fn insert_home(
        &mut self,
        name: &str,
        loc: crate::location::Location,
        ty: Type,
        explicit: bool,
    ) {
        self.homes.insert(name.to_string(), (loc, ty.clone()));
        // 存/取窗口是按 8 字节搬的（`store_stack`/`load_stack`），所以宽于 8 字节的对象
        // 不能拿 caller-saved 家：只能存下半就会丢高半。这类家退回到内存，而不是
        // 默默存一半（错值比少一个寄存器难得多，也检不出来）。
        let wide_ext_home = !explicit
            && loc
                .reg()
                .is_some_and(|r| crate::ssa::regalloc::is_auto_ext_reg(r.n))
            && ty.size() > 8;
        let loc = if wide_ext_home {
            crate::location::Location::Stack(self.register_var(name, &ty))
        } else {
            loc
        };
        self.homes.insert(name.to_string(), (loc, ty.clone()));
        if let Some(r) = loc.reg() {
            // 绑定即占用该寄存器：必须保存 + 不得再当临时值发出（见 `RegBank::claim`）
            self.reg_bank.claim(r.n);
            // 住在 caller-saved 号里的**自动**家需要一个帧内保存槽（前置三）。
            if !explicit && crate::ssa::regalloc::is_auto_ext_reg(r.n) {
                let slot_name = format!("<{}#save>", name);
                let slot_off = self.register_var(&slot_name, &ty);
                self.ext_home_saves
                    .push(crate::codegen::x86_64::codegen::ExtHomeSave {
                        name: name.to_string(),
                        reg: r.n,
                        slot_off,
                        size: ty.size(),
                    });
            }
        }
    }

    /// 用户文本名 → 家（后端事实：名字表只有 `parse_reg_name` 这一份）。
    pub fn parse_reg_home(name: &str) -> Option<crate::location::Location> {
        Self::parse_reg_name(name).map(|n| {
            crate::location::Location::Reg(crate::location::RegId {
                bank: crate::location::RegBank::Gp,
                n,
            })
        })
    }

    pub fn ends_with_return(items: &[BlockItem]) -> bool {
        match items.last() {
            Some(BlockItem::Stmt(Stmt::Return { .. })) => true,
            Some(BlockItem::Stmt(Stmt::Break { .. })) => true,
            Some(BlockItem::Stmt(Stmt::Continue { .. })) => true,
            Some(BlockItem::Stmt(Stmt::Block { b, .. })) => Self::ends_with_return(&b.items),
            Some(BlockItem::Stmt(Stmt::If { then_, else_, .. })) => {
                Self::ends_with_return(&[BlockItem::Stmt(*then_.clone())])
                    && else_
                        .as_ref()
                        .is_some_and(|e| Self::ends_with_return(&[BlockItem::Stmt(*e.clone())]))
            },
            _ => false,
        }
    }

    /// Collect all label names defined in a statement tree.
    /// Used for non-local goto detection in nested functions.
    pub fn collect_local_labels_stmt(stmt: &Stmt, out: &mut std::collections::HashSet<String>) {
        match stmt {
            Stmt::Label { name, stmt, .. } => {
                out.insert(name.name.clone());
                Self::collect_local_labels_stmt(stmt, out);
            },
            // C++ 零成本异常: 落地垫标签同样是本函数内定义的标签
            // (`goto` 可以指向它), 必须计入 non-local goto 判定。
            Stmt::EhLandingPad { label, .. } => {
                out.insert(label.name.clone());
            },
            Stmt::Invoke {
                call, landing_pad, ..
            } => {
                // `landing_pad == None` 是 lp=0 的「本帧不接管」区间, 没有标签可登记。
                if let Some(pad) = landing_pad {
                    out.insert(pad.name.clone());
                }
                Self::collect_local_labels_stmt(call, out);
            },
            Stmt::Block { b, .. } => {
                for item in &b.items {
                    if let BlockItem::Stmt(s) = item {
                        Self::collect_local_labels_stmt(s, out);
                    }
                }
            },
            Stmt::If { then_, else_, .. } => {
                Self::collect_local_labels_stmt(then_, out);
                if let Some(e) = else_ {
                    Self::collect_local_labels_stmt(e, out);
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } => {
                Self::collect_local_labels_stmt(body, out);
            },
            Stmt::For { body, .. } => Self::collect_local_labels_stmt(body, out),
            Stmt::Switch { cases, .. } => {
                for c in cases {
                    for item in &c.body.items {
                        if let BlockItem::Stmt(s) = item {
                            Self::collect_local_labels_stmt(s, out);
                        }
                    }
                }
            },
            _ => {},
        }
    }

    // 扫描最大表达式嵌套深度 (binop/call 各+1)
    pub fn max_expr_depth_stmt(items: &[BlockItem]) -> u32 {
        items
            .iter()
            .map(|item| match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    v.init.as_ref().map_or(0, Self::max_expr_depth_expr)
                },
                BlockItem::Stmt(s) => Self::max_expr_depth_stmt_inner(s),
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }

    fn max_expr_depth_stmt_inner(s: &Stmt) -> u32 {
        match s {
            Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
                Self::max_expr_depth_expr(e)
            },
            Stmt::If {
                cond, then_, else_, ..
            } => {
                let d = Self::max_expr_depth_expr(cond);
                d.max(Self::max_expr_depth_stmt_inner(then_)).max(
                    else_
                        .as_ref()
                        .map_or(0, |e| Self::max_expr_depth_stmt_inner(e)),
                )
            },
            Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
                Self::max_expr_depth_expr(cond).max(Self::max_expr_depth_stmt_inner(body))
            },
            Stmt::For { cond, body, .. } => cond
                .as_ref()
                .map_or(0, Self::max_expr_depth_expr)
                .max(Self::max_expr_depth_stmt_inner(body)),
            Stmt::Block { b, .. } => Self::max_expr_depth_stmt(&b.items),
            _ => 0,
        }
    }

    fn max_expr_depth_expr(e: &Expr) -> u32 {
        match e {
            Expr::Binop { l, r, .. } => {
                1 + Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            },
            Expr::Call { callee, args, .. } => {
                1 + Self::max_expr_depth_expr(callee).max(
                    args.iter()
                        .map(Self::max_expr_depth_expr)
                        .max()
                        .unwrap_or(0),
                )
            },
            Expr::Comma { l, r, .. } => {
                Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            },
            Expr::Assign { l, r, .. } => {
                1 + Self::max_expr_depth_expr(l).max(Self::max_expr_depth_expr(r))
            },
            Expr::Unary { e, .. }
            | Expr::Cast { e, .. }
            | Expr::AddrOf { e, .. }
            | Expr::Deref { e, .. }
            | Expr::PostInc { e, .. }
            | Expr::PostDec { e, .. }
            | Expr::PreInc { e, .. }
            | Expr::PreDec { e, .. } => Self::max_expr_depth_expr(e),
            Expr::Index { arr, idx, .. } => {
                Self::max_expr_depth_expr(arr).max(Self::max_expr_depth_expr(idx))
            },
            Expr::Member { expr, .. } => Self::max_expr_depth_expr(expr),
            Expr::Cond {
                cond, then_, else_, ..
            } => Self::max_expr_depth_expr(cond)
                .max(Self::max_expr_depth_expr(then_))
                .max(Self::max_expr_depth_expr(else_)),
            _ => 0,
        }
    }

    // ========================================================================
    // sret / 物化槽字节水位静态扫描
    // ------------------------------------------------------------------------
    // 与运行时分配器 alloc_sret_slot 完全同口径: 每个槽按 16 字节对齐,
    // 自 sret_region_base 向下累计; 槽在其所属的“语句/块 item”边界死亡。
    // 扫描返回整函数任意执行路径上的最大同时存活字节水位, 供帧规划把
    // 槽区计入 frame_size (旧 bump 不做扫描, 槽位越过帧底覆盖 call
    // outgoing 区, cld 夹具在此段错误)。
    //
    // 建模规则 (必须与代码生成的 save/restore 挂钩点一一对应):
    //   * 一个 BlockItem 从 incoming 水位开始, 结束时回到 incoming —
    //     gen_block_items / 函数顶层循环在每个 item 边界 save/restore;
    //   * 单个表达式树内部不释放 (兄弟子表达式也不回收), 故表达式的
    //     “本树新增字节”对整棵子树求和 (过近似, 安全);
    //   * 控制流语句的条件用 incoming 求值; 每个体语句从其自己的
    //     incoming 起算 (体在 gen_stmt 外层 save/restore 内, 且 switch
    //     等的体可能复用外层为条件分配的槽);
    //   * switch 表达式分配的 16+8 槽存活至全部 case 体结束 — 取
    //     (24 + 体内最大水位)。
    pub fn max_sret_bytes(items: &[BlockItem]) -> i64 {
        items
            .iter()
            .map(Self::sret_bytes_block_item)
            .max()
            .unwrap_or(0)
    }

    fn sret_bytes_block_item(item: &BlockItem) -> i64 {
        match item {
            BlockItem::Decl(Decl::Var(v)) => v.init.as_ref().map_or(0, Self::sret_bytes_expr),
            BlockItem::Stmt(s) => Self::sret_bytes_stmt(s),
            _ => 0,
        }
    }

    fn sret_bytes_stmt(s: &Stmt) -> i64 {
        match s {
            Stmt::Expr { e, .. } => Self::sret_bytes_expr(e),
            Stmt::Return { val: Some(e), .. } => Self::sret_bytes_expr(e),
            Stmt::If {
                cond, then_, else_, ..
            } => Self::sret_bytes_expr(cond)
                .max(Self::sret_bytes_stmt(then_))
                .max(else_.as_ref().map_or(0, |e| Self::sret_bytes_stmt(e))),
            Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
                Self::sret_bytes_expr(cond).max(Self::sret_bytes_stmt(body))
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => init
                .iter()
                .map(|s| Self::sret_bytes_stmt(s))
                .chain(cond.iter().map(Self::sret_bytes_expr))
                .chain(step.iter().map(Self::sret_bytes_expr))
                .max()
                .unwrap_or(0)
                .max(Self::sret_bytes_stmt(body)),
            Stmt::Block { b, .. } => Self::max_sret_bytes(&b.items),
            Stmt::Switch { expr, cases, .. } => {
                // expr 槽 (16+8) 存活至所有 case 体结束
                let base = Self::sret_bytes_expr(expr) + 24;
                cases
                    .iter()
                    .flat_map(|c| c.body.items.iter())
                    .map(Self::sret_bytes_block_item)
                    .max()
                    .unwrap_or(0)
                    .max(base)
            },
            Stmt::GotoIndirect { expr, .. } => Self::sret_bytes_expr(expr),
            _ => 0,
        }
    }

    /// 表达式树在一次求值内“新增的 sret/物化字节”峰值。按当前代码生成,
    /// 表达式求值期间不释放任何槽, 故对各分配站点与其子树求和。
    fn sret_bytes_expr(e: &Expr) -> i64 {
        let own = match e {
            Expr::Call {
                ty, callee, args, ..
            } => {
                let sret = if Self::needs_memory_return(ty) {
                    (ty.size() as i64 + 15) & !15
                } else {
                    0
                };
                // _Complex long double 返回 (经 x87, needs_memory_return
                // == false) 仍在调用侧复制到固定 result_off, 不经过
                // alloc_sret_slot, 故不占本区。
                sret + std::iter::once(callee.as_ref())
                    .map(Self::sret_bytes_expr)
                    .chain(args.iter().map(Self::sret_bytes_expr))
                    .max()
                    .unwrap_or(0)
            },
            Expr::VaArg { ty, ap, .. } => {
                // 只有真正走 `alloc_sret_slot` 的缓冲区交付才占本区；复数
                // `va_arg` 直接落帧底固定的复数物化带 (与 `gen_expr_va_arg`
                // 同源，见 `va_arg_result_in_cx_band`)。
                let own =
                    if Self::va_arg_result_in_memory(ty) && !Self::va_arg_result_in_cx_band(ty) {
                        (ty.size() as i64 + 15) & !15
                    } else {
                        0
                    };
                own + Self::sret_bytes_expr(ap)
            },
            Expr::Binop { l, r, .. } => {
                // _Complex long double 二元: 两个 32B 物化槽同时存活
                // (spill l, spill r), 结果走固定 result_off 不占本区。
                Self::complex_ld_sret_binop(e)
                    + Self::sret_bytes_expr(l).max(Self::sret_bytes_expr(r))
            },
            Expr::Assign { l, r, .. } => Self::sret_bytes_expr(l) + Self::sret_bytes_expr(r),
            Expr::StmtExpr { block, ty, .. } => {
                // 聚合 StmtExpr 分配一个类型槽; 内部块另算
                let own = if ty.size() > 0 {
                    (ty.size() as i64 + 15) & !15
                } else {
                    0
                };
                own + Self::max_sret_bytes(&block.items)
            },
            Expr::Comma { l, r, .. } => Self::sret_bytes_expr(l).max(Self::sret_bytes_expr(r)),
            Expr::Unary { e: inner, .. }
            | Expr::Cast { e: inner, .. }
            | Expr::AddrOf { e: inner, .. }
            | Expr::Deref { e: inner, .. }
            | Expr::PostInc { e: inner, .. }
            | Expr::PostDec { e: inner, .. }
            | Expr::PreInc { e: inner, .. }
            | Expr::PreDec { e: inner, .. } => Self::sret_bytes_expr(inner),
            Expr::Index { arr, idx, .. } => {
                Self::sret_bytes_expr(arr).max(Self::sret_bytes_expr(idx))
            },
            Expr::Member { expr, .. } => Self::sret_bytes_expr(expr),
            Expr::Cond {
                cond,
                then_,
                else_,
                ty,
                ..
            } => {
                // 聚合三元 (`c ? s1 : s2`) 在物化带里占一个类型宽度的临时槽:
                // 两支各自整块拷进该槽, 交付的是槽地址 (`gen_expr_cond`)。
                let own = if Self::value_is_whole_copy(ty)
                    && matches!(ty, Type::Struct(..) | Type::Union(..))
                {
                    (ty.size() as i64 + 15) & !15
                } else {
                    0
                };
                own + Self::sret_bytes_expr(cond)
                    .max(Self::sret_bytes_expr(then_))
                    .max(Self::sret_bytes_expr(else_))
            },
            _ => 0,
        };
        own
    }

    /// 若表达式是 `_Complex long double` 二元 (含一元负/conj 等物化点),
    /// 返回其直接物化槽字节数; 否则 0。
    fn complex_ld_sret_binop(e: &Expr) -> i64 {
        let is_cld = |ty: &Type| {
            matches!(
                ty,
                Type::Complex(q) if matches!(q.base, Type::LongDouble)
            )
        };
        match e {
            Expr::Binop { ty, .. } if is_cld(ty) => 64, // 两个 32B 物化槽
            Expr::Unary { ty, .. } if is_cld(ty) => 32,
            _ => 0,
        }
    }

    /// 单个变量声明占用的 8 字节栈槽数 (分配口径的单一真源)。
    /// VLA 追加 16 字节元信息槽 (基址 + 运行时行大小 + 声明时总大小)。
    /// pre_register_vars 与 register_var 必须共用本函数, 否则两处对
    /// “变量占多大槽”的理解会漂移。
    pub fn var_alloc_slots(ty: &Type) -> i64 {
        let alloc_sz = if Self::type_has_vla(ty) {
            ty.size().saturating_add(16)
        } else {
            ty.size()
        };
        ((alloc_sz.saturating_add(7) / 8).max(1)) as i64
    }

    /// 扫一个函数体，得到「发射期才会冒出来的帧内对象」的需求清单（`LateNeed`）。
    /// 不下降进嵌套函数体（那是它自己一帧的事，与 `collect_frame_var_decls` 同口径）。
    pub fn late_need(items: &[BlockItem]) -> LateNeed {
        let mut need = LateNeed::default();
        Self::late_need_block(items, &mut need);
        need
    }

    fn late_need_block(items: &[BlockItem], need: &mut LateNeed) {
        for item in items {
            match item {
                BlockItem::Decl(d) => Self::late_need_decl(d, need),
                BlockItem::Stmt(s) => Self::late_need_stmt(s, need),
            }
        }
    }

    /// 穷举写：新增 `Decl` 变体会直接编不过，而不是静默少扫一棵子树（§11.23 当年
    /// 就是被「静默少扫」坑的：扫描没数到的那一族到发射期就把预算吃穿了）。
    fn late_need_decl(d: &Decl, need: &mut LateNeed) {
        match d {
            Decl::Var(v) => {
                // 定义+声明一体的局部记录类型也会走一次 `gen_vm_type_bind`
                // (`emit/functions.rs` / `emit/stmt.rs` / 语句表达式那份)
                need.push_vmsize(&v.ty);
                Self::late_need_type(&v.ty, need);
                if let Some(e) = &v.init {
                    Self::late_need_expr(e, need);
                }
            },
            // 块级 struct/union 定义：`gen_vm_struct_bind` 同样按 tag 登一个尺寸槽
            Decl::StructDef(sd) => need.push_vmsize_def(&sd.name.name, &sd.fields, false),
            Decl::UnionDef(sd) => need.push_vmsize_def(&sd.name.name, &sd.fields, true),
            // typedef 不发射运行时代码（`typedef int arr[n];` 的维度也没人求值）；
            // 嵌套函数自己一帧；函数声明不携带站点。
            Decl::Typedef { .. } | Decl::Func(_) | Decl::EnumDef(_) => {},
            // 文件作用域 asm（块级也能写）的操作数是普通表达式，里面有复合字面量与
            // `va_start` 的可能。
            Decl::GlobalAsm {
                outputs, inputs, ..
            } => {
                for e in outputs.iter().chain(inputs.iter()) {
                    Self::late_need_expr(e, need);
                }
            },
        }
    }

    /// 类型树里携带的运行时表达式（只有一种形态：VLA 维度 `ArraySize::Var`）也要扫：
    /// 它们是序言/声明点用 `gen_expr` 求值的，里面可以有复合字面量与聚合成员赋值。
    fn late_need_type(ty: &Type, need: &mut LateNeed) {
        match ty {
            Type::Ptr(q)
            | Type::Complex(q)
            | Type::Imaginary(q)
            | Type::Vector(q, _)
            | Type::Atomic(q) => Self::late_need_type(&q.base, need),
            Type::Array(elem, ArraySize::Var(se)) => {
                Self::late_need_expr(se, need);
                Self::late_need_type(&elem.base, need);
            },
            Type::Array(elem, _) => Self::late_need_type(&elem.base, need),
            Type::Func(ret, params) => {
                Self::late_need_type(&ret.base, need);
                for p in params {
                    Self::late_need_type(&p.base, need);
                }
            },
            Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => {
                for f in fields.iter() {
                    Self::late_need_type(&f.ty, need);
                }
            },
            _ => {},
        }
    }

    /// 与 `max_sret_bytes` 同骨架的语句遍历（那条轴算 sret 区，本条算变量区晚分配）。
    /// 穷举到每一条臂：`Invoke` 的受保护调用体、`EhLandingPad` 的两个接收者、标号体、
    /// `asm goto` 的操作数里全可以有复合字面量与聚合成员赋值。
    fn late_need_stmt(s: &Stmt, need: &mut LateNeed) {
        match s {
            Stmt::Expr { e, .. } => Self::late_need_expr(e, need),
            Stmt::Return { val, .. } => {
                if let Some(e) = val {
                    Self::late_need_expr(e, need);
                }
            },
            Stmt::Block { b, .. } => Self::late_need_block(&b.items, need),
            Stmt::If {
                cond, then_, else_, ..
            } => {
                Self::late_need_expr(cond, need);
                Self::late_need_stmt(then_, need);
                if let Some(e) = else_ {
                    Self::late_need_stmt(e, need);
                }
            },
            Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
                Self::late_need_expr(cond, need);
                Self::late_need_stmt(body, need);
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                if let Some(i) = init {
                    Self::late_need_stmt(i, need);
                }
                if let Some(c) = cond {
                    Self::late_need_expr(c, need);
                }
                if let Some(st) = step {
                    Self::late_need_expr(st, need);
                }
                Self::late_need_stmt(body, need);
            },
            Stmt::Switch { expr, cases, .. } => {
                Self::late_need_expr(expr, need);
                for c in cases {
                    Self::late_need_block(&c.body.items, need);
                }
            },
            Stmt::Label { stmt, .. } => Self::late_need_stmt(stmt, need),
            Stmt::GotoIndirect { expr, .. } => Self::late_need_expr(expr, need),
            Stmt::AsmGoto {
                outputs, inputs, ..
            } => {
                for e in outputs.iter().chain(inputs.iter()) {
                    Self::late_need_expr(e, need);
                }
            },
            Stmt::Invoke { call, .. } => Self::late_need_stmt(call, need),
            Stmt::EhLandingPad {
                object, selector, ..
            } => {
                Self::late_need_expr(object, need);
                Self::late_need_expr(selector, need);
            },
            // 这四条不携带表达式
            Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Goto { .. }
            | Stmt::Fallthrough { .. } => {},
        }
    }

    /// 表达式树里的晚分配站点：
    /// - `CompoundLit` → 一个名字含发射序的对象，按 `var_alloc_slots(ty)` 预扣（向量形态例外：
    ///   `gen_expr_compound_lit` 对 `Type::Vector` 走 `vector_temp_base`，那不在变量区）；
    /// - `__builtin_va_start(ap, …)` → ap 的 24 字节 `va_list` 槽（按名字复用）；
    /// - 聚合成员赋值的一般 RHS → 全函数共用的 `<agg_member_tmp>`。
    /// 穷举写：新增 `Expr` 变体会直接编不过。
    fn late_need_expr(e: &Expr, need: &mut LateNeed) {
        use crate::ast::Expr as E;
        // 类型侧统一走一遍：结果类型自己就能携带 VLA 维度，逐变体重复一遍必漏。
        Self::late_need_type(&e.ty(), need);
        match e {
            E::Ident { .. }
            | E::Int { .. }
            | E::Float { .. }
            | E::ComplexLit { .. }
            | E::ComplexIntLit { .. }
            | E::String { .. }
            | E::Char { .. }
            | E::SizeofTy { .. }
            | E::AlignofTy { .. }
            | E::LabelAddr { .. }
            | E::VaArgPack { .. } => {},
            E::Unary { e: x, .. }
            | E::Cast { e: x, .. }
            | E::AddrOf { e: x, .. }
            | E::Deref { e: x, .. }
            | E::Real { e: x, .. }
            | E::Imag { e: x, .. }
            | E::SizeofExpr { e: x, .. }
            | E::AlignofExpr { e: x, .. }
            | E::VaArg { ap: x, .. }
            | E::Member { expr: x, .. }
            | E::PostInc { e: x, .. }
            | E::PostDec { e: x, .. }
            | E::PreInc { e: x, .. }
            | E::PreDec { e: x, .. } => Self::late_need_expr(x, need),
            E::Binop { l, r, .. } | E::Comma { l, r, .. } | E::Index { arr: l, idx: r, .. } => {
                Self::late_need_expr(l, need);
                Self::late_need_expr(r, need);
            },
            E::Assign { l, r, .. } => {
                if Self::agg_member_scratch_used(l, r) {
                    need.agg_tmp = true;
                }
                Self::late_need_expr(l, need);
                Self::late_need_expr(r, need);
            },
            E::Cond {
                cond, then_, else_, ..
            } => {
                Self::late_need_expr(cond, need);
                Self::late_need_expr(then_, need);
                Self::late_need_expr(else_, need);
            },
            E::Offsetof { indices, .. } => {
                for x in indices {
                    Self::late_need_expr(x, need);
                }
            },
            E::StmtExpr { block, .. } => Self::late_need_block(&block.items, need),
            E::AsmExpr {
                outputs, inputs, ..
            } => {
                for x in outputs.iter().chain(inputs.iter()) {
                    Self::late_need_expr(x, need);
                }
            },
            E::Call { callee, args, .. } => {
                // `va_start` 的 ap 槽：与 `special/builtins.rs` 同一组判据（callee 是那个
                // 内置名、首个实参是**数组类型的 Ident**；指针型 Ident 走 `emit_va_start_at`，
                // 不占帧槽）。全局 ap 发射期走 rip 相对（`emit_va_start` 里同一个
                // `is_global_not_local`），所以名单先拿着，到了 `reserve` 里再按同一个
                // 判据筛。
                if let (E::Ident { name: cn, .. }, Some(E::Ident { name: an, ty, .. })) =
                    (&**callee, args.first())
                {
                    if cn.name == "__builtin_va_start" && matches!(ty, Type::Array(_, _)) {
                        need.push_ap(an.name.clone());
                    }
                }
                Self::late_need_expr(callee, need);
                for a in args {
                    Self::late_need_expr(a, need);
                }
            },
            E::CompoundLit { ty, inits, .. } => {
                // 向量复合字面量不占变量区（走 `vector_temp_base`），其余一个站点一份
                if !matches!(ty, Type::Vector(_, _)) {
                    need.compound_slots += Self::var_alloc_slots(ty);
                }
                for it in inits {
                    match it {
                        InitItem::Expr(ex) => Self::late_need_expr(ex, need),
                        InitItem::Designated { value, .. } => Self::late_need_expr(value, need),
                    }
                }
            },
            E::InitList { inits, .. } => {
                for it in inits {
                    match it {
                        InitItem::Expr(ex) => Self::late_need_expr(ex, need),
                        InitItem::Designated { value, .. } => Self::late_need_expr(value, need),
                    }
                }
            },
        };
    }

    /// 发射点那条「聚合成员赋值 + 一般 RHS」分支在规划期的静态对应（判据见
    /// `emit/expr/dispatch.rs` 里写 `<agg_member_tmp>` 的那一段）：LHS 是成员访问、
    /// RHS 是聚合类型，且 RHS 不是那四条「直接拿地址」的形状。
    ///
    /// 宁多不少：多计只是多 16 字节，少计就是 `E1133`。因此这里把 `Type::Named`（还没
    /// 解开的 typedef）也算作聚合；发射点的判据看的是 LHS 字段类型，两者靠「RHS 与字段
    /// 类型在合法 C 里必须兼容」这条语言规则对齐。
    fn agg_member_scratch_used(l: &Expr, r: &Expr) -> bool {
        if !matches!(l, Expr::Member { .. }) {
            return false;
        }
        if matches!(
            r,
            Expr::Member { .. }
                | Expr::Index { .. }
                | Expr::Deref { .. }
                | Expr::Ident { .. }
                | Expr::CompoundLit { .. }
        ) {
            return false;
        }
        matches!(
            r.ty(),
            Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) | Type::Array(_, _) | Type::Named(_)
        )
    }

    /// VM 尺寸元信息槽的**键**（与 `emit/vla.rs::gen_vm_type_bind` 共用一份拼法：两处各写
    /// 一句 `format!` 就是两套事实，少一个前缀扫描就会漏计一槽）。返回 `Some` 只意味着
    /// 「这个类型有键」，不意味着要分配：字段里没 VLA 时发射点是 `remove`。
    pub fn vm_size_key(ty: &Type) -> Option<String> {
        match ty {
            Type::Struct(tag, _, _, false) => Some(format!("struct:{}", tag)),
            Type::Union(tag, _, _, false) => Some(format!("union:{}", tag)),
            _ => None,
        }
    }

    /// 该记录类型是否需要一个尺寸元信息槽（`gen_vm_type_bind` 的第二条判据：任一字段
    /// 带 VLA）。与上面同一个理由：口径只住一处。
    pub fn vm_record_needs_size_slot(ty: &Type) -> bool {
        match ty {
            Type::Struct(_, fields, _, false) | Type::Union(_, fields, _, false) => {
                fields.iter().any(|f| f.ty.has_vla())
            },
            _ => false,
        }
    }

    /// 帧规划前把「规划期看不见的运行期临时对象」算成晚分配带的尺寸，并把账面交出去。
    /// 两个帧规划分支 (带参数的普通函数 / `main`) 共用本函数, 避免其中一处
    /// 漏扣后又是一次“变量区压进 sret 区”的定向失效。
    ///
    /// 名不叫 `reserve_*`：这里**不预扣变量区水位**（§11.23 步 3 已把那两段删掉），
    /// 真正在做的只是「把晚分配带的槽数算出来，写进 `late_slot_budget`」这一件事，
    /// 再顺手把规划期看得见的自由 ident 登进变量区、把 `frame_planned` 置上作为发射期
    /// 晚分配的闸门。所有槽尺寸（`late_slots` / `spill_slots` / `temp_slots`）都在
    /// `LayoutMap::resolve` 的同一帧里定为输入，本函数只负责晚分配这一条带的输入。
    pub fn plan_late_slot_budget(&mut self, body: Option<&Block>, params: &[crate::ast::Param]) {
        let Some(b) = body else { return };
        // §11.23 步 2：预算 = 发射期那四族的静态需求。每一笔都能对应到一个具体站点，
        // 所以这里不是「宁可多给」而是「算准」（少算了 `E1133` 会响）。
        let need = Self::late_need(&b.items);
        let mut slots = need.compound_slots;
        // 每个 VM 尺寸键一份 `Type::Long`（发射点是 `register_var(_, &Type::Long)`）
        slots += (need.vmsize_keys.len() as i64).max(0);
        for ap in &need.ap_names {
            // 全局 ap 在发射点走 rip 相对真全局内存（`emit_va_start` 里同一个
            // `is_global_not_local` 分派），不占本帧槽 ⇒ 不计费。判据只读一处。
            if self.is_global_not_local(ap) {
                continue;
            }
            // 已经登过的 ap（`va_list ap;` 是一条普通局部声明，规划期已按 24B 给了槽）
            // 在发射点只会命中 var_map、不会新占一槽 ⇒ 不得重复计费。
            // 真正需要本项的是「直接把形参当 ap 用」那一类（它不在 var_map 里）。
            if self.var_map.contains_key(ap) {
                continue;
            }
            slots += Self::var_alloc_slots(&Self::va_list_slot_ty());
        }
        if need.agg_tmp {
            slots += Self::var_alloc_slots(&Type::Int128);
        }
        // §11.23 步 3：这份预算不再**预扣进变量区水位**，而是作为晚分配带的尺寸交给
        // 布局解算（`FramePlan::late_slots`）。带里的对象不再挤在局部变量底下，于是
        // `register_var` 里那两段只在生成期推帧坐标的代码（`min_used` 夹逼 +
        // 「晚分配压低 `sret_region_base` 锚点」）一并删除。
        // 延伸池家的保存槽（Phase 5.1 前置三）**也必须算进这份预算**：`insert_home` 会给每个
        // 住在 caller-saved 号里的自动绑定家申请一个 `<名#save>` 槽，预算要是不知道它，
        // 发射期第一次用到就撞 `E1133`（实测：`register_binding_width.c` 开池后报
        // “1 slots used, 0 reserved (last '<ci#save>')”，而 -O0 完全正常 —— 因为只有 -O2 才跑
        // 寄存器绑定 pass，家才会出现）。计数口径与申请处严格一致：`home` 只由分配器写
        // （用户显式绑定的 `home` 恒为 None），且只有 caller-saved 号才建槽。
        if let Some(b) = body {
            let mut ext_decls: Vec<&VarDecl> = Vec::new();
            Self::collect_frame_var_decls(
                &b.items,
                &self.static_local_map,
                &self.current_func_name,
                &mut ext_decls,
            );
            for v in ext_decls {
                let auto_ext_home = v.home.is_some_and(|l| {
                    l.reg()
                        .is_some_and(|r| crate::ssa::regalloc::is_auto_ext_reg(r.n))
                });
                if auto_ext_home {
                    slots += Self::var_alloc_slots(&v.ty);
                }
            }
        }
        self.late_slot_budget = slots;
        // 体内**被当值引用**、但规划期没登过的名字：以前它们在发射期走
        // `var_offset` 的兜底，拿到一个按 `Type::Int` 猜的栈槽（不是 int 的对象宽度就错，
        // 函数名被当值用时也会白占一个对象槽），而且带容量无法预知。
        // 现在在规划期按**表达式自带的类型**登进变量区：同一帧、同一个可预测位置，
        // 发射期只会命中 var_map，『谁会懒注册』从此不再需要猜。
        // 调用位置（`Call{callee}`）已被收集器跳过，所以函数名不会进入这里；
        // 已在 var_map 里的（形参与规划声明）、已知全局与函数名也排除。
        // 顺序：按首次出现序分配，与 `register_var` 一贯的「首次出现」口径一致。
        let mut idents: Vec<(String, Type)> = Vec::new();
        Self::collect_free_idents(b, &mut idents);
        // 形参类型里的 VLA 维度表达式 (K&R 风格 `int c[1][va_arg(a,int)]`)
        // 也可能引用本帧/外层 upvar 名字; 这些表达式在序言求值, 规划期必须
        // 登记进 var_map, 否则发射期 register_var 触发 E1134/E1133 (pr99324)。
        for p in params {
            Self::collect_free_idents_type(&p.ty, &mut idents);
            if let Some(d) = &p.vla_dim {
                Self::collect_free_idents_expr(d, &mut idents);
            }
        }
        for (name, ty) in idents {
            if self.var_map.contains_key(&name) {
                continue;
            }
            // 已知全局不占帧内槽（带家的文件作用域 register 变量例外：它本来就要
            // 一个帧内中转槽，见 §11.16）。
            if self.is_global_not_local(&name) && !self.global_needs_frame_slot(&name) {
                continue;
            }
            if matches!(ty, Type::Func(_, _)) {
                // 函数名被当值用：发射期走 `emit_func_addr`，不该占对象槽
                continue;
            }
            self.register_var(&name, &ty);
        }
        // 规划到此为止：此后的新名字登记就是发射期晚分配，只能吃上面那份预算
        // （`instructions.rs::register_var` 里的 `E1133`）。
        self.frame_planned = true;
    }

    /// 采集本函数的「局部 → 帧内位置」表，交给 DWARF（Phase 6.2）。
    ///
    /// 两个设计决定值得写明：
    ///   * **名字集合用规划期那一份遍历**（`collect_frame_var_decls`），不另起一次 AST
    ///     扫描——「哪些声明是本函数的用户局部」这件事不得有两个答案。合成名
    ///     （`__compound_lit_*`、`<agg_member_tmp>`）与静态局部（实体在 data/bss）因此天然不在表上；
    ///   * 只在 `-g` 下做。不开调试信息时这是纯白的工作，而且多一份可变的每函数状态
    ///     就多一份与产物无关的风险。
    ///
    /// 调用时机必须在函数体生成完之后、嵌套函数之前：那时 `homes` 与 `var_map` 才是本帧
    /// 的最终值，而嵌套函数会 `compile_func` 递归把两者复位。
    pub fn collect_debug_var_facts(&mut self, f: &FuncDecl) {
        if self.debug_level == 0 {
            return;
        }
        let Some(b) = &f.body else { return };
        let mut decls: Vec<&VarDecl> = Vec::new();
        Self::collect_frame_var_decls(&b.items, &self.static_local_map, &f.name.name, &mut decls);
        let declared: Vec<(String, Type, u32)> = decls
            .iter()
            .map(|v| (v.name.name.clone(), v.ty.clone(), v.span.line))
            .collect();
        drop(decls);
        let mut out: Vec<crate::codegen::x86_64::DebugVarLoc> = Vec::with_capacity(declared.len());
        for (name, ty, line) in declared {
            if let Some(loc) = self.var_location(&name) {
                out.push(crate::codegen::x86_64::DebugVarLoc {
                    name,
                    loc,
                    ty,
                    decl_line: line,
                });
            }
        }
        self.debug_var_facts.push((f.name.name.clone(), out));
    }

    /// 一个名字在本帧里的位置：有家就是家，没家就是它的变量区栈槽。
    ///
    /// **不能走 `home_for`**：那条路径对没登记的名字会调 `var_offset` 的兜底，而兜底从
    /// §11.23 步 1 起是 `E1134`（后端不变量破坏）。采集调试信息不得自己造出错误。
    /// 优先级与 `home_for` 一致（家 > 变量区），只是没命中时返回 `None` 而不是猜一个槽。
    ///
    /// `Spill` 家在这里就翻译回帧内偏移（走 `LayoutMap::spill_off` —— 全仓唯一那个翻译点），
    /// 存成 `Stack`：调试侧因此不需要知道帧布局，也不会出现第二份算溢出槽坐标的公式。
    /// 采集点在函数体生成完之后，那时 `frame_layout` 正是本帧的解算结果。
    fn var_location(&self, name: &str) -> Option<crate::location::Location> {
        if let Some((loc, _)) = self.homes.get(name) {
            return Some(match *loc {
                crate::location::Location::Spill(slot) => {
                    crate::location::Location::Stack(self.frame_layout.spill_off(slot))
                },
                other => other,
            });
        }
        self.var_map
            .get(name)
            .map(|(off, _)| crate::location::Location::Stack(*off))
    }

    /// 该全局是否**在本帧里**需要一个中转槽。判据就是文件作用域绑定的那一张表
    /// `global_homes`（`compile_module` 在逐声明扫描里登进去，每个函数入口恢复），
    /// 不另开一份「哪些全局有 register 请求」的名单 —— 那又是一套事实。
    fn global_needs_frame_slot(&self, name: &str) -> bool {
        self.global_homes.contains_key(name) || self.homes.contains_key(name)
    }

    /// 收集体内被当值引用的标识符（名字 + 表达式自带的类型）。
    /// 只走 `Expr::Ident`，并跳过调用位置；标号不是 Ident，不会混进来。
    /// 不下降进嵌套函数体（那是它自己一帧的事，与 `collect_frame_var_decls` 同口径）。
    fn collect_free_idents(b: &Block, out: &mut Vec<(String, Type)>) {
        for item in &b.items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    if let Some(init) = &v.init {
                        Self::collect_free_idents_expr(init, out);
                    }
                },
                // 嵌套函数自己一帧；类型定义不携带运行时引用
                BlockItem::Decl(_) => {},
                BlockItem::Stmt(s) => Self::collect_free_idents_stmt(s, out),
            }
        }
    }

    /// 类型树里携带的 VLA 维度表达式中的自由标识符。与 `late_need_type`
    /// 同骨架, 但收集 Ident 元组而非 LateNeed 计数 (pr99324)。
    fn collect_free_idents_type(ty: &Type, out: &mut Vec<(String, Type)>) {
        match ty {
            Type::Ptr(q)
            | Type::Complex(q)
            | Type::Imaginary(q)
            | Type::Vector(q, _)
            | Type::Atomic(q) => Self::collect_free_idents_type(&q.base, out),
            Type::Array(elem, ArraySize::Var(se)) => {
                Self::collect_free_idents_expr(se, out);
                Self::collect_free_idents_type(&elem.base, out);
            },
            Type::Array(elem, _) => Self::collect_free_idents_type(&elem.base, out),
            Type::Func(ret, params) => {
                Self::collect_free_idents_type(&ret.base, out);
                for p in params {
                    Self::collect_free_idents_type(&p.base, out);
                }
            },
            Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) => {
                for f in fields.iter() {
                    Self::collect_free_idents_type(&f.ty, out);
                }
            },
            _ => {},
        }
    }

    fn collect_free_idents_stmt(s: &Stmt, out: &mut Vec<(String, Type)>) {
        match s {
            Stmt::Block { b, .. } => Self::collect_free_idents(b, out),
            Stmt::Expr { e, .. } => Self::collect_free_idents_expr(e, out),
            Stmt::Return { val: Some(e), .. } => Self::collect_free_idents_expr(e, out),
            Stmt::GotoIndirect { expr, .. } => Self::collect_free_idents_expr(expr, out),
            Stmt::If {
                cond, then_, else_, ..
            } => {
                Self::collect_free_idents_expr(cond, out);
                Self::collect_free_idents_stmt(then_, out);
                if let Some(e) = else_ {
                    Self::collect_free_idents_stmt(e, out);
                }
            },
            Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
                Self::collect_free_idents_expr(cond, out);
                Self::collect_free_idents_stmt(body, out);
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                if let Some(i) = init {
                    Self::collect_free_idents_stmt(i, out);
                }
                if let Some(c) = cond {
                    Self::collect_free_idents_expr(c, out);
                }
                if let Some(st) = step {
                    Self::collect_free_idents_expr(st, out);
                }
                Self::collect_free_idents_stmt(body, out);
            },
            Stmt::Switch { expr, cases, .. } => {
                Self::collect_free_idents_expr(expr, out);
                for c in cases {
                    Self::collect_free_idents(&c.body, out);
                }
            },
            Stmt::Label { stmt, .. } | Stmt::Invoke { call: stmt, .. } => {
                Self::collect_free_idents_stmt(stmt, out);
            },
            Stmt::AsmGoto {
                outputs, inputs, ..
            } => {
                for x in outputs.iter().chain(inputs.iter()) {
                    Self::collect_free_idents_expr(x, out);
                }
            },
            Stmt::EhLandingPad {
                object, selector, ..
            } => {
                Self::collect_free_idents_expr(object, out);
                Self::collect_free_idents_expr(selector, out);
            },
            Stmt::Return { val: None, .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Goto { .. }
            | Stmt::Fallthrough { .. } => {},
        }
    }

    /// 表达式树里的 `Expr::Ident` 就是「被当值引用的名字」；调用位置上的 callee 不是
    /// 对象（`printf(...)` 里的 printf 走函数地址），所以 `Call` 只走实参。
    /// 穷举写：新增 `Expr` 变体会直接编不过，而不是静默漏一个子树。
    fn collect_free_idents_expr(e: &crate::ast::Expr, out: &mut Vec<(String, Type)>) {
        use crate::ast::Expr as E;
        match e {
            E::Ident { name, ty, .. } => out.push((name.name.clone(), ty.clone())),
            E::Int { .. }
            | E::Float { .. }
            | E::ComplexLit { .. }
            | E::ComplexIntLit { .. }
            | E::String { .. }
            | E::Char { .. }
            | E::SizeofTy { .. }
            | E::AlignofTy { .. }
            | E::LabelAddr { .. }
            | E::VaArgPack { .. } => {},
            E::Offsetof { indices, .. } => {
                for x in indices {
                    Self::collect_free_idents_expr(x, out);
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
            | E::Member { expr: x, .. }
            | E::PostInc { e: x, .. }
            | E::PostDec { e: x, .. }
            | E::PreInc { e: x, .. }
            | E::PreDec { e: x, .. } => Self::collect_free_idents_expr(x, out),
            E::Binop { l, r, .. }
            | E::Assign { l, r, .. }
            | E::Comma { l, r, .. }
            | E::Index { arr: l, idx: r, .. } => {
                Self::collect_free_idents_expr(l, out);
                Self::collect_free_idents_expr(r, out);
            },
            E::Cond {
                cond, then_, else_, ..
            } => {
                Self::collect_free_idents_expr(cond, out);
                Self::collect_free_idents_expr(then_, out);
                Self::collect_free_idents_expr(else_, out);
            },
            E::Call { args, .. } => {
                for x in args {
                    Self::collect_free_idents_expr(x, out);
                }
            },
            E::CompoundLit { inits, .. } | E::InitList { inits, .. } => {
                for it in inits {
                    match it {
                        crate::ast::InitItem::Expr(x) => Self::collect_free_idents_expr(x, out),
                        crate::ast::InitItem::Designated { designator, value } => {
                            Self::collect_free_idents_designator(designator, out);
                            Self::collect_free_idents_expr(value, out);
                        },
                    }
                }
            },
            E::StmtExpr { block, .. } => Self::collect_free_idents(block, out),
            E::AsmExpr {
                outputs, inputs, ..
            } => {
                for x in outputs.iter().chain(inputs.iter()) {
                    Self::collect_free_idents_expr(x, out);
                }
            },
        }
    }

    fn collect_free_idents_designator(d: &crate::ast::Designator, out: &mut Vec<(String, Type)>) {
        use crate::ast::Designator as D;
        match d {
            D::Field(_) => {},
            D::Index(x) => Self::collect_free_idents_expr(x, out),
            D::Range(a, b) => {
                Self::collect_free_idents_expr(a, out);
                Self::collect_free_idents_expr(b, out);
            },
            D::Chain(items) => {
                for sub in items {
                    Self::collect_free_idents_designator(sub, out);
                }
            },
        }
    }

    /// 本函数需要多少个 spill 槽：从**已经交付到声明上的家**里数（P12 Phase 5.1.b）。
    ///
    /// 为什么不在这里重新算一遍分配：分配器的决定已经写进 `VarDecl::home`，帧只需要回答
    /// 「这些家里出现过的最大槽号是多少」。拿 `max(槽号)+1` 当带尺寸，尺寸与坐标就天生
    /// 来自同一份事实；若另加一个 `FuncDecl::spill_slots` 字段由分配器写，就会出现
    /// 「带尺寸与槽号不一致」这种只能靠自觉维护的双轨（§3 的 H4）。
    ///
    /// 只算**真正占帧内槽**的声明：extern 引用全局符号、已由分析 pass 预注册的 static local
    /// 不占帧内空间（`collect_frame_var_decls` 那两处 `continue` 早于本函数），所以它们
    /// 就算带着 Spill 家也不得拉大带。序号负值按非法忽略（宁可少给一个槽也不能算出
    /// 一个越界的偏移）。
    fn count_spill_slot(v: &crate::ast::VarDecl, spill_slots: &mut i64) {
        if let Some(crate::location::Location::Spill(slot)) = v.home {
            if slot >= 0 && slot.saturating_add(1) > *spill_slots {
                *spill_slots = slot.saturating_add(1);
            }
        }
    }

    /// 函数体里「要占帧内槽」的声明，按首次出现顺序产出**一份**列表。
    ///
    /// 为什么必须是一份：定容（`var_alloc_slots` 的最大值折叠）与预注册以前各走一遍
    /// AST（`collect_var_alloc_maxima*` 与 `pre_register_stmt_max` 两份遍历），两份都必须
    /// 同意「哪些声明是规划期看得见的」。它们当时都漏了 `for` 的 init、标号语句的体与
    /// `({...})` 块里的声明，那批声明于是落到发射期懒注册 —— 实测把晚分配带挤爆
    /// （`E1133` 报 `for (int i = 0; …)` 的 `i` 超出带容量）。收成一份遍历之后，
    /// 「新增一种子树」只有一处可改；跳过规则（extern / 已预注册的 static local）也住在
    /// 这里，因为它们同时决定「占不占帧内槽」与「要不要数 spill 槽」。
    ///
    /// 不下降进嵌套函数体：那是它自己一帧的事（`compile_func` 会递归编译它，
    /// 带的容量与偏移也按那一帧单独解算）。
    fn collect_frame_var_decls<'a>(
        items: &'a [BlockItem],
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        out: &mut Vec<&'a VarDecl>,
    ) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    if v.is_extern && v.init.is_none() {
                        continue;
                    }
                    if v.is_static && static_local_map.is_static_local(func_name, &v.name.name) {
                        continue;
                    }
                    out.push(v);
                    // 初始化式里也能嵌着语句表达式块（`int r = ({ typeof(a) _a = (a); …})`，
                    // 内核 `max()`/`min()` 就是这个形状）：那里的声明属于**同一帧**，
                    // 不递归就会在发射期懒注册。先推自己再走 init，保持首次出现顺序。
                    if let Some(init) = &v.init {
                        Self::collect_frame_var_decls_expr(init, static_local_map, func_name, out);
                    }
                },
                // 类型/枚举/别名/块级 asm 定义不占帧内槽；嵌套函数自己一帧
                BlockItem::Decl(
                    Decl::Func(_)
                    | Decl::StructDef(_)
                    | Decl::UnionDef(_)
                    | Decl::EnumDef(_)
                    | Decl::Typedef { .. }
                    | Decl::GlobalAsm { .. },
                ) => {},
                BlockItem::Stmt(s) => {
                    Self::collect_frame_var_decls_stmt(s, static_local_map, func_name, out)
                },
            }
        }
    }

    /// 语句子树里的声明宿主。**不留 `_`**：新增 `Stmt` 变体会直接编不过，
    /// 而不是静默漏一条通路（漏一条的症状就是发射期懒注册 + 晚分配带溢出）。
    fn collect_frame_var_decls_stmt<'a>(
        s: &'a Stmt,
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        out: &mut Vec<&'a VarDecl>,
    ) {
        match s {
            Stmt::Block { b, .. } => {
                Self::collect_frame_var_decls(&b.items, static_local_map, func_name, out);
            },
            Stmt::If {
                cond, then_, else_, ..
            } => {
                Self::collect_frame_var_decls_expr(cond, static_local_map, func_name, out);
                Self::collect_frame_var_decls_stmt(then_, static_local_map, func_name, out);
                if let Some(e) = else_ {
                    Self::collect_frame_var_decls_stmt(e, static_local_map, func_name, out);
                }
            },
            // 循环的条件与体里都能带块（`while (MAX(a,b) > c) {…}`、`do {…} while (MIN(i,1));`）：
            // 只走体就会漏到发射期懒注册。
            Stmt::While { cond, body, .. } | Stmt::Do { body, cond, .. } => {
                Self::collect_frame_var_decls_expr(cond, static_local_map, func_name, out);
                Self::collect_frame_var_decls_stmt(body, static_local_map, func_name, out);
            },
            // `for` 的 init 也是声明的宿主（`for (int i = 0; …)`）
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                if let Some(i) = init {
                    Self::collect_frame_var_decls_stmt(i, static_local_map, func_name, out);
                }
                if let Some(c) = cond {
                    Self::collect_frame_var_decls_expr(c, static_local_map, func_name, out);
                }
                if let Some(st) = step {
                    Self::collect_frame_var_decls_expr(st, static_local_map, func_name, out);
                }
                Self::collect_frame_var_decls_stmt(body, static_local_map, func_name, out);
            },
            Stmt::Switch { expr, cases, .. } => {
                Self::collect_frame_var_decls_expr(expr, static_local_map, func_name, out);
                for case in cases {
                    Self::collect_frame_var_decls(
                        &case.body.items,
                        static_local_map,
                        func_name,
                        out,
                    );
                }
            },
            Stmt::Label { stmt, .. } => {
                Self::collect_frame_var_decls_stmt(stmt, static_local_map, func_name, out);
            },
            Stmt::Invoke { call, .. } => {
                Self::collect_frame_var_decls_stmt(call, static_local_map, func_name, out);
            },
            // 表达式里也可能嵌着块：`({... int x; ...})`、`int v = ({...; x;});`
            Stmt::Expr { e, .. } | Stmt::Return { val: Some(e), .. } => {
                Self::collect_frame_var_decls_expr(e, static_local_map, func_name, out);
            },
            Stmt::GotoIndirect { expr, .. } => {
                Self::collect_frame_var_decls_expr(expr, static_local_map, func_name, out);
            },
            Stmt::AsmGoto {
                outputs, inputs, ..
            } => {
                for x in outputs.iter().chain(inputs.iter()) {
                    Self::collect_frame_var_decls_expr(x, static_local_map, func_name, out);
                }
            },
            Stmt::Return { val: None, .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Goto { .. }
            | Stmt::Fallthrough { .. }
            | Stmt::EhLandingPad { .. } => {},
        }
    }

    /// 表达式子树里的 `({...})` 块也能带声明。穷举写：新增 `Expr` 变体编不过。
    fn collect_frame_var_decls_expr<'a>(
        e: &'a crate::ast::Expr,
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        out: &mut Vec<&'a VarDecl>,
    ) {
        use crate::ast::Expr as E;
        match e {
            E::StmtExpr { block, .. } => {
                Self::collect_frame_var_decls(&block.items, static_local_map, func_name, out);
            },
            E::Int { .. }
            | E::Float { .. }
            | E::ComplexLit { .. }
            | E::ComplexIntLit { .. }
            | E::String { .. }
            | E::Char { .. }
            | E::Ident { .. }
            | E::SizeofTy { .. }
            | E::AlignofTy { .. }
            | E::LabelAddr { .. }
            | E::VaArgPack { .. } => {},
            E::Unary { e: x, .. }
            | E::Cast { e: x, .. }
            | E::AddrOf { e: x, .. }
            | E::Deref { e: x, .. }
            | E::Real { e: x, .. }
            | E::Imag { e: x, .. }
            | E::SizeofExpr { e: x, .. }
            | E::AlignofExpr { e: x, .. }
            | E::VaArg { ap: x, .. }
            | E::Member { expr: x, .. }
            | E::PostInc { e: x, .. }
            | E::PostDec { e: x, .. }
            | E::PreInc { e: x, .. }
            | E::PreDec { e: x, .. } => {
                Self::collect_frame_var_decls_expr(x, static_local_map, func_name, out);
            },
            E::Offsetof { indices, .. } => {
                for x in indices {
                    Self::collect_frame_var_decls_expr(x, static_local_map, func_name, out);
                }
            },
            E::Binop { l, r, .. }
            | E::Assign { l, r, .. }
            | E::Comma { l, r, .. }
            | E::Index { arr: l, idx: r, .. } => {
                Self::collect_frame_var_decls_expr(l, static_local_map, func_name, out);
                Self::collect_frame_var_decls_expr(r, static_local_map, func_name, out);
            },
            E::Cond {
                cond, then_, else_, ..
            } => {
                Self::collect_frame_var_decls_expr(cond, static_local_map, func_name, out);
                Self::collect_frame_var_decls_expr(then_, static_local_map, func_name, out);
                Self::collect_frame_var_decls_expr(else_, static_local_map, func_name, out);
            },
            E::Call { callee, args, .. } => {
                Self::collect_frame_var_decls_expr(callee, static_local_map, func_name, out);
                for x in args {
                    Self::collect_frame_var_decls_expr(x, static_local_map, func_name, out);
                }
            },
            E::CompoundLit { inits, .. } | E::InitList { inits, .. } => {
                for it in inits {
                    match it {
                        crate::ast::InitItem::Expr(x) => {
                            Self::collect_frame_var_decls_expr(x, static_local_map, func_name, out)
                        },
                        crate::ast::InitItem::Designated { designator, value } => {
                            Self::collect_frame_var_decls_designator(
                                designator,
                                static_local_map,
                                func_name,
                                out,
                            );
                            Self::collect_frame_var_decls_expr(
                                value,
                                static_local_map,
                                func_name,
                                out,
                            );
                        },
                    }
                }
            },
            E::AsmExpr {
                outputs, inputs, ..
            } => {
                for x in outputs.iter().chain(inputs.iter()) {
                    Self::collect_frame_var_decls_expr(x, static_local_map, func_name, out);
                }
            },
        }
    }

    fn collect_frame_var_decls_designator<'a>(
        d: &'a crate::ast::Designator,
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        out: &mut Vec<&'a VarDecl>,
    ) {
        use crate::ast::Designator as D;
        match d {
            D::Field(_) => {},
            D::Index(x) => Self::collect_frame_var_decls_expr(x, static_local_map, func_name, out),
            D::Range(a, b) => {
                Self::collect_frame_var_decls_expr(a, static_local_map, func_name, out);
                Self::collect_frame_var_decls_expr(b, static_local_map, func_name, out);
            },
            D::Chain(items) => {
                for sub in items {
                    Self::collect_frame_var_decls_designator(sub, static_local_map, func_name, out);
                }
            },
        }
    }

    /// 帧内槽位规划的唯一入口：**一次**遍历产出「谁占槽」，然后
    ///   1) 定容 —— 每个名字取全函数**所有作用域**中的最大分配需求。var_map 按名字
    ///      去重共享栈槽（遮蔽的既有设计，见 `register_var` 的注释），但槽容量必须取
    ///      同名声明的最大值：旧实现按首次声明定容，后续同名更大的结构体声明
    ///      （如先 `struct with_vec s;` 16B 后 `struct vec_int s;` 32B）会溢出到相邻
    ///      局部 —— 根因: flag_clobber_frame.c 中 `s.i = 7` 静默覆盖已执行的 `ok = 0`；
    ///   2) 按首次出现顺序分配偏移，并顺带在同一趟里数 spill band 的尺寸
    ///      （不另开一份遍历，见 `count_spill_slot`）。
    pub fn pre_register_vars(
        var_map: &mut HashMap<String, (i64, Type)>,
        items: &[BlockItem],
        temp_slots: i32,
        kasan_rz_slots: i32,
        static_local_map: &crate::static_locals::StaticLocalMap,
        func_name: &str,
        total_slots: &mut i64,
        spill_slots: &mut i64,
    ) {
        let mut decls: Vec<&VarDecl> = Vec::new();
        Self::collect_frame_var_decls(items, static_local_map, func_name, &mut decls);
        let mut maxima: HashMap<String, (i64, Type)> = HashMap::new();
        for v in &decls {
            let slots = Self::var_alloc_slots(&v.ty);
            match maxima.get(&v.name.name) {
                Some(&(cur, _)) if cur >= slots => {},
                _ => {
                    maxima.insert(v.name.name.clone(), (slots, v.ty.clone()));
                },
            }
        }
        for v in &decls {
            Self::count_spill_slot(v, spill_slots);
            let (slots, alloc_ty) = match maxima.get(&v.name.name) {
                Some((s, t)) => (*s, t.clone()),
                None => (Self::var_alloc_slots(&v.ty), v.ty.clone()),
            };
            // 同名变量在不同作用域出现多次时, or_insert 不插入但 total_slots
            // 仍递增会导致偏移计算错误 —— 所以先查再推。
            if !var_map.contains_key(&v.name.name) {
                let used = total_slots.saturating_mul(8);
                let hdr = LayoutMap::hdr_of(temp_slots as i64);
                let off = -(used + hdr) - (slots.saturating_sub(1) * 8);
                var_map.insert(v.name.name.clone(), (off, alloc_ty));
                *total_slots = total_slots.saturating_add(slots);
                let rz_slots = kasan_rz_slots as i64;
                *total_slots = total_slots.saturating_add(rz_slots);
                for j in 0..kasan_rz_slots {
                    let rz_name = format!("__kasan_rz_{}_{}", v.name.name, j);
                    let rz_off = off - slots * 8 - j as i64 * 8;
                    var_map.entry(rz_name).or_insert((rz_off, Type::Void));
                }
            }
        }
    }

    pub fn collect_direct_nested_funcs(items: &[BlockItem], out: &mut Vec<FuncDecl>) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Func(f)) if f.body.is_some() => out.push(f.clone()),
                BlockItem::Stmt(s) => Self::collect_direct_nested_funcs_stmt(s, out),
                _ => {},
            }
        }
    }

    fn collect_direct_nested_funcs_stmt(s: &Stmt, out: &mut Vec<FuncDecl>) {
        match s {
            Stmt::Block { b, .. } => Self::collect_direct_nested_funcs(&b.items, out),
            Stmt::If { then_, else_, .. } => {
                Self::collect_direct_nested_funcs_stmt(then_, out);
                if let Some(e) = else_ {
                    Self::collect_direct_nested_funcs_stmt(e, out);
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                Self::collect_direct_nested_funcs_stmt(body, out);
            },
            _ => {},
        }
    }

    pub fn emit_zero_init_frame(&mut self, frame_size: i64) {
        if frame_size <= 0 {
            return;
        }
        // 保存 rcx, rdi, rax (它们可能在后续代码中使用)
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Push,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    0,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // push rax
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Push,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    1,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // push rcx
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Push,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    7,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // push rdi

        // xor eax, eax (rax = 0)
        // P11: 收敛到 AluRm{Xor, B32, rax, rax}.
        {
            let insn = Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                    op: crate::codegen::x86_64::opcode::AluOp::Xor,
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0,
                        crate::codegen::shared::asm_operand::Width::B32,
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0,
                        crate::codegen::shared::asm_operand::Width::B32,
                    ),
                },
                &[],
            );
            if !self.emit_insn(&insn) {
                self.emit_insn(&Insn::alu_rm_reg_reg(
                    crate::codegen::x86_64::opcode::AluOp::Xor,
                    crate::codegen::shared::asm_operand::Width::B32,
                    0,
                    0,
                ));
            }
        }
        // mov rcx, frame_size / 8
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                rip_relative: false,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    1,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Imm(((frame_size as u64) / 8) as i64),
            },
            &[],
        ));
        // rdi = r13 - frame_size (帧底部)
        self.emit_lea_r13(7, -frame_size);
        // rep stosq: F3 48 AB
        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepStosq));

        // 恢复 rdi, rcx, rax
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Pop,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    7,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // pop rdi
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Pop,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    1,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // pop rcx
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Pop,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    0,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // pop rax
    }

    pub fn emit_stack_clash_probes(&mut self, frame_size: i64) {
        if frame_size <= 0 {
            return;
        }
        let page: i64 = 4096;
        let pages = frame_size / page;
        let remaining = frame_size % page;
        if pages > 0 {
            self.mov_imm(11, pages as i128);
            let loop_start = self.pos();
            // sub rsp, 4096: 走 emit_sub_rsp (AluRm{Sub, B64, rsp, imm32} 类型化)
            self.emit_sub_rsp(4096);
            self.emit_insn(&Insn::store_imm_mem(
                crate::codegen::shared::asm_operand::Width::B8,
                4,
                0,
                0,
            )); // mov byte ptr [rsp], 0
                // dec r11: 49 FF CB (REX.W B64, /1 modrm=11 001 011)
            self.emit_insn(&Insn::unary_rm_reg(
                crate::codegen::x86_64::opcode::UnaryOp::Dec,
                crate::codegen::shared::asm_operand::Width::B64,
                11,
            ));
            let loop_end = self.pos();
            let disp = loop_start as i64 - loop_end as i64 - 2;
            self.emit_insn(&Insn::raw_bytes(&[disp as i8 as u8])); // jnz loop
        }
        self.emit_sub_rsp(remaining);
    }

    pub fn emit_xmm_r13_disp(&mut self, pfx: u8, op: u8, xmm: u8, off: i64) {
        let rex = 0x41 | if xmm >= 8 { 0x04 } else { 0 };
        // P9 方案 C: 按 pfx/op 组合生成一条完整 gas 行. 调用方已把完整字节合成过,
        // 我们这里把前缀+opcode 合并发射成单条 Insn, 便于渲染器直接输出 gas_text.
        use crate::codegen::x86_64::emit::gas::{mem_disp as gas_mem_disp, XMM as GAS_XMM};
        // op=0x11 = mov [r13+off], xmm ; op=0x10 = mov xmm, [r13+off]
        let mnem_base = match op {
            0x10 => "mov",
            0x11 => "mov",
            _ => "mov",
        };
        let pfx_sz = match pfx {
            0xF3 => "ss",  // movss
            0xF2 => "sd",  // movsd
            0x66 => "apd", // movapd
            0x00 => "q",   // movq (no SSE prefix)
            _ => "",
        };
        let mnem = format!("{mnem_base}{pfx_sz}");
        let mem = gas_mem_disp(13, off);
        let xmm_name = GAS_XMM[(xmm & 15) as usize];
        // 用单条发射合并 pfx+rex+0x0f+op+modrm+disp.
        // 之前: pfx/rex/0F/op 由本函数发射, modrm+disp 由 emit_r13_modrm_disp 发射.
        // 为了让渲染器一条 gas 行直接出, 这里我们在内部拼完整字节.
        let mut bytes: Vec<u8> = Vec::new();
        if pfx != 0 {
            bytes.push(pfx);
        }
        bytes.push(rex);
        bytes.push(0x0F);
        bytes.push(op);
        // modrm: mod=01 (disp8) / 10 (disp32), reg=xmm, r/m=13=5
        let reg_field = xmm & 7;
        if Self::fits_disp8(off) {
            bytes.push(0x45 | (reg_field << 3));
            bytes.push(off as i8 as u8);
        } else if Self::fits_disp32(off) {
            bytes.push(0x85 | (reg_field << 3));
            bytes.extend_from_slice(&(off as i32).to_le_bytes());
        } else {
            // fallback: 走原路径
            if pfx != 0 {
                self.emit_insn(&Insn::raw_bytes(&[pfx, rex, 0x0F, op]));
            } else {
                self.emit_insn(&Insn::raw_bytes(&[rex, 0x0F, op]));
            }
            self.emit_lea_r13(11, off);
            let rex2 = 0x41 | if xmm >= 8 { 0x04 } else { 0 } | 0x01;
            if pfx != 0 {
                self.emit_insn(&Insn::raw_bytes(&[pfx, rex2, 0x0F, op]));
            } else {
                self.emit_insn(&Insn::raw_bytes(&[rex2, 0x0F, op]));
            }
            self.emit_reg_indirect_modrm(xmm, 11);
            return;
        }
        let gas = if op == 0x11 {
            format!("{mnem} {xmm_name}, {mem}")
        } else {
            format!("{mnem} {mem}, {xmm_name}")
        };
        self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
    }

    pub fn store_xmm32_stack(&mut self, xmm: u8, off: i64) {
        self.emit_xmm_r13_disp(0xF3, 0x11, xmm, off);
    }

    pub fn emit_func_epilogue(&mut self) {
        // Stack canary 检查: 在恢复 rsp 前验证金丝雀值
        if self.target.stack_protector && self.frame_layout.canary_fits() {
            // mov r11, [r13 - 8]  → 加载存储的 canary 到临时寄存器（槽位 = `abi::layout::CANARY_OFF`）
            self.emit_insn(&Insn::mov_rm_mem(
                true,
                crate::codegen::shared::asm_operand::Width::B64,
                11,
                13,
                -8,
            ));
            // xor r11, seg:[0x28]  → 与原始 canary 比较
            // seg = fs (0x64) or gs (0x65) based on guard register
            let seg_prefix = match self.target.stack_protector_guard_reg {
                crate::cli::StackGuardReg::Gs => 0x65u8,
                _ => 0x64u8, // Fs is default
            };
            self.emit_insn(&Insn::raw_bytes(&[
                seg_prefix, 0x4c, 0x33, 0x1c, 0x25, 0x28, 0x00, 0x00, 0x00,
            ]));
            // Always call __stack_chk_fail on canary mismatch.
            // Both ET_REL and ET_EXEC use the same path:
            // __stack_chk_fail is emitted as an undefined global
            // symbol (provided by libc / kernel runtime).
            self.emit_insn(&Insn::short_jump_rel8(0x74, 0x05)); // je .+5 (canary OK → skip)
            self.call_label("__stack_chk_fail");
        }
        // 恢复被调用者保存寄存器: rbx, r12, r14, r15
        // 同序言: 无条件发射, 未使用的寄存器由 patch_callee_saves 改为 nop
        // mov rbx, [r13 - 16]  (REX.WB: r13 needs REX.B)
        self.callee_save_patch_sites.push((self.pos(), 0x01));
        self.emit_insn(
            &Insn::mov_rm_mem(true, crate::codegen::shared::asm_operand::Width::B64, 3, 13, -16)
                .with_gas("mov -16(%r13), %rbx".to_string()),
        );
        // mov r12, [r13 - 24]  (REX.WRB: r12 needs REX.R, r13 needs REX.B)
        self.callee_save_patch_sites.push((self.pos(), 0x02));
        self.emit_insn(
            &Insn::mov_rm_mem(true, crate::codegen::shared::asm_operand::Width::B64, 12, 13, -24)
                .with_gas("mov -24(%r13), %r12".to_string()),
        );
        // mov r14, [r13 - 32]  (REX.WRB: r14 needs REX.R, r13 needs REX.B)
        self.callee_save_patch_sites.push((self.pos(), 0x04));
        self.emit_insn(
            &Insn::mov_rm_mem(true, crate::codegen::shared::asm_operand::Width::B64, 14, 13, -32)
                .with_gas("mov -32(%r13), %r14".to_string()),
        );
        // mov r15, [r13 - 40]  (REX.WRB: r15 在 reg 字段需要 REX.R, r13 在 r/m 需要 REX.B)
        // 这里没有 SIB，REX.X 是多余的：旧代码发 0x4F = W|R|X|B，产物里被 objdump 标成
        // `rex.WRXB mov -0x28(%r13),%r15`（与序言侧同一个复制源）。
        self.callee_save_patch_sites.push((self.pos(), 0x08));
        self.emit_insn(
            &Insn::mov_rm_mem(true, crate::codegen::shared::asm_operand::Width::B64, 15, 13, -40)
                .with_gas("mov -40(%r13), %r15".to_string()),
        );
        // mov rsp, r13: 恢复栈指针
        // P11: 必须保留 0x49 0x8b 0xe5 形式 (mov r/m64,r64 中 rsp 是 r/m, 需
        // REX.B=1 扩展 r13/r/m). mov_rm_reg 走 0x89 形式导致 REX 字节不同, 破坏
        // G10 快照字节一致性. 这里保留 hand-written 形态以维持 byte-identical.
        self.emit_insn(&Insn::raw_with_gas(&[0x49, 0x8b, 0xe5], "mov %r13, %rsp"));
        self.emit_insn(
            &Insn::stack_op_reg(crate::codegen::x86_64::opcode::StackOp::Pop, 13)
                .with_gas("pop %r13".to_string()),
        );
        if self.target.frame_pointer {
            self.emit_insn(
                &Insn::stack_op_reg(crate::codegen::x86_64::opcode::StackOp::Pop, 5)
                    .with_gas("pop %rbp".to_string()),
            );
        }
        // noreturn: 在 ret 前插入 trap 指令 (ud2)
        // 防止函数未正常返回时 (如 panic/abort/longjmp)
        // 执行流落入调用者后续代码
        if self.current_is_noreturn {
            self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Ud2));
        }
        self.emit_return_tail();
    }

    pub fn emit_return_tail(&mut self) {
        if self.target.function_return_thunk_extern {
            self.jmp_external("__x86_return_thunk");
        } else {
            // 步骤 3: 走类型化 Ret{add_imm: None} (C3).
            let mut insn =
                Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Ret { add_imm: None }, &[]);
            insn.gas_text = Some("ret".to_string());
            if !self.emit_insn(&insn) {
                self.emit_insn(&Insn::raw_with_gas(&[0xC3], "ret"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{BlockItem, Decl, Stmt};
    use crate::location::Location;
    use crate::static_locals::StaticLocalMap;

    /// 把指定名字的声明的 `home` 改成 `Spill(slot)`。今天分配器还不产出 Spill 家，所以
    /// 本用例只能这样注入 —— 验的是「带的尺寸从已交付的家导出」这条机制本身，
    /// 而不是某个分配结果。
    fn body_with_spill_home(src: &str, fname: &str, name: &str, slot: i64) -> Vec<BlockItem> {
        let mut items = body_of(src, fname);
        let mut hit = false;
        set_home(&mut items, name, slot, &mut hit);
        assert!(hit, "源里没有名为 '{name}' 的声明（用例不得因为找不到名字而空跑）");
        items
    }

    fn body_of(src: &str, fname: &str) -> Vec<BlockItem> {
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        m.source()
            .decls
            .iter()
            .find_map(|d| match d {
                Decl::Func(f) if f.name.name == fname => Some(f.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("源里没有函数 {fname}"))
            .body
            .expect("被测函数必须有体")
            .items
    }

    /// 遍历形状与 `pre_register_vars` 那一份一致（只服务于测试，不进产物路径）。
    fn set_home(items: &mut [BlockItem], name: &str, slot: i64, hit: &mut bool) {
        for item in items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    if v.name.name == name {
                        v.home = Some(Location::Spill(slot));
                        *hit = true;
                    }
                },
                // 其余声明（函数/类型/typedef/global asm）里没有要改的局部家。
                BlockItem::Decl(_) => {},
                BlockItem::Stmt(s) => set_home_stmt(s, name, slot, hit),
            }
        }
    }

    fn set_home_stmt(s: &mut Stmt, name: &str, slot: i64, hit: &mut bool) {
        match s {
            Stmt::Block { b, .. } => set_home(&mut b.items, name, slot, hit),
            Stmt::If { then_, else_, .. } => {
                set_home_stmt(then_, name, slot, hit);
                if let Some(e) = else_ {
                    set_home_stmt(e, name, slot, hit);
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                set_home_stmt(body, name, slot, hit)
            },
            Stmt::Switch { cases, .. } => {
                for c in cases {
                    set_home(&mut c.body.items, name, slot, hit);
                }
            },
            _ => {},
        }
    }

    fn plan_spill_slots(items: &[BlockItem], fname: &str) -> (i64, usize, i64) {
        let mut var_map: HashMap<String, (i64, crate::ast::Type)> = HashMap::new();
        let mut total = 0i64;
        let mut spill = 0i64;
        Codegen::pre_register_vars(
            &mut var_map,
            items,
            2,
            0,
            &StaticLocalMap::default(),
            fname,
            &mut total,
            &mut spill,
        );
        // 第三个读数就是「遍历真的跑了」的证据：没有它，spill == 0 可能只是根本没进入循环。
        (spill, var_map.len(), total)
    }

    const SRC: &str = "long f(long n){ long a = n; { long deep = n + 1; a += deep; } \
                        extern long g_env; long b = a + n; return b + g_env; }\n";

    /// Phase 5.1.b 的三条：**最大槽号 + 1 就是带的尺寸**；嵌套块里的声明不会漏算；
    /// 而 extern 声明不占帧内空间，就算带着 Spill 家也不得拉大带（拉大了是白吃帧，
    /// 算小了就是越界写）。最后一句钉「没有 Spill 家⇒带为 0」：那才是今天全语料的形状，
    /// 也是「本步产物逐字节不变」的根据。
    #[test]
    fn spill_band_is_sized_from_the_delivered_homes() {
        assert_eq!(plan_spill_slots(&body_with_spill_home(SRC, "f", "a", 0), "f").0, 1);
        assert_eq!(plan_spill_slots(&body_with_spill_home(SRC, "f", "a", 2), "f").0, 3);
        assert_eq!(plan_spill_slots(&body_with_spill_home(SRC, "f", "deep", 5), "f").0, 6);
        // extern 声明不占帧内槽：就算它被塞了 Spill 家也不得拉大带（拉大了就是白吃帧），
        // 而同一个调用里普通局部仍然真的被预注册（否则这条 0 只是空跑）。
        let (for_extern, registered, watermark) =
            plan_spill_slots(&body_with_spill_home(SRC, "f", "g_env", 7), "f");
        assert_eq!(for_extern, 0, "extern 声明不得拉大 spill band");
        assert!(
            registered >= 3 && watermark > 0,
            "同一个调用必须真的预注册了局部（变量区没算起来就是遍历没跑到，上面那个 0 就是假绿）"
        );
        let (none, _, _) = plan_spill_slots(&body_of(SRC, "f"), "f");
        assert_eq!(none, 0, "没有任何 Spill 家时带必须是 0");
    }

    /// §11.23 步 2：晚分配预算的四族各自都能被静态数出来，而且与发射期实际发生的
    /// `register_var` 同口径（同一份语料的名字级对账住在
    /// `.work_tmp/p12_base/late_family_census.sh`：那里逐笔验证「实际晚分配的名字集合」
    /// 就是本用例列的四族，没有第五族）。每一族都带一条反向对照：那个形状必须**不**计费，
    /// 否则本用例只能证明「扫描把所有东西都算上了」。
    #[test]
    fn late_need_charges_the_four_late_families_exactly() {
        // 1. 复合字面量：站点名含发射序 ⇒ 站点之间不复用，16B 一份 = 2 槽
        let n = Codegen::late_need(&body_of(
            "struct S{long a,b;}; long f(int i){ long x = (struct S){i,i}.a; \
             long y = (struct S){1,2}.b; return x + y; }",
            "f",
        ));
        assert_eq!(n.compound_slots, 4, "两个复合字面量站点各 2 槽");
        // 反向对照：向量形态走 `vector_temp_base`，不在变量区 ⇒ 不得计费
        let n = Codegen::late_need(&body_of(
            "typedef int vi __attribute__((vector_size(16))); \
             long f(void){ vi v = (vi){1,2,3,4}; return v[0]; }",
            "f",
        ));
        assert_eq!(n.compound_slots, 0, "向量复合字面量不得预扣变量区槽");

        // 2. VM 尺寸元信息：同一个 tag 的多次声明只对应一槽（发射点按 key 复用）
        let n = Codegen::late_need(&body_of(
            "struct V{int n; int a[n];}; long f(int i){ struct V v; struct V w; \
             v.n=i; return w.n + i; }",
            "f",
        ));
        assert_eq!(n.vmsize_keys, vec!["struct:V".to_string()], "同 tag 去重后的键集");
        // 块级 struct 定义也绑一个尺寸键（`gen_vm_struct_bind`）
        let n = Codegen::late_need(&body_of(
            "long f(int i){ struct W{int m; int b[m];} w; w.m=i; return w.m; }",
            "f",
        ));
        assert_eq!(n.vmsize_keys, vec!["struct:W".to_string()]);
        // 反向对照：字段里没 VLA 的记录类型不绑尺寸槽
        let n = Codegen::late_need(&body_of(
            "struct P{long a,b;}; long f(long i){ struct P p; p.a=i; return p.a; }",
            "f",
        ));
        assert!(n.vmsize_keys.is_empty(), "非 VM 记录类型不得拉大预算");

        // 3. `va_start` 的 ap：按名字去重；指针形态（退化形参）不占帧槽
        let n = Codegen::late_need(&body_of(
            "long f(int i, ...){ __builtin_va_list ap; __builtin_va_start(ap, i); \
             __builtin_va_start(ap, i); return i; }",
            "f",
        ));
        assert_eq!(n.ap_names, vec!["ap".to_string()], "两次 va_start 同一 ap 只一份");
        let n = Codegen::late_need(&body_of(
            "long f(int i, ...){ __builtin_va_list a; __builtin_va_list b; \
             __builtin_va_start(a, i); __builtin_va_start(b, i); return i; }",
            "f",
        ));
        assert_eq!(n.ap_names, vec!["a".to_string(), "b".to_string()]);

        // 4. 聚合成员赋值的一般 RHS 才需要 `<agg_member_tmp>`
        let agg_src = "struct S{long a,b;}; struct T{struct S q; long g;}; \
                       long f(int c, struct S x, struct S y){ struct T t; \
                       t.q = c ? x : y; return t.g; }";
        assert!(Codegen::late_need(&body_of(agg_src, "f")).agg_tmp);
        // 反向对照：Ident / 复合字面量 RHS 走「直接拿地址」那几臂 ⇒ 不需暂存
        let no_tmp_src = "struct S{long a,b;}; struct T{struct S q; long g;}; \
                          long f(struct S x){ struct T t; t.q = x; \
                          t.q = (struct S){1,2}; return t.g; }";
        assert!(
            !Codegen::late_need(&body_of(no_tmp_src, "f")).agg_tmp,
            "排除形状不得置位（否则预算不是算准而是宁可多给）"
        );
    }
}
