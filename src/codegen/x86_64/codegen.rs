// src/codegen/x86_64/codegen.rs - x86_64 Codegen 结构体定义
// Phase 1b Migration: Extract struct from monolithic mod.rs

use crate::ast::{AddrSpace, Decl, Designator, Expr, Field, Program, Type};
use crate::codegen::shared::elf;
use crate::codegen::shared::elf::{ComdatGroup, ElfRela, ElfSection, ElfSym, ExtraRelaSection};
use crate::codegen::x86_64::elf::attr_diag::AttrDiagFn;
use crate::codegen::x86_64::ifunc;
use crate::codegen::x86_64::mem::global;
use crate::codegen::x86_64::FuncDecl;
use crate::const_eval;
use crate::diagnostics::Diagnostic;
use crate::preproc::LineSpan;
use crate::static_locals::StaticLocalMap;
use crate::target::Target;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// x86_64 代码生成器主结构体
///
/// 包含代码生成过程中的所有状态信息，包括：
/// - 生成的机器码 (code) 和数据段 (data_section, rodata)
/// - ELF 符号表和重定位信息
/// - 当前函数上下文和局部变量映射
/// - TLS、嵌套函数、优化等特殊功能支持
pub struct Codegen {
    pub code: Vec<u8>,
    pub entry: u64,
    pub target: Target,
    pub labels: HashMap<String, u64>,
    pub current_asm_line: usize,
    pub current_asm_src_line: u32,
    pub diagnostics: Vec<Diagnostic>,
    /// 预处理输出行号 → 源文件位置的映射 (诊断定位用)
    pub line_map: Vec<LineSpan>,
    pub pending: Vec<PendingJump>,
    pub next_label: u32,
    pub var_map: HashMap<String, (i64, Type)>,
    pub loop_continue: Option<String>,
    pub loop_break: Option<String>,
    /// break 跳转目标栈：(标签，是否在 switch 内需先弹出 switch 值).
    /// switch 压入 (l_break, true), 循环压入 (le, false); break 语句
    /// 总是跳到栈顶目标 (最近封闭的 switch / 循环).
    pub break_targets: Vec<(String, bool)>,
    // .rodata: 字符串池 + 待修补引用
    pub rodata: Vec<u8>,
    pub string_pool: HashMap<Vec<u8>, u64>, // data -> offset in rodata
    pub string_refs: Vec<(u64, Vec<u8>)>,   // (code_pos, data)
    pub float_refs: Vec<(u64, u64)>,        // (code_pos, rodata_offset) - SSE 浮点字面量引用
    pub x87_float_refs: Vec<(u64, u64)>,    // (code_pos, rodata_offset) - x87 fld RIP-relative 引用
    /// -fbounds-check ubsan 诊断数据：LEA rdi, [rip+disp32] 的 code_pos 与
    /// OutOfBoundsData 偏移。ET_EXEC: 偏移在 rodata, resolve() 修补 disp32;
    /// ET_REL: 偏移在 .data, compile_rel 生成 PC32 重定位 (.data 节符号)。
    pub ubsan_lea_refs: Vec<(u64, u64)>,
    /// -fbounds-check ubsan 诊断数据：OutofBoundsData 内绝对指针槽
    /// (data_off, field_off, target_rodata_off), 目标均为 rodata 内
    /// TypeDescriptor / 文件名字符串。ET_EXEC: resolve() 补绝对地址;
    /// ET_REL: 生成 R_X86_64_64 重定位 (.rodata 节符号)。
    pub ubsan_ptr_refs: Vec<(u64, u64, u64)>,
    // 全局变量 (通过 lookup_global_info / lookup_global_by_key 受控访问)
    pub globals_raw: HashMap<String, GlobalVar>,
    /// 符号 → 段地址空间（`__seg_fs` / `__seg_gs` 加在**对象本体**上）。
    ///
    /// 存在理由：对象本体的段不属于任何一个表达式的类型（它是**符号**的属性），
    /// 而访存发射点是 `emit_global_addr/load/store` 这一族中央入口（66 个调用点）。
    /// 把「哪个符号属于哪个段」记成一张表，前缀就只需要在那三个入口贴一次 ——
    /// 若在调用点各贴各的，就是项目规范禁止的分阶段缺口。
    ///
    /// 与 pointee 一侧的段限定（`int __seg_gs *p`）是两条独立的轴：那条走类型系统
    /// `QualType::addr_space`（见 `abi::tyclass::ptr_seg_prefix`），这张表只管符号本体。
    pub global_seg: HashMap<String, AddrSpace>,
    /// 局部**槽** → 该槽里的指针值所指向对象所在的段（`%gs`=0x65 / `%fs`=0x64）。
    ///
    /// 为什么必须有它（`-O2` 才出现的形，判据
    /// `seg_qualifier_on_object_body_emits_segment_prefixes` 两档都跑）：`-O2` 的 SSA
    /// 降把「对象本体带段限定的全局」的访问拆成两条语句 `t = &gv; *t = v;`（真源见
    /// `src/ssa/build.rs` 的 `named_addr_operand`），而 `AddrOf` 造出的指针类型是就地新建的
    /// `Ptr(QualType::new(obj_ty))`，**不带地址空间** —— `ptr_seg_prefix` 那条轴到这里
    /// 必然为空。段信息只能在 codegen 侧按赋值流补回来：登记只发生在
    /// 「左值是单个标识符」的赋值点（一处），查询只发生在
    /// `abi::tyclass::seg_prefix_of_base`（一处）。
    ///
    /// 保守方向：任何其它赋值都会清除该槽的登记（宁可漏发也不误发）。
    ///
    /// **不进并行快照**：它是「一个函数体内推导出来的」逐函数状态，在
    /// `compile_func` 入口清空、只由本函数体的赋值语句重新登记；并行 worker 里
    /// 新造的 `Codegen` 从空表开始，与串行路径得到同一结果（快照反而会引入
    /// 「跳函数残留」这条不一致）。
    pub local_addr_seg: HashMap<String, u8>,
    // CompoundLit 去重缓存：同类型同内容的 CompoundLit 只物化一次
    // key = format!("{:?}", Expr) (类型 +inits 序列化),
    // value = (anon_name, rodata_offset)
    pub compound_lit_cache: HashMap<String, (String, u64)>,
    pub frame_size: i64,
    pub expr_depth: u32,     // 当前表达式嵌套深度 (用于帧临时槽分配)
    pub max_expr_depth: u32, // 最大表达式嵌套深度
    pub temp_slots: i32,     // 帧临时槽数量 (用于变量偏移计算)
    /// 已注册固定栈对象占用的 8 字节槽数。
    /// 只记录真实布局用量，不再为大数组创建 __pad_ 占位项。
    pub var_map_total_slots: i64,
    pub vec_temp_off: i64, // 向量临时槽区基址 (r13 相对)
    pub vla_offsets: HashMap<String, i64>,
    pub vla_total_size: i64,
    /// 局部 variable-sized struct (含 VLA 字段) 的声明时绑定总大小槽:
    /// 类型名 → 槽偏移。类型声明语句执行时求值一次 (C11 6.2.4p7:
    /// VM 类型在声明时绑定维度，后续 sizeof 读槽不重算，20041218-2:
    /// 声明后 n++ 不影响 sizeof(struct s))。循环内重复声明同名类型
    /// 每次迭代重写槽 (重新绑定)。
    pub vm_struct_size_slots: HashMap<String, i64>,
    // 当前函数体含 VLA 声明：goto/break/continue 跳转前需恢复动态栈基，
    // 否则循环内重复进入 VLA 作用域会累积泄漏直至栈耗尽 (vla-dealloc-1)
    pub cur_func_has_vla: bool,
    pub bf_rmw_depth: u32, // 位域 read-modify-write 嵌套深度 (动态帧槽分配)
    /// 向量 binop 操作数物化槽号。嵌套向量 binop 时每层结果槽 R 的
    /// 操作数物化到 R+1 (XMM 快速路径 R+1/R+2), 避免内层子表达式
    /// 结果覆盖外层已写入的结果槽 (pr23135: 链式 k0+k1+k3+...
    /// 低层元素被内层重物化覆盖)。非 binop 上下文默认槽 1。
    pub vector_op_slot: usize,
    // 可重定位模式 (.o)
    pub relocatable: bool,
    pub function_sections: bool, // -ffunction-sections：每函数独立 .text.<name> 节
    pub patchable_function_entry: Option<(usize, usize)>,
    pub symbols: Vec<ElfSym>,
    pub sym_index: HashMap<String, u32>,
    pub relocations: Vec<ElfRela>,
    pub pending_func_rels: Vec<(u64, String)>, // (code_offset, func_name) - 延迟到符号表构建后解析
    pub pending_code_rels: Vec<PendingCodeRel>, // asm 数据伪指令/外部跳转：通用 code relocation
    pub pending_func_addr_rels: Vec<(u64, String, bool)>, // (code_offset, func_name, is_lea: true=PC32, false=GOTPCREL)
    pub defined_funcs: HashSet<String>,                   // 本 TU 中有函数体的函数名
    pub error_attr_fns: HashSet<String>, // __attribute__((error)) 函数名 (调用时发射 ud2)
    pub pending_data_rels: Vec<(u64, String)>, // (disp32_pos, var_name) - 全局变量重定位
    pub pending_sib_data_rels: Vec<(u64, String)>, // SIB base=101 (disp32 绝对寻址) - 需要 R_X86_64_32S
    pub data_section: Vec<u8>,                     // .data 节 (已初始化全局变量)
    pub bss_size: u64,                             // .bss 大小
    pub tdata_section: Vec<u8>,                    // .tdata (TLS 初始化)
    pub tbss_size: u64,                            // .tbss (TLS 未初始化)
    pub tdata_align: u64,
    pub tbss_align: u64,
    pub exec_text_size: u64,                  // ET_EXEC 模式下 .text 大小
    pub pending_tls_rels: Vec<(u64, String)>, // TLS fs 相对引用
    pub text_align: u64,
    pub data_align: u64,
    pub bss_align: u64,
    pub extra_sections: Vec<ElfSection>, // 自定义节 (section 属性)
    pub custom_sec_indices: HashMap<String, usize>, // section name → index in extra_sections
    pub func_sizes: HashMap<String, u64>, // 函数名 → 代码大小 (用于符号 size)
    /// C++ 零成本异常: 当前函数已生成的**受保护调用点** (`Stmt::Invoke` 时追加)。
    /// 函数结束时整体搬进 `eh_funcs`。
    pub eh_sites: Vec<crate::codegen::x86_64::eh_tables::EhSite>,
    /// C++ 零成本异常: 已完成函数的 EH 归档, 由 ELF 组装阶段用来发射
    /// `.gcc_except_table` 与 FDE 的 LSDA 字段。Vec 而非 HashMap: 发射序必须
    /// 稳定可复现。
    pub eh_funcs: Vec<crate::codegen::x86_64::eh_tables::EhFuncInfo>,
    /// C++ 零成本异常: 每个函数的 **CFI 事实** (序言长度 + 实际使用的
    /// callee-saved 掩码)。表驱动展开完全依赖 FDE 的 CFA 规则, 猜值会让
    /// unwinder 走不出本帧 (旧实现对所有函数发同一段常量 ops)。
    pub func_cfi: HashMap<String, crate::codegen::x86_64::eh_tables::FuncCfi>,
    /// 序言结束位置 (自函数起点计): `DW_CFA_advance_loc` 的目标
    pub cur_prologue_end: u64,
    /// C++ 零成本异常: 函数名 → 其 LSDA 在本模块 `.gcc_except_table` 里的偏移。
    /// 由 `assemble_eh_lsda` 在**符号表定稿之前**填好 (FDE 的 LSDA 字段需要
    /// `.gcc_except_table` 节符号, 而局部符号必须在全局符号之前入表)。
    pub eh_lsda_offsets: HashMap<String, u64>,
    /// 类型表待解析的重定位: `(.gcc_except_table 内绝对偏移, 符号名)`。
    pub eh_lsda_relocs: Vec<(u64, String)>,
    /// `.gcc_except_table` 在 `extra_sections` 里的下标 (None = 本模块无 EH)。
    pub eh_except_sec_idx: Option<usize>,
    pub variadic_funcs: HashMap<String, bool>, // 函数名 → 是否可变参数
    pub old_style_funcs: std::collections::HashSet<String>, // K&R 风格定义函数名
    pub variadic_named_params: i32,            // 当前可变参数函数的命名参数数量
    pub variadic_named_gp: i32,
    pub variadic_named_fp: i32,
    pub variadic_overflow_offset: i64,
    pub variadic_reg_save_offset: i64,
    pub current_sret_offset: Option<i64>,
    /// 当前 sret/物化槽区已用字节水位 (自 `sret_region_base` 向下累计,
    /// 16 字节对齐)。替换旧的"按调用次数计数、步长取当前尺寸"的 bump:
    /// 旧实现混合 8/16/32B 分配时槽位互相重叠, 且随调用次数单调增长
    /// 越过 frame_size 覆盖动态 call outgoing 区。
    pub sret_watermark: i64,
    /// sret/物化槽区顶界 (r13 相对偏移, 紧邻变量区底)。槽自该地址
    /// 向下分配, 帧规划保证下界不越过 vec/bf/帧底固定区顶界。
    pub sret_region_base: i64,
    // 嵌套函数静态链
    pub nested_funcs: HashMap<String, Vec<(String, i64)>>, // 嵌套函数名 → upvars (name, parent_offset)
    // 嵌套函数重命名：(父函数名，原名) → 唯一名 (避免同名嵌套函数符号冲突)
    pub nested_func_renames: HashMap<String, HashMap<String, String>>,
    // 唯一名 → 定义父函数名 (静态链上溯解析兄弟/祖先嵌套函数引用)
    pub nested_func_parent: HashMap<String, String>,
    // 唯一名 → 该帧静态链槽偏移 (多层静态链上溯需要逐帧偏移)
    pub nested_tramp_offsets: HashMap<String, i64>,
    // 唯一名 → 定义父帧中 trampoline 槽偏移 (后代引用经静态链定位)
    pub nested_chain_offsets: HashMap<String, i64>,
    pub nested_func_rename_counter: u32,
    pub current_func_name: String,   // 当前正在编译的函数名
    pub current_func_ret_ty: Type,   // 当前函数的返回类型
    pub current_no_instrument: bool, // 当前函数标了 __attribute__((no_instrument_function))
    /// 静态局部变量分析 pass 的结果 (由 compile() 预计算，后端消费)
    pub static_local_map: StaticLocalMap,
    pub real_global_names: std::collections::HashSet<String>, // 真实全局变量名 (不含 static local), 用于避免 static local 覆盖真实全局
    /// 函数形参类型表（SysV 实参按形参类型分类）
    pub func_param_types: HashMap<String, Vec<Type>>,
    pub current_func_is_nested: bool, // 当前编译的函数是否是嵌套函数
    pub local_labels: std::collections::HashSet<String>, // 当前函数体内定义的标签名集合
    /// 当前函数标了 __attribute__((returns_nonnull)): 返回时需检查 rax 非空
    pub current_returns_nonnull: bool,
    /// 当前函数标了 __attribute__((noreturn)) / _Noreturn:
    /// 函数末尾插入 trap 指令 (ud2), 防止执行流落入后续代码
    pub current_is_noreturn: bool,
    pub static_chain_offset: i64,            // 静态链在当前帧中的偏移
    pub upvar_offsets: HashMap<String, i64>, // upvar 名称 → 在起源帧中的偏移
    // upvar 名称 → 需要走多少层静态链到达起源帧 (1=父，2=祖父，...)
    pub upvar_chain_depth: HashMap<String, u32>,
    // upvar 名称 → 中间嵌套帧的 static_chain_offset 列表
    // 对深度 1 为空; 对深度 2 包含 [父帧的 static_chain_offset]
    pub upvar_intermediate_offsets: HashMap<String, Vec<i64>>,
    // &&label 地址引用：(disp32_pos, label_name)
    pub label_addr_refs: Vec<(u64, String)>,
    // register asm 变量绑定：变量名 → (寄存器编号, 声明类型)。类型与寄存器必须同表携带：
    // 绑定路径的读写要按声明宽度截断/扩展，与栈槽路径同一口径，否则同一个变量在两条
    // 通路下会给出不同的值。
    pub reg_bindings: HashMap<String, (u8, Type)>,
    // file-scope register asm 变量绑定：每个函数入口恢复
    pub global_reg_bindings: HashMap<String, (u8, Type)>,
    /// 全程序里「地址被取走」的对象根名；只有 `addr_escaped_done` 为真时才参与判定
    /// （区分「扫过且为空」与「还没扫」）。判据本身在 `ast_gvn::addr_escaped_names`。
    pub addr_escaped: std::collections::HashSet<String>,
    pub addr_escaped_done: bool,
    // data 节 64 位符号引用重定位：(data_section_offset, symbol_name)
    /// (data_off, symbol, custom_sec, rel_type, addend) — addend 用于 &arr[i] 等
    pub pending_data64_rels: Vec<(u64, String, Option<usize>, u32, i64)>,
    // rodata 节内部重定位：rodata 里的 CompoundLit 结构体指针字段
    // 引用另一个 rodata 里的匿名 CompoundLit。(offset_in_rodata, symbol_name, rel_type, addend)
    pub pending_rodata_rels: Vec<(u64, String, u32, i64)>,
    // data 节内 label 地址差值：表达式 `&&A - &&B` 在静态初始化式中
    // 出现时，编译器尚未知 label 偏移，先写占位并在函数编译完后
    // 用 self.labels 里的已知偏移回填。 (data_off, label_A, label_B, width_bytes)
    pub pending_label_diff_rels: Vec<(u64, String, String, usize)>,
    // data 节最终重定位条目
    pub data_relocations: Vec<ElfRela>,
    // cleanup 属性变量：(变量名，清理函数名)
    pub cleanup_vars: Vec<(String, String)>,
    // 额外 .rela 节 (用于 .init_array/.fini_array 等)
    pub extra_rela_sections: Vec<ExtraRelaSection>,
    // COMDAT group 定义
    pub comdat_groups: Vec<ComdatGroup>,
    // .debug_info relocations: (offset_in_section, symbol_name)
    pub pending_debug_info_relocs: Vec<(u64, String)>,
    // .pushsection 非文本节数据收集
    pub pushsection_stack: Vec<usize>,
    /// .section/.previous 节切换栈 (独立于 pushsection_stack)
    pub section_stack: Vec<usize>,
    /// 标记当前节上下文是否由 .section 创建 (用于 .previous 正确区分 .pushsection)
    pub section_active: bool,
    pub pending_custom_sec_rels: Vec<CustomSecRel>,
    /// 可执行 pushsection (.fixup / .altinstr_replacement) 栈：(sec_idx, self.code 起始偏移)
    pub pushsection_exec_stack: Vec<(usize, u64)>,
    /// 可执行 pushsection 代码区间：(text 起始偏移，size, extra_sections 索引，节内偏移)
    pub pushsection_code_ranges: Vec<(u64, u64, usize, u64)>,
    /// 延迟解析：pushsection 数据中标签差值表达式 (前向引用)
    pub deferred_pushsection_fixups: Vec<DeferredPushsectionFixup>,
    // -S 汇编文本输出
    pub emit_asm_text: bool,
    pub debug_level: u8,
    pub emit_cfi: bool,
    // 编译选项：运行时检查 (-fbounds-check / -ftrapv / -fsanitize=address)
    pub bounds_check: bool,
    pub trapv: bool,
    /// KASAN: 栈/全局变量红区插桩
    pub kasan: bool,
    /// #pragma STDC FENV_ACCESS: 浮点环境访问模式.
    /// true 时浮点操作不得被重排或消除 (volatile 语义).
    pub fenv_access: bool,
    /// #pragma STDC FP_CONTRACT: true 时允许融合乘加收缩.
    pub fp_contract: bool,
    /// #pragma STDC CX_LIMITED_RANGE: true 时 complex 除法使用有限范围公式.
    pub cx_limited_range: bool,
    pub asm_directives: Vec<String>,
    pub asm_directives_at: Vec<(u64, String)>,
    pub disasm_calls: HashMap<u64, String>,
    pub disasm_data: HashMap<u64, String>,
    pub global_asm_exports: HashSet<String>,
    pub asm_func_symbols: HashSet<String>,
    pub asm_last_text_label: Option<(String, u64)>,
    pub func_asm_layout: HashMap<String, FuncAsmLayout>,
    pub source_name: String,
    pub parallel_codegen: bool,
    /// asm 块 DWARF 行号记录：(pc_start, pc_end, src_line)
    /// 用于在 -g 下生精确的.debug_line 条目
    pub asm_line_entries: Vec<(u64, u64, u32)>,
    /// asm 块内逐指令 DWARF 调试记录：(pc, src_line, discriminator)
    /// 用于在 -g 下生成 per-instruction.debug_line 条目，
    /// 使 GDB 能单步进入 asm 块内部每条指令 (类似 GCC.debug_insn 效果)
    pub asm_insn_debug_entries: Vec<(u64, u32, u16)>,
    // --- AsmEvent structured logging for -S output ---
    pub asm_events: Vec<crate::codegen::asm_event::AsmEvent>,
    /// dcc 编不出、必须透传给 GAS 的内联汇编行 (TU 级降级判据的唯一真源)。
    pub asm_passthrough: Vec<crate::codegen::shared::asm_source::AsmPassthroughRecord>,
    /// 透传语句在 .text 内的起点与其原始行 (降级文本按位置插入)。
    pub asm_passthrough_regions: Vec<(u64, Vec<String>)>,
    /// 当前 asm 行被本后端编码器拒收的标记 (由 `emit_asm_line` 消费)。
    pub asm_line_rejected: bool,
    pub rodata_string_init_globals: Vec<String>,
    pub rodata_string_slots: Vec<u64>,
    pub pending_data_rodata_rels: Vec<(u64, u64, Option<usize>)>,
    pub mcount_loc_entries: Vec<u64>,
    pub asm_label_map: HashMap<String, String>,
    pub tls_var_models: HashMap<String, String>,
    pub func_alignments: HashMap<String, usize>,
    // ── 寄存器分配器：caller/callee-saved 显式建模 ──
    pub callee_saved_pool: [u8; 4],
    pub reg_allocated: u8,
    pub callee_saved_used: u8,
    pub callee_save_patch_sites: Vec<(u64, u8)>,
    // ── 嵌套函数 trampoline ──
    pub trampoline_slots: HashMap<String, i64>,
    pub trampoline_total_size: i32,
    pub complete_struct_types: HashMap<String, Type>,
    // ── IFUNC / CPUID 合成（见 `ifunc.rs`）──
    /// 本 TU 合成出来的函数清单（`__dcc_cpu_indicator_init` + 每个 `<f>.resolver`）。
    /// 它们不在 `prog.decls` 里，所 `elf/rel_symbols.rs` 需要这份显式清单才能给它们
    /// 发「已定义」符号。
    pub ifunc_synth: Vec<ifunc::SynthFunc>,
    /// IFUNC 基符号清单（`st_value`/`st_size` 取自 `resolver` 的合成体）。
    pub ifunc_defs: Vec<ifunc::IfuncDef>,
    /// 本 TU 是否用过 `__builtin_cpu_*`（决定要不要合成 CPUID 部分）。
    /// 由 `special/builtins.rs` 的分支置位，合成 pass 在函数发射全部结束后跑。
    pub cpu_builtin_used: bool,
    /// `__dcc_cpu_model` 是否已登记进 `.data` 布局（只登记一次；为何用 `.data` 而非
    /// `.bss` 见 `ifunc.rs::ensure_cpu_model`）。
    pub cpu_model_registered: bool,
}
#[derive(Clone)]
pub struct GlobalVar {
    pub offset: u64,
    pub size: usize,
    pub ty: Type,
    pub init: Option<Vec<u8>>,
    pub section: u16,
    pub is_tls: bool,
    pub in_data_section: bool,
}

#[derive(Clone)]
pub struct PendingJump {
    /// 原始 self.code 中的指令起始偏移。
    pub at: u64,
    pub target: String,
    pub kind: JumpKind,
    /// disp32/imm32 字段在原始 self.code 中的偏移。
    pub disp_offset: u64,
    /// PC-relative 指令的下一条指令偏移。
    pub next_ip: u64,
    /// push $label: imm32 = absolute address (not PC-relative)
    pub is_push_imm: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum JumpKind {
    Jmp,
    Je,
    Jne,
    Jl,
    Jle,
    Jg,
    Jge,
    Jb,
    Ja,
    Jns,
    Jo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiClass {
    NoClass,
    Integer,
    Sse,
    SseUp,
    X87,
    X87Up,
    ComplexX87,
    Memory,
}

#[derive(Clone, Copy)]
pub enum ArgLoc {
    Gp(u8),
    Xmm(u8),
    Stack(usize),
}

#[derive(Clone)]
pub struct CustomSecRel {
    pub sec_idx: usize,
    pub offset: u64,
    pub symbol: String,
    pub rel_type: u32,
    pub addend: i64,
}

pub struct DeferredPushsectionFixup {
    pub sec_idx: usize,
    pub data_offset: usize,
    pub left_label: String,
    pub right_label: String,
    pub byte_size: usize, // 1 for .byte, 2 for .short
}

pub struct FuncAsmLayout {
    pub section: String,
    pub bytes: Vec<u8>,
    pub text_start: u64,
}
#[derive(Clone)]
pub struct PendingCodeRel {
    pub offset: u64,
    pub symbol: String,
    pub rel_type: u32,
    pub addend: i64,
}

pub enum VaListAddress {
    /// 命名变量 (局部或全局). codegen 内部按 is_global_not_local 分派.
    Named(String),
    /// va_list 地址已在寄存器 rcx 中 (来自指针解引用或参数退化).
    InReg,
    /// va_list 地址在 [r13 + offset] (来自结构体成员或数组下标).
    Lvalue(i32),
}

/// 聚合赋值**目标**的寻址方式。
///
/// 旧实现把目标固定成「一个有名字的变量」(``dst_name`` + ``dst_off`` 两个隐式
/// 参数), 于是 ``s.f = g()`` / ``a[i] = g()`` / ``*p = g()`` 这类**运行时地址**
/// 目标在 ``Expr::Assign`` 分发里根本进不到聚合路径, 落入恒 8 字节的标量 store。
/// 而聚合 rvalue 的值模型是**地址** (sret 缓冲 / 物化槽), 结果第一字被写成源
/// 地址、其余字保留栈上残值 —— 协程 awaiter 帧槽 >8 字节失效 (「多 quad 聚合值
/// 加载漏第二字」) 即此根因。
///
/// 把目标寻址方式收进枚举后, 目标只剩「怎么把地址放进寄存器」一个问题, 源侧的
/// 物化契约 (Call/sret/va_arg/_Complex/Vector/整体拷贝) 保持单一实现, 不再随
/// 目标形态分叉。
pub enum AggDst {
    /// 具名局部变量。``off`` = 帧槽 (历史 ``dst_off``); ``vla`` = VM struct/VLA
    /// 的元数据槽 (其内容才是数据区基址, 历史 ``vla_offsets.get(name)``)。
    Local { off: i64, vla: Option<i64> },
    /// 全局变量: 只有符号名, 无帧偏移。
    Global(String),
    /// 运行时地址, 已由调用方存进帧槽 ``addr_slot`` (即 ``[r13 + addr_slot]``)。
    /// 与 [`Self::Global`] 同属「不可按帧偏移直接定址」一类, 因此所有
    /// 「先物化到临时缓冲再整块写回」的路径都对它们开放。
    Indirect { addr_slot: i64 },
}

impl AggDst {
    /// 可直接按帧偏移定址的目标 (只有 [`Self::Local`])。
    pub fn frame_off(&self) -> Option<i64> {
        match self {
            AggDst::Local { off, .. } => Some(*off),
            _ => None,
        }
    }

    /// 目标无法按帧偏移直接写: 必须先物化到临时缓冲, 再整块拷贝到真实地址。
    pub fn needs_staged(&self) -> bool {
        self.frame_off().is_none()
    }
}

#[derive(Clone)]
pub struct ParallelGlobalSnap {
    pub offset: u64,
    pub size: usize,
    pub section: u16,
    pub is_tls: bool,
}

#[derive(Clone)]
pub struct ParallelEmitCtx {
    pub target: Target,
    pub relocatable: bool,
    pub function_sections: bool,
    pub emit_asm_text: bool,
    pub debug_level: u8,
    pub emit_cfi: bool,
    pub source_name: String,
    pub line_map: Vec<LineSpan>,
    pub globals: HashMap<String, ParallelGlobalSnap>,
    /// 符号 → 段地址空间。并行发射下**必须**跟着快照走：否则 worker 里的
    /// `emit_global_load/store/addr` 查不到段，同一个源文件在“并行/不并行”两条路上
    /// 产出不同的字节 —— 那是比不发前缀更难查的错。
    pub global_seg: HashMap<String, AddrSpace>,
    pub variadic_funcs: HashMap<String, bool>,
    pub old_style_funcs: std::collections::HashSet<String>,
    pub nested_funcs: HashMap<String, Vec<(String, i64)>>,
    pub nested_func_renames: HashMap<String, HashMap<String, String>>,
    pub nested_func_parent: HashMap<String, String>,
    pub nested_tramp_offsets: HashMap<String, i64>,
    pub nested_chain_offsets: HashMap<String, i64>,
    pub nested_func_rename_counter: u32,
    pub func_param_types: HashMap<String, Vec<Type>>,
    pub defined_funcs: HashSet<String>,
    pub error_attr_fns: HashSet<String>,
    pub global_reg_bindings: HashMap<String, (u8, Type)>,
    pub rodata: Vec<u8>,
    pub string_pool: HashMap<Vec<u8>, u64>,
    pub text_align: u64,
    /// 本 worker 的 `.L` 内部标号命名空间基址。
    ///
    /// `Codegen::next_label` 是 `.L<n>` 唯一的号源 (见 `instructions::fresh_label`),
    /// 而 worker 用 `Codegen::new` 重建 ⇒ 默认全从 0 起号 ⇒ 合并时同名覆盖。
    /// 由 `emit_functions_parallel` 按 **slot 下标** 静态分配互不重叠的区间。
    pub label_base: u32,
    /// 本 worker 可消耗的标号数; 高水位 = `label_base + label_span`。
    pub label_span: u32,
}

#[derive(Clone)]
pub struct FuncEmitChunk {
    pub name: String,
    pub code: Vec<u8>,
    pub entry_offset: u64,
    pub size: u64,
    pub labels: HashMap<String, u64>,
    pub pending: Vec<PendingJump>,
    pub string_refs: Vec<(u64, Vec<u8>)>,
    pub float_refs: Vec<(u64, u64)>,
    pub x87_float_refs: Vec<(u64, u64)>,
    pub pending_func_rels: Vec<(u64, String)>,
    pub pending_code_rels: Vec<PendingCodeRel>,
    pub pending_func_addr_rels: Vec<(u64, String, bool)>,
    pub pending_data_rels: Vec<(u64, String)>,
    pub pending_sib_data_rels: Vec<(u64, String)>,
    pub label_addr_refs: Vec<(u64, String)>,
    pub pending_custom_sec_rels: Vec<CustomSecRel>,
    pub pushsection_code_ranges: Vec<(u64, u64, usize, u64)>,
    pub extra_sections: Vec<elf::ElfSection>,
    pub custom_sec_indices: HashMap<String, usize>,
    /// C++ 零成本异常: 本 chunk 函数的 **CFI 事实**。
    ///
    /// 必须随 chunk 一并合并: 不合并则主 Codegen 没有该函数的序言/寄存器信息,
    /// `cfi_ops_for` 只能退回旧的同一段常量 CFA 规则 —— 并行与串行产物因此
    /// 字节不同 (不可复现), 且错规则会让 unwinder 走不出本帧。
    pub func_cfi: Option<crate::codegen::x86_64::eh_tables::FuncCfi>,
    /// C++ 零成本异常: 本 chunk 函数的受保护调用点归档 (`compile_func` 已在
    /// worker 内归档, 偏移相对 chunk 代码起点, 合并时加上落位基址)。
    pub eh_funcs: Vec<crate::codegen::x86_64::eh_tables::EhFuncInfo>,
    pub disasm_calls: HashMap<u64, String>,
    /// rodata 增量：chunk 编译期间新增的 rodata 数据
    pub new_rodata: Vec<u8>,
    /// chunk 开始时的 rodata 大小 (用于计算增量)
    pub rodata_base: u64,
    /// 本 chunk 的 `.L` 标号区间基址 / 容量 / 实际用量。
    ///
    /// `label_used > label_span` 意味着本函数越进了下一个 slot 的命名空间,
    /// 合并后两个函数的同名 `.L<n>` 会互相覆盖 —— 必须报错, 不能静默降级。
    pub label_base: u32,
    pub label_span: u32,
    pub label_used: u32,
}

impl Codegen {
    pub fn new(target: Target) -> Self {
        // 自 monolith 恢复: -fpatchable-function-entry 的 (N,M) 来自 target 描述,
        // 迁移期间此处丢失导致函数符号 value 不再跳过 patchable 前缀。
        let patchable_function_entry = target.patchable_function_entry;
        Self {
            code: vec![],
            entry: 0,
            target,
            labels: Default::default(),
            pending: vec![],
            next_label: 0,
            var_map: HashMap::new(),
            loop_continue: None,
            loop_break: None,
            break_targets: Vec::new(),
            rodata: vec![],
            string_pool: HashMap::new(),
            string_refs: vec![],
            float_refs: vec![],
            x87_float_refs: vec![],
            ubsan_lea_refs: vec![],
            ubsan_ptr_refs: vec![],
            globals_raw: HashMap::new(),
            global_seg: HashMap::new(),
            local_addr_seg: HashMap::new(),
            compound_lit_cache: HashMap::new(),
            frame_size: 0,
            expr_depth: 0,
            max_expr_depth: 0,
            temp_slots: 0,
            var_map_total_slots: 0,
            vec_temp_off: 0,
            vla_offsets: HashMap::new(),
            vla_total_size: 0,
            vm_struct_size_slots: HashMap::new(),
            cur_func_has_vla: false,
            bf_rmw_depth: 0,
            vector_op_slot: 1,
            relocatable: false,
            function_sections: false,
            patchable_function_entry,
            symbols: vec![],
            sym_index: HashMap::new(),
            relocations: vec![],
            pending_func_rels: vec![],
            pending_code_rels: vec![],
            pending_func_addr_rels: vec![],
            defined_funcs: HashSet::new(),
            error_attr_fns: HashSet::new(),
            pending_data_rels: vec![],
            pending_sib_data_rels: vec![],
            data_section: vec![],
            bss_size: 0,
            tdata_section: vec![],
            tbss_size: 0,
            tdata_align: 8,
            tbss_align: 8,
            exec_text_size: 0,
            pending_tls_rels: vec![],
            text_align: 16,
            data_align: 8,
            bss_align: 8,
            extra_sections: vec![],
            custom_sec_indices: HashMap::new(),
            func_sizes: HashMap::new(),
            eh_sites: Vec::new(),
            eh_funcs: Vec::new(),
            func_cfi: HashMap::new(),
            cur_prologue_end: 0,
            eh_lsda_offsets: HashMap::new(),
            eh_lsda_relocs: Vec::new(),
            eh_except_sec_idx: None,
            variadic_funcs: HashMap::new(),
            old_style_funcs: std::collections::HashSet::new(),
            variadic_named_params: 0,
            variadic_named_gp: 0,
            variadic_named_fp: 0,
            variadic_overflow_offset: 16,
            variadic_reg_save_offset: 0,
            current_sret_offset: None,
            sret_watermark: 0,
            sret_region_base: 0,
            nested_funcs: HashMap::new(),
            nested_func_renames: HashMap::new(),
            nested_func_parent: HashMap::new(),
            nested_tramp_offsets: HashMap::new(),
            nested_chain_offsets: HashMap::new(),
            nested_func_rename_counter: 0,
            current_func_name: String::new(),
            current_func_ret_ty: Type::Int,
            static_local_map: crate::static_locals::StaticLocalMap::default(),
            real_global_names: std::collections::HashSet::new(),
            func_param_types: HashMap::new(),
            current_func_is_nested: false,
            local_labels: std::collections::HashSet::new(),
            current_returns_nonnull: false,
            current_is_noreturn: false,
            current_no_instrument: false,
            static_chain_offset: 0,
            upvar_offsets: HashMap::new(),
            upvar_chain_depth: HashMap::new(),
            upvar_intermediate_offsets: HashMap::new(),
            label_addr_refs: vec![],
            reg_bindings: HashMap::new(),
            addr_escaped: std::collections::HashSet::new(),
            addr_escaped_done: false,
            global_reg_bindings: HashMap::new(),
            pending_data64_rels: vec![],
            pending_rodata_rels: vec![],
            pending_label_diff_rels: vec![],
            data_relocations: vec![],
            cleanup_vars: vec![],
            extra_rela_sections: vec![],
            comdat_groups: vec![],
            pending_debug_info_relocs: vec![],
            pushsection_stack: vec![],
            section_stack: vec![],
            section_active: false,
            pending_custom_sec_rels: vec![],
            pushsection_exec_stack: vec![],
            pushsection_code_ranges: vec![],
            deferred_pushsection_fixups: vec![],
            emit_asm_text: false,
            debug_level: 0,
            emit_cfi: false,
            bounds_check: false,
            trapv: false,
            kasan: false,
            fenv_access: false,
            fp_contract: false,
            cx_limited_range: false,
            asm_directives: vec![],
            asm_directives_at: vec![],
            disasm_calls: HashMap::new(),
            disasm_data: HashMap::new(),
            global_asm_exports: HashSet::new(),
            asm_func_symbols: HashSet::new(),
            asm_last_text_label: None,
            func_asm_layout: HashMap::new(),
            asm_line_entries: Vec::new(),
            asm_insn_debug_entries: Vec::new(),
            // --- AsmEvent structured logging ---
            asm_events: Vec::new(),
            asm_passthrough: Vec::new(),
            asm_passthrough_regions: Vec::new(),
            asm_line_rejected: false,
            current_asm_line: 0,
            current_asm_src_line: 0,
            diagnostics: Vec::new(),
            line_map: Vec::new(),
            source_name: String::new(),
            parallel_codegen: false,
            rodata_string_init_globals: vec![],
            rodata_string_slots: vec![],
            pending_data_rodata_rels: vec![],
            mcount_loc_entries: vec![],
            asm_label_map: HashMap::new(),
            tls_var_models: HashMap::new(),
            // 寄存器分配器初始化
            callee_saved_pool: [3, 12, 14, 15], // rbx, r12, r14, r15
            reg_allocated: 0,
            callee_saved_used: 0,
            callee_save_patch_sites: vec![],
            // 嵌套函数 trampoline
            trampoline_slots: HashMap::new(),
            trampoline_total_size: 0,
            // Function Alignment lookup for __alignof__(func)
            func_alignments: HashMap::new(),
            // 完整 struct/union 定义映射 (前向引用解析)
            complete_struct_types: HashMap::new(),
            // IFUNC / CPUID 合成：展开产物在 `compile_rel` 尾部消费
            ifunc_synth: Vec::new(),
            ifunc_defs: Vec::new(),
            cpu_builtin_used: false,
            cpu_model_registered: false,
        }
    }

    pub fn compile_module(&mut self, module: &crate::ir::Module) {
        let prog = module.source();
        self.fenv_access = prog.fenv_access;
        self.fp_contract = prog.fp_contract;
        self.cx_limited_range = prog.cx_limited_range;
        // 构建完整 struct/union 定义映射 (用于解析前向引用导致的不完整类型)
        self.complete_struct_types.clear();
        for decl in &prog.decls {
            match decl {
                Decl::StructDef(sd) if !sd.fields.is_empty() => {
                    let fields: Arc<Vec<Field>> = Arc::new(sd.fields.clone());
                    let align = Type::aggregate_align_from_offsets(&sd.fields);
                    self.complete_struct_types.insert(
                        sd.name.name.clone(),
                        Type::Struct(sd.name.name.clone(), fields, align, false),
                    );
                },
                Decl::UnionDef(sd) if !sd.fields.is_empty() => {
                    let fields: Arc<Vec<Field>> = Arc::new(sd.fields.clone());
                    let align = Type::aggregate_align_from_offsets(&sd.fields);
                    self.complete_struct_types.insert(
                        sd.name.name.clone(),
                        Type::Union(sd.name.name.clone(), fields, align, false),
                    );
                },
                Decl::Typedef { ty, .. } => {
                    // typedef struct S { ... } S; 形式: 提取内层 struct/union
                    if let Type::Struct(name, fields, align, _) = ty {
                        if !fields.is_empty() {
                            self.complete_struct_types.insert(
                                name.clone(),
                                Type::Struct(name.clone(), fields.clone(), *align, false),
                            );
                        }
                    } else if let Type::Union(name, fields, align, _) = ty {
                        if !fields.is_empty() {
                            self.complete_struct_types.insert(
                                name.clone(),
                                Type::Union(name.clone(), fields.clone(), *align, false),
                            );
                        }
                    }
                },
                _ => {},
            }
        }
        // 检查 __attribute__((error/warning/deprecated)): 扫描所有函数声明
        // 如果函数体中调用了这些函数，发射诊断信息
        let diag_fns: Vec<AttrDiagFn> = prog
            .decls
            .iter()
            .filter_map(|d| {
                if let Decl::Func(f) = d {
                    if f.error_msg.is_some()
                        || f.warning_msg.is_some()
                        || f.deprecated_msg.is_some()
                    {
                        return Some((
                            f.name.name.as_str(),
                            f.error_msg.as_deref(),
                            f.warning_msg.as_deref(),
                            f.deprecated_msg.as_deref(),
                        ));
                    }
                }
                None
            })
            .collect();
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                // 扫描函数体对被标记 error/warning/deprecated 函数的调用
                // (自 monolith 3604 恢复: 迁移期间此行丢失, diag_fns 闲置)
                if let Some(body) = &f.body {
                    Self::check_attr_diagnostics(&body.items, &diag_fns, &f.name.name);
                }
                // 记录 __attribute__((error)) 函数名，调用时发射 ud2
                if f.error_msg.is_some() {
                    self.error_attr_fns.insert(f.name.name.clone());
                }
            }
        }
        // 预扫描：收集函数对齐信息 for __alignof__(func)
        self.func_alignments.clear();
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                self.func_alignments
                    .insert(f.name.name.clone(), f.alignment.unwrap_or(8).max(1));
            }
        }
        // 预扫描: 记录哪些函数是可变参数的
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                Self::register_func_sig(f, &mut self.func_param_types);
                if f.is_variadic {
                    self.variadic_funcs.insert(f.name.name.clone(), true);
                }
                if f.is_old_style {
                    self.old_style_funcs.insert(f.name.name.clone());
                }
                if let Some(body) = &f.body {
                    let mut nested = Vec::new();
                    Self::collect_nested_funcs(&body.items, &mut nested);
                    for nf in nested {
                        Self::register_func_sig(&nf, &mut self.func_param_types);
                        self.nested_funcs
                            .insert(nf.name.name.clone(), nf.upvars.clone());
                        if nf.is_old_style {
                            self.old_style_funcs.insert(nf.name.name.clone());
                        }
                    }
                }
            }
        }
        // 预扫描: 收集 GNU asm label 映射 (C 名 → asm("label"))
        // 同时收集 TLS 变量访问模型
        for decl in &prog.decls {
            match decl {
                Decl::Func(f) => {
                    if let Some(ref label) = f.asm_label {
                        self.asm_label_map
                            .insert(f.name.name.clone(), label.clone());
                    }
                },
                Decl::Var(v) => {
                    if let Some(ref label) = v.asm_label {
                        self.asm_label_map
                            .insert(v.name.name.clone(), label.clone());
                    }
                    if v.is_thread_local {
                        if let Some(ref model) = v.tls_model {
                            self.tls_var_models
                                .insert(v.name.name.clone(), model.clone());
                        }
                    }
                    // 段寄存器限定符加在**对象本体**上（`__seg_gs int gv;`）：那是符号的
                    // 属性，不挂在任何表达式的类型上，所以只能在此处进表。与 TLS 是两条
                    // 独立的轴（上面 `is_thread_local` 那支不干涉本支）。
                    if v.addr_space.is_segment() {
                        self.global_seg
                            .insert(v.name.name.clone(), v.addr_space);
                    }
                },
                _ => {},
            }
        }
        if self.relocatable {
            self.compile_rel(prog);
        } else {
            self.compile_exec(prog);
        }
    }

    /// 收集函数体内 `static` 局部变量的段限定，以 **mangled 名**进 `global_seg`。
    ///
    /// 为什么不能走上面那个 file-scope 循环：函数体里的 `static __seg_fs int x;` 在
    /// `globals_raw` / 发射侧都以 `.Lsl<N>.<func>.<x>` 登记（`resolve_global_key` 把源名
    /// 映到 mangled 名），用源名进表永远查不到 —— 那是“看起来接了、其实没接”。
    /// 本 pass 必须跑在 `analyze_static_locals` 之后（要靠它提供 mangled 名）。
    fn collect_static_local_segments(&mut self, prog: &Program) {
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if let Some(body) = &f.body {
                    let found = Self::collect_seg_statics(body);
                    for (src, addr) in found {
                        if let Some(m) = self.static_local_map.resolve(&f.name.name, &src) {
                            let m = m.to_string();
                            self.global_seg.insert(m, addr);
                        }
                    }
                }
            }
        }
    }

    /// 递归收集一个块（含 switch 分支与嵌套函数体）里带段限定的 `static` 声明。
    /// 只读 AST，返回 (源名, 地址空间)；避开 `&mut self` 重入借用的同时少一处可变状态。
    fn collect_seg_statics(b: &crate::ast::Block) -> Vec<(String, AddrSpace)> {
        let mut out = Vec::new();
        for item in &b.items {
            match item {
                crate::ast::BlockItem::Decl(Decl::Var(v)) => {
                    if v.is_static && v.addr_space.is_segment() {
                        out.push((v.name.name.clone(), v.addr_space));
                    }
                },
                crate::ast::BlockItem::Decl(Decl::Func(nf)) => {
                    // 嵌套函数体里的 static 同样要 cover（发射时归属那个嵌套函数）
                    if let Some(body) = &nf.body {
                        out.extend(Self::collect_seg_statics(body));
                    }
                },
                crate::ast::BlockItem::Stmt(s) => out.extend(Self::collect_seg_statics_of_stmt(s)),
                // StructDef/UnionDef/EnumDef/Typedef 等声明不含对象本体
                _ => {}
            }
        }
        out
    }

    /// 语句里的子块/子语句递归（非块状语句包一层不会找到声明，直接返回空）。
    fn collect_seg_statics_of_stmt(s: &crate::ast::Stmt) -> Vec<(String, AddrSpace)> {
        use crate::ast::Stmt;
        match s {
            Stmt::Block { b, .. } => Self::collect_seg_statics(b),
            Stmt::Switch { cases, .. } => {
                let mut out = Vec::new();
                for c in cases {
                    out.extend(Self::collect_seg_statics(&c.body));
                }
                out
            },
            Stmt::If {
                then_, else_, ..
            } => {
                let mut out = Self::collect_seg_statics_of_stmt(then_);
                if let Some(e) = else_ {
                    out.extend(Self::collect_seg_statics_of_stmt(e));
                }
                out
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } | Stmt::For { body, .. } => {
                Self::collect_seg_statics_of_stmt(body)
            },
            Stmt::Label { stmt, .. } => Self::collect_seg_statics_of_stmt(stmt),
            _ => Vec::new(),
        }
    }

    /// ET_EXEC 模式: 生成可直接执行的 ELF
    fn compile_exec(&mut self, prog: &Program) {
        // IFUNC / CPUID 合成只在 ET_REL 路径实现（resolver 需要重定位与动态链接器
        // 的 R_X86_64_IRELATIVE 机制）。这里绝不能“继续但发一个指向实现体的 IFUNC”
        // —— 那正是本轮要修的静默降级缺陷的源头：符号表看上去有 IFUNC，实际没有
        // resolver，链接期只能挑一条路径，而且很可能是一条错的路径。
        if ifunc::needs_ifunc_synthesis(prog) {
            self.report_no_exec_ifunc_support();
            return;
        }
        self.entry = 0x400000 + 64 + 56;
        // 收集本 TU 中有函数体的函数名
        self.defined_funcs.clear();
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_some() {
                    self.defined_funcs.insert(f.name.name.clone());
                }
            }
        }
        // 收集全局变量 → rodata / .tdata(TLS)
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                if v.is_extern {
                    continue;
                }
                let sz_raw = v.ty.size().max(1);
                // 超大数组 (>1GB) 可能导致内存分配失败，视为外部声明不分配存储
                if sz_raw > (1 << 30) {
                    continue;
                }
                let sz = sz_raw;
                let align = v.alignment.unwrap_or(v.ty.align()).max(1);
                if v.is_thread_local {
                    self.tdata_align = self.tdata_align.max(align as u64);
                    // ET_EXEC: 所有 TLS 数据（含 .tbss）都在单一 TLS 模板中
                    let off = (self.tdata_section.len() + align - 1) & !(align - 1);
                    while self.tdata_section.len() < off {
                        self.tdata_section.push(0);
                    }
                    let offset = self.tdata_section.len() as u64;
                    let init_data = Self::eval_global_init(self, v, sz, false);
                    if let Some(ref data) = init_data {
                        self.tdata_section.extend_from_slice(data);
                        for _ in data.len()..sz {
                            self.tdata_section.push(0);
                        }
                    } else {
                        for _ in 0..sz {
                            self.tdata_section.push(0);
                        }
                    }
                    let sec = if v.init.is_some() { 5u16 } else { 6u16 };
                    if v.init.is_none() {
                        self.tbss_size = self.tbss_size.max(offset + sz as u64);
                    }
                    self.globals_raw.insert(
                        v.name.name.clone(),
                        GlobalVar {
                            offset,
                            size: sz,
                            ty: v.ty.clone(),
                            init: init_data,
                            section: sec,
                            is_tls: true,
                            in_data_section: false,
                        },
                    );
                    continue;
                }
                let off = (self.rodata.len() + align - 1) & !(align - 1);
                while self.rodata.len() < off {
                    self.rodata.push(0);
                }
                let offset = self.rodata.len() as u64;
                // 字符串字面量初始化指针 (char *s = "hello"): 字符串进 rodata 池,
                // 指针槽写入字符串 rodata 偏移 (占位), resolve 阶段修补为绝对地址。
                if matches!(&v.ty, Type::Ptr(_)) {
                    if let Some(Expr::String { data, .. }) = &v.init {
                        // 1. 预留 8 字节指针槽 (offset 处)
                        self.rodata.extend_from_slice(&[0u8; 8]);
                        // 2. 字符串追加到 rodata 末尾 (池化去重)
                        let data = data.clone();
                        if !self.string_pool.contains_key(&data) {
                            let o = self.rodata.len() as u64;
                            self.rodata.extend_from_slice(&data);
                            self.string_pool.insert(data.clone(), o);
                        }
                        let str_off = *self.string_pool.get(&data).unwrap();
                        // 3. 指针槽写入字符串 rodata 偏移 (resolve 修补)
                        let slot = offset as usize;
                        self.rodata[slot..slot + 8].copy_from_slice(&str_off.to_le_bytes());
                        // 4. 记录待修补变量
                        self.rodata_string_init_globals.push(v.name.name.clone());
                        self.globals_raw.insert(
                            v.name.name.clone(),
                            GlobalVar {
                                offset,
                                size: sz,
                                ty: v.ty.clone(),
                                init: None,
                                section: 1,
                                is_tls: false,
                                in_data_section: false,
                            },
                        );
                        continue;
                    }
                }
                let init_data = Self::eval_global_init(self, v, sz, true);
                if let Some(ref data) = init_data {
                    self.rodata.extend_from_slice(data);
                    for _ in data.len()..sz {
                        self.rodata.push(0);
                    }
                } else {
                    for _ in 0..sz {
                        self.rodata.push(0);
                    }
                }
                self.globals_raw.insert(
                    v.name.name.clone(),
                    GlobalVar {
                        offset,
                        size: sz,
                        ty: v.ty.clone(),
                        init: init_data,
                        section: 1,
                        is_tls: false,
                        in_data_section: false,
                    },
                );
                // KASAN 全局变量红区: 追加 32 字节零填充
                if self.kasan {
                    for _ in 0..32 {
                        self.rodata.push(0);
                    }
                }
            }
        }
        let _rodata_size = self.rodata.len();
        let _main = prog
            .decls
            .iter()
            .find_map(|d| match d {
                Decl::Func(f) if f.name.name == "main" => Some(f),
                _ => None,
            })
            .expect("no main()");
        // 收集 constructor/destructor 函数 (按 priority 排序)
        let mut ctors: Vec<(u16, String)> = prog
            .decls
            .iter()
            .filter_map(|d| {
                if let Decl::Func(f) = d {
                    if f.is_constructor && f.body.is_some() {
                        Some((f.ctor_priority.unwrap_or(65535), f.name.name.clone()))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();
        let mut dtors: Vec<(u16, String)> = prog
            .decls
            .iter()
            .filter_map(|d| {
                if let Decl::Func(f) = d {
                    if f.is_destructor && f.body.is_some() {
                        Some((f.dtor_priority.unwrap_or(65535), f.name.name.clone()))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();
        ctors.sort_by_key(|(p, _)| *p);
        dtors.sort_by_key(|(p, _)| *p);
        self.label("_start");
        self.emit(&[0x49, 0x89, 0xe4]); // mov r12, rsp (保存原始 rsp, 用于 leave 后恢复)
        self.emit(&[0x55]); // push rbp
        self.emit(&[0x48, 0x89, 0xe5]); // mov rbp, rsp
                                        // 调用 constructor 函数
        for (_p, ctor) in &ctors {
            self.emit(&[0x48, 0x31, 0xc0]); // xor rax, rax (no variadic args)
            self.call_label(ctor);
        }
        self.emit(&[0x48, 0x31, 0xc0]); // xor rax, rax
        self.call_label("main");
        // 保存 main 返回值
        self.emit(&[0x41, 0x89, 0xc5]); // mov r13d, eax
                                        // 调用 destructor 函数
        for (_p, dtor) in &dtors {
            self.emit(&[0x48, 0x31, 0xc0]); // xor rax, rax
            self.call_label(dtor);
        }
        self.emit(&[0x4c, 0x89, 0xe4]); // mov rsp, r12 (恢复原始 rsp)
        self.emit(&[0x44, 0x89, 0xef]); // mov edi, r13d (main 返回值)
        self.emit(&[0xb8, 0x3c, 0x00, 0x00, 0x00]); // mov eax, 0x3c
        self.emit(&[0x0f, 0x05]); // syscall
        self.emit(&[0xeb, 0xfe]); // jmp self
                                  // 静态局部变量分析 pass + 预注册到 data/bss
        self.static_local_map = crate::static_locals::analyze_static_locals(prog);
        self.pre_register_static_locals();
        self.collect_static_local_segments(prog);
        self.emit_functions(prog);
        // 修补 data_section 里的 label 地址差值
        // (`&&A - &&B` 在静态初始化式中出现, 编译期 label 未知,
        // 函数编译完后 self.labels 已填充, 用 offset 差值回填).
        self.patch_label_diff_rels();
        // 对齐 code 到 8 字节 (ELF 的 .rodata 也按 8 对齐)
        while !self.code.len().is_multiple_of(8) {
            self.code.push(0);
        }
        self.exec_text_size = self.code.len() as u64;
        let rodata_start = self.entry + self.exec_text_size;
        self.code.extend_from_slice(&self.rodata);
        self.code.extend_from_slice(&self.tdata_section);
        // Static 局部变量: 注册到 data_section, 需追加到可执行文件
        // 对齐 data_section 到 8 字节边界
        while !self.data_section.len().is_multiple_of(8) {
            self.data_section.push(0);
        }
        let data_start = self.entry
            + self.exec_text_size
            + self.rodata.len() as u64
            + self.tdata_section.len() as u64;
        self.code.extend_from_slice(&self.data_section);
        // BSS: 仅记录大小, 不写入文件内容
        self.code
            .extend_from_slice(&vec![0u8; self.bss_size as usize]);
        self.collect_exec_symbols(prog);
        self.resolve(rodata_start, data_start);
    }

    fn compile_rel(&mut self, prog: &Program) {
        // 收集本 TU 中有函数体的函数名 (用于 emit_func_addr 区分内部/外部)
        self.defined_funcs.clear();
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_some() {
                    self.defined_funcs.insert(f.name.name.clone());
                }
            }
        }
        // Phase 1: 生成代码, 收集数据
        // 收集全局变量 → .data / .bss (或自定义 section)
        for decl in &prog.decls {
            if let Decl::Var(v) = decl {
                if let Some(ref reg_name) = v.register_name {
                    if let Some(reg_idx) = Self::parse_reg_name(reg_name) {
                        self.global_reg_bindings
                            .insert(v.name.name.clone(), (reg_idx, v.ty.clone()));
                    }
                    continue;
                }
                let sz_raw = v.ty.size().max(1);
                // 超大数组 (>1GB) 可能导致内存分配失败，视为外部声明不分配存储
                if sz_raw > (1 << 30) {
                    continue;
                }
                let sz = sz_raw;
                let align = v.alignment.unwrap_or(v.ty.align()).max(1);
                if v.is_extern && v.init.is_none() {
                    self.globals_raw.insert(
                        v.name.name.clone(),
                        GlobalVar {
                            offset: 0,
                            size: sz,
                            ty: v.ty.clone(),
                            init: None,
                            section: crate::codegen::shared::elf::SHN_UNDEF,
                            is_tls: false,
                            in_data_section: false,
                        },
                    );
                    continue;
                }
                if v.is_thread_local {
                    let initialized = v.init.is_some();
                    if initialized {
                        self.tdata_align = self.tdata_align.max(align as u64);
                    } else {
                        self.tbss_align = self.tbss_align.max(align as u64);
                    }
                    let offset = if initialized {
                        let off = (self.tdata_section.len() + align - 1) & !(align - 1);
                        while self.tdata_section.len() < off {
                            self.tdata_section.push(0);
                        }
                        self.tdata_section.len() as u64
                    } else {
                        let off = self.tbss_size.div_ceil(align as u64) * align as u64;
                        self.tbss_size = off + sz as u64;
                        off
                    };
                    if let Some(init) = &v.init {
                        // 浮点目标: eval_full 取位模式 (double x = 1 / 1.5;),
                        // 避免 eval 截断为 int
                        let is_float =
                            matches!(v.ty, Type::Float | Type::Double | Type::LongDouble);
                        if let Some(bytes) = global::global_init_bytes(init, sz, is_float, &v.ty) {
                            self.tdata_section.extend_from_slice(&bytes);
                        }
                    }
                    self.globals_raw.insert(
                        v.name.name.clone(),
                        GlobalVar {
                            offset,
                            size: sz,
                            ty: v.ty.clone(),
                            init: None,
                            section: if initialized { 5 } else { 6 },
                            is_tls: true,
                            in_data_section: false,
                        },
                    );
                    continue;
                }
                // attribute((used)): 路由到独立节并标记 SHF_GNU_RETAIN
                if v.is_used_attr && !v.is_thread_local {
                    let sec_name = if v.init.is_some() {
                        format!(".data.{}", v.name.name)
                    } else {
                        format!(".bss.{}", v.name.name)
                    };
                    let is_writable = true;
                    let sec_idx = self.ensure_custom_section(&sec_name, false, is_writable);
                    // 标记 SHF_GNU_RETAIN 防止 gc-sections 移除
                    self.extra_sections[sec_idx].flags |=
                        crate::codegen::shared::elf::SHF_GNU_RETAIN;
                    self.extra_sections[sec_idx].align =
                        self.extra_sections[sec_idx].align.max(align as u64);
                    if v.init.is_none() && self.extra_sections[sec_idx].data.is_empty() {
                        self.extra_sections[sec_idx].ty = crate::codegen::shared::elf::SHT_NOBITS;
                    } else if v.init.is_some() {
                        self.extra_sections[sec_idx].ty = crate::codegen::shared::elf::SHT_PROGBITS;
                    }
                    let sec = &mut self.extra_sections[sec_idx];
                    let off = (sec.data.len() + align - 1) & !(align - 1);
                    while sec.data.len() < off {
                        sec.data.push(0);
                    }
                    let offset = sec.data.len() as u64;
                    let mut has_init = false;
                    if let Some(init) = &v.init {
                        match init {
                            Expr::String { data, .. } => {
                                // 指针类型 (char *s = "hello"): 字符串进 rodata 池,
                                // 指针槽通过 pending_data_rodata_rels 生成
                                // R_X86_64_64 重定位 (符号 = .rodata, addend = str_off)。
                                if matches!(&v.ty, Type::Ptr(_)) {
                                    let data = data.clone();
                                    if !self.string_pool.contains_key(&data) {
                                        let o = self.rodata.len() as u64;
                                        self.rodata.extend_from_slice(&data);
                                        self.string_pool.insert(data.clone(), o);
                                    }
                                    let str_off = *self.string_pool.get(&data).unwrap();
                                    let cur = self.extra_sections[sec_idx].data.len();
                                    self.extra_sections[sec_idx].data.resize(cur + 8, 0);
                                    self.pending_data_rodata_rels.push((
                                        cur as u64,
                                        str_off,
                                        Some(sec_idx),
                                    ));
                                    has_init = true;
                                } else {
                                    self.extra_sections[sec_idx].data.extend_from_slice(data);
                                    for _ in data.len()..sz {
                                        self.extra_sections[sec_idx].data.push(0);
                                    }
                                    has_init = true;
                                }
                            },
                            Expr::InitList { inits, .. } => {
                                let base = self.extra_sections[sec_idx].data.len() as u64;
                                if let Some(init_data) = self.gen_global_init_list(
                                    &v.ty,
                                    inits,
                                    Some((base, Some(sec_idx))),
                                    false,
                                ) {
                                    self.extra_sections[sec_idx]
                                        .data
                                        .extend_from_slice(&init_data);
                                    for _ in init_data.len()..sz {
                                        self.extra_sections[sec_idx].data.push(0);
                                    }
                                    has_init = true;
                                }
                            },
                            Expr::AddrOf { e, .. } => {
                                // P1-6: 自定义 section 中的符号引用
                                if let Expr::Ident { name: ref_name, .. } = e.as_ref() {
                                    let ref_off = self.extra_sections[sec_idx].data.len() as u64;
                                    let width = if sz <= 4 { 4 } else { 8 };
                                    self.extra_sections[sec_idx]
                                        .data
                                        .resize(ref_off as usize + sz.max(width), 0);
                                    let rel_type = if width == 4 {
                                        crate::codegen::shared::elf::R_X86_64_32
                                    } else {
                                        crate::codegen::shared::elf::R_X86_64_64
                                    };
                                    self.pending_data64_rels.push((
                                        ref_off,
                                        ref_name.name.clone(),
                                        Some(sec_idx),
                                        rel_type,
                                        0,
                                    ));
                                    has_init = true;
                                } else if let Expr::Index {
                                    arr,
                                    idx,
                                    ty: elem_ty,
                                    ..
                                } = e.as_ref()
                                {
                                    if let (Expr::Ident { name: ref_name, .. }, Ok(i)) =
                                        (arr.as_ref(), const_eval::eval(idx))
                                    {
                                        let ref_off =
                                            self.extra_sections[sec_idx].data.len() as u64;
                                        let width = if sz <= 4 { 4 } else { 8 };
                                        self.extra_sections[sec_idx]
                                            .data
                                            .resize(ref_off as usize + sz.max(width), 0);
                                        let rel_type = if width == 4 {
                                            crate::codegen::shared::elf::R_X86_64_32
                                        } else {
                                            crate::codegen::shared::elf::R_X86_64_64
                                        };
                                        let addend = (i as i64) * elem_ty.size().max(1) as i64;
                                        self.pending_data64_rels.push((
                                            ref_off,
                                            ref_name.name.clone(),
                                            Some(sec_idx),
                                            rel_type,
                                            addend,
                                        ));
                                        has_init = true;
                                    }
                                }
                            },
                            _ => {
                                // 浮点目标: eval_full 取位模式 (double x = 1 / 1.5;)
                                let is_float =
                                    matches!(v.ty, Type::Float | Type::Double | Type::LongDouble);
                                if let Some(bytes) =
                                    global::global_init_bytes(init, sz, is_float, &v.ty)
                                {
                                    self.extra_sections[sec_idx].data.extend_from_slice(&bytes);
                                    has_init = true;
                                }
                            },
                        }
                    }
                    if !has_init {
                        for _ in 0..sz {
                            self.extra_sections[sec_idx].data.push(0);
                        }
                    }
                    self.globals_raw.insert(
                        v.name.name.clone(),
                        GlobalVar {
                            offset,
                            size: sz,
                            ty: v.ty.clone(),
                            init: None,
                            section: (sec_idx + 7) as u16,
                            is_tls: false,
                            in_data_section: false,
                        },
                    );
                    continue;
                }
                // 自定义 section 变量: 路由到 extra_sections
                if let Some(sec_name) = &v.section {
                    let is_writable = !matches!(&v.init, Some(Expr::String { .. }));
                    let sec_idx = self.ensure_custom_section(sec_name, false, is_writable);
                    self.extra_sections[sec_idx].align =
                        self.extra_sections[sec_idx].align.max(align as u64);
                    if v.init.is_none() && self.extra_sections[sec_idx].data.is_empty() {
                        self.extra_sections[sec_idx].ty = crate::codegen::shared::elf::SHT_NOBITS;
                    } else if v.init.is_some() {
                        // 同名节一旦包含显式初始化数据就必须占文件空间。
                        self.extra_sections[sec_idx].ty = crate::codegen::shared::elf::SHT_PROGBITS;
                    }
                    let sec = &mut self.extra_sections[sec_idx];
                    let off = (sec.data.len() + align - 1) & !(align - 1);
                    while sec.data.len() < off {
                        sec.data.push(0);
                    }
                    let offset = sec.data.len() as u64;
                    let mut has_init = false;
                    if let Some(init) = &v.init {
                        match init {
                            Expr::String { data, .. } => {
                                // 指针类型 (char *s = "hello"): 字符串进 rodata 池,
                                // 指针槽通过 pending_data_rodata_rels 生成
                                // R_X86_64_64 重定位 (符号 = .rodata, addend = str_off)。
                                if matches!(&v.ty, Type::Ptr(_)) {
                                    let data = data.clone();
                                    if !self.string_pool.contains_key(&data) {
                                        let o = self.rodata.len() as u64;
                                        self.rodata.extend_from_slice(&data);
                                        self.string_pool.insert(data.clone(), o);
                                    }
                                    let str_off = *self.string_pool.get(&data).unwrap();
                                    let cur = self.extra_sections[sec_idx].data.len();
                                    self.extra_sections[sec_idx].data.resize(cur + 8, 0);
                                    self.pending_data_rodata_rels.push((
                                        cur as u64,
                                        str_off,
                                        Some(sec_idx),
                                    ));
                                } else {
                                    self.extra_sections[sec_idx].data.extend_from_slice(data);
                                    for _ in data.len()..sz {
                                        self.extra_sections[sec_idx].data.push(0);
                                    }
                                }
                                has_init = true;
                            },
                            Expr::InitList { inits, .. } => {
                                let base = self.extra_sections[sec_idx].data.len() as u64;
                                if let Some(init_data) = self.gen_global_init_list(
                                    &v.ty,
                                    inits,
                                    Some((base, Some(sec_idx))),
                                    false,
                                ) {
                                    self.extra_sections[sec_idx]
                                        .data
                                        .extend_from_slice(&init_data);
                                    for _ in init_data.len()..sz {
                                        self.extra_sections[sec_idx].data.push(0);
                                    }
                                    has_init = true;
                                }
                            },
                            Expr::AddrOf { e, .. } => {
                                // P1-6: .data 节中的符号引用
                                // Handle &ident, &a[i], &a[i][j], &s.field, etc.
                                if let Some((base_name, byte_off)) =
                                    global::eval_global_lvalue_offset(e, &self.globals_raw)
                                {
                                    let ref_off = self.extra_sections[sec_idx].data.len() as u64;
                                    let width = if sz <= 4 { 4 } else { 8 };
                                    self.extra_sections[sec_idx]
                                        .data
                                        .resize(ref_off as usize + sz.max(width), 0);
                                    let rel_type = if width == 4 {
                                        crate::codegen::shared::elf::R_X86_64_32
                                    } else {
                                        crate::codegen::shared::elf::R_X86_64_64
                                    };
                                    self.pending_data64_rels.push((
                                        ref_off,
                                        base_name,
                                        Some(sec_idx),
                                        rel_type,
                                        byte_off,
                                    ));
                                    has_init = true;
                                } else if let Expr::Ident { name: ref_name, .. } = e.as_ref() {
                                    // Fallback for function pointers and other symbols not in globals
                                    let ref_off = self.extra_sections[sec_idx].data.len() as u64;
                                    let width = if sz <= 4 { 4 } else { 8 };
                                    self.extra_sections[sec_idx]
                                        .data
                                        .resize(ref_off as usize + sz.max(width), 0);
                                    let rel_type = if width == 4 {
                                        crate::codegen::shared::elf::R_X86_64_32
                                    } else {
                                        crate::codegen::shared::elf::R_X86_64_64
                                    };
                                    self.pending_data64_rels.push((
                                        ref_off,
                                        ref_name.name.clone(),
                                        Some(sec_idx),
                                        rel_type,
                                        0,
                                    ));
                                    has_init = true;
                                } else if let Some((anon_name, _off)) =
                                    global::materialize_compound_literal(self, e, false)
                                {
                                    // &(struct A){42,43} — C99 合法的文件地址常量
                                    let ref_off = self.extra_sections[sec_idx].data.len() as u64;
                                    let width = if sz <= 4 { 4 } else { 8 };
                                    self.extra_sections[sec_idx]
                                        .data
                                        .resize(ref_off as usize + sz.max(width), 0);
                                    let rel_type = if width == 4 {
                                        crate::codegen::shared::elf::R_X86_64_32
                                    } else {
                                        crate::codegen::shared::elf::R_X86_64_64
                                    };
                                    self.pending_data64_rels.push((
                                        ref_off,
                                        anon_name,
                                        Some(sec_idx),
                                        rel_type,
                                        0,
                                    ));
                                    has_init = true;
                                } else {
                                    // No compound literal match
                                }
                            },
                            _ => {
                                // 浮点目标: eval_full 取位模式 (double x = 1 / 1.5;)
                                let is_float =
                                    matches!(v.ty, Type::Float | Type::Double | Type::LongDouble);
                                if let Some(bytes) =
                                    global::global_init_bytes(init, sz, is_float, &v.ty)
                                {
                                    self.extra_sections[sec_idx].data.extend_from_slice(&bytes);
                                    has_init = true;
                                }
                            },
                        }
                    }
                    if !has_init {
                        for _ in 0..sz {
                            self.extra_sections[sec_idx].data.push(0);
                        }
                    }
                    self.globals_raw.insert(
                        v.name.name.clone(),
                        GlobalVar {
                            offset,
                            size: sz,
                            ty: v.ty.clone(),
                            init: None,
                            section: (sec_idx + 7) as u16,
                            is_tls: false,
                            in_data_section: false,
                        },
                    );
                    continue;
                }
                let initialized = v.init.is_some();
                if initialized {
                    self.data_align = self.data_align.max(align as u64);
                } else {
                    self.bss_align = self.bss_align.max(align as u64);
                }
                let offset = if initialized {
                    let off = (self.data_section.len() + align - 1) & !(align - 1);
                    while self.data_section.len() < off {
                        self.data_section.push(0);
                    }
                    self.data_section.len() as u64
                } else {
                    self.bss_size = self.bss_size.div_ceil(align as u64) * align as u64;
                    self.bss_size
                };
                // 静态地址常量优先: &x / (T*)&x / &((arr+i)->f) / arr + n
                // 统一由 const_eval 折叠为 (符号, 字节偏移) 重定位,
                // 覆盖旧 AddrOf/Cast 分支无法表达的形式
                let static_addr_handled = if let Some(init) = &v.init {
                    if let Some((base_name, byte_off)) = Self::static_addr_parts(init) {
                        let ref_off = self.data_section.len() as u64;
                        let width = if sz <= 4 { 4 } else { 8 };
                        self.data_section
                            .resize(ref_off as usize + sz.max(width), 0);
                        let rel_type = if width == 4 {
                            crate::codegen::shared::elf::R_X86_64_32
                        } else {
                            crate::codegen::shared::elf::R_X86_64_64
                        };
                        self.pending_data64_rels
                            .push((ref_off, base_name, None, rel_type, byte_off));
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                let has_init = if static_addr_handled {
                    true
                } else if let Some(init) = &v.init {
                    match init {
                        Expr::String { data, .. } => {
                            // 指针类型 (char *s = "hello"): 字符串进 rodata 池,
                            // data 节指针槽通过 .rodata section 符号 + R_X86_64_64
                            // 重定位指向字符串 (链接时写入 rodata 基址 + 偏移)。
                            if matches!(&v.ty, Type::Ptr(_)) {
                                let data = data.clone();
                                if !self.string_pool.contains_key(&data) {
                                    let o = self.rodata.len() as u64;
                                    self.rodata.extend_from_slice(&data);
                                    self.string_pool.insert(data.clone(), o);
                                }
                                let str_off = *self.string_pool.get(&data).unwrap();
                                let ref_off = self.data_section.len() as u64;
                                self.data_section.resize(ref_off as usize + 8, 0);
                                self.pending_data_rodata_rels.push((ref_off, str_off, None));
                            } else {
                                self.data_section.extend_from_slice(data);
                                for _ in data.len()..sz {
                                    self.data_section.push(0);
                                }
                            }
                            true
                        },
                        Expr::InitList { inits, .. } => {
                            let base = self.data_section.len() as u64;
                            if let Some(init_data) =
                                self.gen_global_init_list(&v.ty, inits, Some((base, None)), false)
                            {
                                self.data_section.extend_from_slice(&init_data);
                                for _ in init_data.len()..sz {
                                    self.data_section.push(0);
                                }
                                true
                            } else {
                                false
                            }
                        },
                        Expr::AddrOf { e, .. } => {
                            // P1-6: .data 节中的符号引用
                            // Handle &ident, &a[i], &a[i][j], &s.field, etc.
                            if let Some((base_name, byte_off)) =
                                global::eval_global_lvalue_offset(e, &self.globals_raw)
                            {
                                let ref_off = self.data_section.len() as u64;
                                let width = if sz <= 4 { 4 } else { 8 };
                                self.data_section
                                    .resize(ref_off as usize + sz.max(width), 0);
                                let rel_type = if width == 4 {
                                    crate::codegen::shared::elf::R_X86_64_32
                                } else {
                                    crate::codegen::shared::elf::R_X86_64_64
                                };
                                self.pending_data64_rels
                                    .push((ref_off, base_name, None, rel_type, byte_off));
                                true
                            } else if let Expr::Ident { name: ref_name, .. } = e.as_ref() {
                                // Fallback for function pointers and other symbols not in globals
                                let ref_off = self.data_section.len() as u64;
                                let width = if sz <= 4 { 4 } else { 8 };
                                self.data_section
                                    .resize(ref_off as usize + sz.max(width), 0);
                                let rel_type = if width == 4 {
                                    crate::codegen::shared::elf::R_X86_64_32
                                } else {
                                    crate::codegen::shared::elf::R_X86_64_64
                                };
                                self.pending_data64_rels.push((
                                    ref_off,
                                    ref_name.name.clone(),
                                    None,
                                    rel_type,
                                    0,
                                ));
                                true
                            } else if let Some((anon_name, _off)) =
                                global::materialize_compound_literal(self, e, false)
                            {
                                // &(struct A){42,43} — C99 合法的文件地址常量
                                let ref_off = self.data_section.len() as u64;
                                let width = if sz <= 4 { 4 } else { 8 };
                                self.data_section
                                    .resize(ref_off as usize + sz.max(width), 0);
                                let rel_type = if width == 4 {
                                    crate::codegen::shared::elf::R_X86_64_32
                                } else {
                                    crate::codegen::shared::elf::R_X86_64_64
                                };
                                self.pending_data64_rels
                                    .push((ref_off, anon_name, None, rel_type, 0));
                                true
                            } else {
                                false
                            }
                        },
                        _ => {
                            // 浮点目标: eval_full 取位模式 (double x = 1 / 1.5;)
                            let is_float =
                                matches!(v.ty, Type::Float | Type::Double | Type::LongDouble);
                            if let Some(bytes) =
                                global::global_init_bytes(init, sz, is_float, &v.ty)
                            {
                                self.data_section.extend_from_slice(&bytes);
                                true
                            } else {
                                false
                            }
                        },
                    }
                } else {
                    false
                };
                if !has_init {
                    if initialized {
                        self.data_section.resize(self.data_section.len() + sz, 0);
                    } else {
                        self.bss_size += sz as u64;
                    }
                }
                self.globals_raw.insert(
                    v.name.name.clone(),
                    GlobalVar {
                        offset,
                        size: sz,
                        ty: v.ty.clone(),
                        init: None,
                        section: if initialized { 3 } else { 4 },
                        is_tls: false,
                        in_data_section: false,
                    },
                );
            }
        }

        // 文件级 asm 先发射到 .text
        for decl in &prog.decls {
            if let Decl::GlobalAsm {
                asm_str,
                outputs,
                output_names,
                output_constraints,
                inputs,
                input_names,
                input_constraints,
                clobbers,
                ..
            } = decl
            {
                self.emit_global_asm(
                    asm_str,
                    outputs,
                    output_names,
                    output_constraints,
                    inputs,
                    input_names,
                    input_constraints,
                    clobbers,
                );
            }
        }

        // 静态局部变量分析 pass + 预注册到 data/bss
        self.static_local_map = crate::static_locals::analyze_static_locals(prog);
        self.pre_register_static_locals();
        self.collect_static_local_segments(prog);
        // 生成函数代码 (无 _start)
        self.emit_functions(prog);
        // 修补 data_section 里的 label 地址差值 (同 compile_exec 路径)
        self.patch_label_diff_rels();

        // 填充反汇编符号表 (全局变量 RIP-relative 引用)
        for (p, name) in &self.pending_data_rels {
            self.disasm_data.insert(*p, name.clone());
        }
        for (p, name) in &self.pending_sib_data_rels {
            self.disasm_data.insert(*p, name.clone());
        }

        // 后处理: 将自定义 section 函数的代码从 .text 提取到 extra_sections
        // 记录函数在自定义 section 内的偏移 (用于符号表)
        let mut custom_func_offsets: HashMap<String, (u16, u64)> = HashMap::new(); // func_name → (section_idx+5, symbol_offset)
        let mut custom_func_ranges: Vec<(u64, u64, usize, u64, u64)> = vec![]; // func_start, emitted_start, emitted_size, sec_idx, sec_off
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                if f.body.is_none() {
                    continue;
                }
                let effective_section = f.section.clone().or_else(|| {
                    if f.is_cold {
                        Some(".text.unlikely".to_string())
                    } else if f.is_hot {
                        Some(".text.hot".to_string())
                    } else {
                        None
                    }
                });
                if let Some(sec_name) = effective_section.as_ref() {
                    if sec_name.is_empty() {
                        continue;
                    }
                    if let Some(&func_start) = self.labels.get(&f.name.name) {
                        let func_size =
                            self.func_sizes
                                .get(&f.name.name)
                                .copied()
                                .unwrap_or_else(|| {
                                    self.compute_func_size_fallback(&f.name.name, func_start)
                                });
                        if func_size == 0 {
                            continue;
                        }
                        let emitted_start = self.function_emitted_start(func_start);
                        let emitted_size = func_size + (func_start - emitted_start);
                        let sec_idx = self.ensure_custom_section(sec_name, true, false);
                        let func_align = f.alignment.unwrap_or(16).max(1);
                        self.extra_sections[sec_idx].align =
                            self.extra_sections[sec_idx].align.max(func_align as u64);
                        let aligned = (self.extra_sections[sec_idx].data.len() + func_align - 1)
                            & !(func_align - 1);
                        self.pad_custom_section_to(sec_idx, aligned);
                        let sec_off = self.extra_sections[sec_idx].data.len() as u64;
                        // 从 .text 拷贝代码到自定义节，包含 patchable entry 前缀。
                        let code_slice = self.code
                            [emitted_start as usize..(emitted_start + emitted_size) as usize]
                            .to_vec();
                        self.extra_sections[sec_idx]
                            .data
                            .extend_from_slice(&code_slice);
                        // 暂时保留 .text 原代码；resolve() 修补本地跳转后会重新复制。
                        custom_func_ranges.push((
                            func_start,
                            emitted_start,
                            emitted_size as usize,
                            sec_idx as u64,
                            sec_off,
                        ));
                        custom_func_offsets.insert(
                            f.name.name.clone(),
                            ((sec_idx + 7) as u16, sec_off + (func_start - emitted_start)),
                        );
                    }
                }
            }
        }

        // -ffunction-sections：将普通函数拆到 .text.<name>，供链接期 --gc-sections 剔除
        if self.function_sections {
            let mut per_func: Vec<FuncDecl> = vec![];
            for decl in &prog.decls {
                if let Decl::Func(f) = decl {
                    if f.body.is_some() && f.section.is_none() && !f.is_cold {
                        per_func.push(f.clone());
                    }
                    if let Some(body) = &f.body {
                        Self::collect_nested_funcs(&body.items, &mut per_func);
                    }
                }
            }
            for f in &per_func {
                if custom_func_offsets.contains_key(&f.name.name) {
                    continue;
                }
                if f.section.is_some() || f.is_cold {
                    continue;
                }
                if let Some(&func_start) = self.labels.get(&f.name.name) {
                    let func_size =
                        self.func_sizes
                            .get(&f.name.name)
                            .copied()
                            .unwrap_or_else(|| {
                                self.compute_func_size_fallback(&f.name.name, func_start)
                            });
                    if func_size == 0 {
                        continue;
                    }
                    let emitted_start = self.function_emitted_start(func_start);
                    let emitted_size = func_size + (func_start - emitted_start);
                    let sec_name = format!(".text.{}", f.name.name);
                    let sec_idx = self.ensure_custom_section(&sec_name, true, false);
                    // attribute((used)): 标记 SHF_GNU_RETAIN 防止 gc-sections 移除
                    if f.is_used_attr {
                        self.extra_sections[sec_idx].flags |=
                            crate::codegen::shared::elf::SHF_GNU_RETAIN;
                    }
                    let func_align = f.alignment.unwrap_or(16).max(1);
                    self.extra_sections[sec_idx].align =
                        self.extra_sections[sec_idx].align.max(func_align as u64);
                    let aligned = (self.extra_sections[sec_idx].data.len() + func_align - 1)
                        & !(func_align - 1);
                    self.pad_custom_section_to(sec_idx, aligned);
                    let sec_off = self.extra_sections[sec_idx].data.len() as u64;
                    let code_slice = self.code
                        [emitted_start as usize..(emitted_start + emitted_size) as usize]
                        .to_vec();
                    self.extra_sections[sec_idx]
                        .data
                        .extend_from_slice(&code_slice);
                    custom_func_ranges.push((
                        func_start,
                        emitted_start,
                        emitted_size as usize,
                        sec_idx as u64,
                        sec_off,
                    ));
                    custom_func_offsets.insert(
                        f.name.name.clone(),
                        ((sec_idx + 7) as u16, sec_off + (func_start - emitted_start)),
                    );
                }
            }
        }

        // 处理字符串引用 → 确保所有字符串在 rodata 中
        let string_data_list: Vec<Vec<u8>> =
            self.string_refs.iter().map(|(_, d)| d.clone()).collect();
        for data in &string_data_list {
            if !self.string_pool.contains_key(data) {
                let off = self.rodata.len() as u64;
                self.rodata.extend_from_slice(data);
                self.string_pool.insert(data.clone(), off);
            }
        }

        let move_ranges: Vec<(u64, u64, usize, u64)> = custom_func_ranges
            .iter()
            .map(|(fs, s, e, i, _)| (*fs, *s, *e, *i))
            .chain(self.pushsection_code_ranges.iter().copied())
            .collect();

        // IFUNC / CPUID 合成体：必须晚于上面那个 `move_ranges` 快照（自定义节搬迁只
        // 覆盖用户函数区间，合成代码不能被搬走），又必须早于 `patch_local_jumps`
        // （resolver 里的 `.L` 前向跳转要和其他函数一起回填，未定义即 E1102）与
        // `collect_rel_symbols`（resolver / IFUNC / `__dcc_cpu_model` 要进 symtab）。
        self.emit_ifunc_synthesis(prog);

        // 所有标签已发射完成后, 先修补本地跳转, 并把最终仍未解析
        // 的 asm 符号跳转转换为 pending code relocation。符号表收集
        // 依赖 pending_code_rels 中的外部符号, 所以必须先于 Phase 2。
        self.patch_local_jumps(&move_ranges);

        // C++ 零成本异常: LSDA 必须在符号表定稿之前落位 —— FDE 的 LSDA 字段要
        // 引用 `.gcc_except_table` 节符号, 而局部符号只能排在全局符号之前。
        self.assemble_eh_lsda();
        // Phase 2: 收集所有符号 (STN_UNDEF → LOCAL → GLOBAL)。
        // 自 monolith 恢复的完整 ET_REL symtab 构建 (elf/rel_symbols.rs)：
        // 含 asm 函数符号、alias/target_clones 变体、嵌套函数、__mcount_loc、
        // pushsection 标签符号、section 符号与 implicit UND。迁移期间曾被
        // 降级为复用 exec 路径的 collect_exec_symbols (仅剩死代码变量循环)。
        self.collect_rel_symbols(prog, &custom_func_offsets);
        // Phase 3 (自 monolith 恢复): 符号表定稿后发射 ET_REL 重定位、
        // init/fini_array 节与自定义节重定位搬迁。
        self.emit_rel_relocations(prog, &custom_func_ranges);
        // C++ 零成本异常: 类型表重定位按符号名查索引, 因此必须在符号表定稿后。
        self.emit_eh_relocations();
    }

    /// 静态地址常量折叠: &x / (T*)&x / &((arr + i)->f) / arr + n 等
    /// 统一走 const_eval 的 ConstValue::Addr (符号 + 字节偏移)。
    pub(crate) fn static_addr_parts(init: &Expr) -> Option<(String, i64)> {
        match const_eval::eval_full(init, false) {
            Ok(const_eval::ConstValue::Addr { name, offset }) => Some((name, offset as i64)),
            _ => None,
        }
    }

    pub(crate) fn compute_func_size_fallback(&self, _name: &str, func_start: u64) -> u64 {
        // 收集所有函数起始位置，找到当前函数之后的下一个函数
        let mut all_starts: Vec<u64> = self.labels.values().copied().collect();
        all_starts.sort();
        // 找到 func_start 之后的下一个起始位置
        let next_start = all_starts
            .iter()
            .find(|&&s| s > func_start)
            .copied()
            .unwrap_or(self.code.len() as u64);
        next_start.saturating_sub(func_start)
    }

    fn function_emitted_start(&self, func_start: u64) -> u64 {
        let prefix_len = self
            .patchable_function_entry
            .map(|(n, m)| m.min(n) as u64)
            .unwrap_or(0);
        func_start.saturating_sub(prefix_len)
    }

    pub(crate) fn pad_custom_section_to(&mut self, sec_idx: usize, len: usize) {
        let section = &mut self.extra_sections[sec_idx];
        let fill = if section.flags & crate::codegen::shared::elf::SHF_EXECINSTR != 0 {
            0x90
        } else {
            0
        };
        section.data.resize(len, fill);
    }

    fn final_text_loc(pos: u64, move_ranges: &[(u64, u64, usize, u64)]) -> (u16, u64) {
        move_ranges
            .iter()
            .find(|(start, size, _, _)| pos >= *start && pos < *start + *size)
            .map(|(start, _size, sec_idx, sec_off)| {
                ((*sec_idx + 7) as u16, *sec_off + (pos - *start))
            })
            .unwrap_or((1u16, pos))
    }

    /// 后端内部不变量破坏的统一上报通道。
    ///
    /// 这类诊断与用户代码无关 (不是前端错误), 但绝不能再往下降级成静默产物。
    /// 走既有的 `Diagnostic` / `Severity::Error` 通道 (与 E1100 同一条路径),
    /// 由 driver 统一以非零退出码终止编译。
    pub(in crate::codegen::x86_64) fn report_internal_bug(
        &mut self,
        code: &'static str,
        msg: String,
        hint: &str,
    ) {
        self.diagnostics.push(Diagnostic {
            sev: crate::diagnostics::Severity::Error,
            code,
            msg,
            span: crate::diagnostics::Span::point("<codegen>", 0, 0),
            hint: Some(hint.to_string()),
            source_line: None,
        });
    }

    fn patch_local_jumps(&mut self, move_ranges: &[(u64, u64, usize, u64)]) {
        let pending = std::mem::take(&mut self.pending);
        for p in pending {
            let Some(&dst) = self.labels.get(&p.target) else {
                // 缺陷 D: 只能降级「合法外部符号」。编译器自己造的 `.L` 标号不进
                // symtab, 把它的跳转当成一条 PC32 重定位发出去, 链接期只能解析成
                // 空符号 / 未定义全局 —— 静默误编译。
                if Self::is_internal_label(&p.target) {
                    self.report_internal_bug(
                        "E1102",
                        format!(
                            "jump at .text+{:#x} targets internal label '{}' which is never defined \
                             in this TU",
                            p.at, p.target
                        ),
                        "do not fall back to an external relocation for a compiler-generated .L label",
                    );
                    continue;
                }
                if self.relocatable && !p.is_push_imm {
                    let rel_off = p.disp_offset;
                    self.pending_code_rels.push(PendingCodeRel {
                        offset: rel_off,
                        symbol: p.target,
                        rel_type: crate::codegen::shared::elf::R_X86_64_PC32,
                        addend: -4,
                    });
                    continue;
                }
                self.pending.push(p);
                continue;
            };
            let off = p.disp_offset as usize;
            if p.is_push_imm {
                // push $label: imm32 = absolute address (lower 32 bits)
                self.code[off..off + 4].copy_from_slice(&(dst as i32).to_le_bytes());
            } else {
                let (src_sec, src_at) = Self::final_text_loc(p.at, move_ranges);
                let src_next = src_at + (p.next_ip - p.at);
                let (dst_sec, dst_pos) = Self::final_text_loc(dst, move_ranges);
                let rel_off = p.disp_offset as usize;
                if src_sec == dst_sec {
                    let rel = (dst_pos as i64) - (src_next as i64);
                    self.code[rel_off..rel_off + 4].copy_from_slice(&(rel as i32).to_le_bytes());
                } else if self.relocatable {
                    self.pending_code_rels.push(PendingCodeRel {
                        offset: p.disp_offset,
                        symbol: p.target,
                        rel_type: crate::codegen::shared::elf::R_X86_64_PC32,
                        addend: -4,
                    });
                } else {
                    let rel = (dst as i64) - (p.next_ip as i64);
                    self.code[rel_off..rel_off + 4].copy_from_slice(&(rel as i32).to_le_bytes());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Abi, Arch, Os, TargetTriple};
    use crate::diagnostics::Severity;

    fn x86() -> Codegen {
        Codegen::new(Target::resolve(&TargetTriple {
            arch: Arch::X86_64,
            vendor: "unknown".into(),
            os: Os::Linux,
            abi: Abi::Gnu,
        }))
    }

    fn jump(target: &str) -> PendingJump {
        PendingJump {
            at: 0,
            target: target.to_string(),
            kind: JumpKind::Jmp,
            disp_offset: 1,
            next_ip: 5,
            is_push_imm: false,
        }
    }

    fn err_codes(cg: &Codegen) -> Vec<&'static str> {
        cg.diagnostics
            .iter()
            .filter(|d| d.sev == Severity::Error)
            .map(|d| d.code)
            .collect()
    }

    /// 缺陷 D: `patch_local_jumps` 只能把「合法外部符号」降级成一条 PC32 重定位。
    /// 编译器自己造的 `.L` 标号不进 symtab, 降级后链接期只能解析成空符号,
    /// 而 `jmp_rel32` 留下的 `0,0,0,0` 占位若不回填语义等于「跳下一条」。
    #[test]
    fn undefined_internal_label_jump_is_not_downgraded_to_a_relocation() {
        let mut cg = x86();
        cg.relocatable = true;
        cg.code = vec![0xE9, 0, 0, 0, 0];
        cg.pending.push(jump(".Lnope"));
        cg.patch_local_jumps(&[]);
        assert!(
            cg.pending_code_rels.is_empty(),
            "内部标号不得变成外部重定位"
        );
        assert_eq!(err_codes(&cg), vec!["E1102"]);
        assert!(cg.diagnostics[0].msg.contains(".Lnope"));
    }

    /// 反向守卫: 外部符号在 `-c` 下仍必须降级成重定位, 不得误报。
    /// 把这一条误报成后端 bug 等于封死所有 `-c` 外部跳转。
    #[test]
    fn external_jump_still_downgrades_to_pc32_relocation() {
        let mut cg = x86();
        cg.relocatable = true;
        cg.code = vec![0xE9, 0, 0, 0, 0];
        cg.pending.push(jump("printf"));
        cg.patch_local_jumps(&[]);
        assert_eq!(cg.pending_code_rels.len(), 1);
        assert_eq!(cg.pending_code_rels[0].symbol, "printf");
        assert!(cg.diagnostics.is_empty(), "unexpected: {:?}", err_codes(&cg));
    }

    /// 已定义的内部标号仍须就地回填 (本改动不得改变正常路径的任何字节)。
    #[test]
    fn defined_internal_label_is_patched_in_place() {
        let mut cg = x86();
        cg.relocatable = true;
        cg.code = vec![0xE9, 0, 0, 0, 0, 0x90, 0x90];
        cg.labels.insert(".L0".to_string(), 6);
        cg.pending.push(jump(".L0"));
        cg.patch_local_jumps(&[]);
        assert!(cg.pending_code_rels.is_empty());
        assert!(cg.diagnostics.is_empty(), "unexpected: {:?}", err_codes(&cg));
        let disp = i32::from_le_bytes(cg.code[1..5].try_into().unwrap());
        assert_eq!(disp, 6 - 5, "disp32 必须等于 dst - next_ip");
    }
}
