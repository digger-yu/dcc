// src/codegen/x86_64/abi/layout.rs —— 帧布局的**解算阶段**（P12 字面形态 Phase 2）。
//
// 为什么要有这一步：`docs/p12-physreg-spill-design.md` §3 的 H1 —— 真 SPILL 意味着
// 「新增家」，而家必须在发射前定完。在此之前本后端**没有**布局解算阶段：
// `abi/frame.rs` 只有 91 行发射助手，各区的偏移由消费者按 `%r13+disp` 现场推算
// （实测：现算 `-self.frame_size + 8` 的有 41 个调用点，硬编码 `48 + temp_slots*8` 的有
// 8 处，`frame_size` 的公式本身在 `emit/functions.rs` 里有两份）。设计文档 Q1 担心的
// 「第八个各自推算偏移的消费者」早已是既成事实。
//
// 本模块把这些算式收成一处：`resolve()` 吃静态扫描的结果，吐一份 `LayoutMap`；
// 帧发射（`emit_sub_rsp`）与所有带边界读数都从这一份派生。
//
// **Phase 2 的硬判据是产物逐字节不变**，因此这里的每条公式都必须与搬迁前字面等值，
// 包括下面两处「看着不对称但今天就是这样」的既有事实（本 Phase 只登记，不修）：
//
//  1. 无参/`main` 分支不参与 `va_slots`/`static chain`/位域 RMW 预留（见
//     `emit/functions.rs` 的 `else` 分支：`raw` 里没有 `bf_rmw_reserve`，
//     `vec_temp_off` 也没有 `va_slots + sc_slot`）。这不是笔误的巧合而是两份公式，
//     本模块用「调用方传进来的带大小就是 0」来表达同一事实，于是公式只剩一份，
//     而不对称留在**输入**里（可见、可单独修）。
//  2. trampoline 区与向量临时区在帧底是**重叠**的：`emit/nested.rs:152` 从
//     `-frame_size + 256` 往下摆 trampoline，而 `vec_temp_off`（非变参时
//     `-frame_size + 48 + 8`）向上覆盖 512 字节，两者相交。今天靠各自预留的富余
//     互不撞。收进 LayoutMap 之后仍然保持原样（改了就是改产物），但把坐标摆在
//     一处，Phase 5 要动 spill band 时必须先看这张表。

/// 帧底「复数 / 16 字节聚合 rvalue 物化带」的字节宽度。
///
/// 后端有 90 余处按「复数 rvalue 的内存对固定在 `-frame_size + 8`」这一契约直接
/// 寻址（实部 @ off、虚部 @ off + inner_size；`_Complex long double` 是两个 16 字节
/// tbyte 槽 → 用到 off+32）。帧规划必须为这条带**预留**空间并让变参 reg save area /
/// 向量临时区从带之上开始，否则：
/// - 变参函数的 GP 保存槽 `+8..+16`（保存的 `rsi`）会被复数物化写坏，随后
///   `va_arg(ap, int)` 从 `gp_offset=8` 读回复数位模式；
/// - `_Complex float/double` 变参自身也无处可存 —— `va_arg` 的装载结果按同一契约
///   必须落在该带内（消费端 `gen_complex_part` / `gen_complex_init` /
///   `spill_complex_operand` 一律按该地址回读，不信任寄存器残值）。
///
/// 宽度取 48（16 对齐，覆盖 off+8..off+40 并留下 guard）。从
/// `Codegen::CX_FRAME_BAND` 搬到这里（那个同名的死常量已删）：带宽度只能有一个出处。
pub const CX_FRAME_BAND: i64 = 48;
/// 复数 / 聚合 rvalue 物化带的**按需**尺寸（§5.12）。
///
/// 与向量带同一纪律：`need` 来自一次穷举扫描（`emit/func_setup.rs` 的 `LateNeed`），
/// 判据是「本函数有没有 complex/imaginary 类型，有没有 ≤16B 聚合 rvalue 站点」。
/// 两者都没有 ⇒ 这条带无处可用，也就不必为它开 48B —— 以前每个内核函数都白开
/// 48B，在 16KB 内核栈上直接抵掉一层多调用帧的余量。
///
/// 万一出现「没扫到却真用了带」的路径，[`LayoutMap::cx_rvalue_off`] 会在发射期
/// 直接报错（与 [`LayoutMap::vec_temp`] 同一条 fail-closed 纪律）。
pub fn cx_band_reserve(need: bool) -> i64 {
    if need {
        CX_FRAME_BAND
    } else {
        0
    }
}
/// 向量临时槽区：512 字节（32 个 16B 槽）。嵌套向量 binop 每层要更深的操作数槽，
/// 128 字节只够 3 层，深链式表达式会溢出到变量区（pr23135）。
pub const VEC_RESERVE: i64 = 512;
/// 向量临时槽带的**格数**（与单槽字节无关）。
///
/// 取 32 是为了保住历史容量：`VEC_RESERVE` = 512 = 32 x 16，即旧固定带宽隐含「32 个 16B
/// 槽」。带按本函数最宽向量的步长拉长时，格数必须**不变**，否则宽向量函数可用的槽会从 32
/// 降到 `512 / 步长`，超出那部分直接跑出带区盖到 bf_rmw / trampoline 区（实测把步长统一
/// 成 64B 后 `torture/pr93170.c` 只剩 8 格，第 9 格起越界 abort）。
/// 所以带宽 = 格数 x 步长：单槽 ≤ 16B 的函数仍得 512，帧布局逐字节不变；只有真出现宽
/// 向量的函数才会把帧拉大。带尾要放 `__builtin_shuffle*` 站点的工作区（每层 3 格，见
/// `special/builtins.rs`），消费端用 `LayoutMap::vec_band_slots` 反推可用格数。
pub const VEC_BAND_SLOTS: i64 = 32;
/// 向量临时槽带的尺寸判据（单源）：给定本函数出现过的最大向量字节数，返回带宽。
///
/// 为什么带宽要跟着最宽向量长：固定 `VEC_RESERVE` 隐含「单槽 16B x 32 格」，一个宽向量
/// 站点（源拼接镜像 + 掩码 + 结果）就要吃掉 3 格 x 步长，256B 向量下三格已是 768B，旧
/// 的 512B 带必然溢出到带下的 `bf_rmw` / trampoline 区（实测
/// `torture/builtin-shufflevector-pr123156.c`）。
///
/// 这里只决定**带宽**，不改 `vec_temp` 的槽步长：把步长按函数统一会暂探到既有「同一个
/// 临时槽两侧传不同尺寸」的不一致（实测 `torture/pr93170.c` 从 RUNOK 变错值），那属另一
/// 条轴上的专项修复，不在本轮范围内。
///
/// **没有任何向量类型的函数得 0**：`max_vec_bytes` 是逐表达式 `expr.ty()` 与逐声明
/// 类型穷举扫出来的（`LateNeed::max_vec_bytes`），为 0 就等价于「本函数不可能走到
/// `vector_temp_base`」。以前这里无条件返回 512，于是**每帧**白开 512B —— 对用户态
/// 程序只是浪费，对 16KB 内核栈是硬伤：实测内核 `arch_atomic_read` 这类平函数
/// `sub $0x390,%rsp`（912B），`start_kernel` 的调用链十几层就见底（dcc 编译 7.1 内核
/// 首次全量构建时逐对象复核）。改成 0 之后同一函数降到 368B。
///
/// 万一将来出现「没扫到向量类型却真用了 vec 槽」的路径，`LayoutMap::vec_temp`
/// 会在发射期直接报错（而不是静默把值写到帧外）。
pub fn vec_band_reserve(max_vec_bytes: usize) -> i64 {
    if max_vec_bytes == 0 {
        return 0;
    }
    let slot_bytes = (max_vec_bytes.div_ceil(8) as i64).saturating_mul(8);
    VEC_RESERVE.max(VEC_BAND_SLOTS.saturating_mul(slot_bytes.max(16)))
}
/// 嵌套函数 trampoline 空间：256 字节（最多 8 个，每个 32 字节）。实际代码在生成期按需写入。
pub const TRAMP_RESERVE: i64 = 256;
/// 位域 RMW 临时槽带。每层嵌套占 3 个 8B 槽（24B），对齐 16 ⇒ 每层 32B。
/// 预留从 32B 提到 64B，覆盖 F3 九层嵌套夹具的 disp32 回绕路径。
///
/// 历史上这条带与向量临时带共用基线（`bf_rmw_band_top = vec_temp_off + vec_reserve`，
/// 而两边一个往上长一个往下减），两者落在同一段字节上，只靠「各自都远大于实际
/// 用量」兑住（文件头「不对称 2」）。§5.12 已把每条带摆进自己的区间（见
/// [`LayoutMap::bf_rmw_band_top`] 与 [`LayoutMap::tramp_top`] 的推导注释），从那以后本带
/// 可以与其他带一样按需给，尺寸单源于 [`bf_rmw_band_reserve`]。
pub const BF_RMW_RESERVE: i64 = 64;
/// 位域 RMW 带的**按需**尺寸（§5.12）：`嵌套层数 × [`BF_RMW_SLOT_BYTES`]`。
///
/// 为什么按**链深度**而不是布尔：本带每层占 32B（`bf_rmw_slot(depth, …)` 减
/// `depth*BF_RMW_SLOT_BYTES`），而发射器在算 RHS / 内层操作数之前会把 `bf_rmw_depth`
/// 往前推一层（`dispatch.rs` 的位域写、`increment.rs::gen_bitfield_inc_dec`），所以
/// 链式写法会**同时**占多层。旧的恒 64B 只够 2 层，而 `tests/fixtures/c/bitfield_rmw_deep_nest.c`
/// 的 `s.a = (s.b = (s.c = … = 3))` 是 9 层 —— 即它一直往带外写 224B，今天不报错
/// 只是因为外面碰巧是恒预留的富余（与本文件「不对称 2」同一个机制）。
///
/// `depth = 0`（本函数没有写位域 / 位域自增的站点）得 0；真用了带而带为 0 ⇒
/// [`LayoutMap::bf_rmw_slot`] 当场报错：旧写法会退回 `-8/-16/-24`，而那三个格与金丝雀、
/// callee-saved 保存区同格（`dispatch.rs` 本带注释记的就是这个坑），静默退回去等于把
/// 「少扫一种形态」变成「毁掉调用者的寄存器」。链深度的判据见
/// `emit/func_setup.rs::Codegen::bf_rmw_chain_depth`。
pub fn bf_rmw_band_reserve(depth: usize) -> i64 {
    (depth as i64).saturating_mul(BF_RMW_SLOT_BYTES)
}
/// 每层嵌套占用的字节数（3 个 8B 槽，按 16 对齐补到 32B）。
/// 两套几何（写 2 槽 @16 vs 自增 3 槽 @24）统一为 3 槽/层 @32。
pub const BF_RMW_SLOT_BYTES: i64 = 32;
/// 每层内的三个槽：slot 0 = 容器地址、slot 1 = 容器旧值 / 提取后旧值、
/// slot 2 = 新值（仅自增路径使用；写路径的第 3 槽留空）。
pub const BF_RMW_PER_DEPTH: i64 = 3;
/// 帧头最上面那一格（`%r13 - 8`）：金丝雀/跳表基址共用槽。
///
/// 帧头从这一格往下依次是：callee-saved 保存槽（每个 8B，**按需**）→ 表达式
/// 溅出槽（`temp_slots` 个）→ 变量区（第一槽偏移 `-hdr`）。旧口径恒给 4 个
/// callee-saved 槽（`FIXED_HDR = 48`），内核 trace 上 119 帧平均只真用 1.11 个，
/// 白付接近 3KB（报告 §5.42）。
pub const FRAME_TOP_BYTES: i64 = 8;
/// 每个 callee-saved 保存槽的字节数。第 j 个**已规划**的槽在 `-(16 + 8j)`。
pub const CS_SAVE_SLOT_BYTES: i64 = 8;
/// callee-saved 池的槽数上限（rbx/r12/r14/r15，与 `abi::regbank::POOL` 同序）。
pub const CS_POOL_SLOTS: i64 = 4;
/// **旧口径**的帧头（canary 8 + 4 个 callee-saved 槽 32 + 变量区第一槽 8）。
///
/// 与 `hdr_of(t, 4)` 逐字节相等，只用于 [`LayoutMap::unresolved`] 的占位读数
/// （帧未解算时 `frame_size == 0`，没有任何消费者依赖这个数的精确值）。
/// 活路径一律走 [`LayoutMap::hdr_of`]。
pub const FIXED_HDR: i64 = 48;
/// 帧底最下面那 8 字节（`-frame_size .. -frame_size+8`）的**显式间隙**。
///
/// 为什么要求它：帧底那摞带全部以 `-frame_size` 为基线定位，而 `vec_temp_off` 里写的
/// 是 `+ … + 8`——那 8 字节以前只存在于坐标里，**不在 `frame_size` 的求和里**。于是
/// 「带底不得探到帧底以下」这条不变式永远差 8 字节，只要把某条恒带缩小就现形。
/// 把它成一个命名项并计入求和，两个方向（带底、带顶与变量区的界）才能同时成立。
pub const BOTTOM_PAD: i64 = 8;
/// 帧顶那一格（`%r13 - 8`）。它有两个主人，同一格、**永不同时使用**：
///  - 栈金丝雀（`-fstack-protector*`）；
///  - 跳表基址 `%rbp` 的存槽（阶段 6 批 4）。互斥由判据端保证：`stack_protector`
///    开着时 `decide_disptab_base_reg` 整体不进，所以不存在两者抢一格的可能。
/// 发射点手写字节 `mov [r13-8], rax` 与序言/尾声两处 `frame_size >= 8` 的守卫必须
/// 共用这个数：前者定位置，后者定「帧里放得下吗」。
pub const CANARY_OFF: i64 = -8;
/// 每个 spill 槽的字节数（GP 槽位，与变量区同口径）。
pub const SPILL_SLOT_BYTES: i64 = 8;

/// 解算的输入：全部来自**发射之前**已有的静态扫描。
#[derive(Debug, Clone, Copy)]
pub struct FramePlan {
    /// 表达式栈深度决定的溅出槽数：`(max_expr_depth + 1).max(2)`
    pub temp_slots: i64,
    /// 变量区字节数（`var_map_total_slots * 8`，含参数与规划期登记的名字）
    pub var_bytes: i64,
    /// 晚分配带的槽数（§11.23 步 3）。它就是 `plan_late_slot_budget()` 算出的那份
    /// 预算（`Codegen::late_slot_budget`）：发射期才冒出来的对象（复合字面量 /
    /// VM 尺寸槽 / `va_start` 的 ap / 聚合成员暂存）**不再挤在变量区水位底下**，
    /// 而是排在本函数自己的一条带里。带是布局解算的**输入**，不是事后发明：
    /// 帧大小因此把它算在内，晚分配再也不能从上面压穿 sret/物化区（那就是
    /// `min_used` 夹逼与「锚点单调下移」那两段存在的全部理由）。
    pub late_slots: i64,
    /// sret/物化槽的最大同时存活水位（`max_sret_bytes()` 静态扫描）
    pub sret_need: i64,
    /// 变参 reg save area（非变参函数传 0）
    pub va_slots: i64,
    /// 嵌套函数静态链槽（非嵌套传 0）
    pub static_chain_slot: i64,
    /// 位域 RMW 带；无参/`main` 分支今天传 0（见文件头「不对称 1」）
    pub bf_rmw_reserve: i64,
    /// 复数 / 聚合 rvalue 物化带（尺寸单源于 [`cx_band_reserve`]）。
    ///
    /// 以前 `resolve` 里直接写常量 `CX_FRAME_BAND`，于是这条带**不进扫描**也
    /// 不参与决策：每个函数无论有没有 complex 都付 48B。收成输入后它与其他带同口径。
    pub cx_band: i64,
    /// 向量临时带（尺寸单源于 [`vec_band_reserve`]）
    pub vec_reserve: i64,
    /// trampoline 带
    pub tramp_reserve: i64,
    /// 红区/向量 scratch（`stack_reserve_bytes()`）
    pub stack_reserve: i64,
    /// spill band 的槽数（Phase 5.1.b）：尺寸从已交付的 `Location::Spill` 家导出，
    /// 与其他带一样**参与**帧大小求和（输出在 `spill_band_bytes`）。
    pub spill_slots: i64,
    /// 帧头里 callee-saved 保存槽的**个数**（0..=4，按需；§5.42）。
    ///
    /// 位图来自 `Codegen::compute_callee_plan`（homes ∪ asm 固定约束 ∪ r12 兜底），
    /// 发射期的发号器只在这个集合里取号（`RegBank::set_plan_mask`），所以「规划了
    /// 几个槽」= 「序言会保存几个号」，两者不允许漂移（`patch_callee_saves` 断言）。
    pub cs_slots: i64,
}

/// 解算的输出：一帧里所有带的边界与大小，一次算完，此后无人再「现场推算」。
///
/// 全部偏移都以 `%r13` 为基址、向帧底为负，与搬迁前的算式逐字节等值。
#[derive(Debug, Clone, Copy)]
pub struct LayoutMap {
    pub temp_slots: i64,
    /// 帧头里 callee-saved 保存槽的个数（见 [`FramePlan::cs_slots`]）
    pub cs_slots: i64,
    /// 帧头：`16 (canary 格 + 变量区第一槽) + 8*cs_slots + 8*temp_slots`
    /// （见 [`LayoutMap::hdr_of`]）
    pub hdr: i64,
    pub var_bytes: i64,
    pub sret_need: i64,
    pub va_slots: i64,
    pub static_chain_slot: i64,
    pub bf_rmw_reserve: i64,
    /// 复数 / 聚合 rvalue 物化带（`0` = 本函数不需要；见 [`cx_band_reserve`]）
    pub cx_band: i64,
    pub vec_reserve: i64,
    pub tramp_reserve: i64,
    pub stack_reserve: i64,
    /// spill band 字节数（Phase 2 恒 0：还没有任何值会被判成 spill）
    pub spill_band_bytes: i64,
    /// 晚分配带字节数（`late_slots * 8`，§11.23 步 3）
    pub late_band_bytes: i64,
    /// `sub rsp, frame_size` 的那个数
    pub frame_size: i64,

    // ---- 带边界（%r13 相对，全部从上面的输入派生）----
    /// sret/物化槽区顶界：紧邻变量区底
    pub sret_region_base: i64,
    /// 向量临时区的起点
    pub vec_temp_off: i64,
    /// 变参 reg save area 起点
    pub variadic_reg_save_off: i64,
    /// 嵌套函数静态链槽
    pub static_chain_off: i64,
    /// 帧底复数/聚合 rvalue 内存对的实部槽（`-frame_size + 8`）。
    ///
    /// 花费者一律走 [`LayoutMap::cx_rvalue_off`]：带为 0 时本读数落在别人身上。
    pub rvalue_off: i64,
    /// sret 指针（`rdi`）在帧底的专用槽：位于变参保存区之上、向量区之下
    /// （与静态链同位置，二者互斥）
    pub sret_ptr_off: i64,
    /// trampoline 区的顶（`-frame_size + tramp_reserve`）
    pub tramp_top: i64,
    /// 位域 RMW 带的顶。**本带排在向量带之上、与自己宽度等长的区间里**：槽位
    /// `bf_rmw_band_top - depth*32 - slot*8` 往下减，所以带顶必须高出整条向量带一个
    /// `bf_rmw_reserve`，否则两条带同段（那是 §5.12 修掉的「不对称 2」）。
    pub bf_rmw_band_top: i64,
    /// spill band 的顶（排在 sret/物化区之下、向量区之上；见 `spill_off`）
    pub spill_band_top: i64,
    /// 晚分配带的顶（变量区正下方，`-(hdr + var_bytes)`；见 `late_band_bottom`）
    pub late_band_top: i64,
    /// 变量区与 sret 区能占的总字节：帧大小减去帧头、减去晚分配带、再减去帧底那摞带。
    ///
    /// 注意：这是**诊断口径**，不是硬错误上限 —— 晚分配已经不在变量区里了，它自己
    /// 有一条带（`late_band_bytes`）并从本读数里扣掉，所以本读数不再对晚分配重复计费
    /// （详见 [`LayoutMap::fits_above_bottom`]）。硬错误走 `fits_above_bottom()`，
    /// 晚分配的到位判定走 [`LayoutMap::fits_in_late_band`]。
    pub arena_room: i64,
}

impl LayoutMap {
    /// 「还没解算过任何函数」的占位表。
    ///
    /// 不能简单全零：旧代码里这些坐标都是现场从 `Codegen::frame_size`（初值 0）推算的，
    /// 于是帧还没定时时 rvalue 带算出来是 `-0 + 8 = 8`、trampoline 顶是 `-0 + 256`。
    /// 如果占位表给 0，「在解算之前就读帧底坐标」这条路径就从 8 变成 0 —— 那是行为
    /// 变更而不是搬迁。把占位表定义成「旧公式喂零镜像的结果」，这个窗口就逐字不存在。
    ///
    /// 实测确实存在三个这样的窗口：串行 TU 的**首个**函数、并行 worker 的**首个**函数
    /// （`parallel/orchestration.rs` 每 chunk 新建一个 `Codegen`）、以及 `naked` 路径在
    /// `resolve()` 之前就 `return`（`emit/functions.rs` 的 naked 分支）。
    pub const fn unresolved() -> Self {
        Self {
            temp_slots: 0,
            cs_slots: CS_POOL_SLOTS,
            hdr: FIXED_HDR,
            var_bytes: 0,
            sret_need: 0,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
            cx_band: CX_FRAME_BAND,
            vec_reserve: 0,
            tramp_reserve: TRAMP_RESERVE,
            stack_reserve: 0,
            spill_band_bytes: 0,
            late_band_bytes: 0,
            frame_size: 0,
            sret_region_base: 0,
            vec_temp_off: 0,
            variadic_reg_save_off: 0,
            static_chain_off: 0,
            rvalue_off: 8,
            sret_ptr_off: 0,
            tramp_top: TRAMP_RESERVE,
            bf_rmw_band_top: TRAMP_RESERVE,
            spill_band_top: 0,
            late_band_top: 0,
            arena_room: 0,
        }
    }

    /// 帧头的唯一算式：`16 (canary 格 + 变量区第一槽) + 8*cs_slots + 8*temp_slots`。
    /// `emit/func_setup.rs` 的静态助手与 `Codegen::frame_hdr()` 都必须走这里，
    /// 否则「变量区从哪儿开始」就又有了第二个真源。
    ///
    /// 布局（自 `%r13` 向下）：canary @ -8 → callee-saved 槽 j @ -(16+8j) →
    /// temp 槽 d @ -(16+8*cs+8d) → 变量区第一槽 @ -hdr（`-hdr` 就是它的偏移）。
    /// **`+16` 里那个 8 不是洞**：变量槽的偏移就是 `-hdr`，它占 `[-hdr, -hdr+8)`，
    /// 也就是帧头区实际字节数 = `hdr - 8`；旧口径的 `48+8t` 与本式在 cs=4 时
    /// 逐字节相等（48 = 16+32），cs 少一个就整整省 8B。
    pub const fn hdr_of(temp_slots: i64, cs_slots: i64) -> i64 {
        16 + cs_slots * CS_SAVE_SLOT_BYTES + temp_slots * 8
    }

    /// 一次解算。求和顺序与搬迁前 `emit/functions.rs` 的 `raw` 完全一致
    /// （顺序无关结果，但保持字面同形便于逐条核对）；加法仍走 `saturating_*`，
    /// 与旧代码逐字同口径（旧式全程 `saturating_add`，本函数不得改成会 overflow-check
    /// 的裸 `+`：那是 dev profile 下的不等价）。
    pub fn resolve(p: &FramePlan) -> Self {
        let hdr = Self::hdr_of(p.temp_slots, p.cs_slots);
        let spill_band_bytes = p.spill_slots.saturating_mul(SPILL_SLOT_BYTES);
        // 晚分配带的尺寸就是规划期那份预算（`Codegen::late_slot_budget`），
        // 一口 8 字节一个对象槽，与 `var_alloc_slots` 同口径。
        let late_band_bytes = p.late_slots.saturating_mul(8);
        let raw = p
            .var_bytes
            .saturating_add(hdr)
            .saturating_add(late_band_bytes)
            .saturating_add(p.sret_need)
            .saturating_add(spill_band_bytes)
            .saturating_add(p.static_chain_slot)
            .saturating_add(p.cx_band)
            .saturating_add(p.va_slots)
            .saturating_add(BOTTOM_PAD)
            .saturating_add(p.vec_reserve)
            .saturating_add(p.bf_rmw_reserve)
            .saturating_add(p.tramp_reserve)
            .saturating_add(p.stack_reserve);
        let frame_size = (raw.saturating_add(15)) & !15; // 16 字节对齐
                                                         // 变量区底下就是晚分配带，sret/物化区再排到带底下。今天之前 sret 顶是
                                                         // 「变量区（已含晚分配预扣）的底」，数值与本式相同（预算全部吃满时），
                                                         // 区别在于晚分配现在只能在自己带里动，不得再压低本锚点。
        let late_band_top = -(hdr.saturating_add(p.var_bytes));
        let sret_region_base = late_band_top.saturating_sub(late_band_bytes);
        let vec_temp_off = -frame_size + p.cx_band + p.va_slots + p.static_chain_slot + 8;
        // §5.12「不对称 2」的修复：帧底那摞带按地址递增各占自己的区间。
        // 从帧底往上依次是：cx(rvalue，包含前导 8B 间隙) → va → 静态链/sret 指针 →
        // BOTTOM_PAD → vec → bf → trampoline。每条带的消费要么从**自己的顶**往下减
        // （`bf_rmw_slot` 减 `depth*32`、`gen_trampoline` 减 `trampoline_total_size`），
        // 要么从**自己的底**往上加（`vec_temp`），所以相邻两带之间不需要富余量。
        // 旧算式把 bf 的顶定在 `vec_temp_off + vec_reserve`，于是 bf 往下减时吃止正落在
        // 向量带里 —— 今天不出事只因为两边都远大于实际用量；一旦把任何一条改成按需
        // （本轮把 cx 改成 0）剩下的缓冲就不够了，trampoline 会探到帧底以下（实测
        // `nested_func_symbol_facts.c` 的 `deep`：帧 128B，trampoline 写到 `-152(%r13)`）。
        // 带顶之和恰好等于 `raw` 里那几项，所以「带底不过帧底」与「带顶不压到变量区」
        // 两条界现在由求和式本身保证（后者还靠 [`BOTTOM_PAD`] 与 16 对齐的富余）。
        let bf_rmw_band_top = vec_temp_off + p.vec_reserve + p.bf_rmw_reserve;
        let tramp_top = bf_rmw_band_top + p.tramp_reserve;
        Self {
            temp_slots: p.temp_slots,
            cs_slots: p.cs_slots,
            hdr,
            var_bytes: p.var_bytes,
            sret_need: p.sret_need,
            va_slots: p.va_slots,
            static_chain_slot: p.static_chain_slot,
            bf_rmw_reserve: p.bf_rmw_reserve,
            cx_band: p.cx_band,
            vec_reserve: p.vec_reserve,
            tramp_reserve: p.tramp_reserve,
            stack_reserve: p.stack_reserve,
            spill_band_bytes,
            late_band_bytes,
            frame_size,
            sret_region_base,
            vec_temp_off,
            variadic_reg_save_off: -frame_size + p.cx_band,
            static_chain_off: -frame_size + p.cx_band + p.va_slots,
            rvalue_off: -frame_size + 8,
            sret_ptr_off: -frame_size + p.cx_band + p.va_slots + p.static_chain_slot,
            // trampoline 带独占 `[tramp_top - tramp_reserve, tramp_top)`：P2 把它从帧底
            // （与 CX/vec 区重叠）抬到 bf 带之上，§5.12 再把 `tramp_reserve` 自身算进
            // 带顶，于是它不再吃 cx/va/sc/vec/bf 那一段（旧算式只**摆**在上面、却没给它
            // 留高度，能容下的判据变成 `cx+va+sc+8+vec+bf ≥ trampoline 实测用量`，与
            // `tramp_reserve` 无关 —— 实测 4 个嵌套函数的 `main` 就差那 8 字节，见
            // `emit/nested.rs::gen_trampoline` 里那条 E1128）。
            tramp_top,
            bf_rmw_band_top,
            spill_band_top: sret_region_base - p.sret_need,
            late_band_top,
            // 诊断读数：帧大小减去帧头、减去晚分配带、再减去帧底那摞带。
            //
            // 这份算法仍**不是**生成期的可用额度：它只是回答「不算穿帧、但会吃掉帧底各带」
            // 的那条更早的告警线在哪（硬错误走 `fits_above_bottom`，晚分配走
            // `fits_in_late_band`）。以前它不能拿来当晚分配上限是因为要对已预扣的那份
            // 重复计费；现在晚分配住在自己的带里，本读数已经把带扣干净了，但它依旧只是
            // 诊断读数 —— 带内的到位判定不靠它（只读审计拓出的双重计费那个坑，
            // §11.4.5 与 `instructions.rs` 各踩过一次）。
            arena_room: frame_size
                .saturating_sub(hdr)
                .saturating_sub(p.stack_reserve)
                .saturating_sub(p.tramp_reserve)
                .saturating_sub(p.bf_rmw_reserve)
                .saturating_sub(p.vec_reserve)
                .saturating_sub(p.va_slots)
                .saturating_sub(p.static_chain_slot)
                .saturating_sub(BOTTOM_PAD)
                .saturating_sub(p.cx_band)
                .saturating_sub(spill_band_bytes)
                .saturating_sub(late_band_bytes),
        }
    }

    /// 第 `slot` 个 spill 槽的偏移（顶对齐 `spill_band_top` 向下生长）。
    ///
    /// 从 Phase 5.1.c 起它是 `Location::Spill` 携带的**槽序号→帧内偏移**的唯一翻译点：
    /// `mem/access.rs` 的三个消费臂（取地址 / 读 / 写）全部走这里，谁自己拿序号当偏移用，
    /// 谁就是第七个现场推算帧坐标的人（违反 §3 的 H1）。
    pub fn spill_off(&self, slot: i64) -> i64 {
        self.spill_band_top - (slot + 1) * SPILL_SLOT_BYTES
    }

    /// spill band 的底（再往下就是帧底那摞带）。
    pub fn spill_band_bottom(&self) -> i64 {
        self.spill_band_top.saturating_sub(self.spill_band_bytes)
    }

    /// 晚分配带的底（再往下就是 sret/物化区的顶）。
    pub fn late_band_bottom(&self) -> i64 {
        self.late_band_top.saturating_sub(self.late_band_bytes)
    }

    /// 发射期晚分配的**到位判据**：一个新对象的最低字节必须在带内。
    ///
    /// 带为空（本函数预算 0 槽，或帧还没解算）时不得启用：那会把「一个对象也没得
    /// 分配」误报成越界。这就是 §11.23 步 3 拿掉 `min_used` 夹逼后补上的那条
    /// 等价但不靠巧的不变式：以前「晚分配不得压穿 sret」靠的是现场夹逼 + 锚点
    /// 单调下移（两个都在推 `sret_region_base`，而 `sub rsp` 早已发射），现在是「只能在
    /// 自己带里动」——上限就是带，而带的尺寸是解算输入。
    pub fn fits_in_late_band(&self, low_off: i64) -> bool {
        if self.late_band_bytes <= 0 || self.frame_size <= 0 {
            return true;
        }
        low_off >= self.late_band_bottom()
    }

    /// 生成期新增分配能不能落在这个位置（**sret/物化区**那一侧；发射期对象走
    /// [`LayoutMap::fits_in_late_band`]，不再仍靠本判据）。这就是 Phase 5.1.d 欠的那条账：
    /// `check_frame_budget` 以前只判帧底，于是 sret 游标可以从上面压穿整个 spill band
    /// —— band 是解算期定下的静态坐标，而 `Codegen::sret_region_base` 在生成期会被单调
    /// 下压，两者之间过去没有任何判据（`arena_room` 那条更早的告警线也零读者）。
    ///
    /// 关键：**band 为空时不得启用第二条判据**。拿它当无条件约束会把合法代码报成
    /// E1128（这个坑 §11.4.5 与 `instructions.rs` 里的双重计费各踩过一次）。只有真给了
    /// 一个非零 band，「不得越进去」才成为不变式。
    ///
    /// 非零 band 时要求的不是「不落在带里」而是「停在带顶之上」：sret 游标只往下走，
    /// 而帧底那摞带（向量/trampoline/静态链）不是分配区——所以任何一个落到带顶以下的
    /// 新家，要么正在盖带，要么已经跨过带（一步跳过去的巨大槽也算）。
    pub fn allows_late_allocation(&self, low_off: i64) -> bool {
        if !self.fits_above_bottom(low_off) {
            return false;
        }
        if self.spill_band_bytes <= 0 {
            return true;
        }
        low_off >= self.spill_band_top
    }

    /// 帧底复数 / ≤16B 聚合 rvalue 物化槽（实部）。
    ///
    /// 带为 0 = 本函数没有任何 complex/imaginary 类型、也没有 ≤16B 聚合 rvalue 站点
    /// （判据见 [`cx_band_reserve`] 的输入扫描）。真走到这里说明扫描漏了一种来源，
    /// 而此时算出的偏移正压在变参保存区/向量带上：宁可当场报错，也不要静默写坏
    /// 相邻区域（与 [`LayoutMap::vec_temp`] 同一条 fail-closed 纪律）。
    /// `unresolved()` 占位表的 `frame_size` 为 0，不参与本判据。
    pub fn cx_rvalue_off(&self) -> i64 {
        assert!(
            self.cx_band > 0 || self.frame_size == 0,
            "complex/聚合 rvalue 物化槽被使用，但本函数没有那条带（cx_band=0, frame_size={}）",
            self.frame_size
        );
        self.rvalue_off
    }

    /// 向量临时槽 `slot`（尺寸 `vec_size`）的偏移。与 `Codegen::vector_temp_base`
    /// 同一条算式，收在这里是为了「帧的坐标只有一个出处」。
    pub fn vec_temp(&self, slot: i64, vec_size: usize) -> i64 {
        // 带宽 0 = 本函数没有任何向量类型（判据见 [`vec_band_reserve`]）。真走到这里
        // 说明「扫描漏了一种向量来源」，此时算出来的偏移会落在帧外：宁可当场报错，
        // 也不要静默写坏相邻帧。`unresolved()` 占位表 frame_size 为 0，不参与本判据。
        assert!(
            self.vec_reserve > 0 || self.frame_size == 0,
            "vec_temp 被调用但本函数没有向量临时带（frame_size={}, vec_size={}）",
            self.frame_size,
            vec_size
        );
        let own = (vec_size.div_ceil(8) as i64).saturating_mul(8);
        self.vec_temp_off + slot * own
    }

    /// 向量临时槽带在步长 `vec_size` 下能容纳的槽数。
    ///
    /// 与 [`Self::vec_temp`] **同一把尺子**（都是 `round8(本次尺寸)`），消费端不得自己
    /// 再除一遍 `reserve / size` —— 那就是同一个事实的两个出处。
    pub fn vec_band_slots(&self, vec_size: usize) -> usize {
        let own = (vec_size.div_ceil(8) as i64).saturating_mul(8);
        let step = own.max(1);
        (self.vec_reserve / step).max(0) as usize
    }

    /// Bitfield RMW slot offset: depth selects the layer, slot_idx the slot in that layer.
    /// Each layer = 3 x 8B slots (24B, aligned to 32B). Unified for write / inc-dec paths.
    ///
    /// 带为 0 且帧已解算 ⇒ 本函数没有写位域 / 位域自增的站点（判据见
    /// [`bf_rmw_band_reserve`]），走到这里就是「扫描漏了一种形态」。**不再退回
    /// `-8/-16/-24`**：那三个格与金丝雀、callee-saved 保存区同格（`dispatch.rs` 本带
    /// 注释记的就是这个坑），静默退回去等于把少扫一种形态变成毁掉调用者的寄存器。
    /// 带不够深同理：最深处那一槽的最低字节必须在带内，否则就是链深度算少了，
    /// 宁可报错也不要静默往带外写（实测 F3 那类 9 层链在旧的恒 64B 带下就是这么跑的）。
    pub fn bf_rmw_slot(&self, depth: i64, slot_idx: i64) -> i32 {
        let off = self.bf_rmw_band_top - depth * BF_RMW_SLOT_BYTES - slot_idx * 8;
        assert!(
            self.bf_rmw_reserve > 0 || self.frame_size == 0,
            "位域 RMW 槽被使用，但本函数没有位域 RMW 带（bf_rmw_reserve=0, frame_size={}, \
             depth={depth}）—— 扫描漏了一种写位域/位域自增的形态，判据见 \
             `emit/func_setup.rs::Codegen::bf_rmw_chain_depth`",
            self.frame_size
        );
        assert!(
            self.bf_rmw_reserve <= 0
                || off <= self.bf_rmw_band_top
                && off >= self.bf_rmw_band_top - self.bf_rmw_reserve,
            "位域 RMW 槽探出本带：depth={depth} slot={slot_idx} 落在 {off}，而带区间是 \
             [{}, {}]（reserve={}B）—— 链深度算少了",
            self.bf_rmw_band_top - self.bf_rmw_reserve,
            self.bf_rmw_band_top,
            self.bf_rmw_reserve,
        );
        off as i32
    }

    /// 表达式第 `depth` 层溅出槽的偏移（帧头 temp 带内）。
    /// 搬迁前有 `emit/expr/dispatch.rs` 与 `emit/expr/binary.rs` 两处直接写
    /// `-(48 + depth*8)`（裸 48，且是 **i32** 乘法）。§5.42 起帧头不再恒 48：
    /// 裸 48 拆成 `16 + 8*cs_slots`，所以本函数必须收 cs_slots；发射期消费点
    /// 一律走 [`LayoutMap::temp_slot`]。cs=4 时与旧产物逐字节相同。
    ///
    /// **返回类型必须是 `i32`，这不是可选的实现细节**：两个消费点都做
    /// `self.emit(&slot_off.to_le_bytes())`（disp32 分支），`i64` 的 `to_le_bytes()`
    /// 是 8 字节 —— 数值相同、**发射宽度不同**，会把紧随其后的指令流整体推偏
    /// （objdump 表现为 `(bad)`，执行到那里 SIGILL）。独立复核拓到了这一条，
    /// 而三档产物哈希抓不到它（语料里没有 `expr_depth >= 10` 的帧槽回退）。
    /// 下面那条宽度契约用例就是守这条线的。
    pub const fn temp_slot_off(depth: i64, cs_slots: i64) -> i32 {
        -(16 + cs_slots * CS_SAVE_SLOT_BYTES + depth * 8) as i32
    }

    /// 本帧的 temp 槽偏移（`self.cs_slots` 是解算定下的布局事实，发射期只读它）。
    pub const fn temp_slot(&self, depth: i64) -> i32 {
        Self::temp_slot_off(depth, self.cs_slots)
    }

    /// 帧底基址（`-frame_size`）。KASAN 毒化整个帧、VLA 动态栈基恢复都以它为轴。
    /// 搬迁前这两处各自写 `-self.frame_size`，现在读同一份表。
    pub fn frame_base(&self) -> i64 {
        -self.frame_size
    }

    /// 帧是否大到放得下一个金丝雀槽。序言存它、尾声验它，两处守卫共用本判据，
    /// 以免「存了但没人验」或「验了但帧不够」这类两侧不一致。
    pub fn canary_fits(&self) -> bool {
        self.frame_size >= -CANARY_OFF
    }

    /// 生成期越界硬判据：任何新增家的**最低字节地址**都不得越过帧底。
    ///
    /// 为什么只收一个标量而不是「变量区字节 + sret 区字节」（独立复核拓出的第二条）：
    /// 两个区各自住在解算定下的带里（变量区、晚分配带、sret/物化区、spill 带、帧底那摞带），
    /// 相加就是把同一段字节数两次：判据会比它声称保护的那条不变量更紧，会把合法代码
    /// 报成溢出。真正的不变量只有一条：帧已在 `sub rsp` 时发射出去，谁都越不过帧底。
    /// 而两个区彼此不重叠不再靠生成期现场夹逼保障（§11.23 步 3 删了「锚点单调下移」），
    /// 而是靠每条带的边界都来自同一个 `resolve()`。
    ///
    /// 也不拿更紧的 `arena_room`（刨掉帧底各带的那个读数）当上限：它是诊断读数而不是
    /// 可用额度，拿它当上限会把合法分配误判成溢出（§11.4.5 踩过一次）。发射期晚分配
    /// 的到位判据在 [`LayoutMap::fits_in_late_band`]。
    pub fn fits_above_bottom(&self, low_off: i64) -> bool {
        low_off >= self.frame_base()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 典型一帧：搬前搬后必须给出**完全相同**的坐标。这里的期望值是手工按
    /// 搬迁前 `emit/functions.rs:627-642` 的算式逐项算出来的。
    #[test]
    fn resolve_reproduces_the_legacy_offsets_bit_for_bit() {
        // temp_slots=3, var_bytes=80, sret_need=64, 变参: va_slots=176, 嵌套: sc=8
        let p = FramePlan {
            temp_slots: 3,
            cs_slots: 4,
            var_bytes: 80,
            // 晚分配带传 0：本用例测的就是「带不存在时全部坐标逐字节等于搬迁前」
            late_slots: 0,
            sret_need: 64,
            va_slots: 48 + 128,
            static_chain_slot: 8,
            bf_rmw_reserve: BF_RMW_RESERVE,
            cx_band: CX_FRAME_BAND,
            vec_reserve: VEC_RESERVE,
            tramp_reserve: TRAMP_RESERVE,
            stack_reserve: 176,
            spill_slots: 0,
        };
        let l = LayoutMap::resolve(&p);
        // §5.42：帧头 = 16(canary 格 + 变量区第一槽) + 8*cs + 8*temp；cs=4 时
        // 与旧口径的 48+8t 逐字节相等（48 = 16+32），所以本用例仍然逐字节等于搬迁前。
        // cs 个数是计划位图的 popcount，本用例给满 4 个。
        let hdr = 48 + 3 * 8; // 72
        let raw = 80 + hdr + 64 + 0 + 8 + CX_FRAME_BAND + 176 + BOTTOM_PAD + 512
            + BF_RMW_RESERVE
            + 256
            + 176;
        let frame_size = (raw + 15) & !15;
        assert_eq!(l.hdr, hdr);
        assert_eq!(l.frame_size, frame_size);
        assert_eq!(l.sret_region_base, -(hdr + 80));
        assert_eq!(l.vec_temp_off, -frame_size + CX_FRAME_BAND + 176 + 8 + 8);
        assert_eq!(l.variadic_reg_save_off, -frame_size + CX_FRAME_BAND);
        assert_eq!(
            l.sret_ptr_off,
            -frame_size + CX_FRAME_BAND + 176 + 8,
            "搬迁前 `functions.rs:822` 的 sret 指针槽"
        );
        assert_eq!(l.rvalue_off, -frame_size + 8);
        assert_eq!(l.spill_band_bytes, 0, "Phase 2 不得凭空长出 spill 带");
        // 额度：帧大小 - 帧头 - 帧底那摞带 = 变量区 + sret 区 + 取整富余
        assert_eq!(l.arena_room, 80 + 64 + (frame_size - raw));
    }

    /// 位域 RMW 带按**链深度**给，不再恒 64B（§5.12）：0 层 ⇒ 0B，1 层 ⇒ 32B，
    /// 9 层（`bitfield_rmw_deep_nest.c` 那条 `s.a = (s.b = (s.c = …))` 链）⇒ 288B。
    /// 旧的恒 64B 只容 2 层，即那个夹具一直在往带外写 224B。
    #[test]
    fn bf_rmw_band_is_scaled_by_chain_depth() {
        assert_eq!(bf_rmw_band_reserve(0), 0, "没有写位域的函数不得预留 BF RMW 带");
        assert_eq!(bf_rmw_band_reserve(1), BF_RMW_SLOT_BYTES);
        assert_eq!(bf_rmw_band_reserve(9), 9 * BF_RMW_SLOT_BYTES);
        // 带必须给每条链留出全部三层槽（slot 0/1/2），最深槽的最低字节仍在带内。
        let mut l = LayoutMap::unresolved();
        l.frame_size = 4096;
        l.bf_rmw_reserve = bf_rmw_band_reserve(9);
        l.bf_rmw_band_top = -128;
        for depth in 0..9 {
            for slot in 0..3 {
                let off = l.bf_rmw_slot(depth, slot) as i64;
                assert!(
                    off > l.bf_rmw_band_top - l.bf_rmw_reserve
                        && off <= l.bf_rmw_band_top,
                    "depth={depth} slot={slot} 落在带外：{off} vs 带 [{}, {}]",
                    l.bf_rmw_band_top - l.bf_rmw_reserve,
                    l.bf_rmw_band_top
                );
            }
        }
    }

    /// 链深度算少了 ⇒ 必须当场报错，而不是静默往带外写（那是旧行为的实质缺陷）。
    #[test]
    #[should_panic(expected = "探出本带")]
    fn bf_rmw_slot_rejects_a_chain_deeper_than_the_band() {
        let mut l = LayoutMap::unresolved();
        l.frame_size = 4096;
        l.bf_rmw_reserve = bf_rmw_band_reserve(2);
        l.bf_rmw_band_top = -128;
        let _ = l.bf_rmw_slot(5, 0);
    }

    /// 没有带却用了带：报死，不退回 `-8/-16/-24`（那三个格与金丝雀、callee-saved 同格）。
    #[test]
    #[should_panic(expected = "没有位域 RMW 带")]
    fn bf_rmw_slot_rejects_a_missing_band() {
        let mut l = LayoutMap::unresolved();
        l.frame_size = 4096;
        l.bf_rmw_reserve = 0;
        let _ = l.bf_rmw_slot(0, 0);
    }

    /// 帧底那摞带各占自己的区间：vec 与 bf 不再同段，trampoline 也不吃别人的高度。
    /// 这是 §5.12「不对称 2」的正面形式，也是一切按需带能成立的前提。
    #[test]
    fn bottom_bands_own_disjoint_ranges() {
        let p = FramePlan {
            temp_slots: 2,
            cs_slots: 4,
            var_bytes: 16,
            late_slots: 0,
            sret_need: 0,
            va_slots: 48 + 128,
            static_chain_slot: 8,
            bf_rmw_reserve: bf_rmw_band_reserve(3),
            cx_band: CX_FRAME_BAND,
            vec_reserve: VEC_RESERVE,
            tramp_reserve: 2 * 32,
            stack_reserve: 0,
            spill_slots: 0,
        };
        let l = LayoutMap::resolve(&p);
        // 自帧底：cx 带 → va → sc → BOTTOM_PAD → vec → bf → tramp，逐段不重叠。
        assert_eq!(l.variadic_reg_save_off, l.rvalue_off - BOTTOM_PAD + CX_FRAME_BAND);
        assert_eq!(l.static_chain_off, l.variadic_reg_save_off + p.va_slots);
        assert_eq!(l.sret_ptr_off, l.static_chain_off + p.static_chain_slot);
        assert_eq!(l.vec_temp_off, l.sret_ptr_off + BOTTOM_PAD);
        assert_eq!(l.bf_rmw_band_top, l.vec_temp_off + p.vec_reserve + p.bf_rmw_reserve);
        assert_eq!(l.tramp_top, l.bf_rmw_band_top + p.tramp_reserve);
        // vec 带 `[vec_temp_off, vec_temp_off+vec)` 与 bf 带 `[bf_top-bf, bf_top)` 不相交。
        assert!(l.vec_temp_off + p.vec_reserve <= l.bf_rmw_band_top - p.bf_rmw_reserve);
        // trampoline 带的底不低过帧底。
        assert!(l.tramp_top - p.tramp_reserve >= l.frame_base());
        // 带顶不压到自上而下的区（帧头 + 变量区）。
        assert!(l.tramp_top <= -(l.hdr + p.var_bytes));
    }

    /// 向量临时带的尺寸判据：单槽步长与带宽同源，且**格数恒为 `VEC_BAND_SLOTS`**。
    #[test]
    fn vec_band_reserve_keeps_slot_count_and_grows_only_for_wide_vectors() {
        // 无向量：不预留（见 `vec_band_reserve` 的理由——每帧 512B 会压穿内核栈）
        assert_eq!(vec_band_reserve(0), 0, "无向量类型的函数不得预留向量带");
        // 单槽 ≤ 16B：带宽恰好还是历史的 512（= 32 x 16），帧布局逐字节不变
        for max in [8usize, 16] {
            assert_eq!(vec_band_reserve(max), VEC_RESERVE, "max_vec_bytes={max} 不得改变带宽");
        }
        // 更宽的向量：带按步长线性拉长，使格数不掉（容量不变是硬要求：格数一掉，
        // 旧固定带下能跑的表达式的槽就会跑出带区，实测 pr93170 abort）
        for max in [32usize, 64, 256] {
            let reserve = vec_band_reserve(max);
            let mut l = LayoutMap::unresolved();
            l.vec_reserve = reserve;
            assert_eq!(
                l.vec_band_slots(max) as i64,
                VEC_BAND_SLOTS,
                "max_vec_bytes={max} 下格数必须保持 VEC_BAND_SLOTS"
            );
            assert!(reserve > VEC_RESERVE, "max_vec_bytes={max} 下旧 512B 带不够用");
        }
    }

    /// `vec_temp` 的槽定位仍按调用方尺寸走：锁定历史算式，防止谁把「按函数统一
    /// 步长」悄悄装回来（那会改变现有宽向量函数的帧布局）。
    #[test]
    fn vec_temp_steps_by_the_requested_size() {
        let mut l = LayoutMap::unresolved();
        l.vec_temp_off = -1000;
        assert_eq!(l.vec_temp(1, 2), -1000 + 8, "2B 步长按 round8 占一格");
        assert_eq!(l.vec_temp(2, 16), -1000 + 32);
        assert_eq!(l.vec_temp(1, 300), -1000 + 304, "尺寸向上 8 对齐后乘槽号");
        assert_eq!(l.vec_temp(3, 256), -1000 + 768);
        // 格数反推与 `vec_temp` 同一把尺子
        l.vec_reserve = 2048;
        assert_eq!(l.vec_band_slots(256), 8);
        assert_eq!(l.vec_band_slots(16), 128);
    }

    /// spill band 为 0 时**一切坐标都不动** —— 这是 Phase 2「零行为 diff」的代数形式。
    #[test]
    fn zero_spill_leaves_the_frame_untouched() {
        let base = FramePlan {
            temp_slots: 2,
            cs_slots: 4,
            var_bytes: 16,
            late_slots: 0,
            sret_need: 0,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
            cx_band: CX_FRAME_BAND,
            vec_reserve: VEC_RESERVE,
            tramp_reserve: TRAMP_RESERVE,
            stack_reserve: 48,
            spill_slots: 0,
        };
        let l0 = LayoutMap::resolve(&base);
        assert_eq!(l0.frame_size % 16, 0);
        assert_eq!(l0.spill_band_bytes, 0);
        // 变量区 16B 再加 8B 的 16 对齐富余；§5.12 补进求和的 [`BOTTOM_PAD`] 8B 已从
        // 本读数里扣掉（952 求和 → 960 对齐）。§5.42 的帧头在 cs=4 时与旧口径同值。
        assert_eq!(l0.arena_room, 24);
        // 同一个 plan 解两次必得同一份表（解算必须是纯函数）
        assert_eq!(LayoutMap::resolve(&base).frame_size, l0.frame_size);
        assert_eq!(LayoutMap::resolve(&base).vec_temp_off, l0.vec_temp_off);
    }

    /// 一旦有 spill 槽，帧大小与 arena_room 的账必须仍然自洽：
    /// spill band 吃的是**自己的**预留，不许挤占变量区/sret 区的额度。
    #[test]
    fn spill_band_is_its_own_reservation() {
        let mut p = FramePlan {
            temp_slots: 2,
            cs_slots: 4,
            var_bytes: 16,
            late_slots: 0,
            sret_need: 32,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
            cx_band: CX_FRAME_BAND,
            vec_reserve: VEC_RESERVE,
            tramp_reserve: TRAMP_RESERVE,
            stack_reserve: 48,
            spill_slots: 0,
        };
        let no_spill = LayoutMap::resolve(&p);
        p.spill_slots = 4;
        let with_spill = LayoutMap::resolve(&p);
        assert_eq!(with_spill.spill_band_bytes, 32);
        assert_eq!(with_spill.arena_room, no_spill.arena_room, "spill 不许动变量区的额度");
        assert_eq!(with_spill.sret_region_base, no_spill.sret_region_base);
        assert_eq!(with_spill.spill_band_top, no_spill.sret_region_base - 32);
        // spill 槽排在 sret 区之下，且第 0 个紧贴带顶
        assert_eq!(with_spill.spill_off(0), with_spill.spill_band_top - 8);
        assert_eq!(with_spill.spill_off(3), with_spill.spill_band_top - 32);
        assert!(with_spill.frame_size >= no_spill.frame_size + 32 - 15);
    }

    /// Phase 5.1.d 的**零 diff 半边**：band 为 0 时新判据必须与今天那条帧底比较一模一样。
    ///
    /// 为什么这条比「新判据真的拦得住」更容易弄错：band=0 时 `spill_band_bottom` 正好落在
    /// `spill_band_top`（零高位的线），而今天合法的晚分配本来就能过这条线（吃帧底各带的富余）。
    /// 若把它当成无条件约束，现有语料会当场报出一堆 E1128。这里在**一堆位置**上逐个对拍两条
    /// 判据，把「退化」钉成可执行事实而不是口头承诺。
    #[test]
    fn an_empty_band_leaves_the_budget_judgment_untouched() {
        let base = FramePlan {
            temp_slots: 2,
            cs_slots: 4,
            var_bytes: 16,
            late_slots: 0,
            sret_need: 32,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
            cx_band: CX_FRAME_BAND,
            vec_reserve: VEC_RESERVE,
            tramp_reserve: TRAMP_RESERVE,
            stack_reserve: 48,
            spill_slots: 0,
        };
        let l = LayoutMap::resolve(&base);
        assert_eq!(l.spill_band_bytes, 0, "本用例测的就是空 band");
        // 从帧底以下一路扫到变量区：每一个位置两条判据必须同答案。
        for probe in (l.frame_base() - 64)..0 {
            assert_eq!(
                l.allows_late_allocation(probe),
                l.fits_above_bottom(probe),
                "probe={probe}：空 band 时新判据改变了旧行为（这会凭空报出 E1128）"
            );
        }
        // 而且零高度的 band 就贴在 sret 区底下：上面那些落在带以下的合法位置，
        // 在旧表上确实存在（否则本用例只在「无人可达」的区间上对拍）。
        assert!(l.fits_above_bottom(l.spill_band_top - 8), "这条位置今天合法，必须仍然合法");
        assert!(l.allows_late_allocation(l.spill_band_top - 8));
    }

    /// Phase 5.1.d 的**新约束半边**：真给了非零 spill band，**sret/物化区那一侧的
    /// 生成期增长**就不能压穿它。（从 §11.23 步 3 起，发射期对象已搬到自己的晚分配带里，
    /// 它们的到位判据是 `fits_in_late_band`；本判据仍然负责 spill band 与 sret 游标之间
    /// 那条线，所以名字里的 “late allocation” 今天指的是“生成期新分配”而不是晚分配带。）
    ///
    /// 判据是「新分配的最低字节仍得停在带底之上」：变量区水位与 sret 游标都是单调下压的，
    /// 所以这一步拒得早（碰到就报），而不是等到整体已经盖到带底下才发现；而带以下
    /// 「本来还可以吃帧底各带的富余」这半边只在 **band=0** 时保留（上面那条用例），因为
    /// 一旦分配器真拿到了带，那块就是它的私有空间，被晚分配盖掉就是静默错值。
    #[test]
    fn late_allocation_must_not_cross_a_nonempty_band() {
        let base = FramePlan {
            temp_slots: 2,
            cs_slots: 4,
            var_bytes: 16,
            late_slots: 0,
            sret_need: 32,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
            cx_band: CX_FRAME_BAND,
            vec_reserve: VEC_RESERVE,
            tramp_reserve: TRAMP_RESERVE,
            stack_reserve: 48,
            spill_slots: 4,
        };
        let l = LayoutMap::resolve(&base);
        assert_eq!(l.spill_band_bytes, 32);
        // 带顶之上（sret 区里）合法
        assert!(l.allows_late_allocation(l.spill_band_top + 8));
        // 带里每一个槽的位置都被拒
        for slot in 0..4 {
            let off = l.spill_off(slot);
            assert!(
                !l.allows_late_allocation(off),
                "第 {slot} 个 spill 槽的位置 {off} 不得允许晚分配落进去（band 顶 {} 底 {}）",
                l.spill_band_top,
                l.spill_band_bottom()
            );
        }
        // 带底以下也算拒：变量区与 sret 区的水位是**单调下压**的，能落到带底下就
        // 说明中间某一步已经穿过带。所以「低界」判据是充分而不是多余的那一条。
        assert!(!l.allows_late_allocation(l.spill_band_bottom() - 8));
        // 帧底以下仍然被拒（旧判据不得被新判据取代）
        assert!(!l.allows_late_allocation(l.frame_base() - 1));
    }

    /// 硬错误只拦「真越帧底」：新家的最低字节地址低于 `-frame_size`。
    ///
    /// 中间那段（超过诊断读数 `arena_room`、但仍在帧内）是**今天就存在的风险**：
    /// 晚分配会吃掉帧底各带的富余。把它当错误报就是新增行为（Phase 2 的判据是
    /// 产物逐字节不变），所以这里用一条用例把它钉住：这种情况**不得**报错。
    #[test]
    fn hard_bound_rejects_only_true_frame_overrun() {
        let p = FramePlan {
            temp_slots: 2, // hdr = 16 + 32 + 16 = 64（§5.42）
            cs_slots: 4,
            var_bytes: 32,
            late_slots: 0,
            sret_need: 16,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
            cx_band: CX_FRAME_BAND,
            vec_reserve: VEC_RESERVE,
            tramp_reserve: TRAMP_RESERVE,
            stack_reserve: 48,
            spill_slots: 0,
        };
        let l = LayoutMap::resolve(&p);
        // raw = 32+64+16+48+512+256+48 + BOTTOM_PAD(8) = 984，16 对齐后 992。
        // §5.42 的帧头在 cs=4 时与旧口径（48+8t）同值，本用例逐字节等于搬迁前。
        assert_eq!(l.frame_size, 992, "先钉住当前公式");
        assert_eq!(l.arena_room, 56, "诊断读数 = 规划期变量区 + sret 区 + 16 对齐富余");
        assert_eq!(l.frame_base(), -992, "帧底");

        // 变量区与 sret 区各自吃帧底富余（今天合法）：不得报错
        assert!(l.fits_above_bottom(l.frame_base() + 276));
        assert!(l.fits_above_bottom(l.frame_base()), "恰好落在帧底算合法");
        // 真越界：低一个字节都不行
        assert!(!l.fits_above_bottom(l.frame_base() - 1));
        // **不得把两个区的字节相加**：那会把夹逼过的 `used` 里的 sret 深度再数一次。
        // 两边同一个偏移以两种算法写出（读数推与直接算），相等就是 `arena_room` 没重复计费。
        assert!(
            l.fits_above_bottom(-(l.arena_room + 800)) == l.fits_above_bottom(-856),
            "arena_room 必须是 56：-(56+800) 与 -856 是同一个位置"
        );
    }

    /// 占位表必须等于「旧公式喂零镜像」的结果，否则「解算前就读帧底坐标」
    /// 的那三个窗口（串行首函数 / 并行 worker 首函数 / `naked` 早返）会从
    /// 8 静默变成 0。这条用例就是那个窗口的反证。
    #[test]
    fn unresolved_placeholder_matches_the_legacy_zero_state() {
        let u = LayoutMap::unresolved();
        assert_eq!(u.frame_size, 0);
        assert_eq!(u.rvalue_off, -u.frame_size + 8, "旧式 `-self.frame_size + 8` 在帧未定时给 8");
        assert_eq!(u.tramp_top, -u.frame_size + TRAMP_RESERVE);
        assert_eq!(u.hdr, FIXED_HDR, "占位表保留旧口径 48（活路径一律走 hdr_of）");
        assert_eq!(u.vec_temp_off, 0, "与 Codegen::vec_temp_off 的初值一致");
        assert_eq!(u.sret_region_base, 0, "与 compile_func 入口的复位一致");
    }

    /// **发射宽度契约**：`slot_off` 会被消费点直接 `to_le_bytes()` 写进 disp32 位。
    /// 旧代码是 `i32`，所以宽度是 4 字节；本函数要是返 `i64`，数值不变但会多发
    /// 4 个 `0xff`，把后面的指令推偏（实测 objdump 报 `(bad)`、运行 SIGILL）。
    /// 这类「只改类型不改数值」的搬迁错，逐行 diff 与产物哈希（语料无覆盖）都抓不到，
    /// 所以把契约写成断言而不是注释。
    #[test]
    fn temp_slot_off_keeps_the_disp32_emit_width() {
        // 边界：|off| < 128 走 disp8，否则走 disp32（消费点的分岔）。
        // cs_slots=4 时与旧产物逐字节相同（旧式裸 48 = 16+32 同值），消洞只动变量区顶。
        assert_eq!(LayoutMap::temp_slot_off(9, 4), -120); // 仍在 disp8 范围内
        assert_eq!(LayoutMap::temp_slot_off(10, 4), -128); // 刚越界，走 disp32
        assert_eq!(LayoutMap::temp_slot_off(14, 4), -160);
        // §5.42：cs 少一个，temp 槽整条带上移 8B（变量区随之上移，不重叠）
        assert_eq!(LayoutMap::temp_slot_off(0, 0), -16);
        assert_eq!(LayoutMap::temp_slot_off(0, 1), -24);
        assert_eq!(LayoutMap::temp_slot_off(0, 4), -48);
        // 发射宽度必须是 4 字节（i32），否则 disp32 位置会多吃 4 字节
        assert_eq!(
            LayoutMap::temp_slot_off(10, 4).to_le_bytes().len(),
            4,
            "消费点用 `&slot_off.to_le_bytes()` 直发 disp32，宽度只能是 4 字节"
        );
    }

    /// 帧头算式只有一个出处。
    #[test]
    fn hdr_is_a_single_formula() {
        // §5.42：canary 格 + 变量区第一槽 + 计划内的 cs 槽 + temp 槽。
        assert_eq!(LayoutMap::hdr_of(0, 0), 16, "无 cs 无 temp：canary 格 + 变量区第一槽");
        assert_eq!(LayoutMap::hdr_of(0, 4), 48, "cs=4 时与旧口径 48 同值");
        assert_eq!(LayoutMap::hdr_of(2, 4), 64);
        let p = FramePlan {
            temp_slots: 7,
            cs_slots: 4,
            var_bytes: 0,
            late_slots: 0,
            sret_need: 0,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
            cx_band: CX_FRAME_BAND,
            vec_reserve: VEC_RESERVE,
            tramp_reserve: TRAMP_RESERVE,
            stack_reserve: 48,
            spill_slots: 0,
        };
        assert_eq!(LayoutMap::resolve(&p).hdr, LayoutMap::hdr_of(7, 4));
    }

    /// 发射期对象不再挤在变量区里了，它自己的那条带就是它的额度：带的顶贴着
    /// 变量区的底，带的底贴着 sret/物化区的顶，帧大小恰好多出一个 `late_band_bytes`。
    ///
    /// 本用例同时钉住三件只有拆开才看得清的事：
    /// 1. **带是解算输入而不是事后发明**：`late_slots` 从 0 变 2，`frame_size` 多 16、
    ///    `sret_region_base` 下移 16，而变量区里的坐标（`hdr`）一个不动 ——
    ///    发射期对象不得再像旧写法那样把变量区水位压低从而盖到 sret 头上；
    /// 2. **到位判据不靠巧**：带内最后一槽过，超出带底一槽就拒；
    /// 3. **带为 0 时不得启用本判据**（一个发射期对象也没有的函数占全语料绝大多数，
    ///    拿零高位的线当约束会把合法代码报成 E1133 —— 那个坑 §11.4.5 踩过一次）。
    #[test]
    fn late_band_is_a_layout_input_not_an_afterthought() {
        let mk = |late_slots: i64| {
            LayoutMap::resolve(&FramePlan {
                temp_slots: 2,
                cs_slots: 4,
                var_bytes: 64,
                late_slots,
                sret_need: 32,
                va_slots: 0,
                static_chain_slot: 0,
                bf_rmw_reserve: 0,
                cx_band: CX_FRAME_BAND,
                vec_reserve: VEC_RESERVE,
                tramp_reserve: TRAMP_RESERVE,
                stack_reserve: 48,
                spill_slots: 0,
            })
        };
        let no_band = mk(0);
        let band = mk(2);
        let hdr = LayoutMap::hdr_of(2, 4);
        // 1. 坐标关系：顶 = -(hdr + var_bytes)，底 = 顶 - 带字节 = sret 区的顶
        assert_eq!(no_band.late_band_bytes, 0, "没给预算就没有带");
        assert_eq!(band.late_band_top, -(hdr + 64));
        assert_eq!(band.late_band_bytes, 16);
        assert_eq!(band.late_band_bottom(), band.late_band_top - 16);
        assert_eq!(band.sret_region_base, band.late_band_bottom(), "sret 区必须贴在带底");
        // 帧尺寸只多了带本身那 16 字节（16 对齐 ⇒ 整数值可预期）
        assert_eq!(band.frame_size, no_band.frame_size + 16, "带是求和里的一项");
        assert_eq!(band.hdr, no_band.hdr, "帧头不动（它只由 temp_slots 决定）");
        // 带吃的是**自己的**预留：变量区与 sret 区的额度不动（与 `spill_band_is_its_own_
        // reservation` 同一个口径）。诊断读数 `arena_room` 因此必须相等 —— 它已经扣掉了带，
        // 而帧大小恰好多出一个带：两边相互抵消。这条等式同时反证「把带当重复计费」那个坑
        // 没回来（拿 arena_room 当晚分配上限就会当场报假溢出）。
        assert_eq!(band.arena_room, no_band.arena_room, "晚分配不许动变量区/sret 区的额度");
        // sret/物化区的顶被带顶推下去整一条带的宽度（这就是「不再压穿」的反面：
        // 两者彼此让开是解算定下的，不是生成期现场夹出来的）
        assert_eq!(band.sret_region_base, no_band.sret_region_base - 16);
        // 2. 到位判据：两个 8 字节对象恰好填满带；第三个就出带
        assert!(band.fits_in_late_band(band.late_band_top - 8), "带内第一槽");
        assert!(
            band.fits_in_late_band(band.late_band_bottom()),
            "带内最后一槽的最低字节恰在带底，必须允许"
        );
        assert!(!band.fits_in_late_band(band.late_band_bottom() - 8), "超出带底一槽就拒");
        // 上侧不需要判据，而是**构造性质**：发射点的游标从 `late_band_top - slots*8`
        // 起、只单调往下走（见 `register_var` 的晚路径），所以任何对象的最低字节最多
        // 也就是 `late_band_top - 8`，永远不会跑到带顶以上。把这条写成判据反而会把
        // 「多槽对象从带顶往下排」的正确位置误拒（16 字节对象的顶就是 `top - 16`）。
        assert!(band.fits_in_late_band(band.late_band_top - 8), "顶格的一个槽合法");
        assert!(
            band.fits_in_late_band(band.late_band_top - 16),
            "带顶起的一个 16 字节对象（最低字节 = top-16）也必须合法"
        );
        // 3. 带为 0 时不得报错（哪怕位置在帧底以下）
        assert!(no_band.fits_in_late_band(-100000), "空带时本判据不启用");
        assert!(
            LayoutMap::unresolved().fits_in_late_band(-100000),
            "帧还没解算时也不得启用（规划期登记会走到这里）"
        );
    }
}
