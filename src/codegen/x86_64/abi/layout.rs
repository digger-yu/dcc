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
/// 向量临时槽区：512 字节（32 个 16B 槽）。嵌套向量 binop 每层要更深的操作数槽，
/// 128 字节只够 3 层，深链式表达式会溢出到变量区（pr23135）。
pub const VEC_RESERVE: i64 = 512;
/// 嵌套函数 trampoline 空间：256 字节（最多 8 个，每个 32 字节）。实际代码在生成期按需写入。
pub const TRAMP_RESERVE: i64 = 256;
/// 位域 RMW 临时槽：32 字节（2 级嵌套，每级 16 字节）。
pub const BF_RMW_RESERVE: i64 = 32;
/// 帧头里固定不动的部分：saved r13(8) + ret addr(8) + callee-saved 保存区(32)。
pub const FIXED_HDR: i64 = 48;
/// 栈顶金丝雀槽（`%r13 - 8`）。发射点手写字节 `mov [r13-8], rax` 与序言/尾声
/// 两处 `frame_size >= 8` 的守卫必须共用这个数：前者定位置，后者定「帧里放得下吗」。
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
    /// 晚分配带的槽数（§11.23 步 3）。它就是 `reserve_late_var_slots()` 算出的那份
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
    /// 向量临时带
    pub vec_reserve: i64,
    /// trampoline 带
    pub tramp_reserve: i64,
    /// 红区/向量 scratch（`stack_reserve_bytes()`）
    pub stack_reserve: i64,
    /// spill band 的槽数（Phase 5.1.b）：尺寸从已交付的 `Location::Spill` 家导出，
    /// 与其他带一样**参与**帧大小求和（输出在 `spill_band_bytes`）。
    pub spill_slots: i64,
}

/// 解算的输出：一帧里所有带的边界与大小，一次算完，此后无人再「现场推算」。
///
/// 全部偏移都以 `%r13` 为基址、向帧底为负，与搬迁前的算式逐字节等值。
#[derive(Debug, Clone, Copy)]
pub struct LayoutMap {
    pub temp_slots: i64,
    /// 帧头：`FIXED_HDR + temp_slots*8`
    pub hdr: i64,
    pub var_bytes: i64,
    pub sret_need: i64,
    pub va_slots: i64,
    pub static_chain_slot: i64,
    pub bf_rmw_reserve: i64,
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
    /// 帧底复数/聚合 rvalue 内存对的实部槽（`-frame_size + 8`）
    pub rvalue_off: i64,
    /// sret 指针（`rdi`）在帧底的专用槽：位于变参保存区之上、向量区之下
    /// （与静态链同位置，二者互斥）
    pub sret_ptr_off: i64,
    /// trampoline 区的顶（`-frame_size + tramp_reserve`）
    pub tramp_top: i64,
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
            hdr: FIXED_HDR,
            var_bytes: 0,
            sret_need: 0,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
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
            spill_band_top: 0,
            late_band_top: 0,
            arena_room: 0,
        }
    }

    /// 帧头的唯一算式（`FIXED_HDR + temp_slots*8`）。
    /// `emit/func_setup.rs` 的静态助手与 `Codegen::frame_hdr()` 都必须走这里，
    /// 否则「变量区从哪儿开始」就又有了第二个真源。
    pub const fn hdr_of(temp_slots: i64) -> i64 {
        FIXED_HDR + temp_slots * 8
    }

    /// 一次解算。求和顺序与搬迁前 `emit/functions.rs` 的 `raw` 完全一致
    /// （顺序无关结果，但保持字面同形便于逐条核对）；加法仍走 `saturating_*`，
    /// 与旧代码逐字同口径（旧式全程 `saturating_add`，本函数不得改成会 overflow-check
    /// 的裸 `+`：那是 dev profile 下的不等价）。
    pub fn resolve(p: &FramePlan) -> Self {
        let hdr = Self::hdr_of(p.temp_slots);
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
            .saturating_add(CX_FRAME_BAND)
            .saturating_add(p.va_slots)
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
        Self {
            temp_slots: p.temp_slots,
            hdr,
            var_bytes: p.var_bytes,
            sret_need: p.sret_need,
            va_slots: p.va_slots,
            static_chain_slot: p.static_chain_slot,
            bf_rmw_reserve: p.bf_rmw_reserve,
            vec_reserve: p.vec_reserve,
            tramp_reserve: p.tramp_reserve,
            stack_reserve: p.stack_reserve,
            spill_band_bytes,
            late_band_bytes,
            frame_size,
            sret_region_base,
            vec_temp_off: -frame_size
                + CX_FRAME_BAND
                + p.va_slots
                + p.static_chain_slot
                + 8,
            variadic_reg_save_off: -frame_size + CX_FRAME_BAND,
            static_chain_off: -frame_size + CX_FRAME_BAND + p.va_slots,
            rvalue_off: -frame_size + 8,
            sret_ptr_off: -frame_size + CX_FRAME_BAND + p.va_slots + p.static_chain_slot,
            tramp_top: -frame_size + p.tramp_reserve,
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
                .saturating_sub(CX_FRAME_BAND)
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

    /// 向量临时槽 `slot`（尺寸 `vec_size`）的偏移。与 `Codegen::vector_temp_base`
    /// 同一条算式，收在这里是为了「帧的坐标只有一个出处」。
    pub fn vec_temp(&self, slot: i64, vec_size: usize) -> i64 {
        let slot_bytes = vec_size.div_ceil(8) as i64 * 8;
        self.vec_temp_off + slot * slot_bytes
    }

    /// 表达式第 `depth` 层溅出槽的偏移（帧头 temp 带内）。
    ///
    /// 搬迁前有 `emit/expr/dispatch.rs` 与 `emit/expr/binary.rs` 两处直接写
    /// `-(48 + depth*8)`（裸 48，且是 **i32** 乘法），与 `FIXED_HDR`/`hdr_of` 同一个
    /// 魔数有三个出处。
    ///
    /// **返回类型必须是 `i32`，这不是可选的实现细节**：两个消费点都做
    /// `self.emit(&slot_off.to_le_bytes())`（disp32 分支），`i64` 的 `to_le_bytes()`
    /// 是 8 字节 —— 数值相同、**发射宽度不同**，会把紧随其后的指令流整体推偏
    /// （objdump 表现为 `(bad)`，执行到那里 SIGILL）。独立复核拓到了这一条，
    /// 而三档产物哈希抓不到它（语料里没有 `expr_depth >= 10` 的帧槽回退）。
    /// 下面那条宽度契约用例就是守这条线的。
    pub const fn temp_slot_off(depth: i64) -> i32 {
        -(FIXED_HDR + depth * 8) as i32
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
            var_bytes: 80,
            // 晚分配带传 0：本用例测的就是「带不存在时全部坐标逐字节等于搬迁前」
            late_slots: 0,
            sret_need: 64,
            va_slots: 48 + 128,
            static_chain_slot: 8,
            bf_rmw_reserve: BF_RMW_RESERVE,
            vec_reserve: VEC_RESERVE,
            tramp_reserve: TRAMP_RESERVE,
            stack_reserve: 176,
            spill_slots: 0,
        };
        let l = LayoutMap::resolve(&p);
        let hdr = 48 + 3 * 8; // 72
        let raw = 80 + hdr + 64 + 0 + 8 + CX_FRAME_BAND + 176 + 512 + 32 + 256 + 176;
        let frame_size = (raw + 15) & !15;
        assert_eq!(l.hdr, hdr);
        assert_eq!(l.frame_size, frame_size);
        assert_eq!(l.sret_region_base, -(hdr + 80));
        assert_eq!(l.vec_temp_off, -frame_size + CX_FRAME_BAND + 176 + 8 + 8);
        assert_eq!(l.variadic_reg_save_off, -frame_size + CX_FRAME_BAND);
        assert_eq!(
            l.sret_ptr_off, -frame_size + CX_FRAME_BAND + 176 + 8,
            "搬迁前 `functions.rs:822` 的 sret 指针槽"
        );
        assert_eq!(l.rvalue_off, -frame_size + 8);
        assert_eq!(l.spill_band_bytes, 0, "Phase 2 不得凭空长出 spill 带");
        // 额度：帧大小 - 帧头 - 帧底那摞带 = 变量区 + sret 区 + 取整富余
        assert_eq!(l.arena_room, 80 + 64 + (frame_size - raw));
    }

    /// spill band 为 0 时**一切坐标都不动** —— 这是 Phase 2「零行为 diff」的代数形式。
    #[test]
    fn zero_spill_leaves_the_frame_untouched() {
        let base = FramePlan {
            temp_slots: 2,
            var_bytes: 16,
            late_slots: 0,
            sret_need: 0,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
            vec_reserve: VEC_RESERVE,
            tramp_reserve: TRAMP_RESERVE,
            stack_reserve: 48,
            spill_slots: 0,
        };
        let l0 = LayoutMap::resolve(&base);
        assert_eq!(l0.frame_size % 16, 0);
        assert_eq!(l0.spill_band_bytes, 0);
        assert_eq!(l0.arena_room, 16);
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
            var_bytes: 16,
            late_slots: 0,
            sret_need: 32,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
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
            var_bytes: 16,
            late_slots: 0,
            sret_need: 32,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
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
            var_bytes: 16,
            late_slots: 0,
            sret_need: 32,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
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
            temp_slots: 2, // hdr = 48 + 16 = 64
            var_bytes: 32,
            late_slots: 0,
            sret_need: 16,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
            vec_reserve: VEC_RESERVE,
            tramp_reserve: TRAMP_RESERVE,
            stack_reserve: 48,
            spill_slots: 0,
        };
        let l = LayoutMap::resolve(&p);
        // raw = 32+64+16+48+512+256+48 = 976（16 整除）
        assert_eq!(l.frame_size, 976, "先钉住搬迁前的公式");
        assert_eq!(l.arena_room, 48, "诊断读数 = 规划期变量区 + sret 区");
        assert_eq!(l.frame_base(), -976, "帧底");

        // 变量区与 sret 区各自吃帧底富余（今天合法）：不得报错
        assert!(l.fits_above_bottom(-700));
        assert!(l.fits_above_bottom(-976), "恰好落在帧底算合法");
        // 真越界：低一个字节都不行
        assert!(!l.fits_above_bottom(-977));
        // **不得把两个区的字节相加**：那会把夹逼过的 `used` 里的 sret 深度再数一次
        assert!(l.fits_above_bottom(-(l.arena_room + 800)) == l.fits_above_bottom(-848));
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
        assert_eq!(u.hdr, LayoutMap::hdr_of(0), "旧 frame_hdr() 在 temp_slots=0 时给 48");
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
        // 边界：|off| < 128 走 disp8，否则走 disp32（消费点的分岔）
        assert_eq!(LayoutMap::temp_slot_off(9), -120); // 仍在 disp8 范围内
        assert_eq!(LayoutMap::temp_slot_off(10), -128); // 刚越界，走 disp32
        assert_eq!(LayoutMap::temp_slot_off(14), -160);
        // 发射宽度必须是 4 字节（i32），否则 disp32 位置会多吃 4 字节
        assert_eq!(
            LayoutMap::temp_slot_off(10).to_le_bytes().len(),
            4,
            "消费点用 `&slot_off.to_le_bytes()` 直发 disp32，宽度只能是 4 字节"
        );
    }

    /// 帧头算式只有一个出处。
    #[test]
    fn hdr_is_a_single_formula() {
        assert_eq!(LayoutMap::hdr_of(0), 48);
        assert_eq!(LayoutMap::hdr_of(2), 64);
        let p = FramePlan {
            temp_slots: 7,
            var_bytes: 0,
            late_slots: 0,
            sret_need: 0,
            va_slots: 0,
            static_chain_slot: 0,
            bf_rmw_reserve: 0,
            vec_reserve: VEC_RESERVE,
            tramp_reserve: TRAMP_RESERVE,
            stack_reserve: 48,
            spill_slots: 0,
        };
        assert_eq!(LayoutMap::resolve(&p).hdr, LayoutMap::hdr_of(7));
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
                var_bytes: 64,
                late_slots,
                sret_need: 32,
                va_slots: 0,
                static_chain_slot: 0,
                bf_rmw_reserve: 0,
                vec_reserve: VEC_RESERVE,
                tramp_reserve: TRAMP_RESERVE,
                stack_reserve: 48,
                spill_slots: 0,
            })
        };
        let no_band = mk(0);
        let band = mk(2);
        let hdr = LayoutMap::hdr_of(2);
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
