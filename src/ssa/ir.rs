// src/ssa/ir.rs - SSA IR 的数据形态与其属性判据
//
// 放什么: SsaVar / Operand / InstKind / Inst / Terminator / BasicBlock / Phi /
//   OrderedNameSet / Function (含各 impl), 以及 collect_uses_* / collect_var_uses_* /
//   kind_operands / kind_operand_ssa_vars / inst_reads_memory / inst_unremovable /
//   inst_clobbers_memory / inst_written_bases / rename_* (IR 级引用改写基础设施)。
// 不放什么: 任何 CFG 遍历、任何 AST 侧降级、任何重建 —— 那些在 cfg.rs /
//   structure.rs / rebuild.rs / expr_rebuild.rs。

use std::collections::{HashMap, HashSet};

use crate::ast::{BinOp, Expr, Type, UnOp};
use crate::diagnostics::Span;

use crate::ssa::rebuild::inst_has_side_effect;

/// SSA 变量名: `"x.3"` 表示 `x` 的第 4 个版本 (下标从 0 起)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsaVar {
    pub base: String,
    pub version: u32,
    pub ty: Type,
    pub span: Span,
}

impl SsaVar {
    /// SSA 名的稳定形式: `"x.3"` 表示 `x` 的第 4 个版本。
    ///
    /// 分隔符不能用 `_`: 那会让「用户变量的版本名」与「编译器合成临时值的 base」
    /// 撞同一个字符串 —— `int _tbin;` 的第二个版本 `_tbin_2` 与 `fresh("bin")`
    /// 产生的临时值 base `_tbin_2` 完全同名字, 两个不相关的 SSA 名互相覆盖,
    /// 实测 `int _tbin=3; for(i<4){s+=_tbin*i; _tbin=_tbin+1;}` 在 -O2 得 18 (应 39)。
    /// C/C++ 标识符不可能含 `.`, 所以用它作分隔符后这类碰撞在结构上消失。
    /// (重建时回到 AST 的一律是 `base`, 所以该名字只在 IR 内部存在; 仅两处按前缀
    /// 判断的 `__dcc_asm_` / `__dcc_` 仍以 `base` 开头, 不受后缀影响。)
    pub fn name(&self) -> String {
        if self.version == 0 {
            self.base.clone()
        } else {
            format!("{}.{}", self.base, self.version)
        }
    }
}

/// SSA 指令的右侧操作数: SSA 变量 / 常量 / 表达式结果
#[derive(Debug, Clone)]
pub enum Operand {
    Var(SsaVar),
    Int(i128),
    Float(crate::ast::FloatConst, Type),
    /// 字符串字面量 (在 rodata 中以 FNV-1a label 表示)
    Str(Vec<u8>),
    /// 全局符号地址 (用符号名占位)
    SymAddr(String, Type),
    /// 函数地址
    FnAddr(String, Type),
    /// 无值 (void)
    Void,
    /// 调用返回
    Call(String),
    /// 复合运算 (Phi 之类)
    Phi(Vec<(SsaVar, String)>),
}

#[derive(Debug, Clone)]
// 指令按值存放在 IR 节点里, 装箱会让每条热路径的模式匹配多一次解引用。
#[allow(clippy::large_enum_variant)]
pub enum InstKind {
    /// `dest = op(l, r)` 二元
    Bin { op: BinOp, l: Operand, r: Operand },
    /// `dest = op(e)` 一元
    Un { op: UnOp, e: Operand },
    /// `dest = (ty)e` 类型转换
    Cast { ty: Type, e: Operand },
    /// `dest = &base` 取址
    AddrOf { base: String },
    /// `dest = *e` 解引用
    Load {
        e: Operand,
        ty: Type,
        /// volatile 访问: 不可被 DCE/DSE 消除或重排序
        is_volatile: bool,
    },
    /// `*e = src` 存储
    Store {
        e: Operand,
        src: Operand,
        /// volatile 访问: 不可被 DCE/DSE 消除或重排序
        is_volatile: bool,
    },
    /// `dest = arr[idx]` —— 读取元素。
    ///
    /// `ty` 是**元素类型**（不是 `arr` 的类型）：下标的字节步长与载入宽度都由
    /// 它决定。 SSA→AST 重建时必须把它原样带回 `Expr::Index{ty}`，否则 C 后端
    /// 会按默认的 `Type::Int`（步长 4 / 4 字节载入）发射，`char buf[4]` 的
    /// `buf[2]` 就会读越界到相邻栈槽。与 `InstKind::Load{ty}` 同一约定。
    IndexLoad {
        arr: Operand,
        idx: Operand,
        ty: Type,
        /// volatile 访问: 不可被 DCE/DSE 消除或重排序
        is_volatile: bool,
    },
    /// `arr[idx] = src` —— 写入元素，`ty` 同样是元素类型（决定步长与存宽）。
    IndexStore {
        arr: Operand,
        idx: Operand,
        src: Operand,
        ty: Type,
        /// volatile 访问: 不可被 DCE/DSE 消除或重排序
        is_volatile: bool,
    },
    /// `dest = call(callee, args)`
    Call {
        callee: Operand,
        args: Vec<Operand>,
        ret_ty: Type,
    },
    /// `dest = &obj.field` —— 子对象**地址**。
    ///
    /// 为什么需要单独一条：`InstKind::AddrOf { base }` 只能取**具名对象**的基址，
    /// 而 `&p->fld` / `&(q.fld)` 要的是「对象地址 + 字段偏移」。缺这条指令时 IR 只能
    /// 把子对象当**值**读（`MemberLoad`），于是数组类型的字段（`struct { int data[]; }`
    /// 的柔性数组、`int data[0]` 的零长数组、VLA 成员）拿不到基址而是拿到字段的
    /// **内容** —— 实测 -O2 读出栈垃圾而 -O0 对（静默误编译）。按本项目既有政策，
    /// 表达不了的形状会让整函数退 SSA（§7.29）；这条指令就是那个真修。
    ///
    /// 字段偏移**不在 SSA 层算**：与 `MemberLoad` 同一约定，只记 `obj`/`obj_ty`/`field`/
    /// `is_arrow`，重建时原样装回 `Expr::AddrOf { e: Expr::Member { .. } }`，由 codegen
    /// 现有的 `&var / &struct.field / &ptr->field` 地址形态发成 `lea`（单一真源，不在
    /// 这里复制一份布局计算）。
    ///
    /// `ty` 是**字段类型**（不是地址类型）；`dest` 的类型由调用处取 `Ptr(ty)`。
    MemberAddr {
        obj: Operand,
        /// 基址表达式的类型（重建时用它恢复结构体类型，与 `MemberLoad::obj_ty` 同义）
        obj_ty: Type,
        field: String,
        ty: Type,
        /// `obj` 已是指向结构体的指针：重建为 `&obj->field`，不再额外解引用。
        is_arrow: bool,
    },
    /// `dest = &arr[idx]` —— 下标表达式的**左值地址**（基址 + 下标*步长，不载入）。
    ///
    /// 与 `MemberAddr` 同一条缺口、同一个解法：`InstKind::AddrOf { base }` 只能取具名
    /// 对象的基址，而 `&a[i]` 要「基址 + 下标*步长」。缺它时降级只能 `unsupported`，
    /// 后果不是错值而是**整个函数不进 SSA**（产物正确但丢了全部优化）—— 实测
    /// `int *p = &x[2];` 就在 `build_from_func` 阶段被拒。
    ///
    /// 偏移与步长都不在 SSA 层算：重建时装回 `Expr::AddrOf{Expr::Index{..}}`，交给
    /// codegen 的 `AddrOf` 分支（它本来就有 `Expr::Index` 与 `Expr::Deref` 两个子形态，
    /// 所以下标取址在 AST 路径上一直是对的）。单一真源，不在这复制一份布局算术。
    ///
    /// 一般的「左值地址」形态：`&m[i][j]`（乃至任意维 `&m[i][j][k]…`）逐层发
    /// `IndexAddr`，每一层的 `arr` 是内一层的地址。内层基址本身是「计算出来的数组行」
    /// （内层下标的结果类型仍是数组）时，降级用 `lower_lvalue_base_addr` 递归再发一条
    /// `IndexAddr`：那一层的 `ty` 取内层下标的**结果类型**（即行数组类型），行步长由
    /// codegen 用 `sizeof(row)` 算。地址类临时值不得被内联进取址操作数（见
    /// `plan_inlined_temps` 的 addr-slot 守卫），重建 AST 始终是 `&<rowvar>[j]` 而非嵌套
    /// 取址。
    ///
    /// `ty` 是**元素类型**（决定步长，与 `IndexLoad`/`IndexStore` 同一约定；中间层可以
    /// 是数组类型 = 行）；`arr` 是基址操作数（数组对象的地址、指针值，或内层 `IndexAddr`）。
    IndexAddr {
        arr: Operand,
        idx: Operand,
        ty: Type,
    },
    /// `dest = obj.field`
    MemberLoad {
        obj: Operand,
        /// 基址表达式的类型 (用于 SSA→AST 重建时保留正确的结构体类型)
        obj_ty: Type,
        field: String,
        ty: Type,
        /// volatile 访问: 不可被 DCE/DSE 消除或重排序
        is_volatile: bool,
        /// obj 已经是指向结构体的指针 (如 C++ 的 this): 重建为
        /// `obj->field`, 保留 obj 的指针类型, 不再额外解引用。
        is_arrow: bool,
    },
    /// `obj.field = src`
    MemberStore {
        obj: Operand,
        /// 基址表达式的类型
        obj_ty: Type,
        field: String,
        src: Operand,
        /// 被写字段的类型: SSA→AST 重建 `Expr::Member{ty}` 的**唯一真源**。
        /// 缺失时重建只能给 `Void`, 后端便认不出「聚合字段按值整体拷贝」
        /// 形态, 退回恒 8/16 字节的标量物化 —— 多 quad 聚合值漏第二字
        /// (协程 awaiter 帧槽 >8B 失效) 即此根因。
        ty: Type,
        /// volatile 访问: 不可被 DCE/DSE 消除或重排序
        is_volatile: bool,
        /// obj 已经是指向结构体的指针 (如 C++ 的 this): 重建为
        /// `obj->field`, 保留 obj 的指针类型, 不再额外解引用。
        is_arrow: bool,
    },
    /// `dest = cond ? t : f`
    Select {
        cond: Operand,
        then_: Operand,
        else_: Operand,
    },
    /// GNU inline asm。输入使用 SSA operand，便于调度器保留数据依赖；
    /// 输出保留为 AST lvalue，SSA→AST 重建后交给后端处理。
    InlineAsm {
        asm_str: String,
        outputs: Vec<Expr>,
        output_names: Vec<Option<String>>,
        output_constraints: Vec<String>,
        output_vars: Vec<SsaVar>,
        inputs: Vec<Operand>,
        input_names: Vec<Option<String>>,
        input_constraints: Vec<String>,
        clobbers: Vec<String>,
        /// asm volatile: 阻止优化器删除/移动
        volatile: bool,
        ty: Type,
    },
    /// 保留原始变量声明 (含初始化式)。用于 SSA 无法表示的初始化式
    /// (如复合字面量 `{1, 2, 3, 4}`)。重建时直接输出原始 Decl::Var。
    PreservedDecl { var: crate::ast::VarDecl },
    /// C++ 零成本异常: **受保护调用** (与普通 `Call` 同样的调用序列, 额外带上
    /// 「本调用指令的地址区间由 landing pad 接管」的异常边)。
    ///
    /// 异常边故意**不进 CFG** (`succs` 只含正常后继): unwinder 是按 PC 落在哪个
    /// 区间里直接跳到落地垫地址, 正常执行流永远不会从这次调用跳到 `unwind`。
    /// 把它当成普通控制流边会让 SSA→AST 的结构化重建在正常路径上多生成一个
    /// 跳转 (错值) 或把落地垫内联进 then 分支 (丢代码)。落地垫块因此由
    /// `Function::eh_unwind_blocks` 单独登记, 在主体重建之后平铺发射。
    Invoke {
        callee: Operand,
        args: Vec<Operand>,
        ret_ty: Type,
        /// 落地垫块名 (只由 unwinder 进入)。
        ///
        /// `None` = 「受保护但本帧不接管」(LSDA 里 `landing_pad = 0`, `action = 0`)。
        /// 这不是可有可无的优化: **只要函数有 LSDA, 展开途中落在本函数内却没有
        /// call-site 区间覆盖的 PC 就会被 libstdc++ 当成「清理代码里二次抛出」而
        /// `std::terminate`**。`_Unwind_Resume` / `__cxa_rethrow` 正是靠这个区间把
        /// 「异常继续向外传播」显式声明出来 (与 g++ 产物里 `.LEHBn/.LEHEn` 包住
        /// `call _Unwind_Resume` 的写法逐字节同构)。
        unwind: Option<String>,
        /// 本调用点的 LSDA action 链
        actions: Vec<crate::ast::EhAction>,
    },
    /// C++ 零成本异常: 落地垫序言。必须是其所在块的**第一条**指令。
    /// `dest` 是接收异常对象指针 (`%rax`) 的变量, `selector` 接收 type
    /// selector (`%rdx`, 0 = 纯 cleanup)。两个值只在交接后的头几条指令内有效。
    EhLandingPad { selector: SsaVar },
}

#[derive(Debug, Clone)]
pub struct Inst {
    pub dest: Option<SsaVar>, // None 表示 store / 无值
    pub kind: InstKind,
    pub span: Span,
}

/// 终结符: BB 末尾的跳转/分支/返回
#[derive(Debug, Clone)]
pub enum Terminator {
    /// `return val`
    Ret { val: Option<Operand> },
    /// 无条件跳到 BB
    Jump(String),
    /// `if cond goto t else f`
    Branch { cond: Operand, t: String, f: String },
    /// 跳转表: switch(idx) { case values[0] -> targets[0], ..., default -> default }
    /// 用于密集 case 值的 switch 语句优化
    SwitchTable {
        idx: Operand,
        values: Vec<i128>,
        targets: Vec<String>,
        default: String,
    },
    /// 不可达
    Unreachable,
}

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub name: String,
    pub phis: Vec<Phi>,
    pub insts: Vec<Inst>,
    pub term: Terminator,
    /// 立即前驱 (CFG 反向边, 在 SSA 构造时填充)
    pub preds: Vec<String>,
    /// 立即后继 (CFG 正向边, 由 terminator 决定)
    pub succs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Phi {
    pub dest: SsaVar,
    pub incoming: Vec<(SsaVar, String)>, // (src var, predecessor BB name)
}

/// 保序去重的名字集合。
///
/// 为什么不用 `HashSet<String>`: `rebuild_block` 会按本集合的**迭代序**为局部
/// 变量生成 AST 声明, 而后端 (`pre_register_vars`) 又按 AST 声明序分配栈槽。
/// `std::collections::HashSet` 的迭代序由进程随机哈希种子决定, 于是同一份源码
/// 每次编译都会得到不同的栈槽偏移 (`lea -72(%r13)` ↔ `lea -80(%r13)`), 这是
/// `-O2` 产物字节不可复现的根因。本类型保持**首次插入序** (= 源码声明序),
/// 同时提供 O(1) 的 `contains`。
#[derive(Default)]
pub struct OrderedNameSet {
    names: Vec<String>,
    seen: HashSet<String>,
}

impl std::fmt::Debug for OrderedNameSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.names.iter()).finish()
    }
}

impl Clone for OrderedNameSet {
    fn clone(&self) -> Self {
        Self {
            names: self.names.clone(),
            seen: self.seen.clone(),
        }
    }
}

impl OrderedNameSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// 首次插入返回 true 并保持插入序; 重复插入只返回 false, 不改变顺序。
    pub fn insert(&mut self, name: String) -> bool {
        if self.seen.insert(name.clone()) {
            self.names.push(name);
            true
        } else {
            false
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.seen.contains(name)
    }

    pub fn iter(&self) -> std::slice::Iter<'_, String> {
        self.names.iter()
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

#[derive(Debug)]
pub struct Function {
    pub name: String,
    pub params: Vec<SsaVar>,
    pub ret_ty: Type,
    pub entry: String,
    pub blocks: HashMap<String, BasicBlock>,
    /// 顺序遍历的 BB 列表
    pub block_order: Vec<String>,
    /// 全局符号表: 全局变量名 -> SSA 变量 (version=0)
    pub globals: HashMap<String, SsaVar>,
    /// 函数是否被标注为 cold
    pub is_cold: bool,
    /// 函数是否被标注为 hot
    pub is_hot: bool,
    /// 函数是否不返回
    pub is_noreturn: bool,
    /// TU 级 `#pragma STDC FENV_ACCESS ON`: 浮点运算的结果依赖运行时舍入模式与
    /// 异常标志, 值编号类 pass (GVN) 因此**完全不得**对浮点运算做复用/消除。
    ///
    /// 为什么必须带在 `Function` 上而不是 pipeline 参数里: 它是 SSA 层多个 pass 都要
    /// 读的**事实**而非开关; 事实随 IR 走才能避免「调用方忘了传」这一类错配 ——
    /// 传错的后果是静默误编译, 不是编译失败。
    pub fenv_access: bool,
    pub span: Span,
    /// 原始 AST 中声明的局部变量名 (用于 rebuild_block 避免重复声明)
    /// 必须是保序集合: 迭代序直接决定重建 AST 的声明序, 进而决定栈槽布局。
    pub original_locals: OrderedNameSet,
    /// 原始局部变量的类型 (用于 rebuild_block 保留正确的类型信息)
    pub local_types: HashMap<String, Type>,
    /// 原始局部变量的类型限定符 (volatile/const/restrict),
    /// 用于 SSA→AST 重建时保留 volatile 语义,防止优化器错误移除读写
    pub local_quals: HashMap<String, crate::ast::QualifierFlags>,
    /// 函数参数的类型限定符 (volatile/const/restrict),
    /// 用于别名分析利用 restrict 不别名保证
    pub param_quals: HashMap<String, crate::ast::QualifierFlags>,
    /// C++ 零成本异常: 只由 unwinder 进入的落地垫块 (按 lowering 顺序)。
    ///
    /// 它们不在从 entry 可达的正常 CFG 里, `rebuild_block` 的结构化重建走不到;
    /// 因此主体重建完成后由 `rebuild_eh_blocks` 以
    /// `Label + 平铺语句 + Goto` 的形式补发射。必须是**保序 Vec** 而不是集合:
    /// 发射序直接决定落地垫在 .text 中的位置, 进而决定产物字节可复现性。
    pub eh_unwind_blocks: Vec<String>,
    /// C++ 零成本异常: 被落地垫区 `goto` 回的正常规块名。
    /// 这些块在结构体重建时必须额外包一层 `Stmt::Label`, 否则异常路径
    /// 「继续执行 try 语句之后的代码」无法表达。
    pub eh_reentry_labels: Vec<String>,
    /// P12 字面形态 (Phase 3): SSA 值的**家**。数据形态与改写入口在 `ssa/location.rs`。
    ///
    /// 为什么放在 `Function` 而不是 `ast::Ident`：编号是后端事实、`Ident` 只能表达
    /// 「叫什么」表达不了「在哪住」，而挂在 IR 上才能谈活性/干涉，并让「复制函数的 pass」
    /// 被编译器点名（`LocMap` 故意不实现 `Clone`/`Default`）。
    ///
    /// 本字段由 `regalloc` 填写、重建时翻译成后端的绑定表；Phase 3.0 阶段它只存
    /// 与不读（产物逐字节不变就是这一步的判据）。
    pub value_loc: crate::ssa::location::LocMap,
    /// D3 / Phase 3.4：版本分裂表（同基名的版本拿到不同家时，它们必须成为
    /// 各自的 AST 局部）。由 `regalloc::assign` 算一次；重建与交付都读它，
    /// 所以「哪个版本叫什么」只有一份答案。今天总是空（⇒ 产物不变）。
    pub local_splits: crate::ssa::location::Splits,
}

impl Function {
    pub fn new(name: String, ret_ty: Type, span: Span) -> Self {
        let mut s = Self {
            name,
            params: Vec::new(),
            ret_ty,
            entry: "entry".to_string(),
            blocks: HashMap::new(),
            block_order: vec!["entry".to_string()],
            globals: HashMap::new(),
            is_cold: false,
            is_hot: false,
            is_noreturn: false,
            fenv_access: false,
            span,
            original_locals: OrderedNameSet::new(),
            local_types: HashMap::new(),
            local_quals: HashMap::new(),
            param_quals: HashMap::new(),
            eh_unwind_blocks: Vec::new(),
            eh_reentry_labels: Vec::new(),
            value_loc: crate::ssa::location::LocMap::empty(),
            local_splits: crate::ssa::location::Splits::default(),
        };
        s.add_block("entry".to_string());
        s
    }

    /// 显式整函数复制 —— Phase 3.3「让编译器点名」的落点（设计 §7 Phase 3.3）。
    ///
    /// `Function` 不再 `derive(Clone)`（因 `value_loc: LocMap` 故意不可复制），于是
    /// **新增一个字段会同时打破本函数与每一个整函数克隆点**，逼写代码的人回答
    /// 「这个字段在克隆→验证→提交的模式里该怎么走」。若靠 derive，新字段会被静默
    /// 带走 —— 而位置表恰恰不能静默：SSA 改名/删块/重编号之后，挂在死名字上的家
    /// 会让新名字默默落到默认值上（实测风险见设计 §11.5 第 8 条）。
    ///
    /// 字段逐条深拷贝，副本与原件完全独立：`pipelining::apply_plan` 靠的正是
    /// 「在副本上落地、验证失败则原件一字不动」这一语义。
    pub fn fork(&self) -> Self {
        Self {
            name: self.name.clone(),
            params: self.params.clone(),
            ret_ty: self.ret_ty.clone(),
            entry: self.entry.clone(),
            blocks: self.blocks.clone(),
            block_order: self.block_order.clone(),
            globals: self.globals.clone(),
            is_cold: self.is_cold,
            is_hot: self.is_hot,
            is_noreturn: self.is_noreturn,
            fenv_access: self.fenv_access,
            span: self.span.clone(),
            original_locals: self.original_locals.clone(),
            local_types: self.local_types.clone(),
            local_quals: self.local_quals.clone(),
            param_quals: self.param_quals.clone(),
            eh_unwind_blocks: self.eh_unwind_blocks.clone(),
            eh_reentry_labels: self.eh_reentry_labels.clone(),
            // 位置表也必须跟着带走，但只能走显式 `fork()`：名字就是「我知道我在
            // 把位置表一起带走」的声明，而不是 derive 的默认行为。
            value_loc: self.value_loc.fork(),
            local_splits: self.local_splits.clone(),
        }
    }

    pub fn add_block(&mut self, name: String) -> String {
        if !self.blocks.contains_key(&name) {
            self.blocks.insert(
                name.clone(),
                BasicBlock {
                    name: name.clone(),
                    phis: Vec::new(),
                    insts: Vec::new(),
                    term: Terminator::Unreachable,
                    preds: Vec::new(),
                    succs: Vec::new(),
                },
            );
            if !self.block_order.contains(&name) {
                self.block_order.push(name.clone());
            }
        }
        name
    }

    pub fn unique_bb(&mut self, prefix: &str) -> String {
        if !self.blocks.contains_key(prefix) {
            return self.add_block(prefix.to_string());
        }
        for i in 0..10000 {
            let n = format!("{}.{}", prefix, i);
            if !self.blocks.contains_key(&n) {
                return self.add_block(n);
            }
        }
        self.add_block(format!("{}.x", prefix))
    }
}

/// 把一批 SSA 引用改写到另一个 SSA 变量 (IR 级基础设施, phi 插入与优化 pass 共用)。
///
/// 为什么住在 `ssa::ir` 而不是各 pass 里各写一份: 这份枚举必须对 `InstKind` **穷举**,
/// 漏掉一个操作数位置就等于留下一个指向已被删除定义的悬空引用 —— 两处各写一份必然漂移。
/// `map` 以「旧 SSA 名 → 新 SSA 变量」为键, 只对完全相同的版本生效。
pub(crate) fn rename_var_ssa(v: &mut SsaVar, map: &HashMap<String, SsaVar>) {
    if let Some(n) = map.get(&v.name()) {
        *v = n.clone();
    }
}

pub(crate) fn rename_operand(o: &mut Operand, map: &HashMap<String, SsaVar>) {
    if let Operand::Var(v) = o {
        rename_var_ssa(v, map);
    }
}

/// 穷举: 新增 `InstKind` 形态时本函数编译失败, 强制同步。
pub(crate) fn rename_kind_uses(k: &mut InstKind, map: &HashMap<String, SsaVar>) {
    match k {
        InstKind::Bin { l, r, .. } => {
            rename_operand(l, map);
            rename_operand(r, map);
        },
        InstKind::Un { e, .. } | InstKind::Cast { e, .. } => rename_operand(e, map),
        InstKind::AddrOf { .. } => {},
        InstKind::Load { e, .. } => rename_operand(e, map),
        InstKind::Store { e, src, .. } => {
            rename_operand(e, map);
            rename_operand(src, map);
        },
        InstKind::IndexLoad { arr, idx, .. } => {
            rename_operand(arr, map);
            rename_operand(idx, map);
        },
        InstKind::IndexAddr { arr, idx, .. } => {
            rename_operand(arr, map);
            rename_operand(idx, map);
        },
        InstKind::IndexStore { arr, idx, src, .. } => {
            rename_operand(arr, map);
            rename_operand(idx, map);
            rename_operand(src, map);
        },
        InstKind::Call { callee, args, .. } => {
            rename_operand(callee, map);
            for a in args {
                rename_operand(a, map);
            }
        },
        InstKind::Invoke { callee, args, .. } => {
            rename_operand(callee, map);
            for a in args {
                rename_operand(a, map);
            }
        },
        InstKind::MemberLoad { obj, .. } | InstKind::MemberAddr { obj, .. } => {
            rename_operand(obj, map)
        },
        InstKind::MemberStore { obj, src, .. } => {
            rename_operand(obj, map);
            rename_operand(src, map);
        },
        InstKind::Select {
            cond, then_, else_, ..
        } => {
            rename_operand(cond, map);
            rename_operand(then_, map);
            rename_operand(else_, map);
        },
        InstKind::InlineAsm { inputs, .. } => {
            for i in inputs {
                rename_operand(i, map);
            }
        },
        InstKind::PreservedDecl { .. } => {},
        InstKind::EhLandingPad { selector } => rename_var_ssa(selector, map),
    }
}

pub(crate) fn rename_term_uses(t: &mut Terminator, map: &HashMap<String, SsaVar>) {
    match t {
        Terminator::Ret { val: Some(v) } => rename_operand(v, map),
        Terminator::Ret { val: None } => {},
        Terminator::Branch { cond, .. } => rename_operand(cond, map),
        Terminator::SwitchTable { idx, .. } => rename_operand(idx, map),
        Terminator::Jump(..) | Terminator::Unreachable => {},
    }
}

/// 在指定块集合内应用一批 SSA 引用改写 (phi 插入后的重命名专用)。
///
/// 两条刻意的限制:
///   * 不改 `inst.dest` / `phi.dest` —— 映射的键本身就是一个定义的名字 (循环携带变量
///     进循环前的 `s_1`), 改定义会造出自引用名字。
///   * phi 入边只改「来自循环体内前驱」的那一行。来自 preheader 的那一行代的是
///     「进入循环时的值」, 它本身就是被本映射改写的对象 —— 跟着改就得到
///     `phi s_2 <- [(s_2, preheader), (s_1, latch)]` 这种自己依赖自己的垃圾 phi。
///     (标准 SSA 重命名是按前驱边做的, 这里用「入边前驱是否在体内」等价地做。)
pub(crate) fn rename_uses_in(
    func: &mut Function,
    blocks: &HashSet<String>,
    map: &HashMap<String, SsaVar>,
) {
    if map.is_empty() {
        return;
    }
    for bb in func.blocks.values_mut() {
        if !blocks.contains(&bb.name) {
            continue;
        }
        for phi in &mut bb.phis {
            for (v, pred) in &mut phi.incoming {
                if blocks.contains(pred) {
                    rename_var_ssa(v, map);
                }
            }
        }
        for inst in &mut bb.insts {
            rename_kind_uses(&mut inst.kind, map);
        }
        rename_term_uses(&mut bb.term, map);
    }
}

/// 一条指令读到的全部 SSA 名 (含 asm 输入与成员/下标基址)。
pub(super) fn collect_uses_kind(k: &InstKind) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    let push = |o: &Operand, v: &mut Vec<String>| {
        v.extend(collect_uses_operand(o));
    };
    match k {
        InstKind::Bin { l, r, .. } => {
            push(l, &mut v);
            push(r, &mut v);
        },
        InstKind::Un { e, .. } | InstKind::Cast { e, .. } | InstKind::Load { e, .. } => {
            push(e, &mut v)
        },
        InstKind::Store { e, src, .. } => {
            push(e, &mut v);
            push(src, &mut v);
        },
        InstKind::IndexLoad { arr, idx, .. } => {
            push(arr, &mut v);
            push(idx, &mut v);
        },
        InstKind::IndexAddr { arr, idx, .. } => {
            push(arr, &mut v);
            push(idx, &mut v);
        },
        InstKind::IndexStore { arr, idx, src, .. } => {
            push(arr, &mut v);
            push(idx, &mut v);
            push(src, &mut v);
        },
        InstKind::Call { callee, args, .. } | InstKind::Invoke { callee, args, .. } => {
            push(callee, &mut v);
            for a in args {
                push(a, &mut v);
            }
        },
        InstKind::MemberLoad { obj, .. } | InstKind::MemberAddr { obj, .. } => push(obj, &mut v),
        InstKind::MemberStore { obj, src, .. } => {
            push(obj, &mut v);
            push(src, &mut v);
        },
        InstKind::Select {
            cond, then_, else_, ..
        } => {
            push(cond, &mut v);
            push(then_, &mut v);
            push(else_, &mut v);
        },
        InstKind::InlineAsm { inputs, .. } => {
            for i in inputs {
                push(i, &mut v);
            }
        },
        InstKind::AddrOf { .. } | InstKind::PreservedDecl { .. } => {},
        InstKind::EhLandingPad { .. } => {},
    }
    v
}

pub(super) fn collect_uses_operand(o: &Operand) -> Vec<String> {
    match o {
        Operand::Var(v) => vec![v.name()],
        Operand::Phi(pairs) => pairs.iter().map(|(v, _)| v.name()).collect(),
        _ => Vec::new(),
    }
}

/// Collect SSA variables used by an instruction, keyed by base name.
fn collect_var_uses_inst(kind: &InstKind, out: &mut HashMap<String, SsaVar>) {
    match kind {
        InstKind::Bin { l, r, .. } => {
            collect_var_uses_op(l, out);
            collect_var_uses_op(r, out);
        },
        InstKind::Un { e, .. } | InstKind::Cast { e, .. } | InstKind::Load { e, .. } => {
            collect_var_uses_op(e, out);
        },
        InstKind::Store { e, src, .. } => {
            collect_var_uses_op(e, out);
            collect_var_uses_op(src, out);
        },
        InstKind::IndexLoad { arr, idx, .. } | InstKind::IndexStore { arr, idx, .. } => {
            collect_var_uses_op(arr, out);
            collect_var_uses_op(idx, out);
        },
        InstKind::Call { callee, args, .. } => {
            collect_var_uses_op(callee, out);
            for a in args {
                collect_var_uses_op(a, out);
            }
        },
        // C++ 零成本 EH: 受保护调用的操作数与普通调用同构处理 (落地垫不引用
        // 任何正常路径的 SSA 值, 它自己就是一个新块的开始)。
        InstKind::Invoke { callee, args, .. } => {
            collect_var_uses_op(callee, out);
            for a in args {
                collect_var_uses_op(a, out);
            }
        },
        InstKind::MemberLoad { obj, .. }
        | InstKind::MemberAddr { obj, .. }
        | InstKind::MemberStore { obj, .. } => {
            collect_var_uses_op(obj, out);
        },
        InstKind::Select { cond, then_, else_ } => {
            collect_var_uses_op(cond, out);
            collect_var_uses_op(then_, out);
            collect_var_uses_op(else_, out);
        },
        InstKind::InlineAsm { inputs, .. } => {
            for input in inputs {
                collect_var_uses_op(input, out);
            }
        },
        _ => {},
    }
}

fn collect_var_uses_op(o: &Operand, out: &mut HashMap<String, SsaVar>) {
    if let Operand::Var(v) = o {
        out.entry(v.base.clone())
            .and_modify(|existing| {
                if v.version > existing.version {
                    *existing = v.clone();
                }
            })
            .or_insert_with(|| v.clone());
    }
}

/// 一条指令的**全部操作数** (含 Store 的 src、Call 的参数), 按 IR 结构穷举。
fn kind_operands(kind: &InstKind) -> Vec<&Operand> {
    match kind {
        InstKind::Bin { l, r, .. } => vec![l, r],
        InstKind::Un { e, .. } | InstKind::Cast { e, .. } | InstKind::Load { e, .. } => vec![e],
        InstKind::Store { e, src, .. } | InstKind::MemberStore { obj: e, src, .. } => vec![e, src],
        InstKind::IndexLoad { arr, idx, .. } => vec![arr, idx],
        InstKind::IndexAddr { arr, idx, .. } => vec![arr, idx],
        InstKind::IndexStore { arr, idx, src, .. } => vec![arr, idx, src],
        InstKind::Call { callee, args, .. } | InstKind::Invoke { callee, args, .. } => {
            let mut v = vec![callee];
            v.extend(args.iter());
            v
        },
        InstKind::MemberLoad { obj, .. } | InstKind::MemberAddr { obj, .. } => vec![obj],
        InstKind::Select { cond, then_, else_ } => vec![cond, then_, else_],
        InstKind::InlineAsm { inputs, .. } => inputs.iter().collect(),
        InstKind::AddrOf { .. }
        | InstKind::PreservedDecl { .. }
        | InstKind::EhLandingPad { .. } => Vec::new(),
    }
}

pub(crate) fn kind_operand_ssa_vars(kind: &InstKind) -> Vec<SsaVar> {
    kind_operands(kind)
        .into_iter()
        .filter_map(|o| match o {
            Operand::Var(v) => Some(v.clone()),
            _ => None,
        })
        .collect()
}

/// 终结符里被**读**的 SSA 变量（与 `kind_operand_ssa_vars` 同一事实的终结符侧）。
///
/// 为什么提到这里而不是留在消费点：终结符的操作数形状本文件已经知道一遍
/// （`rename_term_uses`），而活性/位置表又各自需要「哪些名字在终结符里出现」。
/// 多一份知道 = 多一处会漏变体的地方（`Ret{val}`/`Branch{cond}`/`SwitchTable{idx}`
/// 漏一个就是静默丢活，而不是编不过）。
pub(crate) fn term_operand_ssa_vars(t: &Terminator) -> Vec<SsaVar> {
    let mut out = Vec::new();
    let mut take = |o: &Operand| match o {
        Operand::Var(v) => out.push(v.clone()),
        Operand::Phi(pairs) => {
            for (v, _) in pairs {
                out.push(v.clone());
            }
        },
        _ => {},
    };
    match t {
        Terminator::Ret { val: Some(o) } => take(o),
        Terminator::Ret { val: None } | Terminator::Jump(_) | Terminator::Unreachable => {},
        Terminator::Branch { cond, .. } => take(cond),
        Terminator::SwitchTable { idx, .. } => take(idx),
    }
    out
}

/// 该指令是否**读内存** (解引用读 / 读全局对象内容)。内联它等于把这次读**推迟**
/// 到使用点重放, 因此使用点之前若有任何写内存/调用, 语义就会漂移。
pub(super) fn inst_reads_memory(kind: &InstKind) -> bool {
    match kind {
        InstKind::Load { .. } | InstKind::IndexLoad { .. } | InstKind::MemberLoad { .. } => true,
        _ => kind_operands(kind)
            .iter()
            .any(|o| matches!(o, Operand::SymAddr(..))),
    }
}

/// 该指令是否**不能被整条丢弃**: 带副作用, 或是对 volatile 对象的访问
/// (volatile 读本身就是可观测行为, 与结果有没有人用无关)。
pub(super) fn inst_unremovable(kind: &InstKind) -> bool {
    if inst_has_side_effect(kind) {
        return true;
    }
    matches!(
        kind,
        InstKind::Load {
            is_volatile: true,
            ..
        } | InstKind::IndexLoad {
            is_volatile: true,
            ..
        } | InstKind::MemberLoad {
            is_volatile: true,
            ..
        } | InstKind::Store {
            is_volatile: true,
            ..
        } | InstKind::IndexStore {
            is_volatile: true,
            ..
        } | InstKind::MemberStore {
            is_volatile: true,
            ..
        }
    )
}

/// 该指令是否改写内存 (含 volatile 读 —— 它会与相邻的 volatile 访问**定序**),
/// 以及是否是一条带初始化式的声明。用于「两次内存操作之间不得被拉开」。
pub(super) fn inst_clobbers_memory(kind: &InstKind) -> bool {
    if inst_has_side_effect(kind) || matches!(kind, InstKind::PreservedDecl { .. }) {
        return true;
    }
    match kind {
        InstKind::Load { is_volatile, .. }
        | InstKind::IndexLoad { is_volatile, .. }
        | InstKind::MemberLoad { is_volatile, .. } => *is_volatile,
        _ => false,
    }
}

/// 基名**塌缩**后该指令写到了哪些具名对象。
///
/// 重建后 `n_1`/`n_2` 都叫 `n`, 所以「写到基名 X」等价于「X 的值在此处变了」。
/// 除了 SSA `dest`, 还要覆盖经由 `AddrOf` 落到具名对象上的写 (volatile 局部、
/// 数组元素、结构体字段 —— 它们没有 SSA dest, 却确实改了那个对象)。
pub(super) fn inst_written_bases(
    kind: &InstKind,
    dest: Option<&SsaVar>,
    addr_base: &HashMap<String, String>,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(d) = dest {
        out.push(d.base.clone());
    }
    let via_addr = |o: &Operand| -> Option<String> {
        match o {
            Operand::Var(s) => addr_base.get(&s.name()).cloned(),
            _ => None,
        }
    };
    let base_operand = match kind {
        InstKind::Store { e, .. } => Some(e),
        InstKind::IndexStore { arr, .. } => Some(arr),
        InstKind::MemberStore { obj, .. } => Some(obj),
        _ => None,
    };
    if let Some(b) = base_operand.and_then(via_addr) {
        out.push(b);
    }
    out
}
