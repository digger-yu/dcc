//! emit/expr/utils.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::emit::gas::{gp as gas_gp, REG32 as GAS_REG32, REG64 as GAS_REG64};
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::Codegen;
use crate::codegen::x86_64::*;
use crate::const_eval::{self, ConstValue};
use crate::debug::*;
use crate::diagnostics::{Diagnostic, Severity, Span};
use crate::ir::{self, *};
use crate::semantics::{self, *};
use crate::target::Target;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// 标量写时目标地址的持有方式（P13 阶段2）。
///
/// 三条路按「右值会不会踩到地址」分层，优先级从上到下：
///   * [`AddrKeep::InRcx`]：地址搬进 `rcx`。右值只会写 `rax`（白名单
///     [`Codegen::store_rhs_isolated`]），算完直接 store——一条 `mov`，不碰栈。
///   * [`AddrKeep::Late`]：地址**不保存、也不算两遍**。右值不可能改地址链上的
///     名字（[`Codegen::store_addr_after_rhs`]），所以 C11 6.5.16 的「左右操作数
///     求值无序」允许先算右值、随后一条 `lea`/`mov` 重算出同一个地址——push/pop
///     整对消失，还省掉「押栈 + 弹栈」的 store→load 前递。
///   * [`AddrKeep::OnStack`]：其余形态照旧 `push rax` / `pop rcx`。
#[derive(Clone, Copy)]
pub(crate) enum AddrKeep {
    InRcx,
    Late(LateAddr),
    OnStack,
}

/// [`AddrKeep::Late`] 重算地址所需的全部常量信息。
///
/// 判据与发射单源：`late_addr_shape` 能完整描述出一个形态才允许 Late，发射时就按
/// 同一个描述发码，不会出现「判定放过、发射时已不知道怎么算」的分歧。这也是不把
/// 地址暂存进 `Codegen` 字段的理由——字段会被提前 return 的臂泄漏，局部的
/// `AddrKeep` 会被编译器逼着配上对。
#[derive(Clone, Copy)]
pub(crate) enum LateAddr {
    /// 地址 = `r13 + off`：栈槽本身，或全是常量字段偏移的成员链（`a` / `a.b.c`）。
    Frame(i64),
    /// 地址 = `[r13 + slot] + more`：`*p` 的 `more == 0`，`p->f` 的 `more == 字段偏移`。
    /// 指针值被**重新读一次**，所以右值不得改写它（见 `late_addr_shape`）。
    Loaded { slot: i64, more: i64 },
}

impl LateAddr {
    fn plus(self, off: i64) -> Self {
        match self {
            LateAddr::Frame(o) => LateAddr::Frame(o + off),
            LateAddr::Loaded { slot, more } => LateAddr::Loaded {
                slot,
                more: more + off,
            },
        }
    }
}

impl Codegen {
    /// 比较运算 → `setcc` 助记符 + 条件码。
    ///
    /// 单一真源：`binop` 的「寄存器 ← 寄存器」比较与 `gen_expr` 的「寄存器 ←
    /// 立即数」比较（右操作数是整型常量）必须落到同一张表上，否则同一个 C
    /// 表达式会因为右操作数是不是常量而给出不同的布尔值。
    pub(crate) fn cmp_cc(
        op: BinOp,
        is_signed: bool,
    ) -> (&'static str, crate::codegen::x86_64::opcode::CondCode) {
        use crate::codegen::x86_64::opcode::CondCode;
        match op {
            BinOp::Lt if is_signed => ("setl", CondCode::Lt),
            BinOp::Lt => ("setb", CondCode::B),
            BinOp::Le if is_signed => ("setle", CondCode::Le),
            BinOp::Le => ("setbe", CondCode::Be),
            BinOp::Gt if is_signed => ("setg", CondCode::Gt),
            BinOp::Gt => ("seta", CondCode::A),
            BinOp::Ge if is_signed => ("setge", CondCode::Ge),
            BinOp::Ge => ("setae", CondCode::Ae),
            BinOp::EqEq => ("sete", CondCode::Eq),
            BinOp::Ne => ("setne", CondCode::Ne),
            _ => unreachable!("cmp_cc: 非比较运算"),
        }
    }

    /// 条件直生分支允许的操作数类型：整型（含字符/`_Bool`/枚举）与指针/数组。
    ///
    /// 本判据只负责把「不是走整数 cmp 臂」的形态挡在外面（浮点/复数/向量/`__int128`/
    /// 结构体），位宽选择仍由比较臂自己按 `size()` 定——不在这里重写。
    pub(crate) fn cmp_flags_types_ok(ty: &Type) -> bool {
        Self::is_integer_type(ty) || matches!(ty, Type::Ptr(_) | Type::Array(_, _))
    }
    /// 标量赋值的「右值不会碰地址寄存器」白名单（P13 阶段2）。
    ///
    /// 判据形态与 Binop 臂的 `rhs_isolated` 同源：用**形态枚举**而不是
    /// `expr_has_side_effects`（它对 `StmtExpr`/内置函数返回 false 是既有案底，而那两类里
    /// 完全可能有 call；喊错一条就是静默错值）。命中后地址先进 `rcx`、右值再进 `rax`，
    /// 于是赋值臂不再需要「押地址到栈上再 `pop` 回来」。额外三道门：
    ///   * 两侧都必须是**纯整型或指针**且宽度 ≤ 8：浮点的值通道是 xmm0/st(0)，16 字节的值模型是
    ///     `(rax, rdx)` 一对；聚合类型（struct/union/array/vector/complex）虽然也能 ≤ 8 字节，
    ///     但它们的赋值走的是「整块 `rep movsb`」那条子路，不是单条 store，一并排除。
    ///     三者都沿用原有那条臂，不在这里复制第二份顺序；
    ///   * 左值不是位域：位域写要按宽度做 RMW 掩码，是另一条臂；
    ///   * 右值只能是「只会写 `rax`」的标量读形态：字面量、名字，以及由 `Cast`/`Member`/
    ///     `Deref` 套出来的标量读链（见 [`Self::scalar_load_shape`]）。移位计数、除法的
    ///     `rax/rdx`、call 返回值、`Index` 取址等形态全在名单外。

    /// 右值的「只会写 `rax`」形态判据：除了字面量与名字，还允许**纯标量读链**
    /// （`Cast`/`Member`/`Deref` 套在名字或链上）——本后端这几条取址路径一律把中间地址
    /// 算在 `rax` 上（`gen_lvalue_addr` 的 Member/Deref 臂、emit_lea_r13 + `add rax, off`），
    /// 不会踩 `rcx`。
    ///
    /// **`Index` 故意不进名单**：下标取址要把索引算进 `rcx`（`gen_lvalue_addr` 的 Index 臂
    /// 与 `mem/addr.rs` 的基址累加都如此），那正好是地址寄存器的位置；位域读也要跳
    /// RMW 掩码那一套，一并排除。这一判据不靠「应该是安全的」推断：`store_direct_*.c`
    /// 夹具与四档等价、gcc-dg 对拍都在直接试它。
    fn scalar_load_shape(e: &Expr) -> bool {
        match e {
            Expr::Int { .. } | Expr::Char { .. } | Expr::Ident { .. } => true,
            // 整型常量表达式在 `Expr::Binop` 臂里会被 `const_eval` 折成一条
            // `mov imm`，折叠后只写 `rax`。判据与那条折叠逐字同源
            //（[`Self::binop_const_foldable`]），而不是「大概是安全的」。
            Expr::Binop { .. } => Self::binop_const_foldable(e),
            // `Cast` 一般只是取址/取值链上的一层包装，**但同宽而换符号性的那层必须挡住**：
            // 这条快道后面按**内层对象自己的类型**决定符号/零扩展
            // （`emit_load_self_typed(size, is_signed)`），于是 `(unsigned)v` 被当成 `int` 读，
            // 少一次零扩展。实测：`100000000000UL + (unsigned)v` 在 v=-1 时得 99999999999
            //（gcc 104294967295），`t = (unsigned)v`（t 是 `unsigned long`）同样错；
            // 而 `return (unsigned)v;` 走通用路径是对的。退出快道后由 `gen_expr_cast`
            // 补上那一次归一，代价只是这一种形状少一个暂存号复用。
            Expr::Cast { ty, e: inner, .. } => {
                if ty.size() == inner.ty().size()
                    && Self::is_signed_int(ty) != Self::is_signed_int(&inner.ty())
                {
                    false
                } else {
                    Self::scalar_load_shape(inner)
                }
            }
            Expr::Member { expr, .. } => {
                !matches!(expr.ty(), Type::Struct(_, _, _, _) | Type::Union(_, _, _, _))
                    && Self::scalar_load_shape(expr)
                    && Self::expr_bitfield_width(e).is_none()
            },
            Expr::Deref { e: inner, .. } => Self::scalar_load_shape(inner),
            _ => false,
        }
    }

    /// `Expr::Binop` 臂会不会把整条表达式折成一条 `mov imm`。
    ///
    /// 与 `dispatch.rs` 的 `Expr::Binop` 臂逐字同源：两侧与结果都是整型、不是
    /// `&&`/`||`、无副作用、`const_eval::eval` 成功。任何一条不满足就不折叠，
    /// 那时右值可能写 `rcx`，不能进 [`Self::scalar_load_shape`]。
    fn binop_const_foldable(e: &Expr) -> bool {
        let Expr::Binop { op, l, r, .. } = e else {
            return false;
        };
        !matches!(op, BinOp::AndAnd | BinOp::OrOr)
            && Self::is_integer_type(&l.ty())
            && Self::is_integer_type(&r.ty())
            && Self::is_integer_type(&e.ty())
            && !Self::expr_has_side_effects(e)
            && const_eval::eval(e).is_ok()
    }

    /// 「单条 GP store 就能写完的标量」——阶段2 两条白名单共用的类型门。
    ///
    /// 聚合类型（含 ≤ 8 字节的 struct/array/vector/complex）即使尺寸放得下也不接：
    /// 它们走整块拷贝子路，那里的 `rcx` 后面还要当 `rep movsb` 的计数器用。
    fn store_scalar_ty_ok(t: &Type) -> bool {
        t.size() <= 8
            && match t {
                Type::Struct(_, _, _, _)
                | Type::Union(_, _, _, _)
                | Type::Array(_, _)
                | Type::Vector(_, _)
                | Type::Complex(_)
                | Type::Imaginary(_) => false,
                Type::Ptr(_) => true,
                other => Self::is_integer_type(other),
            }
    }

    /// `DCC_DEST_DEREF`：解引用是否把指针值直接算到调用方要求的号（而不是固定 `%rax`）。
    /// 默认 0，产物与改动前逐字节相同。它同时是下面 [`Self::rhs_only_writes_stage`] 对
    /// `Deref` 放行的前置条件：发射路径没改之前，判据不得先按「计划中的行为」放行。
    pub(crate) fn dest_deref_enabled() -> bool {
        static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *V.get_or_init(|| std::env::var("DCC_DEST_DEREF").map(|s| s != "0").unwrap_or(false))
    }

    /// 「后缀自增与解引用合成一次访存 + 一次加法」是否启用（`DCC_POSTINC_FOLD`，默认关）。
    ///
    /// 单独一个旋钮而不是附在 `DCC_DEST_DEREF` 上：那一条改的是「基址从哪个号读」，
    /// 本条改的是「自增与载入的**先后」**，两者错开的形状不同，量验时必须能单独关。
    pub(crate) fn postinc_fold_enabled() -> bool {
        static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *V.get_or_init(|| std::env::var("DCC_POSTINC_FOLD").map(|s| s != "0").unwrap_or(false))
    }

    /// 「这个类型的值能否用一条 GP 装载/存完」——与下面几个臂共用的标量门。
    /// 浮点走 xmm/st(0) 通道、int128 走 rdx:rax 成对、聚合走整块拷贝（那些路径会占
    /// `rcx`/`rdx`/`xmm0`），三者一律不进「只写自己那个号」的判定。
    /// `pub(crate)`：除本模块的判据外，赋值臂的「直写家」(`dispatch.rs` 的 `wdst`) 也要同一
    /// 扇门，不能两边各写一份名单。
    pub(crate) fn stage_scalar_ty(t: &Type) -> bool {
        match t {
            Type::Bool
            | Type::Char
            | Type::SChar
            | Type::UChar
            | Type::Char8
            | Type::Char16
            | Type::Char32
            | Type::Short
            | Type::UShort
            | Type::Int
            | Type::UInt
            | Type::Long
            | Type::ULong
            | Type::LongLong
            | Type::ULongLong
            | Type::Ptr(_) => true,
            _ => false,
        }
    }

    /// 右值整棵求值树是否**只会写自己那个暂存号**（`gen_expr_decay(r, stage)` 的 dst）。
    ///
    /// 为什么需要它：Binop 臂原本只认「右值是字面量/名字」（`rhs_isolated`）才敢不存左值，
    /// 否则就把左值溅到暂存区。而那个暂存区只有两个实现：callee-saved 池
    /// `POOL=[rbx,r12,r14,r15]`（`regbank.rs:39`）与帧槽 —— 池同时也就是变量家的池，
    /// `luaV_execute` 里四个号全是家，于是 `alloc_callee_saved()`/`alloc_temp` 双双返回
    /// `None`，**每一条 Binop 都退到帧槽**：实测该函数里 258 处「存基址→算右值→取回基址」，
    /// 每处 2 次多余内存操作。按本机标定，一次每步内存操作值 30–75 ms，一条每步指令只值
    /// 3.2 ms —— 这两次存取的价钱是形状问题的十倍以上。
    ///
    /// 判据安全性的支点（两条，缺一即不得放行）：
    ///  1. 发射期的暂存取号器**永不选 `rax`**（`rhs_stage_candidates` = `AUTO_EXT_POOL`，
    ///     `regbank.rs` 底部单测把 rax/rdx/rsi/rdi/r10/r11/r13 钉在候选表之外），所以嵌套树
    ///     自己的 stage 碰不到外面那个号；
    ///  2. 当前这一层节点的发射路径必须只写 dst：成员/解引用止于 `emit_load_self_typed`
    ///     那一族（寄存器无关），但**解引用只有在 `DCC_DEST_DEREF` 开了之后**才不落 `rax`，
    ///     所以那条判据把旋钮状态当输入（没开就必须拒，否则就是拿计划中的行为骗今天的产物）。
    ///
    /// 不收的形态：下标（地址链还要用基址号）、调用/赋值/短路/条件/自增、`__builtin_*`、
    /// 需 `%cl` 的变量移位、需 `rdx:rax` 的乘除、聚合与浮点。不认识就当会写：
    /// 保守方向就是今天的行为，最多是少省两次存取，不会错值。
    pub(crate) fn rhs_only_writes_stage(r: &Expr) -> bool {
        match r {
            // 编译期常量：求值不写任何号（或被折成一条 `mov imm` 只写 dst）。
            Expr::Int { .. } | Expr::Char { .. } | Expr::SizeofTy { .. } | Expr::AlignofTy { .. } => {
                Self::stage_scalar_ty(&r.ty())
            },
            Expr::Ident { .. } => Self::stage_scalar_ty(&r.ty()),
            Expr::Cast { ty, e, .. } => {
                Self::stage_scalar_ty(ty) && Self::rhs_only_writes_stage(e)
            },
            Expr::Unary { op, e, .. } => {
                matches!(op, UnOp::Neg | UnOp::Not | UnOp::BitNot | UnOp::Plus)
                    && Self::stage_scalar_ty(&r.ty())
                    && Self::rhs_only_writes_stage(e)
            },
            // 成员访问：地址链与装载都是 dst-directed（`emit_load_self_typed` 那一族），
            // 字段本身必须是标量（聚合字段走整块拷贝，那条路要占 rcx）。
            Expr::Member { expr, .. } => {
                Self::stage_scalar_ty(&r.ty()) && Self::rhs_only_writes_stage(expr)
            },
            // 解引用：只有 `DCC_DEST_DEREF` 开着时才不碰 rax（见 `lvalue.rs` 那条快道）。
            Expr::Deref { e, ty, .. } => {
                Self::dest_deref_enabled() && Self::stage_scalar_ty(ty)
                    && Self::rhs_only_writes_stage(e)
            },
            Expr::Binop {
                op: op @ (BinOp::Add
                | BinOp::Sub
                | BinOp::And
                | BinOp::Or
                | BinOp::Xor
                | BinOp::Shl
                | BinOp::Shr
                | BinOp::Mul),
                l,
                r: rr,
                ..
            } => {
                if !(Self::stage_scalar_ty(&r.ty()) || matches!(r.ty(), Type::Ptr(_))) {
                    return false;
                }
                // 指针加减的标量那一侧也必须是标量。
                if matches!(r.ty(), Type::Ptr(_))
                    && !matches!(op, BinOp::Add | BinOp::Sub)
                {
                    return false;
                }
                // 移位计数必须是常量：变量计数要走 `%cl`，那是另一个号。
                let count_ok = !matches!(op, BinOp::Shl | BinOp::Shr)
                    || matches!(rr.as_ref(), Expr::Int { .. } | Expr::Char { .. });
                // 乘法则要求有一边是常量：本后端除 `imul r,r,imm` 外还走 `rax` 累加器路径。
                let mul_ok = *op != BinOp::Mul
                    || matches!(l.as_ref(), Expr::Int { .. })
                    || matches!(rr.as_ref(), Expr::Int { .. });
                // 指针算术的一侧必须是指针、另一侧是标量。
                let ptr_ok = !matches!(r.ty(), Type::Ptr(_))
                    || (Self::stage_scalar_ty(&rr.ty()) || Self::stage_scalar_ty(&l.ty()));
                count_ok && mul_ok && ptr_ok && Self::rhs_only_writes_stage(l) && Self::rhs_only_writes_stage(rr)
            },
            _ => false,
        }
    }

    pub(crate) fn store_rhs_isolated(lv: &Expr, r: &Expr, l_ty: &Type) -> bool {
        let r_ty = r.ty();
        Self::scalar_load_shape(r)
            && Self::store_scalar_ty_ok(l_ty)
            && Self::store_scalar_ty_ok(&r_ty)
            && Self::expr_bitfield_width(lv).is_none()
    }

    /// 标量写：把已算在 `rax` 的目标地址保到右值求值之后（阶段2 的统一入口）。
    ///
    /// 今天 Deref / Member / `__real__` / Index / 通用这五条赋值臂各自都写了一遍
    /// 「`push rax` → 算右值 → `pop rcx` → store」，押栈的唯一理由是「地址与右值挤
    /// 同一个号」。判据只存这一份（[`Self::plan_addr_keep`]），调用方只需把返回值
    /// 交给 [`Self::restore_addr_after_rhs`]。
    ///
    /// 为什么用**显式返回值**而不是藏在 `Codegen` 字段里：字段会被「save 与 restore 之间
    /// 提前 return」的臂泄漏，而下一个 `pop(1)` 一旦被静默跳过，store 写的就是陈旧的 rcx
    //（静默错值）。局部变量不会骗人：编译器会报未使用，人也看得到配对。
    pub(crate) fn save_addr_for_rhs(&mut self, lv: &Expr, r: &Expr, l_ty: &Type) -> AddrKeep {
        let keep = self.plan_addr_keep(lv, r, l_ty);
        match keep {
            // 地址之后重算，这里一条也不发（已经算在 rax 的那份就此作废）。
            AddrKeep::Late(_) => {},
            AddrKeep::InRcx => self.mov(1, 0), // rcx = 目标地址（净栈变化为零）
            AddrKeep::OnStack => self.push(0), // 保存目标地址（右值会踩 rax）
        }
        keep
    }

    /// 三条路的判据，按代价从低到高：先把地址搬进 `rcx`（一条 `mov`），否则拖到右值
    /// 之后重算（一条 `lea`/`mov`+`add`），都不行才押栈（两条 + 内存往返）。
    pub(crate) fn plan_addr_keep(&mut self, lv: &Expr, r: &Expr, l_ty: &Type) -> AddrKeep {
        if Self::store_rhs_isolated(lv, r, l_ty) {
            return AddrKeep::InRcx;
        }
        if let Some(shape) = self.store_addr_after_rhs(lv, r, l_ty) {
            return AddrKeep::Late(shape);
        }
        AddrKeep::OnStack
    }

    /// [`AddrKeep::Late`] 的类型门：单条 store 能写完的标量。
    ///
    /// 与 [`Self::store_scalar_ty_ok`] 的差别只在浮点：`InRcx` 要求右值「只会写
    /// `rax`」，而浮点的值通道是 xmm0；Late 只要求「重算地址不碰值通道」，
    /// xmm0/st(0) 与 rcx 互不干扰，所以 `float`/`double` 也能走。
    /// `long double` / `__float128` 是 16 字节且值在 x87 栈上，不接。
    fn late_store_ty_ok(t: &Type) -> bool {
        match t {
            Type::Struct(_, _, _, _)
            | Type::Union(_, _, _, _)
            | Type::Array(_, _)
            | Type::Vector(_, _)
            | Type::Complex(_)
            | Type::Imaginary(_)
            | Type::LongDouble
            | Type::Float128 => false,
            Type::Float | Type::Double | Type::Ptr(_) => true,
            other => Self::is_integer_type(other),
        }
    }

    /// 判据：这条标量写的地址能不能**在右值之后重算**。
    ///
    /// 与 [`Self::store_rhs_isolated`] 的区别在右值形态：那里只收「只会写 `rax`」的
    /// 读链，而这里收「会踩 `rcx`/`rdx` 但不会改地址」的任意形态——`a.b = x + 1` 与
    /// `L->top = L->top + 1` 这两类主力恰好都在前一条的名单外。两道门：
    ///   * 地址链能描述成 [`LateAddr`]（`Ident` / 全常量字段偏移的 `Member` 链 /
    ///     `*p`、`p->f` 且 `p` 住栈槽）；
    ///   * 只写单条 store 的标量，且不是位域（位域写是 RMW 掩码，另一条臂）。
    ///
    /// 「右值不得改写地址链上的名字」的约束内聚在 `late_addr_shape` 里：只有 `Loaded`
    /// 形态的指针值会被重新读，`Frame` 的 `r13+off` 与右值无关。
    fn store_addr_after_rhs(&mut self, lv: &Expr, r: &Expr, l_ty: &Type) -> Option<LateAddr> {
        if !Self::late_store_ty_ok(l_ty) || Self::expr_bitfield_width(lv).is_some() {
            return None;
        }
        self.late_addr_shape(lv, r)
    }

    /// 把 lvalue 的地址形态描述出来；描述不出来就不许 Late。
    ///
    /// `Index` 故意不在名单里：下标取址要算进 `rcx`（`gen_lvalue_addr` 的 Index 臂
    /// 与 `mem/addr.rs` 的基址累加都如此），重算时正好撞上 store 要的地址寄存器；
    /// 而且下标表达式本身可能有 side effect，重算就是重复求值。
    fn late_addr_shape(&mut self, lv: &Expr, rhs: &Expr) -> Option<LateAddr> {
        match lv {
            Expr::Ident { name, ty, .. } => {
                // VLA 的「地址」存在元数据槽里，不在这里重算。
                if ty.has_vla() || !self.name_is_plain_slot(&name.name) {
                    return None;
                }
                Some(LateAddr::Frame(self.stack_home_off(&name.name)))
            },
            Expr::Member {
                expr,
                field,
                is_arrow,
                ..
            } => {
                let field_off = self.late_member_off(expr, field, *is_arrow)?;
                if *is_arrow {
                    // `(&x)->f` 就是 `x.f`：SSA 把变址/取址临时值实体化成 `AddrOf`
                    // 之后，这正是绝大多数「隐式解引用」在 AST 里的样子。
                    // 字段偏移必须按**基址**（结构体本体）算：`expr` 自己的类型是
                    // 指针，`member_target_ty(_, false)` 不解包，拿它查会得到 `None`，
                    // 整条分支就恒不成立（写成这样时实测 m6 的 `s.a = f()` 仍在押栈）。
                    if let Expr::AddrOf { e: base, .. } = &**expr {
                        let dot_off = self.late_member_off(base, field, false)?;
                        return Some(self.late_addr_shape(base, rhs)?.plus(dot_off));
                    }
                    // 基址是指针的**值**：必须住在栈槽上，且右值不得改写它。
                    let Expr::Ident { name, ty, .. } = &**expr else {
                        return None;
                    };
                    if ty.has_vla()
                        || !self.name_is_plain_slot(&name.name)
                        || Self::expr_may_write_name(rhs, &name.name)
                    {
                        return None;
                    }
                    Some(LateAddr::Loaded {
                        slot: self.stack_home_off(&name.name),
                        more: field_off,
                    })
                } else {
                    // `a.b`：基址是对象本身，地址恒定，与右值无关。
                    Some(self.late_addr_shape(expr, rhs)?.plus(field_off))
                }
            },
            Expr::Deref { e: inner, .. } => {
                // `*(&x)` 就是 `x` 本身：SSA 的地址临时值常以这对形态出现。
                if let Expr::AddrOf { e: base, .. } = &**inner {
                    return self.late_addr_shape(base, rhs);
                }
                let Expr::Ident { name, ty, .. } = &**inner else {
                    return None;
                };
                if ty.has_vla()
                    || !self.name_is_plain_slot(&name.name)
                    || Self::expr_may_write_name(rhs, &name.name)
                {
                    return None;
                }
                Some(LateAddr::Loaded {
                    slot: self.stack_home_off(&name.name),
                    more: 0,
                })
            },
            Expr::Cast { e: inner, .. } => self.late_addr_shape(inner, rhs),
            _ => None,
        }
    }

    /// `Member` 的静态字段偏移；带 VLA 成员的结构要运行时算偏移
    ///（`gen_vm_member_offset` 会分配临时槽并写 r10/r11），不在「一条 `lea` 重算」的包络内。
    fn late_member_off(&mut self, base: &Expr, field: &Ident, is_arrow: bool) -> Option<i64> {
        let base_ty = base.ty();
        let target_ty = self.resolve_complete_struct_ty(Self::member_target_ty(base_ty, is_arrow));
        if target_ty.has_vla() {
            return None;
        }
        match &target_ty {
            Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, false) => {
                let (f, extra) = find_field(fields, &field.name)?;
                Some((f.offset + extra) as i64)
            },
            _ => None,
        }
    }

    /// 名字是否是「平平无奇的帧槽」：全局、volatile、upvar、VLA 都不算。
    ///
    /// `home_for` 返回 `Reg` 时值住在寄存器里，`r13+off` 那条地址可能是从没写过的槽——
    /// 地址类消费点对此有专门判据（`stack_home_off` 报内部错误），这里提前挡掉。
    fn name_is_plain_slot(&mut self, name: &str) -> bool {
        if self.is_global_not_local(name)
            || self.volatile_locals.contains_key(name)
            || self.upvar_offsets.contains_key(name)
            || self.vla_offsets.contains_key(name)
        {
            return false;
        }
        matches!(
            self.home_for(name),
            crate::location::Location::Stack(_) | crate::location::Location::Spill(_)
        )
    }

    /// 右值 `e` 是否会写名字 `name`（只问栈槽局部）。
    ///
    /// 为什么问「写」不问「读」：Late 重算要读到**同一个地址**，只有写才会改它。
    /// 读完全安全，而 `L->top = L->top + 1` 正是 luaV_execute 的主力形态。
    ///
    /// 认不得的形态一律 true：`Call`/`StmtExpr`/`AsmExpr` 可能通过指针写任意对象，
    /// `&name` 一出现就说明地址逸出。这两条与后端白名单「宁宽宁窄」的纪律一致
    ///（见 §6：不得用 `expr_has_side_effects` 冒充形态枚举）。
    fn expr_may_write_name(e: &Expr, name: &str) -> bool {
        match e {
            Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::ComplexLit { .. }
            | Expr::ComplexIntLit { .. }
            | Expr::String { .. }
            | Expr::Char { .. }
            | Expr::SizeofTy { .. }
            | Expr::SizeofExpr { .. }
            | Expr::AlignofTy { .. }
            | Expr::AlignofExpr { .. }
            | Expr::LabelAddr { .. }
            | Expr::VaArgPack { .. }
            | Expr::Ident { .. } => false,
            Expr::Binop { l, r, .. } => {
                Self::expr_may_write_name(l, name) || Self::expr_may_write_name(r, name)
            },
            Expr::Unary { e, .. } | Expr::Cast { e, .. } => Self::expr_may_write_name(e, name),
            Expr::Member { expr, .. } => Self::expr_may_write_name(expr, name),
            Expr::Index { arr, idx, .. } => {
                Self::expr_may_write_name(arr, name) || Self::expr_may_write_name(idx, name)
            },
            Expr::Deref { e, .. } | Expr::Real { e, .. } | Expr::Imag { e, .. } => {
                Self::expr_may_write_name(e, name)
            },
            Expr::Cond {
                cond,
                then_,
                else_,
                ..
            } => {
                Self::expr_may_write_name(cond, name)
                    || Self::expr_may_write_name(then_, name)
                    || Self::expr_may_write_name(else_, name)
            },
            Expr::Comma { l, r, .. } => {
                Self::expr_may_write_name(l, name) || Self::expr_may_write_name(r, name)
            },
            Expr::Assign { l, r, .. } => {
                Self::lvalue_is_name(l, name)
                    || Self::expr_may_write_name(l, name)
                    || Self::expr_may_write_name(r, name)
            },
            Expr::PostInc { e, .. }
            | Expr::PostDec { e, .. }
            | Expr::PreInc { e, .. }
            | Expr::PreDec { e, .. } => {
                Self::lvalue_is_name(e, name) || Self::expr_may_write_name(e, name)
            },
            // AddrOf / Call / StmtExpr / AsmExpr / VaArg / Offsetof / CompoundLit /
            // InitList：地址逸出、任意指针写、认不得，一律当作会写。
            _ => true,
        }
    }

    /// 赋值目标 `l` 是否就是 `name` 本身（而不是它指向的东西）。
    ///
    /// `*p = v` 写的是指针指向的对象、`p->f = v` 写的是字段，两者都不改 `p` 的值，
    /// 所以 arrow 与 Deref 都返回 false；`a.f = v` 只可能写 `a` 自己的内存，继续递归。
    fn lvalue_is_name(l: &Expr, name: &str) -> bool {
        match l {
            Expr::Ident { name: n, .. } => n.name == name,
            Expr::Member {
                expr,
                is_arrow: false,
                ..
            } => Self::lvalue_is_name(expr, name),
            Expr::Cast { e, .. } => Self::lvalue_is_name(e, name),
            _ => false,
        }
    }

    /// 按 [`LateAddr`] 把地址重算进 `dst`。
    ///
    /// 只允许写 `dst`：调用时右值结果已经在 `rax`/`xmm0` 里，碰一下就读到残值。
    /// `emit_lea_r13` 只写 dst，`load_stack` 只写 dst，`add_reg_byte_offset`
    /// 在 `dst==1` 时把临时号避开到 rdx——三条臂都满足这条纪律。
    pub(crate) fn emit_late_addr(&mut self, shape: LateAddr, dst: u8) {
        match shape {
            LateAddr::Frame(off) => self.emit_lea_r13(dst, off),
            LateAddr::Loaded { slot, more } => {
                self.load_stack(dst, slot);
                if more != 0 {
                    self.add_reg_byte_offset(dst, more);
                }
            },
        }
    }

    /// [`Self::save_addr_for_rhs`] 的另一半：按 `keep` 把地址放回 `rcx`。
    pub(crate) fn restore_addr_after_rhs(&mut self, keep: AddrKeep) {
        match keep {
            AddrKeep::InRcx => {},
            AddrKeep::OnStack => self.pop(1), // rcx = 目标地址
            AddrKeep::Late(shape) => self.emit_late_addr(shape, 1),
        }
    }

    /// 条件求值（分支语境）：发条件本身，并告诉调用方「flags 已经是结果」。
    ///
    /// 返回 `Some(条件码)` 时，比较只发了 `cmp`/`test`，**没有**材料化 0/1；调用方必须
    /// 立刻用 [`Self::emit_cond_jump`] 把它变成一条 jcc，中间不得发任何指令（任何 `emit`
    /// 都会清掉 [`Codegen::last_cmp`]，那条纪律在 `instructions.rs::emit` 里）。
    ///
    /// 令牌在这里发出、在 Binop 臂被**消费**（不是读取），所以下面这些形态都自然退回
    /// 材料化路径：`if (x + (a<b))`（外层 Add 把令牌拿走）、`if (f() < 0)` 之外的浮点/复数
    /// /向量/`__int128` 比较（各有分流路径）、含赋值/调用的条件。标签号分配顺序保持
    /// 不变：本函数只做「求值条件」这一件事，与原 `gen_expr(cond, EVAL_BASE)` 同位。
    pub(crate) fn gen_cond_flags(
        &mut self,
        cond: &Expr,
    ) -> Option<crate::codegen::x86_64::opcode::CondCode> {
        self.cmp_flags_token = true;
        self.gen_expr(cond, Self::EVAL_BASE);
        self.cmp_flags_token = false;
        self.last_cmp.take()
    }

    /// 条件分支语境的总入口：先做**形状分解**（`&&`/`||`/`!`/`x != 0`），
    /// 再落到 [`Self::gen_cond_flags`] + [`Self::emit_cond_jump`]。
    ///
    /// 为什么需要这一层：`gen_cond_flags` 只把**最外层那一个比较**的 flags 交给
    /// 调用方。`if (a < b && c < d)` 里两个比较各自都被当成「值要用」材料化成 0/1
    /// 再 `test`——实测 `luaV_execute` 的 263 个 `setcc` 里有 186 个只是被 `test`
    /// 消费。分解后每个比较只发 `cmp` + `jcc`，短路顺序逐字保持（左侧先求值）。
    ///
    /// 分解只在「不新增中间落点」的方向上做：
    ///   * `if (l && r)` / `if (l || r)`（假则跳出）都拆成两条同向跳到目标的边——
    ///     `!(l && r)` 与 `!(l || r)` 都等价于「任一侧为假」；
    ///   * `do ... while (l || r)`（真则回头）同理；
    ///   * `do ... while (l && r)` 需要一条「l 真才继续测 r」的边，多开一个标签。
    ///
    /// `!e` 直接把极性翻给 `e`；`x != 0` / `x == 0` 与「x 的真值」等价，也直接消掉。
    /// x87 `long double` 与 `__float128` 没有 `emit_truth_flags` 的真值判据
    /// （float.rs 里那条旁路只覆盖 Float/Double 与 _Complex），一律不分解。
    pub(crate) fn gen_cond_jump_to(&mut self, cond: &Expr, target: &str, jump_if_true: bool) {
        if let Expr::Unary {
            op: UnOp::Not, e, ..
        } = cond
        {
            if Self::truth_test_emittable(&e.ty()) {
                self.gen_cond_jump_to(e, target, !jump_if_true);
                return;
            }
        }
        if let Expr::Binop {
            op: op @ (BinOp::AndAnd | BinOp::OrOr),
            l,
            r,
            ..
        } = cond
        {
            if Self::truth_test_emittable(&l.ty()) && Self::truth_test_emittable(&r.ty()) {
                if !jump_if_true {
                    if matches!(op, BinOp::OrOr) {
                        // `!(l || r)` = 「两侧都假才跳」：l 真就跳过 r，不能像
                        // `&&` 那样「任一侧为假就跳」—— 那是 `!(l && r)` 的形态。
                        // 弄反的后果是短路整条失效：`if (a || f() == 0)` 会先按 a
                        // 的假值跳到 else，右值一次都不求值。
                        let skip = self.fresh_label();
                        self.gen_cond_jump_to(l, &skip, true);
                        self.gen_cond_jump_to(r, target, false);
                        self.label(&skip);
                        return;
                    }
                    // `!(l && r)` = 「任一侧为假就跳」。
                    self.gen_cond_jump_to(l, target, false);
                    self.gen_cond_jump_to(r, target, false);
                    return;
                }
                if matches!(op, BinOp::OrOr) {
                    self.gen_cond_jump_to(l, target, true);
                    self.gen_cond_jump_to(r, target, true);
                    return;
                }
                // `l && r` 为真才回头：l 假就直接落到标签之后。
                let skip = self.fresh_label();
                self.gen_cond_jump_to(l, &skip, false);
                self.gen_cond_jump_to(r, target, true);
                self.label(&skip);
                return;
            }
        }
        if let Expr::Binop {
            op: op @ (BinOp::Ne | BinOp::EqEq),
            l,
            r,
            ..
        } = cond
        {
            let is_zero = |e: &Expr| matches!(e, Expr::Int { v: 0, .. });
            let probe = if is_zero(r) {
                Some(&**l)
            } else if is_zero(l) {
                Some(&**r)
            } else {
                None
            };
            if let Some(p) = probe {
                if Self::truth_test_emittable(&p.ty()) {
                    let pol = if matches!(op, BinOp::Ne) {
                        jump_if_true
                    } else {
                        !jump_if_true
                    };
                    self.gen_cond_jump_to(p, target, pol);
                    return;
                }
            }
        }
        // `l_unlikely(x)` 展开成 `__builtin_expect((x) != 0, 0)`：这一步只是分支
        // 提示，真值完全是第一个实参的。不把它拆掉的话，lvm.c 里每一处
        // `if (l_unlikely(...))` 都会把条件先材料化成 0/1 再 test —— 实测
        // `luaV_execute` 上 150 个 `setcc` 里这类占了大头。
        if let Expr::Call { callee, args, .. } = cond {
            if let Expr::Ident { name, .. } = &**callee {
                if matches!(
                    name.name.as_str(),
                    "__builtin_expect" | "__builtin_expect_with_probability"
                ) && !args.is_empty()
                {
                    // 第二个实参是提示值；字面量/名字无副作用，值本来就没人要，整条丢。
                    // 其它形态（`++x`、call）可能带副作用，退回材料化路径。
                    let hint_dead = args.len() < 2
                        || matches!(&args[1], Expr::Int { .. } | Expr::Char { .. } | Expr::Ident { .. });
                    if hint_dead && Self::truth_test_emittable(&args[0].ty()) {
                        self.gen_cond_jump_to(&args[0], target, jump_if_true);
                        return;
                    }
                }
            }
        }
        let cc = self.gen_cond_flags(cond);
        self.emit_cond_jump(target, cc, &cond.ty(), jump_if_true);
    }

    /// [`Self::emit_truth_flags`] 能对哪些类型给出真值判据（标志位）。
    /// x87 `long double` / `__float128` 的值在 st(0) / 内存里，两条通道都没写
    /// 「测值」那一步（§9.3b 的未修同族），所以不能被 [`Self::gen_cond_jump_to`]
    /// 分解到 `emit_truth_flags` 上，只能继续走材料化路径。
    fn truth_test_emittable(ty: &Type) -> bool {
        !matches!(ty, Type::LongDouble | Type::Float128 | Type::Vector(_, _))
    }

    /// 把条件跳转发出去：拿到条件码就一条 jcc，拿不到就照旧 `test` + `je`/`jne`。
    ///
    /// `jump_if_true == false` 是 If/While/For 的「假则跳出」形态（取反条件码）；
    /// `true` 是 do-while 的「真则回头」。两条路的落点与极性互逆，其余逐字节不变。
    pub(crate) fn emit_cond_jump(
        &mut self,
        target: &str,
        cc: Option<crate::codegen::x86_64::opcode::CondCode>,
        cond_ty: &Type,
        jump_if_true: bool,
    ) {
        if let Some(c) = cc {
            let c = if jump_if_true { c } else { c.inverse() };
            if let Some(kind) = Self::jump_kind_for_cc(c) {
                self.jmp_rel32(target, kind);
                return;
            }
            // 走到这里说明 `gen_expr` 的放行判据与这张表漂移了：条件码不可直生，
            // 而 0/1 又已按判据没有材料化 —— 此时静默退回 `test rax,rax` 就是拿
            // 一个从未写入的号当真值。报内部错误，让漂移在产物与诊断里都看得见。
            self.report_internal_bug(
                "E1137",
                format!("条件直生分支拿到不可映射的条件码 {c:?}（目标 '{target}'）"),
                "要么在 Binop 臂的 hand_off 判据里排除该条件码，要么在 jump_kind_for_cc 补形态；不得退回材料化（值没材料化）",
            );
        }
        self.emit_truth_flags(cond_ty);
        self.jmp_rel32(target, if jump_if_true { JumpKind::Jne } else { JumpKind::Je });
    }

    /// `cmp/test dst, imm` + `setcc al` + `movzbq` + 结果落到 `dst`。
    ///
    /// 进这里时 `dst` 里已经是左操作数的值。`use_32bit` 必须与 `binop` 的位宽
    /// 判定同源（`lt.size() <= 4 && rt.size() <= 4`）；`imm` 必须是「按该宽度
    /// 符号扩展后正好等于常量 mask」的 i64（调用方负责取低 32 位 / 范围检查）。
    ///
    /// `imm == 0` 用 `test dst, dst`：x86 上 `test` 与 `cmp 0` 的 ZF/SF/OF/CF
    /// 完全一致，但短 2~4 字节，且不占用立即数槽。
    pub(crate) fn emit_cmp_imm(
        &mut self,
        op: BinOp,
        dst: u8,
        is_signed: bool,
        use_32bit: bool,
        imm: i64,
    ) {
        use crate::codegen::shared::asm_operand::Width;
        use crate::codegen::x86_64::opcode::{AluOp, Operand as OpcodeOperand, X86_64Opcode};
        let w = if use_32bit { Width::B32 } else { Width::B64 };
        let (mnem, dst_name): (&str, &str) = if use_32bit {
            ("cmp", GAS_REG32[(dst & 15) as usize])
        } else {
            ("cmp", GAS_REG64[(dst & 15) as usize])
        };
        if imm == 0 {
            // test r/m, r: dst 同时在 reg 与 r/m 字段，高 8 号必须两个 REX 位都置。
            let gas = format!("test {dst_name}, {dst_name}");
            self.emit_insn(&Insn::test_rm_reg(w, dst, dst).with_gas(gas));
        } else {
            // `AluRm` 的 imm 分支：i8 内收敛到 `83 /7 ib`，否则 `81 /7 id`。
            // Reg(4)=rsp 的 B64 特例只认 Add/Sub（返回 None），故 B64 不走 rsp。
            let insn = Insn::new(
                X86_64Opcode::AluRm {
                    op: AluOp::Cmp,
                    width: w,
                    dst: OpcodeOperand::Reg(dst, w),
                    src: OpcodeOperand::Imm(imm),
                },
                &[],
            );
            let gas = format!("{mnem} ${imm}, {dst_name}");
            self.emit_insn_expect(&insn.with_gas(gas));
        }
        let (cc_letter, cond) = Self::cmp_cc(op, is_signed);
        // 条件直生分支：`test` 与 `cmp 0` 的 ZF/SF/OF/CF 完全一致（上面那段注释就是
        // 这条判据），所以 flags 可以直接交给 jcc，不必再走 setcc/movzbq/搬回 dst。
        if self.cmp_flags_granted {
            self.last_cmp = Some(cond);
            return;
        }
        self.emit_insn(&Insn::setcc_reg(cond, 0).with_gas(format!("{cc_letter} %al")));
        self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x0F, 0xB6, 0xC0], "movzbq %al, %rax"));
        if dst != 0 {
            self.mov(dst, 0);
        }
    }

    pub(crate) fn binop(
        &mut self,
        op: BinOp,
        dst: u8,
        src: u8,
        is_signed: bool,
        lt: &Type,
        rt: &Type,
    ) {
        let is_cmp =
            matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne);
        // Determine operand size for Add/Sub/And/Or/Xor:
        // - Unsigned 32-bit (uint): no REX.W, result auto zero-extends to 64-bit
        //   This is correct for unsigned arithmetic (masking to 32 bits).
        // - Signed 32-bit (int) or 64-bit: REX.W (64-bit operation)
        //   Signed 32-bit results must be sign-extended, not zero-extended.
        // This is critical for correct unsigned arithmetic: `0U ^ (short)-0x8000`
        // must use 32-bit XOR to get 0xFFFF8000 (zero-extended), not 0xFFFFFFFFFFFF8000.
        let op_size = lt.size().max(rt.size());
        let is_unsigned = !is_signed;
        let use_32bit = !is_cmp
            && !matches!(op, BinOp::Div | BinOp::Mod | BinOp::Mul | BinOp::Shl | BinOp::Shr)
            && op_size <= 4
            && is_unsigned;
        if !is_cmp && !matches!(op, BinOp::Div | BinOp::Mod | BinOp::Mul | BinOp::Shl | BinOp::Shr)
        {
            // P9 方案 C: REX 前缀与 opcode+ModRM 合并为单条 raw_with_gas,
            // 不再分成两次 emit_insn (否则 gas_text 丢了整条指令, 且发射两份 record).
            // Add/Sub/And/Or/Xor (0x01/0x29/...): dst 在 r/m 字段 (需 REX.B),
            // src 在 reg 字段 (需 REX.R). 二者独立判断, 不可 else-if.
            // REX.W=0 for 32-bit ops (auto zero-extend), REX.W=1 for 64-bit ops.
        }
        let r = (src & 7) << 3;
        let m = dst & 7;
        // GAS 宽度: 32 位用 %eXX, 64 位用 %rXX.
        let (src_name, dst_name): (&str, &str) = if use_32bit {
            (GAS_REG32[(src & 15) as usize], GAS_REG32[(dst & 15) as usize])
        } else {
            (GAS_REG64[(src & 15) as usize], GAS_REG64[(dst & 15) as usize])
        };
        // 通用: 拼完整 REX 前缀 (Add/Sub/And/Or/Xor 用).
        let build_rex = |use_32bit: bool| -> u8 {
            let mut rex = if use_32bit { 0x40u8 } else { 0x48u8 };
            if src >= 8 {
                rex |= 0x04;
            }
            if dst >= 8 {
                rex |= 0x01;
            }
            rex
        };
        match op {
            BinOp::Add => {
                let modrm = 0xC0 | r | m;
                let rex = build_rex(use_32bit);
                let bytes: Vec<u8> = if rex != 0x40 {
                    vec![rex, 0x01, modrm]
                } else {
                    vec![0x01, modrm]
                };
                // AT&T: add src, dst (reg 字段=src 在前, r/m=dst 在后).
                let gas = format!("add {src_name}, {dst_name}");
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            },
            BinOp::Sub => {
                let modrm = 0xC0 | r | m;
                let rex = build_rex(use_32bit);
                let bytes: Vec<u8> = if rex != 0x40 {
                    vec![rex, 0x29, modrm]
                } else {
                    vec![0x29, modrm]
                };
                let gas = format!("sub {src_name}, {dst_name}");
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            },
            BinOp::And => {
                let modrm = 0xC0 | r | m;
                let rex = build_rex(use_32bit);
                let bytes: Vec<u8> = if rex != 0x40 {
                    vec![rex, 0x21, modrm]
                } else {
                    vec![0x21, modrm]
                };
                let gas = format!("and {src_name}, {dst_name}");
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            },
            BinOp::Or => {
                let modrm = 0xC0 | r | m;
                let rex = build_rex(use_32bit);
                let bytes: Vec<u8> = if rex != 0x40 {
                    vec![rex, 0x09, modrm]
                } else {
                    vec![0x09, modrm]
                };
                let gas = format!("or {src_name}, {dst_name}");
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            },
            BinOp::Xor => {
                let modrm = 0xC0 | r | m;
                let rex = build_rex(use_32bit);
                let bytes: Vec<u8> = if rex != 0x40 {
                    vec![rex, 0x31, modrm]
                } else {
                    vec![0x31, modrm]
                };
                let gas = format!("xor {src_name}, {dst_name}");
                self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            },
            BinOp::Mul => {
                // imul dst, src: 总是 B64 (REX.W=1). 走 ImulRm typed opcode 保带 GAS.
                let insn =
                    Insn::imul_rm_reg(crate::codegen::shared::asm_operand::Width::B64, dst, src);
                use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                let gas = format!(
                    "imul {}, {}",
                    GAS_REG64[(src & 15) as usize],
                    GAS_REG64[(dst & 15) as usize]
                );
                self.emit_insn(&insn.with_gas(gas));
            },
            BinOp::Div | BinOp::Mod => {
                // 有符号: cqto (符号扩展 rax→rdx:rax) + idiv
                // 无符号: xor rdx,rdx (清零 rdx) + div
                // 32位无符号运算前需零扩展操作数到64位
                let op_size = lt.size().max(rt.size());
                let need_zero_ext = !is_signed && op_size <= 4;
                if src == 2 {
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                            op: crate::codegen::x86_64::opcode::StackOp::Push,
                            width: crate::codegen::shared::asm_operand::Width::B64,
                            dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                2,
                                crate::codegen::shared::asm_operand::Width::B64,
                            ),
                        },
                        &[],
                    )); // push rdx (save divisor)
                }
                if dst != 0 {
                    self.mov(0, dst); // rax = 被除数
                }
                // 零扩展被除数 (rax) 从32位到64位
                if need_zero_ext {
                    self.emit_insn(&Insn::new(
                        crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                            rip_relative: false,
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
                    )); // mov eax, eax (zero-extends to rax)
                }
                if src == 2 {
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
                    )); // pop rcx (restore divisor)
                        // 零扩展除数 (rcx) 从32位到64位
                    if need_zero_ext {
                        self.emit_insn(&Insn::new(
                            crate::codegen::x86_64::opcode::X86_64Opcode::MovRm {
                                rip_relative: false,
                                width: crate::codegen::shared::asm_operand::Width::B32,
                                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                                    1,
                                    crate::codegen::shared::asm_operand::Width::B32,
                                ),
                                src: crate::codegen::x86_64::opcode::Operand::Reg(
                                    1,
                                    crate::codegen::shared::asm_operand::Width::B32,
                                ),
                            },
                            &[],
                        )); // mov ecx, ecx (zero-extends to rcx)
                    }
                    if is_signed {
                        self.emit_insn(&Insn::sign_extend_rax_to_rdx(
                            crate::codegen::shared::asm_operand::Width::B64,
                        )); // cqto
                        self.emit_insn(&Insn::idiv_rm_reg_b64(1)); // idiv rcx
                    } else {
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
                            )); // xor rdx, rdx
                        }
                        self.emit_insn(&Insn::div_rm_reg_b64(1)); // div rcx
                    }
                } else {
                    // 零扩展除数从32位到64位
                    if need_zero_ext {
                        // mov src, src (zero-extends to 64-bit)
                        self.emit_insn(&Insn::mov_rm_reg(
                            crate::codegen::shared::asm_operand::Width::B32,
                            src,
                            src,
                        ));
                    }
                    if is_signed {
                        self.emit_insn(&Insn::sign_extend_rax_to_rdx(
                            crate::codegen::shared::asm_operand::Width::B64,
                        )); // cqto
                        self.emit_insn(&Insn::idiv_rm_reg_b64(src));
                    // idiv src
                    } else {
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
                            )); // xor rdx, rdx
                        }
                        self.emit_insn(&Insn::div_rm_reg_b64(src));
                        // div src
                    }
                }
                if matches!(op, BinOp::Mod) {
                    self.mov(dst, 2);
                } else if dst != 0 {
                    self.mov(dst, 0); // 商在 rax → dst
                }
            },
            BinOp::Shl | BinOp::Shr => {
                // Shl: shl (0xE0)
                // Shr: 有符号用 sar (0xF8, 算术右移), 无符号用 shr (0xE8, 逻辑右移)
                let sub: u8 = if matches!(op, BinOp::Shl) {
                    0xE0
                } else if is_signed {
                    0xF8 // sar: 算术右移, 高位补符号位
                } else {
                    0xE8 // shr: 逻辑右移, 高位补 0
                };
                if src == 1 {
                    // 移位量已在 cl; sub 为 0xE0/0xE8/0xF8, ModRM /n 字段在 bit[5:3].
                    let op_field = (sub >> 3) & 0x07;
                    self.emit_insn(&Insn::shift_reg_cl_b64(op_field, dst));
                } else if dst == 1 {
                    // dst=rcx 且移位量在 src: xchg 后在 src 上移位, 再搬回 rcx
                    self.emit_insn(&Insn::xchg_reg(
                        crate::codegen::shared::asm_operand::Width::B64,
                        1,
                        src,
                        false,
                    )); // xchg rcx, src
                    let op_field = (sub >> 3) & 0x07;
                    self.emit_insn(&Insn::shift_reg_cl_b64(op_field, src)); // shl/shr src, cl
                                                                            // mov rcx, src: 手写用 0x8B (mov r/m64, r64 反向), 非 0x89 形式; 保留 raw.
                    let mut rex = 0x48u8;
                    if src >= 8 {
                        rex |= 0x41;
                    }
                    self.emit_insn(&Insn::raw_bytes(&[rex, 0x8B, 0xC8 | (src & 7)]));
                // mov rcx, src
                } else {
                    // 移位量搬到 cl 后对 dst 移位
                    let mut rex1 = 0x48u8;
                    if src >= 8 {
                        rex1 |= 0x41;
                    }
                    self.emit_insn(&Insn::raw_bytes(&[rex1, 0x8B, 0xC8 | (src & 7)])); // mov rcx, src
                    let op_field = (sub >> 3) & 0x07;
                    self.emit_insn(&Insn::shift_reg_cl_b64(op_field, dst));
                }
            },
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::EqEq | BinOp::Ne => {
                // 确定比较的位宽: 如果两个操作数都是 32 位或更小, 使用 32 位比较
                // 否则使用 64 位比较 (避免函数返回值高位垃圾数据问题)
                let lt_size = lt.size();
                let rt_size = rt.size();
                let use_32bit = lt_size <= 4 && rt_size <= 4;

                // 无符号/混合符号比较: 零扩展操作数到 64 位.
                // 混合比较时, 有符号操作数符号扩展 (高位为 1), 无符号操作数
                // 零扩展 (高位为 0), 64 位 cmp 结果错误. 零扩展修正此问题.
                if !is_signed && !use_32bit {
                    self.zero_extend_for_cmp(dst, lt);
                    self.zero_extend_for_cmp(src, rt);
                }
                // `cmp r/m(dst), r(src)`：dst 在 r/m 字段 ⇒ 只可能贡献 REX.B，src 在 reg
                // 字段 ⇒ 只可能贡献 REX.R。两个字段各自独立判，不可 else-if：P16 把右操作数
                // 暂存号从写死的 %rcx 换成候选表首号（%r8）之后，「src 与 dst 同属高 8 号」
                // 从死分支变成常走分支，漏掉的 REX.B 会让 `cmp %r8d,%r12d` 解码成
                // `cmp %r8d,%esp`（mod=3 rm=100 是非法编码，objdump 直接点名 %esp）。
                // 位序判据由 `rex_matrix::binop_matrix` 穷举锁住，产物级判据见
                // scripts/check_rex_hygiene.sh。
                // P9 方案 C: cmp+setcc+movzbq 合并发射时挂 gas_text, 避免事后反推.
                let (cmp_src_name, cmp_dst_name): (&str, &str) = if use_32bit {
                    (GAS_REG32[(src & 15) as usize], GAS_REG32[(dst & 15) as usize])
                } else {
                    (GAS_REG64[(src & 15) as usize], GAS_REG64[(dst & 15) as usize])
                };
                // AT&T 序: cmp src, dst (reg 字段在前, r/m 字段在后).
                let cmp_insn = Insn::new(
                    crate::codegen::x86_64::opcode::X86_64Opcode::AluRm {
                        op: crate::codegen::x86_64::opcode::AluOp::Cmp,
                        width: if use_32bit {
                            crate::codegen::shared::asm_operand::Width::B32
                        } else {
                            crate::codegen::shared::asm_operand::Width::B64
                        },
                        dst: crate::codegen::x86_64::opcode::Operand::Reg(
                            dst,
                            if use_32bit {
                                crate::codegen::shared::asm_operand::Width::B32
                            } else {
                                crate::codegen::shared::asm_operand::Width::B64
                            },
                        ),
                        src: crate::codegen::x86_64::opcode::Operand::Reg(
                            src,
                            if use_32bit {
                                crate::codegen::shared::asm_operand::Width::B32
                            } else {
                                crate::codegen::shared::asm_operand::Width::B64
                            },
                        ),
                    },
                    &[],
                );
                self.emit_insn(&cmp_insn.with_gas(format!("cmp {cmp_src_name}, {cmp_dst_name}")));
                // 条件直生分支（`gen_cond_flags` 语境）：flags 就是结果，不材料化 0/1。
                // 本函数到此就必须是终点（调用方紧接着发 jcc，中间任何发射都会清掉
                // `last_cmp`），所以这里的 `return` 与上面的 `cmp_flags_granted` 是同一条约定。
                if self.cmp_flags_granted {
                    self.last_cmp = Some(Self::cmp_cc(op, is_signed).1);
                    return;
                }
                // setcc al + movzbq %al, %rax 拆开各自挂 gas.
                let (cc_letter, cond_code) = Self::cmp_cc(op, is_signed);
                self.emit_insn(&Insn::setcc_reg(cond_code, 0).with_gas(format!("{cc_letter} %al")));
                self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x0F, 0xB6, 0xC0], "movzbq %al, %rax"));
                if dst != 0 {
                    self.mov(dst, 0);
                }
            },
            // AndAnd/OrOr 由 gen_expr 短路求值处理, 不应到达此处.
            // 使用显式匹配而非通配符, 确保新增 BinOp 变体时编译器
            // 报 non-exhaustive match 错误 (而非运行时 panic).
            BinOp::AndAnd | BinOp::OrOr => {
                unreachable!("logical ops are lowered via short-circuit in gen_expr")
            },
        }
    }
    pub(crate) fn cc_letter_to_setcc_opcode(letters: &str) -> Option<u8> {
        let off = match letters {
            "o" => 0x00,
            "no" => 0x01,
            "b" | "c" | "nae" => 0x02,
            "ae" | "nb" | "nc" => 0x03,
            "e" | "z" => 0x04,
            "ne" | "nz" => 0x05,
            "be" | "na" => 0x06,
            "a" | "nbe" => 0x07,
            "s" => 0x08,
            "ns" => 0x09,
            "p" | "pe" => 0x0A,
            "np" | "po" => 0x0B,
            "l" | "nge" => 0x0C,
            "ge" | "nl" => 0x0D,
            "le" | "ng" => 0x0E,
            "g" | "nle" => 0x0F,
            _ => return None,
        };
        Some(0x90 + off)
    }
    pub(crate) fn classify_field(ty: &Type, base: usize, classes: &mut [AbiClass]) {
        let size = ty.size();
        if size == 0 {
            return;
        }
        match ty {
            Type::Struct(_, fields, _, false) => {
                for field in fields.iter() {
                    Self::classify_field(&field.ty, base + field.offset, classes);
                }
            },
            Type::Union(_, fields, _, false) => {
                // 联合各字段共享同一基址, 各自分类后合并
                for field in fields.iter() {
                    let f_sz = field.ty.size();
                    if f_sz == 0 {
                        continue;
                    }
                    let n = f_sz.div_ceil(8).min(classes.len() - base / 8);
                    let mut f_classes = vec![AbiClass::NoClass; n];
                    Self::classify_field(&field.ty, 0, &mut f_classes);
                    let start = base / 8;
                    for i in 0..n {
                        if start + i < classes.len() {
                            classes[start + i] =
                                Self::merge_classes(classes[start + i], f_classes[i]);
                        }
                    }
                }
            },
            Type::Array(elem, ArraySize::Const(n)) => {
                for j in 0..*n {
                    Self::classify_field(&elem.base, base + j * elem.size(), classes);
                }
            },
            Type::Atomic(inner) => Self::classify_field(&inner.base, base, classes),
            // binary128 与 SIMD 向量同形: 首 eightbyte Sse, 后续 SseUp。
            // 不能走叶类型分支 (它会把两个 eightbyte 都标成 Sse, 变成**两个**
            // 独立 XMM 寄存器); SseUp 表示"前一 SSE 寄存器的高 8 字节",
            // 合计只占一个 16 字节 XMM 槽 —— 与 gcc 实测的 a→xmm0/b→xmm1 一致。
            Type::Vector(_, _) | Type::Float128 => {
                // SIMD 向量: 首 eightbyte Sse, 后续 SseUp (向量延续)
                let start = base / 8;
                let end = (base + size.saturating_sub(1)) / 8;
                for i in start..=end {
                    if i < classes.len() {
                        let cls = if i == start { AbiClass::Sse } else { AbiClass::SseUp };
                        classes[i] = Self::merge_classes(classes[i], cls);
                    }
                }
            },
            _ => {
                // 叶类型: 直接分类到 classes
                let start = base / 8;
                let end = (base + size.saturating_sub(1)) / 8;
                let cls = Self::leaf_abi_class(ty);
                for i in start..=end {
                    if i < classes.len() {
                        classes[i] = Self::merge_classes(classes[i], cls);
                    }
                }
            },
        }
    }
    // complex 静态初始化器只有一份真源：mem/global.rs。历史上这里有第二份
    // 逐字副本（走 elf/init_list.rs），只改一份会出现“测试全绿但 .data
    // 现象不变”。以下关联函数全部委托 global，保证两条路径语义一致。
    pub(crate) fn complex_init_bytes(init: &Expr, inner: &Type) -> Option<Vec<u8>> {
        crate::codegen::x86_64::mem::global::complex_init_bytes(init, inner)
    }
    fn complex_init_parts(e: &Expr, inner: &Type) -> Option<(FloatConst, FloatConst)> {
        crate::codegen::x86_64::mem::global::complex_init_parts(e, inner)
    }
    fn complex_init_parts_i(e: &Expr) -> Option<(i128, i128)> {
        crate::codegen::x86_64::mem::global::complex_init_parts_i(e)
    }
    /// 浮点初始化器 → 数据字节的**单一真源**。
    ///
    /// 接收完整的 FloatConst：LongDouble 直出真 80-bit 编码（10 字节），
    /// 目标 16 字节对象再补 6 字节 padding；F32/F64 按目标宽度截断。
    /// 以前这些站点各自内联写"低 8 字节 = f64 位型 + 高 8 零"，
    /// 而读侧在对象表示迁移后已统一改成 `fld tbyte` —— 于是写出的字节被
    /// 当 80-bit 解释：`long double g = 1.0L` 的 f64 位型落在 80-bit 的
    /// 尾数域，指数域变成高两个零字节 → 指数 0 且整数位 0 → 读回 `-nan`。
    pub(crate) fn float_data_bytes(c: FloatConst, size: usize) -> Vec<u8> {
        match c {
            FloatConst::LongDouble(b) => {
                // 真 80-bit 编码直出；16 字节对象补 6 字节 padding。
                let mut out = b.to_vec();
                if out.len() < size {
                    out.resize(size, 0);
                }
                if out.len() > size {
                    out.truncate(size);
                }
                out
            },
            FloatConst::F32(f) => {
                let n = size.clamp(1, 4);
                f.to_le_bytes()[..n].to_vec()
            },
            FloatConst::F64(f) => {
                if size <= 4 {
                    let n = size.max(1);
                    return (f as f32).to_bits().to_le_bytes()[..n].to_vec();
                }
                // 目标是 long double (16 字节对象): 把 f64 提升为真 80-bit
                // 编码. 否则写出去的 f64 位型会被读侧 `fld tbyte` 当 80-bit
                // x87 解释成 garbage long double. 这是 960215-1 / align-2 中
                // 形如 `long double C = 2;` / `{ 'e', 27.0 }` 这种 "字面量
                // 被求值成 F64 字段却要落到 16 字节槽" 场景的兜底修复.
                if size >= 16 {
                    let ld = crate::float80::from_f64(f);
                    let mut out = ld.to_vec();
                    out.resize(size, 0);
                    return out;
                }
                let bytes = f.to_bits().to_le_bytes();
                let mut b = bytes[..size.min(8)].to_vec();
                if b.len() < size {
                    b.resize(size, 0);
                }
                b
            },
            FloatConst::Float128(bits) => {
                // binary128 常量落到**非** binary128 目标（float/double/long
                // double）：必须按目标宽度转换后编码 —— 直接把 16 字节
                // binary128 位型写进 long double 槽（x87 80-bit）会被读侧
                // 解析成垃圾。binary128 目标走 float128_data_bytes，不经这里。
                if size >= 16 {
                    let mut out = crate::float128::to_f80(&bits).to_vec();
                    out.resize(size, 0);
                    out
                } else if size <= 4 {
                    let n = size.clamp(1, 4);
                    crate::float128::to_f32(&bits).to_bits().to_le_bytes()[..n].to_vec()
                } else {
                    crate::float128::to_f64(&bits).to_bits().to_le_bytes()[..size.min(8)].to_vec()
                }
            },
        }
    }

    /// binary128 目标类型的浮点初始化器 → 数据字节。
    ///
    /// 与 [`Self::float_data_bytes`] 分家的原因: 后者的 `size >= 16` 分支把
    /// f64 拓宽成 **真 80-bit x87** 编码（那是 `long double` 的对象表示）；
    /// binary128 必须是 16 字节 IEEE quad 位型（113 位尾数），两者字节布局
    /// 完全不同。目标类型同为 16 字节时无法靠 `size` 区分，必须显式走这里。
    pub(crate) fn float128_data_bytes(c: FloatConst, size: usize) -> Vec<u8> {
        let bits: [u8; 16] = match c {
            FloatConst::Float128(b) => b,
            // 非 binary128 的来源: 先拓宽到 binary128（f64 ⊂ binary128, 无损）。
            other => crate::float128::from_f64(other.as_f64()),
        };
        let mut out = bits.to_vec();
        if out.len() > size {
            out.truncate(size);
        } else {
            out.resize(size, 0);
        }
        out
    }
    fn encode_complex_parts_f(re: FloatConst, im: FloatConst, inner: &Type) -> Vec<u8> {
        crate::codegen::x86_64::mem::global::encode_complex_parts_f(re, im, inner)
    }
    fn encode_complex_parts_i(re: i128, im: i128, inner: &Type) -> Vec<u8> {
        crate::codegen::x86_64::mem::global::encode_complex_parts_i(re, im, inner)
    }
    pub(crate) fn eval_asm_operand(expr: &Expr) -> Option<String> {
        match const_eval::eval_full(expr, false) {
            Ok(ConstValue::Int(v)) => return Some(v.to_string()),
            Ok(ConstValue::Float(f)) => return Some((f.as_f64() as i128).to_string()),
            Ok(ConstValue::Addr { name, offset }) => {
                return Some(Self::format_asm_symbol_immediate(&name, offset));
            },
            Ok(ConstValue::Str { label, .. }) | Ok(ConstValue::Bytes { label, .. }) => {
                return Some(label);
            },
            Err(_) => {},
        }
        if let Expr::Offsetof { ty, member, .. } = expr {
            return Some(Self::compute_offset(ty, member).to_string());
        }
        None
    }
    pub(crate) fn eval_const_expr(
        &mut self,
        e: &Expr,
        rel: Option<(u64, Option<usize>)>,
        width: usize,
        target_ty: &Type,
        rodata_slots: bool,
    ) -> Vec<u8> {
        // 目标类型决定浮点字节编码: float/double/long double/binary128 的
        // 对象表示各不相同 (binary128 是 16B IEEE quad, 不是 80-bit x87),
        // 只靠 `width == 16` 无法区分 long double 与 binary128 ⇒ 必须带类型。
        let is_float = target_ty.is_real_float();
        let is_f128 = matches!(target_ty, Type::Float128);
        // 字符串字面量: 不能走 eval_full —— 它返回 ConstValue::Str, 落到
        // 下方 _ 分支会被 0 填充 (char buf[][8] = {"hello"} 元素全为 0)。
        // 指针类型由调用方在 Ptr 上下文特判 (emit_string_ptr_slot);
        // 此处 (char 数组元素等) 返回内容字节。
        if let Expr::String { data, .. } = const_eval::unwrap_string_cast(e) {
            // 截断到 width (如 char[3] = "1234" 只取前 3 字节 "123")
            let mut d = data.clone();
            if width > 0 && d.len() > width {
                d.truncate(width);
            }
            return d;
        }
        // 整型复数字面量: 两分量按 i128 精确 LE 编码，全程不经浮点。
        if let Expr::ComplexIntLit { real, imag, ty, .. } = e {
            let inner = match ty {
                Type::Complex(i) | Type::Imaginary(i) => &i.base,
                other => other,
            };
            let comp_sz = inner.size().max(1).min(16);
            let mut data: Vec<u8> = Vec::new();
            for v in [*real, *imag] {
                data.extend_from_slice(&v.to_le_bytes()[..comp_sz]);
            }
            if width > 0 && data.len() > width {
                data.truncate(width);
            }
            while data.len() < width {
                data.push(0);
            }
            return data;
        }
        // 复数字面量: 必须特殊处理, 因为 eval_full 只返回虚部
        if let Expr::ComplexLit { real, imag, ty, .. } = e {
            // 分量编码由字面量自身类型的 inner 决定: Float→f32 位模式,
            // Double/LongDouble→f64 位模式 (LD 补零到 16B 槽),
            // 整数→整数值 LE 字节。不能用 width/2 猜测分量宽度:
            // width 可能是单分量尺寸 (Complex arm) 或全复数尺寸, 且
            // 整数 inner 目标必须是整数值而非 f64/f32 位模式。
            let inner = match ty {
                Type::Complex(i) | Type::Imaginary(i) => &i.base,
                other => other,
            };
            let comp_sz = inner.size().max(1);
            let mut data: Vec<u8> = Vec::new();
            for v in [*real, *imag] {
                match inner {
                    Type::Float => {
                        data.extend_from_slice(&(v.as_f64() as f32).to_bits().to_le_bytes())
                    },
                    Type::LongDouble => {
                        // 真 80-bit 编码（16B 槽 = 10 字节 + 6 字节 padding）
                        let mut bits = match v {
                            FloatConst::LongDouble(b) => b.to_vec(),
                            other => crate::float80::from_f64(other.as_f64()).to_vec(),
                        };
                        bits.resize(16, 0);
                        data.extend_from_slice(&bits);
                    },
                    Type::Double => data.extend_from_slice(&v.as_f64().to_bits().to_le_bytes()),
                    _ => {
                        let b = (v.as_f64() as i128).to_le_bytes();
                        data.extend_from_slice(&b[..comp_sz.min(16)]);
                    },
                }
            }
            // 分量上下文 (width < 两分量) 只取前部; 不足补零到 width
            if width > 0 && data.len() > width {
                data.truncate(width);
            }
            while data.len() < width {
                data.push(0);
            }
            return data;
        }
        // 优先用 eval_full: 浮点字面量直接走 .to_bits(), 不被截断为整数
        // &&label: 提前处理, 前缀标签名以避免与函数符号名冲突。
        // Cast 包装（`(void*)&&lab` / `(int)&&lab`）先剥掉再判：跳转表的元素常带
        // 显式转换，漏剥就会掉到下面的通用路径把整项当 0。
        let peeled = Self::peel_value_casts(e);
        if let Expr::LabelAddr { label, .. } = peeled {
            let size = width.max(8);
            if let Some((off, sec)) = rel {
                let prefixed = self.c_label_to_asm_resolve_nested(&label.name);
                // 文本标号不进 symtab（`.L` 约定）⇒ 走「.text 节符号 + addend」：
                // ET_EXEC 由 resolve() 就地回填，ET_REL 由 emit_rel_relocations 发
                // R_X86_64_64。旧实现推 `pending_data64_rels`（按符号名进 symtab），
                // 对 `.Ll_*` 永远解析不到符号，那条路实际是死的。
                self.pending_data_textlabel_rels.push((off, prefixed, 0));
                let _ = sec;
            }
            return vec![0u8; size];
        }
        // &&label_a - &&label_b: label address difference (computed goto tables).
        // 编译期尚不知 label 偏移, 记录待回填, 函数编译完后用 self.labels
        // 已知偏移计算差值. 同时处理 Cast 包装: (int)&&lab_a - (int)&&lab_b.
        if let Expr::Binop {
            op: crate::ast::BinOp::Sub,
            l,
            r,
            ..
        } = e
        {
            let (la, lb) = (Self::extract_label_addr(l), Self::extract_label_addr(r));
            if let (Some(label_a), Some(label_b)) = (la, lb) {
                let prefixed_a = self.c_label_to_asm_resolve_nested(&label_a);
                let prefixed_b = self.c_label_to_asm_resolve_nested(&label_b);
                if let Some((off, _sec)) = rel {
                    let size = width.max(4);
                    self.pending_label_diff_rels
                        .push((off, prefixed_a, prefixed_b, size));
                }
                return vec![0u8; width.max(4)];
            }
        }
        // &(T){...} / &{...}: 嵌套 AddrOf+CompoundLit 路径
        // (gen_global_init_list 处理 struct 指针字段时走 eval_const_expr,
        // 而 eval_full 不支持 CompoundLit 取地址, 必须显式 materialize)
        if let Expr::AddrOf { e: inner, .. } = e {
            if self.relocatable {
                // ET_REL: 物化到 .rodata, 登记 LOCAL 符号, 返回占位+重定位
                if let Some((anon_name, rodata_off)) = self.materialize_compound_literal_rel(inner)
                {
                    let sz = width.max(8);
                    let off = rel.map(|(o, _)| o).unwrap_or(0);
                    let sec = rel.and_then(|(_, s)| s);
                    let rel_type = if width <= 4 { R_X86_64_32 } else { R_X86_64_64 };
                    if matches!(sec, Some(v) if v == usize::MAX) {
                        // 当前在 rodata section 内: 发射 .rela.rodata 重定位
                        self.pending_rodata_rels.push((off, anon_name, rel_type, 0));
                    } else {
                        // 在 data section (或 custom section) 内: 走 data 重定位
                        self.pending_data64_rels
                            .push((off, anon_name, sec, rel_type, 0));
                    }
                    let _ = rodata_off; // 符号已登记, 链接时自动解析
                    return vec![0u8; sz];
                }
            } else {
                // ET_EXEC: 物化, 返回绝对地址字节 (compile_exec 直接写死)
                if let Some((anon_name, vaddr)) =
                    self.materialize_compound_literal(inner, rodata_slots)
                {
                    let sz = inner.ty().size().max(1);
                    let rodata_offset = vaddr - self.entry;
                    self.globals_raw.insert(
                        anon_name.clone(),
                        GlobalVar {
                            offset: rodata_offset,
                            size: sz,
                            ty: inner.ty().clone(),
                            init: None,
                            section: 1,
                            is_tls: false,
                            in_data_section: false,
                        },
                    );
                    return vaddr.to_le_bytes().to_vec();
                }
            }
        }
        if let Ok(cv) = const_eval::eval_full(e, false) {
            match cv {
                const_eval::ConstValue::Float(f) => {
                    // 按目标宽度截断: 窄元素 (char/short) 数组不能用表达式
                    // 类型宽度 (int=4) 写数据, 否则元素会错位/越界。
                    // LongDouble 经单一真源直出**真 80-bit 编码**。
                    if is_f128 {
                        // binary128 目标: 16 字节 binary128 位型（不是 80-bit x87!）
                        Self::float128_data_bytes(f, width.max(1))
                    } else {
                        // 目标不是 binary128: 常量先转换到目标精度 —— binary128
                        // 的 16 字节位型不能直接写进 long double 槽。
                        Self::float_data_bytes(f.coerce_to(target_ty), width.max(1))
                    }
                },
                const_eval::ConstValue::Int(v) => {
                    let size = width.max(1);
                    if is_f128 {
                        // binary128 目标: 整型常量在 113 位尾数上精确 RNE 转换。
                        Self::float128_data_bytes(FloatConst::from_int(&Type::Float128, v), size)
                    } else if is_float {
                        // 浮点目标：整型常量在目标精度上精确 RNE 转换
                        // （旧写法经 `v as f64` 中转，|v| >= 2^53 会丢精度）。
                        let target_ty = if size >= 10 {
                            Type::LongDouble
                        } else if size <= 4 {
                            Type::Float
                        } else {
                            Type::Double
                        };
                        let c = FloatConst::from_int(&target_ty, v);
                        Self::float_data_bytes(c, size)
                    } else {
                        let bytes = v.to_le_bytes();
                        bytes[..size.min(bytes.len())].to_vec()
                    }
                },
                const_eval::ConstValue::Addr { name, offset } => {
                    // &&label or &global: emit data relocation
                    // &&label names are prefixed with .Ll_ to avoid collision
                    // with function symbol names
                    let size = width.max(8);
                    // 静态局部初始化式里引用同函数的其它静态局部时 (数组类型会衰变为
                    // 其地址, 经 const_eval 得到 ConstValue::Addr{name=源名}), 必须把
                    // 源名解析到 mangled 名 `.Lsl<N>.<func>.<name>`, 否则裸名在链接期
                    // undefined reference (如 `static struct B f3 = { b, ... };` 引用
                    // `static char b[..]`)。
                    let sym = if let Some(owner) = &self.static_local_label_owner {
                        self.static_local_map
                            .resolve(owner, &name)
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| name.clone())
                    } else {
                        name.clone()
                    };
                    if let Some((off, sec)) = rel {
                        let rel_type = if width <= 4 { R_X86_64_32 } else { R_X86_64_64 };
                        self.pending_data64_rels
                            .push((off, sym, sec, rel_type, offset as i64));
                    }
                    vec![0u8; size]
                },
                _ => {
                    if let Ok(value) = const_eval::eval(e) {
                        let size = width.max(1);
                        let bytes = value.to_le_bytes();
                        return bytes[..size.min(bytes.len())].to_vec();
                    }
                    vec![0u8; width.max(e.ty().size())]
                },
            }
        } else {
            match e {
                Expr::String { data, .. } => data.clone(),
                Expr::Ident { name, .. } => {
                    // 静态局部初始化式里引用同函数的其它静态局部时 (如
                    // `static struct B f3 = { b, ... };` 引用 `static char b[..]`)，
                    // 必须把源名解析到 mangled 名 `.Lsl<N>.<func>.<name>`，否则引用裸名
                    // 会在链接期 undefined reference。仅当处于 static local 初始化上下文
                    // (static_local_label_owner 有值且确为该函数的静态局部) 才替换。
                    let sym = if let Some(owner) = &self.static_local_label_owner {
                        self.static_local_map
                            .resolve(owner, &name.name)
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| name.name.clone())
                    } else {
                        name.name.clone()
                    };
                    if let Some((off, sec)) = rel {
                        let rel_type = if width <= 4 { R_X86_64_32 } else { R_X86_64_64 };
                        self.pending_data64_rels
                            .push((off, sym, sec, rel_type, 0));
                    }
                    vec![0u8; width.max(8)]
                },
                Expr::AddrOf { e: inner, .. } => match inner.as_ref() {
                    Expr::Ident { .. } => {
                        self.eval_const_expr(inner, rel, width, target_ty, rodata_slots)
                    },
                    Expr::Index {
                        arr,
                        idx,
                        ty: elem_ty,
                        ..
                    } => {
                        if let (Expr::Ident { name, .. }, Ok(i)) =
                            (arr.as_ref(), const_eval::eval(idx))
                        {
                            let sym = if let Some(owner) = &self.static_local_label_owner {
                                self.static_local_map
                                    .resolve(owner, &name.name)
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| name.name.clone())
                            } else {
                                name.name.clone()
                            };
                            if let Some((off, sec)) = rel {
                                let rel_type = if width <= 4 { R_X86_64_32 } else { R_X86_64_64 };
                                let addend = (i as i64) * elem_ty.size().max(1) as i64;
                                self.pending_data64_rels.push((
                                    off,
                                    sym,
                                    sec,
                                    rel_type,
                                    addend,
                                ));
                            }
                            vec![0u8; width.max(8)]
                        } else if let (Expr::String { data, .. }, Ok(i)) =
                            (arr.as_ref(), const_eval::eval(idx))
                        {
                            // &"str"[i]: string literal element address.
                            // Use string pool to ensure the string is in
                            // rodata and the pointer slot is patched to
                            // the absolute address (ET_EXEC) or has a
                            // proper relocation (ET_REL).
                            let byte_off = (i as usize).min(data.len());
                            self.emit_string_ptr_slot(rel, &data[byte_off..], rodata_slots)
                        } else {
                            vec![0u8; width.max(e.ty().size())]
                        }
                    },
                    _ => vec![0u8; width.max(e.ty().size())],
                },
                // Strip casts to handle (void *)&"str"[0],
                // (char *)&"hello"[1], etc.
                Expr::Cast { e: inner, .. } => {
                    self.eval_const_expr(inner, rel, width, target_ty, rodata_slots)
                },
                // String pointer arithmetic: "str" + n, n + "str",
                // with optional Cast wrappers.
                Expr::Binop {
                    op: crate::ast::BinOp::Add,
                    l,
                    r,
                    ..
                } if matches!(l.ty(), Type::Array(_, _) | Type::Ptr(_))
                    || matches!(r.ty(), Type::Array(_, _) | Type::Ptr(_)) =>
                {
                    self.eval_string_ptr_arith(l, r, rel, width, rodata_slots)
                },
                Expr::LabelAddr { label, .. } => {
                    // &&label in data section: record for later patching
                    // Prefix label name to avoid collision with function symbols
                    if let Some((off, sec)) = rel {
                        let rel_type = if width <= 4 { R_X86_64_32 } else { R_X86_64_64 };
                        let prefixed = self.c_label_to_asm(&label.name);
                        self.pending_data64_rels
                            .push((off, prefixed, sec, rel_type, 0));
                    }
                    vec![0u8; width.max(8)]
                },
                _ => vec![0u8; width.max(e.ty().size())],
            }
        }
    }
    pub(crate) fn eval_global_lvalue_offset(
        e: &Expr,
        globals: &std::collections::HashMap<String, GlobalVar>,
    ) -> Option<(String, i64)> {
        match e {
            Expr::Ident { name, .. } => {
                if globals.contains_key(&name.name) {
                    Some((name.name.clone(), 0))
                } else {
                    None
                }
            },
            Expr::Index { arr, idx, .. } => {
                let (base, off) = Self::eval_global_lvalue_offset(arr, globals)?;
                // Evaluate the index as a constant
                let idx_val = const_eval::eval(idx).ok()?;
                // Element size = sizeof the type that arr points to
                // (i.e., the element type of the array)
                let arr_ty = arr.ty();
                let elem_sz = match &arr_ty {
                    Type::Array(elem, _) => elem.size() as i128,
                    Type::Ptr(elem) => elem.size() as i128,
                    _ => arr_ty.size() as i128,
                };
                Some((base, off + (idx_val * elem_sz) as i64))
            },
            Expr::Member {
                expr: inner, field, ..
            } => {
                let (base, off) = Self::eval_global_lvalue_offset(inner, globals)?;
                // Compute field offset within the struct/union
                let inner_ty = inner.ty();
                // For arrow (->), dereference the pointer first
                let struct_ty: &Type = match &inner_ty {
                    Type::Ptr(ref t) => &t.base,
                    other => other,
                };
                let field_off = match struct_ty {
                    Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, false) => {
                        let mut cur = 0i64;
                        let is_union = matches!(struct_ty, Type::Union(_, _, _, _));
                        for f in fields.iter() {
                            if is_union {
                                if f.name == field.name {
                                    cur = 0;
                                    break;
                                }
                            } else {
                                let f_align = f.ty.align() as i64;
                                cur = (cur + f_align - 1) & !(f_align - 1);
                                if f.name == field.name {
                                    break;
                                }
                                cur += f.ty.size() as i64;
                            }
                        }
                        cur
                    },
                    _ => return None,
                };
                Some((base, off + field_off))
            },
            Expr::Deref { .. } => {
                // &(*ptr) = ptr, but ptr must be a constant address
                // This is rare in global init; skip for now
                None
            },
            _ => None,
        }
    }
    fn eval_string_ptr_arith(
        &mut self,
        l: &Expr,
        r: &Expr,
        rel: Option<(u64, Option<usize>)>,
        width: usize,
        rodata_slots: bool,
    ) -> Vec<u8> {
        // Strip casts to find the underlying expressions
        let l_inner = Self::strip_casts(l);
        let r_inner = Self::strip_casts(r);
        // Try to find (String, Int) or (Int, String) pattern
        let (data, offset) = match (l_inner, r_inner) {
            (Expr::String { data, .. }, _) => {
                if let Ok(n) = const_eval::eval(r) {
                    (data, n as usize)
                } else {
                    return vec![0u8; width.max(8)];
                }
            },
            (_, Expr::String { data, .. }) => {
                if let Ok(n) = const_eval::eval(l) {
                    (data, n as usize)
                } else {
                    return vec![0u8; width.max(8)];
                }
            },
            _ => return vec![0u8; width.max(8)],
        };
        let byte_off = offset.min(data.len());
        self.emit_string_ptr_slot(rel, &data[byte_off..], rodata_slots)
    }
    /// 副作用判据：**实体已移到后端无关的语义层**（`crate::semantics::expr_has_side_effects`）。
    ///
    /// 为什么：同一件判据 codegen（复合赋值单求值）与 SSA 构建器（自增自减/复合赋值的
    /// 读—改—写）两边都要用，只写在后端里会让 SSA 侧没法区分「重复降级无害」与
    /// 「把 `i++` 跑两遍」。这里保留 `Self::` 入口是为了不拆改动 20 多个调用点。
    pub(crate) fn expr_has_side_effects(e: &Expr) -> bool {
        crate::semantics::expr_has_side_effects(e)
    }
    pub(crate) fn expr_span(e: &Expr) -> &crate::diagnostics::Span {
        match e {
            Expr::Int { span, .. }
            | Expr::Float { span, .. }
            | Expr::ComplexLit { span, .. }
            | Expr::ComplexIntLit { span, .. }
            | Expr::Real { span, .. }
            | Expr::Imag { span, .. }
            | Expr::String { span, .. }
            | Expr::Char { span, .. }
            | Expr::Ident { span, .. }
            | Expr::Binop { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Call { span, .. }
            | Expr::Assign { span, .. }
            | Expr::Cond { span, .. }
            | Expr::Cast { span, .. }
            | Expr::SizeofTy { span, .. }
            | Expr::SizeofExpr { span, .. }
            | Expr::Index { span, .. }
            | Expr::Member { span, .. }
            | Expr::AddrOf { span, .. }
            | Expr::Deref { span, .. }
            | Expr::PostInc { span, .. }
            | Expr::PostDec { span, .. }
            | Expr::PreInc { span, .. }
            | Expr::PreDec { span, .. }
            | Expr::CompoundLit { span, .. }
            | Expr::InitList { span, .. }
            | Expr::AlignofTy { span, .. }
            | Expr::AlignofExpr { span, .. }
            | Expr::StmtExpr { span, .. }
            | Expr::Comma { span, .. }
            | Expr::AsmExpr { span, .. }
            | Expr::LabelAddr { span, .. }
            | Expr::VaArg { span, .. }
            | Expr::VaArgPack { span, .. }
            | Expr::Offsetof { span, .. } => span,
        }
    }
    fn expr_src_pos(&self, span: &crate::diagnostics::Span) -> (String, u32, u32) {
        let fallback_file = if span.file.is_empty() {
            self.source_name.clone()
        } else {
            span.file.to_string()
        };
        let line = span.line;
        if line == 0 || self.line_map.is_empty() {
            return (fallback_file, line, span.col);
        }
        let idx = self.line_map.partition_point(|s| s.out_start <= line);
        if let Some(ls) = idx.checked_sub(1).and_then(|i| self.line_map.get(i)) {
            if line < ls.out_start + ls.count {
                return (ls.file.clone(), ls.src_start + (line - ls.out_start), span.col);
            }
            return (ls.file.clone(), line, span.col);
        }
        if let Some(first) = self.line_map.first() {
            return (first.file.clone(), first.src_start.max(1), span.col);
        }
        (fallback_file, line, span.col)
    }
    /// 结构相等判据：**实体已移到后端无关的语义层**，与上面的副作用判据同一理由
    /// （复合赋值的「r 里哪一半是 lvalue 拷贝」在 codegen 与 SSA 两侧都要判）。
    pub(crate) fn exprs_structurally_equal(a: &Expr, b: &Expr) -> bool {
        crate::semantics::exprs_structurally_equal(a, b)
    }
    pub(crate) fn expr_warn(&self, e: &Expr, msg: &str) {
        eprintln!("{}", self.expr_warning(e, msg));
    }
    fn expr_warning(&self, e: &Expr, msg: &str) -> String {
        let span = Self::expr_span(e);
        let (file, line, col) = self.expr_src_pos(span);
        if line > 0 && col > 0 {
            format!("{}:{}:{}: warning: {}", file, line, col, msg)
        } else if line > 0 {
            format!("{}:{}: warning: {}", file, line, msg)
        } else {
            format!("{}: warning: {}", file, msg)
        }
    }
    fn extract_label_addr(e: &Expr) -> Option<String> {
        let mut cur = e;
        loop {
            match cur {
                Expr::LabelAddr { label, .. } => return Some(label.name.clone()),
                Expr::Cast { e: inner, .. } => cur = inner.as_ref(),
                _ => return None,
            }
        }
    }
    /// 剥掉**值转换**属性的 Cast（`(void*)&&lab` / `(int)&&lab`），露出被包表达式。
    /// 与 `extract_label_addr` 同一个循环形状：只剥 Cast，别的一律停在原地。
    pub(crate) fn peel_value_casts(e: &Expr) -> &Expr {
        let mut cur = e;
        while let Expr::Cast { e: inner, .. } = cur {
            cur = inner.as_ref();
        }
        cur
    }
    pub(crate) fn find_expr_operator(expr: &str) -> (&str, Option<char>, &str) {
        let bytes = expr.as_bytes();
        let mut depth = 0i32;
        let mut in_quote = false;
        // 从右向左扫描，找到最右边的 +/- (不在括号/引号内)
        for i in (0..bytes.len()).rev() {
            match bytes[i] {
                b'"' => in_quote = !in_quote,
                b')' if !in_quote => depth += 1,
                b'(' if !in_quote => depth -= 1,
                b'+' | b'-' if !in_quote && depth == 0 && i > 0 => {
                    return (&expr[..i], Some(bytes[i] as char), &expr[i + 1..]);
                },
                _ => {},
            }
        }
        (expr, None, "")
    }
    fn format_asm_symbol_immediate(name: &str, offset: i128) -> String {
        match offset.cmp(&0) {
            std::cmp::Ordering::Greater => format!("{name}+{offset}"),
            std::cmp::Ordering::Less => format!("{name}{offset}"),
            std::cmp::Ordering::Equal => name.to_string(),
        }
    }
    pub(crate) fn global_init_bytes(
        init: &Expr,
        sz: usize,
        is_float: bool,
        ty: &Type,
    ) -> Option<Vec<u8>> {
        // 单一真源：全局/静态初始化字节一律由 mem/global.rs 负责，
        // 这里仅保留关联函数入口供 elf/init_list.rs 调用，避免再出现
        // 一份经 f64 中转的副本（见 global.rs 中的根因注释）。
        crate::codegen::x86_64::mem::global::global_init_bytes(init, sz, is_float, ty)
    }
    // 号→名（含 AT&T 形态）只有一份表：`abi/regnames.rs::att_name_of_num`
    // （Phase 6.1 的后半）。这里以前是与 `asm/expansion.rs` 逐字相同的第二份副本。
    pub(crate) fn has_field_data_in_range(ty: &Type, lo: usize, hi: usize) -> bool {
        match ty {
            Type::Struct(_, fields, _, false) => {
                for f in fields.iter() {
                    let f_lo = f.offset;
                    let f_hi = f.offset + f.ty.size();
                    if f_lo < hi && f_hi > lo {
                        return true;
                    }
                }
                false
            },
            Type::Union(_, fields, _, false) => {
                for f in fields.iter() {
                    if 0 < hi && f.ty.size() > lo {
                        return true;
                    }
                }
                false
            },
            Type::Array(elem, ArraySize::Const(n)) => {
                for j in 0..*n {
                    let e_lo = j * elem.size();
                    let e_hi = e_lo + elem.size();
                    if e_lo < hi && e_hi > lo {
                        return true;
                    }
                }
                false
            },
            Type::Atomic(inner) => Self::has_field_data_in_range(&inner.base, lo, hi),
            // 标量/向量/浮点: 整个范围都是数据
            _ => true,
        }
    }
    pub(crate) fn is_asm_directive(name: &str) -> bool {
        matches!(
            name,
            ".pushsection"
                | ".popsection"
                | ".section"
                | ".previous"
                | ".type"
                | ".size"
                | ".ident"
                | ".globl"
                | ".global"
                | ".weak"
                | ".hidden"
                | ".protected"
                | ".balign"
                | ".p2align"
                | ".align"
                | ".long"
                | ".quad"
                | ".byte"
                | ".asciz"
                | ".string"
                | ".text"
                | ".data"
                | ".bss"
                | ".rodata"
                | ".org"
                | ".zero"
                | ".skip"
                | ".set"
                | ".equ"
                | ".macro"
                | ".endm"
                | ".if"
                | ".endif"
                | ".else"
                | ".elseif"
                | ".rept"
                | ".endr"
                | ".irp"
                | ".irpc"
                | ".loc"
                | ".file"
                | ".symver"
                | ".local"
                | ".comm"
                | ".lcomm"
                | ".altmacro"
                | ".code64"
                | ".subsection"
                | ".uleb128"
                | ".sleb128"
                | ".2byte"
                | ".4byte"
                | ".8byte"
                | ".word"
                | ".int"
                | ".error"
                | ".warning"
                | ".include"
                | ".incbin"
                | ".cfi_startproc"
                | ".cfi_endproc"
                | ".cfi_def_cfa_offset"
                | ".cfi_def_cfa"
                | ".cfi_offset"
                | ".cfi_restore"
                | ".cfi_remember_state"
                | ".cfi_restore_state"
                | ".cfi_adjust_cfa_offset"
                | ".cfi_def_cfa_register"
                | ".cfi_return_column"
                | ".cfi_signal_frame"
                | ".cfi_window_save"
                | ".cfi_negate_ra_state"
        )
    }
    pub(crate) fn is_char_array(ty: &Type) -> bool {
        matches!(
            ty,
            Type::Array(e, _)
                if matches!(e.base, Type::Char | Type::SChar | Type::UChar)
        )
    }
    pub(crate) fn is_global_object(&self, source_name: &str) -> bool {
        self.lookup_global_info(source_name).is_some()
            || self
                .static_local_map
                .is_static_local(&self.current_func_name, source_name)
    }
    fn leaf_abi_class(ty: &Type) -> AbiClass {
        match ty {
            Type::Float | Type::Double => AbiClass::Sse,
            Type::LongDouble => AbiClass::Memory,
            // binary128: SysV ABI 分类 [Sse, SseUp] —— 占**一个** XMM 寄存器
            // 的 16 字节 (SseUp 是前一 SSE 的高半, 不再额外消耗槽位)。
            // gcc -O0 实测: `f(__float128 a, __float128 b)` 装 a→xmm0, b→xmm1。
            Type::Float128 => AbiClass::Sse,
            // _Complex 分类跟随实数内型 (SysV ABI 3.2.3: 由分量决定):
            // float/double → SSE, long double → Memory,
            // 整数 (_Complex int/long) → INTEGER。此前整数 _Complex
            // 误为 SSE, 参数/返回走错寄存器组 (complex-6)。
            Type::Complex(inner) => Self::leaf_abi_class(&inner.base),
            // _Imaginary float/double: 同 SSE
            Type::Imaginary(inner) if matches!(&inner.base, Type::Float | Type::Double) => {
                AbiClass::Sse
            },
            // SIMD 向量: 首 eightbyte SSE, 后续 SseUp
            Type::Vector(_, _) => AbiClass::Sse,
            // 整数/指针/枚举等
            _ => AbiClass::Integer,
        }
    }
    pub(crate) fn materialize_compound_literal(
        &mut self,
        e: &Expr,
        use_rodata: bool,
    ) -> Option<(String, u64)> {
        let (ty, inits) = match e {
            Expr::CompoundLit { ty, inits, .. } => (ty.clone(), inits.clone()),
            Expr::InitList { inits, ty, .. } => (ty.clone(), inits.clone()),
            _ => return None,
        };
        let sz = ty.size().max(1);
        let align = ty.align().max(1);
        // 匿名名: .L.compound_<next_label>
        let anon_name = format!(".L.compound.{}", self.next_label);
        self.next_label += 1;
        // 分配 rodata 空间并对齐
        let section = if use_rodata {
            while !self.rodata.len().is_multiple_of(align) {
                self.rodata.push(0);
            }
            let base = self.rodata.len() as u64;
            // 递归生成初始化数据 (嵌套 AddrOf→CompoundLit 走 eval_const_expr)
            let init_data = self
                .gen_global_init_list(&ty, &inits, Some((base, None)), use_rodata)
                .unwrap_or_else(|| vec![0u8; sz]);
            self.rodata.extend_from_slice(&init_data);
            while !self.rodata.len().is_multiple_of(align) {
                self.rodata.push(0);
            }
            (base, 1u16)
        } else {
            while !self.tdata_section.len().is_multiple_of(align) {
                self.tdata_section.push(0);
            }
            let base = self.tdata_section.len() as u64;
            let init_data = self
                .gen_global_init_list(&ty, &inits, Some((base, None)), use_rodata)
                .unwrap_or_else(|| vec![0u8; sz]);
            self.tdata_section.extend_from_slice(&init_data);
            while !self.tdata_section.len().is_multiple_of(align) {
                self.tdata_section.push(0);
            }
            (base, 5u16)
        };
        let vaddr = self.entry + section.0;
        // 记录匿名对象 (嵌套 AddrOf→CompoundLit 时 eval_global_lvalue_offset 可查找)
        self.globals_raw.insert(
            anon_name.clone(),
            GlobalVar {
                offset: section.0,
                size: sz,
                ty,
                init: None,
                section: section.1,
                is_tls: false,
                in_data_section: false,
            },
        );
        Some((anon_name, vaddr))
    }
    pub(crate) fn materialize_compound_literal_rel(&mut self, e: &Expr) -> Option<(String, u64)> {
        let (ty, inits) = match e {
            Expr::CompoundLit { ty, inits, .. } => (ty.clone(), inits.clone()),
            Expr::InitList { inits, ty, .. } => (ty.clone(), inits.clone()),
            _ => return None,
        };
        let sz = ty.size().max(1);
        let align = ty.align().max(1);
        // 去重缓存: 同类型同内容只物化一次
        let cache_key = format!("{:?}", e);
        if let Some((cached_name, cached_off)) = self.compound_lit_cache.get(&cache_key) {
            return Some((cached_name.clone(), *cached_off));
        }
        let anon_name = format!(".L.compound.{}", self.next_label);
        self.next_label += 1;
        // 关键: 先递归物化所有子 AddrOf(CompoundLit), 让 rodata 正确增长,
        // 再取 base 保证父子对象的偏移不重叠.
        self.materialize_nested_compound_lits(&inits);
        // 对齐 rodata, 此时所有子对象已经在 rodata 里了
        while !self.rodata.len().is_multiple_of(align) {
            self.rodata.push(0);
        }
        let base = self.rodata.len() as u64;
        let rodata_marker: Option<usize> = Some(usize::MAX);
        let init_data = self
            .gen_global_init_list(&ty, &inits, Some((base, rodata_marker)), false)
            .unwrap_or_else(|| vec![0u8; sz]);
        self.rodata.extend_from_slice(&init_data);
        while !self.rodata.len().is_multiple_of(align) {
            self.rodata.push(0);
        }
        self.globals_raw.insert(
            anon_name.clone(),
            GlobalVar {
                offset: base,
                size: sz,
                ty,
                init: None,
                section: 2,
                is_tls: false,
                in_data_section: false,
            },
        );
        self.compound_lit_cache
            .insert(cache_key, (anon_name.clone(), base));
        Some((anon_name, base))
    }
    fn merge_classes(a: AbiClass, b: AbiClass) -> AbiClass {
        use AbiClass::*;
        if a == b {
            return a;
        }
        // NO_CLASS 为初始值, 合并时取另一方
        if a == NoClass {
            return b;
        }
        if b == NoClass {
            return a;
        }
        // 含 MEMORY 则结果为 MEMORY
        if a == Memory || b == Memory {
            return Memory;
        }
        match (a, b) {
            (Integer, Sse) | (Sse, Integer) => Memory,
            (Integer, X87) | (X87, Integer) => Memory,
            (Integer, X87Up) | (X87Up, Integer) => Memory,
            (Integer, ComplexX87) | (ComplexX87, Integer) => Memory,
            (Sse, X87) | (X87, Sse) => Memory,
            (Sse, X87Up) | (X87Up, Sse) => Memory,
            (Sse, ComplexX87) | (ComplexX87, Sse) => Memory,
            (X87, X87Up) | (X87Up, X87) => Memory,
            (X87, ComplexX87) | (ComplexX87, X87) => Memory,
            (X87Up, ComplexX87) | (ComplexX87, X87Up) => Memory,
            // SSEUP 仅与 SSE 合并产生 SSEUP (向量延续)
            (Sse, SseUp) | (SseUp, Sse) => SseUp,
            _ => Memory,
        }
    }
    pub(crate) fn modrm_reg_reg(reg: u8, rm: u8) -> u8 {
        0xC0 | ((reg & 7) << 3) | (rm & 7)
    }
    pub(crate) fn parse_explicit_reg_name(name: &str) -> Option<ExplicitRegInfo> {
        // 段寄存器
        match name {
            "gs" => {
                return Some(ExplicitRegInfo {
                    reg: 5,
                    size: 16,
                    att_name: "gs",
                    kind: ExplicitRegKind::Seg,
                })
            },
            "fs" => {
                return Some(ExplicitRegInfo {
                    reg: 4,
                    size: 16,
                    att_name: "fs",
                    kind: ExplicitRegKind::Seg,
                })
            },
            "es" => {
                return Some(ExplicitRegInfo {
                    reg: 0,
                    size: 16,
                    att_name: "es",
                    kind: ExplicitRegKind::Seg,
                })
            },
            "cs" => {
                return Some(ExplicitRegInfo {
                    reg: 1,
                    size: 16,
                    att_name: "cs",
                    kind: ExplicitRegKind::Seg,
                })
            },
            "ss" => {
                return Some(ExplicitRegInfo {
                    reg: 2,
                    size: 16,
                    att_name: "ss",
                    kind: ExplicitRegKind::Seg,
                })
            },
            "ds" => {
                return Some(ExplicitRegInfo {
                    reg: 3,
                    size: 16,
                    att_name: "ds",
                    kind: ExplicitRegKind::Seg,
                })
            },
            _ => {},
        }
        // GP 子寄存器
        let (idx, bits, sname) = Self::parse_reg_name_bits(name)?;
        Some(ExplicitRegInfo {
            reg: idx,
            size: bits,
            att_name: sname,
            kind: ExplicitRegKind::Gp,
        })
    }
    /// 显式寄存器引用的名字 → (号, 位宽, 规范名)。名字表只有一份
    /// （`abi/regnames.rs`，Phase 6.1）：这里以前是那张 68 条的本地拷贝，与
    /// `asm/expansion.rs` 里那一份逐字相同（本轮逐条对拍确认），两份各自演化就会
    /// 漂移。strict 口径不认 `%`、不认大写、不认 APX —— 扩大接受集会改变 asm 展开的
    /// 字节，所以这个分寸由表里的 `percent_ok`/`extended` 逐条描述而不是堆在调用点。
    pub(crate) fn parse_reg_name_bits(name: &str) -> Option<(u8, u8, &'static str)> {
        crate::codegen::x86_64::abi::regnames::lookup_reg_name(
            name,
            &crate::codegen::x86_64::abi::regnames::NAME_FORMS_STRICT,
        )
        .map(|e| (e.num, e.bits, e.name))
    }
    pub fn pos(&self) -> u64 {
        self.code.len() as u64
    }
    pub(crate) fn resolve_complete_struct_ty(&self, ty: Type) -> Type {
        match &ty {
            Type::Struct(name, fields, _, _) if fields.is_empty() => {
                if let Some(complete_ty) = self.complete_struct_types.get(name) {
                    return complete_ty.clone();
                }
            },
            Type::Union(name, fields, _, _) if fields.is_empty() => {
                if let Some(complete_ty) = self.complete_struct_types.get(name) {
                    return complete_ty.clone();
                }
            },
            _ => {},
        }
        ty
    }
    pub(crate) fn stmt_contains_label(stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Label { .. } => true,
            Stmt::Block { b, .. } => b.items.iter().any(|item| match item {
                BlockItem::Stmt(s) => Self::stmt_contains_label(s),
                _ => false,
            }),
            Stmt::If { then_, else_, .. } => {
                Self::stmt_contains_label(then_)
                    || else_.as_ref().is_some_and(|e| Self::stmt_contains_label(e))
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } => Self::stmt_contains_label(body),
            Stmt::For { body, .. } => Self::stmt_contains_label(body),
            // 标签是函数作用域的: goto 可以跳进 switch 的 case 体或 case 前的
            // prelude. 漏掉这两处会让死分支被整段跳过 ⇒ E1102 (标签未定义).
            Stmt::Switch { cases, prelude, .. } => {
                prelude.iter().any(|item| match item {
                    BlockItem::Stmt(s) => Self::stmt_contains_label(s),
                    BlockItem::Decl(_) => false,
                }) || cases
                    .iter()
                    .any(|c| c.body.items.iter().any(|item| match item {
                        BlockItem::Stmt(s) => Self::stmt_contains_label(s),
                        BlockItem::Decl(_) => false,
                    }))
            },
            Stmt::Invoke { call, .. } => Self::stmt_contains_label(call),
            _ => false,
        }
    }
    pub(crate) fn stmt_ends_with_return(stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Return { .. } => true,
            Stmt::Break { .. } => true,
            Stmt::Continue { .. } => true,
            Stmt::Block { b, .. } => Self::ends_with_return(&b.items),
            Stmt::If { then_, else_, .. } => {
                Self::stmt_ends_with_return(then_)
                    && else_
                        .as_ref()
                        .is_some_and(|e| Self::stmt_ends_with_return(e))
            },
            _ => false,
        }
    }

    /// 条件最外层是不是 `__builtin_expect(x, 0)`（Lua 的 `l_unlikely`、SQLite/内核的
    /// `unlikely`）——即「then 侧是冷侧」的唯一可靠信号。
    ///
    /// 只认**最外层**：嵌在 `&&`/`||` 里的提示描述的是那个子条件，拿它决定整条边的
    /// 冷热会把真正热的一侧也搬走。`!unlikely(x)` 的 then 侧反而是热路 ⇒ 否定次数为
    /// 奇数时不算冷。`__builtin_expect_with_probability` 的第二参是概率不是 0/1 标志，
    /// 本判据不认。
    pub(crate) fn cond_marked_cold(cond: &Expr) -> bool {
        let mut neg = false;
        let mut cur = cond;
        loop {
            match cur {
                Expr::Unary {
                    op: UnOp::Not, e, ..
                } => {
                    neg = !neg;
                    cur = e;
                },
                Expr::Call { callee, args, .. } => {
                    if !matches!(&**callee, Expr::Ident { name, .. } if name.name == "__builtin_expect")
                    {
                        return false;
                    }
                    return !neg && matches!(args.get(1), Some(Expr::Int { v, .. }) if *v == 0);
                },
                _ => return false,
            }
        }
    }

    /// 表达式里是否可能藏着内联 asm。默认「不认识就当有」：漏外置只是少省字节，
    /// 错外置会改变 asm 数字标签与 LSDA call-site 区间的结算位置 ⇒ 错值。
    fn expr_may_hold_asm(e: &Expr) -> bool {
        match e {
            Expr::AsmExpr { .. } => true,
            // 叶节点（不展开子表达式的那些）。
            Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::String { .. }
            | Expr::Char { .. }
            | Expr::Ident { .. }
            | Expr::ComplexLit { .. }
            | Expr::ComplexIntLit { .. }
            | Expr::SizeofTy { .. }
            | Expr::AlignofTy { .. } => false,
            // `&&label` 取的是标签地址：本后端的跳表常驻化与它同一条通道，不跟着搬。
            Expr::LabelAddr { .. } => true,
            Expr::Unary { e, .. }
            | Expr::Real { e, .. }
            | Expr::Imag { e, .. }
            | Expr::Cast { e, .. }
            | Expr::SizeofExpr { e, .. }
            | Expr::AlignofExpr { e, .. }
            | Expr::AddrOf { e, .. }
            | Expr::Deref { e, .. }
            | Expr::PostInc { e, .. }
            | Expr::PostDec { e, .. }
            | Expr::PreInc { e, .. }
            | Expr::PreDec { e, .. } => Self::expr_may_hold_asm(e),
            Expr::Binop { l, r, .. } | Expr::Assign { l, r, .. } => {
                Self::expr_may_hold_asm(l) || Self::expr_may_hold_asm(r)
            },
            Expr::Index { arr, idx, .. } => {
                Self::expr_may_hold_asm(arr) || Self::expr_may_hold_asm(idx)
            },
            Expr::Member { expr, .. } => Self::expr_may_hold_asm(expr),
            Expr::Cond {
                cond, then_, else_, ..
            } => {
                Self::expr_may_hold_asm(cond)
                    || Self::expr_may_hold_asm(then_)
                    || Self::expr_may_hold_asm(else_)
            },
            Expr::Call { callee, args, .. } => {
                Self::expr_may_hold_asm(callee) || args.iter().any(Self::expr_may_hold_asm)
            },
            // CompoundLit / InitList / StmtExpr / Comma / VaArg / VaArgPack / Offsetof 不展开：
            // 保守当「可能有」。
            _ => true,
        }
    }

    /// 这个语句体能不能被搬到函数尾（`true` = 不能）。三条硬约束都有实测根据：
    ///  * `cleanup_fn`：`end_cleanup_scope` 在**当前位置**发 `lea rdi/call`，清理栈是按发射
    ///    顺序的；把体挪到函数尾会让外层块的清理先发 ⇒ 析构顺序反。
    ///  * `vla_dim` / VLA 类型：`emit_vla_restore` 依赖「块结束时 rsp 已复原」这一发射顺序事实。
    ///  * 内联 asm 与受保护调用（`Invoke`）：数字标签按模板内行号解析，LSDA 以 PC 区间为键。
    /// 不认识的结构（`Switch`/`GotoIndirect`/`AsmGoto`）一律不动，新变体默认落进 `_ => true`。
    pub(crate) fn cold_outline_blocked(stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Expr { e, .. } => Self::expr_may_hold_asm(e),
            Stmt::Return { val, .. } => val.as_ref().is_some_and(Self::expr_may_hold_asm),
            Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Goto { .. } | Stmt::Fallthrough { .. } => {
                false
            },
            Stmt::Label { stmt, .. } => Self::cold_outline_blocked(stmt),
            Stmt::If { cond, then_, else_, .. } => {
                Self::expr_may_hold_asm(cond)
                    || Self::cold_outline_blocked(then_)
                    || else_
                        .as_ref()
                        .is_some_and(|e| Self::cold_outline_blocked(e))
            },
            Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
                Self::expr_may_hold_asm(cond) || Self::cold_outline_blocked(body)
            },
            Stmt::For {
                init, cond, step, body, ..
            } => {
                init.as_ref().is_some_and(|s| Self::cold_outline_blocked(s))
                    || cond.as_ref().is_some_and(Self::expr_may_hold_asm)
                    || step.as_ref().is_some_and(Self::expr_may_hold_asm)
                    || Self::cold_outline_blocked(body)
            },
            Stmt::Block { b, .. } => b.items.iter().any(|it| match it {
                BlockItem::Stmt(s) => Self::cold_outline_blocked(s),
                BlockItem::Decl(Decl::Var(v)) => {
                    // 本地 VLA 的判据与发射侧同一个入口（`gen_vla_alloc` 就挂在
                    // `type_has_vla(&v.ty)` 上），不留两份拷贝。
                    v.cleanup_fn.is_some()
                        || Self::type_has_vla(&v.ty)
                        || v.init.as_ref().is_some_and(Self::expr_may_hold_asm)
                },
                BlockItem::Decl(_) => false,
            }),
            _ => true,
        }
    }
    pub(crate) fn strip_asm_seg_prefix(s: &str) -> (&str, Option<u8>) {
        let s = s.trim_start();
        let (head, rest) = if let Some(after) = s.strip_prefix('%') {
            // 找第一个非字母字符作为 head/rs 分割
            let pos = after
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(after.len());
            (&after[..pos], &after[pos..])
        } else {
            let pos = s
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(s.len());
            (&s[..pos], &s[pos..])
        };
        if let Some((code, _pfx)) = Self::parse_asm_seg(head) {
            if let Some(after_colon) = rest.strip_prefix(':') {
                return (after_colon, Some(code));
            }
            // 单独段名: 整个 s 已经被消耗为段名
            if rest.is_empty() {
                return ("", Some(code));
            }
        }
        (s, None)
    }
    fn strip_casts(e: &Expr) -> &Expr {
        let mut cur = e;
        while let Expr::Cast { e: inner, .. } = cur {
            cur = inner;
        }
        cur
    }
    pub(crate) fn wrap_indirect(inner: AsmOperand) -> AsmOperand {
        // 间接 Reg → 保持 Reg, emit 阶段识别 (jmp *%rax = jmp %rax)
        // 间接 Mem → 保持 Mem
        // 间接 Sym → 标记为 SymDisp(sym, 0) 表示间接跳转
        match inner {
            AsmOperand::Sym(name) => AsmOperand::SymDisp(name, 0),
            other => other, // Reg/Mem 原样返回
        }
    }
}
