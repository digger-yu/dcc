// src/ssa.rs - 轻量 SSA IR (M16 优化 pass 基础设施)
//
// 设计原则 (suckless):
//   - 只覆盖结构化控制流 (if/while/for/do-while/return)。含 goto/label/asm-goto
//     /switch(Switch 多 case 简化为 if-chains)/longjmp 的函数降级回 AST pass。
//   - 单一文件: BB / CFG / SSA 值 / phi / dominator 都在这里。
//   - 零外部依赖,仅 std。
//   - IR 形态: 每个变量按 SSA 版本号命名 (base_N); phi 节点在 merge BB 顶端。

use std::collections::{HashMap, HashSet};

use crate::ast::{BinOp, Block, BlockItem, Case, Decl, Expr, FuncDecl, QualType, Stmt, Type, UnOp};
use crate::diagnostics::Span;

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

#[derive(Debug, Clone)]
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
        };
        s.add_block("entry".to_string());
        s
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

// ============================================================
// Dominator tree
// ============================================================

/// 计算 BB 的 dominator set (经典迭代算法)。
/// Derive successor block names from a BB's terminator.
/// `bb.succs` can become stale after SSA passes mutate terminators
/// without updating it, so every analysis that needs successors must
/// derive them from the Terminator directly.
fn term_succs(term: &Terminator) -> Vec<&str> {
    match term {
        Terminator::Jump(t) => vec![t.as_str()],
        Terminator::Branch { t, f, .. } => vec![t.as_str(), f.as_str()],
        Terminator::SwitchTable {
            targets, default, ..
        } => {
            let mut s: Vec<&str> = targets.iter().map(|t| t.as_str()).collect();
            if !s.contains(&default.as_str()) {
                s.push(default.as_str());
            }
            s
        },
        _ => vec![],
    }
}

/// Derive predecessor block names from all BB terminators.
/// Same rationale as `term_succs`: `bb.preds` can become stale.
///
/// 已 pub：P16 的循环识别同样只能从终结符派生前驱 —— `bb.preds` 是构造期缓存，改过
/// terminator 即脏，另写一份派生逻辑必然与这里漂移。
pub fn collect_all_preds(func: &Function) -> HashMap<String, Vec<String>> {
    let mut preds: HashMap<String, Vec<String>> = HashMap::new();
    for (from_name, bb) in &func.blocks {
        for t in term_succs(&bb.term) {
            preds
                .entry(t.to_string())
                .or_default()
                .push(from_name.clone());
        }
    }
    preds
}

pub fn compute_dominators(func: &Function) -> HashMap<String, HashSet<String>> {
    let mut doms: HashMap<String, HashSet<String>> = HashMap::new();
    if !func.blocks.contains_key(&func.entry) {
        return doms;
    }
    // Derive preds from terminators to avoid stale bb.preds
    let derived_preds = collect_all_preds(func);
    // reverse postorder (now correctly derived from terminators)
    let rpo = reverse_postorder(func);
    let all: HashSet<String> = func.blocks.keys().cloned().collect();
    for b in func.blocks.keys() {
        doms.insert(b.clone(), all.clone());
    }
    doms.insert(func.entry.clone(), {
        let mut s = HashSet::new();
        s.insert(func.entry.clone());
        s
    });
    let mut changed = true;
    while changed {
        changed = false;
        for b in &rpo {
            if *b == func.entry {
                continue;
            }
            let preds = match derived_preds.get(b) {
                Some(p) if !p.is_empty() => p,
                _ => continue,
            };
            // intersect of preds
            let mut new: HashSet<String> = all.clone();
            for p in preds {
                if let Some(s) = doms.get(p) {
                    new = new.intersection(s).cloned().collect();
                }
            }
            new.insert(b.clone());
            if doms.get(b) != Some(&new) {
                doms.insert(b.clone(), new);
                changed = true;
            }
        }
    }
    // 不可达块 (CFG 有断边 / 前端漏连) 不在 rpo 里, 因而永远保留上面的 ⊤ 初值
    // = 「被所有块支配」。这会让它每条出边都被 `is_back_edge` 判成回边, 后端
    // SSA→AST 重建于是凭空造出 `while(1) {}` (运行期死循环)。支配关系只对从
    // entry 可达的块有定义: 定点迭代结束后把不可达块收敛成「只支配自己」。
    // 必须在定点循环**之后**做: 循环里 ⊤ 是交集的中性元素, 提前收紧会削掉
    // 可达块真实的支配集。
    let rpo_set: HashSet<&str> = rpo.iter().map(|s| s.as_str()).collect();
    for b in func.blocks.keys() {
        if !rpo_set.contains(b.as_str()) {
            let mut s = HashSet::new();
            s.insert(b.clone());
            doms.insert(b.clone(), s);
        }
    }
    doms
}

fn reverse_postorder(func: &Function) -> Vec<String> {
    let mut order = Vec::new();
    let mut visited = HashSet::new();
    dfs_rpo(&func.entry, func, &mut visited, &mut order);
    order.reverse();
    order
}

fn dfs_rpo(bb: &str, func: &Function, visited: &mut HashSet<String>, order: &mut Vec<String>) {
    if !visited.insert(bb.to_string()) {
        return;
    }
    let succs = term_succs(&func.blocks[bb].term);
    for s in succs {
        dfs_rpo(s, func, visited, order);
    }
    order.push(bb.to_string());
}

// ============================================================
// 入口: 检测函数是否能用 SSA 化 (结构化控制流)
// ============================================================

/// 探测 `FuncDecl::body` 是否含 goto/label/computed-goto/asm-goto/switch。
/// 任何含这些的函数都降级到 AST pass。
pub fn is_structured(body: &Block) -> bool {
    for item in &body.items {
        if !is_structured_item(item) {
            return false;
        }
    }
    true
}

fn is_structured_item(item: &BlockItem) -> bool {
    match item {
        BlockItem::Stmt(s) => is_structured_stmt(s),
        BlockItem::Decl(d) => match d {
            Decl::Func(f) => f.body.as_ref().is_none_or(is_structured),
            _ => true,
        },
    }
}

fn is_structured_stmt(s: &Stmt) -> bool {
    match s {
        // Goto, labels, computed goto, asm-goto, and switch still use
        // AST-level passes because SSA -> AST rebuild cannot preserve these
        // control-flow forms reliably yet.
        Stmt::Goto { .. }
        | Stmt::GotoIndirect { .. }
        | Stmt::Label { .. }
        | Stmt::AsmGoto { .. }
        | Stmt::Switch { .. } => false,
        Stmt::If { then_, else_, .. } => {
            is_structured_stmt(then_) && else_.as_ref().is_none_or(|e| is_structured_stmt(e))
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => is_structured_stmt(body),
        Stmt::For { body, .. } => is_structured_stmt(body),
        Stmt::Block { b, .. } => is_structured(b),
        _ => true,
    }
}

// ============================================================
// AST -> SSA 转换器
// ============================================================

struct Builder {
    func: Function,
    /// 当前 BB 名称
    cur: String,
    /// 变量名 -> 当前 SSA 版本 (per scope, 简化: 整个函数共享)
    versions: HashMap<String, u32>,
    /// 变量名 -> SSA 变量 (当前可见版本)
    current: HashMap<String, SsaVar>,
    /// 待处理的 break/continue 目标 BB
    break_stack: Vec<String>,
    continue_stack: Vec<String>,
    /// 中间临时变量名计数器
    tmp_count: u32,
    /// P0-4 Phase 2: label 名称 -> BB 名称映射
    label_bbs: HashMap<String, String>,
    /// P0-4 Phase 2: 待解析的 goto (源 BB, 目标 label 名)
    pending_gotos: Vec<(String, String)>,
    /// P0-4 Phase 2: 每个 label BB 入口处的变量版本快照 (label -> current 副本)
    label_var_snapshots: HashMap<String, HashMap<String, SsaVar>>,
    /// 本函数内**被取址**的对象根名 (由 `build_from_func` 的第一遍构建收集)。
    addr_taken: std::collections::HashSet<String>,
    /// 记下「该表达式无法在 SSA 里忠实表达」。非空 ⇒ `build_ssa` 放弃整个函数的
    /// SSA 化，该函数留在 AST 通道上。原因文本只在放弃这条冷路上构造。
    bail: Option<String>,
}

impl Builder {
    fn new(func: Function, addr_taken: std::collections::HashSet<String>) -> Self {
        let cur = func.entry.clone();
        let mut b = Builder {
            func,
            cur,
            versions: HashMap::new(),
            current: HashMap::new(),
            break_stack: Vec::new(),
            continue_stack: Vec::new(),
            tmp_count: 0,
            label_bbs: HashMap::new(),
            pending_gotos: Vec::new(),
            label_var_snapshots: HashMap::new(),
            addr_taken,
            bail: None,
        };
        b.func.blocks.get_mut(&b.cur).unwrap().term = Terminator::Unreachable;
        b
    }

    fn fresh(&mut self, base: &str, ty: Type, span: &Span) -> SsaVar {
        self.tmp_count += 1;
        SsaVar {
            base: format!("_t{}_{}", base, self.tmp_count),
            version: 0,
            ty,
            span: span.clone(),
        }
    }

    fn next_version(&mut self, base: &str) -> u32 {
        let v = self.versions.get(base).copied().unwrap_or(0);
        self.versions.insert(base.to_string(), v + 1);
        v + 1
    }

    fn lookup(&self, name: &str) -> Option<SsaVar> {
        self.current.get(name).cloned()
    }

    fn define(&mut self, var: SsaVar) {
        self.current.insert(var.base.clone(), var);
    }

    /// 本表达式无法降级 ⇒ 记下原因，并返回一个**从不进产物**的占位值。
    ///
    /// 旧行为是静默 `Operand::Int(0)`：那个表达式在产物里真的变成 0，于是一批
    /// `_Complex` / `VaArg` / 复合字面量 / `&下标` `&成员` 形态在 -O2 静默误编译
    /// （见 `docs/ssa-pass-upgrade-plan.md` §7.4 第 1 项）。宁可整个函数不走 SSA，
    /// 也不保留「看着像优化、其实是错值」的形态。穷举的 catch-all 保证以后新增的
    /// `Expr` 变体也默认走这条路，而不是静默变 0。
    fn unsupported(&mut self, why: &str) {
        if self.bail.is_none() {
            self.bail = Some(why.to_string());
        }
    }

    /// 当前块已有的指令数（用来框出「刚降级出来的那段指令」）。
    fn cur_inst_count(&self) -> usize {
        self.func.blocks.get(&self.cur).map(|b| b.insts.len()).unwrap_or(usize::MAX)
    }

    /// 下标 `from` 之后新发射的指令是否全部**纯且不陷获** —— 把它无条件执行是否等价。
    ///
    /// 判据建在**实际发射出的指令**上而不是 AST 白名单上：这样它不会与降级代码漂移，
    /// 新增一种降级形态时默认落到「担心的一侧」。三类算不纯：
    ///   * 读内存（Load/IndexLoad/MemberLoad）：分支被提出后可能访问未映射地址（`c ? *p : 0`）；
    ///   * 除法/求余：提前执行会凭空产生 SIGFPE；
    ///   * 给**用户对象**赋值（dest 不是合成临时值）：`x++` 在本 IR 里就是一条
    ///     dest 为 `x.N` 的纯加法，但重建后它是一条用户可见的赋值，两侧都跑就是错。
    fn emitted_is_pure(&self, from: usize) -> bool {
        let Some(bb) = self.func.blocks.get(&self.cur) else {
            return false;
        };
        bb.insts.iter().skip(from).all(|i| {
            if i.dest
                .as_ref()
                .is_some_and(|d| !is_synthetic_temp_base(&self.func, &d.base))
            {
                return false;
            }
            match &i.kind {
                InstKind::Bin { op, .. } => !matches!(op, BinOp::Div | BinOp::Mod),
                InstKind::Un { .. } | InstKind::Cast { .. } | InstKind::Select { .. }
                | InstKind::AddrOf { .. } => true,
                _ => false,
            }
        })
    }

    /// `name` 是否是**本函数的存储** (局部变量或参数)。这是该事实的唯一判据。
    ///
    /// 为什么必须有这个判据: 构建器早期把每个 Ident 左值都当局部变量, 给它开一个
    /// `base = 名字` 的 SSA 版本并作为指令 `dest` 发射。版本名随后进入 `rebuild_block`
    /// 的 `all_def_order` —— 那里只排除「已在 `original_locals`/参数/preserved」的名字,
    /// 于是文件作用域对象被**合成一条本地声明**, codegen 因此把全局的读写落到本帧栈槽:
    /// `-O2` 实测 `int counter; int bump(void){ counter = counter + 3; }` 读写的是
    /// `-0x48(%r13)`, 全局恒为初值 (静默误编译)。
    ///
    /// C 的作用域规则保证函数体内出现的对象名要么是本函数 (含嵌套块 / `for` init /
    /// switch case) 的局部、要么是参数、要么是文件作用域对象, 因此
    /// `original_locals ∪ params` 就是「局部」的完整真源; 前提是
    /// [`collect_local_decls`] 对语句形态的遍历是穷举的 (见其注释)。
    fn is_local_object(&self, name: &str) -> bool {
        if self.func.original_locals.contains(name) {
            return true;
        }
        self.func.params.iter().any(|p| p.base == name)
    }

    /// `name` 是否**必须用内存表示** (不能开 SSA 版本)。
    ///
    /// 两类:
    ///   * 不是本函数的存储 —— 文件作用域对象, 给全局开版本等于在重建时凭空造一条
    ///     遮蔽它的本地声明 (根因见 [`is_local_object`]);
    ///   * **被取址的标量局部** —— `int acc=0; int *p=&acc; *p=5; return acc;` 里
    ///     读 `acc` 必须读到指针那次间接写。按 SSA 版本处理就会拿到进入循环/语句块
    ///     之前的旧值 (静默错值, 实测 -O2 下 `f1()` 返回 0 而不是 5)。
    ///
    /// 数组/结构体/联合体一律排除: 它们的子访问本来就走 `IndexLoad`/`MemberLoad`
    /// (内存), 把整个对象当成一次标量 `Load` 反而会按错误宽度读内存。
    fn is_mem_only_object(&self, name: &str, ty: &Type) -> bool {
        if !self.addr_taken.contains(name) {
            return false;
        }
        self.is_local_object(name)
            && !matches!(ty, Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _))
    }

    /// 为一个「不是局部的名字」发出取址指令, 返回它的地址操作数。
    ///
    /// 与 volatile 局部走的是同一条 `AddrOf` + `Load`/`Store` 形态: 重建结果是
    /// `*(&sym) = v`, 后端按符号表解析 `sym` 并拿到真实对象宽度 —— 静态存储的
    /// 既有通路, 不新增形态。
    fn named_addr_operand(&mut self, name: &str, obj_ty: &Type, span: &Span) -> Operand {
        let addr = self.fresh(
            "naddr",
            Type::Ptr(Box::new(QualType::new(obj_ty.clone()))),
            span,
        );
        self.emit(Inst {
            dest: Some(addr.clone()),
            kind: InstKind::AddrOf {
                base: name.to_string(),
            },
            span: span.clone(),
        });
        self.define(addr.clone());
        Operand::Var(addr)
    }

    /// 读「不是局部的名字」(文件作用域对象) 的当前值。
    fn load_named(&mut self, name: &str, obj_ty: &Type, span: &Span, is_volatile: bool) -> Operand {
        let addr = self.named_addr_operand(name, obj_ty, span);
        let dest = self.fresh("nload", obj_ty.clone(), span);
        self.emit(Inst {
            dest: Some(dest.clone()),
            kind: InstKind::Load {
                e: addr,
                ty: obj_ty.clone(),
                is_volatile,
            },
            span: span.clone(),
        });
        self.define(dest.clone());
        Operand::Var(dest)
    }

    /// 写「不是局部的名字」: 只发内存写, **绝不**为该名字创建 SSA 版本。
    /// 返回被写入的值 (赋值表达式的结果)。
    fn store_to_named_object(
        &mut self,
        name: &str,
        obj_ty: &Type,
        value: Operand,
        span: &Span,
        is_volatile: bool,
    ) -> Operand {
        let addr = self.named_addr_operand(name, obj_ty, span);
        self.emit(Inst {
            dest: None,
            kind: InstKind::Store {
                e: addr,
                src: value.clone(),
                is_volatile,
            },
            span: span.clone(),
        });
        // 聚合对象（结构体/联合体/数组）的**整体赋值**在本 IR 里没有表示：一条
        // `Store` 的 src 是一个值，重建后写成 `*(&g) = p`，后端按标量搬 8 字节，
        // 于是第二个八字节静默丢失。实测（-O2）：
        //     struct P2 { long a, b; } g;  g = p;   →  g.b == 0
        // 而同一份代码的局部拷贝 (`struct P2 t = p;`) 是对的，因为它走
        // `lower_expr_into`（定义一个聚合类型的 SSA 变量），不走 Store。
        // 与其「只写一半」，不如让整个函数放弃 SSA 化，留在 AST 通道上由
        // gen_struct_assign 正确处理 —— 与不可降级表达式、不认识左值同一条规则。
        if matches!(
            obj_ty,
            Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)
        ) {
            self.unsupported("聚合对象的整体赋值无法在 IR 里表达");
            return value;
        }
        // 赋值表达式的值是**存入之后**的对象值（C11 6.5.16p3）：必须先按左值类型截断
        // 并恢复符号/零扩展，才能往外传。直接把原始 RHS 返回去的话，链式赋值会丢掉
        // 中间那一层宽度：`gl = gu = gs = -1`（三个都是全局量）里 `gu = -1` 的值应是
        // 65535，而实测 -O2 把 -1 传进了 `gl`（-O0 与 gcc 都是 65535）。
        // 这里用 Cast 而不是回读内存：不增加可观察访问（volatile 全局也安全）。
        let is_scalar = !matches!(
            obj_ty,
            Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)
        );
        let value_ty = match &value {
            Operand::Var(v) => Some(v.ty.clone()),
            Operand::Int(_) => Some(Type::Int),
            _ => None,
        };
        if is_scalar && value_ty.as_ref() != Some(obj_ty) {
            let cast = self.fresh("astyp", obj_ty.clone(), span);
            self.emit(Inst {
                dest: Some(cast.clone()),
                kind: InstKind::Cast {
                    ty: obj_ty.clone(),
                    e: value,
                },
                span: span.clone(),
            });
            self.define(cast.clone());
            return Operand::Var(cast);
        }
        value
    }

    /// 添加指令到当前 BB
    fn emit(&mut self, inst: Inst) {
        let cur = self.cur.clone();
        if let Some(bb) = self.func.blocks.get_mut(&cur) {
            bb.insts.push(inst);
        }
    }

    fn set_term(&mut self, t: Terminator) {
        let cur = self.cur.clone();
        if let Some(bb) = self.func.blocks.get_mut(&cur) {
            bb.term = t;
        }
    }

    fn split_cur(&mut self, prefix: &str) -> String {
        let old = self.cur.clone();
        let new = self.func.unique_bb(prefix);
        if let Some(old_bb) = self.func.blocks.get_mut(&old) {
            old_bb.succs.push(new.clone());
        }
        if let Some(new_bb) = self.func.blocks.get_mut(&new) {
            new_bb.preds.push(old.clone());
        }
        self.cur = new.clone();
        new
    }

    /// 在 BB 末尾追加 phi (调用方负责在合适的 merge BB 上调用)
    #[allow(dead_code)]
    fn add_phi(&mut self, dest: SsaVar, incoming: Vec<(SsaVar, String)>) {
        let cur = self.cur.clone();
        if let Some(bb) = self.func.blocks.get_mut(&cur) {
            bb.phis.push(Phi { dest, incoming });
        }
    }

    /// 处理一个 AST expression, 返回对应的 SSA operand
    fn lower_expr(&mut self, e: &Expr) -> Operand {
        self.lower_expr_inner(e, None)
    }

    /// 把 `e` 的值算出来并**确定地写进 `target`**, 返回 `target`。
    ///
    /// 为什么需要这层保证而不是直接调 `lower_expr_inner(e, Some(target))`:
    /// `lower_expr_inner` 的 `Expr::Ident` / `Expr::String` / `Expr::Char` 分支只返回
    /// 裸操作数 (局部变量的 SSA 名、全局符号 / 函数地址、字符串地址、字符常量),
    /// 既不发一条 `dest = target` 的指令, 也不做任何物化。调用方 (`Decl::Var` 初始化)
    /// 仍然会把 `target` 登记进 `current`, 于是 SSA→AST 重建时这个变量
    /// **只剩声明而没有赋值** —— 后端按变量名给它分配栈槽, 而那个槽从未被写。
    ///
    /// 后果不是「少一条指令」而是静默误编译: `int (*g)(int,int) = add; return g(x,1);`
    /// 在 -O2 下只剩 `mov -0x40(%r13),%rax; call *%rax` (拿垃圾地址当函数跳, 实测 SEGV);
    /// 同一函数里加一个 `goto` (被 SSA 闸门跳过、不走本路径) 就完全正确。
    fn lower_expr_into(&mut self, e: &Expr, target: SsaVar) -> Operand {
        let got = self.lower_expr_inner(e, Some(target.clone()));
        // 已经落在 target 上 (Binop/Unop/Cast/… 这些分支自己发 dest = target)。
        if let Operand::Var(v) = &got {
            if v.name() == target.name() {
                return got;
            }
        }
        // 其余情形补一次物化, 用与上面 `Expr::Int` / `Expr::Float` 分支同一份
        // 「copy 形态」(`x + 0`): 重建端的 `infer_binop_type` 已保留 `ptr ± int`
        // (含函数名衰减) 的指针类型, 不会把 64 位地址按 `Int` 截断。
        // 不用 `InstKind::Cast`: 它会让操作数走一遍「lvalue → rvalue 转换」路径,
        // 把函数名 `add` 当未声明变量回退成栈槽。也不用 `AddrOf`:
        // `g = &printf;` 在本后端对函数指针变量又是另一条不对齐的宽度路径。
        let kind = match &got {
            Operand::Float(_, fty) => InstKind::Bin {
                op: BinOp::Add,
                l: got.clone(),
                r: Operand::Float(crate::ast::FloatConst::zero(fty), fty.clone()),
            },
            _ => InstKind::Bin {
                op: BinOp::Add,
                l: got.clone(),
                r: Operand::Int(0),
            },
        };
        self.emit(Inst {
            dest: Some(target.clone()),
            kind,
            span: target.span.clone(),
        });
        self.define(target.clone());
        Operand::Var(target)
    }

    fn lower_expr_inner(&mut self, e: &Expr, target: Option<SsaVar>) -> Operand {
        match e {
            Expr::Int { v, .. } => {
                if let Some(t) = target {
                    self.emit(Inst {
                        dest: Some(t.clone()),
                        kind: InstKind::Bin {
                            op: BinOp::Add,
                            l: Operand::Int(*v),
                            r: Operand::Int(0),
                        },
                        span: t.span.clone(),
                    });
                    self.define(t.clone());
                    return Operand::Var(t);
                }
                Operand::Int(*v)
            },
            Expr::Float { c, ty, .. } => {
                if let Some(t) = target {
                    self.emit(Inst {
                        dest: Some(t.clone()),
                        kind: InstKind::Bin {
                            op: BinOp::Add,
                            l: Operand::Float(*c, ty.clone()),
                            r: Operand::Float(crate::ast::FloatConst::zero(ty), ty.clone()),
                        },
                        span: t.span.clone(),
                    });
                    self.define(t.clone());
                    return Operand::Var(t);
                }
                Operand::Float(*c, ty.clone())
            },
            Expr::String { data, .. } => Operand::Str(data.clone()),
            Expr::Char { v, .. } => Operand::Int(*v),
            Expr::Ident { name, span, ty } => {
                // 必须用内存表示的本函数局部 (被取址的标量): 一律从内存读, 不看
                // SSA 版本 —— 版本可能被指针间接改写过, 也可能根本不存在
                // (`int acc; int *p = &acc; *p = 1; return acc;`)。
                if self.is_mem_only_object(&name.name, ty) {
                    let is_volatile = self
                        .func
                        .local_quals
                        .get(name.name.as_str())
                        .map(|q| q.is_volatile())
                        .unwrap_or(false);
                    return self.load_named(&name.name, ty, span, is_volatile);
                }
                if let Some(v) = self.lookup(&name.name) {
                    // volatile 局部变量: 必须从内存加载, 不能复用 SSA 值
                    let is_volatile = self
                        .func
                        .local_quals
                        .get(name.name.as_str())
                        .map(|q| q.is_volatile())
                        .unwrap_or(false);
                    if is_volatile {
                        // 标量类型 volatile 局部变量: 从栈内存加载, 不走 SSA 版本
                        // 复合类型 (数组/结构体/联合体) 直接返回 SSA 变量:
                        // 它们的子访问 (IndexLoad/MemberLoad) 已带 is_volatile 标志
                        match ty {
                            Type::Array(_, _)
                            | Type::Struct(_, _, _, _)
                            | Type::Union(_, _, _, _) => {
                                return Operand::Var(v);
                            },
                            _ => {},
                        }
                        let addr = self.fresh(
                            "vaddr",
                            Type::Ptr(Box::new(QualType::new(ty.clone()))),
                            span,
                        );
                        self.emit(Inst {
                            dest: Some(addr.clone()),
                            kind: InstKind::AddrOf {
                                base: name.name.clone(),
                            },
                            span: span.clone(),
                        });
                        self.define(addr.clone());
                        let dest = target.unwrap_or_else(|| self.fresh("vload", ty.clone(), span));
                        self.emit(Inst {
                            dest: Some(dest.clone()),
                            kind: InstKind::Load {
                                e: Operand::Var(addr),
                                ty: ty.clone(),
                                is_volatile: true,
                            },
                            span: span.clone(),
                        });
                        self.define(dest.clone());
                        return Operand::Var(dest);
                    }
                    return Operand::Var(v);
                }
                // 全局变量 -> SymAddr 占位
                if let Some(g) = self.func.globals.get(&name.name) {
                    return Operand::SymAddr(name.name.clone(), g.ty.clone());
                }
                // 未在本函数登记的名字 = 文件作用域对象 (全局变量 / 函数 / 数组)。
                // 携带类型必须用**前端已算出的 `ty`**, 不能无条件写 `Ptr(void)`:
                // 重建后这就是 `Expr::Ident{name, ty}`, 后端靠该类型区分
                // 「函数名作右值 ⇒ 取符号地址 (`lea add(%rip)`)」与「普通对象 ⇒ 读内存」;
                // 写成 `Ptr(void)` 会让函数名走进「未声明变量」回退路径, 变成读一个
                // **从未被写的栈槽** (实测 `int (*g)(int,int) = add;` 在 -O2 下 SEGV)。
                Operand::SymAddr(name.name.clone(), ty.clone())
            },
            Expr::Binop { op, l, r, span, ty } => {
                let lo = self.lower_expr(l);
                let ro = self.lower_expr(r);
                let dest = target.unwrap_or_else(|| self.fresh("bin", ty.clone(), span));
                self.emit(Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::Bin {
                        op: *op,
                        l: lo,
                        r: ro,
                    },
                    span: span.clone(),
                });
                self.define(dest.clone());
                Operand::Var(dest)
            },
            Expr::Unary {
                op,
                e: inner,
                span,
                ty,
            } => {
                let o = self.lower_expr(inner);
                let dest = target.unwrap_or_else(|| self.fresh("un", ty.clone(), span));
                self.emit(Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::Un { op: *op, e: o },
                    span: span.clone(),
                });
                self.define(dest.clone());
                Operand::Var(dest)
            },
            Expr::Assign {
                op: _,
                l,
                r,
                span,
                ty,
            } => {
                // 对简单标识符赋值: 直接将 rhs 写入目标变量,
                // 避免 lower_lvalue 产生多余的 copy 指令导致 SSA 重复定义。
                // 注: 复合赋值 (+= 等) 已在 parser 中展开为 a = a op rhs。
                if let Expr::Ident {
                    name, ty: var_ty, ..
                } = l.as_ref()
                {
                    // 不是本函数的局部/参数 ⇒ 文件作用域对象。必须先于下面所有
                    // `next_version` 分支拦下: 给全局开版本 = 在重建阶段凭空造一条
                    // 遮蔽它的本地声明 (根因见 `is_local_object`)。
                    if !self.is_local_object(&name.name) {
                        let ro = self.lower_expr(r);
                        // 含 volatile 全局的函数在 `run_ssa_passes` 入口已被
                        // `func_refs_volatile` 跳过, 且 `Type` 不携带 quals, 故恒非 volatile。
                        return self.store_to_named_object(&name.name, var_ty, ro, span, false);
                    }
                    // 被取址的标量局部: 写必须落到内存, 否则指针读不到它。
                    if self.is_mem_only_object(&name.name, var_ty) {
                        let ro = self.lower_expr(r);
                        let is_volatile = self
                            .func
                            .local_quals
                            .get(name.name.as_str())
                            .map(|q| q.is_volatile())
                            .unwrap_or(false);
                        return self.store_to_named_object(&name.name, var_ty, ro, span, is_volatile);
                    }
                    let is_volatile = self
                        .func
                        .local_quals
                        .get(name.name.as_str())
                        .map(|q| q.is_volatile())
                        .unwrap_or(false);
                    if is_volatile {
                        // 标量类型 volatile 局部变量: 强制走 Store 路径, 不直接 SSA define
                        // 复合类型 (数组/结构体/联合体) 走正常 SSA 路径:
                        // 它们的子访问 (IndexStore/MemberStore) 已带 is_volatile 标志
                        let is_scalar = !matches!(
                            var_ty,
                            Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _)
                        );
                        if is_scalar {
                            let ro = self.lower_expr(r);
                            let addr = self.fresh(
                                "vaddr",
                                Type::Ptr(Box::new(QualType::new(var_ty.clone()))),
                                span,
                            );
                            self.emit(Inst {
                                dest: Some(addr.clone()),
                                kind: InstKind::AddrOf {
                                    base: name.name.clone(),
                                },
                                span: span.clone(),
                            });
                            self.define(addr.clone());
                            self.emit(Inst {
                                dest: None,
                                kind: InstKind::Store {
                                    e: Operand::Var(addr),
                                    src: ro,
                                    is_volatile: true,
                                },
                                span: span.clone(),
                            });
                            // 仍然创建一个新版本供后续引用
                            let nv = SsaVar {
                                base: name.name.clone(),
                                version: self.next_version(&name.name),
                                ty: var_ty.clone(),
                                span: span.clone(),
                            };
                            self.define(nv.clone());
                            return Operand::Var(nv);
                        }
                    }
                    let new_v = SsaVar {
                        base: name.name.clone(),
                        version: self.next_version(&name.name),
                        ty: var_ty.clone(),
                        span: span.clone(),
                    };
                    self.lower_expr_into(r, new_v.clone());
                    self.define(new_v.clone());
                    return Operand::Var(new_v);
                }
                let ro = self.lower_expr(r);
                if let Some(dest_var) = lower_lvalue(l, &BinOp::Add, ro.clone(), span, ty, self) {
                    return Operand::Var(dest_var);
                }
                // 左值形态不被 `lower_lvalue` 认识（`__real__ z = r` 这种复数字段就在这里）。
                // 旧行为是什么也不发、只把 RHS 当表达式值返回 ⇒ **这次写静默消失**：
                // `double _Complex z; __real__ z = r; __imag__ z = i; return z;` 过 SSA 后
                // 整个函数只剩 `return z`（实测：两条 Store 根本不在 IR 里），-O2 因此
                // 返回未初始化数据。不能只做不写：跟「不可降级表达式」同一处理，
                // 整个函数放弃 SSA 化，留在能正确处理的 AST 通道上。
                self.unsupported("赋值左值形态无法降级（写会被丢弃）");
                ro
            },
            Expr::Cast { ty, e: inner, span } => {
                let o = self.lower_expr(inner);
                // 预派发的 `target` 只有在类型跟这条 Cast 的结果类型相同时才能直接当
                // dest。否则中间那一层类型会被丢掉：`long a = (unsigned short)-1;` 走的是
                // `lower_expr_into(rhs, a.1)`（`a.1` 的类型是 long），于是 dest 成了 long，
                // `(unsigned short)` 的宽度与符号在 IR 里消失 —— 实测 -O2 下 SCCP 把常量
                // 折成 -1，而 gcc 是 65535。改成两步：先按表达式自己的类型算出结果，
                // 再显式转成目标类型，两层转换都留在 IR 里。
                let outer: Option<SsaVar> =
                    target.as_ref().filter(|t| &t.ty != ty).cloned();
                let dest: SsaVar = match (&target, &outer) {
                    (Some(t), None) => t.clone(),
                    _ => self.fresh("cast", ty.clone(), span),
                };
                self.emit(Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::Cast {
                        ty: ty.clone(),
                        e: o,
                    },
                    span: span.clone(),
                });
                self.define(dest.clone());
                let value = Operand::Var(dest);
                match outer {
                    Some(t) => {
                        self.emit(Inst {
                            dest: Some(t.clone()),
                            kind: InstKind::Cast {
                                ty: t.ty.clone(),
                                e: value,
                            },
                            span: span.clone(),
                        });
                        self.define(t.clone());
                        Operand::Var(t)
                    },
                    None => value,
                }
            },
            Expr::Cond {
                cond,
                then_,
                else_,
                span,
                ty,
            } => {
                let co = self.lower_expr(cond);
                // `InstKind::Select` 是「两个分支都算完再选」，所以它只在两个分支
                // 都**纯且不陷获**时才等价。旧实现无条件用它，于是
                // `x < y ? x++ : y++` 两边自增都执行（实测 -O2 直接 abort）。
                let before = self.cur_inst_count();
                let t = self.lower_expr(then_);
                let then_ok = self.emitted_is_pure(before);
                let before = self.cur_inst_count();
                let f = self.lower_expr(else_);
                let else_ok = self.emitted_is_pure(before);
                if !(then_ok && else_ok) {
                    // 不静默错下去：整个函数放弃 SSA 化，留在 AST 通道（那里是真分支）。
                    self.unsupported("三元分支带副作用或可陷获（Select 会两侧都执行）");
                    return Operand::Int(0);
                }
                let dest = self.fresh("sel", ty.clone(), span);
                self.emit(Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::Select {
                        cond: co,
                        then_: t,
                        else_: f,
                    },
                    span: span.clone(),
                });
                Operand::Var(dest)
            },
            Expr::Comma { l, r, span, ty } => {
                let _ = span;
                let _ = ty;
                // 逗号表达式的左操作数只为副作用存在，但**必须照常降级**：旧实现直接
                // 把它丢掉，于是 `x = (f(), 1)` 里的调用在产物里消失（静默误编译）。
                self.lower_expr(l);
                self.lower_expr(r)
            },
            Expr::Call {
                callee,
                args,
                span,
                ty,
            } => {
                let callee_o = self.lower_expr(callee);
                let arg_ops: Vec<Operand> = args.iter().map(|a| self.lower_expr(a)).collect();
                let ret_ty = ty.clone();
                let dest = if matches!(ret_ty, Type::Void) {
                    None
                } else {
                    Some(target.unwrap_or_else(|| self.fresh("call", ret_ty.clone(), span)))
                };
                self.emit(Inst {
                    dest: dest.clone(),
                    kind: InstKind::Call {
                        callee: callee_o,
                        args: arg_ops,
                        ret_ty: ret_ty.clone(),
                    },
                    span: span.clone(),
                });
                if let Some(ref d) = dest {
                    self.define(d.clone());
                }
                match dest {
                    Some(v) => Operand::Var(v),
                    None => Operand::Int(0),
                }
            },
            Expr::Index { arr, idx, span, ty } => {
                // `arr` 是**数组类型**时需要「数组 → 首元素指针」的衰减，而基址可以是
                // 子对象地址（`z->data` 的基址 = 对象地址 + 字段偏移）。这件事过去在 IR
                // 里无表示，只能整函数退 SSA（§7.29）；现在由 `InstKind::MemberAddr`
                // 与 `Expr::Member`/`Expr::Deref` 的数组分支直接给出基址，下面
                // `lower_expr(arr)` 拿到的就是那个地址，不再需要特殊判据：成员基址
                // （`z->data[i]`）由 `InstKind::MemberAddr` 表达，解引用基址
                // （`(*pa)[i]`）由 `Expr::Deref` 的数组分支表达。
                //
                // 曾以为「解引用基址表达不了」并加了退路，那是误归因：当时发成
                // `lea -88(%r13)`（拿「存放指针的栈槽地址」当指针值）的真因是形参
                // `int (*pa)[3]` 被解析成 Array(Ptr(Int),3) 而非 Ptr(Array(Int,3))，
                // 基址类型错则地址计算必然错。声明符修正后实测逐档与 gcc 一致，
                // 退路已撤（否则就是拿 SSA 覆盖面换一个已经不存在的洞）。
                let a = self.lower_expr(arr);
                let i = self.lower_expr(idx);
                let dest = target.unwrap_or_else(|| self.fresh("idx", ty.clone(), span));
                // 数组元素访问的 volatile: 取 arr 类型 (Array(Ptr(QT))) 中 QT 的 volatile.
                let is_volatile =
                    matches!(&arr.ty(), Type::Array(elem_qt, _) if elem_qt.is_volatile());
                self.emit(Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::IndexLoad {
                        arr: a,
                        idx: i,
                        ty: ty.clone(),
                        is_volatile,
                    },
                    span: span.clone(),
                });
                self.define(dest.clone());
                Operand::Var(dest)
            },
            Expr::Member {
                expr,
                field,
                span,
                ty,
                is_arrow,
            } => {
                let obj_ty = expr.ty();
                // 数组类型表达式的**值就是它的地址**（C 的衰减规则）。`z->data[i]`、
                // `f->data[1] = 7`、`&z->data`、把 `z->data` 当指针传参 —— 四处要的都是
                // 「对象地址 + 字段偏移」，因此这里必须发 `MemberAddr`，而不是走下面
                // 那条把字段当**值**读的路径：后者对柔性数组/零长数组/VLA 成员拿到的是
                // 字段内容而非基址，实测 -O2 读出栈垃圾而 -O0 对（静默误编译，§7.29）。
                //
                // 只改数组类型成员：标量/结构体成员的既有路径不动，否则就是拿覆盖面
                // 换一个不存在的洞。
                if let Type::Array(elem_qt, _) = ty {
                    let obj = self.lower_expr(expr);
                    let ptr_ty = Type::Ptr(elem_qt.clone());
                    let dest = target.unwrap_or_else(|| self.fresh("subaddr", ptr_ty, span));
                    self.emit(Inst {
                        dest: Some(dest.clone()),
                        kind: InstKind::MemberAddr {
                            obj,
                            obj_ty: obj_ty.clone(),
                            field: field.name.clone(),
                            ty: ty.clone(),
                            is_arrow: *is_arrow,
                        },
                        span: span.clone(),
                    });
                    self.define(dest.clone());
                    return Operand::Var(dest);
                }
                let obj = self.lower_expr(expr);
                let dest = target.unwrap_or_else(|| self.fresh("mem", ty.clone(), span));
                // 结构体字段访问: 若对象本身带 volatile, 则访问沿用.
                let obj_is_volatile = match (&obj_ty, expr.as_ref()) {
                    (Type::Ptr(qt), _) if *is_arrow => qt.is_volatile(),
                    (_, Expr::Ident { name, .. }) if !*is_arrow => self
                        .func
                        .local_quals
                        .get(name.name.as_str())
                        .map(|q| q.is_volatile())
                        .unwrap_or(false),
                    _ => false,
                };
                if *is_arrow {
                    // *obj.field: 先解引用指针, 再访问成员
                    let deref_ty = match &obj_ty {
                        Type::Ptr(inner) => inner.base.clone(),
                        other => other.clone(),
                    };
                    let inner =
                        self.fresh("deref", Type::Ptr(Box::new(QualType::new(ty.clone()))), span);
                    self.emit(Inst {
                        dest: Some(inner.clone()),
                        kind: InstKind::Load {
                            e: obj,
                            ty: ty.clone(),
                            is_volatile: obj_is_volatile,
                        },
                        span: span.clone(),
                    });
                    self.emit(Inst {
                        dest: Some(dest.clone()),
                        kind: InstKind::MemberLoad {
                            obj: Operand::Var(inner),
                            obj_ty: deref_ty,
                            field: field.name.clone(),
                            ty: ty.clone(),
                            is_volatile: obj_is_volatile,
                            is_arrow: false,
                        },
                        span: span.clone(),
                    });
                } else {
                    self.emit(Inst {
                        dest: Some(dest.clone()),
                        kind: InstKind::MemberLoad {
                            obj,
                            obj_ty: obj_ty.clone(),
                            field: field.name.clone(),
                            ty: ty.clone(),
                            is_volatile: obj_is_volatile,
                            is_arrow: false,
                        },
                        span: span.clone(),
                    });
                }
                self.define(dest.clone());
                Operand::Var(dest)
            },
            Expr::Deref { e, span, ty } => {
                // 与 `Expr::Member` 的数组分支同一条 C 衰减规则：`(*pa)[i]` 里 `*pa`
                // 是数组类型，它的值就是基址 = `pa` 所含的**指针值**，不该再发一次
                // `Load` 把整个数组当值读进临时帧。
                //
                // 本轮第一次试这条时它发成了 `lea 存放指针的栈槽`（把指针变量当左值
                // 基址），我当时的结论是「IR 表达不了」并加了退路。那个结论错了：
                // 真因是形参声明符 `int (*pa)[3]` 被建成了 Array(Ptr(Int),3) 而不是
                // Ptr(Array(Int,3))（sizeof 返 24 而非 8），基址类型错则地址计算必然错。
                // 解析器修好后此处实测与 gcc 逐档一致，所
                // 以这里不需要退路。（判据本身见 git 历史里那次误归因，教训记在文档。）
                if matches!(ty, Type::Array(_, _)) {
                    return self.lower_expr(e);
                }
                let o = self.lower_expr(e);
                let dest = target.unwrap_or_else(|| self.fresh("deref", ty.clone(), span));
                // volatile 检测: `e` 的类型若是 Ptr(QT) 且 QT.quals 含 volatile.
                let is_volatile = match &e.ty() {
                    Type::Ptr(qt) => qt.is_volatile(),
                    _ => false,
                };
                self.emit(Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::Load {
                        e: o,
                        ty: ty.clone(),
                        is_volatile,
                    },
                    span: span.clone(),
                });
                self.define(dest.clone());
                Operand::Var(dest)
            },
            Expr::AddrOf { e, span, ty: _ } => {
                if let Expr::Ident { name, .. } = e.as_ref() {
                    let dest = target.unwrap_or_else(|| {
                        self.fresh("addr", Type::Ptr(Box::new(QualType::new(Type::Void))), span)
                    });
                    self.emit(Inst {
                        dest: Some(dest.clone()),
                        kind: InstKind::AddrOf {
                            base: name.name.clone(),
                        },
                        span: span.clone(),
                    });
                    self.define(dest.clone());
                    return Operand::Var(dest);
                }
                // `&s.f` / `&p->fld`：子对象地址。以前这里直接 `unsupported`，
                // 而 `MemberAddr` 就是为它加的（偏移由 codegen 的地址形态给）。
                if let Expr::Member {
                    expr,
                    field,
                    span: m_sp,
                    ty: f_ty,
                    is_arrow,
                } = e.as_ref()
                {
                    let obj_ty = expr.ty();
                    let obj = self.lower_expr(expr);
                    let ptr_ty = Type::Ptr(Box::new(QualType::new(f_ty.clone())));
                    let dest = target.unwrap_or_else(|| self.fresh("subaddr", ptr_ty, m_sp));
                    self.emit(Inst {
                        dest: Some(dest.clone()),
                        kind: InstKind::MemberAddr {
                            obj,
                            obj_ty,
                            field: field.name.clone(),
                            ty: f_ty.clone(),
                            is_arrow: *is_arrow,
                        },
                        span: m_sp.clone(),
                    });
                    self.define(dest.clone());
                    return Operand::Var(dest);
                }
                // `&a[i]` 需要地址算术，而 `InstKind::AddrOf` 只能取具名对象的基址。
                self.unsupported("取址的对象不是简单变量（&下标）");
                Operand::Int(0)
            },
            Expr::PostInc {
                e: operand,
                span,
                ty: _,
            }
            | Expr::PreInc {
                e: operand,
                span,
                ty: _,
            }
            | Expr::PostDec {
                e: operand,
                span,
                ty: _,
            }
            | Expr::PreDec {
                e: operand,
                span,
                ty: _,
            } => {
                // 注意: 这里必须对外层表达式 `e` 取 inc/dec 属性。原实现写成了
                // `matches!(&**e, ...)` 而 `e` 已被模式绑定为操作数 (通常是
                // Expr::Ident), 于是 is_inc 恒为 false —— `x++` 实际被降级成
                // `x--`。修复: 把操作数改绑到 `operand`, 让 `e` 重新指向外层节点。
                let is_inc = matches!(e, Expr::PostInc { .. } | Expr::PreInc { .. });
                // 前自增/前自减表达式的值是"自增后的新值", 后自增/后自减才是旧值。
                // 原实现对四种形式一律返回旧值, `int b = ++x;` 得到的是自增前的值。
                let is_pre = matches!(e, Expr::PreInc { .. } | Expr::PreDec { .. });
                let one = if is_inc { 1i128 } else { -1i128 };
                if let Expr::Ident { name, ty, .. } = operand.as_ref() {
                    let is_volatile = self
                        .func
                        .local_quals
                        .get(name.name.as_str())
                        .map(|q| q.is_volatile())
                        .unwrap_or(false);
                    // 与赋值分支用同一条判据: 名字不是本函数的局部/参数 ⇒ 文件作用域对象
                    // (`gcounter++`)，必须走内存读改写，给它开 SSA 版本就是在重建时
                    // 造一条遮蔽全局的本地声明 (根因见 `is_local_object`)。
                    let is_global = !self.is_local_object(&name.name);
                    // 被取址的标量局部与全局同构: 读改写都走内存, 且**不开版本**。
                    let is_mem_local = self.is_mem_only_object(&name.name, ty);
                    // volatile 局部、文件作用域对象与被取址的局部共用
                    // 「从内存取当前值」这一条路径。
                    let (cur_op, addr_op) = if is_volatile || is_global || is_mem_local {
                        let addr = self.named_addr_operand(&name.name, ty, span);
                        let load_dest = self.fresh("memload", ty.clone(), span);
                        self.emit(Inst {
                            dest: Some(load_dest.clone()),
                            kind: InstKind::Load {
                                e: addr.clone(),
                                ty: ty.clone(),
                                is_volatile,
                            },
                            span: span.clone(),
                        });
                        (Operand::Var(load_dest), Some(addr))
                    } else {
                        let cur_v = self.lookup(&name.name).unwrap_or_else(|| SsaVar {
                            base: name.name.clone(),
                            version: 0,
                            ty: ty.clone(),
                            span: span.clone(),
                        });
                        // post 形式 (`k--`) 必须把「自增前的值」**先固定成一个独立临时值**。
                        // 直接返回 `k_1` 会让它在下面 `k_2` 定义之后才被读 (条件表达式就是
                        // 读它的一方), 而 SSA→AST 重建按**基名塌缩** —— `while (k-- > 0)`
                        // 因此变成 `k = k - 1; if (k > 0) {…}` (测的是自增后的值) ⇒
                        // 循环永不退出 (实测 -O2 忙等)。临时值名唯一, 不参与塌缩冲突。
                        if is_pre {
                            (Operand::Var(cur_v), None)
                        } else {
                            let snap = self.fresh("post", ty.clone(), span);
                            self.emit(Inst {
                                dest: Some(snap.clone()),
                                kind: InstKind::Bin {
                                    op: BinOp::Add,
                                    l: Operand::Var(cur_v),
                                    r: Operand::Int(0),
                                },
                                span: span.clone(),
                            });
                            self.define(snap.clone());
                            (Operand::Var(snap), None)
                        }
                    };
                    let one_op = Operand::Int(one);
                    let new_dest = self.fresh("upd", ty.clone(), span);
                    self.emit(Inst {
                        dest: Some(new_dest.clone()),
                        kind: InstKind::Bin {
                            op: BinOp::Add,
                            l: cur_op.clone(),
                            r: one_op,
                        },
                        span: span.clone(),
                    });
                    // store back (内存路径: volatile 局部 或 文件作用域对象)
                    if let Some(addr) = addr_op.clone() {
                        self.emit(Inst {
                            dest: None,
                            kind: InstKind::Store {
                                e: addr,
                                src: Operand::Var(new_dest.clone()),
                                is_volatile,
                            },
                            span: span.clone(),
                        });
                    }
                    let nv = SsaVar {
                        base: name.name.clone(),
                        // 全局与被取址的局部不占版本: 下面既不发射 `dest = nv` 的指令,
                        // 也不 `define(nv)`。
                        version: if is_global || is_mem_local {
                            0
                        } else {
                            self.next_version(&name.name)
                        },
                        ty: ty.clone(),
                        span: span.clone(),
                    };
                    // Non-volatile path: bind nv to new_dest via a copy
                    // (Bin + 0).  Without this, nv is defined but never
                    // assigned, so subsequent reads of the variable would
                    // reference an undefined SSA value.
                    if !is_global && !is_mem_local {
                        if !is_volatile {
                            self.emit(Inst {
                                dest: Some(nv.clone()),
                                kind: InstKind::Bin {
                                    op: BinOp::Add,
                                    l: Operand::Var(new_dest.clone()),
                                    r: Operand::Int(0),
                                },
                                span: span.clone(),
                            });
                        }
                        self.define(nv.clone());
                    }
                    // 表达式结果值:
                    //  - post 形式: 旧值 cur_op
                    //  - pre 形式:  新值 —— 内存路径必须重新从内存 Load
                    //               (新值刚 Store 进去), 非 volatile 局部直接用 nv
                    let new_val_op = if is_pre {
                        if let Some(addr) = addr_op.clone() {
                            let rel = self.fresh("memreload", ty.clone(), span);
                            self.emit(Inst {
                                dest: Some(rel.clone()),
                                kind: InstKind::Load {
                                    e: addr,
                                    ty: ty.clone(),
                                    is_volatile,
                                },
                                span: span.clone(),
                            });
                            Operand::Var(rel)
                        } else {
                            Operand::Var(nv)
                        }
                    } else {
                        cur_op
                    };
                    // 有 target 时 (`int v = x++;`) 结果必须写进 target:
                    // Assign 分支依赖 lower_expr_into 重置 target, 否则该变量
                    // 只有 define 而没有赋值, 优化后会读到未定义值。
                    let v = target
                        .clone()
                        .unwrap_or_else(|| self.fresh("post", ty.clone(), span));
                    self.emit(Inst {
                        dest: Some(v.clone()),
                        kind: InstKind::Bin {
                            op: BinOp::Add,
                            l: new_val_op,
                            r: Operand::Int(0),
                        },
                        span: span.clone(),
                    });
                    self.define(v.clone());
                    return Operand::Var(v);
                }
                Operand::Int(one)
            },
            Expr::SizeofTy { ty, .. } => Operand::Int(ty.size() as i128),
            Expr::SizeofExpr { e, .. } => match e.as_ref() {
                // 字符串字面量: C 中为 char[N] 数组 (含 NUL), 返回实际字节数
                Expr::String { data, .. } => Operand::Int(data.len() as i128),
                _ => Operand::Int(e.ty().size() as i128),
            },
            Expr::AlignofTy { ty, .. } => Operand::Int(ty.align() as i128),
            Expr::AlignofExpr { e, .. } => Operand::Int(e.ty().align() as i128),
            Expr::Offsetof { ty, member, .. } => {
                Operand::Int(crate::const_eval::eval_offsetof(ty, member) as i128)
            },
            Expr::AsmExpr {
                asm_str,
                outputs,
                output_names,
                output_constraints,
                inputs,
                input_names,
                input_constraints,
                clobbers,
                volatile,
                span,
                ty,
            } => {
                let mut input_ops: Vec<Operand> =
                    inputs.iter().map(|e| self.lower_expr(e)).collect();
                let mut output_vars = Vec::new();
                for (idx, out) in outputs.iter().enumerate() {
                    if output_constraints.get(idx).is_some_and(|c| c.contains('+')) {
                        input_ops.push(self.lower_expr(out));
                    }
                    if let Expr::Ident { name, ty, .. } = out {
                        let v = SsaVar {
                            base: name.name.clone(),
                            version: self.next_version(&name.name),
                            ty: ty.clone(),
                            span: span.clone(),
                        };
                        self.define(v.clone());
                        output_vars.push(v);
                    }
                }
                let dest = if matches!(ty, Type::Void) {
                    None
                } else {
                    Some(target.unwrap_or_else(|| self.fresh("asm", ty.clone(), span)))
                };
                self.emit(Inst {
                    dest: dest.clone(),
                    kind: InstKind::InlineAsm {
                        asm_str: asm_str.clone(),
                        outputs: outputs.clone(),
                        output_names: output_names.clone(),
                        output_constraints: output_constraints.clone(),
                        output_vars,
                        inputs: input_ops,
                        input_names: input_names.clone(),
                        input_constraints: input_constraints.clone(),
                        clobbers: clobbers.clone(),
                        volatile: *volatile,
                        ty: ty.clone(),
                    },
                    span: span.clone(),
                });
                if let Some(d) = dest {
                    self.define(d.clone());
                    Operand::Var(d)
                } else {
                    Operand::Int(0)
                }
            },
            // 其余形态（`_Complex` 家族、VaArg、复合字面量、InitList、语句表达式、
            // label 地址）在 SSA 里没有忠实表达：放弃整个函数的 SSA 化，而不是静默
            // 返 0（原因与实测见 `Builder::unsupported`）。这里必须是 catch-all：
            // 以后新增的 `Expr` 变体要落在「不优化」而不是「错值」这一边。
            other => {
                self.unsupported(&format!("表达式无法在 SSA 里忠实表达（{:?}）", expr_shape(other)));
                Operand::Int(0)
            },
        }
    }

    fn lower_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Expr { e, .. } => {
                let _ = self.lower_expr(e);
            },
            Stmt::Return { val, span: _ } => {
                let v = val.as_ref().map(|e| self.lower_expr(e));
                self.set_term(Terminator::Ret { val: v });
                // 返回后所有后续代码在 unreachable BB
                let dead = self.func.unique_bb(&format!("{}.dead", self.cur));
                let _ = dead;
            },
            Stmt::If {
                cond,
                then_,
                else_,
                span: _,
            } => {
                let cond_o = self.lower_expr(cond);
                let then_bb = self.func.unique_bb("if.then");
                let else_bb = self.func.unique_bb("if.else");
                let merge_bb = self.func.unique_bb("if.end");
                self.set_term(Terminator::Branch {
                    cond: cond_o,
                    t: then_bb.clone(),
                    f: else_bb.clone(),
                });
                if let Some(tbb) = self.func.blocks.get_mut(&then_bb) {
                    tbb.preds.push(self.cur.clone());
                    tbb.preds.dedup();
                }
                if let Some(ebb) = self.func.blocks.get_mut(&else_bb) {
                    ebb.preds.push(self.cur.clone());
                    ebb.preds.dedup();
                }
                self.cur = then_bb;
                self.lower_stmt(then_);
                let then_cur = self.cur.clone();
                if let Some(b) = self.func.blocks.get_mut(&then_cur) {
                    if matches!(b.term, Terminator::Unreachable) {
                        b.term = Terminator::Jump(merge_bb.clone());
                        b.succs.push(merge_bb.clone());
                    }
                }
                self.cur = else_bb;
                if let Some(e) = else_ {
                    self.lower_stmt(e);
                }
                let else_cur = self.cur.clone();
                if let Some(b) = self.func.blocks.get_mut(&else_cur) {
                    if matches!(b.term, Terminator::Unreachable) {
                        b.term = Terminator::Jump(merge_bb.clone());
                        b.succs.push(merge_bb.clone());
                    }
                }
                if let Some(mbb) = self.func.blocks.get_mut(&merge_bb) {
                    mbb.preds.push(then_cur);
                    mbb.preds.push(else_cur);
                    mbb.preds.dedup();
                }
                self.cur = merge_bb;
            },
            Stmt::While {
                cond,
                body,
                span: _,
            } => {
                let cond_bb = self.func.unique_bb("while.cond");
                let body_bb = self.func.unique_bb("while.body");
                let end_bb = self.func.unique_bb("while.end");
                if let Some(b) = self.func.blocks.get_mut(&self.cur) {
                    if matches!(b.term, Terminator::Unreachable) {
                        b.term = Terminator::Jump(cond_bb.clone());
                        b.succs.push(cond_bb.clone());
                    }
                }
                if let Some(cb) = self.func.blocks.get_mut(&cond_bb) {
                    cb.preds.push(self.cur.clone());
                }
                self.cur = cond_bb.clone();
                let cond_o = self.lower_expr(cond);
                self.set_term(Terminator::Branch {
                    cond: cond_o,
                    t: body_bb.clone(),
                    f: end_bb.clone(),
                });
                if let Some(bb) = self.func.blocks.get_mut(&body_bb) {
                    bb.preds.push(self.cur.clone());
                }
                if let Some(eb) = self.func.blocks.get_mut(&end_bb) {
                    eb.preds.push(self.cur.clone());
                }
                self.cur = body_bb.clone();
                self.break_stack.push(end_bb.clone());
                self.continue_stack.push(cond_bb.clone());
                self.lower_stmt(body);
                self.break_stack.pop();
                self.continue_stack.pop();
                let body_cur = self.cur.clone();
                if let Some(b) = self.func.blocks.get_mut(&body_cur) {
                    if matches!(b.term, Terminator::Unreachable) {
                        b.term = Terminator::Jump(cond_bb.clone());
                        b.succs.push(cond_bb.clone());
                    }
                }
                if let Some(cb) = self.func.blocks.get_mut(&cond_bb) {
                    cb.preds.push(body_cur);
                    cb.preds.dedup();
                }
                self.cur = end_bb;
            },
            Stmt::Do {
                body,
                cond,
                span: _,
            } => {
                let body_bb = self.func.unique_bb("do.body");
                let cond_bb = self.func.unique_bb("do.cond");
                let end_bb = self.func.unique_bb("do.end");
                if let Some(b) = self.func.blocks.get_mut(&self.cur) {
                    if matches!(b.term, Terminator::Unreachable) {
                        b.term = Terminator::Jump(body_bb.clone());
                        b.succs.push(body_bb.clone());
                    }
                }
                if let Some(bb) = self.func.blocks.get_mut(&body_bb) {
                    bb.preds.push(self.cur.clone());
                }
                self.cur = body_bb.clone();
                self.break_stack.push(end_bb.clone());
                self.continue_stack.push(cond_bb.clone());
                self.lower_stmt(body);
                self.break_stack.pop();
                self.continue_stack.pop();
                let body_cur = self.cur.clone();
                if let Some(b) = self.func.blocks.get_mut(&body_cur) {
                    if matches!(b.term, Terminator::Unreachable) {
                        b.term = Terminator::Jump(cond_bb.clone());
                        b.succs.push(cond_bb.clone());
                    }
                }
                if let Some(cb) = self.func.blocks.get_mut(&cond_bb) {
                    cb.preds.push(body_cur);
                }
                self.cur = cond_bb.clone();
                let cond_o = self.lower_expr(cond);
                self.set_term(Terminator::Branch {
                    cond: cond_o,
                    t: body_bb.clone(),
                    f: end_bb.clone(),
                });
                if let Some(bb) = self.func.blocks.get_mut(&body_bb) {
                    bb.preds.push(self.cur.clone());
                }
                if let Some(eb) = self.func.blocks.get_mut(&end_bb) {
                    eb.preds.push(self.cur.clone());
                }
                self.cur = end_bb;
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                span: _,
            } => {
                if let Some(i) = init {
                    self.lower_stmt(i);
                }
                let cond_bb = self.func.unique_bb("for.cond");
                let body_bb = self.func.unique_bb("for.body");
                let step_bb = self.func.unique_bb("for.step");
                let end_bb = self.func.unique_bb("for.end");
                if let Some(b) = self.func.blocks.get_mut(&self.cur) {
                    if matches!(b.term, Terminator::Unreachable) {
                        b.term = Terminator::Jump(cond_bb.clone());
                        b.succs.push(cond_bb.clone());
                    }
                }
                if let Some(cb) = self.func.blocks.get_mut(&cond_bb) {
                    cb.preds.push(self.cur.clone());
                }
                self.cur = cond_bb.clone();
                let cond_o = cond
                    .as_ref()
                    .map(|c| self.lower_expr(c))
                    .unwrap_or(Operand::Int(1));
                self.set_term(Terminator::Branch {
                    cond: cond_o,
                    t: body_bb.clone(),
                    f: end_bb.clone(),
                });
                if let Some(bb) = self.func.blocks.get_mut(&body_bb) {
                    bb.preds.push(self.cur.clone());
                }
                if let Some(eb) = self.func.blocks.get_mut(&end_bb) {
                    eb.preds.push(self.cur.clone());
                }
                self.cur = body_bb.clone();
                self.break_stack.push(end_bb.clone());
                self.continue_stack.push(step_bb.clone());
                self.lower_stmt(body);
                self.break_stack.pop();
                self.continue_stack.pop();
                let body_cur = self.cur.clone();
                if let Some(b) = self.func.blocks.get_mut(&body_cur) {
                    if matches!(b.term, Terminator::Unreachable) {
                        b.term = Terminator::Jump(step_bb.clone());
                        b.succs.push(step_bb.clone());
                    }
                }
                if let Some(sb) = self.func.blocks.get_mut(&step_bb) {
                    sb.preds.push(body_cur);
                }
                self.cur = step_bb.clone();
                if let Some(st) = step {
                    let _ = self.lower_expr(st);
                }
                // Re-evaluate condition with updated variables after body.
                // This ensures the branch uses current SSA versions,
                // not the stale versions from the initial cond_bb.
                let fresh_cond = cond
                    .as_ref()
                    .map(|c| self.lower_expr(c))
                    .unwrap_or(Operand::Int(1));
                self.set_term(Terminator::Branch {
                    cond: fresh_cond,
                    t: body_bb.clone(),
                    f: end_bb.clone(),
                });
                if let Some(b) = self.func.blocks.get_mut(&body_bb) {
                    if !b.preds.contains(&self.cur.clone()) {
                        b.preds.push(self.cur.clone());
                    }
                }
                if let Some(eb) = self.func.blocks.get_mut(&end_bb) {
                    if !eb.preds.contains(&self.cur.clone()) {
                        eb.preds.push(self.cur.clone());
                    }
                }
                // `for.cond` 必须**保留自己的 Branch** 作为入口测试。旧实现在这里把它
                // 改写成 `Jump(body)` (注释: "its instructions provide defs needed by body"),
                // 只留下 `for.step` 的底部测试 —— 那等于把循环变成了「先进体再判」的
                // do-while: 零次迭代的 `for (i=0;i<n;i++)` 在 n==0 时仍会跑一圈,
                // 而 `_tbin_1 = i<n` 成了无人使用的孤儿比较。旧行为靠闸门
                // (含循环的函数不进 SSA) 遮蔽, 解锁后必须修正。
                // 保留顶部 Branch 后: 入口由 `for.cond` 判定, 后续各轮由 `for.step`
                // 判定 (它拿到的已是体内自增后的现行版本), 两者不重复也不漏。
                self.cur = end_bb;
            },
            Stmt::Block { b, .. } => {
                self.lower_block(b);
            },
            Stmt::Break { .. } => {
                if let Some(t) = self.break_stack.last().cloned() {
                    self.set_term(Terminator::Jump(t.clone()));
                    if let Some(b) = self.func.blocks.get_mut(&self.cur) {
                        b.succs.push(t);
                    }
                    let dead = self.func.unique_bb(&format!("{}.dead", self.cur));
                    self.cur = dead;
                }
            },
            Stmt::Continue { .. } => {
                if let Some(t) = self.continue_stack.last().cloned() {
                    self.set_term(Terminator::Jump(t.clone()));
                    if let Some(b) = self.func.blocks.get_mut(&self.cur) {
                        b.succs.push(t);
                    }
                    let dead = self.func.unique_bb(&format!("{}.dead", self.cur));
                    self.cur = dead;
                }
            },
            // P0-4 Phase 2: goto/label 支持
            Stmt::Label { name, stmt, .. } => {
                let label_name = name.name.clone();
                // 创建 label BB (如果尚未创建)
                let label_bb = self
                    .label_bbs
                    .entry(label_name.clone())
                    .or_insert_with(|| self.func.unique_bb(&format!("label.{}", label_name)))
                    .clone();
                // 保存当前变量版本快照 (用于 phi 插入)
                self.label_var_snapshots
                    .insert(label_name, self.current.clone());
                // 从当前 BB 跳转到 label BB
                if let Some(b) = self.func.blocks.get_mut(&self.cur) {
                    if matches!(b.term, Terminator::Unreachable) {
                        b.term = Terminator::Jump(label_bb.clone());
                        b.succs.push(label_bb.clone());
                    }
                }
                if let Some(lbb) = self.func.blocks.get_mut(&label_bb) {
                    lbb.preds.push(self.cur.clone());
                    lbb.preds.dedup();
                }
                self.cur = label_bb;
                // 继续处理 label 后面的语句
                self.lower_stmt(stmt);
            },
            Stmt::Goto { target, .. } => {
                let label_name = target.name.clone();
                // 确保目标 label BB 存在
                let target_bb = self
                    .label_bbs
                    .entry(label_name.clone())
                    .or_insert_with(|| self.func.unique_bb(&format!("label.{}", label_name)))
                    .clone();
                // 记录待解析的 goto
                self.pending_gotos.push((self.cur.clone(), label_name));
                // 设置跳转
                self.set_term(Terminator::Jump(target_bb.clone()));
                if let Some(b) = self.func.blocks.get_mut(&self.cur) {
                    b.succs.push(target_bb.clone());
                }
                if let Some(tbb) = self.func.blocks.get_mut(&target_bb) {
                    tbb.preds.push(self.cur.clone());
                    tbb.preds.dedup();
                }
                // goto 后的代码在 unreachable BB
                let dead = self.func.unique_bb(&format!("{}.dead", self.cur));
                self.cur = dead;
            },
            Stmt::GotoIndirect { expr, .. } => {
                // computed goto: 保守处理，跳转到所有已知 label BB
                let _ = self.lower_expr(expr);
                // 保守: 添加边到所有已知 label BB
                for target_bb in self.label_bbs.values() {
                    if let Some(tbb) = self.func.blocks.get_mut(target_bb) {
                        tbb.preds.push(self.cur.clone());
                        tbb.preds.dedup();
                    }
                    if let Some(b) = self.func.blocks.get_mut(&self.cur) {
                        b.succs.push(target_bb.clone());
                    }
                }
                self.set_term(Terminator::Unreachable);
                let dead = self.func.unique_bb(&format!("{}.dead", self.cur));
                self.cur = dead;
            },
            Stmt::Switch { expr, cases, span } => {
                // 密集 case 值走 SwitchTable 终结符
                self.lower_switch(expr, cases, span);
            },
            _ => {}, // 其他: 已通过 is_structured 过滤
        }
    }

    /// 处理 switch 语句: 密集 case 值生成 SwitchTable 终结符
    fn lower_switch(&mut self, expr: &Expr, cases: &[Case], _span: &crate::diagnostics::Span) {
        // 分离普通 case 和 default
        let mut normal_cases: Vec<&Case> = Vec::new();
        let mut default_case: Option<&Case> = None;
        for c in cases {
            if c.val.is_none() {
                default_case = Some(c);
            } else {
                normal_cases.push(c);
            }
        }
        if normal_cases.is_empty() {
            // 只有 default 或空 switch: 直接跳 default 或 exit
            let exit_bb = self.func.unique_bb("sw.exit");
            if let Some(def) = default_case {
                self.set_term(Terminator::Jump(exit_bb.clone()));
                if let Some(b) = self.func.blocks.get_mut(&self.cur) {
                    b.succs.push(exit_bb.clone());
                }
                self.cur = exit_bb;
                self.lower_block(&def.body);
            } else {
                self.set_term(Terminator::Jump(exit_bb.clone()));
                if let Some(b) = self.func.blocks.get_mut(&self.cur) {
                    b.succs.push(exit_bb.clone());
                }
                self.cur = exit_bb;
            }
            return;
        }
        let idx = self.lower_expr(expr);
        // 创建 default BB
        let default_bb = self.func.unique_bb("sw.default");
        let exit_bb = self.func.unique_bb("sw.exit");
        // 收集 case 值和目标 BB
        let mut values: Vec<i128> = Vec::new();
        let mut targets: Vec<String> = Vec::new();
        for c in &normal_cases {
            let val = c.val.as_ref().unwrap();
            let iv = match val {
                Expr::Int { v, .. } => *v,
                _ => 0,
            };
            values.push(iv);
            let case_bb = self.func.unique_bb(&format!("sw.case.{}", iv));
            targets.push(case_bb);
        }
        // 设置 SwitchTable 终结符
        let mut all_succs: Vec<String> = targets.clone();
        all_succs.push(default_bb.clone());
        self.set_term(Terminator::SwitchTable {
            idx,
            values: values.clone(),
            targets: targets.clone(),
            default: default_bb.clone(),
        });
        if let Some(b) = self.func.blocks.get_mut(&self.cur) {
            b.succs = all_succs;
        }
        // 设置 break 栈以便 case 内的 break 能跳出 switch
        self.break_stack.push(exit_bb.clone());
        // 生成每个 case BB
        for (i, case_bb_name) in targets.iter().enumerate() {
            self.cur = case_bb_name.clone();
            if let Some(bb) = self.func.blocks.get_mut(case_bb_name) {
                bb.preds.push(self.cur.clone());
            }
            self.lower_block(&normal_cases[i].body);
            // case 末尾跳到 exit (如果没有 break/return)
            let cur = self.cur.clone();
            if let Some(b) = self.func.blocks.get_mut(&cur) {
                if matches!(b.term, Terminator::Unreachable) {
                    b.term = Terminator::Jump(exit_bb.clone());
                    b.succs.push(exit_bb.clone());
                }
            }
        }
        // 生成 default BB
        self.cur = default_bb.clone();
        if let Some(bb) = self.func.blocks.get_mut(&default_bb) {
            bb.preds.push(self.cur.clone());
        }
        if let Some(def) = default_case {
            self.lower_block(&def.body);
        }
        let cur = self.cur.clone();
        if let Some(b) = self.func.blocks.get_mut(&cur) {
            if matches!(b.term, Terminator::Unreachable) {
                b.term = Terminator::Jump(exit_bb.clone());
                b.succs.push(exit_bb.clone());
            }
        }
        self.break_stack.pop();
        self.cur = exit_bb;
    }

    fn lower_block(&mut self, b: &Block) {
        for item in &b.items {
            match item {
                BlockItem::Stmt(s) => self.lower_stmt(s),
                BlockItem::Decl(Decl::Var(v)) => {
                    // extern 声明引用全局符号, 不创建 SSA 局部变量
                    if v.is_extern {
                        continue;
                    }
                    // 检查初始化式是否包含 SSA 无法表示的模式 (复合字面量/初始化列表)
                    let has_complex_init = v.init.as_ref().is_some_and(|init| {
                        matches!(init, Expr::CompoundLit { .. } | Expr::InitList { .. })
                    });
                    if has_complex_init {
                        // 保留原始声明,不做 SSA 化
                        self.emit(Inst {
                            dest: None,
                            kind: InstKind::PreservedDecl { var: v.clone() },
                            span: v.span.clone(),
                        });
                        // 仍然定义变量以便后续使用
                        let v_0 = SsaVar {
                            base: v.name.name.clone(),
                            version: self.next_version(&v.name.name),
                            ty: v.ty.clone(),
                            span: v.span.clone(),
                        };
                        self.define(v_0);
                    } else if self.is_mem_only_object(&v.name.name, &v.ty) {
                        // 被取址的标量局部: 声明处的初始化也必须落到内存, 否则
                        // `int acc = 0; int *p = &acc;` 里 `*p` 读到未初始化槽位。
                        // 不开 SSA 版本 (后续读写均走内存路径)。
                        if let Some(init) = &v.init {
                            let ro = self.lower_expr(init);
                            self.store_to_named_object(
                                &v.name.name,
                                &v.ty,
                                ro,
                                &v.span,
                                v.quals.is_volatile(),
                            );
                        }
                    } else if let Some(init) = &v.init {
                        let v_0 = SsaVar {
                            base: v.name.name.clone(),
                            version: self.next_version(&v.name.name),
                            ty: v.ty.clone(),
                            span: v.span.clone(),
                        };
                        // Lower init expr directly into the declared variable
                        // to avoid creating unnecessary SSA temporaries.
                        self.lower_expr_into(init, v_0.clone());
                        self.define(v_0);
                    } else {
                        let v_0 = SsaVar {
                            base: v.name.name.clone(),
                            version: self.next_version(&v.name.name),
                            ty: v.ty.clone(),
                            span: v.span.clone(),
                        };
                        self.define(v_0);
                    }
                },
                _ => {},
            }
        }
    }
}

fn lower_lvalue(
    l: &Expr,
    _op: &BinOp,
    src: Operand,
    span: &Span,
    ty: &Type,
    b: &mut Builder,
) -> Option<SsaVar> {
    if let Expr::Ident {
        name, ty: var_ty, ..
    } = l
    {
        // 与 `Expr::Assign` 分支用同一条判据: 文件作用域对象走内存写, 不开 SSA 版本。
        // 当前唯一调用点 (`Expr::Assign`) 已在进入本函数前拦下 Ident 左值, 保留该分支
        // 是为了让「以名字为左值的写」只有一个真源, 避免今后新增调用者重新造出
        // 「用全局名当指令 dest」这条静默遮蔽路径。
        if !b.is_local_object(&name.name) {
            b.store_to_named_object(&name.name, var_ty, src.clone(), span, false);
            return None;
        }
        // 被取址的标量局部: 与赋值分支用同一条判据, 写内存、不开版本。
        if b.is_mem_only_object(&name.name, var_ty) {
            let is_volatile = b
                .func
                .local_quals
                .get(name.name.as_str())
                .map(|q| q.is_volatile())
                .unwrap_or(false);
            b.store_to_named_object(&name.name, var_ty, src.clone(), span, is_volatile);
            return None;
        }
        let v = SsaVar {
            base: name.name.clone(),
            version: b.next_version(&name.name),
            ty: var_ty.clone(),
            span: span.clone(),
        };
        // Emit a copy instruction so rebuild_block connects the
        // computed value to the SSA variable name.
        // Use lower_expr_into if src is a simple operand to avoid
        // creating extra temporaries.
        let v_clone = v.clone();
        match &src {
            Operand::Var(_src_var) => {
                // Source is already a variable - emit direct copy
                b.emit(Inst {
                    dest: Some(v_clone.clone()),
                    kind: InstKind::Bin {
                        op: BinOp::Add,
                        l: src.clone(),
                        r: Operand::Int(0),
                    },
                    span: span.clone(),
                });
            },
            _ => {
                // Source is a constant - emit as immediate assignment
                b.emit(Inst {
                    dest: Some(v_clone.clone()),
                    kind: InstKind::Bin {
                        op: BinOp::Add,
                        l: src.clone(),
                        r: Operand::Int(0),
                    },
                    span: span.clone(),
                });
            },
        }
        b.define(v.clone());
        return Some(v);
    }
    if let Expr::Deref { e: inner, .. } = l {
        let addr = b.lower_expr(inner);
        let store_volatile = match inner.ty() {
            Type::Ptr(qt) => qt.is_volatile(),
            _ => false,
        };
        b.emit(Inst {
            dest: None,
            kind: InstKind::Store {
                e: addr,
                src,
                is_volatile: store_volatile,
            },
            span: span.clone(),
        });
        let dest = b.fresh("store", ty.clone(), span);
        return Some(dest);
    }
    if let Expr::Index { arr, idx, ty, .. } = l {
        let a = b.lower_expr(arr);
        let i = b.lower_expr(idx);
        let store_volatile = match arr.ty() {
            Type::Array(elem_qt, _) => elem_qt.is_volatile(),
            _ => false,
        };
        b.emit(Inst {
            dest: None,
            kind: InstKind::IndexStore {
                arr: a,
                idx: i,
                src,
                ty: ty.clone(),
                is_volatile: store_volatile,
            },
            span: span.clone(),
        });
        let dest = b.fresh("idxstore", ty.clone(), span);
        return Some(dest);
    }
    if let Expr::Member {
        expr,
        field,
        is_arrow,
        ..
    } = l
    {
        let obj_ty = expr.ty();
        let obj = b.lower_expr(expr);
        let member_volatile = match (&obj_ty, expr.as_ref()) {
            (Type::Ptr(qt), _) if *is_arrow => qt.is_volatile(),
            (_, Expr::Ident { name, .. }) if !*is_arrow => b
                .func
                .local_quals
                .get(name.name.as_str())
                .map(|q| q.is_volatile())
                .unwrap_or(false),
            _ => false,
        };
        if *is_arrow {
            let deref_ty = match &obj_ty {
                Type::Ptr(inner) => inner.base.clone(),
                other => other.clone(),
            };
            let inner = b.fresh("deref", Type::Ptr(Box::new(QualType::new(ty.clone()))), span);
            b.emit(Inst {
                dest: Some(inner.clone()),
                kind: InstKind::Load {
                    e: obj,
                    ty: ty.clone(),
                    is_volatile: member_volatile,
                },
                span: span.clone(),
            });
            b.emit(Inst {
                dest: None,
                kind: InstKind::MemberStore {
                    obj: Operand::Var(inner),
                    obj_ty: deref_ty,
                    field: field.name.clone(),
                    src,
                    ty: ty.clone(),
                    is_volatile: member_volatile,
                    is_arrow: false,
                },
                span: span.clone(),
            });
        } else {
            b.emit(Inst {
                dest: None,
                kind: InstKind::MemberStore {
                    obj,
                    obj_ty: obj_ty.clone(),
                    field: field.name.clone(),
                    src,
                    ty: ty.clone(),
                    is_volatile: member_volatile,
                    is_arrow: false,
                },
                span: span.clone(),
            });
        }
        let dest = b.fresh("memstore", ty.clone(), span);
        return Some(dest);
    }
    None
}

/// 递归收集 block 中所有 Decl::Var 的变量名
fn collect_local_decls(
    items: &[BlockItem],
    out: &mut OrderedNameSet,
    types: &mut HashMap<String, Type>,
    quals: &mut HashMap<String, crate::ast::QualifierFlags>,
) {
    for item in items {
        match item {
            BlockItem::Decl(Decl::Var(v)) => {
                // extern 声明引用全局符号, 不作为局部变量收集
                if !v.is_extern {
                    out.insert(v.name.name.clone());
                    types.insert(v.name.name.clone(), v.ty.clone());
                    quals.insert(v.name.name.clone(), v.quals);
                }
            },
            BlockItem::Stmt(s) => collect_local_decls_stmt(s, out, types, quals),
            _ => {},
        }
    }
}

fn collect_local_decls_stmt(
    s: &Stmt,
    out: &mut OrderedNameSet,
    types: &mut HashMap<String, Type>,
    quals: &mut HashMap<String, crate::ast::QualifierFlags>,
) {
    // 本函数必须是**穷举**的: `is_local_object` 以 `original_locals` 为「局部」的唯一真源,
    // 漏掉任何一个可以携带声明的语句形态, 就会把该形态里声明的局部变量误判为文件
    // 作用域对象 (写它时变成 `*(&name) = v`, 后端找不到该符号即报 E1103)。
    // 故这里不写 `_ => {}` 兼容分支: 新增 `Stmt` 形态时本函数会编译失败, 强制同步。
    match s {
        Stmt::Block { b, .. } => collect_local_decls(&b.items, out, types, quals),
        Stmt::If { then_, else_, .. } => {
            collect_local_decls_stmt(then_, out, types, quals);
            if let Some(e) = else_ {
                collect_local_decls_stmt(e, out, types, quals);
            }
        },
        // `for (int i = 0; ...)` 的声明藏在 init 语句里, 与 body 同等重要。
        Stmt::For { init, body, .. } => {
            if let Some(i) = init {
                collect_local_decls_stmt(i, out, types, quals);
            }
            collect_local_decls_stmt(body, out, types, quals);
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            collect_local_decls_stmt(body, out, types, quals);
        },
        Stmt::Switch { cases, .. } => {
            for c in cases {
                collect_local_decls(&c.body.items, out, types, quals);
            }
        },
        Stmt::Label { stmt, .. } => {
            collect_local_decls_stmt(stmt, out, types, quals);
        },
        // 以下形态不可能包含声明: 只读取值 / 控制流边 / 本身不嵌入子语句。
        Stmt::Expr { .. }
        | Stmt::Return { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Fallthrough { .. }
        | Stmt::Goto { .. }
        | Stmt::GotoIndirect { .. }
        | Stmt::AsmGoto { .. } => {},
        // C++ 零成本异常: `Invoke` 的 `call` 是一次受保护调用, `EhLandingPad` 是两条
        // 寄存器交接声明 —— 两者都不携带块级声明, 且含它们的函数走不到本路径
        // (`is_structured` 已将 EH 形态旁路), 仍显式列出以保持穷举。
        Stmt::Invoke { .. } | Stmt::EhLandingPad { .. } => {},
    }
}

/// 把一批 SSA 引用改写到另一个 SSA 变量 (IR 级基础设施, phi 插入与优化 pass 共用)。
///
/// 为什么住在 `ssa.rs` 而不是各 pass 里各写一份: 这份枚举必须对 `InstKind` **穷举**,
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
        }
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

/// 结构不变式检查: SSA IR 里每个被引用的名字都必须有定义。
///
/// 为什么需要一个「自己检查自己」的函数: 删除指令的 pass (值编号、死代码消除) 都是
/// 「改写引用 + 删除定义」两步动作, 其中任一步因guards/去重而被跳过时, 留下的就是
/// 指向已删定义的悬空引用 —— 重建后表现为读一个从未赋值的临时名/槽位, 是**静默**错值。
/// 本轮 GVN 就因「拒绝第二次改写同一源名但仍记录删除」造出过 `if (_tbin_2)` 这种引用。
/// 把它做成可调用的谓词, 单测就能把该不变式长期钉住。
pub fn dangling_refs(func: &Function) -> Vec<String> {
    let mut defined: HashSet<String> = HashSet::new();
    for p in &func.params {
        defined.insert(p.name());
    }
    for bb in func.blocks.values() {
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                defined.insert(d.name());
            }
        }
        for phi in &bb.phis {
            defined.insert(phi.dest.name());
        }
    }
    let mut bad: Vec<String> = Vec::new();
    for bb in ordered_block_names(func) {
        let Some(b) = func.blocks.get(&bb) else {
            continue;
        };
        let mut names: Vec<String> = Vec::new();
        for inst in &b.insts {
            names.extend(collect_uses_kind(&inst.kind));
        }
        for phi in &b.phis {
            for (v, _) in &phi.incoming {
                names.push(v.name());
            }
        }
        match &b.term {
            Terminator::Ret { val: Some(o) } => names.extend(collect_uses_operand(o)),
            Terminator::Branch { cond, .. } => names.extend(collect_uses_operand(cond)),
            Terminator::SwitchTable { idx, .. } => names.extend(collect_uses_operand(idx)),
            _ => {}
        }
        for n in names {
            if !defined.contains(&n) {
                bad.push(format!("{bb}: {n}"));
            }
        }
    }
    bad.sort();
    bad.dedup();
    bad
}

/// 结构不变式：全函数**单赋值** —— 每个 SSA 名最多被定义一次
/// (一个 dest 指令或一个 phi)。返回违反该不变式的名字列表。
///
/// 为什么必须有这条检查而不只靠 `dangling_refs`：pass 可以保留原有指令不动、
/// 只把一批副本追到同一个块末尾，就造成同 SSA 名的多条定义。那在真 SSA 里
/// 不会报错（读的都是名字），但 SSA→AST 重建按**基名塌缩 + 发射序**产出语句，
/// 副本会被**再执行一遍**：如果两份中间夹了对同一基名的写入，第二份算出的就是
/// 错值。已删的 SSA 层伪 `loop_tiling`（“把纯运算指令复制一份追加”）就是这条
/// 不变式的真实反例，当时实测 -O3 得 38 而应 32。
pub fn duplicate_ssa_defs(func: &Function) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut dup: HashSet<String> = HashSet::new();
    for bb in func.blocks.values() {
        for phi in &bb.phis {
            if !seen.insert(phi.dest.name()) {
                dup.insert(phi.dest.name());
            }
        }
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                if !seen.insert(d.name()) {
                    dup.insert(d.name());
                }
            }
        }
    }
    let mut out: Vec<String> = dup.into_iter().collect();
    out.sort();
    out
}

/// 一条指令读到的全部 SSA 名 (含 asm 输入与成员/下标基址)。
fn collect_uses_kind(k: &InstKind) -> Vec<String> {
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
        InstKind::AddrOf { .. } | InstKind::PreservedDecl { .. } => {}
        InstKind::EhLandingPad { .. } => {}
    }
    v
}

fn collect_uses_operand(o: &Operand) -> Vec<String> {
    match o {
        Operand::Var(v) => vec![v.name()],
        Operand::Phi(pairs) => pairs.iter().map(|(v, _)| v.name()).collect(),
        _ => Vec::new(),
    }
}

/// 由终结符重建 `bb.preds` / `bb.succs` —— 这两个字段是全函数唯一的缓存型 CFG 数据。
///
/// 为什么必须由本函数统一维护: `preds`/`succs` 是在接线时逐个 `push` 上去的, 而任何
/// 改写 terminator 的动作 (for 循环的旋转、`lower_switches_in_func`、SCCP 的常量 switch
/// 折叠、P19 跳转穿线) 都**不会**同步它们。而 `find_loop_body_multi` 这类分析又是沿
/// `preds` 反向走的 —— 读到的就是旧边。实测后果不是「少优化一点」而是重建出错:
/// `for (...) { if (i==1) continue; ... }` 的回边被发成空的 `if (i<n) {}`, 循环一次也不跑。
/// 本模块开头 `term_succs` / `collect_all_preds` 的注释早就写明「preds 会陈旧」,
/// 但没人负责让它不陈旧 —— 现在有两个读取点就有一份真源。
pub fn refresh_cfg(func: &mut Function) {
    let preds = collect_all_preds(func);
    for bb in func.blocks.values_mut() {
        bb.succs = term_succs(&bb.term).into_iter().map(|s| s.to_string()).collect();
        bb.preds = preds.get(&bb.name).cloned().unwrap_or_default();
    }
}

/// 从终结符派生后继的公开入点 (与 `term_succs` 同一真源, 供 pass 使用)。
pub fn term_succs_pub(term: &Terminator) -> Vec<String> {
    term_succs(term).into_iter().map(|s| s.to_string()).collect()
}

/// Insert phi nodes at loop headers for variables modified in loop bodies.
///
/// When the SSA builder processes loops (while/for/do), it creates a CFG
/// with back edges. Variables modified in the loop body get new SSA versions,
/// but without phi nodes at the loop header, SSA optimization passes cannot
/// correctly reason about loop-carried values.
///
/// This pass:
/// 1. Identifies back edges (latch → header) using dominator analysis
/// 2. For each loop header, collects variables defined in the loop body
/// 3. Inserts phi nodes merging the pre-loop and post-body versions
/// 4. **把循环体内对该变量的引用重命名到 phi dest**, 并给 phi 记上真正的 preheader。
///
/// 第 4 步与「从整个循环体而不是只看 header 收集使用」是本函数的核心修正: 旧实现
/// 只看 header 块里的指令, 而 `while` 的 header 常常只有 cond (`n > 0`), 循环携带变量
/// `s` 的读发生在体内那条 `s += n` 上 ⇒ 根本不生成 phi, IR 不在真正的 SSA 形式上;
/// 即使生成了 phi, 体内引用仍指向进循环前的版本, 于是「每轮读上一轮的值」这件事
/// 在 IR 里不存在。两者叠加使重建结果只在「基名塌缩」巧合下才对, 任何常量传播或
/// 值编号一碰就错。
pub fn insert_loop_phis(func: &mut Function) {
    // Compute dominator tree for correct back-edge detection.
    // A back edge is an edge A→B where B dominates A.
    let doms = compute_dominators(func);
    let mut back_edges: Vec<(String, String)> = Vec::new(); // (latch, header)
                                                            // 必须用 `ordered_block_names` 而非 `&func.blocks`: back_edges 的处理顺序
                                                            // 决定同一 header 上 phi 的排列序, 而 phi 序会经 rebuild 影响产物字节。
    for bb_name in ordered_block_names(func) {
        let Some(bb) = func.blocks.get(&bb_name) else {
            continue;
        };
        let successors = match &bb.term {
            Terminator::Jump(t) => vec![t.clone()],
            Terminator::Branch { t, f, .. } => vec![t.clone(), f.clone()],
            Terminator::SwitchTable {
                targets, default, ..
            } => {
                let mut s = targets.clone();
                if !s.contains(default) {
                    s.push(default.clone());
                }
                s
            },
            _ => vec![],
        };
        let from_doms = match doms.get(&bb_name) {
            Some(d) => d,
            None => continue,
        };
        for succ in successors {
            // Back edge: target dominates source
            if from_doms.contains(&succ) {
                back_edges.push((bb_name.clone(), succ));
            }
        }
    }
    if back_edges.is_empty() {
        return;
    }
    // For each back edge (latch → header), find variables defined in the
    // loop body and insert phi nodes at the header.
    let all_preds = collect_all_preds(func);
    for (latch, header) in back_edges.clone() {
        // Collect loop body blocks: all blocks that can reach the latch
        // without going through the header (simplified: blocks dominated
        // by the header that can reach the latch).
        let loop_bbs = find_loop_body(func, &header, &latch);
        let in_body: HashSet<String> = loop_bbs.iter().cloned().collect();
        // 体内基名 → 体内最后一个定义 (回边带回来的值); 体内外的定义一起扫,
        // 体外取「循环前最后一次定义」= preheader 可用的那个版本。
        let mut body_defs: HashMap<String, SsaVar> = HashMap::new();
        let mut outside_defs: HashMap<String, SsaVar> = HashMap::new();
        let mut max_version: HashMap<String, u32> = HashMap::new();
        for p in &func.params {
            max_version
                .entry(p.base.clone())
                .and_modify(|v| {
                    if p.version > *v {
                        *v = p.version;
                    }
                })
                .or_insert(p.version);
            outside_defs.entry(p.base.clone()).or_insert_with(|| p.clone());
        }
        for bb_name in ordered_block_names(func) {
            let Some(bb) = func.blocks.get(&bb_name) else {
                continue;
            };
            let inside = in_body.contains(&bb_name);
            for inst in &bb.insts {
                if let Some(d) = &inst.dest {
                    bump_version(&mut max_version, &d.base, d.version);
                    if inside {
                        body_defs.insert(d.base.clone(), d.clone());
                    } else {
                        outside_defs.insert(d.base.clone(), d.clone());
                    }
                }
            }
            for phi in &bb.phis {
                bump_version(&mut max_version, &phi.dest.base, phi.dest.version);
                if inside {
                    body_defs.insert(phi.dest.base.clone(), phi.dest.clone());
                }
            }
        }
        // header 已有的 phi (多回边/内层循环重复处理时) 不重复创建。
        let existing_phi: HashSet<String> = func
            .blocks
            .get(&header)
            .map(|b| b.phis.iter().map(|p| p.dest.base.clone()).collect())
            .unwrap_or_default();
        // 真正的 preheader: header 的前驱中不在循环体内者。旧实现写魔法串
        // `"__entry"` (全仓无消费者), phi 的「从哪条边进来」这个信息因此丢失。
        let preheader = all_preds
            .get(&header)
            .map(|ps| {
                let mut outside: Vec<&String> =
                    ps.iter().filter(|p| !in_body.contains(p.as_str())).collect();
                outside.sort();
                outside.first().map(|p| (*p).clone())
            })
            .flatten();
        let mut new_phis: Vec<Phi> = Vec::new();
        let mut renames: HashMap<String, SsaVar> = HashMap::new();
        // body_defs 是 HashMap, 直接迭代会把随机序写进 bb.phis; 按名字排序保证可复现。
        let mut body_def_names: Vec<&String> = body_defs.keys().collect();
        body_def_names.sort();
        for base in body_def_names {
            if existing_phi.contains(base.as_str()) {
                continue;
            }
            let body_var = &body_defs[base];
            let Some(entry_var) = outside_defs.get(base.as_str()) else {
                continue;
            };
            if entry_var.name() == body_var.name() {
                continue;
            }
            let phi_dest = SsaVar {
                base: base.clone(),
                version: max_version.get(base.as_str()).copied().unwrap_or(0) + 1,
                ty: body_var.ty.clone(),
                span: body_var.span.clone(),
            };
            new_phis.push(Phi {
                dest: phi_dest.clone(),
                incoming: vec![
                    (
                        entry_var.clone(),
                        preheader.clone().unwrap_or_else(|| "__entry".into()),
                    ),
                    (body_var.clone(), latch.clone()),
                ],
            });
            // 关键一步: 循环体内所有对该变量的读改写都指向 phi dest。不做这步,
            // 「每轮读上一轮的值」在 IR 里根本不存在, 任何常量传播/值编号都会
            // 把循环携带变量在进循环前的初值传进体里。
            renames.insert(entry_var.name(), phi_dest);
        }
        if let Some(bb) = func.blocks.get_mut(&header) {
            bb.phis.extend(new_phis);
        }
        rename_uses_in(func, &in_body, &renames);
    }
}

fn bump_version(map: &mut HashMap<String, u32>, base: &str, v: u32) {
    map.entry(base.to_string())
        .and_modify(|cur| {
            if v > *cur {
                *cur = v;
            }
        })
        .or_insert(v);
}

/// P0-4 Phase 2: 在 label BB 入口插入 phi 节点.
///
/// 当 goto 跳转到 label 时，变量版本可能与 label 正常入口不同.
/// 为每个被 goto 跨越的变量插入 phi 节点合并不同版本.
fn insert_label_phis(
    func: &mut Function,
    label_bbs: &HashMap<String, String>,
    _label_var_snapshots: &HashMap<String, HashMap<String, SsaVar>>,
) {
    if label_bbs.is_empty() {
        return;
    }
    // 对每个 label BB，检查是否有多个前驱且前驱的变量版本不同
    // 先把待处理 BB 按名字排序: 不同 label 彼此独立, 但显式定序可防未来改成
    // 全局副作用型逻辑时重新引入 HashMap 迭代序泄漏。
    let mut label_bbs_sorted: Vec<&String> = label_bbs.values().collect();
    label_bbs_sorted.sort();
    for bb_name in label_bbs_sorted {
        let bb = match func.blocks.get(bb_name) {
            Some(b) => b,
            None => continue,
        };
        if bb.preds.len() <= 1 {
            continue; // 只有一个前驱，不需要 phi
        }
        // 收集每个前驱 BB 最后定义的变量版本
        let mut pred_vars: Vec<(String, HashMap<String, SsaVar>)> = Vec::new();
        for pred_name in &bb.preds {
            let pred_bb = match func.blocks.get(pred_name) {
                Some(b) => b,
                None => continue,
            };
            let mut vars: HashMap<String, SsaVar> = HashMap::new();
            for inst in &pred_bb.insts {
                if let Some(d) = &inst.dest {
                    vars.insert(d.base.clone(), d.clone());
                }
            }
            pred_vars.push((pred_name.clone(), vars));
        }
        if pred_vars.len() < 2 {
            continue;
        }
        // 找出在所有前驱中都有定义但版本不同的变量
        let first_vars = &pred_vars[0].1;
        let mut needs_phi: Vec<String> = Vec::new();
        // first_vars 是 HashMap, 必须先排序再收集, 否则 needs_phi 的序 (→ phi 在
        // BB 内的排列序) 会随进程随机种子变化。
        let mut var_names: Vec<&String> = first_vars.keys().collect();
        var_names.sort();
        for base in var_names {
            let var = &first_vars[base];
            let mut all_same = true;
            for (_, other_vars) in &pred_vars[1..] {
                if let Some(other_var) = other_vars.get(base) {
                    if other_var.name() != var.name() {
                        all_same = false;
                        break;
                    }
                }
            }
            if !all_same {
                needs_phi.push(base.clone());
            }
        }
        // 为需要 phi 的变量插入 phi 节点
        if !needs_phi.is_empty() {
            let bb = match func.blocks.get_mut(bb_name) {
                Some(b) => b,
                None => continue,
            };
            for base in &needs_phi {
                // 找到最高版本号 + 1 作为 phi 目标版本
                let max_version = pred_vars
                    .iter()
                    .filter_map(|(_, vars)| vars.get(base))
                    .map(|v| v.version)
                    .max()
                    .unwrap_or(0);
                let ty = pred_vars
                    .iter()
                    .filter_map(|(_, vars)| vars.get(base))
                    .next()
                    .map(|v| v.ty.clone())
                    .unwrap_or(Type::Int);
                let span = pred_vars
                    .iter()
                    .filter_map(|(_, vars)| vars.get(base))
                    .next()
                    .map(|v| v.span.clone())
                    .unwrap_or_else(|| func.span.clone());
                let phi_dest = SsaVar {
                    base: base.clone(),
                    version: max_version + 1,
                    ty,
                    span,
                };
                let incoming: Vec<(SsaVar, String)> = pred_vars
                    .iter()
                    .filter_map(|(pred_name, vars)| {
                        vars.get(base).map(|v| (v.clone(), pred_name.clone()))
                    })
                    .collect();
                if incoming.len() >= 2 {
                    bb.phis.push(Phi {
                        dest: phi_dest,
                        incoming,
                    });
                }
            }
        }
    }
}

/// Find the body blocks of a natural loop (header, latch).
/// Loop body = {header} ∪ {blocks that can reach latch without going through header}
///
/// 已 pub：「在环内」与「受头支配」不是同一个关系 —— 循环出口也受头支配
/// （实测 `while.end` 的支配集含 `while.cond`），拿支配关系当环内判据会把出口
/// 误当成环内后继，然后把整个 `while` 形态错拒。P16 的循环识别必须用这里的结果；
/// 另写一份自然循环收集只会与这里漂移。
///
/// 返回值按名字排序: 调用方会用它决定 phi 插入序, 直接 `HashSet::into_iter()`
/// 会把进程随机哈希序泄到下游。
pub fn find_loop_body(func: &Function, header: &str, latch: &str) -> Vec<String> {
    find_loop_body_multi(func, header, std::slice::from_ref(&latch.to_string()))
}

/// 多 latch 版本的自然循环体收集。
///
/// 多个 latch (如 `if (c) continue;` 的 latch 与尾部回边共存) 必须全部参与
/// 反向 DFS: 只从单个 latch 出发会漏掉其它 latch 所在的分支区域, SSA→AST
/// 重建时那部分会被误判为“循环外后继”而发成 `break` (continue 语义丢失,
/// 循环提前终止)。
fn find_loop_body_multi(func: &Function, header: &str, latches: &[String]) -> Vec<String> {
    let mut body: HashSet<String> = HashSet::new();
    body.insert(header.to_string());
    // Reverse DFS from every latch, not going through header
    let mut stack: Vec<String> = latches
        .iter()
        .filter(|l| l.as_str() != header)
        .cloned()
        .collect();
    while let Some(bb) = stack.pop() {
        if !body.insert(bb.clone()) {
            continue;
        }
        if let Some(bb_data) = func.blocks.get(&bb) {
            for pred in &bb_data.preds {
                if pred != header && !body.contains(pred) {
                    stack.push(pred.clone());
                }
            }
        }
    }
    let mut v: Vec<String> = body.into_iter().collect();
    v.sort();
    v
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

// ============================================================
// Switch -> If-chain 降级 pass
// ============================================================

/// 将函数体中的 switch 语句转换为 if-else 链.
/// 密集 case 值的 switch 保留原样, 由 SSA Builder 生成 SwitchTable 终结符.
/// 这样含 switch 的函数也能进入 SSA 化路径.
pub fn lower_switches_in_func(f: &mut FuncDecl) -> bool {
    if let Some(body) = &mut f.body {
        lower_switches_in_block(body)
    } else {
        false
    }
}

/// 递归扫描 block，将所有 switch 语句替换为 if-else 链.
fn lower_switches_in_block(block: &mut Block) -> bool {
    let mut modified = false;
    let mut new_items = Vec::with_capacity(block.items.len());
    for mut item in block.items.drain(..) {
        match &mut item {
            BlockItem::Stmt(s) => {
                if let Some(new_stmt) = lower_switch_stmt(s) {
                    *s = new_stmt;
                    modified = true;
                }
                // 递归处理嵌套 block
                lower_switches_in_stmt(s);
            },
            BlockItem::Decl(d) => {
                if let Decl::Func(f) = d {
                    if let Some(body) = &mut f.body {
                        if lower_switches_in_block(body) {
                            modified = true;
                        }
                    }
                }
            },
        }
        new_items.push(item);
    }
    block.items = new_items;
    modified
}

/// 递归处理语句中的嵌套 block.
fn lower_switches_in_stmt(s: &mut Stmt) {
    match s {
        Stmt::If { then_, else_, .. } => {
            lower_switches_in_stmt(then_);
            if let Some(e) = else_ {
                lower_switches_in_stmt(e);
            }
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            lower_switches_in_stmt(body);
        },
        Stmt::For { body, .. } => {
            lower_switches_in_stmt(body);
        },
        Stmt::Block { b, .. } => {
            lower_switches_in_block(b);
        },
        // 嵌套 switch: 外层降级完成后留下的内层 switch 同样要处理，否则一个
        // 「switch 嵌在 case 体里」的函数会整块失去 SSA 化 (`is_structured` 见
        // Switch 即拒)，而它的内层 switch 本身是完全可以忠实降级的。
        Stmt::Switch { cases, .. } => {
            for c in cases.iter_mut() {
                lower_switches_in_block(&mut c.body);
            }
        },
        Stmt::Label { stmt, .. } => {
            lower_switches_in_stmt(stmt);
        },
        _ => {},
    }
}

/// 一个 if-chain 分支: 「命中这些 case 值 (或 default)」 + 「分支体」。
struct SwitchArm {
    /// `None` 含义上的默认分支由 `is_default` 表达; 有 `is_default` 时 `vals` 无用。
    vals: Vec<Expr>,
    is_default: bool,
    body: Vec<BlockItem>,
    span: Span,
}

/// 将单个 switch 语句转换为 if-else 链。
///
/// 返回 `None` = 本 switch 不能用 if-chain 忠实表达 ⇒ **放弃降级**，函数带着
/// `Stmt::Switch` 走 AST 路径 (`is_structured` 对 Switch 返回 false，因此不进 SSA；
/// 这比降级错语义好)。放弃的形态只有两类：真正的 fallthrough (含 `[[fallthrough]]`
/// / GNU `case lo ... hi:` 区间)，以及绑定到本 switch 的 `continue`。
///
/// 为什么必须判这两类、又为什么必须包一层 `do { … } while (0)` —— 旧实现直接
/// 把 `case.body` 拼成 `if (e == v) { body } else …`，有两处静默误编译：
///
///   1. **break 改绑**：`break` 原本跳出 switch，摊成 if-chain 后它跳出的是
///      **外层循环**。实测 `for(i<3){switch(i){case 0:break;case 1:s+=1;break;
///      default:s+=2;break;}}` 在 -O2 返回 0 而不是 3：第一圈 `i==0` 的 `break`
///      直接把循环于掉了。这条只在「循环里含 switch」时才发作，而循环此前被
///      `func_has_loop` 闸门挡在 SSA 外，所以长期没被观测到。
///      ⇒ 现在把整条链包进 `do { … } while (0)`：`break` 仍然「跳出整个 switch」。
///   2. **fallthrough 丢失**：C 的 `case 1: x; case 2: y;` 会连着执行，if-chain
///      只会执行 `x`。空体标签 (`case 1: case 2: …`) 是合法的「共享分支」，按值
///      合并即可；非空体且没自带退出语义的就是真 fallthrough ⇒ 放弃降级。
fn lower_switch_stmt(s: &Stmt) -> Option<Stmt> {
    let Stmt::Switch { expr, cases, span: _ } = s else {
        return None;
    };
    if cases.is_empty() {
        return None;
    }
    // GNU 区间 case 无法用 `==` 表达。
    if cases.iter().any(|c| c.range_end.is_some()) {
        return None;
    }
    // 1. 按源码顺序合并「空体标签」: `case 1: case 2: body` ⇒ 一个分支两个值。
    let mut arms: Vec<SwitchArm> = Vec::new();
    let mut pending_vals: Vec<Expr> = Vec::new();
    let mut pending_default = false;
    for c in cases {
        match &c.val {
            Some(v) => pending_vals.push(v.clone()),
            None => pending_default = true,
        }
        if c.body.items.is_empty() {
            continue;
        }
        arms.push(SwitchArm {
            vals: std::mem::take(&mut pending_vals),
            is_default: std::mem::replace(&mut pending_default, false),
            body: c.body.items.clone(),
            span: c.span.clone(),
        });
    }
    if arms.is_empty() {
        // 整个 switch 只有空体标签 ⇒ 什不发生 (default 不存在时就是无操作)。
        return Some(Stmt::Block {
            b: Block {
                items: Vec::new(),
                span: cases[0].span.clone(),
            },
            span: cases[0].span.clone(),
        });
    }
    // 2. fallthrough / switch-bound continue 判定。
    for (i, arm) in arms.iter().enumerate() {
        if has_switch_bound_continue(&arm.body) {
            return None;
        }
        let is_last_chain_arm = i + 1 == arms.len();
        if !is_last_chain_arm && !arm_ends_with_exit(&arm.body) {
            return None;
        }
        // 没有 default 时，最后一个分支“落空”后还会继续 switch 后面的代码 —— 与
        // if-chain 末尾无 else 同构，无需额外判定；但有 default 时 default 必须是
        // 链的最后一环，否则它前面的分支「落空」会跑到错误的目标。
        if arm.is_default && !is_last_chain_arm {
            return None;
        }
    }
    let need_guard = arms.iter().any(|a| has_switch_bound_break(&a.body));
    // 3. 从后往前拼 if-else 链。
    let mut current_else: Option<Box<Stmt>> = None;
    for arm in arms.iter().rev() {
        let body = Stmt::Block {
            b: Block {
                items: arm.body.clone(),
                span: arm.span.clone(),
            },
            span: arm.span.clone(),
        };
        current_else = Some(Box::new(if arm.is_default || arm.vals.is_empty() {
            body
        } else {
            let mut cond = Expr::Binop {
                op: BinOp::EqEq,
                l: Box::new(expr.clone()),
                r: Box::new(arm.vals[0].clone()),
                span: arm.span.clone(),
                ty: expr_ty(expr),
            };
            for v in &arm.vals[1..] {
                cond = Expr::Binop {
                    op: BinOp::Or,
                    l: Box::new(cond),
                    r: Box::new(Expr::Binop {
                        op: BinOp::EqEq,
                        l: Box::new(expr.clone()),
                        r: Box::new(v.clone()),
                        span: arm.span.clone(),
                        ty: expr_ty(expr),
                    }),
                    span: arm.span.clone(),
                    ty: Type::Int,
                };
            }
            Stmt::If {
                cond,
                then_: Box::new(body),
                else_: current_else.take(),
                span: arm.span.clone(),
            }
        }));
    }
    let chain = current_else.map(|s| *s)?;
    if !need_guard {
        return Some(chain);
    }
    // 4. `do { 链 } while (0)`: 把绑定到 switch 的 `break` 留在「跳出整个 switch」
    //    这个位置上。否则 `break` 在 if-chain 里会绑到外层循环 (上面根因 1)。
    let zero = Expr::Int {
        v: 0,
        span: cases[0].span.clone(),
        ty: Type::Int,
    };
    Some(Stmt::Do {
        body: Box::new(Stmt::Block {
            b: Block {
                items: vec![BlockItem::Stmt(chain)],
                span: cases[0].span.clone(),
            },
            span: cases[0].span.clone(),
        }),
        cond: zero,
        span: cases[0].span.clone(),
    })
}

/// 分支体是否 自带「离开 switch」的语义 (fallthrough 判定)。
fn arm_ends_with_exit(items: &[BlockItem]) -> bool {
    let Some(last) = items.last() else {
        return false;
    };
    matches!(
        last,
        BlockItem::Stmt(
            Stmt::Break { .. }
                | Stmt::Return { .. }
                | Stmt::Goto { .. }
                | Stmt::GotoIndirect { .. }
                | Stmt::AsmGoto { .. }
        )
    )
}

/// 体内是否存在「绑定到本 switch」的 `break` (更内层的循环/switch 会屏蔽它)。
fn has_switch_bound_break(items: &[BlockItem]) -> bool {
    items.iter().any(|it| match it {
        BlockItem::Stmt(Stmt::Break { .. }) => true,
        BlockItem::Stmt(s) => bound_flow(s, false),
        BlockItem::Decl(_) => false,
        _ => true,
    })
}

fn has_switch_bound_continue(items: &[BlockItem]) -> bool {
    items.iter().any(|it| match it {
        BlockItem::Stmt(s) => bound_flow(s, true),
        _ => false,
    })
}

/// `want_continue` = 找 `continue` (屏蔽层同样不计算在内); 否则找 `break`。
///
/// 更内层的 `for`/`while`/`do` 体会自已的 `break`/`continue`，`switch` 体会自已
/// `break` (但 `continue` 仍穿透它绑定到更外层循环)，所以屏蔽位必须分开。
fn bound_flow(s: &Stmt, want_continue: bool) -> bool {
    let stmt = |st: &Stmt| bound_flow(st, want_continue);
    match s {
        Stmt::Break { .. } => !want_continue,
        Stmt::Continue { .. } => want_continue,
        Stmt::Block { b, .. } => b.items.iter().any(|i| match i {
            BlockItem::Stmt(st) => stmt(st),
            _ => false,
        }),
        Stmt::If {
            cond, then_, else_, ..
        } => {
            expr_has_loop_control(cond, want_continue)
                || stmt(then_)
                || else_.as_ref().map_or(false, |e| stmt(e))
        },
        Stmt::For { init, .. } => {
            // 内层循环屏蔽 break; continue 在 for-init (声明) 里不可能出现。
            if want_continue {
                init.as_ref().map_or(false, |i| stmt(i))
            } else {
                false
            }
        },
        Stmt::While { body, .. } | Stmt::Do { body, .. } => {
            if want_continue {
                false
            } else {
                bound_flow(body, false)
            }
        },
        Stmt::Switch { cases, expr, .. } => {
            expr_has_loop_control(expr, want_continue)
                || cases.iter().any(|c| {
                    c.body.items.iter().any(|i| match i {
                        BlockItem::Stmt(st) => bound_flow(st, want_continue),
                        _ => false,
                    })
                })
        },
        _ => false,
    }
}

/// 表达式里的 `break`/`continue` 只能来自语句表达式 `({...})` —— 保守报「有」。
fn expr_has_loop_control(e: &Expr, _want_continue: bool) -> bool {
    match e {
        Expr::StmtExpr { .. } => true,
        _ => false,
    }
}

/// 判断 switch 的 case 值是否密集 (适合跳转表优化).
/// 密集条件: max - min < 4 * count
fn is_dense_switch(cases: &[Case]) -> bool {
    let mut vals: Vec<i128> = Vec::new();
    for c in cases {
        if let Some(Expr::Int { v, .. }) = &c.val {
            vals.push(*v);
        }
    }
    if vals.len() < 2 {
        return false;
    }
    let min_val = *vals.iter().min().unwrap();
    let max_val = *vals.iter().max().unwrap();
    (max_val - min_val) < 4 * vals.len() as i128
}

/// 获取表达式的类型 (简化版).
fn expr_ty(e: &Expr) -> Type {
    match e {
        Expr::Int { ty, .. } => ty.clone(),
        Expr::Float { ty, .. } => ty.clone(),
        Expr::String { ty, .. } => ty.clone(),
        Expr::Char { ty, .. } => ty.clone(),
        Expr::Ident { ty, .. } => ty.clone(),
        Expr::Binop { ty, .. } => ty.clone(),
        Expr::Unary { ty, .. } => ty.clone(),
        Expr::Call { ty, .. } => ty.clone(),
        Expr::Assign { ty, .. } => ty.clone(),
        Expr::Cond { ty, .. } => ty.clone(),
        Expr::Cast { ty, .. } => ty.clone(),
        Expr::SizeofTy { ty, .. } => ty.clone(),
        Expr::SizeofExpr { .. } => Type::ULong,
        Expr::Index { ty, .. } => ty.clone(),
        Expr::Member { ty, .. } => ty.clone(),
        Expr::AddrOf { ty, .. } => ty.clone(),
        Expr::Deref { ty, .. } => ty.clone(),
        Expr::CompoundLit { ty, .. } => ty.clone(),
        Expr::InitList { ty, .. } => ty.clone(),
        Expr::StmtExpr { ty, .. } => ty.clone(),
        Expr::Comma { ty, .. } => ty.clone(),
        Expr::ComplexLit { ty, .. } | Expr::ComplexIntLit { ty, .. } => ty.clone(),
        Expr::Real { ty, .. } => ty.clone(),
        Expr::Imag { ty, .. } => ty.clone(),
        Expr::VaArg { ty, .. } => ty.clone(),
        Expr::Offsetof { result_ty, .. } => result_ty.clone(),
        Expr::AlignofTy { .. } => Type::ULong,
        Expr::AlignofExpr { .. } => Type::ULong,
        Expr::PostInc { ty, .. }
        | Expr::PostDec { ty, .. }
        | Expr::PreInc { ty, .. }
        | Expr::PreDec { ty, .. } => ty.clone(),
        Expr::AsmExpr { ty, .. } => ty.clone(),
        Expr::LabelAddr { ty, .. } => ty.clone(),
        _ => Type::Int,
    }
}

/// 构建一个函数的 SSA IR。
///
/// 为什么要跑两遍: 「本函数哪些对象被取址」是一条**全函数**别名事实, 而 C 允许
/// 在读写之前很远的地方才出现 `&x` (甚至在读之后), 单遇构建无法预知。第一遍只为了
/// 收集事实: 直接读它发射的 `InstKind::AddrOf { base }` —— 与第二遍走的是同一份
/// lowering, 因此不可能漏掉某种 AST 形态 (手写 AST 扫描则必须对 `Expr` 穷举)。
/// 没出现任何取址 (绝大多数函数) 时直接用第一遍结果, 不付出额外构建代价。
pub fn build_from_func(f: &FuncDecl) -> Option<Function> {
    let probe = build_ssa(f, HashSet::new())?;
    let addr_taken: HashSet<String> = probe
        .blocks
        .values()
        .flat_map(|bb| bb.insts.iter())
        .filter_map(|inst| match &inst.kind {
            InstKind::AddrOf { base } => Some(base.clone()),
            _ => None,
        })
        .collect();
    if addr_taken.is_empty() {
        return Some(probe);
    }
    build_ssa(f, addr_taken)
}

/// 给「没能降级的表达式」一个可读的形态名（只用于诊断文本，不参与判据）。
/// 列出的就是 catch-all 实际会碰到的变体；其余一律报「未列举形态」。
fn expr_shape(e: &Expr) -> &'static str {
    match e {
        Expr::ComplexLit { .. } | Expr::ComplexIntLit { .. } => "_Complex 字面量",
        Expr::Real { .. } | Expr::Imag { .. } => "__real__/__imag__",
        Expr::VaArg { .. } => "va_arg",
        Expr::CompoundLit { .. } => "复合字面量",
        Expr::InitList { .. } => "初始化列表",
        Expr::StmtExpr { .. } => "语句表达式 ({...})",
        Expr::LabelAddr { .. } => "label 地址",
        _ => "未列举形态",
    }
}

fn build_ssa(f: &FuncDecl, addr_taken: HashSet<String>) -> Option<Function> {
    let body = f.body.as_ref()?;
    if !is_structured(body) {
        return None;
    }
    let mut func = Function::new(f.name.name.clone(), f.ret_ty.clone(), f.span.clone());
    func.is_cold = f.is_cold;
    func.is_hot = f.is_hot;
    func.is_noreturn = f.is_noreturn;
    // Collect original local variable names, types, and qualifiers for rebuild_block
    collect_local_decls(
        &body.items,
        &mut func.original_locals,
        &mut func.local_types,
        &mut func.local_quals,
    );
    let mut builder = Builder::new(func, addr_taken);
    for p in &f.params {
        if let Some(n) = &p.name {
            let v = SsaVar {
                base: n.name.clone(),
                version: 0,
                ty: p.ty.clone(),
                span: p.span.clone(),
            };
            builder.func.params.push(v.clone());
            // Store parameter qualifiers for alias analysis (restrict)
            builder.func.param_quals.insert(n.name.clone(), p.quals);
            builder.define(v);
        }
    }
    builder.lower_block(body);
    // 有任何表达式没能忠实降级 ⇒ 放弃整个函数的 SSA 化，该函数留在 AST 通道上。
    // 诊断窗口：`DCC_SSA_BAIL=1` 打印函数名与原因（丢优化与拿到错值在产物上很像，
    // 没有原因就只能猜，同 `DCC_PIPE_DUMP` 的理由）。
    if let Some(why) = builder.bail.take() {
        if std::env::var("DCC_SSA_BAIL").is_ok() {
            eprintln!("[SSA] {} 不进管线：{why}", f.name.name);
        }
        return None;
    }
    // 若最终 BB 没有 terminator, 加 Ret void
    let cur = builder.cur.clone();
    if let Some(b) = builder.func.blocks.get_mut(&cur) {
        if matches!(b.term, Terminator::Unreachable) {
            b.term = Terminator::Ret { val: None };
        }
    }
    let mut func = builder.func;
    // Insert phi nodes at loop headers for variables modified in loop bodies.
    // This enables SSA optimization passes to correctly analyze loop-carried
    // dependencies (induction variables, strength reduction, etc.).
    insert_loop_phis(&mut func);
    // P0-4 Phase 2: Insert phi nodes at label BBs for goto targets
    insert_label_phis(&mut func, &builder.label_bbs, &builder.label_var_snapshots);
    // 上面的 phi 插入与构建期的循环旋转都改过 terminator, 此处统一刷新 CFG 缓存:
    // `find_loop_body_multi` 等分析沿 `preds` 反向走, 看到旧边就会把循环体算错。
    refresh_cfg(&mut func);
    Some(func)
}

// ============================================================
// SSA -> AST 重建器
// ============================================================

/// 返回确定性的 BB 遍历序: 先按 `block_order` (创建序), 再补上 `blocks`
/// 里未出现在 `block_order` 的 key (按名字排序)。
///
/// 为什么需要: `blocks: HashMap<String, BasicBlock>` 的 `values()`/`keys()`
/// 迭代序由进程随机种子决定。任何“递增编号”或“首个胜出”型逻辑（如 alias set id、
/// 变量类型/声明序）一旦建立在它之上, 就会泄漏到产物字节里。同时保留
/// `block_order` 未能覆盖的块, 以免新增块静默丢失。
pub fn ordered_block_names(func: &Function) -> Vec<String> {
    let mut emitted: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::with_capacity(func.blocks.len());
    for n in &func.block_order {
        if func.blocks.contains_key(n) && emitted.insert(n.clone()) {
            out.push(n.clone());
        }
    }
    let mut rest: Vec<&String> = func
        .blocks
        .keys()
        .filter(|k| !emitted.contains(k.as_str()))
        .collect();
    rest.sort();
    out.extend(rest.into_iter().cloned());
    out
}

/// 一条指令的**全部操作数** (含 Store 的 src、Call 的参数), 按 IR 结构穷举。
fn kind_operands(kind: &InstKind) -> Vec<&Operand> {
    match kind {
        InstKind::Bin { l, r, .. } => vec![l, r],
        InstKind::Un { e, .. } | InstKind::Cast { e, .. } | InstKind::Load { e, .. } => vec![e],
        InstKind::Store { e, src, .. } | InstKind::MemberStore { obj: e, src, .. } => vec![e, src],
        InstKind::IndexLoad { arr, idx, .. } => vec![arr, idx],
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

/// 该指令是否**读内存** (解引用读 / 读全局对象内容)。内联它等于把这次读**推迟**
/// 到使用点重放, 因此使用点之前若有任何写内存/调用, 语义就会漂移。
fn inst_reads_memory(kind: &InstKind) -> bool {
    match kind {
        InstKind::Load { .. } | InstKind::IndexLoad { .. } | InstKind::MemberLoad { .. } => true,
        _ => kind_operands(kind)
            .iter()
            .any(|o| matches!(o, Operand::SymAddr(..))),
    }
}

/// 该指令是否**不能被整条丢弃**: 带副作用, 或是对 volatile 对象的访问
/// (volatile 读本身就是可观测行为, 与结果有没有人用无关)。
fn inst_unremovable(kind: &InstKind) -> bool {
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
fn inst_clobbers_memory(kind: &InstKind) -> bool {
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
fn inst_written_bases(
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

/// `name` 在指令/terminator 中的**首个**使用点: (块名, 序号), 序号 == insts.len()
/// 表示它出现在该块的 terminator 上。
fn find_use_site(func: &Function, name: &str) -> Option<(String, usize)> {
    for bb_name in &func.block_order {
        let Some(bb) = func.blocks.get(bb_name) else {
            continue;
        };
        for (i, inst) in bb.insts.iter().enumerate() {
            if op_count_in_kind(&inst.kind, name) > 0 {
                return Some((bb_name.clone(), i));
            }
        }
        let in_term = match &bb.term {
            Terminator::Ret { val: Some(o) } => op_count_in_op(o, name) > 0,
            Terminator::Branch { cond, .. } => op_count_in_op(cond, name) > 0,
            Terminator::SwitchTable { idx, .. } => op_count_in_op(idx, name) > 0,
            _ => false,
        };
        if in_term {
            return Some((bb_name.clone(), bb.insts.len()));
        }
    }
    None
}

/// 把一个临时值内联到使用点后, 该表达式**实际**读取的基名与内存/副作用需求。
struct ExpandedTemp {
    read_bases: HashSet<String>,
    reads_mem: bool,
    has_side_effect: bool,
    /// 展开链上最早一环在本块内的序号 (安全窗口的左边界)
    min_idx: usize,
}

/// 判定「把 `inst` 的结果内联到它唯一的使用点」是否保持语义。
///
/// 返回 `None` = 不安全 (调用方必须保留为独立赋值语句)。判定窗口是块内的
/// `[min_idx, use_pos)`:
///   * 窗口内不得写入表达式读到的任何**基名** —— 基名塌缩后这就是重算时机错;
///   * 表达式若读内存或自带副作用, 窗口内不得有任何写内存/调用/volatile 访问;
///   * 使用点必须与定义点同块 —— 跨块时中间可能经过任意循环回边/分支,
///     无法在「一条直线窗口」里证明稳定, 保守拒绝;
///   * 链式内联的操作数也必须同块且在窗口左界之前。
fn inline_window_safe(
    func: &Function,
    ssa_name: &str,
    def_bb: &str,
    def_idx: usize,
    inst: &Inst,
    def_pos: &HashMap<String, (String, usize)>,
    addr_base: &HashMap<String, String>,
    subst: &HashMap<String, Option<Expr>>,
    expanded: &HashMap<String, ExpandedTemp>,
) -> Option<ExpandedTemp> {
    let mut info = ExpandedTemp {
        read_bases: HashSet::new(),
        reads_mem: inst_reads_memory(&inst.kind),
        has_side_effect: inst_has_side_effect(&inst.kind),
        min_idx: def_idx,
    };
    for uvar in kind_operand_ssa_vars(&inst.kind) {
        let uname = uvar.name();
        if !matches!(subst.get(&uname), Some(Some(_))) {
            // 操作数保留为真实变量 (具名局部 / 未内联的临时值): 它按基名参与检查。
            info.read_bases.insert(uvar.base.clone());
            continue;
        }
        let (blk, idx) = def_pos.get(&uname)?.clone();
        if blk != def_bb {
            return None;
        }
        let inner = expanded.get(&uname)?;
        info.read_bases.extend(inner.read_bases.iter().cloned());
        info.reads_mem |= inner.reads_mem;
        info.has_side_effect |= inner.has_side_effect;
        info.min_idx = info.min_idx.min(inner.min_idx).min(idx);
    }
    let (u_bb, u_pos) = match find_use_site(func, ssa_name) {
        // 没有指令级消费者: 结果要么无人读 (丢弃安全), 要么只被 phi 经 `env`
        // 间接消费 (phi 的展开点在各自前驱路径上, 由 phi 处理逻辑负责定序)。
        Some(p) => p,
        None => return Some(info),
    };
    if u_bb != def_bb || u_pos <= def_idx {
        return None;
    }
    let bb = func.blocks.get(def_bb)?;
    for other in bb.insts.iter().take(u_pos).skip(info.min_idx + 1) {
        if (info.reads_mem || info.has_side_effect) && inst_clobbers_memory(&other.kind) {
            return None;
        }
        if inst_written_bases(&other.kind, other.dest.as_ref(), addr_base)
            .iter()
            .any(|b| info.read_bases.contains(b))
        {
            return None;
        }
    }
    Some(info)
}

/// 「合成临时值」的判据: **先排除用户可见存储**, 再认已知的合成命名方案。
///
/// 两个方向都不能只用名字前缀:
///   * `_s` / `__d` / `_tbin` 是 C 合法的用户标识符。把它们当合成值会在重建时
///     内联掉它们的赋值、丢掉循环推进 —— 实测 `int _s=0; for(i<n) _s+=i;` 从 -O2
///     起算错。它们在 `original_locals ∪ params` 里, 所以上面那一律优先。
///   * 反向也不行: 本仓 pass 生成的临时名不必以 `_` 开头 (`dcc_t<序号>_xxx`)。
///   * 还有一类既不在 `original_locals` 也不属于合成命名方案的名字 —— C++ 后端的
///     短路求值结果变量 `dcc_land1` / `dcc_lor2` 就是这样的**具名局部** (它们不靠
///     `Decl::Var` 节点存在, 而靠重建按定义序合成声明), 把它们当合成值会让
///     `a ? b : c` 的结果丢失 (实测 cpp 夹具 `ternary=` 第二项变成 0.0000)。
///
/// 所以「是不是合成值」只能问「它匹配哪一套已登记的合成命名方案」, 并且用户可见
/// 声明集永远优先。`SsaVar::name()` 的分符改成 `.` 后, 版本名不会再与合成 base 撞名
/// (旧写法下 `int _tbin` 的第 2 版 = `_tbin_2` = `fresh("bin")` 的 base)。
pub fn is_synthetic_temp_base(func: &Function, base: &str) -> bool {
    if func.original_locals.contains(base) {
        return false;
    }
    if func.params.iter().any(|p| p.base == base) {
        return false;
    }
    base.starts_with('_') || is_lower_ir_temp_base(base)
}

/// SSA→AST 重建的内联计划。
struct InlinePlan {
    /// SSA 名 -> 内联表达式 (`Some(None)` = 不可内联, 保留为独立赋值语句)
    subst: HashMap<String, Option<Expr>>,
    /// 未内联因而必须以独立赋值发射、并且需要**局部声明**的 `_` 前缀临时基名
    kept_temp_bases: OrderedNameSet,
}

/// 把 SSA→AST 重建「单次使用临时值就地内联」的决策算一遍, 产出 `subst` 表。
///
/// 必须独立成函数并在**生成局部声明之前**调用: 被拒绝内联的临时值会退化成一条
/// 对 `_xxx` 变量的赋值语句, 于是那个名字必须进局部声明表 (否则后端按隐式 int
/// 处理, 指针/double 临时值直接被截断) —— 声明序因此依赖内联决策。
fn plan_inlined_temps(func: &Function) -> InlinePlan {
    // 收集所有被用作操作数的 SSA 变量
    let mut used_ssa: HashSet<String> = HashSet::new();
    for bb in func.blocks.values() {
        for inst in &bb.insts {
            collect_op_uses(&inst.kind, &mut used_ssa);
        }
        match &bb.term {
            Terminator::Ret { val: Some(o) } => collect_op_uses_op(o, &mut used_ssa),
            Terminator::Branch { cond, .. } => collect_op_uses_op(cond, &mut used_ssa),
            Terminator::SwitchTable { idx, .. } => collect_op_uses_op(idx, &mut used_ssa),
            _ => {},
        }
    }

    // 定义位置索引 (SSA 名 -> 块/序号) 与 `AddrOf` 目标基名索引。
    // 按 `block_order` 创建序遍历, 保证同一名字多次定义时首个胜出且与哈希序无关。
    let mut def_pos: HashMap<String, (String, usize)> = HashMap::new();
    let mut addr_base: HashMap<String, String> = HashMap::new();
    for bb_name in &func.block_order {
        let Some(bb) = func.blocks.get(bb_name) else {
            continue;
        };
        for (idx, inst) in bb.insts.iter().enumerate() {
            let Some(d) = &inst.dest else { continue };
            def_pos.entry(d.name()).or_insert((bb_name.clone(), idx));
            if let InstKind::AddrOf { base } = &inst.kind {
                addr_base.entry(d.name()).or_insert_with(|| base.clone());
            }
        }
    }

    let mut subst: HashMap<String, Option<Expr>> = HashMap::new();
    let mut kept_temp_bases = OrderedNameSet::new();
    let mut expanded: HashMap<String, ExpandedTemp> = HashMap::new();

    // 对每个 SSA 定义: 如果是临时变量且只被使用一次, 标记为内联
    for bb_name in &func.block_order {
        let Some(bb) = func.blocks.get(bb_name) else {
            continue;
        };
        for (idx, inst) in bb.insts.iter().enumerate() {
            let Some(d) = &inst.dest else { continue };
            let ssa_name = d.name();
            // 「合成临时值」的判定必须覆盖生产端的**全部**命名方案 (`_xxx` 与
            // `dcc_t<序号>_xxx` 都在内): 漏掉一类不是「少内联一次」, 而是**静默错值** ——
            // `rebuild_cycle` 在能提出循环条件时会整体丢弃节点头块的普通指令 (那里只有
            // `cond_expr.is_none()` 才 emit), 前提是条件操作数已被本表 (subst) 内联成
            // 表达式; 判定失效后 `while (i < 3)` 的这次比较连同写入一起消失, 只剩
            // `Stmt::While{cond: Ident "dcc_t1__tmp"}` 读一个从未赋值的局部 (0),
            // 循环体一次都不执行。但判定也不能把**用户变量**当临时值 (前缀启发式的错),
            // 所以用 `is_synthetic_temp_base`: 它只信「局部声明集 + 参数」。
            let is_temp = is_synthetic_temp_base(func, &d.base);
            let use_count = used_ssa.get(&ssa_name).map_or(0, |_| {
                let mut count = 0;
                for b in func.blocks.values() {
                    for i in &b.insts {
                        count += op_count_in_kind(&i.kind, &ssa_name);
                    }
                    match &b.term {
                        Terminator::Ret { val: Some(o) } => {
                            count += op_count_in_op(o, &ssa_name);
                        },
                        Terminator::Branch { cond, .. } => {
                            count += op_count_in_op(cond, &ssa_name);
                        },
                        Terminator::SwitchTable { idx, .. } => {
                            count += op_count_in_op(idx, &ssa_name);
                        },
                        _ => {},
                    }
                }
                count
            });
            // 有副作用的临时结果 (如返回值被丢弃的 Call) 在
            // use_count==0 时不允许内联: 表达式存入 subst 后没有任何
            // 使用点消费, 重建时会被跳过, 副作用随之丢失. volatile 访问
            // (含未被读的 volatile 局部) 同理 —— 那次访问本身就是结果.
            let side_effect_unused = use_count == 0 && inst_unremovable(&inst.kind);
            // C++ 零成本 EH: 受保护调用与落地垫序言**永不内联**。
            // 内联等于把定义点丢掉、在使用处重算: 前者会让 LSDA 里登记
            // 的地址区间指向一条不存在的调用指令 (异常找不到帧), 后者会
            // 让落地垫序言不再是一条独立语句 (rax/rdx 被覆盖)。
            let is_eh_pinned = matches!(
                inst.kind,
                InstKind::Invoke { .. } | InstKind::EhLandingPad { .. }
            );
            let mut chain: Option<ExpandedTemp> = None;
            let inlineable = is_temp
                && use_count <= 1
                && !side_effect_unused
                && !is_eh_pinned
                && {
                    // SSA→AST 重建把 `n_1`/`n_2` 塌缩回同一个变量 `n`, 于是内联
                    // 「在使用点重算表达式」会把对 `n` 的读**搬到中途赋值之后**。
                    // 真 SSA 里无害, 塌缩后就是静默错值: 调度后
                    // `_tbin = 2*n; _tupd = n-1; n = _tupd; s = _tbin + s`
                    // 内联成 `s = (2*n) + s`, 乘的是**已自减**的 `n` —— 实测
                    // `int s=0; while(n>0){int step=2; s+=step*n; n--;}` 的
                    // `a(4)` 在 -O3 返回 6 而不是 12。因此内联必须过窗口判定。
                    match inline_window_safe(
                        func,
                        &ssa_name,
                        bb_name,
                        idx,
                        inst,
                        &def_pos,
                        &addr_base,
                        &subst,
                        &expanded,
                    ) {
                        Some(info) => {
                            chain = Some(info);
                            true
                        },
                        None => false,
                    }
                };
            if inlineable {
                let resolved = inst_to_expr_resolved(
                    &inst.kind,
                    &subst,
                    &inst.span,
                    func,
                    inst.dest.as_ref().map(|v| &v.ty),
                );
                subst.insert(ssa_name.clone(), Some(resolved));
                if let Some(info) = chain {
                    expanded.insert(ssa_name, info);
                }
            } else {
                subst.insert(ssa_name, None);
                // 未内联的合成临时值会发射成对 `_xxx`/`dcc_tX_xxx` 的赋值: 必须有
                // 声明, 否则后端按隐式 int 处理。用户自己的 `_s` 不在这里 (它已在
                // `original_locals` 里被声明), 用 is_temp 区分。
                if is_temp && !is_pass_hint_inst(&inst.kind) {
                    kept_temp_bases.insert(d.base.clone());
                }
            }
        }
    }

    InlinePlan {
        subst,
        kept_temp_bases,
    }
}

/// 结构化重建**表达不了**的循环形状：某个自然循环有**两个以上**的「循环外后继」。
///
/// 为什么这是硬歧义而不是可以就地展开的细节：`while` 只有一个落出口，共享续体既要从
/// 循环的 break 路径到这里，又要由**外层**那条边发射一次；在一处发就把另一处吃掉了。
/// 实测两端都错过：
///   * 把侧出口当续体在循环后发 ⇒ `a(2)` 返 7（gcc 返 1，循环体其实只正确跑了 2 轮）；
///   * 提升节点头分支为 `while` 条件、续体取正常落出块 ⇒ `a(0)`/`a(1)` 返 0（落出路径
///     后面空无一物，函数直接落到序言）。
/// 为什么不是「续体被外层共享」：普通 `for` 的 `for.end` 本来就同时是 preheader
/// `for.cond` 的假边目标（实测前驱 = [for.step, for.cond]），那条判据会把**所有** for
/// 循环都挡掉。用「出边个数」才对准本歧义：`for (i=0;i<n;i++) if (i==2) return 7;`
/// 有 `if.then`(ret 7) 与 `for.end`(ret 1) 两个出口，而普通循环只有 `for.end` 一个。
///
/// 与聚合整体赋值同一政策：IR 表达不了的形状**整函数退出 SSA 往返**，而不是发一个
/// 看起来对、实际接错线的结构。
pub fn has_multi_exit_cycle(func: &Function) -> bool {
    let doms = compute_dominators(func);
    for (header, hb) in func.blocks.iter() {
        // 自然循环的 header：存在一条从某前驱回来的回边。
        let latches: Vec<String> = hb
            .preds
            .iter()
            .filter(|p| is_back_edge(p, header, &doms))
            .cloned()
            .collect();
        if latches.is_empty() {
            continue;
        }
        let body: HashSet<String> = find_loop_body_multi(func, header, &latches)
            .into_iter()
            .collect();
        // 收集循环外后继
        let mut outside: Vec<String> = Vec::new();
        for name in body.iter()
            .chain(std::iter::once(header))
        {
            let Some(b) = func.blocks.get(name) else { continue };
            // 后继从 terminator 现取, 不读 `b.succs`: 本项目的 `term_succs` 文档写明
            // SSA pass 改过 terminator 后 `succs` 可能过期。这里读旧边集会**漏掉真存在的
            // 出口边** ⇒ 少计一个出口 ⇒ 把表达不了的循环放进来 (静默误编译的危险方向)。
            for s in term_succs(&b.term) {
                if s == header || body.contains(s) {
                    continue;
                }
                if is_back_edge(name, s, &doms) {
                    continue;
                }
                if !outside.iter().any(|e| e.as_str() == s) {
                    outside.push(s.to_string());
                }
            }
        }
        // 多于一个循环外后继 ⇒ 单个 `while` 表达不了（它只有一个落出口）：
        // 侧出口（体内 return/break 走到的块）与正常落出块是两个不同的块。
        if outside.len() > 1 {
            return true;
        }
    }
    false
}

/// 把 SSA Function 重建为 AST Block (codegen 仍走 AST 路径)。
///
/// 策略:
///   1. 只声明原始局部变量 (来自 func.original_locals) 与**未被内联**的 SSA 临时变量。
///   2. 单次使用且顺序安全的 SSA 临时变量在其使用处内联展开, 避免多余栈槽。
///   3. phi 节点在 merge BB 用对原变量赋值处理。
pub fn rebuild_block(func: &Function) -> Block {
    let mut items: Vec<BlockItem> = Vec::new();
    let entry = func.entry.clone();
    let mut visited: HashSet<String> = HashSet::new();
    let mut env: HashMap<String, SsaVar> = HashMap::new();

    // 0. 内联计划 (必须先于局部声明, 见 `plan_inlined_temps` 注释)
    let InlinePlan {
        subst,
        kept_temp_bases,
    } = plan_inlined_temps(func);

    // 1. 只声明原始局部变量 (不声明 SSA 临时变量)
    let param_names: HashSet<String> = func.params.iter().map(|p| p.base.clone()).collect();
    // 收集已通过 PreservedDecl 保留原始声明的变量名
    let preserved_vars: HashSet<String> = func
        .blocks
        .values()
        .flat_map(|bb| bb.insts.iter())
        .filter_map(|inst| match &inst.kind {
            InstKind::PreservedDecl { var } => Some(var.name.name.clone()),
            _ => None,
        })
        .collect();

    // 收集所有 SSA 定义中的基础变量名及其类型
    // 用于为不在 original_locals 中但有多次赋值的变量生成声明
    //
    // 必须按 `ordered_block_names` (创建序) 而不是 `blocks.values()` (HashMap 随机序)
    // 遍历: 同一 base 的多个 SSA 版本可能携带不同 `ty`, `or_insert_with` 的
    // "首个胜出" 规则会因遍历序而选到不同类型; 而且这里得到的名字序会被下面
    // 直接拼进 `decl_names`, 决定局部变量声明序 → 决定栈槽偏移。
    let mut all_def_types: HashMap<String, Type> = HashMap::new();
    let mut all_def_order = OrderedNameSet::new();
    for bb_name in ordered_block_names(func) {
        let Some(bb) = func.blocks.get(&bb_name) else {
            continue;
        };
        for inst in &bb.insts {
            if let Some(d) = &inst.dest {
                if all_def_order.insert(d.base.clone()) {
                    all_def_types.insert(d.base.clone(), d.ty.clone());
                }
            }
        }
        for phi in &bb.phis {
            if all_def_order.insert(phi.dest.base.clone()) {
                all_def_types.insert(phi.dest.base.clone(), phi.dest.ty.clone());
            }
        }
    }

    // phi 的 dest 永远不会由指令赋值: rebuild_bb 只把它映射到 env 里的某个
    // 已有 SSA 值。若在这里把它当成局部变量声明, codegen 会为它分配栈槽但
    // 没有任何写入, 读取时拿到未初始化值 —— `int z = a && b;` 之类经
    // lower_logical_and 生成 phi 的表达式会静默出错。因此 phi dest 一律不声明。
    let phi_dest_names: HashSet<String> = func
        .blocks
        .values()
        .flat_map(|bb| bb.phis.iter())
        .map(|p| phi_name(&p.dest))
        .collect();

    let mut decl_names: Vec<String> = func.original_locals.iter().cloned().collect();
    // 追加: 非 _ 开头、非参数、非 original_locals、非 preserved 的变量
    // 这些是 lower_ir 生成的非临时变量（如 fresh_var 生成的非临时名），
    // 需要声明否则 codegen 会按隐式 int 处理导致类型错误
    // 注意: 这里遵循 `all_def_order`(首次定义序) 而非 HashMap 随机序。
    for name in all_def_order.iter() {
        if phi_dest_names.contains(name) {
            continue;
        }
        // 合成临时值默认**不**声明 —— 它们通常已被内联成表达式。
        // 只有内联被判定为顺序不安全 (见 `plan_inlined_temps`) 因而会发射成一条
        // 对它的真实赋值时, 才必须给它声明, 否则后端按隐式 int 处理。
        // 用户自己的 `_s` 已在 `original_locals` 里, 不走这条跳过分支。
        if is_synthetic_temp_base(func, name) && !kept_temp_bases.contains(name) {
            continue;
        }
        if param_names.contains(name) {
            continue;
        }
        if func.original_locals.contains(name) {
            continue;
        }
        if preserved_vars.contains(name) {
            continue;
        }
        decl_names.push(name.clone());
    }

    for name in &decl_names {
        if param_names.contains(name) {
            continue;
        }
        // 已通过 PreservedDecl 保留原始声明, 跳过自动声明
        if preserved_vars.contains(name) {
            continue;
        }
        // Use the stored type from local_types if available, otherwise try to infer from instructions
        let ty = func.local_types.get(name).cloned().unwrap_or_else(|| {
            all_def_types.get(name).cloned().unwrap_or_else(|| {
                func.blocks
                    .values()
                    .flat_map(|bb| bb.insts.iter())
                    .find_map(|inst| {
                        inst.dest
                            .as_ref()
                            .filter(|d| &d.base == name)
                            .map(|d| d.ty.clone())
                    })
                    .unwrap_or(Type::Int)
            })
        });
        // 保留原始类型限定符 (volatile/const/restrict),
        // 防止 volatile 变量的读写被优化器错误移除
        let quals = func
            .local_quals
            .get(name)
            .copied()
            .unwrap_or(crate::ast::QualifierFlags::NONE);
        items.push(BlockItem::Decl(Decl::Var(crate::ast::VarDecl {
            ty,
            name: crate::ast::Ident {
                name: name.clone(),
                span: func.span.clone(),
            },
            init: None,
            span: func.span.clone(),
            is_extern: false,
            is_static: false,
            section: None,
            alignment: None,
            is_weak: false,
            visibility: None,
            is_unused: false,
            register_name: None,
            cleanup_fn: None,
            is_thread_local: false,
            no_sanitize: vec![],
            alias_name: None,
            copy_fn: None,
            is_retain: false,
            is_used_attr: false,
            quals,
            addr_space: crate::ast::AddrSpace::None,
            tls_model: None,
            asm_label: None,
        })));
    }

    let doms = compute_dominators(func);
    let mut cx = RebuildCtx {
        func,
        doms: &doms,
        subst: &subst,
        visited: &mut visited,
        env: &mut env,
    };
    rebuild_bb(&entry, &mut cx, &mut items, None, None);
    // C++ 零成本异常: 落地垫区不属于正常 CFG (只由 unwinder 按地址区间进入),
    // 上面的结构化重建从 entry 出发走不到它们。在函数体末尾以
    // `Label + 平铺语句 + Goto` 的形式补发射。
    rebuild_eh_blocks(func, &mut cx, &mut items);
    Block {
        items,
        span: func.span.clone(),
    }
}

/// 把一条 SSA 指令翻译成 AST 语句并追加到 `out`。
///
/// `rebuild_bb` (结构化重建) 与 `rebuild_eh_blocks` (落地垫平铺发射) 共用同一份
/// 翻译规则: 否则受保护调用与落地垫序言在两条路径上的语义会漂移 —— 实测只
/// 在其中一处特殊处理, `Stmt::Invoke` 的包装会时有时无, LSDA 登记的地址区间
/// 因此指向不存在的调用指令 (抛异常时找不到帧)。
fn push_inst_stmt(inst: &Inst, bb_name: &str, cx: &mut RebuildCtx<'_>, out: &mut Vec<BlockItem>) {
    let func = cx.func;
    let subst = cx.subst;
    // C++ 零成本异常: 落地垫序言必须是一条**专用语句** (后端要在标签后的第一
    // 条指令把 %rax/%rdx 溢出到槽位), 不能退化成一次赋值表达式。
    if let InstKind::EhLandingPad { selector } = &inst.kind {
        let Some(obj) = inst.dest.as_ref() else {
            return;
        };
        let sel = selector;
        cx.env.insert(obj.name(), obj.clone());
        cx.env.insert(sel.name(), sel.clone());
        let mk_ident = |v: &SsaVar| Expr::Ident {
            name: crate::ast::Ident {
                name: v.base.clone(),
                span: v.span.clone(),
            },
            span: v.span.clone(),
            ty: v.ty.clone(),
        };
        out.push(BlockItem::Stmt(Stmt::EhLandingPad {
            label: crate::ast::Ident {
                name: bb_name.to_string(),
                span: func.span.clone(),
            },
            object: Box::new(mk_ident(obj)),
            selector: Box::new(mk_ident(sel)),
            span: func.span.clone(),
        }));
        return;
    }
    // 受保护调用: 先按普通调用生成一条语句, 再用 `Stmt::Invoke` 把那条语句整体
    // 包起来 —— 后端以被包装语句的地址区间作为 LSDA call-site 区间。
    let eh_wrap: Option<(Option<String>, Vec<crate::ast::EhAction>)> = match &inst.kind {
        InstKind::Invoke {
            unwind, actions, ..
        } => Some((unwind.clone(), actions.clone())),
        _ => None,
    };
    if let Some(d) = &inst.dest {
        cx.env.insert(d.name(), d.clone());
        let ssa_name = d.name();
        if let Some(Some(_)) = subst.get(&ssa_name) {
            return;
        }
        let rhs = inst_to_expr_resolved(&inst.kind, subst, &inst.span, func, Some(&d.ty));
        let assign = Expr::Assign {
            op: crate::ast::BinOp::Add,
            l: Box::new(Expr::Ident {
                name: crate::ast::Ident {
                    name: d.base.clone(),
                    span: d.span.clone(),
                },
                span: d.span.clone(),
                ty: d.ty.clone(),
            }),
            r: Box::new(rhs),
            span: inst.span.clone(),
            ty: d.ty.clone(),
        };
        out.push(BlockItem::Stmt(Stmt::Expr {
            e: assign,
            span: inst.span.clone(),
        }));
    } else {
        let e = inst_to_expr_resolved(&inst.kind, subst, &inst.span, func, None);
        out.push(BlockItem::Stmt(Stmt::Expr {
            e,
            span: inst.span.clone(),
        }));
    }
    if let Some((lp, actions)) = eh_wrap {
        // 上面必定刚追入了一条语句; 取回它并包装。若拿不到 (不是语句形态)
        // 则保持原样推回, 不牺牲已有代码。
        let Some(BlockItem::Stmt(call)) = out.pop() else {
            return;
        };
        out.push(BlockItem::Stmt(Stmt::Invoke {
            call: Box::new(call),
            landing_pad: lp.map(|name| crate::ast::Ident {
                name,
                span: func.span.clone(),
            }),
            actions,
            span: func.span.clone(),
        }));
    }
}

/// 在函数体末尾平铺发射落地垫区。
///
/// 发射顺序 = `eh_unwind_blocks` 的登记序 (由 lowering 保证确定), 因为该顺序
/// 直接决定落地垫在 `.text` 中的位置与 LSDA call-site 表里的 landing pad 偏移,
/// 必须是产物可复现的单一真源。区内**不**做结构化循环/条件恢复: 每条边都用
/// 显式 `goto`, 因此一个块只会发射一次, 且跳转目标可以是任意块
/// (其他落地垫块, 或带 `eh_reentry_labels` 标签的正常规块)。
fn rebuild_eh_blocks(func: &Function, cx: &mut RebuildCtx<'_>, out: &mut Vec<BlockItem>) {
    if func.eh_unwind_blocks.is_empty() {
        return;
    }
    // 兜底: 仍有回入点没被发射过就不能留悬空 `goto`。
    //
    // 正常情况下的 try 汇合块一定在入口可达的条带上 (lowering 把 `current_bb`
    // 切到它继续下探)。但整块 try 处于死代码 (如 `if (0) { try{...}catch{...} }`)
    // 时, 结构化重建从 entry 走不到它, 而落地垫区仍会发 catch 体里的
    // `goto try_end` —— 链接期就是 undefined reference。这里按平铺模式补发射
    // 未贴标签的回入点 (`rebuild_bb` 自己负责贴标签, 并已访问的后继直接为空),
    // 而不是只报错: 死代码不该让编译失败。
    let missing: Vec<String> = func
        .eh_reentry_labels
        .iter()
        .filter(|name| {
            func.blocks.contains_key(*name) && !cx.visited.contains(&format!("eh_label#{}", name))
        })
        .cloned()
        .collect();
    for name in missing {
        rebuild_bb(&name, cx, out, None, None);
    }
    for name in &func.eh_unwind_blocks {
        let Some(bb) = func.blocks.get(name) else {
            continue;
        };
        let bb = bb.clone();
        // 同一个落地垫块可能被多个调用点共享, 只发射一次。
        if !cx.visited.insert(name.clone()) {
            continue;
        }
        let mut items: Vec<BlockItem> = Vec::new();
        for phi in &bb.phis {
            if let Some((src, _)) = phi.incoming.first() {
                cx.env.insert(phi_name(&phi.dest), src.clone());
            }
        }
        for inst in &bb.insts {
            if is_pass_hint_inst(&inst.kind) {
                continue;
            }
            if let InstKind::PreservedDecl { var } = &inst.kind {
                items.push(BlockItem::Decl(Decl::Var(var.clone())));
                continue;
            }
            push_inst_stmt(inst, name, cx, &mut items);
        }
        match &bb.term {
            Terminator::Ret { val } => {
                let e = val
                    .as_ref()
                    .map(|o| resolve_operand(o, cx.env, cx.subst, func));
                items.push(BlockItem::Stmt(Stmt::Return {
                    val: e,
                    span: func.span.clone(),
                }));
            },
            Terminator::Jump(t) => items.push(BlockItem::Stmt(Stmt::Goto {
                target: crate::ast::Ident {
                    name: t.clone(),
                    span: func.span.clone(),
                },
                span: func.span.clone(),
            })),
            Terminator::Branch { cond, t, f } => {
                let cond_e = resolve_operand(cond, cx.env, cx.subst, func);
                let mk_goto = |n: &str| Stmt::Goto {
                    target: crate::ast::Ident {
                        name: n.to_string(),
                        span: func.span.clone(),
                    },
                    span: func.span.clone(),
                };
                items.push(BlockItem::Stmt(Stmt::If {
                    cond: cond_e,
                    then_: Box::new(mk_goto(t)),
                    else_: Some(Box::new(mk_goto(f))),
                    span: func.span.clone(),
                }));
            },
            // 落地垫区内的 `Unreachable` = 正常永远走不到 (如 `_Unwind_Resume`
            // 后的填充); 不发任何跳转, 自然落入下一段。
            // `SwitchTable` 不出现在落地垫区 (分派只按 selector 比较)。
            Terminator::Unreachable | Terminator::SwitchTable { .. } => {},
        }
        out.push(BlockItem::Stmt(Stmt::Label {
            name: crate::ast::Ident {
                name: name.clone(),
                span: func.span.clone(),
            },
            stmt: Box::new(Stmt::Block {
                b: Block {
                    items,
                    span: func.span.clone(),
                },
                span: func.span.clone(),
            }),
            span: func.span.clone(),
        }));
    }
}

fn phi_name(v: &SsaVar) -> String {
    v.name()
}

/// 识别 lower_ir `fresh_syn` 产出的合成临时值 base 名: `dcc_t<序号>_<后缀>`.
///
/// 这些名字由编译器生成、全局唯一、从不与用户局部变量重名 (`fresh_syn` 会跳过
/// 重名候选), 因此可以和 `_` 前缀名一样参与「单次使用即内联」的重建: 内联后进
/// 循环条件的表达式会在每次迭代重算, 语义正确.
fn is_lower_ir_temp_base(base: &str) -> bool {
    let rest = match base.strip_prefix("dcc_t") {
        Some(r) => r,
        None => return false,
    };
    match rest.find('_') {
        Some(idx) => {
            let digits = &rest[..idx];
            !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
        },
        None => false,
    }
}

/// 判断某条 SSA 指令是否为优化 pass 插入的 hint/metadata (不对应真实计算).
/// 这些指令以 Store 到 `__dcc_` 前缀符号的形式存在, 例如:
///   - register_allocation: `__dcc_regalloc_done`
///   - auto_parallelization: `__dcc_parallel_hint`
///   - pgo: `__dcc_hot_hint` / `__dcc_cold_hint`
///
/// 在 SSA→AST 重建时必须丢弃: 它们向**不存在的符号**发内存写, 送到后端会被按
/// 「未声明名字」回退成本帧栈槽, 于是产出一条 向未初始化地址的写 → 运行期 SEGV
/// (旧 P11 的 `__dcc_section_hot/cold` 就是这么崩的, 该 pass 已删)。
/// 因此丢弃规则不分例外: `__dcc_` 前缀的 Store 一律是 pass 内部元数据。
fn is_pass_hint_inst(kind: &InstKind) -> bool {
    if let InstKind::Store {
        e: Operand::SymAddr(name, _),
        ..
    } = kind
    {
        name.starts_with("__dcc_")
    } else {
        false
    }
}

/// 指令是否带有不可消除的副作用 (Call / Store / volatile InlineAsm 等)。
/// SSA→AST 重建时, 即使其结果未被使用也不能整条丢弃。
fn inst_has_side_effect(kind: &InstKind) -> bool {
    match kind {
        InstKind::Call { .. }
        | InstKind::Invoke { .. }
        | InstKind::EhLandingPad { .. }
        | InstKind::Store { .. }
        | InstKind::IndexStore { .. }
        | InstKind::MemberStore { .. } => true,
        // volatile asm 始终保留；非 volatile asm 带输出也保留
        InstKind::InlineAsm {
            volatile, outputs, ..
        } => *volatile || !outputs.is_empty(),
        _ => false,
    }
}

/// 循环体重建时判断「自然循环之外、但只能整体返回」的块。
///
/// `find_loop_body` 只收集能沿前驱走到 latch 的块 (自然循环定义), 因此**循环体内
/// 直接 `return`** 的块 (terminator 是 `Ret`, 没有任何后继) 不在 body_set 里。
/// 旧实现在这种目标上一律发 `Stmt::Break`, 于是
/// `while (n > 0) { if (n == 3) return 42; n = n - 1; } return 0;`
/// 被重建成 `if (n==3) break;` —— **返回值 42 静默消失**, 函数返回循环之后的值
/// (C++ 后端经 `rebuild_block` 走这条路, C 前端直接建 AST 故不受影响)。
///
/// 只有当该块 ① 无 phi、② 仅有单一前驱 (即它不是多条路径的汇聚出口, 不存在
/// 需要按路径合并的值) 时, 才可以原地展开成 `Stmt::Return`; 其余情形仍按
/// `break` 处理, 保持循环出口汇聚块 (带 phi 的 `while_end`) 的既有正确行为。
fn is_loop_exit_pure_return(target: &str, func: &Function) -> bool {
    let Some(bb) = func.blocks.get(target) else {
        return false;
    };
    if !matches!(bb.term, Terminator::Ret { .. }) {
        return false;
    }
    if !bb.phis.is_empty() {
        return false;
    }
    let mut uniq: HashSet<&str> = HashSet::new();
    for p in &bb.preds {
        uniq.insert(p.as_str());
    }
    uniq.len() == 1
}

/// 循环体外目标块能不能就地展开成「本块语句 + 逐出循环」。
///
/// 背景: `rebuild_cycle` 按**路径**重建循环体, 目标是「不在循环体内」的分支目标
/// 会直接射成 `break`。当该目标块自己带语句时 (实测
/// `for (...) { if (i == 1) { printf("X"); break; } printf("L"); }`), 那些语句随
/// 目标块一起被丢弃 —— dcc 只打 `L`, g++ 打 `LX`。判据:
///   * 本块没有自己的语句 → 与 `break` 完全等价, 不展开 (避免无意义的重复)。
///   * header 的直接出口不展开: 那是循环的**正常后继**, 由循环之后的续体发射一次;
///     展开会让它的语句在 break 路径与落出路径上各跑一遍 (双份执行)。
///   * 任何 latch (含非回边后继的循环体块) 的后继也不展开: do-while / 带尾判的
///     循环里「正常落出」不经过 header, 落出块同样由循环之后的续体发射一次。
///   * 前驱必须全在循环内 (含 header): 否则循环外的路径也会进入本块, 同样双份执行。
///
/// 展开后本块的 terminator (跳出循环的 Jump) 会自然递归成 `break`, 因此不会把循环
/// 出口块的内容带进来。
fn should_inline_loop_exit_target(
    target: &str,
    func: &Function,
    header: &str,
    body: &HashSet<String>,
) -> bool {
    let Some(bb) = func.blocks.get(target) else {
        return false;
    };
    if bb.insts.iter().all(|i| is_pass_hint_inst(&i.kind)) {
        return false;
    }
    if let Some(h) = func.blocks.get(header) {
        if h.succs.iter().any(|s| s == target && !body.contains(s)) {
            return false;
        }
    }
    // do-while 这类「条件在尾部」的循环: 正常落出由 latch 的非回边后继完成,
    // 那个落出块同样属于循环之后的续体, 内联会双份执行。
    for (name, b) in func.blocks.iter() {
        if name == header || !body.contains(name) {
            continue;
        }
        if !b.succs.iter().any(|s| s == header) {
            continue;
        }
        if b.succs.iter().any(|s| s == target) {
            return false;
        }
    }
    bb.preds
        .iter()
        .all(|p| p.as_str() == header || body.contains(p.as_str()))
}

/// SSA→AST 结构化重建 (`rebuild_bb` / `rebuild_cycle`) 的共享上下文。
///
/// 把一路沿递归透传的 5 个参数 (`func` / `doms` / `subst` / `visited` /
/// `env`) 聚成一个结构体, 让两条重建入口的签名不再超长。其中前三个是
/// 只读视角, 后两个是随重建推进的可变状态。
///
/// 分支 / `switch` 的每条路径需要各自独立的 `env`/`subst` 副本, 因此在
/// 递归调用处就地派生子上下文 (`RebuildCtx { env: &mut path_env, .. }`),
/// 语义与原先逐参数克隆传递完全一致。
struct RebuildCtx<'a> {
    /// 正在重建的 SSA 函数: 块表 / block_order / span 等只读信息
    func: &'a Function,
    /// 支配树 (块名 → 支配它的块名集合): 回边与循环识别用, 只读
    doms: &'a HashMap<String, HashSet<String>>,
    /// SSA 变量名 → 可内联表达式 (`None` = 已定义但不可内联), 只读
    subst: &'a HashMap<String, Option<Expr>>,
    /// 已整体发射过的块; 仅直线条带区 (`loop_ctx.is_none()`) 生效
    visited: &'a mut HashSet<String>,
    /// SSA 变量名 → 当前活跃的 SSA 版本, 随 phi / 指令赋值推进
    env: &'a mut HashMap<String, SsaVar>,
}

fn rebuild_bb(
    bb_name: &str,
    cx: &mut RebuildCtx<'_>,
    out: &mut Vec<BlockItem>,
    loop_ctx: Option<(&str, &HashSet<String>)>,
    // 直线条带区里的**汇聚点**: 走到它就立即返回, 由调用方 (产生分支的那个
    // `Terminator::Branch`) 在 If **之后**发射一次。仅在 `loop_ctx.is_none()`
    // 时有意义 (循环体内两条路径本来各自以 continue/break 结束)。
    stop_at: Option<&str>,
) {
    // 从上下文取出只读视角: 都是 Copy 的共享引用, 取出后与 `cx` 的可变借用
    // 互不冲突 (就地派生子上下文时仍可继续使用)。
    let func = cx.func;
    let doms = cx.doms;
    let subst = cx.subst;
    // In loop-internal mode, we rebuild blocks per-path so that join points
    // (e.g. for_inc reached from both branches of an if) are not skipped.
    // Each *straight-line* path in a structured loop ends with continue or
    // break; a nested loop header reached inside the body is a cycle entry
    // whose back edge would otherwise be followed as a normal edge and
    // recurse forever (header -> body -> latch -> header -> ...). That case
    // is converged by the `is_cycle_entry` branch below (nested
    // `rebuild_cycle`), which is also why the check must run in loop-internal
    // mode, not only at top level.
    if loop_ctx.is_none() {
        // 根因修复: 以前这里直接把汇聚块内联进**第一条**被重建的路径
        // (`Jump(join)` 递归下去) 并用共享的 `visited` 把它标记成已处理,
        // 于是另一条路径重建时它返回空 (`else_: None`), If **之后**的所有
        // 语句只会在条件为真时执行 —— 静默错值/跳初始化后的全部代码.
        // 典型现场: `if (__cxa_guard_acquire(g)) { init; release; } 后续代码`
        // 使函数局部 static 只在首次调用时执行函数体 (其余调用直接返回),
        // `if (c) {...} ...` 同理 (C 路径不受影响, 它不走这个重建)。
        if let Some(s) = stop_at {
            if bb_name == s {
                return;
            }
        }
        if !cx.visited.insert(bb_name.to_string()) {
            return;
        }
    }
    let bb = match func.blocks.get(bb_name) {
        Some(b) => b,
        None => return,
    };
    // In loop-internal mode, reaching the loop header means continue.
    if let Some((lh, _)) = loop_ctx {
        if bb_name == lh {
            out.push(BlockItem::Stmt(Stmt::Continue {
                span: func.span.clone(),
            }));
            return;
        }
    }
    // Detect if this block is a cycle entry (target of a back edge).
    // If so, emit a while loop to represent the cyclic CFG.
    //
    // 这一检查在循环体内同样必须做: 到达当前循环 header 的路径在上面的
    // `bb_name == lh` 分支已经变成 `continue` 提前返回, 所以循环内模式走到
    // 这里的块若仍是回边目标, 它必然是**嵌套内层循环**的 header。以前只在
    // `loop_ctx.is_none()` 时识别环, 内层回边 (latch -> inner header) 被当作
    // 普通边继续递归展开, `header -> body -> latch -> header -> ...` 无限互
    // 递归 —— C++ 后端任意两层 for/while 嵌套都栈溢出 (根因修复)。
    // C++ 零成本 EH: 落地垫 / catch 区用 `goto` 回入正常规块, 这些块因此必须留下标签。
    //
    // 标签挂在**空块**上: 后端发射 `label:` 后紧跟空块不产生任何指令, 所以正常
    // 执行流照常落到本块的第一条语句, `goto label` 也恰好落到同一位置 —— 两者
    // 共用同一个入口点, 不需要把整个块的内容包进一层嵌套 (那会改变块内变量的
    // 作用域)。
    //
    // 重复标签 = 汇编器错误, 所以单独用一个带命名空间前缀的 `visited` key 记录
    // 「已贴过标签」, 而不是只处理直线条带区:
    //   * 直线条带区本来就由上面的 `visited` 保证每个块只展开一次, 于是这里也只会
    //     贴一次标签。
    //   * 循环体内 (`loop_ctx.is_some()`) 由 `rebuild_cycle` 按**路径**分别重建 ——
    //     这是为了让循环里的汇聚点不被某条路径吃掉, 代价是同一个汇聚块会被展开多次。
    //     以前只在这种模式下不贴标签, 于是「循环体内的 try 正常走完」的回入点成为
    //     悬空 `goto` (链接期 undefined reference)。现在按第一次贴标签并就地展开,
    //     后续路径改成 `goto label`: 两条路径本就是同一个汇聚块, 落到同一份后续代码
    //     正是它们应有的语义。
    //
    // 本检查必须在 `is_cycle_entry` 之前: 回入点恰好是循环头 (如 `while (true)` 里
    // 最后一句是 try) 时, 标签仍要发在循环之前, `goto` 才能正确重入循环。
    if func.eh_reentry_labels.iter().any(|n| n == bb_name) {
        let label_key = format!("eh_label#{}", bb_name);
        if cx.visited.insert(label_key) {
            out.push(BlockItem::Stmt(Stmt::Label {
                name: crate::ast::Ident {
                    name: bb_name.to_string(),
                    span: func.span.clone(),
                },
                stmt: Box::new(Stmt::Block {
                    b: Block {
                        items: Vec::new(),
                        span: func.span.clone(),
                    },
                    span: func.span.clone(),
                }),
                span: func.span.clone(),
            }));
        } else if loop_ctx.is_some() {
            out.push(BlockItem::Stmt(Stmt::Goto {
                target: crate::ast::Ident {
                    name: bb_name.to_string(),
                    span: func.span.clone(),
                },
                span: func.span.clone(),
            }));
            return;
        }
    }
    if is_cycle_entry(bb_name, func, doms) {
        rebuild_cycle(bb_name, cx, out, stop_at, loop_ctx);
        return;
    }
    let bb = bb.clone();
    // 0. phi 处理: 将前驱的 SSA 值映射到 phi dest 的基础名
    for phi in &bb.phis {
        for (src, pred) in &phi.incoming {
            if pred == bb_name || cx.env.contains_key(&phi_name(&phi.dest)) {
                cx.env.insert(phi_name(&phi.dest), src.clone());
            }
        }
        if let Some((src, _)) = bb.phis.first().and_then(|p| p.incoming.first()) {
            cx.env.insert(phi_name(&phi.dest), src.clone());
        }
    }
    // 1. 翻译指令为 AST 赋值语句
    for inst in &bb.insts {
        // 跳过 SSA pass 插入的 hint/metadata 指令
        if is_pass_hint_inst(&inst.kind) {
            continue;
        }
        // PreservedDecl: 直接输出原始变量声明
        if let InstKind::PreservedDecl { var } = &inst.kind {
            out.push(BlockItem::Decl(Decl::Var(var.clone())));
            continue;
        }
        push_inst_stmt(inst, bb_name, cx, out);
    }
    // 2. 翻译 terminator -> AST 后, 继续后继
    match &bb.term {
        Terminator::Ret { val } => {
            let e = val
                .as_ref()
                .map(|o| resolve_operand(o, cx.env, subst, func));
            out.push(BlockItem::Stmt(Stmt::Return {
                val: e,
                span: func.span.clone(),
            }));
        },
        Terminator::Jump(t) => {
            if let Some((lh, body_set)) = loop_ctx {
                if t == lh {
                    out.push(BlockItem::Stmt(Stmt::Continue {
                        span: func.span.clone(),
                    }));
                } else if body_set.contains(t) {
                    rebuild_bb(t, cx, out, loop_ctx, stop_at);
                } else if is_loop_exit_pure_return(t, func)
                    || should_inline_loop_exit_target(t, func, lh, body_set)
                {
                    // 循环体内的 `return` 块 (见 `is_loop_exit_pure_return` 注释),
                    // 或「先做事再跳出循环」的 break 路径块 (见
                    // `should_inline_loop_exit_target` 注释): 展开成对应语句, 不能只发
                    // `break` / `return` 把本块的代码丢掉。
                    rebuild_bb(t, cx, out, loop_ctx, stop_at);
                } else {
                    out.push(BlockItem::Stmt(Stmt::Break {
                        span: func.span.clone(),
                    }));
                }
            } else {
                rebuild_bb(t, cx, out, loop_ctx, stop_at);
            }
        },
        Terminator::Branch { cond, t, f } => {
            // 两条路径的**公共汇聚点** (沿「唯一前驱 + 无条件跳转」链向前走的
            // 第一个多入边块)。发现后: 两条路径都走到它即返回, If 之后把它作为
            // 续体发射一次 —— 否则它会被内联进第一条路径, 另一条路径直接跳过
            // (见函数开头 `stop_at` 处的根因说明)。
            //
            // 只在直线条带区做: 循环体内由 rebuild_cycle 自己处理收敛。
            let join = if loop_ctx.is_none() {
                branch_join(bb_name, t, f, func)
            } else {
                None
            };
            let path_stop = join.as_deref().or(stop_at);
            let cond_e = resolve_operand(cond, cx.env, subst, func);
            let mut then_items: Vec<BlockItem> = Vec::new();
            let mut then_env = cx.env.clone();
            let then_subst = subst.clone();
            match loop_ctx {
                Some((lh, _)) if t == lh => {
                    then_items.push(BlockItem::Stmt(Stmt::Continue {
                        span: func.span.clone(),
                    }));
                },
                Some((lh, body_set)) if !body_set.contains(t) => {
                    if is_loop_exit_pure_return(t, func)
                        || should_inline_loop_exit_target(t, func, lh, body_set)
                    {
                        rebuild_bb(
                            t,
                            &mut RebuildCtx {
                                func,
                                doms,
                                subst: &then_subst,
                                visited: &mut *cx.visited,
                                env: &mut then_env,
                            },
                            &mut then_items,
                            loop_ctx,
                            path_stop,
                        );
                    } else {
                        then_items.push(BlockItem::Stmt(Stmt::Break {
                            span: func.span.clone(),
                        }));
                    }
                },
                _ => {
                    rebuild_bb(
                        t,
                        &mut RebuildCtx {
                            func,
                            doms,
                            subst: &then_subst,
                            visited: &mut *cx.visited,
                            env: &mut then_env,
                        },
                        &mut then_items,
                        loop_ctx,
                        path_stop,
                    );
                },
            }
            let mut else_items: Vec<BlockItem> = Vec::new();
            let mut else_env = cx.env.clone();
            let else_subst = subst.clone();
            match loop_ctx {
                Some((lh, _)) if f == lh => {
                    else_items.push(BlockItem::Stmt(Stmt::Continue {
                        span: func.span.clone(),
                    }));
                },
                Some((lh, body_set)) if !body_set.contains(f) => {
                    if is_loop_exit_pure_return(f, func)
                        || should_inline_loop_exit_target(f, func, lh, body_set)
                    {
                        rebuild_bb(
                            f,
                            &mut RebuildCtx {
                                func,
                                doms,
                                subst: &else_subst,
                                visited: &mut *cx.visited,
                                env: &mut else_env,
                            },
                            &mut else_items,
                            loop_ctx,
                            path_stop,
                        );
                    } else {
                        else_items.push(BlockItem::Stmt(Stmt::Break {
                            span: func.span.clone(),
                        }));
                    }
                },
                _ => {
                    rebuild_bb(
                        f,
                        &mut RebuildCtx {
                            func,
                            doms,
                            subst: &else_subst,
                            visited: &mut *cx.visited,
                            env: &mut else_env,
                        },
                        &mut else_items,
                        loop_ctx,
                        path_stop,
                    );
                },
            }
            out.push(BlockItem::Stmt(Stmt::If {
                cond: cond_e,
                then_: Box::new(Stmt::Block {
                    b: Block {
                        items: then_items,
                        span: func.span.clone(),
                    },
                    span: func.span.clone(),
                }),
                else_: if else_items.is_empty() {
                    None
                } else {
                    Some(Box::new(Stmt::Block {
                        b: Block {
                            items: else_items,
                            span: func.span.clone(),
                        },
                        span: func.span.clone(),
                    }))
                },
                span: func.span.clone(),
            }));
            // 汇聚点作为 If **之后**的续体发射 (只发一次: `stop_at` 已经阻止任何
            // 一条路径把它内联进去)。若它就是外层区域的停止点, 则交给外层发射。
            if let Some(j) = join {
                if Some(j.as_str()) != stop_at {
                    rebuild_bb(&j, cx, out, loop_ctx, stop_at);
                }
            }
        },
        Terminator::Unreachable => {},
        Terminator::SwitchTable {
            idx,
            values,
            targets,
            default,
        } => {
            let idx_e = resolve_operand(idx, cx.env, subst, func);
            let mut cases = Vec::new();
            // Each case branch rebuilds its path independently with its own
            // visited set, so that shared downstream blocks (e.g. switch_end)
            // are processed for each case rather than skipped on subsequent
            // cases due to the first case marking them visited.
            for (v, target) in values.iter().zip(targets.iter()) {
                let mut target_items: Vec<BlockItem> = Vec::new();
                let mut target_env = cx.env.clone();
                let target_subst = subst.clone();
                let mut case_visited = cx.visited.clone();
                // `None`: 每条 case 自带 visited 副本, 链上的多入边块在各条
                // case 路径里各自展开 (与旧行为一致), 因此不走 `stop_at` 机制。
                rebuild_bb(
                    target,
                    &mut RebuildCtx {
                        func,
                        doms,
                        subst: &target_subst,
                        visited: &mut case_visited,
                        env: &mut target_env,
                    },
                    &mut target_items,
                    loop_ctx,
                    None,
                );
                cases.push(Case {
                    val: Some(Expr::Int {
                        v: *v,
                        span: func.span.clone(),
                        ty: Type::Int,
                    }),
                    range_end: None,
                    body: Block {
                        items: target_items,
                        span: func.span.clone(),
                    },
                    span: func.span.clone(),
                });
            }
            // default case
            let mut def_items: Vec<BlockItem> = Vec::new();
            let mut def_env = cx.env.clone();
            let def_subst = subst.clone();
            let mut def_visited = cx.visited.clone();
            rebuild_bb(
                default,
                &mut RebuildCtx {
                    func,
                    doms,
                    subst: &def_subst,
                    visited: &mut def_visited,
                    env: &mut def_env,
                },
                &mut def_items,
                loop_ctx,
                None,
            );
            cases.push(Case {
                val: None,
                range_end: None,
                body: Block {
                    items: def_items,
                    span: func.span.clone(),
                },
                span: func.span.clone(),
            });
            out.push(BlockItem::Stmt(Stmt::Switch {
                expr: idx_e,
                cases,
                span: func.span.clone(),
            }));
        },
    }
}

/// Check if a block is the target of a back edge (cycle entry).
fn is_cycle_entry(bb_name: &str, func: &Function, doms: &HashMap<String, HashSet<String>>) -> bool {
    for (name, bb) in &func.blocks {
        if !is_back_edge(name, bb_name, doms) {
            continue;
        }
        let succs = match &bb.term {
            Terminator::Jump(t) => vec![t.as_str()],
            Terminator::Branch { t, f, .. } => vec![t.as_str(), f.as_str()],
            Terminator::SwitchTable {
                targets, default, ..
            } => {
                let mut s: Vec<&str> = targets.iter().map(|t| t.as_str()).collect();
                if !s.contains(&default.as_str()) {
                    s.push(default.as_str());
                }
                s
            },
            _ => vec![],
        };
        if succs.contains(&bb_name) {
            return true;
        }
    }
    false
}

/// Emit a while loop for a cyclic CFG region.
///
/// Strategy:
/// 1. Find the latch block (source of the back edge to header).
/// 2. Collect loop body blocks via reverse DFS from latch (not going through header).
/// 3. Header instructions are NOT emitted outside the loop; the loop condition
///    is resolved via `resolve_operand` which inlines temporary variables.
/// 4. Body blocks are emitted in order; back edges become `continue`, exits become `break`.
///
/// `outer_ctx` 是调用方所处的**外层**循环上下文 (`loop_ctx`, 顶层为 `None`):
/// 重建嵌套循环时, 循环出口的续体必须回到外层上下文 (外层循环体里的
/// 后继块仍要按外层循环展开, 落到外层 header 时发 `continue`), 而不是
/// 当作顶层直线条带。
fn rebuild_cycle(
    header: &str,
    cx: &mut RebuildCtx<'_>,
    out: &mut Vec<BlockItem>,
    stop_at: Option<&str>,
    outer_ctx: Option<(&str, &HashSet<String>)>,
) {
    // 同 `rebuild_bb`: 只读视角取出为局部, 与下面的可变上下文互不借用。
    let func = cx.func;
    let doms = cx.doms;
    let subst = cx.subst;
    let header_bb = match func.blocks.get(header) {
        Some(b) => b,
        None => return,
    };
    // Mark header as visited so we don't re-enter it via rebuild_bb
    cx.visited.insert(header.to_string());

    // Find all back edges that target the header (latch -> header).
    let mut latches: Vec<String> = Vec::new();
    for (name, bb) in &func.blocks {
        if name == header {
            continue;
        }
        if !is_back_edge(name, header, doms) {
            continue;
        }
        let succs: Vec<&str> = match &bb.term {
            Terminator::Jump(t) => vec![t.as_str()],
            Terminator::Branch { t, f, .. } => vec![t.as_str(), f.as_str()],
            Terminator::SwitchTable {
                targets, default, ..
            } => {
                let mut s: Vec<&str> = targets.iter().map(|t| t.as_str()).collect();
                if !s.contains(&default.as_str()) {
                    s.push(default.as_str());
                }
                s
            },
            _ => vec![],
        };
        if succs.contains(&header) {
            latches.push(name.clone());
        }
    }
    if latches.is_empty() {
        // No back edge found — should not happen for a cycle entry.
        // Fall back to rebuilding the header as a normal block.
        return;
    }
    // Collect loop body blocks via reverse DFS from **all** latches (excluding
    // header). 单 latch 会漏掉 continue 类 latch 所在的分支区域 (见
    // `find_loop_body_multi` 注释)。
    let body_bbs_set: HashSet<String> = find_loop_body_multi(func, header, &latches)
        .into_iter()
        .filter(|b| b != header)
        .collect();

    // Handle header phi nodes: map phi dest to the entry (first incoming) version.
    // In the AST representation, there is only one variable per base name,
    // so SSA phi semantics are captured by mutable variable assignments in the body.
    for phi in &header_bb.phis {
        if let Some((entry_src, _)) = phi.incoming.first() {
            cx.env.insert(phi_name(&phi.dest), entry_src.clone());
        }
    }

    // Determine loop condition from header's branch terminator.
    // For a while-loop, the header is the condition block and we extract
    // the condition expression.  For a do-while (header is a body block
    // whose terminator is a Jump), the condition lives inside the loop
    // body and is expressed as `if (!cond) break;`.
    //
    // 提取出 `cond_expr` 的形态会**整体丢弃**节点头块的普通指令 (见下方
    // `if cond_expr.is_none()`), 因为它的答案已按 `subst` 内联进条件表达式。
    // 只有当每条指令都确实「只是被内联掉的合成临时值」时这个丢弃才不丢语义:
    // 头块里写入**具名对象**的指令一旦被丢掉就是静默错值/死循环 ——
    // `while (k-- > 0) s += k;` 的 `k_2 = k_1 - 1` 正落在节点头, 丢掉后 `k`
    // 永不递减 (实测 -O2 忙等)。
    let header_insts_droppable = header_bb.insts.iter().all(|inst| {
        is_pass_hint_inst(&inst.kind)
            || inst
                .dest
                .as_ref()
                .is_some_and(|d| matches!(subst.get(&d.name()), Some(Some(_))))
    });
    let cond_expr_raw: Option<Expr> = match &header_bb.term {
        Terminator::Branch { cond, t, f } => {
            let t_in_body = body_bbs_set.contains(t);
            let f_in_body = body_bbs_set.contains(f);
            let t_back = is_back_edge(header, t, doms);
            let f_back = is_back_edge(header, f, doms);
            let t_continues = t_in_body || t_back;
            let f_continues = f_in_body || f_back;
            if t_continues && !f_continues {
                Some(resolve_operand(cond, cx.env, subst, func))
            } else if f_continues && !t_continues {
                // Negate condition
                let cond_e = resolve_operand(cond, cx.env, subst, func);
                Some(Expr::Unary {
                    op: crate::ast::UnOp::Not,
                    e: Box::new(cond_e),
                    span: func.span.clone(),
                    ty: Type::Int,
                })
            } else {
                None
            }
        },
        _ => None, // infinite loop or non-branch header (e.g. do-while)
    };
    let cond_expr: Option<Expr> = if header_insts_droppable { cond_expr_raw } else { None };

    // Determine loop body entry block(s) and exit blocks from header terminator.
    // For while-loop: header is cond block, body entry = the successor that's
    // inside the loop, exit = the successor outside the loop.
    // For do-while/infinite: header is a body block, we start recursing from it.
    let mut body_entries: Vec<String> = Vec::new();
    let mut exit_blocks: Vec<String> = Vec::new();
    let mut seen_exit: HashSet<String> = HashSet::new();
    match &header_bb.term {
        Terminator::Branch { t, f, .. } => {
            let t_in_body = body_bbs_set.contains(t);
            let f_in_body = body_bbs_set.contains(f);
            let t_back = is_back_edge(header, t, doms);
            let f_back = is_back_edge(header, f, doms);
            if t_in_body && !t_back {
                body_entries.push(t.clone());
            }
            if f_in_body && !f_back {
                body_entries.push(f.clone());
            }
            if !t_in_body && !t_back && seen_exit.insert(t.clone()) {
                exit_blocks.push(t.clone());
            }
            if !f_in_body && !f_back && seen_exit.insert(f.clone()) {
                exit_blocks.push(f.clone());
            }
        },
        Terminator::Jump(t) => {
            if body_bbs_set.contains(t) && !is_back_edge(header, t, doms) {
                body_entries.push(t.clone());
            } else if !is_back_edge(header, t, doms) {
                exit_blocks.push(t.clone());
            }
        },
        Terminator::SwitchTable {
            targets, default, ..
        } => {
            for t in targets {
                if body_bbs_set.contains(t) && !is_back_edge(header, t, doms) {
                    body_entries.push(t.clone());
                } else if !is_back_edge(header, t, doms) && seen_exit.insert(t.clone()) {
                    exit_blocks.push(t.clone());
                }
            }
            if body_bbs_set.contains(default) && !is_back_edge(header, default, doms) {
                body_entries.push(default.clone());
            } else if !is_back_edge(header, default, doms) && seen_exit.insert(default.clone()) {
                exit_blocks.push(default.clone());
            }
        },
        _ => {},
    }
    // Also scan all body blocks for exit successors (for break, etc.)
    for bb_name in body_bbs_set.iter() {
        let bb = match func.blocks.get(bb_name) {
            Some(b) => b,
            None => continue,
        };
        let succs: Vec<&str> = match &bb.term {
            Terminator::Jump(t) => vec![t.as_str()],
            Terminator::Branch { t, f, .. } => vec![t.as_str(), f.as_str()],
            Terminator::SwitchTable {
                targets, default, ..
            } => {
                let mut s: Vec<&str> = targets.iter().map(|t| t.as_str()).collect();
                if !s.contains(&default.as_str()) {
                    s.push(default.as_str());
                }
                s
            },
            _ => vec![],
        };
        for s in succs {
            if s == header {
                continue;
            }
            if body_bbs_set.contains(s) {
                continue;
            }
            if is_back_edge(bb_name, s, doms) {
                continue;
            }
            if seen_exit.insert(s.to_string()) {
                exit_blocks.push(s.to_string());
            }
        }
    }
    let exit_succ = exit_blocks.into_iter().next();

    // Emit loop body using rebuild_bb in loop-internal mode.
    // Start with the header instructions if the header is a body block
    // (do-while, infinite loop). Then recurse from each body entry block;
    // rebuild_bb handles all control flow recursively, converting jumps to
    // the loop header into `continue` and jumps out of the loop into `break`.
    let mut body_items: Vec<BlockItem> = Vec::new();
    // header 自己就是一个**分支点**时 (无限循环 / 旋转后的 do-while 形态, 即
    // `cond_expr.is_none()`), 必须把它的 Branch 发成 `if (c) {…} else {…}`。
    // 旧做法是把 header 的两个 in-body 后继都归进 `body_entries` 再**顺序**展开 ——
    // 「选哪条边」的条件仍丢掉: `for (...) { if (i==1) continue; s+=i; }` 被重建为
    // 「先 i++ 再测 i<n」的两段直线代码, 循环第一圈就 return (实测 `cont(4)` 返回 0)。
    let mut branched_from_header = false;
    if cond_expr.is_none() {
        for inst in &header_bb.insts {
            if is_pass_hint_inst(&inst.kind) {
                continue;
            }
            emit_rebuild_inst(inst, &mut body_items, cx.env, subst, func);
        }
        if let Terminator::Branch { cond, t, f } = &header_bb.term {
            let both_in_body = body_bbs_set.contains(t) && body_bbs_set.contains(f);
            let neither_is_header = t != header && f != header;
            // 头块指令不可丢弃时 (`while (k-- > 0)` 这种「条件里带自增」的形态)
            // 必须连 header 的 Branch 一起发成 `if (c) {…} else {…}`: 那种情况下
            // 退出判据无法被提到 `while (...)` 里 (提上去就要丢掉头块的写)。
            if (both_in_body && neither_is_header) || !header_insts_droppable {
                branched_from_header = true;
                let cond_e = resolve_operand(cond, cx.env, subst, func);
                let mut then_items: Vec<BlockItem> = Vec::new();
                let mut else_items: Vec<BlockItem> = Vec::new();
                let mut then_env = cx.env.clone();
                let mut else_env = cx.env.clone();
                let then_subst = subst.clone();
                let else_subst = subst.clone();
                let lctx = Some((header, &body_bbs_set));
                // 某一侧跳出循环 (典型: `while (k-- > 0)` 的 exit 边) 时发 `break`;
                // 落在循环内 (含回到 header ⇒ `rebuild_bb` 自己发 `continue`) 才递归重建。
                let side_in_loop = |target: &str| -> bool {
                    target == header || body_bbs_set.contains(target)
                };
                if side_in_loop(t) {
                    rebuild_bb(
                        t,
                        &mut RebuildCtx {
                            func,
                            doms,
                            subst: &then_subst,
                            visited: &mut *cx.visited,
                            env: &mut then_env,
                        },
                        &mut then_items,
                        lctx,
                        stop_at,
                    );
                } else {
                    then_items.push(BlockItem::Stmt(Stmt::Break {
                        span: func.span.clone(),
                    }));
                }
                if side_in_loop(f) {
                    rebuild_bb(
                        f,
                        &mut RebuildCtx {
                            func,
                            doms,
                            subst: &else_subst,
                            visited: &mut *cx.visited,
                            env: &mut else_env,
                        },
                        &mut else_items,
                        lctx,
                        stop_at,
                    );
                } else {
                    else_items.push(BlockItem::Stmt(Stmt::Break {
                        span: func.span.clone(),
                    }));
                }
                body_items.push(BlockItem::Stmt(Stmt::If {
                    cond: cond_e,
                    then_: Box::new(Stmt::Block {
                        b: Block {
                            items: then_items,
                            span: func.span.clone(),
                        },
                        span: func.span.clone(),
                    }),
                    else_: if else_items.is_empty() {
                        None
                    } else {
                        Some(Box::new(Stmt::Block {
                            b: Block {
                                items: else_items,
                                span: func.span.clone(),
                            },
                            span: func.span.clone(),
                        }))
                    },
                    span: func.span.clone(),
                }));
            }
        }
    }
    if !branched_from_header {
        for entry in &body_entries {
            let mut entry_env = cx.env.clone();
            let entry_subst = subst.clone();
            rebuild_bb(
                entry,
                &mut RebuildCtx {
                    func,
                    doms,
                    subst: &entry_subst,
                    visited: &mut *cx.visited,
                    env: &mut entry_env,
                },
                &mut body_items,
                Some((header, &body_bbs_set)),
                stop_at,
            );
        }
    }
    // Emit the while loop
    let cond = cond_expr.unwrap_or(Expr::Int {
        v: 1,
        span: func.span.clone(),
        ty: Type::Int,
    });
    out.push(BlockItem::Stmt(Stmt::While {
        cond,
        body: Box::new(Stmt::Block {
            b: Block {
                items: body_items,
                span: func.span.clone(),
            },
            span: func.span.clone(),
        }),
        span: func.span.clone(),
    }));
    // Continue from exit block (回到外层循环上下文, 顶层调用时 outer_ctx 为 None)
    if let Some(exit) = exit_succ {
        rebuild_bb(&exit, cx, out, outer_ctx, stop_at);
    }
}

/// 计算每个块的前驱个数 (含回边; 同一终结符重复的目标只计一次)。
fn predecessor_counts(func: &Function) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for bb in func.blocks.values() {
        let mut succs: Vec<&str> = Vec::new();
        match &bb.term {
            Terminator::Jump(t) => succs.push(t.as_str()),
            Terminator::Branch { t, f, .. } => {
                succs.push(t.as_str());
                succs.push(f.as_str());
            },
            Terminator::SwitchTable {
                targets, default, ..
            } => {
                for tg in targets {
                    succs.push(tg.as_str());
                }
                succs.push(default.as_str());
            },
            Terminator::Ret { .. } | Terminator::Unreachable => {},
        }
        let mut uniq: HashSet<&str> = HashSet::new();
        for s in succs {
            if uniq.insert(s) {
                *counts.entry(s.to_string()).or_insert(0) += 1;
            }
        }
    }
    counts
}

/// 从 `start` 沿「唯一前驱 + 无条件跳转」链向前走, 返回第一个多条入边的块。
///
/// 链上没有汇聚点 (碰到 return / 二分支 / 缺失块) 或回到已访问块时返回 `None`。
fn walk_to_join(
    start: &str,
    func: &Function,
    preds: &HashMap<String, usize>,
    seen: &mut HashSet<String>,
) -> Option<String> {
    let mut cur = start.to_string();
    loop {
        if !seen.insert(cur.clone()) {
            return None;
        }
        if preds.get(&cur).copied().unwrap_or(0) > 1 {
            return Some(cur);
        }
        let bb = func.blocks.get(&cur)?;
        match &bb.term {
            Terminator::Jump(s) => cur = s.clone(),
            _ => return None,
        }
    }
}

/// 二路分支的公共汇聚块: 先走「唯一前驱 + 无条件跳转」快路径, 不行再取
/// 分支块的**直接后支配者** (见 [`compute_ipdoms`])。
///
/// `from` 是发出这个 `Terminator::Branch` 的块名。
fn branch_join(from: &str, t: &str, f: &str, func: &Function) -> Option<String> {
    if t == f {
        return Some(t.to_string());
    }
    let preds = predecessor_counts(func);
    let jt = walk_to_join(t, func, &preds, &mut HashSet::new());
    let jf = walk_to_join(f, func, &preds, &mut HashSet::new());
    if let (Some(a), Some(b)) = (&jt, &jf) {
        if a == b {
            return Some(a.clone());
        }
    }
    // 快路径判不出凝聚点 (典型: 某一条路径自己又是一个分支 —— 嵌套三元
    // `a ? b : c ? d : e`, 或三元分支里的 `&&`/`||` 短路链), 用后支配者这个
    // 正确判据。它比启发式宽, 因此必须同时要求凝聚点不是分支块自己。
    let ipdom = compute_ipdoms(func);
    let j = ipdom.get(from)?;
    if j == from {
        return None;
    }
    Some(j.clone())
}

/// 反向 CFG 的直接后支配者 (`ipdom[b] = j`: 从 `b` 走到函数出口的**全部**路径
/// 都必经过的、离 `b` 最近的那个块)。
///
/// 为什么需要它: 旧的 `branch_join` 只沿「唯一前驱 + 无条件跳转」链前进, 碰到
/// 二分支就放弃。于是「分支的某一条路径本身又是一个分支」时凝聚点被判为不存在,
/// `rebuild_bb` 就把凝聚块内联进**第一条**展开的路径并用共享的 `visited` 把它标成
/// 已处理 —— 另一条路径重建时它返回空, `If` 之后的所有语句只在条件为真时执行。
/// 实测: `x = a > 10 ? 1 : (a > 3 && a < 9);` 编译后整个主函数在赋值之后就提前
/// 返回, 后面的 `printf` 一句也不执行 (静默丢代码, 不是编不过)。
///
/// 算法: Cooper–Harvey–Kennedy 不动点迭代在反向 CFG (以虚拟出口为根) 上求支配树,
/// 即原 CFG 的后支配树。`Unreachable` / 不可达出口的块不进入结果。
fn compute_ipdoms(func: &Function) -> HashMap<String, String> {
    /// 虚拟出口: 所有 `return` 块的共同后继。
    const EXIT: &str = "\u{1}[postdom-exit]";
    // 1. 正向后继 (`Ret` 额外指向虚拟出口)
    let mut succs: HashMap<&str, Vec<&str>> = HashMap::new();
    for (name, bb) in &func.blocks {
        let mut v: Vec<&str> = term_succs(&bb.term);
        if matches!(bb.term, Terminator::Ret { .. }) {
            v.push(EXIT);
        }
        succs.insert(name.as_str(), v);
    }
    // 2. 反向边
    let mut rev: HashMap<&str, Vec<&str>> = HashMap::new();
    rev.entry(EXIT).or_default();
    for (from, ss) in &succs {
        for s in ss {
            rev.entry(s).or_default().push(from);
        }
    }
    // 3. 反向 CFG 的逆后序 (RPO) 与编号
    let mut post: Vec<&str> = Vec::new();
    let mut visited: HashSet<&str> = HashSet::new();
    visited.insert(EXIT);
    let mut stack: Vec<(&str, usize)> = vec![(EXIT, 0)];
    loop {
        let Some(&(n, idx)) = stack.last() else { break };
        let children: &[&str] = match rev.get(n) {
            Some(v) => v.as_slice(),
            None => &[],
        };
        if idx < children.len() {
            let c = children[idx];
            // 只能在子列表不再被借用时才拿可变引用
            let need_push = visited.insert(c);
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            if need_push {
                stack.push((c, 0));
            }
        } else {
            post.push(n);
            stack.pop();
        }
    }
    post.reverse();
    let num: HashMap<&str, usize> = post.iter().copied().enumerate().map(|(i, n)| (n, i)).collect();
    // 4. 不动点迭代 (Cooper–Harvey–Kennedy)
    //
    // 注意两张表用途不同:**遍历序**用反向图的邻接 (`rev`, 即从出口能走到谁),
    // 而求 `n` 的直接后支配者要看的是反向图的**前驱** = 正向 CFG 的**后继**
    // (`succs[n]`) —— 混用 `rev` 会把所有块的 idom 算成空。
    let mut idom: HashMap<&str, &str> = HashMap::new();
    idom.insert(EXIT, EXIT);
    let mut changed = true;
    while changed {
        changed = false;
        for n in post.iter().skip(1) {
            let forward_succs: &[&str] = match succs.get(n) {
                Some(v) => v.as_slice(),
                None => &[],
            };
            let mut cand: Option<&str> = None;
            for p in forward_succs {
                if !idom.contains_key(p) {
                    continue;
                }
                cand = Some(match cand {
                    None => *p,
                    Some(x) => intersect_post_dom(x, *p, &idom, &num),
                });
            }
            if let Some(d) = cand {
                if idom.get(n) != Some(&d) {
                    idom.insert(n, d);
                    changed = true;
                }
            }
        }
    }
    // 5. 去掉虚拟出口与自己, 只留真实块名的后支配关系
    let mut out: HashMap<String, String> = HashMap::new();
    for (n, d) in idom {
        if n == EXIT || d == EXIT || n == d {
            continue;
        }
        out.insert(n.to_string(), d.to_string());
    }
    out
}

/// 在 RPO 编号上取两个候选后支配者的最近公共祖先。
fn intersect_post_dom<'a>(
    mut a: &'a str,
    mut b: &'a str,
    idom: &HashMap<&'a str, &'a str>,
    num: &HashMap<&'a str, usize>,
) -> &'a str {
    while a != b {
        while num[a] < num[b] {
            b = idom[b];
        }
        while num[b] < num[a] {
            a = idom[a];
        }
    }
    a
}

/// Check if edge from→to is a back edge (to dominates from).
fn is_back_edge(from: &str, to: &str, doms: &HashMap<String, HashSet<String>>) -> bool {
    doms.get(from).map(|d| d.contains(to)).unwrap_or(false)
}

/// Emit a single SSA instruction as an AST statement during rebuild.
fn emit_rebuild_inst(
    inst: &Inst,
    out: &mut Vec<BlockItem>,
    env: &mut HashMap<String, SsaVar>,
    subst: &HashMap<String, Option<Expr>>,
    func: &Function,
) {
    // PreservedDecl: 直接输出原始变量声明 (跳过 SSA 化)
    if let InstKind::PreservedDecl { var } = &inst.kind {
        out.push(BlockItem::Decl(Decl::Var(var.clone())));
        return;
    }
    if let Some(d) = &inst.dest {
        env.insert(d.name(), d.clone());
        let ssa_name = d.name();
        if let Some(Some(_)) = subst.get(&ssa_name) {
            return;
        }
        let rhs = inst_to_expr_resolved(&inst.kind, subst, &inst.span, func, Some(&d.ty));
        let assign = Expr::Assign {
            op: crate::ast::BinOp::Add,
            l: Box::new(Expr::Ident {
                name: crate::ast::Ident {
                    name: d.base.clone(),
                    span: d.span.clone(),
                },
                span: d.span.clone(),
                ty: d.ty.clone(),
            }),
            r: Box::new(rhs),
            span: inst.span.clone(),
            ty: d.ty.clone(),
        };
        out.push(BlockItem::Stmt(Stmt::Expr {
            e: assign,
            span: inst.span.clone(),
        }));
    } else {
        let e = inst_to_expr_resolved(&inst.kind, subst, &inst.span, func, None);
        out.push(BlockItem::Stmt(Stmt::Expr {
            e,
            span: inst.span.clone(),
        }));
    }
}

/// SSA 变量 → AST `Ident` 的唯一出口。重建的多条路径共用一份构造，避免各自漂移。
///
/// 注意：这里**不能**给数组类型的临时值做「衰减成指针」的改写。试过，错的：同一个
/// 「数组类型的 SSA 值」在本 IR 里有两种存储语义（子对象的别名，与住在自己槽里的
/// 聚合），按类型一刀切会让 `may_alias.c` 在 -O2 段错误。正确的修在 GVN 侧（
/// `val_key` 不给数组类型结果编号），见那里的根因说明。
fn ssa_ident_expr(name: &str, v: &SsaVar) -> Expr {
    Expr::Ident {
        name: crate::ast::Ident {
            name: name.to_string(),
            span: v.span.clone(),
        },
        span: v.span.clone(),
        ty: v.ty.clone(),
    }
}

/// 解析操作数: 先查 subst 内联映射, 再回退到基础名。
fn resolve_operand(
    o: &Operand,
    env: &HashMap<String, SsaVar>,
    subst: &HashMap<String, Option<Expr>>,
    func: &Function,
) -> Expr {
    match o {
        Operand::Var(v) => {
            let ssa_name = v.name();
            if let Some(Some(expr)) = subst.get(&ssa_name) {
                return expr.clone();
            }
            // 非内联: 使用基础变量名
            if let Some(found) = env.get(&ssa_name) {
                ssa_ident_expr(&found.base, v)
            } else {
                ssa_ident_expr(&v.base, v)
            }
        },
        _ => operand_to_expr(o, env, func),
    }
}

/// 整数常量的 AST 类型: 与 C++ 字面量类型推导共用 [`crate::cpp::const_text::int_literal_rank_bits`]
/// 这一份真源 (秩 32 → `Int`, 秩 64 → 64 位整型)。
pub fn int_const_ty(v: i128) -> Type {
    match crate::cpp::const_text::int_literal_rank_bits(v) {
        32 => Type::Int,
        _ => Type::LongLong,
    }
}

/// SSA 重建时修正基址表达式的类型。
/// 当 SSA 变量的类型与原始结构体类型不一致时 (如变量被简化为 int),
/// 用原始类型覆盖, 确保后续 IR lowering 能正确解析成员访问。
fn set_expr_ty_if_compatible(expr: &mut Expr, desired_ty: &Type) {
    // 只对 Ident / Deref 等叶子节点修正类型; 复杂表达式保留其计算类型
    match expr {
        Expr::Ident { ty, .. } => {
            *ty = desired_ty.clone();
        },
        Expr::Deref { ty, .. } => {
            *ty = desired_ty.clone();
        },
        _ => {},
    }
}

/// 从两个操作数推导 BinOp 的结果类型
fn infer_binop_type(op: &BinOp, l: &Expr, r: &Expr) -> Type {
    let lt = l.ty();
    let rt = r.ty();
    // 指针算术: `ptr ± int` 的结果仍是该指针类型 (C11 6.5.6)。
    // 必须在这里保留, 否则下游 x86_64 codegen 会按 `Type::Int`(4 字节)
    // 对加法结果做 movslq 截断, 把 64 位地址的高 32 位丢掉
    // (栈地址 0x7ffd_XXXX_XXXX 被截成 0xFFFFFFFF_xxxx, 直接段错误)。
    if matches!(op, BinOp::Add | BinOp::Sub) {
        // `Type::Func` 也算指针类: 函数名作右值时会衰减成函数指针
        // (C11 6.3.2.1/4)。遗这里不得把结果类型回退成 `Int`: SSA 重建里「把值拷进
        // 变量」的 copy 形态就是 `fn + 0`, 不保留指针就得到 32 位截断的地址
        // (实测 -O2 下 `int (*fn)(...) = printf;` 存进 0xffffffffd8e60100,
        // 而真实值是 0x7f... —— 动态库函数地址全部错高位)。
        let is_ptr = |t: &Type| matches!(t, Type::Ptr(_) | Type::Array(_, _) | Type::Func(..));
        let decay = |t: &Type| -> Type {
            match t {
                Type::Func(..) => Type::Ptr(Box::new(QualType::new(t.clone()))),
                other => other.clone(),
            }
        };
        let is_int_like = |t: &Type| t.is_int();
        if is_ptr(&lt) && is_int_like(&rt) {
            return decay(&lt);
        }
        if is_ptr(&rt) && is_int_like(&lt) {
            return decay(&rt);
        }
    }
    // 向量操作: 结果类型与向量操作数相同 (比较也返回向量, 全 1/0 每元素)
    if matches!(&lt, Type::Vector(_, _)) {
        return lt.clone();
    }
    if matches!(&rt, Type::Vector(_, _)) {
        return rt.clone();
    }
    if matches!(
        op,
        BinOp::EqEq
            | BinOp::Ne
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
            | BinOp::AndAnd
            | BinOp::OrOr
    ) && !matches!(&lt, Type::Vector(_, _))
        && !matches!(&rt, Type::Vector(_, _))
    {
        // [expr.rel] / [expr.eq] / C11 6.5.13: **关系、相等、逻辑运算符的结果类型是
        // `int`**, 与操作数是不是浮点无关。必须排在浮点分支之前:
        // C 前端 (`src/parser.rs::parse_binary`) 就是先判关系再算算术类型, 后端
        // 则从**两个操作数自己的类型**选出 `ucomisd` / x87 比较。重建时若把
        // 比较节点标成浮点, 消费端 (赋值 / 存栈槽) 会认为需要一次浮点→整数转换,
        // 于是 `int r = (d < 0.4);` 在 `ucomisd/setb/movzbq` 之后又多一条
        // `cvttsd2si %xmm0,%eax`, 把已算好的 0/1 用残留的浮操作数盖掉
        // (实测 dcc r=0 / g++ r=1; `d < 0.4` 直接作变量用时得到 2)。
        // 向量比较是 GNU 扩展的例外: 结果逐元素为全 1/全 0, 类型同操作数。
        return Type::Int;
    }
    // 复数运算：结果类型是复数，其内部类型取两侧实部的通常算术类型 (C11 6.2.5p13
    // 与 6.3.1.8：`_Complex double` 与 `double` 运算得 `_Complex double`)。必须排在
    // 浮点分支之前、比较分支之后：复数的关系/相等运算结果仍是 int（上面已返回），
    // 而复数算术若不在此定型，就会掉到最后的 Int 回退 —— codegen 靠操作数与结果
    // 类型选择 `gen_complex_binop`，类型一变复数加减就退化成标量浮点/整数运算。
    // 实测：`double _Complex add_dc(x,y){ return x + y; }` 在 -O2 得到 (1.0e9, 0)，
    // 而 gcc 与 -O0 都是 (6, 8)。
    if matches!(&lt, Type::Complex(_) | Type::Imaginary(_))
        || matches!(&rt, Type::Complex(_) | Type::Imaginary(_))
    {
        let real_part = |t: &Type| -> Type {
            match t {
                Type::Complex(q) | Type::Imaginary(q) => q.base.clone(),
                other => other.clone(),
            }
        };
        let (li, ri) = (real_part(&lt), real_part(&rt));
        let rank = |t: &Type| -> u8 {
            match t {
                Type::LongDouble => 3,
                Type::Double => 2,
                Type::Float => 1,
                _ => 0,
            }
        };
        let base = if rank(&li) >= rank(&ri) { li } else { ri };
        return Type::Complex(Box::new(QualType::new(base)));
    }
    if matches!(&lt, Type::Float | Type::Double | Type::LongDouble)
        || matches!(&rt, Type::Float | Type::Double | Type::LongDouble)
    {
        if matches!(&lt, Type::LongDouble) || matches!(&rt, Type::LongDouble) {
            Type::LongDouble
        } else if matches!(&lt, Type::Double) || matches!(&rt, Type::Double) {
            Type::Double
        } else if matches!(&lt, Type::Float) || matches!(&rt, Type::Float) {
            Type::Float
        } else {
            Type::Double
        }
    } else if lt.is_int() && rt.is_int() {
        // 整数运算。旧实现在这里无条件返回 `Type::Int`, 把 64 位整数运算
        // 降格成 32 位记账: SSA→AST 重建后, 把该结果存回 `long` 对象时插入的
        // `Cast{long}` 被当作 sext(i32→i64) 而非原值搬运, 高 32 位静默截掉
        // (`long fp = 103060990L; fp = fp*100 - 10;` 得 1716164398)。
        // C++ 前端整条链都走这条重建路径, 故同一段代码 .c 正常、.cpp 错。
        // 现在与 C 前端共用 [`Type::usual_arith_type`] 单一真源。
        if matches!(
            op,
            BinOp::EqEq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::AndAnd
                | BinOp::OrOr
        ) {
            // C11 6.5.13/6.5.14: 关系与逻辑运算符结果为 `int`。
            Type::Int
        } else if matches!(op, BinOp::Shl | BinOp::Shr) {
            // C11 6.5.7: 移位结果类型 = 左操作数整数提升后的类型
            // (右操作数仅是移位计数, 不参与通常算术转换)。
            lt.int_promotion()
        } else {
            lt.usual_arith_type(&rt)
        }
    } else {
        // 指针差、`_Complex`/`_Imaginary`、枚举等非纯整数情形沿用旧口径
        // (调用方或下游 codegen 自己处理结果表示)。
        Type::Int
    }
}

/// `InstKind::Store` 重建 LHS `Expr::Deref` 时的 **pointee 类型** (store 宽度真源)。
///
/// SSA 的 `Store` 不携带「被写对象类型」字段, 旧实现一律重建为
/// `Expr::Deref { ty: Type::Void }`; 后端按 `lvalue.ty().size()` 选 store 指令,
/// `Void` 回退 8 字节 ⇒ 恒发 `mov %rax,(%rcx)`。对窄对象就是越界写: 实测 C++
/// 析构里 `int` 全局 `g_b = g_b + 1` 会把紧邻的 `g_d` 高 4 字节清零
/// (Itanium 对象模型下 `delete p` 的基类析构计数被抹掉)。
///
/// 宽度只能从**地址操作数自身的类型**还原: `lower_ir` 侧所有 `Store` 的地址
/// 都是 `Ptr(对象类型)` —— 静态存储 (`AddrOf`/`lower_sym_addr`)、引用槽、
/// `*p = v`、vptr 槽。聚合与 `void` pointee 保持 `Void` 原样: 整体拷贝不走
/// `Store` (`lower_static_storage_store` 明确报 unsupported), 不在此猜宽度。
fn store_pointee_ty(addr: &Expr) -> Type {
    match addr.ty() {
        Type::Ptr(inner) => match inner.base {
            Type::Void => Type::Void,
            Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) | Type::Array(_, _) => Type::Void,
            t => t,
        },
        _ => Type::Void,
    }
}

/// `InstKind::AddrOf` 重建 `&base` 的指针类型。
///
/// 旧实现恒写 `Ptr(void)`, 丢掉了「`base` 到底是什么类型」这一件信息:
/// 下游 `*(&sym) = v` (静态存储 / volatile 局部 / 引用槽的回写全部长这样)
/// 只能看到一个 `Ptr(void)` 地址, store 宽度因此回退到 8 字节 —— 对 `int`
/// 对象就是越界写邻位。指真类型就在指令自己的 `dest` 上 (`lower_ir` /
/// `build_from_func` 都以 `Ptr(对象类型)` 声明), 原样带回去即可。
fn addr_of_ty(dest_ty: Option<&Type>) -> Type {
    match dest_ty {
        Some(t @ Type::Ptr(_)) => t.clone(),
        _ => Type::Ptr(Box::new(QualType::new(Type::Void))),
    }
}

/// 一元指令重建时的**结果类型**。
///
/// 第一真源是指令 `dest` 的类型：构建器在 `lower_expr` 里就是用原始
/// `Expr::Unary.ty` 给 dest 定型的，所以它已经带好了「`-d` 是 double、`!x` 是 int」
/// 这类 C 语义。旧实现只认向量，其余一律写 `Type::Int` ⇒ `n = -b`（b 是 double）
/// 重建后成了 `Unary{ty: Int}`，codegen 的一元负号分支（`emit/expr/unary.rs` 靠
/// `e.ty()` 判走不走 xorpd）于是落回整数 `neg r64`，浮点取负直接丢失（实测 -O2
/// 打印 `0.000000` 而不是 `-2.300000`）。dest 缺失时才按 op 与操作数类型推。
fn unary_result_ty(dest_ty: Option<&Type>, op: &UnOp, inner_ty: &Type) -> Type {
    if let Some(t) = dest_ty {
        if !matches!(t, Type::Void) {
            return t.clone();
        }
    }
    match op {
        // `!x` 的结果在 C 里永远是 int。
        UnOp::Not => Type::Int,
        // 其余一元运算保持操作数类型（Neg/BitNot/Plus 在向量上逐元素，类型不变）。
        UnOp::Neg | UnOp::BitNot | UnOp::Plus => inner_ty.clone(),
    }
}

/// 将 SSA 指令转换为 AST 表达式, 同时应用内联替换。
/// `inst_span` 是原始 SSA 指令的 span, 用于保留源码位置信息。
/// `dest_ty` 是该指令 `dest` 变器的 SSA 类型 —— 它是 `AddrOf` 重建 `&base` 与
/// `Un` 重建一元结果（`-d` 必须仍是 double）这两个分支的类型**唯一真源**。
fn inst_to_expr_resolved(
    kind: &InstKind,
    subst: &HashMap<String, Option<Expr>>,
    inst_span: &Span,
    func: &Function,
    dest_ty: Option<&Type>,
) -> Expr {
    let sp = inst_span.clone();
    let fake_env: HashMap<String, SsaVar> = HashMap::new();
    match kind {
        InstKind::Bin { op, l, r } => {
            let l_expr = resolve_operand(l, &fake_env, subst, func);
            let r_expr = resolve_operand(r, &fake_env, subst, func);
            let binop_ty = infer_binop_type(op, &l_expr, &r_expr);
            Expr::Binop {
                op: *op,
                l: Box::new(l_expr),
                r: Box::new(r_expr),
                span: sp.clone(),
                ty: binop_ty,
            }
        },
        InstKind::Un { op, e } => {
            let inner = resolve_operand(e, &fake_env, subst, func);
            let ty = unary_result_ty(dest_ty, op, &inner.ty());
            Expr::Unary {
                op: *op,
                e: Box::new(inner),
                span: sp.clone(),
                ty,
            }
        },
        InstKind::Cast { ty, e } => Expr::Cast {
            ty: ty.clone(),
            e: Box::new(resolve_operand(e, &fake_env, subst, func)),
            span: sp.clone(),
        },
        InstKind::AddrOf { base } => Expr::AddrOf {
            e: Box::new(Expr::Ident {
                name: crate::ast::Ident {
                    name: base.clone(),
                    span: sp.clone(),
                },
                span: sp.clone(),
                ty: Type::Void,
            }),
            span: sp.clone(),
            ty: addr_of_ty(dest_ty),
        },
        InstKind::Load { e, ty, .. } => Expr::Deref {
            e: Box::new(resolve_operand(e, &fake_env, subst, func)),
            span: sp.clone(),
            ty: ty.clone(),
        },
        InstKind::Store { e, src, .. } => {
            let addr = resolve_operand(e, &fake_env, subst, func);
            let dty = store_pointee_ty(&addr);
            Expr::Assign {
                op: BinOp::Add,
                l: Box::new(Expr::Deref {
                    e: Box::new(addr),
                    span: sp.clone(),
                    ty: dty,
                }),
                r: Box::new(resolve_operand(src, &fake_env, subst, func)),
                span: sp.clone(),
                ty: Type::Void,
            }
        },
        InstKind::IndexLoad { arr, idx, ty, .. } => Expr::Index {
            arr: Box::new(resolve_operand(arr, &fake_env, subst, func)),
            idx: Box::new(resolve_operand(idx, &fake_env, subst, func)),
            span: sp.clone(),
            ty: ty.clone(),
        },
        InstKind::IndexStore {
            arr, idx, src, ty, ..
        } => Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Index {
                arr: Box::new(resolve_operand(arr, &fake_env, subst, func)),
                idx: Box::new(resolve_operand(idx, &fake_env, subst, func)),
                span: sp.clone(),
                ty: ty.clone(),
            }),
            r: Box::new(resolve_operand(src, &fake_env, subst, func)),
            span: sp.clone(),
            ty: Type::Void,
        },
        InstKind::Call {
            callee,
            args,
            ret_ty,
        } => Expr::Call {
            callee: Box::new(resolve_operand(callee, &fake_env, subst, func)),
            args: args
                .iter()
                .map(|a| resolve_operand(a, &fake_env, subst, func))
                .collect(),
            span: sp.clone(),
            ty: ret_ty.clone(),
        },
        // C++ 零成本 EH: 受保护调用的**正常路径**与普通调用完全同形 (落地垫由
        // unwinder 按地址区间进入, 不是表达式的一部分)。`rebuild_bb` 会先把这条
        // 指令拦下来包成 `Stmt::Invoke`, 这里只负责调用表达式本身。
        InstKind::Invoke {
            callee,
            args,
            ret_ty,
            ..
        } => Expr::Call {
            callee: Box::new(resolve_operand(callee, &fake_env, subst, func)),
            args: args
                .iter()
                .map(|a| resolve_operand(a, &fake_env, subst, func))
                .collect(),
            span: sp.clone(),
            ty: ret_ty.clone(),
        },
        // 落地垫序言由 `rebuild_bb` 拦截成 `Stmt::EhLandingPad`, 不会走到这里;
        // 保留一个无副作用占位, 以免新增 InstKind 被误当成可内联的纯计算。
        InstKind::EhLandingPad { .. } => Expr::Int {
            v: 0,
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::MemberAddr {
            obj,
            obj_ty,
            field,
            ty,
            is_arrow,
            ..
        } => {
            // 子对象地址: 装回 `&obj->field` / `&(obj.field)`，偏移由 codegen 的
            // 地址形态给出（与 `MemberLoad` 同一个单一真源约定）。
            let mut base = resolve_operand(obj, &fake_env, subst, func);
            if !*is_arrow {
                set_expr_ty_if_compatible(&mut base, obj_ty);
            }
            Expr::AddrOf {
                e: Box::new(Expr::Member {
                    expr: Box::new(base),
                    field: crate::ast::Ident {
                        name: field.clone(),
                        span: sp.clone(),
                    },
                    span: sp.clone(),
                    ty: ty.clone(),
                    is_arrow: *is_arrow,
                }),
                span: sp.clone(),
                ty: Type::Ptr(Box::new(crate::ast::QualType::new(ty.clone()))),
            }
        },
        InstKind::MemberLoad {
            obj,
            obj_ty,
            field,
            ty,
            is_arrow,
            ..
        } => {
            let mut base = resolve_operand(obj, &fake_env, subst, func);
            if *is_arrow {
                // obj 本身就是结构体指针: 保留其指针类型, 重建为 ptr->field
            } else {
                // 重建时修正基址类型: SSA 变量可能丢失了原始结构体类型
                set_expr_ty_if_compatible(&mut base, obj_ty);
            }
            Expr::Member {
                expr: Box::new(base),
                field: crate::ast::Ident {
                    name: field.clone(),
                    span: sp.clone(),
                },
                span: sp.clone(),
                ty: ty.clone(),
                is_arrow: *is_arrow,
            }
        },
        InstKind::MemberStore {
            obj,
            obj_ty,
            field,
            src,
            ty,
            is_arrow,
            ..
        } => {
            let mut base = resolve_operand(obj, &fake_env, subst, func);
            if *is_arrow {
                // obj 本身就是结构体指针: 保留其指针类型
            } else {
                set_expr_ty_if_compatible(&mut base, obj_ty);
            }
            Expr::Assign {
                op: BinOp::Add,
                l: Box::new(Expr::Member {
                    expr: Box::new(base),
                    field: crate::ast::Ident {
                        name: field.clone(),
                        span: sp.clone(),
                    },
                    span: sp.clone(),
                    ty: ty.clone(),
                    is_arrow: *is_arrow,
                }),
                r: Box::new(resolve_operand(src, &fake_env, subst, func)),
                span: sp.clone(),
                // C11 6.5.16p3: 赋值表达式的类型 = 左操作数的类型
                ty: ty.clone(),
            }
        },
        InstKind::Select { cond, then_, else_ } => Expr::Cond {
            cond: Box::new(resolve_operand(cond, &fake_env, subst, func)),
            then_: Box::new(resolve_operand(then_, &fake_env, subst, func)),
            else_: Box::new(resolve_operand(else_, &fake_env, subst, func)),
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::InlineAsm {
            asm_str,
            outputs,
            output_names,
            output_constraints,
            inputs,
            input_names,
            input_constraints,
            clobbers,
            volatile,
            ty,
            output_vars,
            ..
        } => {
            let resolved_outputs = outputs
                .iter()
                .enumerate()
                .map(|(idx, output)| {
                    output_vars
                        .get(idx)
                        .map(|v| Expr::Ident {
                            name: crate::ast::Ident {
                                name: v.base.clone(),
                                span: v.span.clone(),
                            },
                            span: v.span.clone(),
                            ty: v.ty.clone(),
                        })
                        .unwrap_or_else(|| output.clone())
                })
                .collect();
            Expr::AsmExpr {
                asm_str: asm_str.clone(),
                outputs: resolved_outputs,
                output_names: output_names.clone(),
                output_constraints: output_constraints.clone(),
                inputs: inputs
                    .iter()
                    .take(input_names.len())
                    .map(|o| resolve_operand(o, &fake_env, subst, func))
                    .collect(),
                input_names: input_names.clone(),
                input_constraints: input_constraints.clone(),
                clobbers: clobbers.clone(),
                volatile: *volatile,
                span: sp.clone(),
                ty: ty.clone(),
            }
        },
        InstKind::PreservedDecl { .. } => {
            // Handled before inst_to_expr_resolved is called
            Expr::Int {
                v: 0,
                span: sp.clone(),
                ty: Type::Int,
            }
        },
    }
}

/// 收集 InstKind 中所有被引用的 SSA 变量名。
fn collect_op_uses(kind: &InstKind, out: &mut HashSet<String>) {
    match kind {
        InstKind::Bin { l, r, .. } => {
            collect_op_uses_op(l, out);
            collect_op_uses_op(r, out);
        },
        InstKind::Un { e, .. } => collect_op_uses_op(e, out),
        InstKind::Cast { e, .. } => collect_op_uses_op(e, out),
        InstKind::Load { e, .. } => collect_op_uses_op(e, out),
        InstKind::Store { e, src, .. } => {
            collect_op_uses_op(e, out);
            collect_op_uses_op(src, out);
        },
        InstKind::IndexLoad { arr, idx, .. } => {
            collect_op_uses_op(arr, out);
            collect_op_uses_op(idx, out);
        },
        InstKind::IndexStore { arr, idx, src, .. } => {
            collect_op_uses_op(arr, out);
            collect_op_uses_op(idx, out);
            collect_op_uses_op(src, out);
        },
        InstKind::Call { callee, args, .. } => {
            collect_op_uses_op(callee, out);
            for a in args {
                collect_op_uses_op(a, out);
            }
        },
        // C++ 零成本 EH: 同 `collect_var_uses_op`。
        InstKind::Invoke { callee, args, .. } => {
            collect_op_uses_op(callee, out);
            for a in args {
                collect_op_uses_op(a, out);
            }
        },
        InstKind::MemberLoad { obj, .. } => collect_op_uses_op(obj, out),
        InstKind::MemberStore { obj, src, .. } => {
            collect_op_uses_op(obj, out);
            collect_op_uses_op(src, out);
        },
        InstKind::Select { cond, then_, else_ } => {
            collect_op_uses_op(cond, out);
            collect_op_uses_op(then_, out);
            collect_op_uses_op(else_, out);
        },
        InstKind::InlineAsm { inputs, .. } => {
            for input in inputs {
                collect_op_uses_op(input, out);
            }
        },
        _ => {},
    }
}

fn collect_op_uses_op(o: &Operand, out: &mut HashSet<String>) {
    if let Operand::Var(v) = o {
        out.insert(v.name());
    }
}

/// 计算某个 SSA 变量在 InstKind 中被引用的次数。
fn op_count_in_kind(kind: &InstKind, name: &str) -> usize {
    let mut count = 0;
    match kind {
        InstKind::Bin { l, r, .. } => {
            count += op_count_in_op(l, name);
            count += op_count_in_op(r, name);
        },
        InstKind::Un { e, .. } => count += op_count_in_op(e, name),
        InstKind::Cast { e, .. } => count += op_count_in_op(e, name),
        InstKind::Load { e, .. } => count += op_count_in_op(e, name),
        InstKind::Store { e, src, .. } => {
            count += op_count_in_op(e, name);
            count += op_count_in_op(src, name);
        },
        InstKind::IndexLoad { arr, idx, .. } => {
            count += op_count_in_op(arr, name);
            count += op_count_in_op(idx, name);
        },
        InstKind::IndexStore { arr, idx, src, .. } => {
            count += op_count_in_op(arr, name);
            count += op_count_in_op(idx, name);
            count += op_count_in_op(src, name);
        },
        InstKind::Call { callee, args, .. } => {
            count += op_count_in_op(callee, name);
            for a in args {
                count += op_count_in_op(a, name);
            }
        },
        // C++ 零成本 EH: 受保护调用的操作数必须计入使用次数。
        InstKind::Invoke { callee, args, .. } => {
            count += op_count_in_op(callee, name);
            for a in args {
                count += op_count_in_op(a, name);
            }
        },
        InstKind::MemberLoad { obj, .. } => count += op_count_in_op(obj, name),
        InstKind::MemberStore { obj, src, .. } => {
            count += op_count_in_op(obj, name);
            count += op_count_in_op(src, name);
        },
        InstKind::Select { cond, then_, else_ } => {
            count += op_count_in_op(cond, name);
            count += op_count_in_op(then_, name);
            count += op_count_in_op(else_, name);
        },
        InstKind::InlineAsm { inputs, .. } => {
            for input in inputs {
                count += op_count_in_op(input, name);
            }
        },
        _ => {},
    }
    count
}

fn op_count_in_op(o: &Operand, name: &str) -> usize {
    if let Operand::Var(v) = o {
        if v.name() == name {
            return 1;
        }
    }
    0
}

fn operand_to_expr(o: &Operand, env: &HashMap<String, SsaVar>, _func: &Function) -> Expr {
    match o {
        Operand::Var(v) => {
            if let Some(found) = env.get(&v.name()) {
                ssa_ident_expr(&found.base, v)
            } else {
                ssa_ident_expr(&v.base, v)
            }
        },
        Operand::Int(v) => Expr::Int {
            v: *v,
            span: Span::point("ssa", 0, 0),
            // 整数常量在 SSA 里是**无类型**的 (`Operand::Int(i128)`), 重建时必须
            // 还原它的类型 —— 旧实现硬写 `Type::Int`, 于是
            // ① 立即数按 32 位物化, `10306098990L / 100L` 的高位被截成 1716164398;
            // ② `infer_binop_type` 从操作数类型推常见算术转换, 整个运算也跟着降格
            //    成 32 位记账 (与下面 `infer_binop_type` 的根因说明同一轴)。
            // 类型取「能表示该值的最小秩」(= C++ [lex.icon] 无符号后缀十进制字面量
            // 的量级规则): 32 位可表示时仍是 `Int`, 现有代码路径零变化; 只有原本就
            // 被截断的宽常量才升秩。
            ty: int_const_ty(*v),
        },
        Operand::Float(c, ty) => Expr::Float {
            c: *c,
            span: Span::point("ssa", 0, 0),
            ty: ty.clone(),
        },
        Operand::Str(d) => Expr::String {
            data: d.clone(),
            span: Span::point("ssa", 0, 0),
            ty: Type::Array(
                Box::new(QualType::new(Type::Char)),
                crate::ast::ArraySize::Const(d.len()),
            ),
        },
        Operand::SymAddr(n, ty) => Expr::Ident {
            name: crate::ast::Ident {
                name: n.clone(),
                span: Span::point("ssa", 0, 0),
            },
            span: Span::point("ssa", 0, 0),
            ty: ty.clone(),
        },
        Operand::FnAddr(n, ty) => Expr::Ident {
            name: crate::ast::Ident {
                name: n.clone(),
                span: Span::point("ssa", 0, 0),
            },
            span: Span::point("ssa", 0, 0),
            ty: Type::Ptr(Box::new(QualType::new(ty.clone()))),
        },
        Operand::Void => Expr::Int {
            v: 0,
            span: Span::point("ssa", 0, 0),
            ty: Type::Void,
        },
        Operand::Call(_) => Expr::Int {
            v: 0,
            span: Span::point("ssa", 0, 0),
            ty: Type::Int,
        },
        Operand::Phi(_) => Expr::Int {
            v: 0,
            span: Span::point("ssa", 0, 0),
            ty: Type::Int,
        },
    }
}

/// 将 SSA 指令的 kind 转换为 AST 表达式 (右侧值)。
fn inst_to_expr(kind: &InstKind, env: &HashMap<String, SsaVar>, func: &Function) -> Expr {
    let sp = Span::point("ssa", 0, 0);
    match kind {
        InstKind::Bin { op, l, r } => {
            let l_expr = operand_to_expr(l, env, func);
            let r_expr = operand_to_expr(r, env, func);
            let binop_ty = infer_binop_type(op, &l_expr, &r_expr);
            Expr::Binop {
                op: *op,
                l: Box::new(l_expr),
                r: Box::new(r_expr),
                span: sp.clone(),
                ty: binop_ty,
            }
        },
        InstKind::Un { op, e } => Expr::Unary {
            op: *op,
            e: Box::new(operand_to_expr(e, env, func)),
            span: sp.clone(),
            // 与 `inst_to_expr_resolved` 共用同一推型规则（本函数目前无调用点，
            // 但两套规则一旦分叉，将来启用它就会拿到错类型）。
            ty: unary_result_ty(None, op, &match e {
                Operand::Var(v) => v.ty.clone(),
                Operand::Float(_, t) => t.clone(),
                Operand::SymAddr(_, t) => t.clone(),
                Operand::FnAddr(_, t) => t.clone(),
                Operand::Int(_) => Type::Int,
                _ => Type::Int,
            }),
        },
        InstKind::Cast { ty, e } => Expr::Cast {
            ty: ty.clone(),
            e: Box::new(operand_to_expr(e, env, func)),
            span: sp.clone(),
        },
        InstKind::AddrOf { base } => Expr::AddrOf {
            e: Box::new(Expr::Ident {
                name: crate::ast::Ident {
                    name: base.clone(),
                    span: sp.clone(),
                },
                span: sp.clone(),
                ty: Type::Void,
            }),
            span: sp.clone(),
            ty: addr_of_ty(None),
        },
        InstKind::Load { e, ty, .. } => Expr::Deref {
            e: Box::new(operand_to_expr(e, env, func)),
            span: sp.clone(),
            ty: ty.clone(),
        },
        InstKind::Store { e, src, .. } => {
            let addr = operand_to_expr(e, env, func);
            let dty = store_pointee_ty(&addr);
            Expr::Assign {
                op: BinOp::Add,
                l: Box::new(Expr::Deref {
                    e: Box::new(addr),
                    span: sp.clone(),
                    ty: dty,
                }),
                r: Box::new(operand_to_expr(src, env, func)),
                span: sp.clone(),
                ty: Type::Void,
            }
        },
        InstKind::IndexLoad { arr, idx, ty, .. } => Expr::Index {
            arr: Box::new(operand_to_expr(arr, env, func)),
            idx: Box::new(operand_to_expr(idx, env, func)),
            span: sp.clone(),
            ty: ty.clone(),
        },
        InstKind::IndexStore {
            arr, idx, src, ty, ..
        } => Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Index {
                arr: Box::new(operand_to_expr(arr, env, func)),
                idx: Box::new(operand_to_expr(idx, env, func)),
                span: sp.clone(),
                ty: ty.clone(),
            }),
            r: Box::new(operand_to_expr(src, env, func)),
            span: sp.clone(),
            ty: Type::Void,
        },
        InstKind::Call {
            callee,
            args,
            ret_ty,
        } => Expr::Call {
            callee: Box::new(operand_to_expr(callee, env, func)),
            args: args.iter().map(|a| operand_to_expr(a, env, func)).collect(),
            span: sp.clone(),
            ty: ret_ty.clone(),
        },
        // C++ 零成本 EH: 同 `inst_to_expr_resolved`, 受保护调用的正常路径就是
        // 一次普通调用; 落地垫序言由 `rebuild_bb` 拦截, 此处仅占位。
        InstKind::Invoke {
            callee,
            args,
            ret_ty,
            ..
        } => Expr::Call {
            callee: Box::new(operand_to_expr(callee, env, func)),
            args: args.iter().map(|a| operand_to_expr(a, env, func)).collect(),
            span: sp.clone(),
            ty: ret_ty.clone(),
        },
        InstKind::EhLandingPad { .. } => Expr::Int {
            v: 0,
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::MemberAddr {
            obj,
            obj_ty,
            field,
            ty,
            is_arrow,
            ..
        } => {
            // 子对象地址: 装回 `&obj->field` / `&(obj.field)`, 偏移交给 codegen
            // 现有的地址形态（与 `MemberLoad` 单一真源同一约定）。
            let mut base = operand_to_expr(obj, env, func);
            if !*is_arrow {
                set_expr_ty_if_compatible(&mut base, obj_ty);
            }
            Expr::AddrOf {
                e: Box::new(Expr::Member {
                    expr: Box::new(base),
                    field: crate::ast::Ident {
                        name: field.clone(),
                        span: sp.clone(),
                    },
                    span: sp.clone(),
                    ty: ty.clone(),
                    is_arrow: *is_arrow,
                }),
                span: sp.clone(),
                ty: Type::Ptr(Box::new(crate::ast::QualType::new(ty.clone()))),
            }
        },
        InstKind::MemberLoad {
            obj,
            obj_ty,
            field,
            ty,
            is_arrow,
            ..
        } => {
            let mut base = operand_to_expr(obj, env, func);
            if *is_arrow {
                // obj 本身就是结构体指针: 保留其指针类型, 重建为 ptr->field
            } else {
                set_expr_ty_if_compatible(&mut base, obj_ty);
            }
            Expr::Member {
                expr: Box::new(base),
                field: crate::ast::Ident {
                    name: field.clone(),
                    span: sp.clone(),
                },
                span: sp.clone(),
                ty: ty.clone(),
                is_arrow: *is_arrow,
            }
        },
        InstKind::MemberStore {
            obj,
            obj_ty,
            field,
            src,
            ty,
            is_arrow,
            ..
        } => {
            let mut base = operand_to_expr(obj, env, func);
            if *is_arrow {
                // obj 本身就是结构体指针: 保留其指针类型
            } else {
                set_expr_ty_if_compatible(&mut base, obj_ty);
            }
            Expr::Assign {
                op: BinOp::Add,
                l: Box::new(Expr::Member {
                    expr: Box::new(base),
                    field: crate::ast::Ident {
                        name: field.clone(),
                        span: sp.clone(),
                    },
                    span: sp.clone(),
                    ty: ty.clone(),
                    is_arrow: *is_arrow,
                }),
                r: Box::new(operand_to_expr(src, env, func)),
                span: sp.clone(),
                // C11 6.5.16p3: 赋值表达式的类型 = 左操作数的类型
                ty: ty.clone(),
            }
        },
        InstKind::Select { cond, then_, else_ } => Expr::Cond {
            cond: Box::new(operand_to_expr(cond, env, func)),
            then_: Box::new(operand_to_expr(then_, env, func)),
            else_: Box::new(operand_to_expr(else_, env, func)),
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::InlineAsm {
            asm_str,
            outputs,
            output_names,
            output_constraints,
            inputs,
            input_names,
            input_constraints,
            clobbers,
            volatile,
            ty,
            ..
        } => Expr::AsmExpr {
            asm_str: asm_str.clone(),
            outputs: outputs.clone(),
            output_names: output_names.clone(),
            output_constraints: output_constraints.clone(),
            inputs: inputs
                .iter()
                .take(input_names.len())
                .map(|a| operand_to_expr(a, env, func))
                .collect(),
            input_names: input_names.clone(),
            input_constraints: input_constraints.clone(),
            clobbers: clobbers.clone(),
            volatile: *volatile,
            span: sp.clone(),
            ty: ty.clone(),
        },
        InstKind::PreservedDecl { .. } => {
            // Handled separately in rebuild; should not reach here
            Expr::Int {
                v: 0,
                span: sp.clone(),
                ty: Type::Int,
            }
        },
    }
}

// ============================================================
// Pass 工具: 打印 (调试用)
// ============================================================

pub fn dump_function(func: &Function) -> String {
    let mut s = String::new();
    s.push_str(&format!("fn {}: {{\n", func.name));
    for b in &func.block_order {
        if let Some(bb) = func.blocks.get(b) {
            s.push_str(&format!("  {}: preds={:?} succs={:?}\n", bb.name, bb.preds, bb.succs));
            for phi in &bb.phis {
                s.push_str(&format!("    phi {} <- {:?}\n", phi.dest.name(), phi.incoming));
            }
            for inst in &bb.insts {
                let dst = inst
                    .dest
                    .as_ref()
                    .map(|d| d.name())
                    .unwrap_or_else(|| "_".to_string());
                s.push_str(&format!("    {} = {:?}\n", dst, inst.kind));
            }
            s.push_str(&format!("    term: {:?}\n", bb.term));
        }
    }
    s.push_str("}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Span;

    /// 手搭一个「二路分支的其中一条路径本身又是二路分支」的 CFG —— 嵌套三元
    /// (`a ? b : c ? d : e`)、三元分支里的 `&&`/`||`、以及语句级
    /// `if (x) {...} else { if (y) {...} }` 降级后都是这个形状。
    ///
    /// ```text
    /// entry: if c1 -> A, B
    /// A:     goto M
    /// B:     if c2 -> B1, B2      <- 汇聚点不在「唯一前驱+无条件跳转」链上
    /// B1:    goto M2
    /// B2:    goto M2
    /// M2:    goto M
    /// M:     goto E
    /// E:     return
    /// ```
    fn nested_branch_cfg() -> Function {
        let span = Span::point("t.c", 1, 1);
        let mut f = Function::new("f".into(), Type::Int, span);
        for b in ["A", "B", "B1", "B2", "M", "M2", "E"] {
            f.add_block(b.to_string());
        }
        let c = Operand::Int(1);
        f.blocks.get_mut("entry").unwrap().term = Terminator::Branch {
            cond: c.clone(),
            t: "A".into(),
            f: "B".into(),
        };
        f.blocks.get_mut("A").unwrap().term = Terminator::Jump("M".into());
        f.blocks.get_mut("B").unwrap().term = Terminator::Branch {
            cond: c,
            t: "B1".into(),
            f: "B2".into(),
        };
        f.blocks.get_mut("B1").unwrap().term = Terminator::Jump("M2".into());
        f.blocks.get_mut("B2").unwrap().term = Terminator::Jump("M2".into());
        f.blocks.get_mut("M2").unwrap().term = Terminator::Jump("M".into());
        f.blocks.get_mut("M").unwrap().term = Terminator::Jump("E".into());
        f.blocks.get_mut("E").unwrap().term = Terminator::Ret { val: Some(Operand::Int(0)) };
        f
    }

    /// 后支配者必须把嵌套分支的凝聚点判对: `entry` 的凝聚点是 `M` (而不是内层
    /// 的 `M2`), `B` 的是 `M2`。`branch_join` 的旧启发式从 `B` 出发直接放弃
    /// (它是个二分支), 因此 `entry` 这一层只能靠后支配者兜底 —— 判错或直接
    /// 判空都会让 `M` 被内联进一条路径, `If` 之后的代码在另一条路径上丢失。
    #[test]
    fn ipdom_and_branch_join_find_nested_joins() {
        let f = nested_branch_cfg();
        let ip = compute_ipdoms(&f);
        assert_eq!(ip.get("entry").map(|s| s.as_str()), Some("M"), "entry 的后支配者应为 M");
        assert_eq!(ip.get("B").map(|s| s.as_str()), Some("M2"), "B 的后支配者应为 M2");
        assert_eq!(branch_join("entry", "A", "B", &f).as_deref(), Some("M"));
        assert_eq!(branch_join("B", "B1", "B2", &f).as_deref(), Some("M2"));
    }

    #[test]
    fn simple_function_builds_ssa() {
        let span = Span::point("t.c", 1, 1);
        let f = FuncDecl {
            ret_ty: Type::Int,
            name: crate::ast::Ident {
                name: "f".into(),
                span: span.clone(),
            },
            params: vec![],
            body: Some(Block {
                items: vec![
                    BlockItem::Decl(Decl::Var(crate::ast::VarDecl {
                        ty: Type::Int,
                        name: crate::ast::Ident {
                            name: "x".into(),
                            span: span.clone(),
                        },
                        init: Some(Expr::Int {
                            v: 1,
                            span: span.clone(),
                            ty: Type::Int,
                        }),
                        span: span.clone(),
                        is_extern: false,
                        is_static: false,
                        section: None,
                        alignment: None,
                        is_weak: false,
                        visibility: None,
                        is_unused: false,
                        register_name: None,
                        cleanup_fn: None,
                        is_thread_local: false,
                        no_sanitize: vec![],
                        alias_name: None,
                        copy_fn: None,
                        is_retain: false,
                        is_used_attr: false,
                        quals: crate::ast::QualifierFlags::NONE,
                        addr_space: crate::ast::AddrSpace::None,
                        tls_model: None,
                        asm_label: None,
                    })),
                    BlockItem::Stmt(Stmt::Return {
                        val: Some(Expr::Ident {
                            name: crate::ast::Ident {
                                name: "x".into(),
                                span: span.clone(),
                            },
                            span: span.clone(),
                            ty: Type::Int,
                        }),
                        span: span.clone(),
                    }),
                ],
                span: span.clone(),
            }),
            span: span.clone(),
            is_extern: false,
            is_static: false,
            section: None,
            alignment: None,
            is_weak: false,
            visibility: None,
            is_unused: false,
            is_cold: false,
            is_hot: false,
            is_noinline: false,
            is_always_inline: false,
            is_inline: false,
            is_variadic: false,
            is_constructor: false,
            is_destructor: false,
            alias_name: None,
            is_nested: false,
            upvars: vec![],
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None,
            dtor_priority: None,
            is_retain: false,
            is_used_attr: false,
            is_flatten: false,
            error_msg: None,
            warning_msg: None,
            deprecated_msg: None,
            ifunc_resolver: None,
            asm_output_template: None,
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
            is_noreturn: false,
            is_pure: false,
            is_const_attr: false,
            returns_twice: false,
            no_instrument: false,
            is_malloc: false,
            malloc_dealloc: None,
            malloc_dealloc_argno: None,
            alloc_size: None,
            assume_aligned: None,
            warn_unused_result: false,
            is_sentinel: false,
            format_arg_idx: None,
            is_nodiscard: false,
            asm_label: None,
        };
        let func = build_from_func(&f).expect("structured");
        assert!(func.blocks.contains_key("entry"));
        assert_eq!(func.params.len(), 0);
    }

    #[test]
    fn if_else_structured() {
        let span = Span::point("t.c", 1, 1);
        let f = FuncDecl {
            ret_ty: Type::Int,
            name: crate::ast::Ident {
                name: "f".into(),
                span: span.clone(),
            },
            params: vec![],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::If {
                    cond: Expr::Int {
                        v: 1,
                        span: span.clone(),
                        ty: Type::Int,
                    },
                    then_: Box::new(Stmt::Return {
                        val: Some(Expr::Int {
                            v: 1,
                            span: span.clone(),
                            ty: Type::Int,
                        }),
                        span: span.clone(),
                    }),
                    else_: Some(Box::new(Stmt::Return {
                        val: Some(Expr::Int {
                            v: 0,
                            span: span.clone(),
                            ty: Type::Int,
                        }),
                        span: span.clone(),
                    })),
                    span: span.clone(),
                })],
                span: span.clone(),
            }),
            span: span.clone(),
            is_extern: false,
            is_static: false,
            section: None,
            alignment: None,
            is_weak: false,
            visibility: None,
            is_unused: false,
            is_cold: false,
            is_hot: false,
            is_noinline: false,
            is_always_inline: false,
            is_inline: false,
            is_variadic: false,
            is_constructor: false,
            is_destructor: false,
            alias_name: None,
            is_nested: false,
            upvars: vec![],
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None,
            dtor_priority: None,
            is_retain: false,
            is_used_attr: false,
            is_flatten: false,
            error_msg: None,
            warning_msg: None,
            deprecated_msg: None,
            ifunc_resolver: None,
            asm_output_template: None,
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
            is_noreturn: false,
            is_pure: false,
            is_const_attr: false,
            returns_twice: false,
            no_instrument: false,
            is_malloc: false,
            malloc_dealloc: None,
            malloc_dealloc_argno: None,
            alloc_size: None,
            assume_aligned: None,
            warn_unused_result: false,
            is_sentinel: false,
            format_arg_idx: None,
            is_nodiscard: false,
            asm_label: None,
        };
        let func = build_from_func(&f).expect("structured");
        assert!(func.blocks.contains_key("if.then"));
        assert!(func.blocks.contains_key("if.else"));
        assert!(func.blocks.contains_key("if.end"));
    }

    #[test]
    fn goto_bails_out() {
        let span = Span::point("t.c", 1, 1);
        let f = FuncDecl {
            ret_ty: Type::Int,
            name: crate::ast::Ident {
                name: "f".into(),
                span: span.clone(),
            },
            params: vec![],
            body: Some(Block {
                items: vec![BlockItem::Stmt(Stmt::Goto {
                    target: crate::ast::Ident {
                        name: "L".into(),
                        span: span.clone(),
                    },
                    span: span.clone(),
                })],
                span: span.clone(),
            }),
            span: span.clone(),
            is_extern: false,
            is_static: false,
            section: None,
            alignment: None,
            is_weak: false,
            visibility: None,
            is_unused: false,
            is_cold: false,
            is_hot: false,
            is_noinline: false,
            is_always_inline: false,
            is_inline: false,
            is_variadic: false,
            is_constructor: false,
            is_destructor: false,
            alias_name: None,
            is_nested: false,
            upvars: vec![],
            is_old_style: false,
            format_kind: None,
            format_string_idx: None,
            format_first_arg: None,
            no_sanitize: vec![],
            target_clones: vec![],
            returns_nonnull: false,
            ctor_priority: None,
            dtor_priority: None,
            is_retain: false,
            is_used_attr: false,
            is_flatten: false,
            error_msg: None,
            warning_msg: None,
            deprecated_msg: None,
            ifunc_resolver: None,
            asm_output_template: None,
            is_global: false,
            is_device: false,
            is_host: false,
            is_naked: false,
            is_noreturn: false,
            is_pure: false,
            is_const_attr: false,
            returns_twice: false,
            no_instrument: false,
            is_malloc: false,
            malloc_dealloc: None,
            malloc_dealloc_argno: None,
            alloc_size: None,
            assume_aligned: None,
            warn_unused_result: false,
            is_sentinel: false,
            format_arg_idx: None,
            is_nodiscard: false,
            asm_label: None,
        };
        assert!(build_from_func(&f).is_none());
    }

    /// SSA→AST 重建的「单次使用即内联」命名契约。
    ///
    /// 这条规则直接决定循环条件/短路结果能不能正确工作:
    /// 内联意味着值不在每次读时重算, 所以**只有**编译器合成、全局唯一、
    /// 从不被用户代码引用的 base 才能享受它。`lower_ir::fresh_logical_base`
    /// 产的短路结果变量 (`dcc_land<N>` / `dcc_lor<N>`) 故意**不**符合本形状:
    /// 它们在 merge 块被读一次, 但必须在两条分支里各写一次, 一旦被内联进
    /// 使用点就会丢掉赋值 (历史缺陷: `a==2 || a==3` 恒为假)。
    #[test]
    fn lower_ir_temp_base_recognizes_only_fresh_syn_names() {
        // `fresh_syn` 形状: `dcc_t<序号>_<后缀>`
        assert!(is_lower_ir_temp_base("dcc_t1_field"));
        assert!(is_lower_ir_temp_base("dcc_t42_sym_addr"));
        assert!(is_lower_ir_temp_base("dcc_t7_call_result"));
        // 序号位必须是纯数字, 且必须带 `_后缀` 分隔符:
        assert!(!is_lower_ir_temp_base("dcc_ttmp"));
        assert!(!is_lower_ir_temp_base("dcc_t1"));
        assert!(!is_lower_ir_temp_base("dcc_table_x"));
        // C 前端 (`lower.rs`) 的 `_t<N>_<base>` 是另一套命名, 不由本函数识别:
        assert!(!is_lower_ir_temp_base("_t3_add"));
        // 短路结果变量是**具名局部**, 必须走真实的声明+赋值路径:
        assert!(!is_lower_ir_temp_base("dcc_land1"));
        assert!(!is_lower_ir_temp_base("dcc_lor2"));
        // 用户/符号名一律不匹配:
        assert!(!is_lower_ir_temp_base(""));
        assert!(!is_lower_ir_temp_base("i"));
        assert!(!is_lower_ir_temp_base("__dcc_static_0"));
    }

    /// 取第一个函数并试 SSA 化；`None` = 该函数不进管线（留在 AST 通道上）。
    fn try_build(src: &str, name: &str) -> Option<Function> {
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        for decl in prog.decls.iter() {
            if let crate::ast::Decl::Func(f) = decl {
                if f.name.name == name {
                    return build_from_func(f);
                }
            }
        }
        panic!("找不到函数 {name}");
    }

    /// 子对象地址现在**有 IR 表示**（`InstKind::MemberAddr`）：`p->fld[i]`（fld 是零长/
    /// 柔性数组）的基址 = 对象地址 + 字段偏移。以前 IR 表达不了，只能整函数退 SSA
    /// （§7.29），本节把「退路」的断言换成「表示存在」的断言。
    ///
    /// 为什么不只断言「能进 SSA」：退路删掉后，发一个看起来能跑的错形状也能做绿。
    /// 所以这里钉「IR 里确实出现了 `MemberAddr`」，而**值对不对**交给
    /// `vla_and_flexible_array_match_gcc_at_all_levels` 拿 gcc 实测地面真值逐档对照。
    /// 读与写两侧都钉（历史上「只挡一边」重复错过两次）。
    #[test]
    fn subobject_array_decay_is_represented_as_member_addr() {
        fn n_member_addr(src: &str, fname: &str) -> usize {
            let f = try_build(src, fname).expect("子对象下标应能进 SSA");
            f.blocks
                .values()
                .flat_map(|b| b.insts.iter())
                .filter(|i| matches!(i.kind, InstKind::MemberAddr { .. }))
                .count()
        }

        let zero_len = "struct Z { int count; int data[0]; };\n\
                        int t(struct Z* z){ return z->data[0] + z->data[2]; }\n";
        assert!(
            n_member_addr(zero_len, "t") >= 1,
            "零长数组成员的下标基址必须走 MemberAddr"
        );
        // 条数不钉死：构建期不做 CSE，`data[0] + data[2]` 现在发两次衰减（实测 2 条），
        // 以后的 pass 把它合成一条不是错。只钉「有没有」与「不该有的地方没有」。
        let flex = "struct F { int count; int data[]; };\n\
                    int t(struct F* f){ f->data[1] = 7; return f->data[1]; }\n";
        assert!(
            n_member_addr(flex, "t") >= 1,
            "柔性数组成员的**写**同样要拿到子对象地址（与读同一机制，不能只做一边）"
        );
        // `&成员` 取址（不是数组字段）也必须能表达：这是 §7.29 同一缺口的另一侧。
        let addr_of_scalar_member = "struct P { int a; int b; };\n\
                                     int* t(struct P* p){ return &p->b; }\n";
        assert!(
            n_member_addr(addr_of_scalar_member, "t") >= 1,
            "&p->b 要发 MemberAddr，而不是把字段当值读"
        );

        let fixed_2d = "int t(void){ int a[3][4]; a[2][1] = 5; return a[2][1]; }\n";
        assert!(
            try_build(fixed_2d, "t").is_some(),
            "固定大小二维数组仍应走 SSA 往返"
        );
        let ptr_elem = "int t(int* p){ p[2] = 9; return p[2]; }\n";
        assert!(
            try_build(ptr_elem, "t").is_some(),
            "指针下标（无需衰减）不得被误挡"
        );
        assert_eq!(
            n_member_addr(ptr_elem, "t"),
            0,
            "指针下标不应凭空多造一条 MemberAddr（反向对照）"
        );
    }

    /// 「一个 `while` 只有一个落出口」这条限制到底挡住了多少函数：判据过宽会把
    /// 普通循环一起挡掉（SSA 覆盖面大幅丢失），过窄则挡不住真歧义。两边都钉，
    /// 因为它们各对应一类历史错误（先例：「续体被外层共享」那条判据实测会挡掉
    /// **所有** for 循环 —— 普通 for 的 `for.end` 本来就同时是 preheader 的假边目标）。
    #[test]
    fn multi_exit_cycle_predicate_is_narrow_and_sound() {
        // 不挡：普通 for、只有 continue 的循环（出口只有 for.end）
        let plain_for = "long s(long n){long acc=0;for(long i=0;i<n;i++)acc+=i*3;return acc+n;}\n";
        assert!(
            !has_multi_exit_cycle(&try_build(plain_for, "s").expect("普通 for 应能进 SSA")),
            "普通 for 循环不该被判为多出口（P12-B 的验收夹具就是这个形状）"
        );
        let cont_only =
            "long s(long n){long acc=0;for(long i=0;i<n;i++){if(i%2==0)continue;acc+=i;}return acc;}\n";
        assert!(
            !has_multi_exit_cycle(&try_build(cont_only, "s").expect("continue 循环应能进 SSA")),
            "只有 continue 的循环不该被判为多出口"
        );

        // 要挡：体内提前 return / break 与正常落出是两个不同的循环外后继
        let early_return = "int a(int n){for(int i=0;i<n;i++){if(i==2)return 7;}return 1;}\n";
        assert!(
            has_multi_exit_cycle(&try_build(early_return, "a").expect("早退循环应能建 IR")),
            "体内 return + 循环落出 = 两个出口，必须判为多出口"
        );
        let ret_and_break =
            "int a(int n){int s=0;for(int i=0;i<n;i++){if(i==1)return 9;if(i==3)break;s+=i;}return s;}\n";
        assert!(
            has_multi_exit_cycle(&try_build(ret_and_break, "a").expect("应能建 IR")),
            "return 与 break 同现时更要判为多出口"
        );
    }

    /// 无法在 SSA 里忠实表达的表达式必须让**整个函数**放弃 SSA 化，而不是静默
    /// 返 `Operand::Int(0)`（那条路径曾在 -O2 产生一批错值，见 `Builder::unsupported`）。
    ///
    /// 反向对照同样重要：不带该形态的同一函数必须能进管线，否则「所有函数全部
    /// 放弃 SSA」也能把测试做绿（那其实就是「SSA 整层空转」那种假绿）。
    #[test]
    fn unlowerable_expr_bails_out_of_ssa_instead_of_becoming_zero() {
        assert!(try_build("int a(void){ return (int[]){1,2,3}[1]; }\n", "a").is_none(), "复合字面量不可降级");
        assert!(try_build("int b(void){ int x[4]; int *p = &x[2]; return (int)(long)p; }\n", "b").is_none(), "&下标 需地址算术");
        assert!(try_build("int c(void){ int x[4]; x[2] = 9; return x[2]; }\n", "c").is_some(), "对照组必须仍进 SSA 管线");
    }

    /// 逗号表达式的左操作数只为副作用存在，但必须照常降级：旧实现直接丢掉 `l`,
    /// 于是 `x = (g(), 1)` 里的调用在产物里消失。这条测试钉住两个面：
    /// 函数仍能进 SSA（逗号不是放弃理由）且 IR 里真有那一次调用。
    #[test]
    fn comma_keeps_the_left_operand_side_effect() {
        let f = try_build("int g(void); int f(void){ int x = (g(), 1); return x; }\n", "f")
            .expect("逗号表达式不应成为放弃 SSA 的理由");
        let calls = f
            .blocks
            .values()
            .flat_map(|bb| bb.insts.iter())
            .filter(|i| matches!(&i.kind, InstKind::Call { callee, .. } if matches!(callee, Operand::SymAddr(n, _) if n == "g")))
            .count();
        assert_eq!(calls, 1, "必须保留逗号左侧的那一次调用");
    }

    /// 三元分支带副作用/可陷获时不得降级成 `Select`（它两侧都执行），而要让整个
    /// 函数放弃 SSA 化。反向对照：纯分支必须仍走 Select，否则「一律放弃」也能绿。
    #[test]
    fn ternary_with_side_effect_branches_bails_out() {
        assert!(
            try_build("int t1(void){ int x = 3, y = 2; return (x < y ? x++ : y++); }\n", "t1")
                .is_none(),
            "两侧自增不得被无条件执行"
        );
        assert!(
            try_build("int t3(int c, int *p){ return c ? *p : 0; }\n", "t3").is_none(),
            "分支里的读内存不得被提前执行"
        );
        let f = try_build("int t2(int c,int a,int b){ return c ? a + 1 : b + 2; }\n", "t2")
            .expect("纯三元表达式仍应进 SSA 管线");
        assert!(
            f.blocks
                .values()
                .any(|bb| bb.insts.iter().any(|i| matches!(i.kind, InstKind::Select { .. }))),
            "纯分支仍降级成 Select（否则本判据只是在关掉整层优化）"
        );
    }

    /// 不被认识的赋值左值必须让整函数放弃 SSA 化，而不是“只做不写”。
    ///
    /// `__real__ z = r` 这类左值不在 `lower_lvalue` 的形态表里，旧实现于是
    /// 什么都不发、只把 RHS 当表达式值返回 —— 两条写指令在 IR 里根本不存在，
    /// `double _Complex z; __real__ z=r; __imag__ z=i; return z;` 在 -O2 返回未初始化数据。
    /// 反向对照：普通局部赋值与 `p->a = v` 这类受支持的左值仍须进管线。
    #[test]
    fn unrecognised_assign_lvalue_bails_instead_of_dropping_the_store() {
        assert!(
            try_build(
                "double _Complex f(void){ double _Complex z; __real__ z = 1; __imag__ z = 2; return z; }\n",
                "f"
            )
            .is_none(),
            "复数字段赋值不能被静默丢弃"
        );
        let ok = try_build("struct S{double a;}; double g(struct S *p){ p->a = 1; return p->a; }\n", "g")
            .expect("受支持的左值仍应进 SSA 管线");
        assert!(
            ok.blocks.values().any(|bb| bb
                .insts
                .iter()
                .any(|i| matches!(i.kind, InstKind::MemberStore { .. }))),
            "成员赋值必须真的发射成一条 MemberStore"
        );
    }

    /// 聚合对象的整体赋值在 IR 里没有表示，必须让整个函数退出 SSA，而不是只搬
    /// 一半字节。实测 -O2 旧行为：`struct P2 { long a, b; } g; g = p;` 之后
    /// `g.b` 读到 0 —— 那条 `Store` 重建后是 `*(&g) = p`，后端按标量搬 8 字节。
    /// 同一份代码里 `struct P2 t = p;` 是对的（它走 `lower_expr_into`，不走 Store），
    /// 所以坏的不是「结构体」而是**给具名对象整体赋值**这条形态。
    /// 反向对照：成员赋值 `g.b = v` 必须仍进管线（不能一刀切退出）。
    #[test]
    fn aggregate_assignment_to_named_object_bails() {
        assert!(
            try_build(
                "struct P { long a; long b; }; struct P g; void f(struct P p){ g = p; }\n",
                "f"
            )
            .is_none(),
            "聚合整体赋值不得降级成一次标量 Store"
        );
        assert!(
            try_build("struct P { long a; long b; }; struct P g; void f(long v){ g.b = v; }\n", "f")
                .is_some(),
            "成员赋值是受支持的形态，不得一起退出 SSA 管线"
        );
    }

    /// 复数算术的重建类型必须是复数（关系运算仍是 int）。
    ///
    /// codegen 靠类型选 `gen_complex_binop`；`infer_binop_type` 少了复数分支时，
    /// `x + y`（两侧都是 `double _Complex`）会掉到 Int 回退，复数加法退化成
    /// 标量运算（实测 -O2 得 (1.0e9, 0)，gcc 与 -O0 都是 (6, 8)）。
    #[test]
    fn infer_binop_type_keeps_complex_results() {
        let sp = Span::point("t.c", 1, 1);
        let cx = || Expr::Ident {
            name: crate::ast::Ident { name: "x".into(), span: sp.clone() },
            span: sp.clone(),
            ty: Type::Complex(Box::new(QualType::new(Type::Double))),
        };
        let dbl = || Expr::Ident {
            name: crate::ast::Ident { name: "y".into(), span: sp.clone() },
            span: sp.clone(),
            ty: Type::Double,
        };
        for op in [BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div] {
            let t = infer_binop_type(&op, &cx(), &cx());
            assert!(
                matches!(&t, Type::Complex(q) if matches!(q.base, Type::Double)),
                "{:?} 的复数运算结果必须是 _Complex double，实得 {:?}",
                op,
                t
            );
            // 复数与实数混合运算得同宽度的复数 (C11 6.3.1.8)。
            assert!(
                matches!(infer_binop_type(&op, &cx(), &dbl()), Type::Complex(_)),
                "{:?} 复数+实数应仍为复数",
                op
            );
        }
        // 关系/相等运算的结果仍是 int（上面必须早于复数分支）。
        assert!(
            matches!(infer_binop_type(&BinOp::EqEq, &cx(), &cx()), Type::Int),
            "复数相等运算结果必须是 int"
        );
    }
}

/// 一元指令重建必须把结果类型定成指令 `dest` 的类型。
///
/// 旧实现只认向量类型，其余一律写 `Type::Int`，于是 `double n = -b;` 重建后成了
/// `Unary{ty: Int}`，codegen 靠 `e.ty()` 判走不走 xorpd 的分支落回整数 `neg r64`
/// ⇒ 浮点取负直接丢失（实测 -O2 打印 `0.000000`，真值 `-2.300000`）。
#[cfg(test)]
mod unary_rebuild_ty_tests {
    use super::*;

    fn un(op: crate::ast::UnOp, inner_ty: Type) -> InstKind {
        let sp = Span::point("t.c", 1, 1);
        InstKind::Un {
            op,
            e: Operand::Var(SsaVar {
                base: "b".into(),
                version: 1,
                ty: inner_ty,
                span: sp,
            }),
        }
    }

    fn rebuilt(kind: &InstKind, dest_ty: Option<&Type>) -> Expr {
        let sp = Span::point("t.c", 1, 1);
        let f = Function::new("f".into(), Type::Int, sp.clone());
        inst_to_expr_resolved(kind, &HashMap::new(), &sp, &f, dest_ty)
    }

    #[test]
    fn float_negation_keeps_double_and_int_keeps_int() {
        let k = un(crate::ast::UnOp::Neg, Type::Double);
        assert!(
            matches!(&rebuilt(&k, Some(&Type::Double)), Expr::Unary { ty, .. } if matches!(ty, Type::Double)),
            "double 取负的结果类型不得退化成 Int：{:?}",
            rebuilt(&k, Some(&Type::Double))
        );
        let ki = un(crate::ast::UnOp::Neg, Type::Int);
        assert!(
            matches!(&rebuilt(&ki, Some(&Type::Int)), Expr::Unary { ty, .. } if matches!(ty, Type::Int)),
            "整数取负仍得 Int"
        );
        // `!x` 在 C 里结果永远是 int，不得因为操作数是 double 就把结果定成 double。
        let kn = un(crate::ast::UnOp::Not, Type::Double);
        assert!(
            matches!(&rebuilt(&kn, Some(&Type::Int)), Expr::Unary { ty, .. } if matches!(ty, Type::Int)),
            "逻辑非必须回到 int：{:?}",
            rebuilt(&kn, Some(&Type::Int))
        );
        // dest 缺失时按操作数类型推（不能默认成 Int，否则浮点又丢一次）。
        assert!(
            matches!(&rebuilt(&k, None), Expr::Unary { ty, .. } if matches!(ty, Type::Double)),
            "无 dest 时按操作数类型推"
        );
    }
}
