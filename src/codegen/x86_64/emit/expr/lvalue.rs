// src/codegen/x86_64/emit/expr/lvalue.rs
// gen_expr 中 lvalue-rvalue 类别的 arm 搬迁 (Member / Index / Deref / Ident)。
// 本文件由 mod.rs 逐字搬迁而来 (batch 4g): 每个 arm body 原样剪切进
// `impl Codegen` 的一个新方法, 语义 diff 为 0。旧影子自由函数
// `gen_lvalue_addr` / `handle_identifier` / `handle_member` 等一并作废。

use crate::ast::{find_field, Expr, Ident, Type};
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::{Codegen, JumpKind};
use crate::diagnostics::Span;

impl Codegen {
    /// gen_expr 中 Expr::Deref arm 的搬迁体, 逐字复制, 未改逻辑。
    pub fn gen_expr_deref_rvalue(&mut self, e: &Expr, ty: &Type, dst: u8) {
        // *e: 求值内部表达式得到地址, 然后按结果类型大小加载
        let load_ty = ty.clone();
        let load_size = load_ty.size().max(1);
        // C11 6.3.2.1p3: *e 的类型是数组时 (e 指向数组的指针),
        // 值上下文衰减为首元素地址 = e 的值本身, 不得按数组
        // 大小加载内容 (va-arg-21: vprintf(s, **ap_ptr) 会把
        // va_list 前 8 字节当指针传入 → 段错误)。
        if matches!(load_ty, Type::Array(_, _)) {
            self.gen_expr(e, dst);
            return;
        }
        self.gen_expr(e, Self::EVAL_BASE);
        // __seg_gs/__seg_fs 指针解引用: 发射段前缀 (%gs:/%fs:)。`e` 就是**基址表达式**，
        // 所以走 `seg_prefix_of_base`（先看本层 pointee，再沿链下潜 —— `*pp` /
        // `*(p + 1)` 这些形状外层不一定就是带段指针）。
        if let Some(seg) = self.seg_prefix_of_base(e) {
            self.emit_load_mem_typed_seg(load_size, &load_ty, seg);
        } else {
            self.emit_load_mem_typed(load_size, &load_ty);
        }
        if dst != 0 && !matches!(load_ty, Type::Float | Type::Double) {
            self.mov(dst, 0);
        }
    }

    /// gen_expr 中 Expr::Index arm 的搬迁体, 逐字复制, 未改逻辑。
    pub fn gen_expr_index_rvalue(
        &mut self,
        e: &Expr,
        arr: &Expr,
        idx: &Expr,
        elem_ty: &Type,
        span: &Span,
        dst: u8,
    ) {
        // Step 1: 语义层计算地址描述符
        use crate::semantics::{
            compute_lvalue_address, determine_action, AddressBase, AddressContext, ByteOffset,
            MaterializeAction,
        };

        let ctx = AddressContext {
            is_nested: self.current_func_is_nested,
        };

        // 多维/混合下标链由语义层描述符完整表达 (`ByteOffset::Runtime.terms`
        // 逐项携带自己的下标表达式), 因此这里不需要任何「嵌套运行时索引」
        // 浅层试探: 物化失败时统一退回下方旧路径。
        {
            let desc = compute_lvalue_address(self, e, &ctx);

            // Step 2: 确定物化动作
            let action = determine_action(elem_ty, true);

            // Step 3: SIB 快速路径优化 (仅全局数组 + 标量加载 + 单个运行时下标)
            // 这是在语义层解析之后的优化: 使用已解析的 symbol_key
            if matches!(action, MaterializeAction::LoadScalar)
                && matches!(&desc.base, AddressBase::GlobalSymbolAddr { .. })
                && matches!(&desc.offset, ByteOffset::Runtime { addend: 0, .. })
            {
                if let AddressBase::GlobalSymbolAddr { symbol_key } = &desc.base {
                    if let ByteOffset::Runtime { terms, addend: 0 } = &desc.offset {
                        // 只有单个运行时下标项才能走 SIB scale
                        if let [term] = terms.as_slice() {
                            let es = term.stride as u8;
                            // 只有 1/2/4/8 字节元素能用 SIB scale
                            if term.stride == 1
                                || term.stride == 2
                                || term.stride == 4
                                || term.stride == 8
                            {
                                self.gen_expr(&term.index, 1);
                                self.emit_load_global_indexed(dst, 1, symbol_key, es, elem_ty);
                                return;
                            }
                        }
                    }
                }
            }

            if std::env::var("DCC_DBG_IDX").is_ok()
                && self.current_func_name == "vdbeSorterCompareInt"
            {
                eprintln!("[IDX-ENTER] f={} e={} elem={} arr_ty={} desc.base={:?} desc.off={:?} action={:?}",
                self.current_func_name, crate::ast::type_kind_summary(&e.ty()),
                crate::ast::type_kind_summary(elem_ty),
                crate::ast::type_kind_summary(&arr.ty()),
                desc.base, desc.offset,
                determine_action(elem_ty, true));
            }
            // Step 4: 通用路径 - 通过语义层物化
            // 数组元素衰减: 元素是数组时返回地址 (ArrayDecay)
            if matches!(action, MaterializeAction::ArrayDecay) {
                if self.materialize_address(
                    &desc,
                    MaterializeAction::TakeAddress,
                    Some(idx),
                    e,
                    Some(arr),
                    dst,
                ) {
                    return;
                }
            } else if matches!(action, MaterializeAction::AggregateLoad) {
                // 聚合元素: 返回地址供调用方使用
                if self.materialize_address(
                    &desc,
                    MaterializeAction::TakeAddress,
                    Some(idx),
                    e,
                    Some(arr),
                    dst,
                ) {
                    return;
                }
            } else if matches!(action, MaterializeAction::LoadScalar) {
                // 标量加载: 先做 bounds check, 再加载
                // 注意: bounds check 需要在索引求值后、地址计算前插入
                // 为简化实现, 我们先尝试语义层路径, 失败则退回旧路径
                if self.bounds_check {
                    // bounds check 模式: 退回旧路径 (保证边界检查正确)
                    // TODO: 将 bounds check 集成到 materialize_address 中
                } else {
                    if self.materialize_address(&desc, action, Some(idx), e, Some(arr), dst) {
                        if std::env::var("DCC_DBG_IDX").is_ok()
                            && self.current_func_name == "vdbeSorterCompareInt"
                        {
                            eprintln!("[IDX-MAT-OK] f={}", self.current_func_name);
                        }
                        return;
                    }
                    if std::env::var("DCC_DBG_IDX").is_ok()
                        && self.current_func_name == "vdbeSorterCompareInt"
                    {
                        eprintln!("[IDX-MAT-FAIL] f={} -> fallback", self.current_func_name);
                    }
                }
            }
        }

        // Fallback: 旧路径 (语义层无法处理时使用)
        // 数组类型的元素作为值使用时退化为指向首元素的指针
        // (C11 6.3.2.1p3): 如 words[1] (char words[][6]) 作函数参数
        // 时应传 &words[1] 地址, 而非对元素做标量加载。
        if matches!(elem_ty, Type::Array(_, _)) {
            self.gen_lvalue_addr(e, dst);
            return;
        }
        let es = elem_ty.size() as i32;
        if let Expr::Ident {
            name: arr_name,
            ty: arr_ty,
            ..
        } = arr
        {
            // 仅全局数组走 SIB 索引快速路径; 全局指针 (char *s = "hello")
            // 的下标访问必须先加载指针值再解引用, 不能对符号本身绝对寻址。
            // 修复: 使用 resolve_global_key 解析 static local -> mangled 名,
            // 避免源名称与文件级全局同名时的错误路由 (BUG-A)。
            // 注意: SIB 快速路径仅用于文件级全局数组 (RIP-relative 寻址)。
            // Static local 虽然注册在 globals 中, 但不能用 RIP-relative,
            // 必须走慢路径 (emit_ident_base_addr) 从栈帧访问。
            let is_static_local = self
                .static_local_map
                .is_static_local(&self.current_func_name, &arr_name.name);
            let resolved_key = self.resolve_global_key(&arr_name.name).to_string();
            let is_global_array = !is_static_local
                && self
                    .globals_raw
                    .get(&resolved_key)
                    .is_some_and(|g| matches!(g.ty, Type::Array(_, _)));
            if is_global_array {
                // 全局数组: 单条 [disp32+idx*scale] 加载, 避免 LEA+ADD 两步。
                // 注: 不在前面 imul rcx, rcx, es - emit_load_global_indexed 已用 SIB scale 体现 es。
                self.gen_expr(idx, 1);
                self.emit_load_global_indexed(dst, 1, &resolved_key, es as u8, elem_ty);
            } else {
                // 统一基址计算 (修复 BUG-B: 使用 resolve_global_key 解析 static local)
                self.emit_ident_base_addr(&arr_name.name, arr_ty, 0);
                self.push(0); // 保存基址 (gen_expr(idx) 可能破坏 rax)
                self.gen_expr(idx, 1); // rcx = index
                self.pop(0); // 恢复基址到 rax
                if self.bounds_check {
                    // 保存元素索引 (imul 会覆盖 rcx): libubsan handler 第二参数
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                            rip_relative: false,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                6,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    )); // mov rsi, rcx
                }
                self.emit_index_scale(es, arr, elem_ty);
                // -fbounds-check: 数组下标运行时边界检查
                if self.bounds_check {
                    if let Type::Array(_, crate::ast::ArraySize::Const(n)) = arr_ty {
                        // 比较 rcx (idx*es) < n*es; 不在范围内调 __ubsan_handle_out_of_bounds
                        let ok_label = self.fresh_label();
                        let n_imm = (*n as i64) * (es as i64);
                        if n_imm > 0 && n_imm < i32::MAX as i64 {
                            // cmp rcx, imm32 (81 /7 id, B64). 走类型化 imm32 构造器.
                            self.emit_insn(&Insn::alu_rm_reg_imm32_b64(7, 1, n_imm));
                            self.jmp_rel32(&ok_label, JumpKind::Jb);
                            // out of bounds: 调用 __ubsan_handle_out_of_bounds
                            // 签名: (OutOfBoundsData *Data, unsigned long Index)。
                            // Data 不能传 0/NULL: libubsan 会解引用
                            // Data->Loc 与描述符打印诊断
                            // (此前传垃圾指针, 越界即段错误而非报错)。
                            self.emit_ubsan_oob_data(
                                &idx.ty(),
                                elem_ty,
                                &format!("{} [{}]", Self::ubsan_type_name(elem_ty), *n),
                                span,
                            );
                            self.emit_insn(&Insn::call_rel32()); // call rel32
                                                                 // call 位移起始于当前 code.len() - 4 (0xe8 之后).
                                                                 // 与 trapv 处同号。- 4 错为 - 0 会让 R_X86_64_PLT32
                                                                 // 盖到下一条指令首字节, 运行时 SIGILL.
                            let call_at = self.code.len() as u64 - 4;
                            self.pending_func_rels
                                .push((call_at, "__ubsan_handle_out_of_bounds".to_string()));
                            self.label(&ok_label);
                        }
                        // n_imm 溢出/0/未知大小: 跳过检查 (留待运行时)
                    }
                }
                // P11: add rax, rcx via typed opcode.
                {
                    let insn = Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::Add,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                1,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    );
                    if !self.emit_insn(&insn) {
                        self.emit_insn(&Insn::alu_rm_reg_reg(
                            crate::codegen::x86_64::opcode::AluOp::Add,
                            crate::codegen::shared::asm_operand::Width::B64,
                            0,
                            1,
                        )); // add rax, rcx
                    }
                }
                // 语义层描述符接不住时的兼容路径：段限定基址同样要带前缀，
                // 否则 `p[i]` 会退化成普通访问（单一入口见 `lvalue_seg_prefix`）。
                self.emit_seg(self.lvalue_seg_prefix(e));
                self.emit_load_mem_typed(es as usize, elem_ty); // 按元素类型加载
                if dst != 0 && !matches!(elem_ty, Type::Float | Type::Double) {
                    self.mov(dst, 0);
                }
            }
        } else {
            // 通用路径: 基址来自任意 lvalue/指针表达式 (如 s.i[k])
            let arr_ty = arr.ty();
            let arr_is_arr = matches!(arr_ty, Type::Array(_, _) | Type::Vector(_, _));
            if arr_is_arr {
                if !self.gen_lvalue_addr(arr, Self::EVAL_BASE) {
                    self.gen_expr(arr, Self::EVAL_BASE);
                }
            } else {
                self.gen_expr(arr, Self::EVAL_BASE);
            }
            self.push(0);
            self.gen_expr(idx, 1);
            self.pop(0);
            self.emit_index_scale(es, arr, elem_ty);
            // P11: add rax, rcx via typed opcode.
            {
                let insn = Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                        op: crate::codegen::x86_64::opcode::AluOp::Add,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Reg(
                            1,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                    },
                    &[],
                );
                if !self.emit_insn(&insn) {
                    self.emit_insn(&Insn::alu_rm_reg_reg(
                        crate::codegen::x86_64::opcode::AluOp::Add,
                        crate::codegen::shared::asm_operand::Width::B64,
                        0,
                        1,
                    )); // add rax, rcx
                }
            }
            self.emit_seg(self.lvalue_seg_prefix(e));
            self.emit_load_mem_typed(es as usize, elem_ty);
            if dst != 0 && !matches!(elem_ty, Type::Float | Type::Double) {
                self.mov(dst, 0);
            }
        }
    }

    /// gen_expr 中 Expr::Member arm 的搬迁体, 逐字复制, 未改逻辑。
    pub fn gen_expr_member_rvalue(
        &mut self,
        expr: &Expr,
        field: &Ident,
        is_arrow: &bool,
        ty: &Type,
        dst: u8,
    ) {
        // struct/union 成员访问: 基址 + 偏移
        // 1. 计算基址 (struct指针或struct变量地址)
        if *is_arrow {
            self.gen_expr(expr, dst);
        } else {
            if let Expr::Ident { name, .. } = expr {
                if self.is_global_not_local(&name.name) {
                    self.emit_global_addr(dst, &name.name);
                } else {
                    let off = self.var_offset(&name.name);
                    // VLA/VM struct 变量: [off] 存数据基址, 须加载
                    // 而非 lea 槽地址 (否则寻址到元数据槽,
                    // pr82210: s.a[i].c 写坏基址槽)
                    if self.vla_offsets.contains_key(&name.name) {
                        self.load_stack(dst, off);
                    } else {
                        // `lea off(%r13), dst`：目的寄存器在 ModRM.reg ⇒ dst>=8 需要的是
                        // REX.R。这三行以前手写 `0x49`（W+B，给的是基址 %r13）加
                        // `0x45|((dst&7)<<3)`，于是 dst>=8 时 reg 字段被截成 dst&7 而不扩：
                        // `lea -72(%r13), %r8` 实际编成 `%rax`，地址落错了号。
                        // 单源入口 `emit_lea_r13` 已经在算 REX.R，这里直接用它。
                        self.emit_lea_r13(dst, off);
                    }
                }
            } else {
                // 通用路径: 嵌套成员/下标等复合 lvalue (如 s.a.b)
                if !self.gen_lvalue_addr(expr, dst) {
                    self.gen_expr(expr, dst);
                    // If expr is a non-lvalue aggregate (e.g. function
                    // call returning struct), dst holds the struct VALUE
                    // not a pointer. We must spill it to a temp slot
                    // and use that slot's address as the base.
                    // Exception: CompoundLit already returns an ADDRESS
                    // (the stack slot where it was built), so we can
                    // use dst directly without spilling.
                    let expr_ty = expr.ty();
                    let is_compound = matches!(expr, Expr::CompoundLit { .. });
                    if matches!(expr_ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _))
                        && !is_compound
                    {
                        let struct_size = expr_ty.size();
                        // 物化到帧临时区 result_off (r13 相对)。
                        // 不能用动态 sub rsp 槽: 该槽在表达式求值
                        // 期间从不回收, 泄漏使后续 rsp 相对寻址
                        // (emit_sysv_call 的参数暂存槽/对齐垫) 全部
                        // 错位 — f().a 作 printf 实参时段错误/乱码。
                        // SysV ABI: >16B (MEMORY 类) 返回时 rax 已是
                        // 被调方写入的 sret 缓冲地址, 无需物化。
                        if struct_size > 16 {
                            // dst (rax) 已指向返回值, 保持不变
                        } else {
                            // <=8B 全值在 rax; >8B 且 <=16B 在 rax:rdx
                            let result_off = self.complex_result_off();
                            self.emit_store(0, result_off, 8);
                            if struct_size > 8 {
                                self.emit_store(2, result_off + 8, 8);
                            }
                            // dst = 指向物化后的 struct 值
                            self.emit_lea_r13(dst, result_off);
                        }
                    }
                }
            }
        }
        // 2. 查找字段信息 (支持匿名 struct/union), 加上字段偏移
        //    注意: `->` 时 expr.ty() 是指针类型, 须解引用后再查字段
        let expr_ty = expr.ty();
        let target_ty =
            self.resolve_complete_struct_ty(Self::member_target_ty(expr_ty.clone(), *is_arrow));
        let field_info = match &target_ty {
            Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, false) => {
                find_field(fields, &field.name).map(|(f, extra)| {
                    let mut fc = f.clone();
                    fc.offset += extra;
                    fc
                })
            },
            _ => None,
        };
        // VM struct 运行时成员偏移 (目标字段在首 VLA 字段之后,
        // pr82210): 偏移已加到 dst, 跳过静态偏移加法
        let vm_off_done = matches!(&target_ty, Type::Struct(_, _, _, _))
            && self.gen_vm_member_offset(&target_ty, &field.name, dst);
        // DEBUG: trace field resolution
        if std::env::var("DCC_DEBUG_FIELD").is_ok() {
            if field.name == "pBt"
                || (matches!(&expr_ty, Type::Ptr(p) if matches!(&p.base, Type::Struct(s,_,_,_) if s.as_str() == "Db")))
            {
                eprintln!("[FIELD] name={} arrow={} expr_ty={:?} target_ty={:?} field_info={:?} ty={:?} vm_off_done={}",
                    field.name, is_arrow, expr_ty, target_ty,
                    field_info.as_ref().map(|f| (f.offset, format!("{:?}", f.ty))), ty, vm_off_done);
            }
        }
        if std::env::var("DCC_DBG_NOFIELD").is_ok() && field_info.is_none() {
            eprintln!(
                "[NOFIELD] field={} arrow={} expr_ty={:?} target_ty={:?}",
                field.name, is_arrow, expr_ty, target_ty
            );
        }
        if std::env::var("DCC_DBG_CHAIN").is_ok()
            && (field.name == "pKeyInfo" || field.name == "pSorter" || field.name == "aSortFlags")
        {
            let sname = |t: &Type| -> String {
                match t {
                    Type::Struct(n, _, _, _) | Type::Union(n, _, _, _) => n.clone(),
                    _ => crate::ast::type_kind_summary(t),
                }
            };
            let shape = |e: &Expr| -> &'static str {
                match e {
                    Expr::Member { is_arrow, .. } => {
                        if *is_arrow {
                            "M->"
                        } else {
                            "M."
                        }
                    },
                    Expr::Ident { .. } => "id",
                    Expr::Deref { .. } => "deref",
                    Expr::Index { .. } => "idx",
                    _ => "other",
                }
            };
            eprintln!("[CHAIN] {} field={} arrow={} expr_ty={} target={} off={:?} fty={} obj_shape={} obj_ty={} vm={}",
                self.current_func_name, field.name, is_arrow,
                crate::ast::type_kind_summary(&expr_ty), sname(&target_ty),
                field_info.as_ref().map(|f| f.offset),
                field_info.as_ref().map(|f| crate::ast::type_kind_summary(&f.ty)).unwrap_or_default(),
                shape(expr),
                crate::ast::type_kind_summary(&expr.ty()), vm_off_done);
        }
        if !vm_off_done {
            if let Some(ref f) = field_info {
                let field_off = f.offset as i32;
                if field_off != 0 {
                    // dst 在 ModRM.r/m 上（`81 /0 id`），扩展位是 REX.B；写 0x44(REX.R)
                    // 会把 /0 抬成 /8 = 未定义操作码。实测 `4c 81 c4` 被 objdump 读成
                    // `rex.WR add $0x1,%rsp`。
                    let rex = 0x48 | if dst >= 8 { 0x01 } else { 0 }; // 位序: REX.B —— 该寄存器只在 ModRM.r/m 或 opcode 低 3 位; 机器判据见 rex_matrix, 产物级见 scripts/check_rex_hygiene.sh
                    use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                    let gas = format!("add ${}, {}", field_off, GAS_REG64[(dst & 15) as usize]);
                    let mut bytes = vec![rex, 0x81, 0xC0 | (dst & 7)];
                    bytes.extend_from_slice(&field_off.to_le_bytes());
                    self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
                }
            }
        }
        // 3. 加载成员值 (位域 vs 普通)
        //
        // 段前缀：`p->a` / `q[i].a` / `p->a.b` 的基址可能是 `__seg_gs` 指针，而地址只是
        // 在普通算术后加上去的偏移 —— 前缀必须贴在**每一笔**访存前，故先算一次、各站点复用。
        let seg = self.seg_prefix_of_base(expr);
        // 数组字段 (如 decNumber 的 Unit lsu[1]): 值上下文中衰减为
        // 首元素地址 — dst 已持有该地址, 不得再按元素大小加载值,
        // 否则 `p = s->arr` 会把 arr[0] 的值当作地址.
        let field_is_array = match &field_info {
            Some(f) => matches!(f.ty, Type::Array(_, _)),
            None => matches!(ty, Type::Array(_, _)),
        };
        if !field_is_array {
            if let Some(ref f) = field_info {
                if let Some(bw) = f.bit_width {
                    // 位域读取: 加载容器 → 右移 → 掩码
                    let container_size = f.ty.size();
                    // 加载容器值：自反 ModRM（同一个号既在 reg 又在 r/m）+ rm==4/5
                    // 的 SIB/ disp8 规则，全部收收在 `emit_load_self_typed` 里（单一真源，
                    // 理由见那个入口的注释）。
                    self.emit_seg(seg);
                    if matches!(container_size, 1 | 2 | 4 | 8) {
                        // 位域容器一律按无符号取回（符号扩展由下面的位宽自己管）。
                        self.emit_load_self_typed(container_size, false, dst);
                    }
                    // 右移 bit_offset 位
                    if f.bit_offset > 0 {
                        // `C1 /5 ib` (shr dst, imm8, B64): dst 在 r/m → REX.B.
                        // 走 Bucket T 强制 C1 形式构造器 (即使 imm=1 也发 4 字节).
                        self.emit_insn(&Insn::shift_reg_imm8_c1_b64(
                            0b101,
                            dst,
                            f.bit_offset as u8,
                        ));
                    }
                    // 掩码: and dst, (1 << bw) - 1
                    if bw < 64 {
                        let mask: u64 = if bw == 64 { u64::MAX } else { (1u64 << bw) - 1 };
                        // imm32 在 REX.W 下会符号扩展到 64 位,
                        // 所以 >0x7FFFFFFF 的掩码不能用 imm32 编码
                        // (例如 bw=32 时 mask=0xFFFFFFFF 会被
                        // 符号扩展为 0xFFFFFFFFFFFFFFFF, 变成 no-op).
                        if mask <= 0x7FFFFFFF {
                            // 32 位有符号 imm32 可安全编码 (81 /4 id, B64).
                            self.emit_insn(&Insn::alu_rm_reg_imm32_b64(4, dst, mask as i64));
                        } else {
                            // >32 位掩码: movabs tmp, mask; and dst, tmp
                            // 使用 rcx 作为临时寄存器 (如 dst==rcx 则用 rdx)
                            let tmp = if dst != 1 { 1 } else { 2 }; // rcx or rdx
                                                                    // movabs tmp, imm64: Bucket T 强制 B8+rd 形式 (不能走 mov_imm
                                                                    // 的 C7 /0 符号扩展短形式, 否则高 32 位 mask 会被截断).
                            self.emit_insn(&Insn::movabs_reg_imm64(tmp, mask));
                            // and dst, tmp: tmp 在 reg 字段(REX.R), dst 在 r/m(REX.B).
                            self.emit_insn(&Insn::alu_rm_reg_reg(
                                crate::codegen::x86_64::opcode::AluOp::And,
                                crate::codegen::shared::asm_operand::Width::B64,
                                dst,
                                tmp,
                            ));
                        }
                    }
                    // 有符号位域: 符号扩展
                    // 枚举位域符号性: GCC 对全部枚举值非负的 enum
                    // 取 unsigned int 作兼容类型, 此时位域按无符号
                    // 解释 (不得符号扩展); 含负值则为 int.
                    let is_unsigned = match &f.ty {
                        Type::UInt
                        | Type::ULong
                        | Type::ULongLong
                        | Type::UShort
                        | Type::UChar
                        | Type::Bool => true,
                        Type::Enum(_, values, underlying) => match underlying {
                            Some(u) => !Self::is_signed_int(&u.base),
                            None => values.iter().all(|(_, v)| *v >= 0),
                        },
                        _ => false,
                    };
                    if !is_unsigned && bw < 64 {
                        let sign_bit: u64 = 1u64 << (bw - 1);
                        let inv_mask: u64 = !((1u64 << bw) - 1);
                        if sign_bit <= 0xFFFFFFFF {
                            // 32 位符号位: 可用 imm32 编码 (REX.W F7 /0 id, B64).
                            // test dst, sign_bit.
                            self.emit_insn(&Insn::test_rm_reg_imm32_b64(dst, sign_bit as i64));
                            // jz +7 (跳过符号扩展): rel8 短跳, 固定偏移.
                            self.emit_insn(&Insn::short_jump_rel8(0x74, 0x07));
                            // or dst, inv_mask (填充高位, 81 /1 id, B64).
                            self.emit_insn(&Insn::alu_rm_reg_imm32_b64(1, dst, inv_mask as i64));
                        } else {
                            // >32 位: 用临时寄存器 (rcx/rdx)
                            let tmp = if dst != 1 { 1 } else { 2 };
                            // movabs tmp, sign_bit: Bucket T 强制 B8+rd 形式.
                            self.emit_insn(&Insn::movabs_reg_imm64(tmp, sign_bit));
                            // test dst, tmp: tmp 在 reg(REX.R), dst 在 r/m(REX.B).
                            self.emit_insn(&Insn::test_rm_reg(
                                crate::codegen::shared::asm_operand::Width::B64,
                                dst,
                                tmp,
                            ));
                            // jz +N (跳过 movabs + or)
                            // movabs tmp, inv_mask (2+8=10 bytes) + or (3 bytes) = 13 bytes.
                            self.emit_insn(&Insn::short_jump_rel8(0x74, 13));
                            // movabs tmp, inv_mask.
                            self.emit_insn(&Insn::movabs_reg_imm64(tmp, inv_mask));
                            // or dst, tmp: tmp 在 reg(REX.R), dst 在 r/m(REX.B).
                            self.emit_insn(&Insn::alu_rm_reg_reg(
                                crate::codegen::x86_64::opcode::AluOp::Or,
                                crate::codegen::shared::asm_operand::Width::B64,
                                dst,
                                tmp,
                            ));
                        }
                    }
                } else if let Type::Complex(inner) = &f.ty {
                    // 复数字段: 按 complex rvalue 统一约定物化到
                    // result_off (rax=地址)。不能按标量加载到 dst —
                    // spill/比较端把 rax 当源地址 rep movsb, 标量加载
                    // 会让它以位模式值为指针 (foo().x 段错误)。
                    let isz = inner.base.size();
                    let result_off = self.complex_result_off();
                    self.mov(6, dst); // rsi = 字段地址
                    self.emit_lea_r13(7, result_off); // rdi = 物化槽
                    self.mov_imm(1, (isz * 2) as i128);
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld));
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb)); // cld; rep movsb
                    if matches!(inner.base, Type::Float | Type::Double | Type::LongDouble) {
                        // 浮点 inner 结果约定: xmm0 = 实部
                        let pfx = if matches!(inner.base, Type::Float) {
                            0xF3u8
                        } else {
                            0xF2u8
                        };
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off);
                    }
                    self.emit_lea_r13(0, result_off);
                } else {
                    // 普通字段: 直接加载
                    let size = f.ty.size();
                    // float/double/long double 字段: 只加载到 xmm0
                    // 不能同时做 GP 加载，否则会把浮点位模式当整数写入 dst，
                    // 后续 mixed-type 比较（如 float > int）会错误地把该
                    // 位模式当作整数值用 cvtsi2ss 转换成浮点数。
                    if matches!(f.ty, Type::Float | Type::Double | Type::LongDouble) {
                        // 单一真源：long double 必须 `fld tbyte [dst]`（旧写法
                        // movsd 只读 16 字节槽的低 8 字节，依赖“高 8 字节为 0
                        // 且就是 f64 位型”的旧不变量）。
                        self.emit_seg(seg);
                        self.emit_load_scalar_float_at(&f.ty, dst);
                    } else {
                        let is_signed = !matches!(
                            &f.ty,
                            Type::Bool
                                | Type::UChar
                                | Type::UShort
                                | Type::UInt
                                | Type::ULong
                                | Type::ULongLong
                                | Type::UInt128
                        );
                        self.emit_seg(seg);
                        // 按类型装载（自反 ModRM 的 REX/SIB 规则单源，见
                        // `emit_load_self_typed`）；非 1/2/4/8 的形状以前也不发任何字节。
                        if matches!(size, 1 | 2 | 4 | 8) {
                            self.emit_load_self_typed(size, is_signed, dst);
                        }
                    }
                }
            } else {
                let size = ty.size();
                // float/double/long double 字段: 只加载到 xmm0
                // 不能同时做 GP 加载，否则会把浮点位模式当整数写入 dst，
                // 后续 mixed-type 比较（如 float > int）会错误地把该
                // 位模式当作整数值用 cvtsi2ss 转换成浮点数。
                if matches!(ty, Type::Float | Type::Double | Type::LongDouble) {
                    // 同上：单一真源，long double 走 `fld tbyte`。
                    self.emit_seg(seg);
                    self.emit_load_scalar_float_at(ty, dst);
                } else {
                    let is_signed = !matches!(
                        ty,
                        Type::Bool
                            | Type::UChar
                            | Type::UShort
                            | Type::UInt
                            | Type::ULong
                            | Type::ULongLong
                            | Type::UInt128
                    );
                    self.emit_seg(seg);
                    // 同上：字段信息查不到时的那份同形代码，也收敛到同一个入口
                    // （以前两处各抄一遍字节，就是「同一个事实有两份副本」的现场）。
                    if matches!(size, 1 | 2 | 4 | 8) {
                        self.emit_load_self_typed(size, is_signed, dst);
                    }
                }
            }
        }
    }

    /// gen_expr 中 Expr::Ident arm 的搬迁体, 逐字复制, 未改逻辑。
    pub fn gen_expr_ident_rvalue(&mut self, e: &Expr, name: &Ident, ty: &Type, dst: u8) {
        if matches!(ty, Type::Array(_, _)) {
            // 数组标识符在值上下文中衰减为首元素地址 (C 数组到指针
            // 转换): int *p = arr; / return arr; / x ? arr1 : arr2。
            // 此前直接 gen_expr 会误读 a[0] 的值 (mov 而非 lea)。
            return self.gen_expr_decay(e, dst);
        }
        if matches!(ty, Type::Func(_, _)) && !self.var_map.contains_key(&name.name) {
            // 函数名作为值: 退化为函数地址
            // 沿作用域链解析嵌套函数引用 (支持兄弟/祖先定义的嵌套函数)
            if let Some((unique, hops, offs)) = self.resolve_nested_func_ref(&name.name) {
                // trampoline 地址 (hops>0 时经静态链访问定义父帧)
                self.load_trampoline_addr(dst, &unique, hops, &offs);
            } else {
                let effective_name = self
                    .nested_func_renames
                    .get(&self.current_func_name)
                    .and_then(|m| m.get(&name.name))
                    .cloned()
                    .unwrap_or_else(|| name.name.clone());
                self.emit_func_addr(dst, &effective_name);
            }
        } else if matches!(ty, Type::Vector(_, _)) {
            // 向量变量: 标量逐元素加载到帧临时区 slot 0
            self.gen_vector_load(e);
        } else if self.is_global_not_local(&name.name) && matches!(ty, Type::Int128 | Type::UInt128)
        {
            self.gen_int128_load(&name.name);
            if dst != 0 {
                self.mov(dst, 0);
            }
        } else if self.is_global_not_local(&name.name) {
            // 浮点全局变量加载到 xmm0 (浮点值约定, 同局部变量);
            // 整数路径 emit_global_load 会把 double 装进 GP 寄存器,
            // 与 gen_float_binop 期望的 xmm0 不符 (TLS/普通全局都受影响)
            if matches!(ty, Type::Float | Type::Double | Type::LongDouble)
                || matches!(ty, Type::Imaginary(_))
            {
                self.emit_global_float_load(&name.name, ty);
            } else if matches!(ty, Type::Complex(_)) {
                // _Complex 全局变量: 加载实部和虚部到帧临时区
                // rax 指向结果区, 供后续 spill_complex_operand 使用
                let inner_sz = match ty {
                    Type::Complex(ref inner) => inner.size(),
                    _ => 8,
                };
                let total_sz = inner_sz * 2;
                // 统一物化到 result_off (dcc complex rvalue 约定:
                // result_off 内存对 + xmm0 实部 + rax=地址),
                // 消费端 (gen_complex_part 非 lvalue 分支等) 按
                // result_off 读取; 旧目标 vector_temp_base 与
                // 消费端约定错位。
                let result_off = self.complex_result_off();
                let is_float_inner = matches!(
                    ty,
                    Type::Complex(ref inner)
                        if matches!(inner.base, Type::Float | Type::Double | Type::LongDouble)
                );
                let inner = match ty {
                    Type::Complex(ref c) => c.base.clone(),
                    _ => Type::Double,
                };
                if is_float_inner {
                    // long double 内层: 每个分量按 80-bit tbyte 加载到 st(0),
                    // 直接 fstp 落 result_off 的 16 字节槽;
                    // float/double 内层: global → xmm0 → movss/movsd 落槽。
                    if matches!(inner, Type::LongDouble) {
                        self.emit_global_float_load_x87_offset(&name.name, 0, &inner);
                        self.emit_x87_fstp_at(13, result_off as i32, &inner);
                        self.emit_global_float_load_x87_offset(&name.name, inner_sz as i32, &inner);
                        self.emit_x87_fstp_at(13, (result_off + inner_sz as i64) as i32, &inner);
                    } else {
                        let pfx = if matches!(inner, Type::Float) { 0xF3u8 } else { 0xF2u8 };
                        // 加载实部: global → xmm0 → [r13+result_off]
                        self.emit_global_float_load_to_xmm(0, &name.name, &inner);
                        self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x11], result_off);
                        // 加载虚部: global+inner_sz → xmm0 → [r13+result_off+inner_sz]
                        self.emit_global_float_load_to_xmm_offset(
                            0,
                            &name.name,
                            inner_sz as i32,
                            &inner,
                        );
                        self.emit_xmm0_r13_disp(
                            &[pfx, 0x41, 0x0F, 0x11],
                            result_off + inner_sz as i64,
                        );
                    }
                } else {
                    // 整数 complex 全局: 加载 16 字节到临时区
                    self.emit_global_addr(0, &name.name);
                    // rax = global addr, 拷贝到临时区
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                            rip_relative: false,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                6,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    )); // mov rsi, rax
                    self.emit_lea_r13(7, result_off); // rdi = r13 + result_off
                    self.mov_imm(1, total_sz as i128);
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld));
                    self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb));
                    // cld; rep movsb
                }
                self.emit_lea_r13(0, result_off);
            } else {
                self.emit_global_load(dst, &name.name);
            }
        } else if matches!(ty, Type::Complex(_))
            && !(self.current_func_is_nested && self.upvar_offsets.contains_key(&name.name))
        {
            // _Complex 局部变量: 物化整体到 result_off (统一 rvalue
            // 约定: result_off 内存对 + xmm0 实部 + rax=地址)。
            // 旧实现落 mov_from_stack 按标量 8B 读, 实部/虚部全错。
            let inner_sz = match ty {
                Type::Complex(ref inner) => inner.size(),
                _ => 8,
            };
            let total_sz = inner_sz * 2;
            let result_off = self.complex_result_off();
            let src_off = self.var_offset(&name.name);
            self.emit_lea_r13(6, src_off); // rsi = &var
            self.emit_lea_r13(7, result_off); // rdi = result_off
            self.mov_imm(1, total_sz as i128);
            self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cld));
            self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RepMovsb)); // cld; rep movsb
            let pfx = if matches!(ty, Type::Complex(ref inner) if matches!(inner.base, Type::Float))
            {
                0xF3u8
            } else {
                0xF2u8
            };
            self.emit_xmm0_r13_disp(&[pfx, 0x41, 0x0F, 0x10], result_off);
            self.emit_lea_r13(0, result_off);
        } else if matches!(ty, Type::Float | Type::Double | Type::LongDouble)
            || matches!(ty, Type::Imaginary(_))
        {
            self.gen_float_load(&name.name, ty);
        } else if self.current_func_is_nested
            && self.upvar_offsets.contains_key(&name.name)
            && matches!(ty, Type::Int128 | Type::UInt128)
        {
            let parent_off = self.upvar_offsets.get(&name.name).copied().unwrap_or(0);
            self.load_upvar(0, parent_off, 8, &name.name);
            self.load_upvar(2, parent_off + 8, 8, &name.name);
            if dst != 0 {
                self.mov(dst, 0);
            }
        } else if self.current_func_is_nested && self.upvar_offsets.contains_key(&name.name) {
            let parent_off = self.upvar_offsets.get(&name.name).copied().unwrap_or(0);
            self.load_upvar(dst, parent_off, ty.size(), &name.name);
        } else {
            // P1-4: __int128 变量加载 (两个 64 位槽)
            if matches!(ty, Type::Int128 | Type::UInt128) {
                let off = self.var_offset(&name.name);
                self.load_stack(dst, off); // low 64 bits
                self.load_stack(2, off + 8); // high 64 bits → rdx
                if dst != 0 { /* rax already set */ }
            } else {
                self.mov_from_stack(dst, &name.name);
            }
        }
        // 整数提升: char/short → int
        if ty.is_int() {
            self.gen_int_promote(dst, ty);
        }
    }
}
