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

/// SSA 变量名: `"x_3"` 表示 `x` 的第 4 个版本 (下标从 0 起)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsaVar {
    pub base: String,
    pub version: u32,
    pub ty: Type,
    pub span: Span,
}

impl SsaVar {
    pub fn name(&self) -> String {
        if self.version == 0 {
            self.base.clone()
        } else {
            format!("{}_{}", self.base, self.version)
        }
    }
}

/// SSA 指令的右侧操作数: SSA 变量 / 常量 / 表达式结果
#[derive(Debug, Clone)]
pub enum Operand {
    Var(SsaVar),
    Int(i128),
    Float(f64, Type),
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
    /// `dest = arr[idx]`
    IndexLoad {
        arr: Operand,
        idx: Operand,
        /// volatile 访问: 不可被 DCE/DSE 消除或重排序
        is_volatile: bool,
    },
    /// `arr[idx] = src`
    IndexStore {
        arr: Operand,
        idx: Operand,
        src: Operand,
        /// volatile 访问: 不可被 DCE/DSE 消除或重排序
        is_volatile: bool,
    },
    /// `dest = call(callee, args)`
    Call {
        callee: Operand,
        args: Vec<Operand>,
        ret_ty: Type,
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

impl Default for OrderedNameSet {
    fn default() -> Self {
        Self {
            names: Vec::new(),
            seen: HashSet::new(),
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
            span,
            original_locals: OrderedNameSet::new(),
            local_types: HashMap::new(),
            local_quals: HashMap::new(),
            param_quals: HashMap::new(),
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
fn term_succs<'a>(term: &'a Terminator) -> Vec<&'a str> {
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
fn collect_all_preds(func: &Function) -> HashMap<String, Vec<String>> {
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
}

impl Builder {
    fn new(func: Function) -> Self {
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

    /// 处理表达式并将结果写入目标 SSA 变量 (避免创建不必要的临时变量)
    fn lower_expr_into(&mut self, e: &Expr, target: SsaVar) -> Operand {
        self.lower_expr_inner(e, Some(target))
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
            Expr::Float { v, ty, .. } => {
                if let Some(t) = target {
                    self.emit(Inst {
                        dest: Some(t.clone()),
                        kind: InstKind::Bin {
                            op: BinOp::Add,
                            l: Operand::Float(*v, ty.clone()),
                            r: Operand::Float(0.0, ty.clone()),
                        },
                        span: t.span.clone(),
                    });
                    self.define(t.clone());
                    return Operand::Var(t);
                }
                Operand::Float(*v, ty.clone())
            },
            Expr::String { data, .. } => Operand::Str(data.clone()),
            Expr::Char { v, .. } => Operand::Int(*v),
            Expr::Ident { name, span, ty } => {
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
                // 未定义变量: 视为全局 (codegen fallback)
                Operand::SymAddr(name.name.clone(), Type::Ptr(Box::new(QualType::new(Type::Void))))
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
                ro
            },
            Expr::Cast { ty, e: inner, span } => {
                let o = self.lower_expr(inner);
                let dest = target.unwrap_or_else(|| self.fresh("cast", ty.clone(), span));
                self.emit(Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::Cast {
                        ty: ty.clone(),
                        e: o,
                    },
                    span: span.clone(),
                });
                self.define(dest.clone());
                Operand::Var(dest)
            },
            Expr::Cond {
                cond,
                then_,
                else_,
                span,
                ty,
            } => {
                let co = self.lower_expr(cond);
                let t = self.lower_expr(then_);
                let f = self.lower_expr(else_);
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
            Expr::Comma { l: _, r, span, ty } => {
                // 丢弃 lhs (在 SSA 化中已通过指令序列完成)
                let _ = span;
                let o = self.lower_expr(r);
                let _ = ty;
                o
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
                    None => Operand::Void,
                }
            },
            Expr::Index { arr, idx, span, ty } => {
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
                Operand::Void
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
                    // volatile 局部变量: 从内存 Load 当前值, 不走 SSA 版本
                    let (cur_op, addr_op) = if is_volatile {
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
                        let load_dest = self.fresh("vload", ty.clone(), span);
                        self.emit(Inst {
                            dest: Some(load_dest.clone()),
                            kind: InstKind::Load {
                                e: Operand::Var(addr.clone()),
                                ty: ty.clone(),
                                is_volatile: true,
                            },
                            span: span.clone(),
                        });
                        (Operand::Var(load_dest), Some(Operand::Var(addr)))
                    } else {
                        let cur_v = self.lookup(&name.name).unwrap_or_else(|| SsaVar {
                            base: name.name.clone(),
                            version: 0,
                            ty: ty.clone(),
                            span: span.clone(),
                        });
                        (Operand::Var(cur_v), None)
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
                    // store back (volatile path only)
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
                        version: self.next_version(&name.name),
                        ty: ty.clone(),
                        span: span.clone(),
                    };
                    // Non-volatile path: bind nv to new_dest via a copy
                    // (Bin + 0).  Without this, nv is defined but never
                    // assigned, so subsequent reads of the variable would
                    // reference an undefined SSA value.
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
                    // 表达式结果值:
                    //  - post 形式: 旧值 cur_op
                    //  - pre 形式:  新值 —— volatile 变量必须重新从内存 Load
                    //               (新值刚 Store 进去), 非 volatile 直接用 nv
                    let new_val_op = if is_pre {
                        if let Some(addr) = addr_op.clone() {
                            let rel = self.fresh("vreload", ty.clone(), span);
                            self.emit(Inst {
                                dest: Some(rel.clone()),
                                kind: InstKind::Load {
                                    e: addr,
                                    ty: ty.clone(),
                                    is_volatile: true,
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
                    Operand::Void
                }
            },
            _ => Operand::Void, // 简化: 复杂 expr (CompoundLit, StmtExpr, LabelAddr, VaArg) 降级
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
                // Override old cond_bb terminator: jump to body
                // (its instructions provide defs needed by body).
                if let Some(cb) = self.func.blocks.get_mut(&cond_bb) {
                    cb.term = Terminator::Jump(body_bb.clone());
                    if !cb.succs.contains(&body_bb) {
                        cb.succs.push(body_bb.clone());
                    }
                }
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
    if let Expr::Index { arr, idx, .. } = l {
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
    match s {
        Stmt::Block { b, .. } => collect_local_decls(&b.items, out, types, quals),
        Stmt::If { then_, else_, .. } => {
            collect_local_decls_stmt(then_, out, types, quals);
            if let Some(e) = else_ {
                collect_local_decls_stmt(e, out, types, quals);
            }
        },
        Stmt::While { body, .. } | Stmt::For { body, .. } | Stmt::Do { body, .. } => {
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
        _ => {},
    }
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
    for (latch, header) in &back_edges {
        // Collect loop body blocks: all blocks that can reach the latch
        // without going through the header (simplified: blocks dominated
        // by the header that can reach the latch).
        let loop_bbs = find_loop_body(func, header, latch);
        // Collect variables defined (assigned) in the loop body
        let mut body_defs: HashMap<String, SsaVar> = HashMap::new();
        for bb_name in &loop_bbs {
            if let Some(bb) = func.blocks.get(bb_name) {
                for inst in &bb.insts {
                    if let Some(d) = &inst.dest {
                        body_defs.insert(d.base.clone(), d.clone());
                    }
                }
            }
        }
        // For each variable defined in the loop body, check if it has
        // different versions at the header entry and latch exit.
        // If so, insert a phi node at the header.
        let header_bb = match func.blocks.get(header) {
            Some(b) => b,
            None => continue,
        };
        // Find the version of each variable used at the header
        let mut header_uses: HashMap<String, SsaVar> = HashMap::new();
        for inst in &header_bb.insts {
            collect_var_uses_inst(&inst.kind, &mut header_uses);
        }
        if let Terminator::Branch { cond, .. } = &header_bb.term {
            collect_var_uses_op(cond, &mut header_uses);
        }
        // Insert phi for each variable that is both used at the header
        // and redefined in the loop body
        let mut new_phis: Vec<Phi> = Vec::new();
        // body_defs 是 HashMap, 直接迭代会把随机序写进 bb.phis; 按名字排序保证可复现。
        let mut body_def_names: Vec<&String> = body_defs.keys().collect();
        body_def_names.sort();
        for base in body_def_names {
            let body_var = &body_defs[base];
            if let Some(header_var) = header_uses.get(base) {
                if header_var.name() != body_var.name() {
                    // Different versions: need a phi node
                    let phi_dest = SsaVar {
                        base: base.clone(),
                        version: body_var.version + 1,
                        ty: body_var.ty.clone(),
                        span: body_var.span.clone(),
                    };
                    new_phis.push(Phi {
                        dest: phi_dest,
                        incoming: vec![
                            (header_var.clone(), "__entry".to_string()),
                            (body_var.clone(), latch.clone()),
                        ],
                    });
                }
            }
        }
        if let Some(bb) = func.blocks.get_mut(header) {
            bb.phis.extend(new_phis);
        }
    }
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
/// 返回值按名字排序: 调用方会用它决定 phi 插入序, 直接 `HashSet::into_iter()`
/// 会把进程随机哈希序泄到下游。
fn find_loop_body(func: &Function, header: &str, latch: &str) -> Vec<String> {
    let mut body: HashSet<String> = HashSet::new();
    body.insert(header.to_string());
    if header == latch {
        // Self-loop: only the header itself
        let mut v: Vec<String> = body.into_iter().collect();
        v.sort();
        return v;
    }
    // Reverse DFS from latch, not going through header
    let mut stack = vec![latch.to_string()];
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
        InstKind::MemberLoad { obj, .. } | InstKind::MemberStore { obj, .. } => {
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
        Stmt::Label { stmt, .. } => {
            lower_switches_in_stmt(stmt);
        },
        _ => {},
    }
}

/// 将单个 switch 语句转换为 if-else 链.
/// 密集 case 值的 switch 不转换 (返回 None), 留给 SSA Builder 生成 SwitchTable.
fn lower_switch_stmt(s: &Stmt) -> Option<Stmt> {
    let Stmt::Switch {
        expr,
        cases,
        span: _,
    } = s
    else {
        return None;
    };
    if cases.is_empty() {
        return None;
    }
    // 密集 case 值也转换为 if-else 链 (SSA SwitchTable 重建仍有问题)
    // if is_dense_switch(cases) { return None; }

    // 分离 default case 和普通 case
    let mut normal_cases: Vec<&Case> = Vec::new();
    let mut default_case: Option<&Case> = None;
    for c in cases {
        if c.val.is_none() {
            default_case = Some(c);
        } else {
            normal_cases.push(c);
        }
    }

    // 从后向前构建 if-else 链
    // 最后是 default (如果有)
    let mut current_else: Option<Box<Stmt>> = default_case.map(|def| {
        Box::new(Stmt::Block {
            b: def.body.clone(),
            span: def.span.clone(),
        })
    });

    // 从后向前遍历普通 case，构建嵌套 if-else
    for case in normal_cases.iter().rev() {
        let case_val = case.val.as_ref().unwrap();
        // 构建条件: expr == case_val
        let cond = Expr::Binop {
            op: BinOp::EqEq,
            l: Box::new(expr.clone()),
            r: Box::new(case_val.clone()),
            span: case.span.clone(),
            ty: expr_ty(expr),
        };
        let then_stmt = Box::new(Stmt::Block {
            b: case.body.clone(),
            span: case.span.clone(),
        });
        let if_stmt = Stmt::If {
            cond,
            then_: then_stmt,
            else_: current_else.take(),
            span: case.span.clone(),
        };
        current_else = Some(Box::new(if_stmt));
    }

    // 最外层的 if 语句就是结果
    current_else.map(|s| *s)
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
        Expr::ComplexLit { ty, .. } => ty.clone(),
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

pub fn build_from_func(f: &FuncDecl) -> Option<Function> {
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
    let mut builder = Builder::new(func);
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

/// 把 SSA Function 重建为 AST Block (codegen 仍走 AST 路径)。
///
/// 策略:
///   1. 只声明原始局部变量 (来自 func.original_locals), 不声明 SSA 临时变量。
///   2. 单次使用的 SSA 临时变量在其使用处内联展开, 避免多余栈槽。
///   3. phi 节点在 merge BB 用对原变量赋值处理。
pub fn rebuild_block(func: &Function) -> Block {
    let mut items: Vec<BlockItem> = Vec::new();
    let entry = func.entry.clone();
    let mut visited: HashSet<String> = HashSet::new();
    let mut env: HashMap<String, SsaVar> = HashMap::new();

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
        if name.starts_with('_') {
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

    // 2. 构建内联替换映射: 单次使用的 SSA 临时变量在使用处内联
    let mut subst: HashMap<String, Option<Expr>> = HashMap::new();

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

    // 对每个 SSA 定义: 如果是临时变量且只被使用一次, 标记为内联
    for bb_name in &func.block_order {
        if let Some(bb) = func.blocks.get(bb_name) {
            for inst in &bb.insts {
                if let Some(d) = &inst.dest {
                    let ssa_name = d.name();
                    let is_temp = d.base.starts_with('_');
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
                    // 使用点消费, 重建时会被跳过, 副作用随之丢失.
                    let side_effect_unused = use_count == 0 && inst_has_side_effect(&inst.kind);
                    let inlineable = is_temp && use_count <= 1 && !side_effect_unused;
                    if inlineable {
                        let resolved = inst_to_expr_resolved(&inst.kind, &subst, &inst.span, func);
                        subst.insert(ssa_name, Some(resolved));
                    } else {
                        subst.insert(ssa_name, None);
                    }
                }
            }
        }
    }

    let doms = compute_dominators(func);
    rebuild_bb(&entry, func, &mut items, &mut visited, &mut env, &subst, &doms, None);
    Block {
        items,
        span: func.span.clone(),
    }
}

fn phi_name(v: &SsaVar) -> String {
    v.name()
}

/// 判断某条 SSA 指令是否为优化 pass 插入的 hint/metadata (不对应真实计算).
/// 这些指令以 Store 到 `__dcc_` 前缀符号的形式存在, 例如:
///   - register_allocation: `__dcc_regalloc_done`
///   - auto_parallelization: `__dcc_parallel_hint`
///   - pgo: `__dcc_hot_hint` / `__dcc_cold_hint`
///
/// 在 SSA→AST 重建时必须丢弃, 否则会生成对不存在全局变量的访问.
/// 但 section 标记 (`__dcc_section_hot` / `__dcc_section_cold`) 除外:
/// 它们需要被保留为符号引用, 供 codegen 把热/冷函数放入不同 section.
fn is_pass_hint_inst(kind: &InstKind) -> bool {
    if let InstKind::Store {
        e: Operand::SymAddr(name, _),
        ..
    } = kind
    {
        name.starts_with("__dcc_") && !name.starts_with("__dcc_section_")
    } else {
        false
    }
}

/// 指令是否带有不可消除的副作用 (Call / Store / volatile InlineAsm 等)。
/// SSA→AST 重建时, 即使其结果未被使用也不能整条丢弃。
fn inst_has_side_effect(kind: &InstKind) -> bool {
    match kind {
        InstKind::Call { .. }
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

fn rebuild_bb(
    bb_name: &str,
    func: &Function,
    out: &mut Vec<BlockItem>,
    visited: &mut HashSet<String>,
    env: &mut HashMap<String, SsaVar>,
    subst: &HashMap<String, Option<Expr>>,
    doms: &HashMap<String, HashSet<String>>,
    loop_ctx: Option<(&str, &HashSet<String>)>,
) {
    // In loop-internal mode, we rebuild blocks per-path so that join points
    // (e.g. for_inc reached from both branches of an if) are not skipped.
    // Each path in a structured loop ends with continue or break, so no
    // infinite recursion is possible within the body.
    if loop_ctx.is_none() {
        if !visited.insert(bb_name.to_string()) {
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
    // Skip this check when we are already inside a loop body.
    if loop_ctx.is_none() {
        if is_cycle_entry(bb_name, func, doms) {
            rebuild_cycle(bb_name, func, out, visited, env, subst, doms);
            return;
        }
    }
    let bb = bb.clone();
    // 0. phi 处理: 将前驱的 SSA 值映射到 phi dest 的基础名
    for phi in &bb.phis {
        for (src, pred) in &phi.incoming {
            if pred == bb_name || env.contains_key(&phi_name(&phi.dest)) {
                env.insert(phi_name(&phi.dest), src.clone());
            }
        }
        if let Some((src, _)) = bb.phis.first().and_then(|p| p.incoming.first()) {
            env.insert(phi_name(&phi.dest), src.clone());
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
        if let Some(d) = &inst.dest {
            env.insert(d.name(), d.clone());
            let ssa_name = d.name();
            if let Some(Some(_)) = subst.get(&ssa_name) {
                continue;
            }
            let rhs = inst_to_expr_resolved(&inst.kind, subst, &inst.span, func);
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
            let e = inst_to_expr_resolved(&inst.kind, subst, &inst.span, func);
            out.push(BlockItem::Stmt(Stmt::Expr {
                e,
                span: inst.span.clone(),
            }));
        }
    }
    // 2. 翻译 terminator -> AST 后, 继续后继
    match &bb.term {
        Terminator::Ret { val } => {
            let e = val.as_ref().map(|o| resolve_operand(o, env, subst, func));
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
                    rebuild_bb(t, func, out, visited, env, subst, doms, loop_ctx);
                } else {
                    out.push(BlockItem::Stmt(Stmt::Break {
                        span: func.span.clone(),
                    }));
                }
            } else {
                rebuild_bb(t, func, out, visited, env, subst, doms, loop_ctx);
            }
        },
        Terminator::Branch { cond, t, f } => {
            let cond_e = resolve_operand(cond, env, subst, func);
            let mut then_items: Vec<BlockItem> = Vec::new();
            let mut then_env = env.clone();
            let then_subst = subst.clone();
            match loop_ctx {
                Some((lh, _)) if t == lh => {
                    then_items.push(BlockItem::Stmt(Stmt::Continue {
                        span: func.span.clone(),
                    }));
                },
                Some((_, body_set)) if !body_set.contains(t) => {
                    then_items.push(BlockItem::Stmt(Stmt::Break {
                        span: func.span.clone(),
                    }));
                },
                _ => {
                    rebuild_bb(
                        t,
                        func,
                        &mut then_items,
                        visited,
                        &mut then_env,
                        &then_subst,
                        doms,
                        loop_ctx,
                    );
                },
            }
            let mut else_items: Vec<BlockItem> = Vec::new();
            let mut else_env = env.clone();
            let else_subst = subst.clone();
            match loop_ctx {
                Some((lh, _)) if f == lh => {
                    else_items.push(BlockItem::Stmt(Stmt::Continue {
                        span: func.span.clone(),
                    }));
                },
                Some((_, body_set)) if !body_set.contains(f) => {
                    else_items.push(BlockItem::Stmt(Stmt::Break {
                        span: func.span.clone(),
                    }));
                },
                _ => {
                    rebuild_bb(
                        f,
                        func,
                        &mut else_items,
                        visited,
                        &mut else_env,
                        &else_subst,
                        doms,
                        loop_ctx,
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
        },
        Terminator::Unreachable => {},
        Terminator::SwitchTable {
            idx,
            values,
            targets,
            default,
        } => {
            let idx_e = resolve_operand(idx, env, subst, func);
            let mut cases = Vec::new();
            // Each case branch rebuilds its path independently with its own
            // visited set, so that shared downstream blocks (e.g. switch_end)
            // are processed for each case rather than skipped on subsequent
            // cases due to the first case marking them visited.
            for (v, target) in values.iter().zip(targets.iter()) {
                let mut target_items: Vec<BlockItem> = Vec::new();
                let mut target_env = env.clone();
                let target_subst = subst.clone();
                let mut case_visited = visited.clone();
                rebuild_bb(
                    target,
                    func,
                    &mut target_items,
                    &mut case_visited,
                    &mut target_env,
                    &target_subst,
                    doms,
                    loop_ctx,
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
            let mut def_env = env.clone();
            let def_subst = subst.clone();
            let mut def_visited = visited.clone();
            rebuild_bb(
                default,
                func,
                &mut def_items,
                &mut def_visited,
                &mut def_env,
                &def_subst,
                doms,
                loop_ctx,
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
fn rebuild_cycle(
    header: &str,
    func: &Function,
    out: &mut Vec<BlockItem>,
    visited: &mut HashSet<String>,
    env: &mut HashMap<String, SsaVar>,
    subst: &HashMap<String, Option<Expr>>,
    doms: &HashMap<String, HashSet<String>>,
) {
    let header_bb = match func.blocks.get(header) {
        Some(b) => b,
        None => return,
    };
    // Mark header as visited so we don't re-enter it via rebuild_bb
    visited.insert(header.to_string());

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
    // Pick the last latch in block_order as the primary latch
    let latch = latches
        .into_iter()
        .max_by_key(|l| func.block_order.iter().position(|b| b == l).unwrap_or(0))
        .unwrap();

    // Collect loop body blocks via reverse DFS from latch (excluding header).
    let body_bbs_set: HashSet<String> = find_loop_body(func, header, &latch)
        .into_iter()
        .filter(|b| b != header)
        .collect();

    // Handle header phi nodes: map phi dest to the entry (first incoming) version.
    // In the AST representation, there is only one variable per base name,
    // so SSA phi semantics are captured by mutable variable assignments in the body.
    for phi in &header_bb.phis {
        if let Some((entry_src, _)) = phi.incoming.first() {
            env.insert(phi_name(&phi.dest), entry_src.clone());
        }
    }

    // Determine loop condition from header's branch terminator.
    // For a while-loop, the header is the condition block and we extract
    // the condition expression.  For a do-while (header is a body block
    // whose terminator is a Jump), the condition lives inside the loop
    // body and is expressed as `if (!cond) break;`.
    let cond_expr: Option<Expr> = match &header_bb.term {
        Terminator::Branch { cond, t, f } => {
            let t_in_body = body_bbs_set.contains(t);
            let f_in_body = body_bbs_set.contains(f);
            let t_back = is_back_edge(header, t, doms);
            let f_back = is_back_edge(header, f, doms);
            let t_continues = t_in_body || t_back;
            let f_continues = f_in_body || f_back;
            if t_continues && !f_continues {
                Some(resolve_operand(cond, env, subst, func))
            } else if f_continues && !t_continues {
                // Negate condition
                let cond_e = resolve_operand(cond, env, subst, func);
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
            if !t_in_body && !t_back {
                if seen_exit.insert(t.clone()) {
                    exit_blocks.push(t.clone());
                }
            }
            if !f_in_body && !f_back {
                if seen_exit.insert(f.clone()) {
                    exit_blocks.push(f.clone());
                }
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
                } else if !is_back_edge(header, t, doms) {
                    if seen_exit.insert(t.clone()) {
                        exit_blocks.push(t.clone());
                    }
                }
            }
            if body_bbs_set.contains(default) && !is_back_edge(header, default, doms) {
                body_entries.push(default.clone());
            } else if !is_back_edge(header, default, doms) {
                if seen_exit.insert(default.clone()) {
                    exit_blocks.push(default.clone());
                }
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
    if cond_expr.is_none() {
        for inst in &header_bb.insts {
            if is_pass_hint_inst(&inst.kind) {
                continue;
            }
            emit_rebuild_inst(inst, &mut body_items, env, subst, func);
        }
    }
    for entry in &body_entries {
        let mut entry_env = env.clone();
        let entry_subst = subst.clone();
        rebuild_bb(
            entry,
            func,
            &mut body_items,
            visited,
            &mut entry_env,
            &entry_subst,
            doms,
            Some((header, &body_bbs_set)),
        );
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
    // Continue from exit block
    if let Some(exit) = exit_succ {
        rebuild_bb(&exit, func, out, visited, env, subst, doms, None);
    }
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
        let rhs = inst_to_expr_resolved(&inst.kind, subst, &inst.span, func);
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
        let e = inst_to_expr_resolved(&inst.kind, subst, &inst.span, func);
        out.push(BlockItem::Stmt(Stmt::Expr {
            e,
            span: inst.span.clone(),
        }));
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
                Expr::Ident {
                    name: crate::ast::Ident {
                        name: found.base.clone(),
                        span: v.span.clone(),
                    },
                    span: v.span.clone(),
                    ty: v.ty.clone(),
                }
            } else {
                Expr::Ident {
                    name: crate::ast::Ident {
                        name: v.base.clone(),
                        span: v.span.clone(),
                    },
                    span: v.span.clone(),
                    ty: v.ty.clone(),
                }
            }
        },
        _ => operand_to_expr(o, env, func),
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
fn infer_binop_type(l: &Expr, r: &Expr) -> Type {
    let lt = l.ty();
    let rt = r.ty();
    // 向量操作: 结果类型与向量操作数相同 (比较也返回向量, 全 1/0 每元素)
    if matches!(&lt, Type::Vector(_, _)) {
        return lt.clone();
    }
    if matches!(&rt, Type::Vector(_, _)) {
        return rt.clone();
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
    } else {
        Type::Int
    }
}

/// 将 SSA 指令转换为 AST 表达式, 同时应用内联替换。
/// `inst_span` 是原始 SSA 指令的 span, 用于保留源码位置信息。
fn inst_to_expr_resolved(
    kind: &InstKind,
    subst: &HashMap<String, Option<Expr>>,
    inst_span: &Span,
    func: &Function,
) -> Expr {
    let sp = inst_span.clone();
    let fake_env: HashMap<String, SsaVar> = HashMap::new();
    match kind {
        InstKind::Bin { op, l, r } => {
            let l_expr = resolve_operand(l, &fake_env, subst, func);
            let r_expr = resolve_operand(r, &fake_env, subst, func);
            let binop_ty = infer_binop_type(&l_expr, &r_expr);
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
            // 向量 UnOp: 结果类型同内部表达式 (Neg/BitNot 在向量上逐元素)
            let ty = if matches!(inner.ty(), Type::Vector(_, _)) {
                inner.ty()
            } else {
                Type::Int
            };
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
            ty: Type::Ptr(Box::new(QualType::new(Type::Void))),
        },
        InstKind::Load { e, ty, .. } => Expr::Deref {
            e: Box::new(resolve_operand(e, &fake_env, subst, func)),
            span: sp.clone(),
            ty: ty.clone(),
        },
        InstKind::Store { e, src, .. } => Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Deref {
                e: Box::new(resolve_operand(e, &fake_env, subst, func)),
                span: sp.clone(),
                ty: Type::Void,
            }),
            r: Box::new(resolve_operand(src, &fake_env, subst, func)),
            span: sp.clone(),
            ty: Type::Void,
        },
        InstKind::IndexLoad { arr, idx, .. } => Expr::Index {
            arr: Box::new(resolve_operand(arr, &fake_env, subst, func)),
            idx: Box::new(resolve_operand(idx, &fake_env, subst, func)),
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::IndexStore { arr, idx, src, .. } => Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Index {
                arr: Box::new(resolve_operand(arr, &fake_env, subst, func)),
                idx: Box::new(resolve_operand(idx, &fake_env, subst, func)),
                span: sp.clone(),
                ty: Type::Int,
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
                    ty: Type::Void,
                    is_arrow: *is_arrow,
                }),
                r: Box::new(resolve_operand(src, &fake_env, subst, func)),
                span: sp.clone(),
                ty: Type::Void,
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
                Expr::Ident {
                    name: crate::ast::Ident {
                        name: found.base.clone(),
                        span: v.span.clone(),
                    },
                    span: v.span.clone(),
                    ty: v.ty.clone(),
                }
            } else {
                Expr::Ident {
                    name: crate::ast::Ident {
                        name: v.base.clone(),
                        span: v.span.clone(),
                    },
                    span: v.span.clone(),
                    ty: v.ty.clone(),
                }
            }
        },
        Operand::Int(v) => Expr::Int {
            v: *v,
            span: Span::point("ssa", 0, 0),
            ty: Type::Int,
        },
        Operand::Float(v, ty) => Expr::Float {
            v: *v,
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
            let binop_ty = infer_binop_type(&l_expr, &r_expr);
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
            ty: Type::Int,
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
            ty: Type::Ptr(Box::new(QualType::new(Type::Void))),
        },
        InstKind::Load { e, ty, .. } => Expr::Deref {
            e: Box::new(operand_to_expr(e, env, func)),
            span: sp.clone(),
            ty: ty.clone(),
        },
        InstKind::Store { e, src, .. } => Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Deref {
                e: Box::new(operand_to_expr(e, env, func)),
                span: sp.clone(),
                ty: Type::Void,
            }),
            r: Box::new(operand_to_expr(src, env, func)),
            span: sp.clone(),
            ty: Type::Void,
        },
        InstKind::IndexLoad { arr, idx, .. } => Expr::Index {
            arr: Box::new(operand_to_expr(arr, env, func)),
            idx: Box::new(operand_to_expr(idx, env, func)),
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::IndexStore { arr, idx, src, .. } => Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Index {
                arr: Box::new(operand_to_expr(arr, env, func)),
                idx: Box::new(operand_to_expr(idx, env, func)),
                span: sp.clone(),
                ty: Type::Int,
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
                    ty: Type::Void,
                    is_arrow: *is_arrow,
                }),
                r: Box::new(operand_to_expr(src, env, func)),
                span: sp.clone(),
                ty: Type::Void,
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
}
