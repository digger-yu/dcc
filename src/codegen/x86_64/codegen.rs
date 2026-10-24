// src/codegen/x86_64/codegen.rs - x86_64 Codegen 结构体定义
// Phase 1b Migration: Extract struct from monolithic mod.rs

use crate::ast::{AddrSpace, Decl, Designator, Expr, Field, Program, Type};
use crate::codegen::shared::elf;
use crate::codegen::shared::elf::{ComdatGroup, ElfRela, ElfSection, ElfSym, ExtraRelaSection};
use crate::codegen::x86_64::elf::attr_diag::AttrDiagFn;
use crate::codegen::x86_64::ifunc;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::mem::global;
use crate::codegen::x86_64::FuncDecl;
use crate::const_eval;
use crate::diagnostics::Diagnostic;
use crate::preproc::LineSpan;
use crate::static_locals::StaticLocalMap;
use crate::target::Target;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// 待外置的冷分支（方案 §3 的 P2）。发射器是单遍直写，所以下沉一条冷边只能「先记下，
/// 到函数尾再发」——但 `break`/`continue` 要靠发射现场的目标栈才能解对，所以必须连同
/// 三个栈一起快照，而不是到 flush 时再用当前值。
///
/// `join` = 冷块跑完要回到的汇合标签；`None` = 体 自己以 return 结束（此时不再发 jmp）。
pub(crate) struct ColdArm {
    pub entry: String,
    pub body: crate::ast::Stmt,
    pub join: Option<String>,
    pub loop_continue: Option<String>,
    pub loop_break: Option<String>,
    pub break_targets: Vec<(String, bool)>,
}

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
    /// 冷格外置队列（`DCC_COLD_OUTLINE=1` 才写入；默认空 ⇒ 产物逐字节不变）。
    /// `pub(crate)`：`ColdArm` 是 crate 内类型，挂在 `pub` 字段上会触发
    /// private_interfaces（向外暴露一个外部拿不到的类型）。
    pub(crate) cold_arms: Vec<ColdArm>,
    /// 本函数被指纹合并掉的外置块数（S6 的机制检查值，不进产物字节）。
    ///
    /// 为什么单独计数而不从「函数变小了」倒推：一个变小的函数可能同时吃了外置与合并，
    /// 也可能只是nop 填充重排；合并次数是唯一能回答「到底有多少对冷尾真的全等」的读数。
    pub(crate) cold_merged: u64,
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
    /// 本函数帧布局的**解算结果**（P12 Phase 2）。
    ///
    /// 搬迁前这里同时有 `frame_size` / `vec_temp_off` 两个裸字段，与各消费者现场
    /// 推算的算式构成「同一个帧坐标有多个理解」（实测 42 处 `-self.frame_size + 8`、
    /// 10 处 `48 + temp_slots*8`）。现在帧里**解算后不再变**的坐标只有本表这一个出处；
    /// 而 `sret_region_base` / `var_map_total_slots` / `sret_watermark` 仍是 Codegen 字段，
    /// 因为它们是**生成期会往下压的游标**，不是布局读数。
    ///
    /// 初值取 `unresolved()`：它等于「旧公式喂旧零镜像」的结果，所以下游即使在
    /// 解算之前读帧底坐标（串行/并行首函数、`naked` 早返）也与搬迁前逐字相同。
    pub frame_layout: crate::codegen::x86_64::abi::layout::LayoutMap,
    pub expr_depth: u32, // 当前表达式嵌套深度 (用于帧临时槽分配)
    /// 晚分配站点的**发射序**计数（P12 Phase 5.1.a 配套，见 `Codegen::next_late_site`）。
    /// 逐函数复位（与 `expr_depth` 同处）。
    pub late_site_seq: u32,
    /// §11.23 步 2：规划期为「发射期才会冒出来的帧内对象」预扣的槽数上界。
    /// 由 `plan_late_slot_budget()` 一次写下，逐函数复位。
    pub late_slot_budget: i64,
    /// 本函数出现过的最大向量字节数（向量临时槽带的尺寸输入，单源于
    /// `abi::layout::vec_band_reserve`）。与 `late_slot_budget` 同一趟规划写下，逐函数复位。
    pub max_vec_bytes: usize,
    /// `__builtin_shuffle` / `__builtin_shufflevector` 的**站点嵌套层号**（0 起）。
    /// 内层站点在外层实参物化期间跑，两边的拼接镜像/掩码/结果必须各用自己的
    /// 槽；层号乘上每站槽数从向量带尾部往下定位（实参表达式自己的 binop 嵌套从带
    /// 顶往下长，两个方向不相遇，除非带整体溢出——那是本后端已有的限度）。
    /// 与 `late_site_seq` 同口径逐函数复位。
    pub vec_perm_level: usize,
    /// 发射期**实际**新增的槽数（只统计 `frame_planned` 之后的新名字 `register_var`）。
    /// 它与 `late_slot_budget` 的差就是静态扫描的余量；超出即 `E1133`。
    pub late_slot_used: i64,
    /// 晚分配带的**字节游标**（§11.23 步 3）：带顶往下已吃了多少。它取代了
    /// 「晚分配挤在变量区水位里 + `min_used` 夹逼」那套现场让步，逐函数复位。
    pub late_var_cursor: i64,
    /// 本函数的帧规划是否已经走完（`plan_late_slot_budget()` 的最后一件事）。
    /// 只有在此之后的 `register_var` 新名字才算「晚分配」，才进 `late_slot_used`。
    pub frame_planned: bool,
    /// 最近一次「求值到基址」留下的**持有记录**：`Some(号)` 意思是「那个号里现在就是刚
    /// 求出来的值」（Phase 5.1 前置一·下）。
    ///
    /// 为什么它可以被信任：失效点只有一个 —— `Codegen::emit`，即「又发了一条指令出去」。
    /// 本轮已把函数体内所有绕开 `emit` 的直接追写收进 `emit`（nop 填充、`mov r13,r10`、
    /// 内联 asm 的 `push_code`），所以「发了新指令但记录仍声称有效」这个形状在结构上不成立。
    /// 写入点在 `gen_expr_value` 里、发生在它内部所有发射**之后**，顺序不能反。
    pub eval_holding: Option<crate::location::RegId>,
    /// 发射层窥孔的「上一条已发指令」（见 `instructions.rs::PeepLast`）。
    ///
    /// 只用来削**相邻两条即可证明等价**的 mov（重复 / 64 位往返 / 同槽转发），
    /// 不做活性分析。任何 `emit`（裸字节）、标签定义或函数边界都必须把它清空 ——
    /// 「上一条」一旦跨过控制流汇合点，结论就不再成立。
    pub(crate) peep_last: Option<crate::codegen::x86_64::instructions::PeepLast>,
    /// 「这条比较的结果只用来决定分支」的**令牌**（分支发射点发出，Binop 臂消费）。
    ///
    /// 是拿走而不是读取：`if (x + (a<b))` 里外层 Add 一进臂就把令牌拿走，内层 `a<b`
    /// 再也看不到它 —— 只有真正直接流向分支的那一次比较才被允许「只留 flags、
    /// 不材料化 0/1」。带返回值的语境（赋值、`!!x`、函数实参）不会发令牌。
    pub(crate) cmp_flags_token: bool,
    /// 「这条表达式的值根本没人要」（P13 阶段1 的 `ValueUse::Discarded`）。
    ///
    /// 今天赋值臂末尾总要把结果归约到左值类型（`reduce_assign_value_to_lvalue`），因为
    /// 那条归约同时是「赋值表达式的值」的语义（C11 6.5.16p3）。但归约后的寄存器只有在
    /// 值**真被人读**时才必需：内存左值的 store 本身就按左值宽度截断。语句级表达式
    /// （`i = i + 1;`、`for` 的 step、逗号运算数的左侧）上这一条恒等的
    /// `movslq %eax,%eax` 就是白发的（实测 luaV_execute 里 `movsxd rax,eax` 有 243 条）。
    ///
    /// 与 `cmp_flags_token` 同一条纪律：在 `gen_expr` 入口被**消费**，所以 `f(a = 1)` 里
    /// `a = 1` 的值要交给实参，看到的是 `false`。
    pub(crate) value_is_dead: bool,
    /// 本次 `binop` / `emit_cmp_imm` 调用**可以**把条件码交给 `last_cmp`（由 Binop 臂
    /// 紧贴调用点置位/复位，中间不发射任何指令，所以不会溢出到别的表达式）。
    pub(crate) cmp_flags_granted: bool,
    /// 「上一条指令就是 cmp/test，且没人动过 flags」的条件码。
    ///
    /// 生命周期与 `peep_last` 同一条纪律：任何一次 `emit`（含 push/pop、裸字节）与任何
    /// 标签定义都把它清空 —— 标签是控制流汇合点，从别处跳进来时 flags 是另一条边上的。
    /// 写入只发生在比较臂发完 cmp 之后（顺序不能反，否则自己把自己清掉）。
    pub(crate) last_cmp: Option<crate::codegen::x86_64::opcode::CondCode>,
    /// **块内**机器级等价的当前状态（P13 阶段6 地基，见 `blockopt`）。
    ///
    /// 与 `peep_last` 同一条纪律：`label()` / 裸字节 `emit()` / 函数边界都整体清空。
    /// 它只记录「本块内可证明的等价」，任何认不得的形态一律全杀。
    pub(crate) block_state: crate::codegen::x86_64::blockopt::BlockState,
    /// 本函数里 **volatile / 段限定** 局部对象的帧偏移（8 字节槽粒度）。
    ///
    /// `var_map` 只存 `Type`（限定符在 `QualType` 上），所以「这个槽能不能被转发」
    /// 这条事实必须在声明处单独记一笔。逐函数清空；块内优化对它只读。
    pub(crate) volatile_frames: HashSet<i64>,
    /// **本函数的控制流护栏**（方案 §4.7：S2 的清槽四条件与家绑定的护栏必须同源）。
    ///
    /// 与 `volatile_frames` 的分工：那张表是**偏移粒度**的屏障（哪些槽永不被事实穿过），
    /// 本字段是**函数粒度**的结论（帧槽事实能不能活过一次 `call`、区域家能不能开工）。
    /// 两者都必须逐函数重算；泄漏到下一个函数就是拿别人的控制流形状给本帧开绿灯。
    /// 算式在 `crate::frame_guard`，本模块不写第二份判据。
    pub(crate) frame_guard: crate::frame_guard::FrameGuard,
    /// 本函数有自标（`goto *p` / `asm goto`）或有内联 asm ⇒ 整个函数的 A2 放宽都关掉：
    /// 间接跳转的落点在运行时才知道，而 `%l0` 形式与 asm 文本里的 `jmp` 都是本模块
    /// 数不到的边；单遍发射无法证明「L 只有一个前驱」。
    pub(crate) a2_no_inherit: bool,
    /// 每个标签在 AST 里被多少条跳转指着（先验真源：`frame_guard::jump_edges`）。
    ///
    /// 多前驱取交（S2 A2 另一臂）拿它答「到 `label(L)` 为止，本 pass 是否已经把 L 的
    /// 前驱**看全**」：只数到已经发出的跳转，所以要与 AST 给的应发条数比。两者相等才
    /// 说明剩下的都是落空边；不等（少 = 有回边或死代码里的边、多 = 发射器拆了条）一律
    /// 退回整体清。
    pub(crate) a2_incoming: HashMap<String, u32>,
    /// 每个标签已经收了多少条前驱边（`jmp_rel32` 发射时累加）。
    pub(crate) a2_seen: HashMap<String, u32>,
    /// 某个标签的已知前驱出口事实**交集**。逐边在 `a2_contribute` 里取交，
    /// `label()` 只把它并进当前状态。
    pub(crate) a2_snap: HashMap<String, crate::codegen::x86_64::blockopt::BlockState>,
    /// **区内家状态**（方案 §3 S4 的 L2 家模型）。
    ///
    /// 逐函数重建：`RegionState` 里每一张表都是本帧坐标（槽位移）或本函数集合
    /// （heat/blacklisted），泄漏到下一个函数就是拿别人的槽号当自己的家——那是直接错值。
    pub(crate) region: crate::codegen::x86_64::region::RegionState,
    /// 一段 PC 区间上的位置（方案 §4.4 的 loclist 生产点）。
    ///
    /// 今天只有一种生产者：延伸池（caller-saved）家在 call 序列里被存进保存槽、回来时
    /// 再装载 —— 那段区间里真值在槽里而不在号里，而单 `DW_AT_location` 只能说一个位置
    /// （说号就是让调试器在函数调用期间读一个callee 己破坏的寄存器）。
    /// `(变量名, begin_pc, end_pc, 位置)`，按名交给 `collect_debug_var_facts`。
    pub debug_loc_windows: Vec<(String, u64, u64, crate::location::Location)>,
    /// 本函数里「地址被取走过的」局部/形参占的帧偏移（8 字节槽粒度，S2 A1 与 S4 的槽级屏障）。
    ///
    /// 与 `volatile_frames` 同一形态、不同含义：那张表上的槽「永不可被事实穿过」（volatile
    /// 的每一次访问都是可观察事件）；本表上的槽「不可活过一次 call、不可把存出推迟到区尾」
    /// （callee 拿着逃逸出去的指针可以写它，而那份写不在事实表的视野里）。
    /// 同一个偏移可能在两张表上，取并集就是对的。
    pub(crate) escaped_frames: HashSet<i64>,
    /// 尾部悬空的 legacy 前缀字节数（段前缀 0x64/0x65/… 与 0x66/0x67 那一族）。
    ///
    /// 为什么要记它：段前缀是以「单独一笔 `emit_insn`」发的（`emit_global_seg` 与
    /// `lvalue_seg_prefix` 那几处都是这个形状），字节流里因此会出现一段「前缀已落、
    /// 被它修饰的那条还没落」的间隙。任何在本条之前插入字节的变换插进这个间隙，前缀就
    /// 贴到了插入的那条上：实测就是 `percpu_offset.c` / `seg_object_body.c` 的 SIGILL ——
    /// 插入的存出吃了 `%gs:`，真正的访问丢了前缀，指令流从中间错位。
    ///
    /// 区内家（S4）已经改成零插入，不再需要绕这段间隙；本字段留下来是因为它是
    /// 「不得在发射入口插字节」那条约束的**证据**（用例：`region_home_is_gated_off_and_live_when_on`）。
    #[allow(dead_code)]
    pub(crate) dangling_prefix: usize,
    /// 本函数里「帧槽读写不得被等价事实穿过」的局部变量名 → 占用的槽数（P13 阶段6）。
    ///
    /// 与 `volatile_frames` 的分工：这张表在**声明处**建成，此时晚分配变量的偏移还
    /// 不存在；`register_var` 定下偏移时据它把具体偏移登记进 `volatile_frames`。名字
    /// 口径与 `collect_frame_var_decls` 同源，`for` 的 init、`({...})` 块、标号语句里
    /// 的 volatile 局部都不会漏。
    pub(crate) volatile_locals: HashMap<String, i64>,
    /// 本函数是否有「槽数大到不值得枚举」的 volatile / 段限定局部（P13 阶段6）。
    ///
    /// `volatile_frames` 是**枚举**出来的屏障：一个字节数组局部占多少槽就要插多少条。
    /// 大对象（实测 `volatile char big[0x100000000UL]` 的 VLA，`var_alloc_slots` 给出
    /// 5.4 亿槽）按槽枚举会把编译器拖死（实测 6 分钟、5.9 GB RSS，`perf_regression`
    /// 的 30 秒上限直接超时）。槽枚举不完时唯一安全的方向是**整体关掉本函数的槽事实**：
    /// 少削几条只是丢优化，漏登记一条 volatile 槽就是静默错值。
    pub(crate) volatile_slots_overflow: bool,
    /// **程序点级家覆盖**（Phase 5.1 前置二）：名字 → 「在这一段里它其实住在哪里」。
    ///
    /// 声明级的家（`homes`）只能回答「这个变量的家是哪个号」，而前置三需要的恰好是它的
    /// 反面：在一段代码里（一个 call 序列）那个号**会被毁掉**，值暂时只在内存里。没有
    /// 这一层，参数装载就会在「已存下、尚未取回」的窗口里读到被自己盖掉的号（形状与
    /// 成因见 `abi/call.rs` 与 `src/ssa/regalloc.rs::AUTO_EXT_POOL` 的注释）。
    ///
    /// 只有一个读入口能看到它：`home_for`。地址类消费点也走同一个入口，所以「先查覆盖
    /// 再查家表」的优先级在全后端只有一份。
    pub home_overrides: HashMap<String, crate::location::Location>,
    /// 延伸池家的保存槽（前置三的存/取两头都要的那个内存位置）。
    ///
    /// 在**规划期**建好（满足 H1：家在发射前定完），内容不随发射变化；每个条目就是一个
    /// 住在 caller-saved 号里的自动绑定变量：它的号、它的槽偏移与它的宽度。
    pub ext_home_saves: Vec<ExtHomeSave>,
    pub max_expr_depth: u32, // 最大表达式嵌套深度
    pub temp_slots: i32,     // 帧临时槽数量 (用于变量偏移计算)
    /// 已注册固定栈对象占用的 8 字节槽数。
    /// 只记录真实布局用量，不再为大数组创建 __pad_ 占位项。
    pub var_map_total_slots: i64,
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
    /// 阶段 6 批 4：本函数把**哪张**跳表的基址常驻 `%rbp`（None = 不常驻）。
    ///
    /// 存在理由：computed goto 解释器的分派尾部今天是三条 `lea 表基址,%r11` +
    /// `mov (%r11,%rcx,8),%rax` + `jmp *%rax`，而表是一个**静态局部**（地址是符号，
    /// 函数范围内不变）。基址进寄存器后塌成一条 `jmp *(%rbp,%rcx,8)`。
    /// 实测 gcc -O2 在同一个位置用的就是这条（它的基址号是 `%r12`）。
    ///
    /// 为什么是 `%rbp` 而不是池里的第五个号：`POOL=[rbx,r12,r14,r15]` 是分配器可发的
    /// 家号（luaV_execute 四个全被变量家占了），而 `%rbp` 本后端从不发号（帧基址是
    /// `%r13`），默认档（`-fomit-frame-pointer`）下也不参与任何寻址，是一个真正闲置的
    /// callee-saved 号。代价是三条互斥（见 `decide_disptab_base_reg`）与序言/尾声要
    /// 多存复一次，存槽用帧里已经预留但只有金丝雀会用的 `%r13-8`。
    pub disptab_base_reg_sym: Option<String>,
    /// 当前函数是 `__attribute__((naked))`：没有序言/尾声，也没有帧，函数体就是用户的汇编。
    ///
    /// 两个读取方靠它区分形态（P12 Phase 5.2a）：
    ///   * 内联 asm 的「保存/恢复守护」不得在 naked 函数里插 `push`/`pop` —— 栈是用户在管；
    ///   * 而且这条路径上 `reset_reg_tracker()` 根本没跑，`RegBank` 的两个位图还是上一个
    ///     函数的残值（那是旧代码就有的行为，本阶段不顺手改），据它算出的守护集合不可信。
    pub cur_func_is_naked: bool,
    pub bf_rmw_depth: u32, // 位域 read-modify-write 嵌套深度 (动态帧槽分配)
    /// 未配平的 `push`/`pop` 字节数 (push +8 / pop -8, 由 emit_insn 按
    /// 类型化 Stack 指令累计)。SysV ABI 要求 **call 指令处** rsp 16 对齐;
    /// 函数序章对齐后, 中途为"保存左值地址跨调用"而 push 的值会让 rsp
    /// 偏离 8, 调用点的 sub frame_bytes 必须补回同样偏移, 否则被调方把
    /// 第一个栈参按 16 对齐取 (va_arg 的 overflow 对齐、align>16 的
    /// MEMORY 聚合), 整体错位 (pr92904: `v.b = f1(4,...)` 的 __int128
    /// 高半读成栈上垃圾)。
    pub stack_shift: i64,
    /// 向量 binop 操作数物化槽号。嵌套向量 binop 时每层结果槽 R 的
    /// 操作数物化到 R+1 (XMM 快速路径 R+1/R+2), 避免内层子表达式
    /// 结果覆盖外层已写入的结果槽 (pr23135: 链式 k0+k1+k3+...
    /// 低层元素被内层重物化覆盖)。非 binop 上下文默认槽 1。
    pub vector_op_slot: usize,
    /// 本函数局部变量的最大过对齐值 (`__attribute__((aligned(N)))` 或 alignas,
    /// N>16)。0 表示无过对齐局部。仅用于栈帧动态对齐 (pr16660)。
    pub cur_func_over_align: usize,
    /// 过对齐帧里保存「对齐前帧顶 rsp」的 r13 相对槽偏移 (`-hdr`)。
    /// 仅 `cur_func_do_align` 为真时有效。
    pub cur_func_anchor_off: i64,
    /// 本函数是否真的在序言里把 %r13 动态对齐到 `cur_func_over_align`。
    /// 仅当「有过对齐局部 + 非变参 + 无栈传参」时为真。
    pub cur_func_do_align: bool,
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
    pub pending_data_rels_with_off: Vec<(u64, String, i32)>, // (disp32_pos, var_name, elem_off) - 带元素偏移的全局重定位(数组 static local 逐元素初始化)
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
    pub current_func_name: String, // 当前正在编译的函数名
    /// 预注册 static local 期间该变量的**属主函数名**：`&&label` 初始化式的标签
    /// 命名空间（见 `c_label_to_asm`）。只在 `register_static_global` 调用期间非空。
    pub static_local_label_owner: Option<String>,
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
    /// 变量名 → **家** + 声明类型。这张表就是「哪些变量住物理寄存器」的唯一答案
    /// （P12 Phase 4.1c 之前叫 `reg_bindings: HashMap<String,(u8,Type)>`；改成携带
    /// `Location` 是为了 Phase 5 能表达 `Spill` 而不必再造第二张表）。
    ///
    /// 类型必须与家**同表携带**：绑定路径的读写要按声明宽度截断/扩展，与栈槽路径同一
    /// 口径，否则同一个变量在两条通路下会给出不同的值（§7.16 实测）。
    pub homes: HashMap<String, (crate::location::Location, Type)>,
    // file-scope 绑定的家：每个函数入口恢复
    pub global_homes: HashMap<String, (crate::location::Location, Type)>,
    /// 本函数「后端自己挑的」家：名字 → 号。
    ///
    /// 只给**没走 SSA**的函数用（`VarDecl::home` 全空）。这类函数拿不到分配器的
    /// 结果，于是每个局部变量的每次读写都过栈 —— 实测 `luaV_execute` 的静态栈引用
    /// 是 gcc 的 10 倍，而 fib(32) 的耗时比恰好等于静态指令数比。这张表与
    /// `homes` 分开存，是因为 `bind_decl_home` 只在「本表命中」时才把声明绑上；
    /// 直接查 `homes` 会把同名全局绑定的号误当成局部变量的家。
    pub auto_homes: HashMap<String, crate::location::Location>,
    /// 全程序里「地址被取走」的对象根名；只有 `addr_escaped_done` 为真时才参与判定
    /// （区分「扫过且为空」与「还没扫」）。判据本身在 `ast_gvn::addr_escaped_names`。
    pub addr_escaped: std::collections::HashSet<String>,
    pub addr_escaped_done: bool,
    // data 节 64 位符号引用重定位：(data_section_offset, symbol_name)
    /// (data_off, symbol, custom_sec, rel_type, addend) — addend 用于 &arr[i] 等
    pub pending_data64_rels: Vec<(u64, String, Option<usize>, u32, i64)>,
    /// data 节里指向**本 TU 文本标号**（`&&label`）的 64 位槽：
    /// `(data_off, label_asm_name, addend)`。
    ///
    /// 为什么不走 `pending_data64_rels`：那些名字要进 symtab，而 `.Ll_*` 按 GAS/ELF
    /// 约定**不进** symtab（`referenced_reloc_symbols` 显式排除 `.L` 前缀）。文本标号
    /// 的正确形状是「`.text` **节符号** + 节内偏移 addend」——与
    /// `pending_data_rodata_rels` 对字符串字面量用的手法同源。ET_EXEC 下由
    /// `resolve()` 用 `self.labels` 就地回填；ET_REL 下由 `emit_rel_relocations`
    /// 发 `R_X86_64_64`（符号 = `.text`）。
    ///
    /// 存在理由：computed-goto 解释器（Lua `luaV_execute` 的 `disptab`）此前走
    /// 「static local + 运行时初始化」，每次函数调用都重发 ~120 条 `lea+mov` 把表填
    /// 一遍（fib(32) ~7M 次调用 ≈ 8.4 亿条指令，实测占 dcc/gcc 差距的 ~1/4）。
    pub pending_data_textlabel_rels: Vec<(u64, String, i64)>,
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
    /// Phase 6.2：逐函数采集的「局部 → 帧内位置」表（函数名 → 条目），只在 `-g` 下填。
    /// 它存在的理由与 `SymbolFacts` 同类：那是一张**函数级**事实表，而 DWARF 在
    /// 整个 TU 编译完之后才拼，当时单函数的 `homes`/`var_map` 早已被下一个函数复位。
    pub debug_var_facts: Vec<(String, Vec<DebugVarLoc>)>,
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
    /// 非执行 pushsection 内命名 label 在自定义节数据中的位置：
    /// label 名 → (extra_sections 索引, 节内偏移).
    ///
    /// 为什么需要独立一张表: `self.labels` 一律用 `self.code.len()` (text 坐标) 登记,
    /// 对可执行 pushsection 后续由 `pushsection_code_ranges` 反查搬迁映射; 但非执行
    /// pushsection (`.discard.call_table` / `.discard.retpoline_sites` 等) 内的 label
    /// 位置不在 `self.code` 而在 `extra_sections[sec_idx].data`, 无法反推. 这张表
    /// 在 `emit_pushsection_data` 的 label 分支显式记录 (sec_idx, data.len()), 供
    /// symtab 构建阶段把 label 编码为 STT_SECTION + offset 本地符号.
    pub pushsection_label_offsets: HashMap<String, (usize, u64)>,
    /// 延迟解析：pushsection 数据中标签差值表达式 (前向引用)
    pub deferred_pushsection_fixups: Vec<DeferredPushsectionFixup>,
    // -S 汇编文本输出
    pub emit_asm_text: bool,
    pub debug_level: u8,
    /// `-O0..-O3`。发射侧读它不是为了「要不要优化」，而是护栏：区内家（S4）与
    /// A2 标签汇合在 -O0 下必须整体关——那一档不跑活性分析，「本区内这个槽会不会
    /// 再被写」没有依据（方案 §3 S4 硬互斥清单里的 `-O0`，并要求补本字段）。
    pub opt_level: u8,
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
    /// P9 步骤 5: 内联汇编每条指令的 (起始 pc, 包装好的 Raw Insn) 日志。
    /// -S 渲染时按 pc 区间切片读这张表, 不再对这些字节跑 disasm 反汇编。
    /// 原生 C 代码发射的指令不进这张表 (步骤 6 另起 self.insns)。
    /// 当前只是"事后观察 self.code 增量"写入, 尚未有读取方 —— 步骤 6 起消费。
    pub(crate) asm_insn_log: Vec<(u64, Insn)>,
    /// P9 方案 C: 原生 C 代码每条发射指令的 (起始 pc, Insn) 记录.
    /// -S 渲染器按 pc 遍历这张表直接输出 Insn.gas_text, 不再走 disasm 反推.
    /// 与 asm_insn_log 分两张表的原因: asm_insn_log 只覆盖内联汇编块,
    /// 原生 C 路径走 emit_insn → 这里. 两张表在 build_asm_output_v2 合并.
    pub(crate) insn_records: Vec<(u64, Insn)>,
    /// P9 方案 C 临时统计: DCC_GAS_STATS=1 时记录每个发射点的调用频率.
    pub(crate) emit_insn_stats: std::collections::HashMap<String, usize>,
    /// P9 方案 C 临时统计: (has_gas, count).
    pub(crate) emit_insn_gas_hits: std::collections::HashMap<bool, usize>,
    /// 普通 C 语句的 DWARF 行号记录：(语句起始 PC, 源码行)。
    ///
    /// 为什么单开一张表：上面两张只覆盖内联 asm 块，它们的 PC 区间是「整块」且带
    /// discriminator 语义（`backend.rs` 拼表时按「同址时语句边界优先」的规则喂进去）。把
    /// C 语句塞进同一条通道会让那套规则失真。本表填上之前，纯 C 单元的
    /// `CompileUnit::line_entries` 恒空，`src/debug.rs` 只能退到「每函数一行（声明行）」的
    /// 兜底表 —— 症状是 gdb 里 `break file.c:5` 报 `No line 5 in file`（语句级断点绑不上）。
    /// aarch64 / riscv 后端早就在语句发射入口记这一条（`src/codegen/aarch64/emit/block.rs`）。
    pub stmt_line_entries: Vec<(u64, u32)>,
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
    // ── 寄存器占用真源：池定义 + 两个位图 + 钉住集合，全在 `RegBank` 里 ──
    // 见 `abi/regbank.rs` 头注释与 docs/p12-physreg-spill-design.md §3 H4。
    // 旧的 `callee_saved_pool` / `reg_allocated` / `callee_saved_used` 三个裸字段
    // 已收进本类型：位图仍是内部表示，但外部只能经查询方法读，写路径只剩
    // `claim`/`alloc_temp`/`release`/`reset` 四条。
    pub reg_bank: crate::codegen::x86_64::abi::regbank::RegBank,
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JumpKind {
    Jmp,
    Je,
    Jne,
    Jl,
    Jle,
    Jg,
    Jge,
    Jb,
    Jae,
    Ja,
    Jbe,
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
    /// 声明类型。**必须跟着快照走**：worker 里拿 `Type::Int` 占位就是把「全局变量的
    /// 符号性/宽度」这个程序级事实丢在了并行路径外——实测 `_Bool g; … = g;` 在串行下发
    /// `movzbl`、在并行下发 `movsbl`（§11.13 的 N12，与 N1 同一类）。
    pub ty: Type,
    /// 已机化好的初始化字节（与 `GlobalVar::init` 同形）。少了它，worker 就无法像串行
    /// 那样从初值折叠。
    pub init: Option<Vec<u8>>,
    /// 该变量是否已经在数据段里有实体。发射点的分派读它，不能拿默认值凑。
    pub in_data_section: bool,
}

#[derive(Clone)]
pub struct ParallelEmitCtx {
    pub target: Target,
    pub relocatable: bool,
    pub function_sections: bool,
    pub emit_asm_text: bool,
    pub debug_level: u8,
    /// 优化档。并行发射下**必须**跟着快照下行：护栏（S4/A2）读它，worker 里丢了它
    /// 就是「同一个源文件在并行/不并行两条路上产出不同字节」。
    pub opt_level: u8,
    pub emit_cfi: bool,
    pub source_name: String,
    pub line_map: Vec<LineSpan>,
    pub globals: HashMap<String, ParallelGlobalSnap>,
    /// 静态局部变量的程序级地图（名字 mangle / 需运行时初始化集合）。
    /// 不下行时 worker 里 `is_static` 的局部走不了「实体在 data/bss」那条路，同一个
    /// 函数的字节比串行一大截（§11.13 的 N12：实测 `static_local_arrays.c` 串行 1458B /
    /// 并行 1610B）。主 cg 在 `pre_register_static_locals()` 回填之后才取本快照，
    /// 所以 worker 拿到的就是发射点实际读到的那一份。
    pub static_local_map: crate::static_locals::StaticLocalMap,
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
    /// 程序级符号事实的下行快照（与 `FuncEmitChunk::facts` 同一张字段表）。
    pub facts: SymbolFacts,
    pub error_attr_fns: HashSet<String>,
    pub global_homes: HashMap<String, (crate::location::Location, Type)>,
    pub rodata: Vec<u8>,
    pub string_pool: HashMap<Vec<u8>, u64>,
    pub text_align: u64,
    /// 完整 struct/union 定义表（名字 → 字段内联的完整类型）。worker 用
    /// `Codegen::new` 重建时不带这张表，遇到「先前向声明、后定义」产生的不透明
    /// 命名 struct（字段为空）就无法补全成员偏移，`->field` 会静默退化为偏移 0。
    /// 故与 global_seg / static_local_map 一样必须跟着快照下行。
    pub complete_struct_types: HashMap<String, Type>,
    /// 本 worker 的 `.L` 内部标号命名空间基址。
    ///
    /// `Codegen::next_label` 是 `.L<n>` 唯一的号源 (见 `instructions::fresh_label`),
    /// 而 worker 用 `Codegen::new` 重建 ⇒ 默认全从 0 起号 ⇒ 合并时同名覆盖。
    /// 由 `emit_functions_parallel` 按 **slot 下标** 静态分配互不重叠的区间。
    pub label_base: u32,
    /// 本 worker 可消耗的标号数; 高水位 = `label_base + label_span`。
    pub label_span: u32,
}

/// 一个住在 caller-saved 号里的自动绑定变量，它在帧内的**保存槽**（前置三）。
///
/// 为什么存的是偏移而不是 `Spill(slot)`：发射侧存/取用的就是 `store_stack`/`load_stack`
/// 这两个按帧内偏移工作的函数（它们也是 `Location::Stack` 走的那两个），拿偏移就不必
/// 再造一个「谁会把槽序号翻译成偏移」的第二知道。翻译仍只有 `LayoutMap::spill_off` 一处。
#[derive(Debug, Clone)]
pub struct ExtHomeSave {
    /// 变量名（覆盖表与存/取都用它作键）
    pub name: String,
    /// 它的家（一定在 `AUTO_EXT_POOL` 里，否则不会进本表）
    pub reg: u8,
    /// 帧内保存槽偏移
    pub slot_off: i64,
    /// 存取宽度（按该变量的类型，不是按 8 字节默认）
    pub size: usize,
}

/// 一个局部变量在**本帧**里的位置（Phase 6.2 交给 DWARF 的唯一读数）。
///
/// 为什么需要它与它的采集点：以前 `-g` 下一个变量 DIE 都不发（`local_vars` 恒空），
/// 而 `src/ssa/regalloc.rs` 因此必须在 `-g` 下禁止寄存器家（DWARF 只能发栈槽位置，
/// 绑上去就是让调试器读一个从未写过的槽）。取消那条限制的前提就是本结构：
/// 「每个局部住在哪」由发射器在函数体生成完之后交付（那时 `homes` 与 `var_map`
/// 都是最终值），而不让 debug 侧自己再推一遍帧坐标 —— 那是第六个现场推算偏移的人。
#[derive(Debug, Clone)]
pub struct DebugVarLoc {
    pub name: String,
    /// 与 DWARF 的偏移基准无关：这里就是 `%r13 + off`（`Stack`）、物理寄存器（`Reg`）
    /// 或溢出槽序号（`Spill`）；换算（CFA 关系、DWARF 寄存器号）只发生在 `debug.rs` 那一侧。
    pub loc: crate::location::Location,
    /// 分段位置（loclist）：`[begin_pc, end_pc)` 这段 PC 上对象其实在 `loc` 里说的地方。
    ///
    /// 为什么必须能分段：区内家（S4）推迟一次存出时，那段区间里**真值在号里而不是在
    /// 栈槽上**；单 `loc` 会让调试器读一个从未写过的槽（方案 §4.4 点名的就是这件事）。
    /// 空表 ⇒ 本函数只有一个位置，与木结构存在前的产物逐字节相同。
    pub ranges: Vec<DebugVarRange>,
    pub ty: crate::ast::Type,
    pub decl_line: u32,
}

/// 一段 PC 区间上的位置（与 `debug::DwarfLocRange` 同形状，但不拿 DWARF 的注册号：
/// 发射侧只知道硬件号，换算只在 `debug.rs` 那一侧发生）。
#[derive(Debug, Clone, Copy)]
pub struct DebugVarRange {
    pub begin_pc: u64,
    pub end_pc: u64,
    pub loc: crate::location::Location,
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
    /// 带元素偏移的全局重定位 (数组型 static local 逐元素运行时初始化,
    /// 如 Lua 的 `static const void *disptab[] = {&&L_OP_*, ...}` 计算跳转表)。
    ///
    /// **必须随 chunk 合并**: 漏了它, 并行发射 (-O2 起) 里 worker 为跳转表各元素发的
    /// store 的重定位就永远回不到主 Codegen, disp32 停留在 `elem_off` 占位值上, 于是一条
    /// `mov %rax, disp32(%rip)` 落到 `rip + elem_off` —— 函数自己的 .text 里, 写只读文本
    /// 直接段错误 (Lua `luaV_execute` 的 disptab 初始化即此形状)。
    pub pending_data_rels_with_off: Vec<(u64, String, i32)>,
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
    /// Phase 6.2：本 chunk 函数的「局部 → 位置」表（只在 `-g` 下非空）。不随 chunk
    /// 带回，并行 `-g` 构建里由 worker 发的那些函数就一个变量 DIE 都没有 ——
    /// 与 N1/N12 同一族（函数级事实只存在于主 Codegen），所以必须带。
    pub debug_var_facts: Vec<(String, Vec<DebugVarLoc>)>,
    /// Phase 6.2：本 chunk 采集的**行号条目**（只在 `-g` 下非空），PC 相对 chunk 代码起点。
    ///
    /// 三张表都必须带回：`-O2` 起默认走并行发射（`cli.rs` 的 `opt >= 2`），而
    /// `.debug_line` 只由主 Codegen 的这三张表拼装（`backend.rs::build_debug_sections`）。
    /// 不带回就是「`-g -O0` 有语句行、`-g -O2` 整张表退化成每函数一行」——`break file.c:N`
    /// 在被 Phase 6.2 放开寄存器家的那一档（`-g -O2`）里全部绑不上，与 N1/N12/debug_var_facts
    /// 同一族（函数级事实只存在于 worker，没有回程）。
    pub stmt_line_entries: Vec<(u64, u32)>,
    pub asm_line_entries: Vec<(u64, u64, u32)>,
    pub asm_insn_debug_entries: Vec<(u64, u32, u16)>,
    /// P9 步骤 5: worker 采集的内联汇编指令日志 (chunk 相对 pc), 回主 Codegen 合并。
    pub(crate) asm_insn_log: Vec<(u64, Insn)>,
    /// P9 方案 C: worker 采集的原生 C 指令记录 (chunk 相对 pc), 合并时加 base。
    pub(crate) insn_records: Vec<(u64, Insn)>,
    /// P9 方案 C 临时统计 (worker 副本).
    pub(crate) emit_insn_stats: std::collections::HashMap<String, usize>,
    pub(crate) emit_insn_gas_hits: std::collections::HashMap<bool, usize>,
    /// rodata 增量：chunk 编译期间新增的 rodata 数据
    pub new_rodata: Vec<u8>,
    /// chunk 开始时的 rodata 大小 (用于计算增量)
    pub rodata_base: u64,
    /// 本 chunk **新登记**的内部化条目 `(内容键, chunk 局部 rodata 偏移)`，按偏移升序。
    ///
    /// 这是 N10 的唯一修复抓手。串行发射下「谁第一次写到这个键，谁就定字节位置」，
    /// 而 worker 只看见自己的 `string_pool` 快照 ⇒ 跨 worker 重复的字符串各自落一份
    /// 字节、并且主 Codegen 的 `string_pool` 根本不知道有这回事（于是 `compile_rel`
    /// 末尾那趟「把 `string_refs` 里的键补进 rodata」又加第三份）。回传这张表，
    /// 合并阶段就能按 **源码序重放**「字节落位 + 按内容键去重」这件事本身，
    /// 并把引用里那些按偏移的读数从 chunk 局部坐标映射到最终坐标（N10，§11.33）。
    /// 必须按偏移升序：`HashMap` 的迭代序随运行变化，直接遍历它会让产物依赖哈希随机度。
    pub pool_add: Vec<(Vec<u8>, u64)>,
    /// 本 chunk 的 `.L` 标号区间基址 / 容量 / 实际用量。
    ///
    /// `label_used > label_span` 意味着本函数越进了下一个 slot 的命名空间,
    /// 合并后两个函数的同名 `.L<n>` 会互相覆盖 —— 必须报错, 不能静默降级。
    pub label_base: u32,
    pub label_span: u32,
    pub label_used: u32,
    /// worker 侧新增的程序级符号事实（上行回传）。
    ///
    /// 不回传 ⇒ 主 Codegen 的 `defined_funcs` 里没有嵌套函数，ELF 侧把它当外部
    /// 符号发成 UND，链接期 `undefined reference to 'inner_parent.0'`；`func_sizes`
    /// 缺失则符号 size 退回 `compute_func_size_fallback`（§11.13 的 N1）。
    pub facts: SymbolFacts,
}

/// 跨函数共享、且在 `compile_func` 期间还会**新增**的程序级符号事实。
///
/// 为什么收成一张表而不是散字段：并行发射的 N1 病根就是「worker 新增的事实没有
/// 回程」，而散字段意味着下行快照、worker 重建、chunk 结构、合并四处各自记住一份
/// 「哪几个字段算程序级」。下行与上行共用这一个类型，将来新增一条事实只有一处可漏。
#[derive(Clone, Default)]
pub struct SymbolFacts {
    /// 本 TU 中有函数体的函数名（含嵌套函数的唯一名）
    pub defined_funcs: HashSet<String>,
    /// 函数名 → 代码大小（嵌套函数的 size 只有编译它的那个 worker 知道）
    pub func_sizes: HashMap<String, u64>,
}

impl Codegen {
    /// 取当前程序级符号事实的快照（并行发射下行的那一份）。
    pub(in crate::codegen::x86_64) fn capture_symbol_facts(&self) -> SymbolFacts {
        SymbolFacts {
            defined_funcs: self.defined_funcs.clone(),
            func_sizes: self.func_sizes.clone(),
        }
    }

    /// 合并 worker 带回的程序级符号事实：**只插缺的**。
    ///
    /// 这个语义成立的前提是「所有 worker 都从同一份快照出发，且每个函数只会被
    /// 一个 worker 编译」⇒ 两侧同名键的值必然相同，谁先落表都一样。
    pub(in crate::codegen::x86_64) fn merge_symbol_facts(&mut self, facts: &SymbolFacts) {
        for name in &facts.defined_funcs {
            self.defined_funcs.insert(name.clone());
        }
        for (name, size) in &facts.func_sizes {
            self.func_sizes.entry(name.clone()).or_insert(*size);
        }
    }
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
            frame_layout: crate::codegen::x86_64::abi::layout::LayoutMap::unresolved(),
            expr_depth: 0,
            late_site_seq: 0,
            late_slot_budget: 0,
            max_vec_bytes: 0,
            vec_perm_level: 0,
            late_slot_used: 0,
            late_var_cursor: 0,
            frame_planned: false,
            eval_holding: None,
            peep_last: None,
            cmp_flags_token: false,
            value_is_dead: false,
            cmp_flags_granted: false,
            last_cmp: None,
            block_state: Default::default(),
            volatile_frames: HashSet::new(),
            frame_guard: crate::frame_guard::FrameGuard::default(),
            a2_no_inherit: false,
            a2_incoming: HashMap::new(),
            a2_seen: HashMap::new(),
            a2_snap: HashMap::new(),
            region: Default::default(),
            escaped_frames: HashSet::new(),
            dangling_prefix: 0,
            volatile_locals: HashMap::new(),
            volatile_slots_overflow: false,
            auto_homes: HashMap::new(),
            home_overrides: HashMap::new(),
            ext_home_saves: Vec::new(),
            max_expr_depth: 0,
            temp_slots: 0,
            var_map_total_slots: 0,
            vla_offsets: HashMap::new(),
            vla_total_size: 0,
            vm_struct_size_slots: HashMap::new(),
            cur_func_has_vla: false,
            disptab_base_reg_sym: None,
            cur_func_is_naked: false,
            bf_rmw_depth: 0,
            stack_shift: 0,
            vector_op_slot: 1,
            cur_func_over_align: 0,
            cur_func_anchor_off: 0,
            cur_func_do_align: false,
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
            pending_data_rels_with_off: vec![],
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
            static_local_label_owner: None,
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
            homes: HashMap::new(),
            addr_escaped: std::collections::HashSet::new(),
            addr_escaped_done: false,
            global_homes: HashMap::new(),
            pending_data64_rels: vec![],
            pending_data_textlabel_rels: vec![],
            pending_rodata_rels: vec![],
            pending_label_diff_rels: vec![],
            data_relocations: vec![],
            cleanup_vars: vec![],
            cold_arms: vec![],
            cold_merged: 0,
            extra_rela_sections: vec![],
            comdat_groups: vec![],
            pending_debug_info_relocs: vec![],
            debug_var_facts: Vec::new(),
            debug_loc_windows: Vec::new(),
            pushsection_stack: vec![],
            section_stack: vec![],
            section_active: false,
            pending_custom_sec_rels: vec![],
            pushsection_exec_stack: vec![],
            pushsection_code_ranges: vec![],
            pushsection_label_offsets: HashMap::new(),
            deferred_pushsection_fixups: vec![],
            emit_asm_text: false,
            debug_level: 0,
            opt_level: 0,
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
            asm_insn_log: Vec::new(),
            insn_records: Vec::new(),
            emit_insn_stats: std::collections::HashMap::new(),
            emit_insn_gas_hits: std::collections::HashMap::new(),
            stmt_line_entries: Vec::new(),
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
            // 寄存器占用真源（逐函数复位见 `reset_reg_tracker`）
            reg_bank: crate::codegen::x86_64::abi::regbank::RegBank::new(),
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
        // 预扫描: 记录每个函数的变参性。
        //
        // 为什么现在也要记 `false`：调用序列里那句 `mov $N, %al` 是 SysV 给**变参**被调方
        // 用的（告诉它几个 xmm 寄存器装了参数）。只知道「查不到」时不能省，而本表以前只存
        // `true` 条目，等于「所有已知非变参函数也被当成可能读 %al」。实测 `luaV_execute`
        // 因此多了 250 条两字节指令，全在分派热路径上。
        //
        // 但「记 `false`」的门槛必须是**看过原型**：
        //   * 有体（定义）⇒ 参数表就是真的；
        //   * 无体但 params 非空 ⇒ 带原型的声明；
        //   * 无体且 params 为空 ⇒ 可能是 K&R 风格的 `int foo();`（参数 unspecified，
        //     旧代码里这种被调方可以读 %al）⇒ **不记**，退回照旧发那条 mov。
        for decl in &prog.decls {
            if let Decl::Func(f) = decl {
                Self::register_func_sig(f, &mut self.func_param_types);
                if f.is_variadic {
                    self.variadic_funcs.insert(f.name.name.clone(), true);
                } else if !f.is_old_style && (f.body.is_some() || !f.params.is_empty()) {
                    self.variadic_funcs.insert(f.name.name.clone(), false);
                }
                if f.is_old_style {
                    self.old_style_funcs.insert(f.name.name.clone());
                }
                if let Some(body) = &f.body {
                    let mut nested = Vec::new();
                    Self::collect_nested_funcs(&body.items, &mut nested);
                    for nf in nested {
                        Self::register_func_sig(&nf, &mut self.func_param_types);
                        if nf.is_old_style {
                            self.old_style_funcs.insert(nf.name.name.clone());
                        }
                    }
                    // 嵌套函数的唯一符号名在此一次性预分配（以**唯一名**进
                    // `nested_funcs`），发射期只查不分配：并行下 worker 改不了全局
                    // 计数，也不把新登记的事实带回来（N1）。
                    self.prescan_nested_names(&body.items, &f.name.name);
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
                        self.global_seg.insert(v.name.name.clone(), v.addr_space);
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
                _ => {},
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
            Stmt::If { then_, else_, .. } => {
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
        // 逐 TU 采集的调试信息表也逐 TU 清空：不准则上一次的局部会被当成这一次的名字
        // （同一进程里跑两次编译、或串行/串行对比时都会露）。
        self.debug_var_facts.clear();
        self.debug_loc_windows.clear();
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
        self.emit_insn(&Insn::mov_rm_reg(crate::codegen::shared::asm_operand::Width::B64, 12, 4)); // mov r12, rsp (保存原始 rsp, 用于 leave 后恢复)
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Push,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    5,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // push rbp
        self.emit_insn(&Insn::mov_rm_reg(crate::codegen::shared::asm_operand::Width::B64, 5, 4)); // mov rbp, rsp
                                                                                                  // 调用 constructor 函数
        for (_p, ctor) in &ctors {
            // P11: xor rax, rax via typed opcode (no variadic args)
            {
                let insn = Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                        op: crate::codegen::x86_64::opcode::AluOp::Xor,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                    },
                    &[],
                );
                if !self.emit_insn(&insn) {
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::Xor,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    )); // xor rax, rax (no variadic args)
                }
            }
            self.call_label(ctor);
        }
        // P11: xor rax, rax via typed opcode (main call)
        {
            let insn = Insn::new(
                crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                    op: crate::codegen::x86_64::opcode::AluOp::Xor,
                    width: crate::codegen::shared::asm_operand::Width::B64,
                    dst: crate::codegen::x86_64::opcode::Operand::Reg(
                        0,
                        crate::codegen::shared::asm_operand::Width::B64,
                    ),
                    src: crate::codegen::x86_64::opcode::Operand::Reg(
                        0,
                        crate::codegen::shared::asm_operand::Width::B64,
                    ),
                },
                &[],
            );
            if !self.emit_insn(&insn) {
                self.emit_insn(&Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                        op: crate::codegen::x86_64::opcode::AluOp::Xor,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                    },
                    &[],
                )); // xor rax, rax
            }
        }
        self.call_label("main");
        // 保存 main 返回值
        self.emit_insn(&Insn::mov_rm_reg(crate::codegen::shared::asm_operand::Width::B32, 13, 0)); // mov r13d, eax
                                                                                                   // 调用 destructor 函数
        for (_p, dtor) in &dtors {
            // P11: xor rax, rax via typed opcode
            {
                let insn = Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                        op: crate::codegen::x86_64::opcode::AluOp::Xor,
                        width: crate::codegen::shared::asm_operand::Width::B64,
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Reg(
                            0,
                            crate::codegen::shared::asm_operand::Width::B64,
                        ),
                    },
                    &[],
                );
                if !self.emit_insn(&insn) {
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                            op: crate::codegen::x86_64::opcode::AluOp::Xor,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                            src: crate::codegen::x86_64::opcode::Operand::Reg(
                                0,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    )); // xor rax, rax
                }
            }
            self.call_label(dtor);
        }
        self.emit_insn(&Insn::mov_rm_reg(crate::codegen::shared::asm_operand::Width::B64, 4, 12)); // mov rsp, r12 (恢复原始 rsp)
        self.emit_insn(&Insn::mov_rm_reg(crate::codegen::shared::asm_operand::Width::B32, 7, 13)); // mov edi, r13d (main 返回值)
                                                                                                   // mov eax, 0x3c
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                rip_relative: false,
                width: crate::codegen::shared::asm_operand::Width::B32,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    0,
                    crate::codegen::shared::asm_operand::Width::B32,
                ),
                src: crate::codegen::x86_64::opcode::Operand::Imm(0x3c),
            },
            &[],
        ));
        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Syscall));
        self.emit_insn(&Insn::short_jump_rel8(0xEB, -2)); // jmp self
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
        // 逐 TU 采集的调试信息表也逐 TU 清空：不准则上一次的局部会被当成这一次的名字
        // （同一进程里跑两次编译、或串行/串行对比时都会露）。
        self.debug_var_facts.clear();
        self.debug_loc_windows.clear();
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
                // Phase 3.2: 判据读 `v.home`（IR 位置表已解析出来的家），不再独立判
                // `register_name.is_some()` —— 同一事实只有一处。用户文本名仍由
                // `resolve_asm_reg` 这一份解析器翻译；没拿到家（非法名/保留号）就继续往下
                // 走正常存储分配，与 rel_symbols 那侧（`global_homes.contains_key`）同源。
                if let Some(loc) = v.home.or_else(|| self.resolve_asm_reg(v)) {
                    // 文件作用域是唯一需要这句话的地方：与局部绑定不同，全局对象看起来
                    // 「应该住在某处」，而绑到一个会被调用毁掉的号上时这个期望正是错的。
                    self.diag_call_clobbered_global_binding(v, &loc);
                    self.global_homes
                        .insert(v.name.name.clone(), (loc, v.ty.clone()));
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
                        let is_float = v.ty.is_real_float();
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
                                let is_float = v.ty.is_real_float();
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
                                let is_float = v.ty.is_real_float();
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
                    match const_eval::unwrap_string_cast(init) {
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
                        // 函数指针 / 外部符号初始化: T *fp = other_fn; 或
                        // static T (*fp)(...) = other_fn;
                        // 这里 init 直接是 Expr::Ident (不是 &ident), 需要发出
                        // R_X86_64_64 重定位, 在链接时填入符号地址。
                        Expr::Ident { name: ref_name, .. } => {
                            if matches!(&v.ty, Type::Ptr(_) | Type::Func(..)) {
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
                            } else {
                                false
                            }
                        },
                        _ => {
                            // 浮点目标: eval_full 取位模式 (double x = 1 / 1.5;)
                            let is_float = v.ty.is_real_float();
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

        // 字符串引用 → rodata 定址的前置条件：每个被引用过的内容键都已在池里。
        //
        // N10: 这里原本是一趟「兜底补拷贝」—— 把 `string_refs` 里还没进池的键再
        // `extend` 进 `rodata`。它把并行发射的「漏登记」掩盖成「产物看起来合法但
        // `.rodata` 多存一份」：旧 `merge_func_chunk` 从不把 chunk 的新键写回主
        // `string_pool`，于是每个被引用过的字符串都会在这里再落一份字节（实测
        // `tests/fixtures/c/main.c` 的 `.rodata` 28B → 56B，符号地址随之改变）。
        // 现在合并期按内容键重放已经保证「每个内部化条目在池里恰好一份字节、
        // 引用必然命中」，命中不了就是后端内部不变量破坏：只能报 E1105，绝不
        // 能再静默补一份（补一份就是把本缺陷写回产物）。
        // 保留补拷贝本身只为诊断可继续（否则会走到 `string_pool[&data]` 直接
        // panic），但报了 E1105 就是硬错，产物不会被当作合法输出交付。
        let string_data_list: Vec<Vec<u8>> =
            self.string_refs.iter().map(|(_, d)| d.clone()).collect();
        for data in &string_data_list {
            if !self.string_pool.contains_key(data) {
                self.report_internal_bug(
                    "E1105",
                    format!(
                        "string reference at .text+{:#x} has no interned rodata entry for {:?} \
                         (pool has {} entries, .rodata is {} bytes)",
                        self.string_refs
                            .iter()
                            .find(|(_, d)| *d == *data)
                            .map(|(p, _)| *p)
                            .unwrap_or(0),
                        data,
                        self.string_pool.len(),
                        self.rodata.len()
                    ),
                    "rodata 内部化条目的登记与引用必须同时到达主 Codegen; \
                     不要用 DCC_NO_PARALLEL / -j1 绕过, 请修 merge_chunk_rodata",
                );
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

    /// P13 阶段6：槽数超过上限的 volatile 局部不得按槽枚举。
    ///
    /// `volatile char big[0x100000000UL]` 的 VLA 在 `var_alloc_slots` 里是 5.4 亿槽，
    /// 逐槽插入实测把编译拖到 6 分钟 / 5.9 GB RSS（`perf_regression` 的 30 秒上限
    /// 直接超时）。溢出时唯一的正确方向是整体关闭本函数的槽事实：少削几条只丢优化，
    /// 漏登记一条 volatile 槽就是静默错值。
    #[test]
    fn huge_volatile_object_disables_slot_facts_instead_of_enumerating_them() {
        let mut cg = x86();
        cg.volatile_locals.insert("big".into(), 536_870_912);
        cg.note_volatile_var_slots("big", -64);
        assert!(cg.volatile_slots_overflow, "超大 volatile 必须触发整体关闭");
        assert!(cg.volatile_frames.is_empty(), "溢出时不得留下一半屏障");
    }

    /// 反向守卫：普通大小的 volatile 局部仍要逐槽登记（否则块内优化会把它的槽
    /// 当成普通槽转发，那次访问就凭空少了一次）。
    #[test]
    fn ordinary_volatile_object_still_enumerates_every_slot() {
        let mut cg = x86();
        cg.volatile_locals.insert("v".into(), 2);
        cg.note_volatile_var_slots("v", -64);
        assert!(!cg.volatile_slots_overflow);
        assert!(cg.volatile_frames.contains(&-64));
        assert!(cg.volatile_frames.contains(&-56));
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
        assert!(cg.pending_code_rels.is_empty(), "内部标号不得变成外部重定位");
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

    /// Phase 5.1.c：**`Spill(槽号)` 与「同一个偏移的 `Stack` 家」必须发出逐字节相同的指令**。
    ///
    /// 这条对拍就是「翻译只发生在一处」的证据：三个消费臂（取地址 / 读 / 写）如果此哪个
    /// 自己拿槽号当偏移用（少翻译一次）或翻译两次（多减一层），字节流当场分叉。比直接匹配
    /// 位移字节好：它不依赖宽度/寻址形式，将来改 `load_stack` 的编码不会把用例弄假。
    ///
    /// 最后一句钉住「不是靠回退到 var_map 旧槽凑上的」：同一个名字没有家时算出来的
    /// 坐标必须与带里那个不同（否则本用例三个臂可以全部走回退而照样绿）。
    #[test]
    fn spill_home_goes_through_the_single_translation_point() {
        use crate::codegen::x86_64::abi::layout::{
            FramePlan, LayoutMap, TRAMP_RESERVE, VEC_RESERVE,
        };
        use crate::location::Location;
        let mut cg = x86();
        cg.frame_layout = LayoutMap::resolve(&FramePlan {
            temp_slots: 2,
            var_bytes: 512,
            // 晚分配带与本用例无关（测的是 spill 翻译点），传 0 保持原有坐标
            late_slots: 0,
            sret_need: 64,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
            vec_reserve: VEC_RESERVE,
            tramp_reserve: TRAMP_RESERVE,
            stack_reserve: 48,
            spill_slots: 3,
        });
        assert_eq!(cg.frame_layout.spill_band_bytes, 24, "本用例需要一个非零 band");
        let want = cg.frame_layout.spill_off(1);
        cg.homes
            .insert("v".to_string(), (Location::Spill(1), Type::Long));
        assert_eq!(cg.stack_home_off("v"), want, "取地址必须走 spill_off");

        // `code.clear()` 只是测试的隔离手法；窥孔窗口记在 `peep_last` 里，不跟着
        // 字节一起清。不清就会把「上一次清空前同地址的 load」当成相邻重复而删掉
        // （规则 1），下面两条等式就变成空跑的相等。
        cg.code.clear();
        cg.peep_last = None;
        cg.block_state.reset();
        cg.mov_from_stack(0, "v");
        let read_spill = cg.code.clone();
        cg.code.clear();
        cg.peep_last = None;
        cg.block_state.reset();
        cg.mov_to_stack(0, "v");
        let write_spill = cg.code.clone();

        // 换成显式 `Stack(want)`（同一个坐标）：两条发射序列必须逐字节相同
        cg.homes
            .insert("v".to_string(), (Location::Stack(want), Type::Long));
        cg.code.clear();
        cg.peep_last = None;
        cg.block_state.reset();
        cg.mov_from_stack(0, "v");
        let read_stack = cg.code.clone();
        cg.code.clear();
        cg.peep_last = None;
        cg.block_state.reset();
        cg.mov_to_stack(0, "v");
        let write_stack = cg.code.clone();
        assert_eq!(read_spill, read_stack, "读路的 spill_off 与 Stack 家发的字节不一样");
        assert_eq!(write_spill, write_stack, "写路的 spill_off 与 Stack 家发的字节不一样");
        assert!(
            !read_spill.is_empty() && !write_spill.is_empty(),
            "两侧都是空字节流 ⇒ 上面两条等式是空跑的相等"
        );

        // 没家时走 var_map：坐标与带里那个不同（排除「靠回退凑绿」）。这里**显式**
        // `register_var` 而不是拿 `var_offset` 的兜底：§11.23 步 1 之后那条兜底已是
        // `E1134` 后端不变量破坏，本用例不得依赖一个错路才能成立。
        cg.homes.remove("v");
        cg.register_var("v", &crate::ast::Type::Long);
        assert_ne!(cg.stack_home_off("v"), want, "本用例必须真的在比较两个不同的坐标");
        // 而带外的家（Imm）仍得走点名报错，不得静默当成帧内坐标
        cg.homes
            .insert("w".to_string(), (Location::Imm(7), Type::Long));
        cg.stack_home_off("w");
        assert!(
            err_codes(&cg).contains(&"E1129"),
            "Imm 家必须仍报 E1129（没接线的家要可见地失败），实得 {:?}",
            err_codes(&cg)
        );
    }

    /// §11.23 步 3：**发射期晚分配只能住在自己的带里**。
    ///
    /// 本用例把布局解算、预算、账面、到位判据四件事拉在一起对拍，因为它们只要有一件
    /// 与实际不一致，产物就会静默重叠（旧写法靠 `min_used` 夹逼避免那一幕，但夹逼本身
    /// 就是在已发射出去的 `sub rsp` 之下现场推坐标）：
    /// * 两个对象从带顶往下排，第三个就出带 ⇒ `E1133`；
    /// * 带里用掉的字节不得再压变量区水位（`var_map_total_slots` 不动），也不得
    ///   压低 `sret_region_base` 锚点（解算完它就不再动）；
    /// * 预算不足但位置还在帧内 ⇒ 仍要报 `E1133`（账面判据与到位判据两条独立）。
    #[test]
    fn late_allocation_lands_in_the_band_and_rejects_overrun() {
        use crate::codegen::x86_64::abi::layout::{
            FramePlan, LayoutMap, TRAMP_RESERVE, VEC_RESERVE,
        };
        let mut cg = x86();
        cg.frame_layout = LayoutMap::resolve(&FramePlan {
            temp_slots: 2,
            var_bytes: 128,
            late_slots: 2,
            sret_need: 64,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
            vec_reserve: VEC_RESERVE,
            tramp_reserve: TRAMP_RESERVE,
            stack_reserve: 48,
            spill_slots: 0,
        });
        // 本用例需要非零带：带顶/带底不得重合，否则「在带内」是空条件
        assert_eq!(cg.frame_layout.late_band_bytes, 16, "需要一个两槽的带");
        // 预算与「规划已结束」由 `plan_late_slot_budget()` 写下，这里给同形的值
        cg.late_slot_budget = 2;
        cg.frame_planned = true;
        let sret_anchor_before = cg.sret_region_base;
        let var_slots_before = cg.var_map_total_slots;

        let a = cg.register_var("__compound_lit_1", &crate::ast::Type::Long);
        let b = cg.register_var("__compound_lit_2", &crate::ast::Type::Long);
        assert_eq!(a, cg.frame_layout.late_band_top - 8, "第一槽从带顶开始");
        assert_eq!(b, cg.frame_layout.late_band_top - 16, "第二槽接着往下");
        assert_eq!(b, cg.frame_layout.late_band_bottom(), "第二槽的最低字节恰在带底");
        assert_eq!(cg.late_slot_used, 2, "账面必须逐笔累加");
        assert!(cg.diagnostics.is_empty(), "带内分配不得报错: {:?}", err_codes(&cg));
        // 晚分配不得再动变量区水位与 sret 锚点（那两段现场推坐标的代码就是被删掉的）
        assert_eq!(cg.var_map_total_slots, var_slots_before, "带里的对象不占变量区");
        assert_eq!(cg.sret_region_base, sret_anchor_before, "锚点不再被晚分配压低");

        // 第三个：带里没地方了 ⇒ 账面与到位两条判据都得响
        let c = cg.register_var("__compound_lit_3", &crate::ast::Type::Long);
        assert_eq!(c, cg.frame_layout.late_band_bottom() - 8);
        assert!(
            err_codes(&cg).contains(&"E1133"),
            "超出预算的晚分配必须报 E1133，实得 {:?}",
            err_codes(&cg)
        );
        assert_eq!(cg.late_slot_used, 3, "报错后仍分配一个槽，只为后续流程不崩");
    }

    /// 「刚求出来的值在哪」必须是一条**可作废的记录**，而不是一个永远声称成立的常量。
    /// 本用例同时钉三件事：两种写法字节全等（不改产物）、记得到、任何新指令出去就作废。
    #[test]
    fn eval_holding_is_recorded_and_invalidated_by_the_single_sink() {
        use crate::location::{RegBank as RBank, RegId};
        let e = Expr::Int {
            v: 7,
            span: crate::diagnostics::Span::point("t.c", 1, 1),
            ty: Type::Int,
        };
        let mut a = x86();
        let mut b = x86();
        let held = a.gen_expr_value(&e);
        b.gen_expr(&e, Codegen::EVAL_BASE);
        assert_eq!(a.code, b.code, "持有记录不是只读事实：它改了发射字节");
        assert_eq!(
            held,
            RegId {
                bank: RBank::Gp,
                n: Codegen::EVAL_BASE
            },
            "落点必须由求值基址给出，而不是用例里手写一个 0"
        );
        assert_eq!(a.eval_held(), Some(held), "刚求完就应该查得到");
        // 核心语义：失效点就是唯一的追写出口。
        a.emit(&[0x90]);
        assert_eq!(a.eval_held(), None, "发了新指令后记录还声称有效 —— 那是陈旧值");
    }

    /// 持有记录不得活过一个函数：不准则新函数开头的 `eval_held()` 会拿到上一个函数留下的号。
    #[test]
    fn eval_holding_is_reset_per_function() {
        use crate::location::{RegBank as RBank, RegId};
        let (prog, _d) = crate::parser::parse(b"int q(void) { return 1; }", "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        let src = m.source();
        let f = src
            .decls
            .iter()
            .find_map(|d| match d {
                crate::ast::Decl::Func(f) if f.name.name == "q" => Some(f.clone()),
                _ => None,
            })
            .expect("源里没有带体的 q");
        let mut cg = x86();
        cg.eval_holding = Some(RegId {
            bank: RBank::Gp,
            n: 3,
        });
        cg.compile_func(&f, src);
        assert_eq!(cg.eval_holding, None, "compile_func 必须复位持有记录");
    }
}
