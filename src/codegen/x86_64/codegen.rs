// src/codegen/x86_64/codegen.rs - x86_64 Codegen 结构体定义
// Phase 1b Migration: Extract struct from monolithic mod.rs

use crate::ast::{Designator, Expr, Type};
use crate::codegen::elf::{ComdatGroup, ElfRela, ElfSection, ExtraRelaSection, ElfSym};
use crate::codegen::x86_64::{CustomSecRel, DeferredPushsectionFixup, FuncAsmLayout, GlobalVar, PendingCodeRel, PendingJump};
use crate::diagnostics::Diagnostic;
use crate::preproc::LineSpan;
use crate::static_locals::StaticLocalMap;
use crate::target::Target;
use std::collections::{HashMap, HashSet};

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
    current_asm_line: usize,
    current_asm_src_line: u32,
    pub diagnostics: Vec<Diagnostic>,
    /// 预处理输出行号 → 源文件位置的映射 (诊断定位用)
    pub line_map: Vec<LineSpan>,
    pending: Vec<PendingJump>,
    next_label: u32,
    var_map: HashMap<String, (i64, Type)>,
    loop_continue: Option<String>,
    loop_break: Option<String>,
    /// break 跳转目标栈：(标签，是否在 switch 内需先弹出 switch 值).
    /// switch 压入 (l_break, true), 循环压入 (le, false); break 语句
    /// 总是跳到栈顶目标 (最近封闭的 switch / 循环).
    break_targets: Vec<(String, bool)>,
    // .rodata: 字符串池 + 待修补引用
    pub rodata: Vec<u8>,
    string_pool: HashMap<Vec<u8>, u64>, // data -> offset in rodata
    string_refs: Vec<(u64, Vec<u8>)>,   // (code_pos, data)
    float_refs: Vec<(u64, u64)>,        // (code_pos, rodata_offset) - SSE 浮点字面量引用
    x87_float_refs: Vec<(u64, u64)>,    // (code_pos, rodata_offset) - x87 fld RIP-relative 引用
    /// -fbounds-check ubsan 诊断数据：LEA rdi, [rip+disp32] 的 code_pos 与
    /// OutOfBoundsData 偏移。ET_EXEC: 偏移在 rodata, resolve() 修补 disp32;
    /// ET_REL: 偏移在 .data, compile_rel 生成 PC32 重定位 (.data 节符号)。
    ubsan_lea_refs: Vec<(u64, u64)>,
    /// -fbounds-check ubsan 诊断数据：OutofBoundsData 内绝对指针槽
    /// (data_off, field_off, target_rodata_off), 目标均为 rodata 内
    /// TypeDescriptor / 文件名字符串。ET_EXEC: resolve() 补绝对地址;
    /// ET_REL: 生成 R_X86_64_64 重定位 (.rodata 节符号)。
    ubsan_ptr_refs: Vec<(u64, u64, u64)>,
    // 全局变量 (通过 lookup_global_info / lookup_global_by_key 受控访问)
    globals_raw: HashMap<String, GlobalVar>,
    // CompoundLit 去重缓存：同类型同内容的 CompoundLit 只物化一次
    // key = format!("{:?}", Expr) (类型+inits 序列化),
    // value = (anon_name, rodata_offset)
    compound_lit_cache: HashMap<String, (String, u64)>,
    frame_size: i64,
    expr_depth: u32,     // 当前表达式嵌套深度 (用于帧临时槽分配)
    max_expr_depth: u32, // 最大表达式嵌套深度
    temp_slots: i32,     // 帧临时槽数量 (用于变量偏移计算)
    /// 已注册固定栈对象占用的 8 字节槽数。
    /// 只记录真实布局用量，不再为大数组创建 __pad_ 占位项。
    var_map_total_slots: i64,
    vec_temp_off: i64,  // 向量临时槽区基址 (r13 相对)
    vla_offsets: HashMap<String, i64>,
    vla_total_size: i64,
    /// 局部 variable-sized struct (含 VLA 字段) 的声明时绑定总大小槽:
    /// 类型名 → 槽偏移。类型声明语句执行时求值一次 (C11 6.2.4p7:
    /// VM 类型在声明时绑定维度，后续 sizeof 读槽不重算，20041218-2:
    /// 声明后 n++ 不影响 sizeof(struct s))。循环内重复声明同名类型
    /// 每次迭代重写槽 (重新绑定)。
    vm_struct_size_slots: HashMap<String, i64>,
    // 当前函数体含 VLA 声明：goto/break/continue 跳转前需恢复动态栈基，
    // 否则循环内重复进入 VLA 作用域会累积泄漏直至栈耗尽 (vla-dealloc-1)
    cur_func_has_vla: bool,
    bf_rmw_depth: u32, // 位域 read-modify-write 嵌套深度 (动态帧槽分配)
    /// 向量 binop 操作数物化槽号。嵌套向量 binop 时每层结果槽 R 的
    /// 操作数物化到 R+1 (XMM 快速路径 R+1/R+2), 避免内层子表达式
    /// 结果覆盖外层已写入的结果槽 (pr23135: 链式 k0+k1+k3+...
    /// 低层元素被内层重物化覆盖)。非 binop 上下文默认槽 1。
    vector_op_slot: usize,
    // 可重定位模式 (.o)
    pub relocatable: bool,
    pub function_sections: bool, // -ffunction-sections：每函数独立 .text.<name> 节
    patchable_function_entry: Option<(usize, usize)>,
    pub symbols: Vec<ElfSym>,
    pub sym_index: HashMap<String, u32>,
    pub relocations: Vec<ElfRela>,
    pending_func_rels: Vec<(u64, String)>, // (code_offset, func_name) - 延迟到符号表构建后解析
    pending_code_rels: Vec<PendingCodeRel>, // asm 数据伪指令/外部跳转：通用 code relocation
    pending_func_addr_rels: Vec<(u64, String, bool)>, // (code_offset, func_name, is_lea: true=PC32, false=GOTPCREL)
    defined_funcs: HashSet<String>,        // 本 TU 中有函数体的函数名
    error_attr_fns: HashSet<String>,       // __attribute__((error)) 函数名 (调用时发射 ud2)
    pending_data_rels: Vec<(u64, String)>, // (disp32_pos, var_name) - 全局变量重定位
    pending_sib_data_rels: Vec<(u64, String)>, // SIB base=101 (disp32 绝对寻址) - 需要 R_X86_64_32S
    pub data_section: Vec<u8>,             // .data 节 (已初始化全局变量)
    pub bss_size: u64,                     // .bss 大小
    pub tdata_section: Vec<u8>,            // .tdata (TLS 初始化)
    pub tbss_size: u64,                    // .tbss (TLS 未初始化)
    pub tdata_align: u64,
    pub tbss_align: u64,
    pub exec_text_size: u64,              // ET_EXEC 模式下 .text 大小
    pending_tls_rels: Vec<(u64, String)>, // TLS fs 相对引用
    pub text_align: u64,
    pub data_align: u64,
    pub bss_align: u64,
    pub extra_sections: Vec<ElfSection>, // 自定义节 (section 属性)
    custom_sec_indices: HashMap<String, usize>, // section name → index in extra_sections
    pub func_sizes: HashMap<String, u64>,     // 函数名 → 代码大小 (用于符号 size)
    variadic_funcs: HashMap<String, bool>,    // 函数名 → 是否可变参数
    old_style_funcs: std::collections::HashSet<String>, // K&R 风格定义函数名
    variadic_named_params: i32,               // 当前可变参数函数的命名参数数量
    variadic_named_gp: i32,
    variadic_named_fp: i32,
    variadic_overflow_offset: i64,
    variadic_reg_save_offset: i64,
    current_sret_offset: Option<i64>,
    sret_depth: i32,
    // 嵌套函数静态链
    nested_funcs: HashMap<String, Vec<(String, i64)>>, // 嵌套函数名 → upvars (name, parent_offset)
    // 嵌套函数重命名：(父函数名，原名) → 唯一名 (避免同名嵌套函数符号冲突)
    nested_func_renames: HashMap<String, HashMap<String, String>>,
    // 唯一名 → 定义父函数名 (静态链上溯解析兄弟/祖先嵌套函数引用)
    nested_func_parent: HashMap<String, String>,
    // 唯一名 → 该帧静态链槽偏移 (多层静态链上溯需要逐帧偏移)
    nested_tramp_offsets: HashMap<String, i64>,
    // 唯一名 → 定义父帧中 trampoline 槽偏移 (后代引用经静态链定位)
    nested_chain_offsets: HashMap<String, i64>,
    nested_func_rename_counter: u32,
    current_func_name: String, // 当前正在编译的函数名
    current_func_ret_ty: Type, // 当前函数的返回类型
    current_no_instrument: bool, // 当前函数标了 __attribute__((no_instrument_function))
    /// 静态局部变量分析 pass 的结果 (由 compile() 预计算，后端消费)
    static_local_map: StaticLocalMap,
    real_global_names: std::collections::HashSet<String>, // 真实全局变量名 (不含 static local), 用于避免 static local 覆盖真实全局
    /// 函数形参类型表（SysV 实参按形参类型分类）
    func_param_types: HashMap<String, Vec<Type>>,
    current_func_is_nested: bool, // 当前编译的函数是否是嵌套函数
    local_labels: std::collections::HashSet<String>, // 当前函数体内定义的标签名集合
    /// 当前函数标了 __attribute__((returns_nonnull)): 返回时需检查 rax 非空
    current_returns_nonnull: bool,
    /// 当前函数标了 __attribute__((noreturn)) / _Noreturn:
    /// 函数末尾插入 trap 指令 (ud2), 防止执行流落入后续代码
    current_is_noreturn: bool,
    static_chain_offset: i64,            // 静态链在当前帧中的偏移
    upvar_offsets: HashMap<String, i64>, // upvar 名称 → 在起源帧中的偏移
    // upvar 名称 → 需要走多少层静态链到达起源帧 (1=父，2=祖父，...)
    upvar_chain_depth: HashMap<String, u32>,
    // upvar 名称 → 中间嵌套帧的 static_chain_offset 列表
    // 对深度 1 为空; 对深度 2 包含 [父帧的 static_chain_offset]
    upvar_intermediate_offsets: HashMap<String, Vec<i64>>,
    // &&label 地址引用：(disp32_pos, label_name)
    label_addr_refs: Vec<(u64, String)>,
    // register asm 变量绑定：变量名 → 寄存器编号
    reg_bindings: HashMap<String, u8>,
    // file-scope register asm 变量绑定：每个函数入口恢复
    global_reg_bindings: HashMap<String, u8>,
    // data 节 64 位符号引用重定位：(data_section_offset, symbol_name)
    /// (data_off, symbol, custom_sec, rel_type, addend) — addend 用于 &arr[i] 等
    pending_data64_rels: Vec<(u64, String, Option<usize>, u32, i64)>,
    // rodata 节内部重定位：rodata 里的 CompoundLit 结构体指针字段
    // 引用另一个 rodata 里的匿名 CompoundLit。(offset_in_rodata, symbol_name, rel_type, addend)
    pending_rodata_rels: Vec<(u64, String, u32, i64)>,
    // data 节内 label 地址差值：表达式 `&&A - &&B` 在静态初始化式中
    // 出现时，编译器尚未知 label 偏移，先写占位并在函数编译完后
    // 用 self.labels 里的已知偏移回填。 (data_off, label_A, label_B, width_bytes)
    pending_label_diff_rels: Vec<(u64, String, String, usize)>,
    // data 节最终重定位条目
    pub data_relocations: Vec<ElfRela>,
    // cleanup 属性变量：(变量名，清理函数名)
    cleanup_vars: Vec<(String, String)>,
    // 额外 .rela 节 (用于 .init_array/.fini_array 等)
    pub extra_rela_sections: Vec<ExtraRelaSection>,
    // COMDAT group 定义
    pub comdat_groups: Vec<ComdatGroup>,
    // .debug_info relocations: (offset_in_section, symbol_name)
    pub pending_debug_info_relocs: Vec<(u64, String)>,
    // .pushsection 非文本节数据收集
    pushsection_stack: Vec<usize>,
    /// .section/.previous 节切换栈 (独立于 pushsection_stack)
    section_stack: Vec<usize>,
    /// 标记当前节上下文是否由 .section 创建 (用于 .previous 正确区分 .pushsection)
    section_active: bool,
    pending_custom_sec_rels: Vec<CustomSecRel>,
    /// 可执行 pushsection (.fixup / .altinstr_replacement) 栈：(sec_idx, self.code 起始偏移)
    pushsection_exec_stack: Vec<(usize, u64)>,
    /// 可执行 pushsection 代码区间：(text 起始偏移，size, extra_sections 索引，节内偏移)
    pushsection_code_ranges: Vec<(u64, u64, usize, u64)>,
    /// 延迟解析：pushsection 数据中标签差值表达式 (前向引用)
    deferred_pushsection_fixups: Vec<DeferredPushsectionFixup>,
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
    asm_directives: Vec<String>,
    asm_directives_at: Vec<(u64, String)>,
    disasm_calls: HashMap<u64, String>,
    disasm_data: HashMap<u64, String>,
    global_asm_exports: HashSet<String>,
    asm_func_symbols: HashSet<String>,
    asm_last_text_label: Option<(String, u64)>,
    func_asm_layout: HashMap<String, FuncAsmLayout>,
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
    /// rodata 中被字符串初始化指针占位的变量名列表 (resolve 阶段修补绝对地址)
    rodata_string_init_globals: Vec<String>,
    /// rodata 中字符串初始化指针槽的偏移列表 (compile_exec/ET_EXEC 路径的
    /// InitList 数组/结构体元素; resolve 阶段修补绝对地址)
    rodata_string_slots: Vec<u64>,
    /// data 节引用 rodata 字符串的重定位：(data_off, string_rodata_off, custom_sec)
    pending_data_rodata_rels: Vec<(u64, u64, Option<usize>)>,
    /// -mrecord-mcount: call site addresses for __mcount_loc section
    mcount_loc_entries: Vec<u64>,
    /// GNU asm label 映射：C 名 → asm("label") 覆盖名
    /// 用于将 ELF 符号名从 C 标识符重映射为用户指定的 asm 标签
    asm_label_map: HashMap<String, String>,
    /// TLS 变量访问模型映射：变量名 → tls_model 字符串
    /// 支持 "local-exec", "initial-exec", "local-dynamic", "global-dynamic"
    tls_var_models: HashMap<String, String>,
    /// Function declarations for __alignof__(func) lookup
    func_alignments: HashMap<String, usize>,
    // ── 寄存器分配器：caller/callee-saved 显式建模 ──
    /// 被调用者保存寄存器池：rbx(3), r12(12), r14(14), r15(15).
    /// 这些寄存器在函数内可用作额外暂存，但必须在序言保存/尾声恢复.
    /// 使用 x86 硬件编码 (非 parse_reg_name 编号).
    callee_saved_pool: [u8; 4],
    /// 当前已分配的被调用者保存寄存器 (位掩码，bit i = callee_saved_pool[i])
    reg_allocated: u8,
    /// 当前函数是否使用了任何被调用者保存寄存器 (用于 CFI / 序言优化)
    callee_saved_used: u8,
    /// 序言/尾声中 callee-saved 保存/恢复指令的补丁点:
    /// (code 偏移，位掩码 bit). 函数体生成完毕后，若该寄存器未被使用，
    /// 将对应的 4 字节 mov 改写为 4×0x90 (nop), 避免叶子函数的冗余保存.
    callee_save_patch_sites: Vec<(u64, u8)>,
    // ── 嵌套函数 trampoline ──
    /// 嵌套函数 trampoline 栈槽：函数名 → 帧偏移.
    /// 当嵌套函数地址被取时，在栈上生成 trampoline 代码，
    /// 设置 r10 = 父帧指针后跳转到实际函数.
    trampoline_slots: HashMap<String, i64>,
    /// trampoline 代码大小 (每函数 32 字节)
    trampoline_total_size: i32,
    /// 完整 struct/union 定义映射：名称 → 完整类型 (含字段列表).
    /// 用于解析前向引用导致的不完整类型 (如 `struct S *p` 在 S 定义前声明，
    /// 后续 `p->field` 需要完整字段列表来计算偏移).
    complete_struct_types: HashMap<String, Type>,
}
