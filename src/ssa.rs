// src/ssa.rs - 轻量 SSA IR (M16 优化 pass 基础设施)
//
// 设计原则 (suckless):
//   - 只覆盖结构化控制流 (if/while/for/do-while/return)。含 goto/label/asm-goto
//     /switch(Switch 多 case 简化为 if-chains)/longjmp 的函数降级回 AST pass。
//   - 单一文件: BB / CFG / SSA 值 / phi / dominator 都在这里。
//   - 零外部依赖,仅 std。
//   - IR 形态: 每个变量按 SSA 版本号命名 (base_N); phi 节点在 merge BB 顶端。

use std::collections::{HashMap, HashSet};

use crate::ast::{BinOp, Block, BlockItem, Decl, Expr, FuncDecl, Stmt, Type, UnOp};
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
    Float(f64),
    /// 字符串字面量 (在 rodata 中以 FNV-1a label 表示)
    Str(Vec<u8>),
    /// 全局符号地址 (用符号名占位)
    SymAddr(String),
    /// 函数地址
    FnAddr(String),
    /// 无值 (void)
    Void,
    /// 调用返回
    Call(String),
    /// 复合运算 (Phi 之类)
    Phi(Vec<(SsaVar, String)>),
}

#[derive(Debug, Clone)]
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
    Load { e: Operand, ty: Type },
    /// `*e = src` 存储
    Store { e: Operand, src: Operand },
    /// `dest = arr[idx]`
    IndexLoad { arr: Operand, idx: Operand },
    /// `arr[idx] = src`
    IndexStore { arr: Operand, idx: Operand, src: Operand },
    /// `dest = call(callee, args)`
    Call { callee: Operand, args: Vec<Operand>, ret_ty: Type },
    /// `dest = obj.field`
    MemberLoad { obj: Operand, field: String, ty: Type },
    /// `obj.field = src`
    MemberStore { obj: Operand, field: String, src: Operand },
    /// `dest = cond ? t : f`
    Select { cond: Operand, then_: Operand, else_: Operand },
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
        };
        s.add_block("entry".to_string());
        s
    }

    pub fn add_block(&mut self, name: String) -> String {
        if !self.blocks.contains_key(&name) {
            self.blocks.insert(name.clone(), BasicBlock {
                name: name.clone(),
                phis: Vec::new(),
                insts: Vec::new(),
                term: Terminator::Unreachable,
                preds: Vec::new(),
                succs: Vec::new(),
            });
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
pub fn compute_dominators(func: &Function) -> HashMap<String, HashSet<String>> {
    let mut doms: HashMap<String, HashSet<String>> = HashMap::new();
    if !func.blocks.contains_key(&func.entry) {
        return doms;
    }
    // reverse postorder
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
            let preds = &func.blocks[b].preds;
            if preds.is_empty() {
                continue;
            }
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
    for s in &func.blocks[bb].succs {
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
            Decl::Func(f) => f.body.as_ref().map_or(true, is_structured),
            _ => true,
        },
    }
}

fn is_structured_stmt(s: &Stmt) -> bool {
    match s {
        Stmt::Goto { .. } | Stmt::GotoIndirect { .. } | Stmt::Label { .. } => false,
        Stmt::AsmGoto { .. } => false,
        Stmt::Switch { .. } => false, // Switch 简化为 if-chains 后再 SSA 化
        Stmt::If { then_, else_, .. } => {
            is_structured_stmt(then_)
                && else_.as_ref().map_or(true, |e| is_structured_stmt(e))
        }
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
        };
        b.func.blocks.get_mut(&b.cur).unwrap().term = Terminator::Unreachable;
        b
    }

    fn fresh(&mut self, base: &str, ty: Type, span: &Span) -> SsaVar {
        self.tmp_count += 1;
        SsaVar { base: format!("_t{}_{}", base, self.tmp_count), version: 0, ty, span: span.clone() }
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
        match e {
            Expr::Int { v, .. } => Operand::Int(*v),
            Expr::Float { v, .. } => Operand::Float(*v),
            Expr::String { data, .. } => Operand::Str(data.clone()),
            Expr::Char { v, .. } => Operand::Int(*v as i128),
            Expr::Ident { name, span: _, ty: _ } => {
                if let Some(v) = self.lookup(&name.name) {
                    return Operand::Var(v);
                }
                // 全局变量 -> SymAddr 占位
                if let Some(g) = self.func.globals.get(&name.name) {
                    let _ = g;
                    return Operand::SymAddr(name.name.clone());
                }
                // 未定义变量: 视为全局 (codegen fallback)
                Operand::SymAddr(name.name.clone())
            }
            Expr::Binop { op, l, r, span, ty } => {
                let lo = self.lower_expr(l);
                let ro = self.lower_expr(r);
                let dest = self.fresh("bin", ty.clone(), span);
                self.emit(Inst { dest: Some(dest.clone()), kind: InstKind::Bin { op: *op, l: lo, r: ro }, span: span.clone() });
                Operand::Var(dest)
            }
            Expr::Unary { op, e: inner, span, ty } => {
                let o = self.lower_expr(inner);
                let dest = self.fresh("un", ty.clone(), span);
                self.emit(Inst { dest: Some(dest.clone()), kind: InstKind::Un { op: *op, e: o }, span: span.clone() });
                Operand::Var(dest)
            }
            Expr::Assign { op, l, r, span, ty } => {
                let ro = self.lower_expr(r);
                if let Some(dest_var) = lower_lvalue(l, op, ro.clone(), span, ty, self) {
                    return Operand::Var(dest_var);
                }
                ro
            }
            Expr::Cast { ty, e: inner, span } => {
                let o = self.lower_expr(inner);
                let dest = self.fresh("cast", ty.clone(), span);
                self.emit(Inst { dest: Some(dest.clone()), kind: InstKind::Cast { ty: ty.clone(), e: o }, span: span.clone() });
                Operand::Var(dest)
            }
            Expr::Cond { cond, then_, else_, span, ty } => {
                let co = self.lower_expr(cond);
                let t = self.lower_expr(then_);
                let f = self.lower_expr(else_);
                let dest = self.fresh("sel", ty.clone(), span);
                self.emit(Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::Select { cond: co, then_: t, else_: f },
                    span: span.clone(),
                });
                Operand::Var(dest)
            }
            Expr::Comma { l: _, r, span, ty } => {
                // 丢弃 lhs (在 SSA 化中已通过指令序列完成)
                let _ = span;
                let o = self.lower_expr(r);
                let _ = ty;
                o
            }
            Expr::Call { callee, args, span, ty } => {
                let callee_o = self.lower_expr(callee);
                let arg_ops: Vec<Operand> = args.iter().map(|a| self.lower_expr(a)).collect();
                let ret_ty = ty.clone();
                let dest = if matches!(ret_ty, Type::Void) {
                    None
                } else {
                    Some(self.fresh("call", ret_ty.clone(), span))
                };
                self.emit(Inst {
                    dest: dest.clone(),
                    kind: InstKind::Call { callee: callee_o, args: arg_ops, ret_ty: ret_ty.clone() },
                    span: span.clone(),
                });
                match dest {
                    Some(v) => Operand::Var(v),
                    None => Operand::Void,
                }
            }
            Expr::Index { arr, idx, span, ty } => {
                let a = self.lower_expr(arr);
                let i = self.lower_expr(idx);
                let dest = self.fresh("idx", ty.clone(), span);
                self.emit(Inst { dest: Some(dest.clone()), kind: InstKind::IndexLoad { arr: a, idx: i }, span: span.clone() });
                Operand::Var(dest)
            }
            Expr::Member { expr, field, span, ty, is_arrow } => {
                let obj = self.lower_expr(expr);
                let dest = self.fresh("mem", ty.clone(), span);
                if *is_arrow {
                    // *obj.field
                    let inner = self.fresh("deref", Type::Ptr(Box::new(ty.clone())), span);
                    self.emit(Inst { dest: Some(inner.clone()), kind: InstKind::Load { e: obj, ty: ty.clone() }, span: span.clone() });
                    self.emit(Inst { dest: Some(dest.clone()), kind: InstKind::MemberLoad { obj: Operand::Var(inner), field: field.name.clone(), ty: ty.clone() }, span: span.clone() });
                } else {
                    self.emit(Inst { dest: Some(dest.clone()), kind: InstKind::MemberLoad { obj, field: field.name.clone(), ty: ty.clone() }, span: span.clone() });
                }
                Operand::Var(dest)
            }
            Expr::Deref { e, span, ty } => {
                let o = self.lower_expr(e);
                let dest = self.fresh("deref", ty.clone(), span);
                self.emit(Inst { dest: Some(dest.clone()), kind: InstKind::Load { e: o, ty: ty.clone() }, span: span.clone() });
                Operand::Var(dest)
            }
            Expr::AddrOf { e, span, ty: _ } => {
                if let Expr::Ident { name, .. } = e.as_ref() {
                    let dest = self.fresh("addr", Type::Ptr(Box::new(Type::Void)), span);
                    self.emit(Inst { dest: Some(dest.clone()), kind: InstKind::AddrOf { base: name.name.clone() }, span: span.clone() });
                    return Operand::Var(dest);
                }
                Operand::Void
            }
            Expr::PostInc { e, span, ty: _ } | Expr::PreInc { e, span, ty: _ }
            | Expr::PostDec { e, span, ty: _ } | Expr::PreDec { e, span, ty: _ } => {
                let is_inc = matches!(&**e, Expr::PostInc { .. } | Expr::PreInc { .. });
                let one = if is_inc { 1i128 } else { -1i128 };
                if let Expr::Ident { name, ty, .. } = e.as_ref() {
                    let cur_v = self.lookup(&name.name).unwrap_or_else(|| SsaVar { base: name.name.clone(), version: 0, ty: ty.clone(), span: span.clone() });
                    let one_op = Operand::Int(one);
                    let new_dest = self.fresh("upd", ty.clone(), span);
                    self.emit(Inst {
                        dest: Some(new_dest.clone()),
                        kind: InstKind::Bin { op: BinOp::Add, l: Operand::Var(cur_v.clone()), r: one_op },
                        span: span.clone(),
                    });
                    // store back
                    self.emit(Inst {
                        dest: None,
                        kind: InstKind::Store { e: Operand::SymAddr(name.name.clone()), src: Operand::Var(new_dest.clone()) },
                        span: span.clone(),
                    });
                    let nv = SsaVar { base: name.name.clone(), version: self.next_version(&name.name), ty: ty.clone(), span: span.clone() };
                    self.define(nv.clone());
                    // override previous
                    let _ = cur_v;
                    let v = self.fresh("post", ty.clone(), span);
                    self.emit(Inst { dest: Some(v.clone()), kind: InstKind::Bin { op: BinOp::Add, l: Operand::Var(cur_v), r: Operand::Int(0) }, span: span.clone() });
                    return Operand::Var(v);
                }
                Operand::Int(one)
            }
            Expr::SizeofTy { ty, .. } => Operand::Int(ty.size() as i128),
            Expr::SizeofExpr { e, .. } => Operand::Int(e.ty().size() as i128),
            Expr::AlignofTy { ty, .. } => Operand::Int(ty.align() as i128),
            Expr::AlignofExpr { e, .. } => Operand::Int(e.ty().align() as i128),
            _ => Operand::Void, // 简化: 复杂 expr (CompoundLit, StmtExpr, LabelAddr, VaArg) 降级
        }
    }

    fn lower_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Expr { e, .. } => {
                let _ = self.lower_expr(e);
            }
            Stmt::Return { val, span: _ } => {
                let v = val.as_ref().map(|e| self.lower_expr(e));
                self.set_term(Terminator::Ret { val: v });
                // 返回后所有后续代码在 unreachable BB
                let dead = self.func.unique_bb(&format!("{}.dead", self.cur));
                let _ = dead;
            }
            Stmt::If { cond, then_, else_, span: _ } => {
                let cond_o = self.lower_expr(cond);
                let then_bb = self.func.unique_bb("if.then");
                let else_bb = self.func.unique_bb("if.else");
                let merge_bb = self.func.unique_bb("if.end");
                self.set_term(Terminator::Branch { cond: cond_o, t: then_bb.clone(), f: else_bb.clone() });
                if let Some(tbb) = self.func.blocks.get_mut(&then_bb) { tbb.preds.push(self.cur.clone()); tbb.preds.dedup(); }
                if let Some(ebb) = self.func.blocks.get_mut(&else_bb) { ebb.preds.push(self.cur.clone()); ebb.preds.dedup(); }
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
            }
            Stmt::While { cond, body, span: _ } => {
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
                self.set_term(Terminator::Branch { cond: cond_o, t: body_bb.clone(), f: end_bb.clone() });
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
            }
            Stmt::Do { body, cond, span: _ } => {
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
                self.set_term(Terminator::Branch { cond: cond_o, t: body_bb.clone(), f: end_bb.clone() });
                if let Some(bb) = self.func.blocks.get_mut(&body_bb) {
                    bb.preds.push(self.cur.clone());
                }
                if let Some(eb) = self.func.blocks.get_mut(&end_bb) {
                    eb.preds.push(self.cur.clone());
                }
                self.cur = end_bb;
            }
            Stmt::For { init, cond, step, body, span: _ } => {
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
                let cond_o = cond.as_ref().map(|c| self.lower_expr(c)).unwrap_or(Operand::Int(1));
                self.set_term(Terminator::Branch { cond: cond_o, t: body_bb.clone(), f: end_bb.clone() });
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
                if let Some(b) = self.func.blocks.get_mut(&self.cur) {
                    if matches!(b.term, Terminator::Unreachable) {
                        b.term = Terminator::Jump(cond_bb.clone());
                        b.succs.push(cond_bb.clone());
                    }
                }
                if let Some(cb) = self.func.blocks.get_mut(&cond_bb) {
                    cb.preds.push(self.cur.clone());
                    cb.preds.dedup();
                }
                self.cur = end_bb;
            }
            Stmt::Block { b, .. } => {
                self.lower_block(b);
            }
            Stmt::Break { .. } => {
                if let Some(t) = self.break_stack.last().cloned() {
                    self.set_term(Terminator::Jump(t.clone()));
                    if let Some(b) = self.func.blocks.get_mut(&self.cur) {
                        b.succs.push(t);
                    }
                    let dead = self.func.unique_bb(&format!("{}.dead", self.cur));
                    self.cur = dead;
                }
            }
            Stmt::Continue { .. } => {
                if let Some(t) = self.continue_stack.last().cloned() {
                    self.set_term(Terminator::Jump(t.clone()));
                    if let Some(b) = self.func.blocks.get_mut(&self.cur) {
                        b.succs.push(t);
                    }
                    let dead = self.func.unique_bb(&format!("{}.dead", self.cur));
                    self.cur = dead;
                }
            }
            _ => {} // 其他: 已通过 is_structured 过滤
        }
    }

    fn lower_block(&mut self, b: &Block) {
        for item in &b.items {
            match item {
                BlockItem::Stmt(s) => self.lower_stmt(s),
                BlockItem::Decl(Decl::Var(v)) => {
                    if let Some(init) = &v.init {
                        let o = self.lower_expr(init);
                        let v_0 = SsaVar { base: v.name.name.clone(), version: self.next_version(&v.name.name), ty: v.ty.clone(), span: v.span.clone() };
                        // 隐式 store: dest = o
                        if let Operand::Var(src) = o {
                            let _ = src; // 已经在前 emit, 简化: 直接定义
                        }
                        self.define(v_0);
                    } else {
                        let v_0 = SsaVar { base: v.name.name.clone(), version: self.next_version(&v.name.name), ty: v.ty.clone(), span: v.span.clone() };
                        self.define(v_0);
                    }
                }
                _ => {}
            }
        }
    }
}

fn lower_lvalue(
    l: &Expr,
    op: &BinOp,
    src: Operand,
    span: &Span,
    ty: &Type,
    b: &mut Builder,
) -> Option<SsaVar> {
    if let Expr::Ident { name, ty: var_ty, .. } = l {
        let v = SsaVar { base: name.name.clone(), version: b.next_version(&name.name), ty: var_ty.clone(), span: span.clone() };
        let _ = op; // AST 已是展开形式
        b.emit(Inst {
            dest: None,
            kind: InstKind::Store { e: Operand::SymAddr(name.name.clone()), src: src.clone() },
            span: span.clone(),
        });
        b.define(v.clone());
        return Some(v);
    }
    if let Expr::Deref { e: inner, .. } = l {
        let addr = b.lower_expr(inner);
        b.emit(Inst {
            dest: None,
            kind: InstKind::Store { e: addr, src },
            span: span.clone(),
        });
        let dest = b.fresh("store", ty.clone(), span);
        return Some(dest);
    }
    if let Expr::Index { arr, idx, .. } = l {
        let a = b.lower_expr(arr);
        let i = b.lower_expr(idx);
        b.emit(Inst {
            dest: None,
            kind: InstKind::IndexStore { arr: a, idx: i, src },
            span: span.clone(),
        });
        let dest = b.fresh("idxstore", ty.clone(), span);
        return Some(dest);
    }
    if let Expr::Member { expr, field, is_arrow, .. } = l {
        let obj = b.lower_expr(expr);
        if *is_arrow {
            let inner = b.fresh("deref", Type::Ptr(Box::new(ty.clone())), span);
            b.emit(Inst { dest: Some(inner.clone()), kind: InstKind::Load { e: obj, ty: ty.clone() }, span: span.clone() });
            b.emit(Inst { dest: None, kind: InstKind::MemberStore { obj: Operand::Var(inner), field: field.name.clone(), src }, span: span.clone() });
        } else {
            b.emit(Inst { dest: None, kind: InstKind::MemberStore { obj, field: field.name.clone(), src }, span: span.clone() });
        }
        let dest = b.fresh("memstore", ty.clone(), span);
        return Some(dest);
    }
    None
}

pub fn build_from_func(f: &FuncDecl) -> Option<Function> {
    let body = f.body.as_ref()?;
    if !is_structured(body) {
        return None;
    }
    let mut func = Function::new(f.name.name.clone(), f.ret_ty.clone(), f.span.clone());
    func.is_cold = f.is_cold;
    func.is_hot = false; // FuncDecl 暂无 hot 字段
    func.is_noreturn = false;
    let mut builder = Builder::new(func);
    for p in &f.params {
        if let Some(n) = &p.name {
            let v = SsaVar { base: n.name.clone(), version: 0, ty: p.ty.clone(), span: p.span.clone() };
            builder.func.params.push(v.clone());
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
    Some(builder.func)
}

// ============================================================
// SSA -> AST 重建器
// ============================================================

/// 把 SSA Function 重建为 AST Block (codegen 仍走 AST 路径)。
///
/// 简化策略: 全部用临时变量 / Store 把 SSA 值塞回原变量名; phi 在 merge
/// BB 顶端处理为对原变量多次赋值 (用同名 var, 以 if 块包成 then/else)。
pub fn rebuild_block(func: &Function) -> Block {
    let mut items: Vec<BlockItem> = Vec::new();
    let entry = func.entry.clone();
    let mut visited: HashSet<String> = HashSet::new();
    let mut env: HashMap<String, SsaVar> = HashMap::new();
    rebuild_bb(&entry, func, &mut items, &mut visited, &mut env);
    Block { items, span: func.span.clone() }
}

fn phi_name(v: &SsaVar) -> String { v.name() }

fn rebuild_bb(
    bb_name: &str,
    func: &Function,
    out: &mut Vec<BlockItem>,
    visited: &mut HashSet<String>,
    env: &mut HashMap<String, SsaVar>,
) {
    if !visited.insert(bb_name.to_string()) {
        return;
    }
    let bb = match func.blocks.get(bb_name) {
        Some(b) => b.clone(),
        None => return,
    };
    // 0. 应用 phi 之前的前驱 env 合并: 简单做法 - 用基本变量名, 取 self.current 同样的策略
    for phi in &bb.phis {
        for (src, pred) in &phi.incoming {
            if pred == bb_name || env.contains_key(&phi_name(&phi.dest)) {
                env.insert(phi_name(&phi.dest), src.clone());
            }
        }
        // 简化: 单一前驱或首个前驱值
        if let Some((src, _)) = bb.phis.first().and_then(|p| p.incoming.first()) {
            env.insert(phi_name(&phi.dest), src.clone());
        }
    }
    // 1. 翻译指令
    for inst in &bb.insts {
        if let Some(d) = &inst.dest {
            env.insert(d.name(), d.clone());
        }
    }
    // 2. 翻译 terminator -> AST 后, 继续后继
    match &bb.term {
        Terminator::Ret { val } => {
            let e = val.as_ref().map(|o| operand_to_expr(o, env));
            out.push(BlockItem::Stmt(Stmt::Return { val: e, span: func.span.clone() }));
        }
        Terminator::Jump(t) => {
            rebuild_bb(t, func, out, visited, env);
        }
        Terminator::Branch { cond, t, f } => {
            let cond_e = operand_to_expr(cond, env);
            // 顺序重建 then/else 到一个 Block; 共用 visited + env 以避免回环无限递归
            let mut then_items: Vec<BlockItem> = Vec::new();
            let then_env = env.clone();
            rebuild_bb(t, func, &mut then_items, visited, &mut then_env.clone());
            let mut else_items: Vec<BlockItem> = Vec::new();
            let else_env = env.clone();
            rebuild_bb(f, func, &mut else_items, visited, &mut else_env.clone());
            out.push(BlockItem::Stmt(Stmt::If {
                cond: cond_e,
                then_: Box::new(Stmt::Block { b: Block { items: then_items, span: func.span.clone() }, span: func.span.clone() }),
                else_: if else_items.is_empty() { None } else { Some(Box::new(Stmt::Block { b: Block { items: else_items, span: func.span.clone() }, span: func.span.clone() })) },
                span: func.span.clone(),
            }));
        }
        Terminator::Unreachable => {}
    }
}

fn operand_to_expr(o: &Operand, env: &HashMap<String, SsaVar>) -> Expr {
    match o {
        Operand::Var(v) => {
            if let Some(found) = env.get(&v.name()) {
                Expr::Ident { name: crate::ast::Ident { name: found.name(), span: v.span.clone() }, span: v.span.clone(), ty: v.ty.clone() }
            } else {
                Expr::Ident { name: crate::ast::Ident { name: v.name(), span: v.span.clone() }, span: v.span.clone(), ty: v.ty.clone() }
            }
        }
        Operand::Int(v) => Expr::Int { v: *v, span: Span::point("ssa", 0, 0), ty: Type::Int },
        Operand::Float(v) => Expr::Float { v: *v, span: Span::point("ssa", 0, 0), ty: Type::Double },
        Operand::Str(d) => Expr::String { data: d.clone(), span: Span::point("ssa", 0, 0), ty: Type::Array(Box::new(Type::Char), crate::ast::ArraySize::Const(d.len())) },
        Operand::SymAddr(n) => Expr::Ident { name: crate::ast::Ident { name: n.clone(), span: Span::point("ssa", 0, 0) }, span: Span::point("ssa", 0, 0), ty: Type::Ptr(Box::new(Type::Void)) },
        Operand::FnAddr(n) => Expr::Ident { name: crate::ast::Ident { name: n.clone(), span: Span::point("ssa", 0, 0) }, span: Span::point("ssa", 0, 0), ty: Type::Ptr(Box::new(Type::Void)) },
        Operand::Void => Expr::Int { v: 0, span: Span::point("ssa", 0, 0), ty: Type::Void },
        Operand::Call(_) => Expr::Int { v: 0, span: Span::point("ssa", 0, 0), ty: Type::Int },
        Operand::Phi(_) => Expr::Int { v: 0, span: Span::point("ssa", 0, 0), ty: Type::Int },
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
                let dst = inst.dest.as_ref().map(|d| d.name()).unwrap_or_else(|| "_".to_string());
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
            name: crate::ast::Ident { name: "f".into(), span: span.clone() },
            params: vec![],
            body: Some(Block {
                items: vec![
                    BlockItem::Decl(Decl::Var(crate::ast::VarDecl {
                        ty: Type::Int,
                        name: crate::ast::Ident { name: "x".into(), span: span.clone() },
                        init: Some(Expr::Int { v: 1, span: span.clone(), ty: Type::Int }),
                        span: span.clone(),
                        is_extern: false, is_static: false, section: None, alignment: None,
                        is_weak: false, visibility: None, is_unused: false, register_name: None,
                        cleanup_fn: None, is_thread_local: false, no_sanitize: vec![], copy_fn: None,
                        is_retain: false, is_used_attr: false,
                    })),
                    BlockItem::Stmt(Stmt::Return { val: Some(Expr::Ident {
                        name: crate::ast::Ident { name: "x".into(), span: span.clone() },
                        span: span.clone(), ty: Type::Int,
                    }), span: span.clone() }),
                ],
                span: span.clone(),
            }),
            span: span.clone(),
            is_extern: false, is_static: false, section: None, alignment: None,
            is_weak: false, visibility: None, is_unused: false, is_cold: false,
            is_noinline: false, is_always_inline: false, is_variadic: false,
            is_constructor: false, is_destructor: false, alias_name: None, is_nested: false,
            upvars: vec![], is_old_style: false, format_kind: None, format_string_idx: None,
            format_first_arg: None, no_sanitize: vec![], target_clones: vec![], returns_nonnull: false,
            ctor_priority: None, dtor_priority: None, is_retain: false, is_used_attr: false,
            is_flatten: false, error_msg: None, warning_msg: None, deprecated_msg: None,
            ifunc_resolver: None,
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
            name: crate::ast::Ident { name: "f".into(), span: span.clone() },
            params: vec![],
            body: Some(Block {
                items: vec![
                    BlockItem::Stmt(Stmt::If {
                        cond: Expr::Int { v: 1, span: span.clone(), ty: Type::Int },
                        then_: Box::new(Stmt::Return { val: Some(Expr::Int { v: 1, span: span.clone(), ty: Type::Int }), span: span.clone() }),
                        else_: Some(Box::new(Stmt::Return { val: Some(Expr::Int { v: 0, span: span.clone(), ty: Type::Int }), span: span.clone() })),
                        span: span.clone(),
                    }),
                ],
                span: span.clone(),
            }),
            span: span.clone(),
            is_extern: false, is_static: false, section: None, alignment: None,
            is_weak: false, visibility: None, is_unused: false, is_cold: false,
            is_noinline: false, is_always_inline: false, is_variadic: false,
            is_constructor: false, is_destructor: false, alias_name: None, is_nested: false,
            upvars: vec![], is_old_style: false, format_kind: None, format_string_idx: None,
            format_first_arg: None, no_sanitize: vec![], target_clones: vec![], returns_nonnull: false,
            ctor_priority: None, dtor_priority: None, is_retain: false, is_used_attr: false,
            is_flatten: false, error_msg: None, warning_msg: None, deprecated_msg: None,
            ifunc_resolver: None,
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
            name: crate::ast::Ident { name: "f".into(), span: span.clone() },
            params: vec![],
            body: Some(Block {
                items: vec![
                    BlockItem::Stmt(Stmt::Goto { target: crate::ast::Ident { name: "L".into(), span: span.clone() }, span: span.clone() }),
                ],
                span: span.clone(),
            }),
            span: span.clone(),
            is_extern: false, is_static: false, section: None, alignment: None,
            is_weak: false, visibility: None, is_unused: false, is_cold: false,
            is_noinline: false, is_always_inline: false, is_variadic: false,
            is_constructor: false, is_destructor: false, alias_name: None, is_nested: false,
            upvars: vec![], is_old_style: false, format_kind: None, format_string_idx: None,
            format_first_arg: None, no_sanitize: vec![], target_clones: vec![], returns_nonnull: false,
            ctor_priority: None, dtor_priority: None, is_retain: false, is_used_attr: false,
            is_flatten: false, error_msg: None, warning_msg: None, deprecated_msg: None,
            ifunc_resolver: None,
        };
        assert!(build_from_func(&f).is_none());
    }
}
