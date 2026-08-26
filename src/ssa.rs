// src/ssa.rs - 轻量 SSA IR (M16 优化 pass 基础设施)
//
// 设计原则 (suckless):
//   - 只覆盖结构化控制流 (if/while/for/do-while/return)。含 goto/label/asm-goto
//     /switch(Switch 多 case 简化为 if-chains)/longjmp 的函数降级回 AST pass。
//   - 单一文件: BB / CFG / SSA 值 / phi / dominator 都在这里。
//   - 零外部依赖,仅 std。
//   - IR 形态: 每个变量按 SSA 版本号命名 (base_N); phi 节点在 merge BB 顶端。

use std::collections::{HashMap, HashSet};

use crate::ast::{BinOp, Block, BlockItem, Decl, Expr, FuncDecl, QualType, Stmt, Type, UnOp};
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
    IndexStore {
        arr: Operand,
        idx: Operand,
        src: Operand,
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
    },
    /// `obj.field = src`
    MemberStore {
        obj: Operand,
        /// 基址表达式的类型
        obj_ty: Type,
        field: String,
        src: Operand,
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
        ty: Type,
    },
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
    /// 原始 AST 中声明的局部变量名 (用于 rebuild_block 避免重复声明)
    pub original_locals: HashSet<String>,
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
            original_locals: HashSet::new(),
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
            Decl::Func(f) => f.body.as_ref().is_none_or(is_structured),
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
            is_structured_stmt(then_) && else_.as_ref().is_none_or(|e| is_structured_stmt(e))
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
            }
            Expr::Float { v, .. } => {
                if let Some(t) = target {
                    self.emit(Inst {
                        dest: Some(t.clone()),
                        kind: InstKind::Bin {
                            op: BinOp::Add,
                            l: Operand::Float(*v),
                            r: Operand::Int(0),
                        },
                        span: t.span.clone(),
                    });
                    self.define(t.clone());
                    return Operand::Var(t);
                }
                Operand::Float(*v)
            }
            Expr::String { data, .. } => Operand::Str(data.clone()),
            Expr::Char { v, .. } => Operand::Int(*v as i128),
            Expr::Ident {
                name,
                span: _,
                ty: _,
            } => {
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
            }
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
            }
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
            }
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
            }
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
            }
            Expr::Comma { l: _, r, span, ty } => {
                // 丢弃 lhs (在 SSA 化中已通过指令序列完成)
                let _ = span;
                let o = self.lower_expr(r);
                let _ = ty;
                o
            }
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
            }
            Expr::Index { arr, idx, span, ty } => {
                let a = self.lower_expr(arr);
                let i = self.lower_expr(idx);
                let dest = target.unwrap_or_else(|| self.fresh("idx", ty.clone(), span));
                self.emit(Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::IndexLoad { arr: a, idx: i },
                    span: span.clone(),
                });
                self.define(dest.clone());
                Operand::Var(dest)
            }
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
                if *is_arrow {
                    // *obj.field: 先解引用指针, 再访问成员
                    let deref_ty = match &obj_ty {
                        Type::Ptr(inner) => inner.base.clone(),
                        other => other.clone(),
                    };
                    let inner = self.fresh("deref", Type::Ptr(Box::new(QualType::new(ty.clone()))), span);
                    self.emit(Inst {
                        dest: Some(inner.clone()),
                        kind: InstKind::Load {
                            e: obj,
                            ty: ty.clone(),
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
                        },
                        span: span.clone(),
                    });
                }
                self.define(dest.clone());
                Operand::Var(dest)
            }
            Expr::Deref { e, span, ty } => {
                let o = self.lower_expr(e);
                let dest = target.unwrap_or_else(|| self.fresh("deref", ty.clone(), span));
                self.emit(Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::Load {
                        e: o,
                        ty: ty.clone(),
                    },
                    span: span.clone(),
                });
                self.define(dest.clone());
                Operand::Var(dest)
            }
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
            }
            Expr::PostInc { e, span, ty: _ }
            | Expr::PreInc { e, span, ty: _ }
            | Expr::PostDec { e, span, ty: _ }
            | Expr::PreDec { e, span, ty: _ } => {
                let is_inc = matches!(&**e, Expr::PostInc { .. } | Expr::PreInc { .. });
                let one = if is_inc { 1i128 } else { -1i128 };
                if let Expr::Ident { name, ty, .. } = e.as_ref() {
                    let cur_v = self.lookup(&name.name).unwrap_or_else(|| SsaVar {
                        base: name.name.clone(),
                        version: 0,
                        ty: ty.clone(),
                        span: span.clone(),
                    });
                    let one_op = Operand::Int(one);
                    let new_dest = self.fresh("upd", ty.clone(), span);
                    self.emit(Inst {
                        dest: Some(new_dest.clone()),
                        kind: InstKind::Bin {
                            op: BinOp::Add,
                            l: Operand::Var(cur_v.clone()),
                            r: one_op,
                        },
                        span: span.clone(),
                    });
                    // store back
                    self.emit(Inst {
                        dest: None,
                        kind: InstKind::Store {
                            e: Operand::SymAddr(name.name.clone()),
                            src: Operand::Var(new_dest.clone()),
                        },
                        span: span.clone(),
                    });
                    let nv = SsaVar {
                        base: name.name.clone(),
                        version: self.next_version(&name.name),
                        ty: ty.clone(),
                        span: span.clone(),
                    };
                    self.define(nv.clone());
                    // override previous
                    let _ = cur_v;
                    let v = self.fresh("post", ty.clone(), span);
                    self.emit(Inst {
                        dest: Some(v.clone()),
                        kind: InstKind::Bin {
                            op: BinOp::Add,
                            l: Operand::Var(cur_v),
                            r: Operand::Int(0),
                        },
                        span: span.clone(),
                    });
                    return Operand::Var(v);
                }
                Operand::Int(one)
            }
            Expr::SizeofTy { ty, .. } => Operand::Int(ty.size() as i128),
            Expr::SizeofExpr { e, .. } => match e.as_ref() {
                // 字符串字面量: C 中为 char[N] 数组 (含 NUL), 返回实际字节数
                Expr::String { data, .. } => Operand::Int(data.len() as i128),
                _ => Operand::Int(e.ty().size() as i128),
            },
            Expr::AlignofTy { ty, .. } => Operand::Int(ty.align() as i128),
            Expr::AlignofExpr { e, .. } => Operand::Int(e.ty().align() as i128),
            Expr::AsmExpr {
                asm_str,
                outputs,
                output_names,
                output_constraints,
                inputs,
                input_names,
                input_constraints,
                clobbers,
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
            }
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
            }
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
            }
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
            }
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
                }
                _ => {}
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
            }
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
            }
        }
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
            kind: InstKind::IndexStore {
                arr: a,
                idx: i,
                src,
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
fn collect_local_decls(items: &[BlockItem], out: &mut HashSet<String>) {
    for item in items {
        match item {
            BlockItem::Decl(Decl::Var(v)) => {
                out.insert(v.name.name.clone());
            }
            BlockItem::Stmt(s) => collect_local_decls_stmt(s, out),
            _ => {}
        }
    }
}

fn collect_local_decls_stmt(s: &Stmt, out: &mut HashSet<String>) {
    match s {
        Stmt::Block { b, .. } => collect_local_decls(&b.items, out),
        Stmt::If { then_, else_, .. } => {
            collect_local_decls_stmt(then_, out);
            if let Some(e) = else_ {
                collect_local_decls_stmt(e, out);
            }
        }
        Stmt::While { body, .. } | Stmt::For { body, .. } | Stmt::Do { body, .. } => {
            collect_local_decls_stmt(body, out);
        }
        _ => {}
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
    // Collect original local variable names for rebuild_block
    collect_local_decls(&body.items, &mut func.original_locals);
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
    for name in &func.original_locals {
        if param_names.contains(name) {
            continue;
        }
        // 找到该变量的类型
        let ty = func
            .blocks
            .values()
            .flat_map(|bb| bb.insts.iter())
            .find_map(|inst| {
                inst.dest
                    .as_ref()
                    .filter(|d| &d.base == name)
                    .map(|d| d.ty.clone())
            })
            .unwrap_or(Type::Int);
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
            quals: crate::ast::QualifierFlags::NONE,
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
            _ => {}
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
                                }
                                Terminator::Branch { cond, .. } => {
                                    count += op_count_in_op(cond, &ssa_name);
                                }
                                _ => {}
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
                        let resolved = inst_to_expr_resolved(&inst.kind, &subst, &inst.span);
                        subst.insert(ssa_name, Some(resolved));
                    } else {
                        subst.insert(ssa_name, None);
                    }
                }
            }
        }
    }

    rebuild_bb(&entry, func, &mut items, &mut visited, &mut env, &subst);
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
        e: Operand::SymAddr(name),
        ..
    } = kind
    {
        name.starts_with("__dcc_") && !name.starts_with("__dcc_section_")
    } else {
        false
    }
}

/// 指令是否带有不可消除的副作用 (Call / Store / InlineAsm 等)。
/// SSA→AST 重建时, 即使其结果未被使用也不能整条丢弃。
fn inst_has_side_effect(kind: &InstKind) -> bool {
    matches!(
        kind,
        InstKind::Call { .. }
            | InstKind::Store { .. }
            | InstKind::IndexStore { .. }
            | InstKind::MemberStore { .. }
            | InstKind::InlineAsm { .. }
    )
}

fn rebuild_bb(
    bb_name: &str,
    func: &Function,
    out: &mut Vec<BlockItem>,
    visited: &mut HashSet<String>,
    env: &mut HashMap<String, SsaVar>,
    subst: &HashMap<String, Option<Expr>>,
) {
    if !visited.insert(bb_name.to_string()) {
        return;
    }
    let bb = match func.blocks.get(bb_name) {
        Some(b) => b.clone(),
        None => return,
    };
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
        // 跳过 SSA pass 插入的 hint/metadata 指令 (register_allocation,
        // auto_parallelization, pgo 等). 这些指令以 Store 到 __dcc_ 前缀
        // 符号的形式存在, 不对应真实计算, 在 SSA→AST 重建时必须丢弃.
        if is_pass_hint_inst(&inst.kind) {
            continue;
        }
        if let Some(d) = &inst.dest {
            env.insert(d.name(), d.clone());
            let ssa_name = d.name();
            // 如果被标记为内联, 跳过赋值 (表达式已在 subst 中)。
            // 有副作用的指令 (如返回值被丢弃的 Call) 只在确实有使用点时
            // 允许内联——结果未使用 (use_count==0) 时 subst 里存的表达式
            // 没有任何使用点消费, 跳过会导致副作用丢失 (fesetround 被删).
            if let Some(Some(_)) = subst.get(&ssa_name) {
                continue;
            }
            // 发射赋值: base_name = resolved_rhs
            let rhs = inst_to_expr_resolved(&inst.kind, subst, &inst.span);
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
            let e = inst_to_expr_resolved(&inst.kind, subst, &inst.span);
            out.push(BlockItem::Stmt(Stmt::Expr {
                e,
                span: inst.span.clone(),
            }));
        }
    }
    // 2. 翻译 terminator -> AST 后, 继续后继
    match &bb.term {
        Terminator::Ret { val } => {
            let e = val.as_ref().map(|o| resolve_operand(o, env, subst));
            out.push(BlockItem::Stmt(Stmt::Return {
                val: e,
                span: func.span.clone(),
            }));
        }
        Terminator::Jump(t) => {
            rebuild_bb(t, func, out, visited, env, subst);
        }
        Terminator::Branch { cond, t, f } => {
            let cond_e = resolve_operand(cond, env, subst);
            let mut then_items: Vec<BlockItem> = Vec::new();
            let mut then_env = env.clone();
            let then_subst = subst.clone();
            rebuild_bb(
                t,
                func,
                &mut then_items,
                visited,
                &mut then_env,
                &then_subst,
            );
            let mut else_items: Vec<BlockItem> = Vec::new();
            let mut else_env = env.clone();
            let else_subst = subst.clone();
            rebuild_bb(
                f,
                func,
                &mut else_items,
                visited,
                &mut else_env,
                &else_subst,
            );
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
        }
        Terminator::Unreachable => {}
    }
}

/// 解析操作数: 先查 subst 内联映射, 再回退到基础名。
fn resolve_operand(
    o: &Operand,
    env: &HashMap<String, SsaVar>,
    subst: &HashMap<String, Option<Expr>>,
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
        }
        _ => operand_to_expr(o, env),
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
        }
        Expr::Deref { ty, .. } => {
            *ty = desired_ty.clone();
        }
        _ => {}
    }
}

/// 将 SSA 指令转换为 AST 表达式, 同时应用内联替换。
/// `inst_span` 是原始 SSA 指令的 span, 用于保留源码位置信息。
fn inst_to_expr_resolved(
    kind: &InstKind,
    subst: &HashMap<String, Option<Expr>>,
    inst_span: &Span,
) -> Expr {
    let sp = inst_span.clone();
    let fake_env: HashMap<String, SsaVar> = HashMap::new();
    match kind {
        InstKind::Bin { op, l, r } => Expr::Binop {
            op: *op,
            l: Box::new(resolve_operand(l, &fake_env, subst)),
            r: Box::new(resolve_operand(r, &fake_env, subst)),
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::Un { op, e } => Expr::Unary {
            op: *op,
            e: Box::new(resolve_operand(e, &fake_env, subst)),
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::Cast { ty, e } => Expr::Cast {
            ty: ty.clone(),
            e: Box::new(resolve_operand(e, &fake_env, subst)),
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
        InstKind::Load { e, ty } => Expr::Deref {
            e: Box::new(resolve_operand(e, &fake_env, subst)),
            span: sp.clone(),
            ty: ty.clone(),
        },
        InstKind::Store { e, src } => Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Deref {
                e: Box::new(resolve_operand(e, &fake_env, subst)),
                span: sp.clone(),
                ty: Type::Void,
            }),
            r: Box::new(resolve_operand(src, &fake_env, subst)),
            span: sp.clone(),
            ty: Type::Void,
        },
        InstKind::IndexLoad { arr, idx } => Expr::Index {
            arr: Box::new(resolve_operand(arr, &fake_env, subst)),
            idx: Box::new(resolve_operand(idx, &fake_env, subst)),
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::IndexStore { arr, idx, src } => Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Index {
                arr: Box::new(resolve_operand(arr, &fake_env, subst)),
                idx: Box::new(resolve_operand(idx, &fake_env, subst)),
                span: sp.clone(),
                ty: Type::Int,
            }),
            r: Box::new(resolve_operand(src, &fake_env, subst)),
            span: sp.clone(),
            ty: Type::Void,
        },
        InstKind::Call {
            callee,
            args,
            ret_ty,
        } => Expr::Call {
            callee: Box::new(resolve_operand(callee, &fake_env, subst)),
            args: args
                .iter()
                .map(|a| resolve_operand(a, &fake_env, subst))
                .collect(),
            span: sp.clone(),
            ty: ret_ty.clone(),
        },
        InstKind::MemberLoad {
            obj,
            obj_ty,
            field,
            ty,
        } => {
            let mut base = resolve_operand(obj, &fake_env, subst);
            // 重建时修正基址类型: SSA 变量可能丢失了原始结构体类型
            set_expr_ty_if_compatible(&mut base, obj_ty);
            Expr::Member {
                expr: Box::new(base),
                field: crate::ast::Ident {
                    name: field.clone(),
                    span: sp.clone(),
                },
                span: sp.clone(),
                ty: ty.clone(),
                is_arrow: false,
            }
        }
        InstKind::MemberStore {
            obj,
            obj_ty,
            field,
            src,
        } => {
            let mut base = resolve_operand(obj, &fake_env, subst);
            set_expr_ty_if_compatible(&mut base, obj_ty);
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
                    is_arrow: false,
                }),
                r: Box::new(resolve_operand(src, &fake_env, subst)),
                span: sp.clone(),
                ty: Type::Void,
            }
        }
        InstKind::Select { cond, then_, else_ } => Expr::Cond {
            cond: Box::new(resolve_operand(cond, &fake_env, subst)),
            then_: Box::new(resolve_operand(then_, &fake_env, subst)),
            else_: Box::new(resolve_operand(else_, &fake_env, subst)),
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
                    .map(|o| resolve_operand(o, &fake_env, subst))
                    .collect(),
                input_names: input_names.clone(),
                input_constraints: input_constraints.clone(),
                clobbers: clobbers.clone(),
                span: sp.clone(),
                ty: ty.clone(),
            }
        }
    }
}

/// 收集 InstKind 中所有被引用的 SSA 变量名。
fn collect_op_uses(kind: &InstKind, out: &mut HashSet<String>) {
    match kind {
        InstKind::Bin { l, r, .. } => {
            collect_op_uses_op(l, out);
            collect_op_uses_op(r, out);
        }
        InstKind::Un { e, .. } => collect_op_uses_op(e, out),
        InstKind::Cast { e, .. } => collect_op_uses_op(e, out),
        InstKind::Load { e, .. } => collect_op_uses_op(e, out),
        InstKind::Store { e, src } => {
            collect_op_uses_op(e, out);
            collect_op_uses_op(src, out);
        }
        InstKind::IndexLoad { arr, idx } => {
            collect_op_uses_op(arr, out);
            collect_op_uses_op(idx, out);
        }
        InstKind::IndexStore { arr, idx, src } => {
            collect_op_uses_op(arr, out);
            collect_op_uses_op(idx, out);
            collect_op_uses_op(src, out);
        }
        InstKind::Call { callee, args, .. } => {
            collect_op_uses_op(callee, out);
            for a in args {
                collect_op_uses_op(a, out);
            }
        }
        InstKind::MemberLoad { obj, .. } => collect_op_uses_op(obj, out),
        InstKind::MemberStore { obj, src, .. } => {
            collect_op_uses_op(obj, out);
            collect_op_uses_op(src, out);
        }
        InstKind::Select { cond, then_, else_ } => {
            collect_op_uses_op(cond, out);
            collect_op_uses_op(then_, out);
            collect_op_uses_op(else_, out);
        }
        InstKind::InlineAsm { inputs, .. } => {
            for input in inputs {
                collect_op_uses_op(input, out);
            }
        }
        _ => {}
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
        }
        InstKind::Un { e, .. } => count += op_count_in_op(e, name),
        InstKind::Cast { e, .. } => count += op_count_in_op(e, name),
        InstKind::Load { e, .. } => count += op_count_in_op(e, name),
        InstKind::Store { e, src } => {
            count += op_count_in_op(e, name);
            count += op_count_in_op(src, name);
        }
        InstKind::IndexLoad { arr, idx } => {
            count += op_count_in_op(arr, name);
            count += op_count_in_op(idx, name);
        }
        InstKind::IndexStore { arr, idx, src } => {
            count += op_count_in_op(arr, name);
            count += op_count_in_op(idx, name);
            count += op_count_in_op(src, name);
        }
        InstKind::Call { callee, args, .. } => {
            count += op_count_in_op(callee, name);
            for a in args {
                count += op_count_in_op(a, name);
            }
        }
        InstKind::MemberLoad { obj, .. } => count += op_count_in_op(obj, name),
        InstKind::MemberStore { obj, src, .. } => {
            count += op_count_in_op(obj, name);
            count += op_count_in_op(src, name);
        }
        InstKind::Select { cond, then_, else_ } => {
            count += op_count_in_op(cond, name);
            count += op_count_in_op(then_, name);
            count += op_count_in_op(else_, name);
        }
        InstKind::InlineAsm { inputs, .. } => {
            for input in inputs {
                count += op_count_in_op(input, name);
            }
        }
        _ => {}
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

fn operand_to_expr(o: &Operand, env: &HashMap<String, SsaVar>) -> Expr {
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
        }
        Operand::Int(v) => Expr::Int {
            v: *v,
            span: Span::point("ssa", 0, 0),
            ty: Type::Int,
        },
        Operand::Float(v) => Expr::Float {
            v: *v,
            span: Span::point("ssa", 0, 0),
            ty: Type::Double,
        },
        Operand::Str(d) => Expr::String {
            data: d.clone(),
            span: Span::point("ssa", 0, 0),
            ty: Type::Array(Box::new(QualType::new(Type::Char)), crate::ast::ArraySize::Const(d.len())),
        },
        Operand::SymAddr(n) => Expr::Ident {
            name: crate::ast::Ident {
                name: n.clone(),
                span: Span::point("ssa", 0, 0),
            },
            span: Span::point("ssa", 0, 0),
            ty: Type::Ptr(Box::new(QualType::new(Type::Void))),
        },
        Operand::FnAddr(n) => Expr::Ident {
            name: crate::ast::Ident {
                name: n.clone(),
                span: Span::point("ssa", 0, 0),
            },
            span: Span::point("ssa", 0, 0),
            ty: Type::Ptr(Box::new(QualType::new(Type::Void))),
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
fn inst_to_expr(kind: &InstKind, env: &HashMap<String, SsaVar>) -> Expr {
    let sp = Span::point("ssa", 0, 0);
    match kind {
        InstKind::Bin { op, l, r } => Expr::Binop {
            op: *op,
            l: Box::new(operand_to_expr(l, env)),
            r: Box::new(operand_to_expr(r, env)),
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::Un { op, e } => Expr::Unary {
            op: *op,
            e: Box::new(operand_to_expr(e, env)),
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::Cast { ty, e } => Expr::Cast {
            ty: ty.clone(),
            e: Box::new(operand_to_expr(e, env)),
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
        InstKind::Load { e, ty } => Expr::Deref {
            e: Box::new(operand_to_expr(e, env)),
            span: sp.clone(),
            ty: ty.clone(),
        },
        InstKind::Store { e, src } => Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Deref {
                e: Box::new(operand_to_expr(e, env)),
                span: sp.clone(),
                ty: Type::Void,
            }),
            r: Box::new(operand_to_expr(src, env)),
            span: sp.clone(),
            ty: Type::Void,
        },
        InstKind::IndexLoad { arr, idx } => Expr::Index {
            arr: Box::new(operand_to_expr(arr, env)),
            idx: Box::new(operand_to_expr(idx, env)),
            span: sp.clone(),
            ty: Type::Int,
        },
        InstKind::IndexStore { arr, idx, src } => Expr::Assign {
            op: BinOp::Add,
            l: Box::new(Expr::Index {
                arr: Box::new(operand_to_expr(arr, env)),
                idx: Box::new(operand_to_expr(idx, env)),
                span: sp.clone(),
                ty: Type::Int,
            }),
            r: Box::new(operand_to_expr(src, env)),
            span: sp.clone(),
            ty: Type::Void,
        },
        InstKind::Call {
            callee,
            args,
            ret_ty,
        } => Expr::Call {
            callee: Box::new(operand_to_expr(callee, env)),
            args: args.iter().map(|a| operand_to_expr(a, env)).collect(),
            span: sp.clone(),
            ty: ret_ty.clone(),
        },
        InstKind::MemberLoad {
            obj,
            obj_ty,
            field,
            ty,
        } => {
            let mut base = operand_to_expr(obj, env);
            set_expr_ty_if_compatible(&mut base, obj_ty);
            Expr::Member {
                expr: Box::new(base),
                field: crate::ast::Ident {
                    name: field.clone(),
                    span: sp.clone(),
                },
                span: sp.clone(),
                ty: ty.clone(),
                is_arrow: false,
            }
        }
        InstKind::MemberStore {
            obj,
            obj_ty,
            field,
            src,
        } => {
            let mut base = operand_to_expr(obj, env);
            set_expr_ty_if_compatible(&mut base, obj_ty);
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
                    is_arrow: false,
                }),
                r: Box::new(operand_to_expr(src, env)),
                span: sp.clone(),
                ty: Type::Void,
            }
        }
        InstKind::Select { cond, then_, else_ } => Expr::Cond {
            cond: Box::new(operand_to_expr(cond, env)),
            then_: Box::new(operand_to_expr(then_, env)),
            else_: Box::new(operand_to_expr(else_, env)),
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
                .map(|a| operand_to_expr(a, env))
                .collect(),
            input_names: input_names.clone(),
            input_constraints: input_constraints.clone(),
            clobbers: clobbers.clone(),
            span: sp.clone(),
            ty: ty.clone(),
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
            s.push_str(&format!(
                "  {}: preds={:?} succs={:?}\n",
                bb.name, bb.preds, bb.succs
            ));
            for phi in &bb.phis {
                s.push_str(&format!(
                    "    phi {} <- {:?}\n",
                    phi.dest.name(),
                    phi.incoming
                ));
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
        };
        assert!(build_from_func(&f).is_none());
    }
}
