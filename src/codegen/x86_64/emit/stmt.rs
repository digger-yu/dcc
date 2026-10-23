// src/codegen/x86_64/emit/stmt.rs
// Statement code generation for x86_64 backend.
// 本文件由 mod.rs 逐字搬迁而来 (batch 2): `gen_stmt` 原样剪切至 `impl Codegen`,
// 语义 diff 为 0。

use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::{AbiClass, AggDst, Codegen, JumpKind};

use crate::const_eval;

// ---------------------------------------------------------------------------
// 布局扰动旋钮（P13 阶段6 批0c；它们**不是**优化开关）
// ---------------------------------------------------------------------------
//
// 为什么需要它们：本仓已经有一次「删 135 条 to-next `jmp`，静态变小、实测慢 3.8%」的
// 负结果，而本机 PMU 全部不可用（`perf_event_paranoid=4`，`cycles`/`instructions`/
// `branch-misses` 均 `<not supported>`，也没有 valgrind）。「只减取指体量」那一类改动
// （冷路径外置、tail merging）因此在本机没有直接判据。唯一的出路是先量出本机
// `d(runtime)/d(footprint)` 的斜率：用一组「语义逐字节相同、只有布局不同」的产物做
// 滴定，再用斜率去折算某个改动的体量收益值多少 ms。这两个旋钮就是滴定用的砝码。
//
// 只用单字节 `nop`（`0x90`）：多字节 `nopw`/`cs` 形态会把
// `scripts/check_asm_text_parity.sh`（它只跳精确 `nop`）直接打成 PARITY-FAIL，
// 也会让主代理量 `E` 的 nop 过滤漏网。两个旋钮默认 0 ⇒ 一条指令都不发，
// 产物与本改动之前逐字相同（`scripts/aa_layout.sh` 会断言这一点）。

// std::sync::OnceLock 各自只读一次 env：布局规则在发射途中变化会让同一 TU 的
// 两段按不同规则发射，那种产物没人能复现（与 `blockopt::level()` 同一条纪律）。
fn fn_shift() -> u64 {
    static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *V.get_or_init(|| read_layout_knob("DCC_FN_SHIFT"))
}

fn handler_pad() -> u64 {
    static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *V.get_or_init(|| read_layout_knob("DCC_HANDLER_PAD"))
}

fn read_layout_knob(name: &str) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

impl Codegen {
    /// `goto *表[下标]` 且那张表的基址已常驻 `%rbp` ⇒ 只发一条
    /// `jmp *(%rbp,%rcx,stride)`（阶段 6 批 4）。
    ///
    /// 返 `false` = 不合形状，调用方走原来的三步链。每一条拒接都必须是「退回原路」而不是
    /// 「换个形态发出」：这一跳的落点错了就是跳进别的函数（实测同款症状是 SIGSEGV）。
    ///
    /// 与 [`crate::codegen::x86_64::emit::expr::Codegen::gen_expr_index_rvalue`] 的 SIB 快车道
    /// 用**同一份**语义层描述符与同一个暂存号（`%rcx`），所以两条路算出的地址逐位相同；
    /// 下标也只求值一次（`Expr::Index` 的下标可以有副作用，如 `disptab[*(pc++)]`）。
    fn emit_hoisted_jumptable_jump(&mut self, expr: &Expr) -> bool {
        let Some(sym) = self.disptab_base_reg_sym.clone() else {
            return false;
        };
        let Some(site) = crate::ast_dispatch::indirect_table_site(expr) else {
            return false;
        };
        // 站点读的那张表必须就是被常驻的那一张（源码名 → mangled 同一入口）。
        if self.static_local_map.resolve(&self.current_func_name, site.table) != Some(sym.as_str())
        {
            return false;
        }
        // 段限定前缀只由原路那侧处理（`emit_load_global_indexed`），这里不抢。
        // 静态局部不可能带 `__seg_*`，但判据不能靠「不可能」。
        if self.global_seg_prefix(&sym).is_some() {
            return false;
        }
        use crate::semantics::{
            compute_lvalue_address, determine_action, AddressBase, AddressContext, ByteOffset,
            MaterializeAction,
        };
        let ctx = AddressContext {
            is_nested: self.current_func_is_nested,
        };
        let desc = compute_lvalue_address(self, expr, &ctx);
        if !matches!(determine_action(site.elem, true), MaterializeAction::LoadScalar) {
            return false;
        }
        // 基址是这张表的符号、偏移是**单个**运行时下标项且无补数：与 SIB 快车道逐字同判据。
        let (stride, idx) = match (desc.base, desc.offset) {
            (
                AddressBase::GlobalSymbolAddr { symbol_key },
                ByteOffset::Runtime { mut terms, addend: 0 },
            ) => {
                if symbol_key != sym || terms.len() != 1 {
                    return false;
                }
                let term = terms.pop().expect("terms.len()==1 已在上面判过");
                if !matches!(term.stride, 1 | 2 | 4 | 8) {
                    return false;
                }
                (term.stride as u8, term.index)
            },
            _ => return false,
        };
        // 下标求值与原路逐字相同：同一个表达式（由描述符递出来，不是自己再拆一遍 AST）、
        // 同一个暂存号 `%rcx`。原路紧接着把项读进 rax 再 `jmp *rax`，而这一条把「读」
        // 并进了跳转：rax 不再承载目标地址，也没有任何后续指令读它。
        let idx_reg: u8 = 1;
        self.gen_expr(&idx, idx_reg);
        // 不挂 gas：文本由 `opcode.rs` 里 `JmpRmMem` 的 render 分支给（与字节同源）。
        if !self.emit_insn(&Insn::jmp_rm_mem_sib(5, idx_reg, stride)) {
            // 到这里意味着编码器拒了这个形态（号与乘数都已在上面筛过，理论上不可达）。
            // 兜底只有一条规矩：**必须仍然发出一条跳转**。只报错不跳会把这条分派落到
            // 下一条指令上，那比错值更难查。原路那两条本来就编得出（mov 那侧带 disp8）。
            self.emit_load_global_indexed(0, idx_reg, &sym, stride, site.elem);
            self.emit_insn(&Insn::jmp_rm_reg(0)); // jmp *%rax
        }
        true
    }

    /// 记录「本条语句的起始 PC ↔ 源码行」，供 `.debug_line` 用（Phase 6.2 收尾）。
    ///
    /// 为什么以前没有：x86_64 后端只在内联 asm 的发射路径里记过行号（`asm/emit.rs` 那两张
    /// 表），于是纯 C 函数交给 `src/debug.rs` 的 `line_entries` 恒空，行表退到「每函数
    /// 一行（声明行）」的兜底形态：函数断点能绑（靠 `.debug_info` 的 low_pc/high_pc），
    /// 而 `break file.c:5` 这种语句级断点必然报 `No line 5 in file`。
    ///
    /// 只在 `-g` 下攒表：`debug_level == 0` 时这张表没有消费者，白白多一堆 push。
    /// 行号为 0（合成语句、`Span::unknown()`）不记：0 不是合法源码行，绑上去只会让 gdb
    /// 停在一个屏幕上看不到的位置。
    pub(crate) fn note_stmt_line(&mut self, s: &Stmt) {
        self.note_src_line(stmt_src_line(s));
    }

    /// `note_stmt_line` 的底层：行号已取好，只负责守卫与去重。
    ///
    /// 单独拆出来是因为局部声明（`int t = sink(0);`）不是 `Stmt`，而是
    /// `BlockItem::Decl(Decl::Var(..))`：不给它记一条，声明行上仍然绑不上断点。
    pub(crate) fn note_src_line(&mut self, line: u32) {
        if self.debug_level == 0 {
            return;
        }
        if line == 0 {
            return;
        }
        let pc = self.code.len() as u64;
        // 同一 PC 上同一行再记一次没有信息量（外层语句紧接着内层首条语句），而
        // 同 PC 不同行是合法的语句重叠，必须保留。
        //
        // 去重键**必须含 PC**，只看行号会把「相邻两个不同地址上的同一行」也吞掉：
        // 这张表在串行侧是跨函数累积的，而函数克隆（`__attribute__((target_clones))`
        // / multiversioning）与相邻函数很容易落在同一行上 ⇒ 串行少行、并行（每个 worker
        // 一张新表）不少行，`-O2 -g` 的 `.debug_line`/`.rela.debug_line`/`.BTF.ext`
        // 三节对不上（实测 4 个 clone 夹具）。那处差异不在 PC 上，按行号去重无法区分，
        // 也就守不住「并行不改产物」这条不变式。
        if let Some(&(last_pc, last_line)) = self.stmt_line_entries.last() {
            if last_line == line && last_pc == pc {
                return;
            }
        }
        self.stmt_line_entries.push((pc, line));
    }

    pub fn gen_stmt(&mut self, s: &Stmt) {
        self.note_stmt_line(s);
        match s {
            Stmt::Return { val, .. } => {
                if let Some(e) = val {
                    // 数组表达式在返回值上下文中衰减为指针
                    // (int *f() { int a[4]; return a; } → 返回 &a)。
                    // 若不衰减, 12B+ 数组会误走聚合按值返回路径 (读内容)。
                    let ret_ty = match e.ty() {
                        Type::Array(elem, _) => Type::Ptr(elem),
                        t => t,
                    };
                    let ret_sz = ret_ty.size();
                    // A1: 内存返回 (sret) — 聚合/长双精度整体写入 rdi 指向的缓冲区.
                    // 函数声明返回 long double 时, 即使表达式类型是 int/double
                    // (隐式转换), 也必须走 sret 写回 (调用方从 sret 槽读值).
                    let func_ret_is_ld = matches!(self.current_func_ret_ty, Type::LongDouble);
                    // P4 V4: `_Imaginary long double` 与 `long double` 对称 — rvalue
                    // 已在 st(0), 不需要任何中转 (emit_operand_to_st0 对 LD 是 no-op)。
                    let func_ret_is_imag_ld = matches!(
                        &self.current_func_ret_ty,
                        Type::Imaginary(i) if matches!(&i.base, Type::LongDouble)
                    );
                    if (func_ret_is_ld || func_ret_is_imag_ld) && self.current_sret_offset.is_none()
                    {
                        // D10-3（已简化）：SysV 规定标量 `long double` 返回值在 **st(0)**。
                        // 本后端 `long double` 的 rvalue 值通道已经就是 st(0)，所以
                        // 返回表达式本身就是 `fldt`/x87 运算的终点，**不需要任何中转**。
                        // 只有当被返回的表达式类型不是 st(0) 通道（整型 / f32 / f64）
                        // 时才需要一次精确拓宽：整数走 `fild`（80-bit 有 64 位显式
                        // 尾数，任意 int64/u64 无损），f32/f64 走内存中转 `fld`（同样是
                        // 子集拓宽，不引入舍入）。旧实现无条件 spill xmm0→`fld qword`，
                        // 那是在适配“ld 内部只有 f64 精度”的旧不变量，会把真 80-bit
                        // 返回值重新舍回 53 位尾数。
                        let e_ty = e.ty();
                        self.gen_expr(e, Self::EVAL_BASE);
                        self.emit_operand_to_st0(&e_ty, 0);
                    } else if matches!(self.current_func_ret_ty, Type::Complex(_))
                        && self.current_sret_offset.is_none()
                    {
                        // 复数返回的**交付与隐式转换都按声明类型决定**: `ret_ty`
                        // 只是表达式类型, `cd f(int n){ return n; }` 的它是 int ——
                        // 若按表达式判分就会把 int 的位模式当复数对交回 (实测
                        // 分量全 0/垃圾), 且遗漏 C6.3.1.8p1 的标量→复数与
                        // p4 的异内型逐分量转换。两者都住在 `gen_complex_return`
                        // 里 (与形参侧共用 `complex_src_needs_convert` 单一真源)。
                        let cx_ty = self.current_func_ret_ty.clone();
                        self.gen_complex_return(e, &cx_ty);
                    } else if (Self::needs_memory_return(&ret_ty)
                        || func_ret_is_ld
                        || Self::needs_memory_return(&self.current_func_ret_ty))
                        && self.current_sret_offset.is_some()
                    {
                        // 同样按**声明**返回类型判分: 整数内型 `_Complex` 被
                        // `leaf_abi_class` force 成 Memory 类, 而 `cd`/`cf` 的表达式
                        // 类型可能是标量 (`ci f(int n){return n;}` 的 ret_ty 是 int),
                        // 只看表达式会整条 sret 臂落空。缓冲区尺寸/转换在
                        // `gen_sret_return` 内按声明类型处理。
                        self.gen_sret_return(e, &ret_ty);
                    } else {
                        // _Complex float (8B) 同样走 SSE 寄存器返回
                        // (xmm0=实部, xmm1=虚部), 不适用 >8B 门槛;
                        // 否则落入 gen_expr 整数路径, xmm0/xmm1 为垃圾
                        let is_agg_ret = if matches!(&ret_ty, Type::Complex(_)) {
                            true
                        } else {
                            matches!(
                                &ret_ty,
                                Type::Struct(_, _, _, _)
                                    | Type::Union(_, _, _, _)
                                    | Type::Array(_, ArraySize::Const(_))
                                    | Type::Atomic(_)
                                    | Type::Vector(_, _)
                            ) && ret_sz > 8
                        };
                        if is_agg_ret {
                            if let Type::Complex(_) = &ret_ty {
                                // 交付与隐式转换一并交给复数返回总入口
                                // (`gen_complex_return`): 返回上下文的标量→复数 /
                                // 异内型复数→复数转换 (C6.3.1.8) 必须在此发生,
                                // 否则 callee 把源的位模式当成目标分量交付。
                                self.gen_complex_return(e, &ret_ty);
                            } else {
                                // 16B [Sse, SseUp] 聚合返回值 (独立向量或含
                                // 16B 向量字段/对齐填充的小结构体): SysV ABI
                                // 全在 xmm0, 不走 rax/rdx → xmm0/xmm1 拆分.
                                // 直接 movups 16B 到 xmm0.
                                if Self::is_sseup_128_aggregate(&ret_ty) {
                                    // 动作只有一个: 把返回表达式的 16 字节**对象表示**
                                    // 整体装进 xmm0。先物化到一个帧内缓冲, 再一条
                                    // `movups` 读回 —— 物化契约 (Ident/Member/Index/
                                    // Deref 取地址、Call 走 ABI 返值、Vector 走
                                    // `vector_temp_base` 约定) 全部复用聚合赋值的同
                                    // 一真源 `gen_aggregate_assign_to`, 本站点不得重
                                    // 写任何形态判据或帧偏移。
                                    //
                                    // 旧写法在这里硬编码 `-frame_size + 8` 当作“向量
                                    // 临时槽”, 并从该处 movups。那只是一个巧合: 非变参
                                    // 函数的 `vec_temp_off` 历史上恰好等于 `-frame_size
                                    // + 8`。该巧合如今被两处正规改动打破 (帧规划为复数
                                    // rvalue 划出 `CX_FRAME_BAND` 固定带、变参函数再另加
                                    // 176 字节 save area), 硬编码因此变成读复数物化带的
                                    // 残值 —— `return (v4f){...}` / `return s;` 全栈垃圾。
                                    // 而 `-frame_size + 8` 的**真**契约是复数内存对, 更不
                                    // 能说向量临时槽。
                                    let buf_off = self.alloc_sret_slot(ret_sz.max(16));
                                    let dst = AggDst::Local {
                                        off: buf_off,
                                        vla: None,
                                    };
                                    self.gen_aggregate_assign_to(&dst, &ret_ty, e);
                                    // movups xmm0, [r13 + buf_off]
                                    self.load_xmm128_stack(0, buf_off);
                                } else {
                                    // struct 返回: 加载到 rax[:rdx]
                                    if let Expr::Ident { name, .. } = &e {
                                        if self.is_global_not_local(&name.name) {
                                            // 全局结构体变量: 用 RIP-relative 寻址加载
                                            self.emit_global_addr(0, &name.name); // rax = 全局变量地址
                                            if ret_sz <= 8 {
                                                match ret_sz {
                                                    8 => {
                                                        self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
        rip_relative: false,
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B64
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(0), index: None, seg: None,
                        }
                    ),
                },
                &[],
            ));
                                                    }, // mov rax, [rax]
                                                    4 => {
                                                        self.emit_insn(&Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
        rip_relative: false,
                    width: crate::codegen::shared::asm_operand::Width::B32,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0, crate::codegen::shared::asm_operand::Width::B32
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Mem(
                        crate::codegen::x86_64::opcode::MemOperand {
                            disp: 0, base: Some(0), index: None, seg: None,
                        }
                    ),
                },
                &[],
            ));
                                                        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
        rip_relative: false,
                                                            sign: false,
                                                            from: crate::codegen::shared::asm_operand::Width::B16,
                                                            to: crate::codegen::shared::asm_operand::Width::B64,
                                                            dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                                                            src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B16),
                                                        }, &[]));
                                                    }, // movzx
                                                    2 => {
                                                        self.emit_insn(&Insn::mov_rm_mem(
                                                            true,
                                                            crate::codegen::shared::asm_operand::Width::B16,
                                                            0,
                                                            0,
                                                            0,
                                                        ));
                                                        self.emit_insn(&Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
        rip_relative: false,
                                                            sign: false,
                                                            from: crate::codegen::shared::asm_operand::Width::B16,
                                                            to: crate::codegen::shared::asm_operand::Width::B64,
                                                            dst: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B64),
                                                            src: crate::codegen::x86_64::opcode::Operand::Reg(0, crate::codegen::shared::asm_operand::Width::B16),
                                                        }, &[]));
                                                    },
                                                    1 => {
                                                        self.emit_insn(&Insn::mov_ext_mem(
                                                            false,
                                                            crate::codegen::shared::asm_operand::Width::B8,
                                                            crate::codegen::shared::asm_operand::Width::B32,
                                                            0,
                                                            0,
                                                            0,
                                                        ));
                                                    }, // movzx al, [rax]
                                                    _ => {},
                                                }
                                            } else {
                                                // >8 字节: 先用地址取高 8 字节, 再取低 8 字节
                                                self.emit_insn(&Insn::mov_rm_mem(
                                                    true,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                    2,
                                                    0,
                                                    8,
                                                )); // mov rdx, [rax+8]
                                                self.emit_insn(&Insn::mov_rm_mem(
                                                    true,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                    0,
                                                    0,
                                                    0,
                                                )); // mov rax, [rax]
                                            }
                                        } else {
                                            let off = self.var_offset(&name.name);
                                            self.load_stack(0, off); // rax = 前 8 字节
                                            if ret_sz > 8 {
                                                self.load_stack(2, off + 8); // rdx = 后 8 字节
                                            }
                                        }
                                    } else {
                                        // CompoundLit/InitList 等: gen_expr 返回地址 (CompoundLit)
                                        // 或垃圾值 (InitList). 需要物化后加载到 rax/rdx.
                                        if matches!(
                                            e,
                                            Expr::CompoundLit { .. } | Expr::InitList { .. }
                                        ) {
                                            // 使用帧底部临时区 (-frame_size+8)
                                            let temp_off = self.complex_result_off();
                                            let inits = match e {
                                                Expr::CompoundLit { inits, .. } => Some(inits),
                                                Expr::InitList { inits, .. } => Some(inits),
                                                _ => None,
                                            };
                                            if let Some(inits) = inits {
                                                self.gen_init_list_at(temp_off, &ret_ty, inits);
                                            }
                                            self.load_stack(0, temp_off); // rax = 前 8 字节
                                            if ret_sz > 8 {
                                                self.load_stack(2, temp_off + 8);
                                                // rdx = 后 8 字节
                                            }
                                        } else {
                                            // D4: 聚合 rvalue 以「rax = 缓冲区地址」交付
                                            // (`va_arg` 聚合走 `gen_expr_va_arg` 的
                                            // `alloc_sret_slot` 路径)。本分支的交付目标
                                            // 是 SysV **寄存器**返回（[Integer,..] →
                                            // rax[:rdx]，下方再把 Sse 类 movq 进 xmm0/xmm1），
                                            // 必须按内容装载；不装载则调用方把地址字节当
                                            // 数据用（实测 `return va_arg(ap, struct Pair64)`
                                            // 的 `p.a` = 临时槽地址、`p.b` = 残留 rdx）。
                                            //
                                            // `gen_expr` 对 lvalue 表达式（Index/Member/
                                            // Deref）返回的是**地址**（rax = &obj），
                                            // 不是值本身。寄存器返回 [Int, Int] 类聚合
                                            // 必须从该地址加载 8/16 字节值。pr82388:
                                            // `return h[29];` 的 rax 是 `&h[29]`，
                                            // callee 直接把地址字节当返回值交付给
                                            // caller，main 解引用得到栈垃圾。
                                            let rax_is_agg_addr = match e {
                                                Expr::VaArg { ref ty, .. } => {
                                                    Self::va_arg_result_in_memory(ty)
                                                },
                                                // lvalue 表达式: gen_expr 输出对象地址
                                                Expr::Index { .. }
                                                | Expr::Member { .. }
                                                | Expr::Deref { .. } => true,
                                                _ => false,
                                            };
                                            self.gen_expr(e, Self::EVAL_BASE);
                                            if rax_is_agg_addr {
                                                // 先取高 8 再取低 8: 后一条会覆写 rax
                                                self.emit_insn(&Insn::mov_rm_mem(
                                                    true,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                    2,
                                                    0,
                                                    8,
                                                )); // mov rdx, [rax+8]
                                                self.emit_insn(&Insn::mov_rm_mem(
                                                    true,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                    0,
                                                    0,
                                                    0,
                                                )); // mov rax, [rax]
                                            }
                                        }
                                    }
                                    // SysV: SSE/SSEUP 类 eightbyte 返回在 xmm0/xmm1 (而非 rax/rdx),
                                    // 如 struct {double,double}. _Complex float/double 已由
                                    // gen_complex_sse_return 直接填充返回寄存器。
                                    let ret_classes = Self::abi_classes(&ret_ty);
                                    for (c, cls) in ret_classes.iter().enumerate() {
                                        if matches!(cls, AbiClass::Sse | AbiClass::SseUp) {
                                            let src = if c == 0 { 0u8 } else { 2u8 };
                                            self.emit_movq_gp_to_xmm(c as u8, src);
                                        }
                                    }
                                } // end else (non-Vector agg return)
                            }
                        } else {
                            self.gen_expr(e, Self::EVAL_BASE);
                            // Small vector (<=8B) return: gen_expr puts the
                            // vector data in temp slot 0 on the stack.  SysV
                            // ABI classifies <=16B vectors as SSE, so the
                            // return value must be in xmm0 (xmm1 for the
                            // upper 8B of 16B vectors, handled by is_agg_ret
                            // path above).  Load from temp slot 0 into xmm0.
                            if matches!(&ret_ty, Type::Vector(_, _)) && ret_sz <= 8 {
                                let toff = self.vector_temp_base(0, ret_sz);
                                // vector < 8B 不能读穿自身槽位。
                                // 按真实字节宽度加载，再把零扩展位型移入 xmm0。
                                self.load_stack_xmm(0, toff, ret_sz, false);
                            }
                            // __int128 返回值: 若函数返回 __int128 但表达式非 __int128, rdx 未设置
                            let func_ret_ty_for_128 = self.current_func_ret_ty.clone();
                            if matches!(&func_ret_ty_for_128, Type::Int128 | Type::UInt128)
                                && !matches!(e.ty(), Type::Int128 | Type::UInt128)
                            {
                                if matches!(func_ret_ty_for_128, Type::UInt128)
                                    || matches!(
                                        e.ty(),
                                        Type::Bool
                                            | Type::UChar
                                            | Type::UShort
                                            | Type::UInt
                                            | Type::ULong
                                            | Type::ULongLong
                                    )
                                {
                                    // P11: xor rdx, rdx via typed opcode.
                                    let insn = Insn::new(
                                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                            op: crate::codegen::x86_64::opcode::AluOp::Xor,
                                            width: crate::codegen::shared::asm_operand::Width::B64,
                                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                2,
                                                crate::codegen::shared::asm_operand::Width::B64,
                                            ),
                                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                2,
                                                crate::codegen::shared::asm_operand::Width::B64,
                                            ),
                                        },
                                        &[],
                                    );
                                    if !self.emit_insn(&insn) {
                                        self.emit_insn(&Insn::new(
                                            crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                                                op: crate::codegen::x86_64::opcode::AluOp::Xor,
                                                width:
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    2,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    2,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                            },
                                            &[],
                                        )); // xor rdx, rdx
                                    }
                                } else {
                                    self.emit_insn(&Insn::sign_extend_rax_to_rdx(
                                        crate::codegen::shared::asm_operand::Width::B64,
                                    )); // cqo
                                }
                            }
                            // Small struct (<=8B) returned as value in rax.
                            // CompoundLit/InitList gen_expr returns an ADDRESS
                            // in rax; we must load the VALUE from that address.
                            if matches!(&ret_ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _))
                                && ret_sz <= 8
                                && matches!(e, Expr::CompoundLit { .. } | Expr::InitList { .. })
                            {
                                match ret_sz {
                                    1 => {
                                        // movzbq (%rax),%rax: 48 0F B6 00
                                        self.emit_insn(&Insn::new(
                                            crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                                                rip_relative: false,
                                                sign: false,
                                                from:
                                                    crate::codegen::shared::asm_operand::Width::B8,
                                                to: crate::codegen::shared::asm_operand::Width::B64,
                                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                                src: crate::codegen::x86_64::opcode::Operand::Mem(
                                                    crate::codegen::x86_64::opcode::MemOperand {
                                                        disp: 0,
                                                        base: Some(0),
                                                        index: None,
                                                        seg: None,
                                                    },
                                                ),
                                            },
                                            &[],
                                        ));
                                    }, // movzbq (%rax),%rax
                                    2 => {
                                        // movzwq (%rax),%rax: 48 0F B7 00
                                        self.emit_insn(&Insn::new(
                                            crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                                                rip_relative: false,
                                                sign: false,
                                                from:
                                                    crate::codegen::shared::asm_operand::Width::B16,
                                                to: crate::codegen::shared::asm_operand::Width::B64,
                                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                                src: crate::codegen::x86_64::opcode::Operand::Mem(
                                                    crate::codegen::x86_64::opcode::MemOperand {
                                                        disp: 0,
                                                        base: Some(0),
                                                        index: None,
                                                        seg: None,
                                                    },
                                                ),
                                            },
                                            &[],
                                        ));
                                    }, // movzwq (%rax),%rax
                                    4 => {
                                        self.emit_insn(&Insn::new(
                                            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                                rip_relative: false,
                                                width:
                                                    crate::codegen::shared::asm_operand::Width::B32,
                                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B32,
                                                ),
                                                src: crate::codegen::x86_64::opcode::Operand::Mem(
                                                    crate::codegen::x86_64::opcode::MemOperand {
                                                        disp: 0,
                                                        base: Some(0),
                                                        index: None,
                                                        seg: None,
                                                    },
                                                ),
                                            },
                                            &[],
                                        ));
                                    }, // mov (%rax),%eax
                                    8 => {
                                        self.emit_insn(&Insn::new(
                                            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                                rip_relative: false,
                                                width:
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                                src: crate::codegen::x86_64::opcode::Operand::Mem(
                                                    crate::codegen::x86_64::opcode::MemOperand {
                                                        disp: 0,
                                                        base: Some(0),
                                                        index: None,
                                                        seg: None,
                                                    },
                                                ),
                                            },
                                            &[],
                                        ));
                                    }, // mov (%rax),%rax
                                    _ => {},
                                }
                            }
                            // Integer → float return conversion:
                            // `return int_expr` in a function returning
                            // float/double. The integer result is in rax;
                            // convert to xmm0.
                            let expr_ty = e.ty();
                            let func_ret_ty = self.current_func_ret_ty.clone();
                            if matches!(&func_ret_ty, Type::Float | Type::Double | Type::LongDouble)
                                && Self::is_integer_type(&expr_ty)
                            {
                                self.emit_scalar_to_xmm0_if_needed(e, &func_ret_ty);
                            }
                            // Float/double return type mismatch:
                            // convert expression type to the declared
                            // return type.  E.g. `return float_var`
                            // in a function returning double needs
                            // cvtss2sd.
                            if matches!(&expr_ty, Type::Float)
                                && matches!(&func_ret_ty, Type::Double | Type::LongDouble)
                            {
                                // cvtss2sd xmm0, xmm0
                                self.emit_insn(&Insn::simd_arith_reg(
                                    crate::codegen::x86_64::opcode::SimdArithOp::CvtSdSs,
                                    crate::codegen::x86_64::opcode::SimdWidth::SS,
                                    0,
                                    0,
                                ));
                            } else if matches!(&expr_ty, Type::Double | Type::LongDouble)
                                && matches!(&func_ret_ty, Type::Float)
                            {
                                // cvtsd2ss xmm0, xmm0
                                self.emit_insn(&Insn::simd_arith_reg(
                                    crate::codegen::x86_64::opcode::SimdArithOp::CvtSdSs,
                                    crate::codegen::x86_64::opcode::SimdWidth::SD,
                                    0,
                                    0,
                                ));
                            } else if matches!(&expr_ty, Type::LongDouble)
                                && matches!(&func_ret_ty, Type::Double)
                            {
                                // long double 在 st(0), 函数返回 double 需 80→64 舍入.
                                self.emit_x87_to_xmm0();
                            }
                            // Float/double -> integer return:
                            // convert xmm0 to integer in rax.
                            //
                            // 直接复用 `emit_implicit_float_to_int` 这一单一真源:
                            // 旧代码在这里又手写了一遍 cvtt*/cvttsd2si 编码, 并把
                            // `long double` 当作「已在 xmm0」—— 而它的值通道是 x87
                            // st(0), 于是 `int f(void){ return ld; }` 读到陈旧 xmm0
                            // 得 0 (与隐式截断同一根因)。
                            if matches!(&expr_ty, Type::Float | Type::Double | Type::LongDouble)
                                && func_ret_ty.is_int()
                            {
                                self.emit_implicit_float_to_int(0, &expr_ty, &func_ret_ty);
                            }
                            // Truncate return value to the function's
                            // declared return type width. For types smaller
                            // than int, the BinOp result may have extra bits
                            // in the register that must be cleared.
                            let ret_bits = func_ret_ty.size() * 8;
                            if func_ret_ty.is_int() && ret_bits < 64 {
                                let is_signed = Self::is_signed_int(&func_ret_ty);
                                match ret_bits {
                                    8 if is_signed => {
                                        // movsbl %al, %rax (sign-extend to 64-bit)
                                        self.emit_insn(&Insn::new(
                                            crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                                                rip_relative: false,
                                                sign: true,
                                                from:
                                                    crate::codegen::shared::asm_operand::Width::B8,
                                                to: crate::codegen::shared::asm_operand::Width::B64,
                                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B8,
                                                ),
                                            },
                                            &[],
                                        ));
                                    },
                                    8 => {
                                        // movzbq %al, %rax (zero-extend to 64-bit)
                                        self.emit_insn(&Insn::new(
                                            crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                                                rip_relative: false,
                                                sign: false,
                                                from:
                                                    crate::codegen::shared::asm_operand::Width::B8,
                                                to: crate::codegen::shared::asm_operand::Width::B64,
                                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B8,
                                                ),
                                            },
                                            &[],
                                        ));
                                    },
                                    16 if is_signed => {
                                        // movswq %ax, %rax (sign-extend to 64-bit)
                                        self.emit_insn(&Insn::new(
                                            crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                                                rip_relative: false,
                                                sign: true,
                                                from:
                                                    crate::codegen::shared::asm_operand::Width::B16,
                                                to: crate::codegen::shared::asm_operand::Width::B64,
                                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B16,
                                                ),
                                            },
                                            &[],
                                        ));
                                    },
                                    16 => {
                                        // movzwq %ax, %rax (zero-extend to 64-bit)
                                        self.emit_insn(&Insn::new(
                                            crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                                                rip_relative: false,
                                                sign: false,
                                                from:
                                                    crate::codegen::shared::asm_operand::Width::B16,
                                                to: crate::codegen::shared::asm_operand::Width::B64,
                                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B16,
                                                ),
                                            },
                                            &[],
                                        ));
                                    },
                                    32 if is_signed => {
                                        // movsxd %eax, %rax (sign-extend to 64-bit)
                                        self.emit_insn(&Insn::new(
                                            crate::codegen::x86_64::opcode::X86_64Opcode::MovExt {
                                                rip_relative: false,
                                                sign: true,
                                                from:
                                                    crate::codegen::shared::asm_operand::Width::B32,
                                                to: crate::codegen::shared::asm_operand::Width::B64,
                                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B64,
                                                ),
                                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                                    0,
                                                    crate::codegen::shared::asm_operand::Width::B32,
                                                ),
                                            },
                                            &[],
                                        ));
                                    },
                                    32 => {
                                        // mov %eax, %eax (zero-extend to 64-bit)
                                        self.emit_insn(&Insn::raw_with_gas(
                                            &[0x89, 0xC0],
                                            "mov %eax, %eax",
                                        ));
                                    },
                                    _ => {},
                                }
                            }
                        }
                    }
                    // __attribute__((returns_nonnull)): 返回前检查 rax 非空.
                    // 若为 0 触发 UD2 (执行未定义指令) 立即 abort.
                    if self.current_returns_nonnull
                        && matches!(ret_ty, Type::Ptr(_) | Type::Func(_, _))
                    {
                        // test rax, rax; jnz .ok; ud2; .ok:
                        self.emit_insn(&Insn::test_rm_reg(
                            crate::codegen::shared::asm_operand::Width::B64,
                            0,
                            0,
                        )); // test rax, rax
                        let ok_label = self.fresh_label();
                        self.jmp_rel32(&ok_label, JumpKind::Jne);
                        self.emit_insn(&Insn::raw_with_gas(&[0x0f, 0x0b], "ud2")); // ud2 - undefined instruction trap
                        self.label(&ok_label);
                    }
                } else {
                    self.emit_insn(&Insn::raw_with_gas(&[0x31, 0xc0], "xor %eax, %eax"));
                }
                // return 语句: 在 epilogue 前先插入函数退出插桩
                if self.target.instrument_functions && !self.current_no_instrument {
                    self.emit_instrument_func_exit();
                }
                self.emit_func_epilogue();
            },
            Stmt::Expr { e, .. } => {
                if !self.gen_unknown_builtin_stmt(e) {
                    // 表达式语句的值没人要（P13 阶段1 的 Discarded 语境）：带上这个上下文
                    // 去求值，赋值臂末尾的宽度归约就能省掉。`gen_expr` 入口会把它消费掉，
                    // 所以只有本节点看得到，嵌套子表达式一律当「值要用」。
                    self.value_is_dead = true;
                    self.gen_expr(e, Self::EVAL_BASE);
                    self.value_is_dead = false;
                }
            },
            Stmt::If {
                cond, then_, else_, ..
            } => {
                // 常量条件优化: if (1) 只生成 then, if (0) 只生成 else
                // 这对 __compiletime_assert 等内核宏至关重要:
                // 条件为真时不应生成对 __compiletime_assert_N 的调用
                //
                // 注意: 不能简单跳过死分支, 因为其中可能包含 goto 目标标签.
                // 如果死分支含有标签, 必须正常生成该分支代码. 但死分支只允许
                // 经 goto 进入: 必须用一条 jmp 越过整段, 否则落空路径会顺进来
                // (实测 `if (0) { L: g = 1; }` 把 g 置 1; pr110176.c 因此死循环).
                let emit_dead_branch = |this: &mut Self, s: &Stmt| {
                    let skip = this.fresh_label();
                    this.jmp_rel32(&skip, JumpKind::Jmp);
                    this.gen_stmt(s);
                    this.label(&skip);
                };
                if let Ok(cv) = const_eval::eval(cond) {
                    let has_labels_in_then = Self::stmt_contains_label(then_);
                    let has_labels_in_else = else_
                        .as_ref()
                        .map_or(false, |s| Self::stmt_contains_label(s));
                    if cv != 0 {
                        // condition is true
                        self.gen_stmt(then_);
                        // else 分支里可能含被 then 分支 goto 跳转的标签（如 20030503-1.c:
                        // `if (1) goto foo; else { foo: ... }`），必须也发射 else 分支。
                        if has_labels_in_else {
                            if let Some(el) = else_ {
                                emit_dead_branch(self, el);
                            }
                        }
                    } else {
                        // condition is false: else 是活路径, then 是死分支
                        // (只作 goto 目标存在, 不得顶替 else)。
                        if has_labels_in_then {
                            if else_.is_some() {
                                // 死 then 后紧跟活 else: 落空路径跳到 else, 但经
                                // goto 进入死 then 后必须**越过 else** (jmp end),
                                // 否则会顺落进 else (pr17078-1.c: goto 进
                                // `if(0){L:i=0;}else i=1;` 曾顺落得 i=1 → abort)。
                                let to_else = self.fresh_label();
                                let end = self.fresh_label();
                                self.jmp_rel32(&to_else, JumpKind::Jmp);
                                self.gen_stmt(then_);
                                self.jmp_rel32(&end, JumpKind::Jmp);
                                self.label(&to_else);
                                if let Some(el) = else_ {
                                    self.gen_stmt(el);
                                }
                                self.label(&end);
                            } else {
                                // 无 else: 死分支只需被一条 jmp 越过, 顺落即跳过。
                                emit_dead_branch(self, then_);
                            }
                        } else if let Some(el) = else_ {
                            self.gen_stmt(el);
                        }
                    }
                } else {
                    let le = self.fresh_label();
                    let lend = self.fresh_label();
                    // 条件进分支语境：`&&`/`||`/`!`/`!= 0` 在 [gen_cond_jump_to] 里
                    // 按短路拆成直生 jcc，不再把每个比较材料化成 0/1 再 test。
                    self.gen_cond_jump_to(cond, &le, false);
                    self.gen_stmt(then_);
                    // Skip dead jmp when then-body ends with return
                    let then_ends_ret = Self::stmt_ends_with_return(then_);
                    // **不要**在这里追加「无 else 就不发这条 jmp」的 to-next 抑制：
                    // 实测 luaV_execute 里能删掉 135 条（静态 −675 字节），但 fib(32) 反而
                    // 慢 3.8%（同机交替 best-of-9：min 310.9→322.6ms）—— 那 5 字节×135 的
                    // 位置移动把热循环体推离了原有对齐，布局代价远大于省下一条恒定 Taken 跳。
                    // 数据与结论见 .trae/documents/x86_64-p13-hot-path-emission-optimization-plan.md §2。
                    if !then_ends_ret {
                        self.jmp_rel32(&lend, JumpKind::Jmp);
                    }
                    self.label(&le);
                    if let Some(el) = else_ {
                        self.gen_stmt(el);
                    }
                    self.label(&lend);
                }
            },
            Stmt::While { cond, body, .. } => {
                let ls = self.fresh_label();
                let le = self.fresh_label();
                let (sc, sb) = (self.loop_continue.clone(), self.loop_break.clone());
                self.loop_continue = Some(ls.clone());
                self.loop_break = Some(le.clone());
                self.break_targets.push((le.clone(), false));
                self.maybe_align_loop();
                self.label(&ls);
                self.gen_cond_jump_to(cond, &le, false);
                self.gen_stmt(body);
                self.jmp_rel32(&ls, JumpKind::Jmp);
                self.label(&le);
                self.loop_continue = sc;
                self.loop_break = sb;
                self.break_targets.pop();
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                if let Some(i) = init {
                    self.gen_stmt(i);
                }
                let ls = self.fresh_label();
                let le = self.fresh_label();
                let lst = self.fresh_label();
                let (sc, sb) = (self.loop_continue.clone(), self.loop_break.clone());
                self.loop_continue = Some(lst.clone());
                self.loop_break = Some(le.clone());
                self.break_targets.push((le.clone(), false));
                self.maybe_align_loop();
                self.label(&ls);
                if let Some(c) = cond {
                    self.gen_cond_jump_to(c, &le, false);
                }
                self.gen_stmt(body);
                self.label(&lst);
                if let Some(s) = step {
                    // for 的 step 也是表达式语句：值同样没人读。
                    self.value_is_dead = true;
                    self.gen_expr(s, Self::EVAL_BASE);
                    self.value_is_dead = false;
                }
                self.jmp_rel32(&ls, JumpKind::Jmp);
                self.label(&le);
                self.loop_continue = sc;
                self.loop_break = sb;
                self.break_targets.pop();
            },
            Stmt::Do { body, cond, .. } => {
                let ls = self.fresh_label();
                let lc = self.fresh_label();
                let le = self.fresh_label();
                let (sc, sb) = (self.loop_continue.clone(), self.loop_break.clone());
                // do { body; cond } while(cond); 里的 continue 语义上跳到
                // 条件表达式求值处, 而不是循环体开头. 若跳到 body 开头,
                // 循环体内 `continue; ++i;` 中的 ++i 会被跳过, 造成死循环.
                self.loop_continue = Some(lc.clone());
                self.loop_break = Some(le.clone());
                self.break_targets.push((le.clone(), false));
                self.maybe_align_loop();
                self.label(&ls);
                self.gen_stmt(body);
                self.label(&lc);
                self.gen_cond_jump_to(cond, &ls, true);
                self.label(&le);
                self.loop_continue = sc;
                self.loop_break = sb;
                self.break_targets.pop();
            },
            Stmt::Block { b, .. } => {
                let cleanup_base = self.begin_cleanup_scope();
                self.gen_block_items(&b.items);
                // P2-9: 块结束时逆序调用 cleanup 函数（与语句表达式共用同一份实现）
                self.end_cleanup_scope(cleanup_base);
                // 含 VLA 的块 fall-through 退出: 恢复动态栈基,
                // 丢弃本块及更深层已退出的动态分配 (见 emit_vla_restore)
                if Self::block_contains_vla(&b.items) {
                    self.emit_vla_restore();
                }
            },
            Stmt::Break { .. } => {
                // break 跳到最近封闭的 switch / 循环出口。switch 的
                // 出口标签 l_break 会统一弹出 switch 值 (add rsp, 8),
                // 因此这里只发跳转, 不重复弹栈。
                self.emit_vla_restore();
                if let Some((l, _)) = self.break_targets.last().cloned() {
                    self.jmp_rel32(&l, JumpKind::Jmp);
                }
            },
            Stmt::Continue { .. } => {
                // 回跳前恢复动态栈基: continue 丢弃本轮迭代内的 VLA
                self.emit_vla_restore();
                if let Some(l) = self.loop_continue.clone() {
                    self.jmp_rel32(&l, JumpKind::Jmp);
                }
            },
            Stmt::Goto { target, .. } => {
                // goto 可能跳出含 VLA 的块或回跳到声明前, 跳转前统一
                // 恢复动态栈基, 防止回跳循环累积泄漏 (栈耗尽段错误)
                self.emit_vla_restore();
                // Non-local goto: 嵌套函数 goto 到父函数中的标签.
                // 标签不在当前函数体内定义 → 需要恢复父帧指针.
                // r10 (静态链) 指向父帧, 将其加载到 r13 (帧指针).
                let is_nonlocal =
                    self.current_func_is_nested && !self.local_labels.contains(&target.name);
                if is_nonlocal {
                    // mov r13, r10  (恢复父帧指针)
                    self.emit_insn(&Insn::mov_rm_reg(
                        crate::codegen::shared::asm_operand::Width::B64,
                        13,
                        10,
                    ));
                }
                // non-local goto: label 定义在父函数里, 必须用父函数名作为命名空间,
                // 否则 c_label_to_asm 生成 `.Ll_<child>_<label>` 而父函数发射
                // `label:` 时生成 `.Ll_<parent>_<label>`, 两者不匹配 (E1102)。
                // 统一走 c_label_to_asm_resolve_nested: 对非嵌套/本函数内标签
                // 走原 c_label_to_asm, 对嵌套且非本函数的标签走父函数命名空间。
                let lbl = self.c_label_to_asm_resolve_nested(&target.name);
                self.jmp_rel32(&lbl, JumpKind::Jmp);
            },
            Stmt::Label { name, stmt, .. } => {
                let lbl = self.c_label_to_asm(&name.name);
                self.label(&lbl);
                self.gen_stmt(stmt);
            },
            // C++ 零成本异常: 受保护调用。LSDA call-site 区间就是被包装调用语句
            // 的地址范围 —— 表驱动展开以 PC 为键, 区间越接近「一条调用指令」,
            // 落地垫能做出的清理决定就越精确 (少算则泄漏, 多算则重复析构)。
            Stmt::Invoke {
                call,
                landing_pad,
                actions,
                ..
            } => {
                let start = self.pos();
                self.gen_stmt(call);
                let end = self.pos();
                self.eh_sites
                    .push(crate::codegen::x86_64::eh_tables::EhSite {
                        start,
                        end,
                        landing_pad: landing_pad
                            .as_ref()
                            .map(|pad| self.c_label_to_asm(&pad.name)),
                        actions: actions.clone(),
                    });
            },
            // C++ 零成本异常: 落地垫序言。unwinder 把 IP 直接交到标签处, 此时按
            // x86-64 C++ ABI: %rax = 异常对象指针, %rdx = type selector。
            // 这两个寄存器值只在本序列之后仍然有效, 因此贴完标签就立即溢出到槽位;
            // 剩下的清理 / 按 selector 分派 / `__cxa_begin_catch` 都是普通语句。
            Stmt::EhLandingPad {
                label,
                object,
                selector,
                ..
            } => {
                let lbl = self.c_label_to_asm(&label.name);
                self.label(&lbl);
                let slots = match (&**object, &**selector) {
                    (Expr::Ident { name: o, .. }, Expr::Ident { name: s, .. }) => {
                        Some((o.name.clone(), s.name.clone()))
                    },
                    _ => None,
                };
                let Some((o_name, s_name)) = slots else {
                    self.diagnostics.push(crate::diagnostics::Diagnostic {
                        sev: crate::diagnostics::Severity::Error,
                        code: "E1121",
                        msg: "exception landing pad slots must be named local variables"
                            .to_string(),
                        span: crate::diagnostics::Span::point("<eh>", 0, 0),
                        hint: Some("后端只能在落地垫第一条指令把 rax/rdx 存入具名栈槽".into()),
                        source_line: None,
                    });
                    return;
                };
                // rax = 0, rdx = 2; 两个槽都是指针/整数宽度 (8 字节)
                let obj_off = self.var_offset(&o_name);
                self.store_stack(0, obj_off);
                let sel_off = self.var_offset(&s_name);
                self.store_stack(2, sel_off);
            },
            Stmt::Switch {
                expr,
                cases,
                prelude,
                ..
            } => {
                // switch: 计算表达式, 比较链跳转到对应 case 体.
                // 布局: 所有比较集中在开头 (相等跳 l_case_i), 无匹配
                // 跳 default / break 出口; case 体按源码顺序物理排列,
                // fall-through (case 体末尾无 break) 自然落入下一 case 体.
                let l_end = self.fresh_label();
                let l_default = self.fresh_label();
                let l_break = self.fresh_label();
                let mut has_default = false;
                // break 目标栈: switch 压入 (l_break, false)。switch 值
                // 保存在帧内槽, break 跳转无需额外清理。
                self.break_targets.push((l_break.clone(), false));
                // 计算switch表达式到rax
                self.gen_expr(expr, Self::EVAL_BASE);
                let switch_is_int128 = matches!(expr.ty(), Type::Int128 | Type::UInt128);
                // switch 值保存到 r13 帧内槽而非 push 栈槽: case 体中
                // 可能含函数调用, push 跨越调用会使 rsp 错位 8, 违反
                // 16 字节栈对齐 ABI (pr44575: case 体中 call printf
                // 时 libc 内 movaps 对齐崩溃)。
                let sw_slot = self.alloc_sret_slot(16); // int128 需低/高 16 字节
                let sw_lo_slot = self.alloc_sret_slot(8); // 范围比较 low 暂存
                if switch_is_int128 {
                    // __int128: gen_expr 已正确加载 rax(low) + rdx(high)
                    self.store_stack(0, sw_slot); // low
                    self.store_stack(2, sw_slot + 8); // high
                } else {
                    self.store_stack(0, sw_slot); // 保存switch值
                }
                // 判断 switch 表达式是否有符号类型 (决定范围比较使用
                // 有符号 jl/jg 还是无符号 jb/ja 跳转指令).
                let switch_is_signed = match expr.ty() {
                    Type::Enum(_, _, Some(inner)) => Self::is_signed_int(&inner.base),
                    ref ty => Self::is_signed_int(ty),
                };
                let mut clause_bodies: Vec<(&crate::ast::Case, Option<String>)> = Vec::new();
                for case in cases {
                    if let Some(ref val) = case.val {
                        let l_case = self.fresh_label();
                        if let Some(ref hi) = case.range_end {
                            // GNU case low ... high: 范围匹配
                            // 保存 low 到临时栈槽, 防止 gen_expr(hi) 求值时
                            // 将 rdx 用作临时寄存器导致 low 值被覆盖.
                            let l_next = self.fresh_label();
                            self.gen_expr(val, 2); // rdx = low
                            self.store_stack(2, sw_lo_slot);
                            self.gen_expr(hi, 8); // r8 = high
                                                  // Bug fix: gen_expr 可能将 rcx 用作临时寄存器,
                                                  // 每次比较前从帧内槽重新加载 switch 值到 rcx.
                            self.load_stack(1, sw_slot); // rcx = switch 值
                            self.load_stack(2, sw_lo_slot); // rdx = low
                            self.emit_insn(&Insn::alu_rm_reg_reg(
                                crate::codegen::x86_64::opcode::AluOp::Cmp,
                                crate::codegen::shared::asm_operand::Width::B64,
                                1,
                                2,
                            )); // cmp rcx, rdx
                                // 有符号用 jl (skip if < low), 无符号用 jb
                            let jk_lo = if switch_is_signed { JumpKind::Jl } else { JumpKind::Jb };
                            self.jmp_rel32(&l_next, jk_lo);
                            // 重新加载 switch 值 (gen_expr(hi) 可能覆写 rcx)
                            self.load_stack(1, sw_slot); // rcx = switch 值
                            self.emit_insn(&Insn::alu_rm_reg_reg(
                                crate::codegen::x86_64::opcode::AluOp::Cmp,
                                crate::codegen::shared::asm_operand::Width::B64,
                                1,
                                8,
                            )); // cmp rcx, r8
                                // 有符号用 jg (skip if > high), 无符号用 ja
                            let jk_hi = if switch_is_signed { JumpKind::Jg } else { JumpKind::Ja };
                            self.jmp_rel32(&l_next, jk_hi);
                            self.jmp_rel32(&l_case, JumpKind::Jmp);
                            self.label(&l_next);
                        } else {
                            // case N: 比较 rcx == N, 相等跳 case 体
                            if switch_is_int128 {
                                // __int128 case 比较: 纯寄存器方式
                                // [sw_slot] = switch low, [sw_slot+8] = switch high
                                let case_low: i64 = if let Ok(cv) = const_eval::eval(val) {
                                    cv as i64
                                } else {
                                    0
                                };
                                let case_high: i64 = if let Ok(cv) = const_eval::eval(val) {
                                    (cv >> 64) as i64
                                } else {
                                    0
                                };
                                // 比较低 64 位: switch_low vs case_low
                                self.load_stack(0, sw_slot); // rax = switch low
                                if case_low == 0 {
                                    self.emit_insn(&Insn::test_rm_reg(
                                        crate::codegen::shared::asm_operand::Width::B64,
                                        0,
                                        0,
                                    )); // test rax, rax
                                } else {
                                    // mov rdx, case_low; cmp rax, rdx
                                    self.emit_insn(&Insn::movabs_reg_imm64(2, case_low as u64));
                                    self.emit_insn(&Insn::alu_rm_reg_reg(
                                        crate::codegen::x86_64::opcode::AluOp::Cmp,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                        0,
                                        2,
                                    )); // cmp rax, rdx
                                }
                                let l_nomatch = self.fresh_label();
                                self.jmp_rel32(&l_nomatch, JumpKind::Jne);
                                // 低 64 位匹配, 检查高 64 位
                                self.load_stack(9, sw_slot + 8); // r9 = switch high
                                if case_high == 0 {
                                    self.emit_insn(&Insn::test_rm_reg(
                                        crate::codegen::shared::asm_operand::Width::B64,
                                        9,
                                        9,
                                    )); // test r9, r9
                                } else {
                                    // mov r8, case_high; cmp r9, r8
                                    self.emit_insn(&Insn::new(
                                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                            rip_relative: false,
                                            width: crate::codegen::shared::asm_operand::Width::B64,
                                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                                8,
                                                crate::codegen::shared::asm_operand::Width::B64,
                                            ),
                                            src: crate::codegen::x86_64::opcode::Operand::Imm(
                                                case_high as i64,
                                            ),
                                        },
                                        &[],
                                    ));
                                    self.emit_insn(&Insn::alu_rm_reg_reg(
                                        crate::codegen::x86_64::opcode::AluOp::Cmp,
                                        crate::codegen::shared::asm_operand::Width::B64,
                                        9,
                                        8,
                                    )); // cmp r9, r8
                                }
                                self.jmp_rel32(&l_case, JumpKind::Je);
                                self.label(&l_nomatch);
                            } else {
                                self.gen_expr(val, 2);
                                // gen_expr 可能覆写 rcx, 比较前重新加载
                                self.load_stack(1, sw_slot); // rcx = switch 值
                                self.emit_insn(&Insn::alu_rm_reg_reg(
                                    crate::codegen::x86_64::opcode::AluOp::Cmp,
                                    crate::codegen::shared::asm_operand::Width::B64,
                                    1,
                                    2,
                                ));
                                self.jmp_rel32(&l_case, JumpKind::Je);
                            }
                        }
                        clause_bodies.push((case, Some(l_case)));
                    } else {
                        // default: 记录标签, 体留在源码位置
                        has_default = true;
                        clause_bodies.push((case, None));
                    }
                }
                // 没有 case 匹配: 跳 default 或 break 出口 (出口统一弹栈)
                if has_default {
                    self.jmp_rel32(&l_default, JumpKind::Jmp);
                } else {
                    self.jmp_rel32(&l_break, JumpKind::Jmp);
                }
                // 滴定砝码一：全体子句体位移 n 字节。位置在分派链末尾那条**无条件 jmp**
                // 之后 ⇒ 这段 nop 只能被跳过、不会被落入（唯一例外是有 prelude 的
                // switch，见 `fn_shift` 头注释，测量脚本会把那种 TU 排除）。
                for _ in 0..fn_shift() {
                    self.nop();
                }
                // 发射前驱段: `switch(x){ foo: case 1: ... }` 中 foo: 等出现在
                // 第一个 case 之前的普通语句/标签。按 C 语义它们物理排在第一个
                // case 体之前 (fall-through 到第一个 case), 同时也能被 goto 直接命中。
                // 发射点在分派链之后、第一个 l_case 标签之前: 分派链永远不会落到
                // 这里, 只有 goto 或 (理论上) 落入 switch 的执行才会走到。
                if !prelude.is_empty() {
                    self.gen_block_items(prelude);
                }
                // 子句体按源码顺序物理排列, default 也留在它的源码位置上:
                // 物理相邻 == C 的 fall-through 语义。把 default 排到全部
                // case 之后会让末尾 case 误落入 default 体, 同时丢掉
                // default 自身向下落入后继子句的边 (sqlite OP_MakeRecord
                // 的 default-first switch 即因此只写入 8 字节中的 2 字节)。
                for (case, l_case) in &clause_bodies {
                    // 滴定砝码二：每个子句标签前插 k 字节。上一个子句以无条件跳转或
                    // return 收尾时这段 nop 不执行；**fall-through 进来的子句会真的执行
                    // 它们**，所以这是「footprint + 少量 nop」的混合扰动，斜率口径以
                    // `DCC_FN_SHIFT` 为准，本旋钮用来区分「对齐」与「体量」两种效应。
                    for _ in 0..handler_pad() {
                        self.nop();
                    }
                    match l_case {
                        Some(l) => self.label(l),
                        None => self.label(&l_default),
                    }
                    // 子句体完整生成 (含局部变量声明), 与普通块一致
                    self.gen_block_items(&case.body.items);
                }
                // break 出口: switch 值保存在帧内槽, 无需弹栈
                // `l_end` 紧贴 `l_break`，这条 jmp 看起来是 to-next 冗余，但和上面 If 那一支
                // 同属已实测**删了会变慢**的一类（布局/对齐代价），保留原形状。
                self.label(&l_break);
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                self.label(&l_end);
                self.break_targets.pop();
            },
            Stmt::GotoIndirect { expr, .. } => {
                // 批 4：表基址常驻 `%rbp` 时，整条分派塌成一条 `jmp *(%rbp,%rcx,8)`。
                // 不合形状则原样往下走（原路自己也会因为基址已常驻而少一条 lea）。
                if self.emit_hoisted_jumptable_jump(expr) {
                    return;
                }
                // goto *expr: 计算目标地址并跳转
                self.gen_expr(expr, Self::EVAL_BASE); // rax = 目标地址
                                                      // 步骤 3: 走类型化 Jmp{target: Reg(0=rax), pending_rel: false} → FF /4 rax.
                                                      // 原手写字节: 0xFF, 0xE0 (= FF /4 r/m=rax).
                let insn = Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::Jmp {
                        target: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        pending_rel: false,
                    },
                    &[],
                );
                if !self.emit_insn(&insn) {
                    self.emit_insn(&Insn::jmp_rm_reg(0)); // jmp rax
                }
            },
            Stmt::Fallthrough { .. } => {},
            Stmt::AsmGoto {
                asm_str,
                outputs,
                output_names,
                output_constraints,
                inputs,
                input_names,
                input_constraints,
                clobbers,
                goto_labels,
                volatile: _,
                span,
            } => {
                // 记录 asm 语句的源行号, 使 mini-asm 警告能附带源位置
                self.current_asm_src_line = span.line;
                // 将 GCC 的 %lN / %l[name] / %c[label] 占位符解析为 C 标签名。
                // 使用数字边界检查防止 %l1 错误匹配 %l10 等多位索引。
                // 标签名前缀 .Ll_ 以避免与函数符号名冲突。
                let mut resolved_asm = asm_str.clone();
                let label_base = outputs.len() + inputs.len();
                for (i, label) in goto_labels.iter().enumerate().rev() {
                    let prefixed = self.c_label_to_asm_resolve_nested(&label.name);
                    resolved_asm = Self::asm_replace_label_idx(&resolved_asm, i, &prefixed);
                    resolved_asm =
                        Self::asm_replace_label_idx(&resolved_asm, label_base + i, &prefixed);
                    resolved_asm = resolved_asm.replace(&format!("%l[{}]", label.name), &prefixed);
                    // %c[label]: strip immediate prefix, resolve to label
                    resolved_asm = resolved_asm.replace(&format!("%c[{}]", label.name), &prefixed);
                    // Also replace literal label definitions in asm text:
                    // "labelname:" -> ".Ll_labelname:" so they match the
                    // prefixed label names used by the codegen.
                    resolved_asm = resolved_asm
                        .replace(&format!("{}:", label.name), &format!("{}:", prefixed));
                }
                // 跨函数标签地址 + asm goto 混用警告:
                // &&label 产生的地址仅在当前函数内有效; 若作为输入操作数
                // 传给 asm goto, 跨函数边界使用会导致未定义行为。
                for inp in inputs.iter() {
                    if matches!(inp, Expr::LabelAddr { .. }) {
                        self.asm_reject(
                            "label address used in asm goto may be invalid \
                             if the label is in a different function",
                        );
                        break;
                    }
                }
                // GCC 语义: asm goto 输出约束写回仅 fall-through 路径生效;
                // 跳转分支保留原值。
                self.emit_inline_asm_goto(
                    &resolved_asm,
                    outputs,
                    output_names,
                    output_constraints,
                    inputs,
                    input_names,
                    input_constraints,
                    clobbers,
                );
            },
        }
    }

    pub fn gen_block_items(&mut self, items: &[BlockItem]) {
        for it in items {
            // 每个 block-item 从当前水位开始, 结束时回收其新增槽位
            // (与 max_sret_bytes 静态扫描的 item 边界一一对应)。
            let mark = self.save_sret_watermark();
            self.gen_block_item(it);
            self.restore_sret_watermark(mark);
        }
    }

    /// 块作用域的 cleanup 登记边界：进来时记水位。
    ///
    /// 为什么要成对地抽出来：`__attribute__((cleanup))` 的**登记**在 `gen_block_item` 里，
    /// 而**发出**在 `Stmt::Block` 臂里；语句表达式 `({...})` 走的是 `gen_block_item` 的一份
    /// 副本（`emit/expr/dispatch.rs`），它既有自己的登记点又没有这个收尾点 —— 于是写在
    /// 语句表达式里的 cleanup 从来不会执行（实测 `hits=0`，gcc 是 4）。把两端收成一个
    /// 成对入口，副本忘了哪一端都会当场可见。
    pub fn begin_cleanup_scope(&mut self) -> usize {
        self.cleanup_vars.len()
    }

    /// 出块：按 LIFO 发出本作用域内登记的 cleanup 调用。
    pub fn end_cleanup_scope(&mut self, base: usize) {
        while self.cleanup_vars.len() > base {
            if let Some((var_name, fn_name)) = self.cleanup_vars.pop() {
                // 发射: lea rdi, [r13 + offset]; call fn_name
                let off = self.var_offset(&var_name);
                self.emit_lea_r13(7, off); // rdi = &var
                self.call_label(&fn_name);
            }
        }
    }

    fn gen_block_item(&mut self, it: &BlockItem) {
        if let BlockItem::Decl(Decl::Var(v)) = it {
            self.note_src_line(v.span.line);
        }
        match it {
            BlockItem::Stmt(s) => self.gen_stmt(s),
            BlockItem::Decl(Decl::Var(v)) => {
                // extern 声明不分配栈槽, 仅作为全局符号引用标记
                if v.is_extern && v.init.is_none() {
                    return;
                }
                // static 局部变量已在全局数据段分配,
                // 跳过栈分配 (与函数顶层遍历 L6970 一致)
                if v.is_static
                    && self
                        .static_local_map
                        .is_static_local(&self.current_func_name, &v.name.name)
                    && !self
                        .static_local_map
                        .needs_runtime_init(&self.current_func_name, &v.name.name)
                {
                    return;
                }
                // 声明处的绑定走唯一入口（与 `compile_func` 那侧同一份判据，
                // 以前是两个拷贝，改一处漏一处）。
                self.bind_decl_home(v);
                // P2-9: cleanup 属性变量注册
                if let Some(ref fn_name) = v.cleanup_fn {
                    self.cleanup_vars
                        .push((v.name.name.clone(), fn_name.clone()));
                }
                // 定义+声明一体的 struct/union 类型: 变量声明点即类型
                // 声明点, 触发 VM 槽绑定/静态清除 (parser 不产生独立
                // StructDef BlockItem)
                if matches!(v.ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)) {
                    self.gen_vm_type_bind(&v.ty);
                }
                // static local 需要运行时初始化: 发射初始化代码到全局地址
                if v.is_static
                    && self
                        .static_local_map
                        .needs_runtime_init(&self.current_func_name, &v.name.name)
                {
                    if let Some(init) = &v.init {
                        // 数组型 static local (如 `static const void *l[] = {&&a, &&b};`):
                        // InitList 中的每个元素都要逐元素写入对应偏移,
                        // 不能用 gen_expr(EVAL_BASE) + 单次 store (只写第一个元素).
                        if let Expr::InitList { inits, .. } = init {
                            if let Type::Array(elem_ty, _) = &v.ty {
                                let elem_size = elem_ty.size();
                                let mut off: i32 = 0;
                                for item in inits.iter() {
                                    if let InitItem::Expr(e) = item {
                                        self.gen_expr(e, Self::EVAL_BASE);
                                        self.emit_global_store_offset(0, &v.name.name, off);
                                        off += elem_size as i32;
                                    }
                                }
                            } else {
                                self.gen_expr(init, Self::EVAL_BASE);
                                self.emit_global_store(0, &v.name.name);
                            }
                        } else {
                            self.gen_expr(init, Self::EVAL_BASE);
                            self.emit_global_store(0, &v.name.name);
                        }
                    }
                    return;
                }
                self.register_var(&v.name.name, &v.ty);
                if Self::type_has_vla(&v.ty) {
                    self.gen_vla_alloc(v);
                } else if let Some(init) = &v.init {
                    if let Expr::InitList { inits, .. } = init {
                        self.gen_init_list(&v.name.name, &v.ty, inits);
                    } else {
                        let is_agg = matches!(
                            &v.ty,
                            Type::Struct(_, _, _, _)
                                | Type::Union(_, _, _, _)
                                | Type::Array(_, ArraySize::Const(_))
                                | Type::Atomic(_)
                        ) && v.ty.size() > 8;
                        if matches!(&v.ty, Type::Vector(_, _)) || is_agg {
                            let dst_ty = v.ty.clone();
                            let dst_name = v.name.name.clone();
                            self.gen_aggregate_assign(&dst_name, &dst_ty, init);
                        } else if let Type::Complex(inner) = &v.ty {
                            self.gen_complex_init(&v.name.name, &inner.base, init);
                        } else if let Type::Imaginary(ref inner) = &v.ty {
                            self.gen_imaginary_init(&v.name.name, &inner.base, init);
                        } else if matches!(
                            &v.ty,
                            Type::Float | Type::Double | Type::LongDouble | Type::Float128
                        ) {
                            // 浮点: xmm0 结果必须用 movsd/movss 存回栈, 不能用整数 mov rax
                            self.gen_expr(init, Self::EVAL_BASE);
                            if matches!(&v.ty, Type::Float128) {
                                // binary128: 值通道是 xmm0 的 **16 字节**。
                                // 整数 RHS 要先归一到 binary128 (libgcc TFmode 例程),
                                // 浮点 RHS 则由 emit_to_float128 拓宽; 归一后必须
                                // 整块 16 字节存回槽 (gen_float_store 走 movups)。
                                // 走下面的整数分支会退化成 `movq %xmm0` 只存低 8 字节
                                // ⇒ 高半是栈上垃圾, 值直接损坏。
                                self.emit_to_float128(&init.ty());
                            } else {
                                // int 常量初始化浮点变量: 先转换到 xmm0 (double x = 1;)
                                self.emit_scalar_to_xmm0_if_needed(init, &v.ty);
                            }
                            self.gen_float_store(&v.name.name, &v.ty);
                        } else {
                            self.gen_expr(init, Self::EVAL_BASE);
                            // Implicit float→int conversion
                            let init_ty = init.ty();
                            if matches!(
                                &init_ty,
                                Type::Float | Type::Double | Type::LongDouble | Type::Float128
                            ) && Self::is_integer_type(&v.ty)
                            {
                                self.emit_implicit_float_to_int(0, &init_ty, &v.ty);
                            }
                            // D7: `__int128` 是 16 字节对象，且 dcc 的值模型把它
                            // 按 rdx:rax 双寄存器交付。`mov_to_stack` 对任何
                            // sz >= 8 都只写一个 8 字节 store，于是高半从未落槽：
                            // 循环体内 `__int128 v = va_arg(...)` 后的 `v >> 64`
                            // 读到的是栈残值（实测三次迭代全部返回同一个个值）。
                            // 只在“目标与源都是 128 位整型值”时补高半 store，
                            // 避开 float→__int128 转换只备 rax 的情形。
                            let store_hi = matches!(&v.ty, Type::Int128 | Type::UInt128)
                                && matches!(&init_ty, Type::Int128 | Type::UInt128)
                                && !self.homes.contains_key(&v.name.name);
                            self.mov_to_stack(0, &v.name.name);
                            if store_hi {
                                let off = self.var_offset(&v.name.name);
                                self.store_stack(2, off + 8); // rdx = 高 8 字节
                            }
                        }
                    }
                }
            },
            // 局部 struct/union 定义声明: 含 VLA 字段时在声明点
            // 求值总大小存入绑定槽 (C11 6.2.4p7), 供 sizeof(类型)
            // 读槽 (20041218-2: 声明后 n++ 不影响 sizeof)。
            // 静态类型 (重)声明则清除残留绑定。
            BlockItem::Decl(Decl::StructDef(sd)) => {
                self.gen_vm_struct_bind(sd, false);
            },
            BlockItem::Decl(Decl::UnionDef(sd)) => {
                self.gen_vm_struct_bind(sd, true);
            },
            _ => {},
        }
    }
}

/// `Stmt` → 源码行（1-based，0 表示无位置信息）。
///
/// 为什么本模块自带一份：`src/opt.rs` 的 `StmtSpan`、`src/loop_opt.rs` 与
/// `src/profile/gcov.rs` 的 `stmt_span` 都是各自模块的 private 辅助，跨不过；
/// 把它们合并到 `src/ast.rs` 的 `impl Stmt { pub fn span(&self) -> &Span }` 是
/// 下一步该做的事（那会改十几个调用点），不在本次行表接线范围内。这里先保证
/// 行号只有一个入口：上面的 `note_stmt_line` 是本文件唯一的调用者。
fn stmt_src_line(s: &Stmt) -> u32 {
    let span = match s {
        Stmt::If { span, .. }
        | Stmt::While { span, .. }
        | Stmt::Do { span, .. }
        | Stmt::For { span, .. }
        | Stmt::Return { span, .. }
        | Stmt::Expr { span, .. }
        | Stmt::Block { span, .. }
        | Stmt::Break { span, .. }
        | Stmt::Continue { span, .. }
        | Stmt::Goto { span, .. }
        | Stmt::GotoIndirect { span, .. }
        | Stmt::Label { span, .. }
        | Stmt::Switch { span, .. }
        | Stmt::AsmGoto { span, .. }
        | Stmt::Invoke { span, .. }
        | Stmt::EhLandingPad { span, .. }
        | Stmt::Fallthrough { span } => span,
    };
    span.line
}
