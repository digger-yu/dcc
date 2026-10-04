// src/location.rs —— 一个值的**家**（Location）：中立的类型层。
//
// 为什么单独一层（而不是放在 `ssa` 或某个后端里）：P12 字面形态要求「谁住在哪里」这件事
// 只有一个决定点、并且被所有消费方读同一个结果（`docs/p12-physreg-spill-design.md` §1 目标
// 1/2/4）。承载它的有两个位置：
//   * IR 侧的**版本粒度表**（`crate::ssa::location::LocMap`，按 SSA 名）—— 分配器的决定记录；
//   * AST 侧的**逐声明家**（`crate::ast::VarDecl::home`）—— 交给后端的交付结果，
//     必须在所有档位（含 -O0 与降级回 AST 的路径）都存在，而那些路径上没有 `ssa::Function`。
// 两处都要用同一组类型，所以类型住在两边都能引用的中立模块里；让 `ast` 去依赖 `ssa`
// 是反方向的依赖（前端不该知道 IR 的存在）。
//
// 编号域按**通道**解释（设计 §6 的 Q3）：`0` 在 GP 里是 `%rax`、在 SIMD 里是 `%xmm0`。
// 全局唯一编号会迫使 SSE/x87 与 GP 共用一张占用表，而两个通道的宽度与保存约定完全不同。

/// 寄存器编号的通道。
///
/// 名字取 ISA 中立形（不叫 `XMM`/`x87`），因为这是 IR/AST 都用的类型；每个通道里
/// 「有哪些号可动」是后端事实，住在后端的池定义里（x86_64：`abi/regbank.rs`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegBank {
    /// 整型/指针通用寄存器。
    Gp,
    /// 打包向量 / SSE 标量通道。
    Simd,
    /// x87 那种「按栈顶访问」的浮点通道。
    FloatStack,
}

/// 一个物理寄存器号：必须带通道，单独的 `u8` 在当前设计下**无意义**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegId {
    pub bank: RegBank,
    /// 后端自己解释的编号（x86_64 是 REX 三字段编码出来的那个 0..31）。
    pub n: u8,
}

/// 栈槽偏移的「未定」哨兵。
///
/// **为什么需要它**：帧偏移要到 codegen 期的布局解算之后才知道（`abi/layout.rs`），而
/// 决定「住栈槽」这件事在更早就发生了。所以早期写下的 `Stack` 只能是「由后端按既有规则
/// 定槽」的占位。做成显式哨兵而不是 `Stack(0)`，是为了让「未定」在代码里可读、可断言 ——
/// 真实的 0 偏移与「还没定」必须是两个能区分的值。
pub const STACK_BACKEND_ASSIGNED: i64 = i64::MIN;

/// 一个值的家。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// 栈槽：后端帧表示（x86_64 是 `%r13 + disp`）。`STACK_BACKEND_ASSIGNED` 表示
    /// 「由后端按既有变量分配规则定槽」，此时本变体只是**类型正确的占位**。
    Stack(i64),
    /// 物理寄存器（编号按 `RegId::bank` 解释）。
    Reg(RegId),
    /// 分配器保留的溢出槽。**携带的是 band 内的槽序号（0 起），不是帧内偏移**：
    /// 分配器在 opt 期还看不到帧布局（`abi/layout.rs` 的 `spill_band_top` 要到解算
    /// 之后才存在），所以「哪个溢出槽」是它能给出的唯一确定量。序号→偏移的翻译
    /// 只允许发生在后端的 `LayoutMap::spill_off`（`codegen::x86_64::abi::layout`）一处；谁自己
    /// 拿这个 i64 当偏移用，谁就成了第七个现场推算帧坐标的地方（违反 §3 的 H1）。
    Spill(i64),
    /// 常量家（设计 §7 Phase 5 才用得上；本阶段只定义，不产生）。
    Imm(i128),
}

impl Location {
    /// 「后端定槽」的占位家。早期建表时用它，而不是编一个假偏移。
    pub fn backend_assigned_stack() -> Self {
        Location::Stack(STACK_BACKEND_ASSIGNED)
    }

    pub fn is_backend_assigned_stack(&self) -> bool {
        matches!(self, Location::Stack(o) if *o == STACK_BACKEND_ASSIGNED)
    }

    pub fn reg(&self) -> Option<RegId> {
        match self {
            Location::Reg(r) => Some(*r),
            _ => None,
        }
    }

    /// 是否已经是**后端可直接照办**的家（占位栈槽不算：它需要后端自己算偏移）。
    ///
    /// 这个判据只有一处定义，因为「表到底填好了没有」这件事如果被每个消费方各自理解，
    /// 就会出现「一半消费方等填、一半消费方自己算」的双轨 —— 那正是本方案要消灭的形状。
    pub fn is_resolved(&self) -> bool {
        match self {
            Location::Stack(o) => *o != STACK_BACKEND_ASSIGNED,
            Location::Reg(_) | Location::Spill(_) | Location::Imm(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 编号域按通道解释：`n` 相同的两个号不是同一个家。
    #[test]
    fn reg_numbers_are_per_bank() {
        let gp0 = Location::Reg(RegId {
            bank: RegBank::Gp,
            n: 0,
        });
        let simd0 = Location::Reg(RegId {
            bank: RegBank::Simd,
            n: 0,
        });
        assert_ne!(gp0, simd0, "同一个数在两个通道里是两个不同的寄存器");
        assert_eq!(gp0.reg().map(|r| r.bank), Some(RegBank::Gp));
        assert_eq!(simd0.reg().map(|r| r.n), Some(0));
    }

    /// 「后端定槽」的占位必须可识别，且不得与真实偏移 0 混淆。
    #[test]
    fn backend_assigned_stack_is_an_explicit_placeholder() {
        let ph = Location::backend_assigned_stack();
        assert!(ph.is_backend_assigned_stack());
        assert!(
            !Location::Stack(0).is_backend_assigned_stack(),
            "`Stack(0)` 是一个真实偏移，不是占位"
        );
        assert_eq!(ph.reg(), None);
        // 占位不算「已定」：消费方读到它必须去查后端规则，而不是照抄偏移。
        assert!(!ph.is_resolved());
        assert!(Location::Stack(0).is_resolved());
        // `Spill` 携带的是**槽序号**而不是偏移（见枚举上的注释）：-600 这种写法会让人
        // 误以为它是帧坐标，于是消费方要么少翻译一次、要么翻译两次（后者静默算出 band
        // 之下的地址）。这里用两个真实可能的序号，同时钉住「已定完」的读法。
        assert!(Location::Spill(0).is_resolved());
        assert!(Location::Spill(3).is_resolved());
        assert!(!Location::backend_assigned_stack().is_resolved());
    }
}
