// src/node_pool.rs - Typed AST node arena
//
// 第一性原理: 解析阶段大量创建小节点, 默认 Box/Vec/String 分配开销大.
// 把每种 AST 节点按 variant 类型分桶放在连续向量中, 减少分配次数并改善缓存局部性.
//
// 这是 ADDITIVE 基础设施 - 不改变现有 Expr / Stmt / Decl / Program 类型.

use crate::ast::{Decl, Expr, Stmt};

/// 表达式节点 handle: pool_id 指向 variant 桶, node_id 指向桶内位置.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PooledExpr {
    pub pool_id: u32,
    pub node_id: u32,
}

/// 语句节点 handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PooledStmt {
    pub pool_id: u32,
    pub node_id: u32,
}

/// 声明节点 handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PooledDecl {
    pub pool_id: u32,
    pub node_id: u32,
}

/// 按 Expr variant 类型分桶的内部表达.
type ExprBuckets = Vec<Vec<Expr>>;
type StmtBuckets = Vec<Vec<Stmt>>;
type DeclBuckets = Vec<Vec<Decl>>;

/// 通用 AST 节点 arena. 每种节点类型 (Expr / Stmt / Decl) 按 variant 分桶存储.
/// 使用 `Vec<Vec<T>>` 而非单个 `Vec<T>` 的原因: 同一 variant 的节点大小相近,
/// 连续分配可获得更好的 cache 局部性, 同时避免按 variant 分支遍历时跨越大对象.
#[derive(Debug, Default)]
pub struct NodePool {
    exprs: ExprBuckets,
    stmts: StmtBuckets,
    decls: DeclBuckets,
}

// SAFETY: NodePool 仅持有 owned AST 节点 (Expr/Stmt/Decl), 这些类型
// 内部只含 Box / Vec / String / Arc / Span, 均为 Send + Sync.
unsafe impl Send for NodePool {}
unsafe impl Sync for NodePool {}

impl NodePool {
    /// 创建空 arena. 保留 `cap` 参数仅为 API 一致 (未来可预分配桶容量).
    /// 当前实现按需懒建桶, 行为上等价于空 arena.
    pub fn with_capacity(cap: usize) -> Self {
        let _ = cap;
        Self::default()
    }

    /// 分配一个表达式, 返回 handle.
    pub fn alloc_expr(&mut self, e: Expr) -> PooledExpr {
        let pool_id = bucket_id(&e);
        while self.exprs.len() <= pool_id as usize {
            self.exprs.push(Vec::new());
        }
        let bucket = &mut self.exprs[pool_id as usize];
        let node_id = bucket.len() as u32;
        bucket.push(e);
        PooledExpr { pool_id, node_id }
    }

    /// 分配一个语句, 返回 handle.
    pub fn alloc_stmt(&mut self, s: Stmt) -> PooledStmt {
        let pool_id = stmt_bucket_id(&s);
        while self.stmts.len() <= pool_id as usize {
            self.stmts.push(Vec::new());
        }
        let bucket = &mut self.stmts[pool_id as usize];
        let node_id = bucket.len() as u32;
        bucket.push(s);
        PooledStmt { pool_id, node_id }
    }

    /// 分配一个声明, 返回 handle.
    pub fn alloc_decl(&mut self, d: Decl) -> PooledDecl {
        let pool_id = decl_bucket_id(&d);
        while self.decls.len() <= pool_id as usize {
            self.decls.push(Vec::new());
        }
        let bucket = &mut self.decls[pool_id as usize];
        let node_id = bucket.len() as u32;
        bucket.push(d);
        PooledDecl { pool_id, node_id }
    }

    /// 通过 handle 取回表达式的不可变引用.
    /// 若 handle 越界则 panic, 与标准容器越界行为一致.
    pub fn get_expr(&self, h: PooledExpr) -> &Expr {
        &self.exprs[h.pool_id as usize][h.node_id as usize]
    }

    /// 通过 handle 取回语句的不可变引用.
    pub fn get_stmt(&self, h: PooledStmt) -> &Stmt {
        &self.stmts[h.pool_id as usize][h.node_id as usize]
    }

    /// 通过 handle 取回声明的不可变引用.
    pub fn get_decl(&self, h: PooledDecl) -> &Decl {
        &self.decls[h.pool_id as usize][h.node_id as usize]
    }

    /// 返回当前已分配的表达式总数 (跨所有 variant 桶).
    pub fn expr_count(&self) -> usize {
        self.exprs.iter().map(|b| b.len()).sum()
    }

    /// 返回当前已分配的语句总数.
    pub fn stmt_count(&self) -> usize {
        self.stmts.iter().map(|b| b.len()).sum()
    }

    /// 返回当前已分配的声明总数.
    pub fn decl_count(&self) -> usize {
        self.decls.iter().map(|b| b.len()).sum()
    }
}

// ---------- 内部: variant -> bucket id 映射 ----------
// 用手工编号 (而非 std::mem::discriminant) 是为了:
//   1. 不依赖运行时内存地址 (PIE/ASLR 下稳定)
//   2. 桶 id 在序列化/日志中可读
//   3. 未来若 discriminant 顺序变化, 这里可独立调整而不影响 handle 兼容性 (handle 由
//      (pool_id, node_id) 组成, 同一进程内可任意重排).

#[allow(unreachable_patterns)]
fn bucket_id(e: &Expr) -> u32 {
    match e {
        Expr::Int { .. } => 0,
        Expr::Float { .. } => 1,
        Expr::ComplexLit { .. } | Expr::ComplexIntLit { .. } => 2,
        Expr::Real { .. } => 3,
        Expr::Imag { .. } => 4,
        Expr::String { .. } => 5,
        Expr::Char { .. } => 6,
        Expr::Ident { .. } => 7,
        Expr::Binop { .. } => 8,
        Expr::Unary { .. } => 9,
        Expr::Call { .. } => 10,
        Expr::Assign { .. } => 11,
        Expr::Cond { .. } => 12,
        Expr::Cast { .. } => 13,
        Expr::SizeofTy { .. } => 14,
        Expr::SizeofExpr { .. } => 15,
        Expr::Index { .. } => 16,
        Expr::Member { .. } => 17,
        Expr::AddrOf { .. } => 18,
        Expr::Deref { .. } => 19,
        Expr::PostInc { .. } => 20,
        Expr::PostDec { .. } => 21,
        Expr::PreInc { .. } => 22,
        Expr::PreDec { .. } => 23,
        Expr::CompoundLit { .. } => 24,
        Expr::InitList { .. } => 25,
        Expr::AlignofTy { .. } => 26,
        Expr::AlignofExpr { .. } => 27,
        Expr::StmtExpr { .. } => 28,
        Expr::Comma { .. } => 29,
        Expr::AsmExpr { .. } => 30,
        Expr::LabelAddr { .. } => 31,
        Expr::VaArg { .. } => 32,
        Expr::Offsetof { .. } => 33,
        _ => 34, // 兜底: 未来新增 variant 也能工作 (落到公共桶)
    }
}

#[allow(unreachable_patterns)]
fn stmt_bucket_id(s: &Stmt) -> u32 {
    match s {
        Stmt::Return { .. } => 0,
        Stmt::Expr { .. } => 1,
        Stmt::If { .. } => 2,
        Stmt::While { .. } => 3,
        Stmt::Do { .. } => 4,
        Stmt::For { .. } => 5,
        Stmt::Block { .. } => 6,
        Stmt::Break { .. } => 7,
        Stmt::Continue { .. } => 8,
        Stmt::Goto { .. } => 9,
        Stmt::GotoIndirect { .. } => 10,
        Stmt::Label { .. } => 11,
        Stmt::Switch { .. } => 12,
        Stmt::AsmGoto { .. } => 13,
        Stmt::Fallthrough { .. } => 14,
        _ => 15, // 兜底
    }
}

#[allow(unreachable_patterns)]
fn decl_bucket_id(d: &Decl) -> u32 {
    match d {
        Decl::Var(_) => 0,
        Decl::Func(_) => 1,
        Decl::StructDef(_) => 2,
        Decl::UnionDef(_) => 3,
        Decl::EnumDef(_) => 4,
        Decl::Typedef { .. } => 5,
        Decl::GlobalAsm { .. } => 6,
        _ => 7, // 兜底
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Decl, Expr, Ident, Stmt, Type, VarDecl};
    use crate::diagnostics::Span;

    fn dummy_span() -> Span {
        Span::point("test", 0, 0)
    }

    fn make_int_expr(v: i64) -> Expr {
        Expr::Int {
            v: v as i128,
            span: dummy_span(),
            ty: Type::Int,
        }
    }

    fn make_return_stmt() -> Stmt {
        Stmt::Return {
            val: Some(make_int_expr(0)),
            span: dummy_span(),
        }
    }

    #[test]
    fn default_is_empty() {
        let pool = NodePool::default();
        assert_eq!(pool.expr_count(), 0);
        assert_eq!(pool.stmt_count(), 0);
        assert_eq!(pool.decl_count(), 0);
    }

    #[test]
    fn with_capacity_is_empty() {
        let pool = NodePool::with_capacity(16);
        assert_eq!(pool.expr_count(), 0);
        assert_eq!(pool.stmt_count(), 0);
        assert_eq!(pool.decl_count(), 0);
    }

    #[test]
    fn alloc_expr_then_get_returns_same() {
        let mut pool = NodePool::default();
        let e = make_int_expr(42);
        let h = pool.alloc_expr(e.clone());
        let got = pool.get_expr(h);
        // Expr 不实现 PartialEq, 用字段访问验证.
        match (got, &e) {
            (Expr::Int { v: a, .. }, Expr::Int { v: b, .. }) => assert_eq!(a, b),
            _ => panic!("expected Int variant"),
        }
    }

    #[test]
    fn alloc_many_exprs_works() {
        let mut pool = NodePool::default();
        let mut handles = Vec::new();
        for i in 0..1000 {
            handles.push(pool.alloc_expr(make_int_expr(i as i64)));
        }
        assert_eq!(pool.expr_count(), 1000);
        for (i, h) in handles.iter().enumerate() {
            if let Expr::Int { v, .. } = pool.get_expr(*h) {
                assert_eq!(*v, i as i128);
            } else {
                panic!("expected Int variant");
            }
        }
    }

    #[test]
    fn expr_count_reflects_allocations() {
        let mut pool = NodePool::default();
        assert_eq!(pool.expr_count(), 0);
        pool.alloc_expr(make_int_expr(1));
        assert_eq!(pool.expr_count(), 1);
        pool.alloc_expr(make_int_expr(2));
        assert_eq!(pool.expr_count(), 2);
    }

    #[test]
    fn stmt_alloc_and_get() {
        let mut pool = NodePool::default();
        let s = make_return_stmt();
        let h = pool.alloc_stmt(s.clone());
        let got = pool.get_stmt(h);
        // Stmt 不实现 PartialEq, 用字段访问验证.
        match (got, &s) {
            (Stmt::Return { val: a, .. }, Stmt::Return { val: b, .. }) => {
                assert!(a.is_some());
                assert!(b.is_some());
            },
            _ => panic!("expected Return variant"),
        }
        assert_eq!(pool.stmt_count(), 1);
    }

    #[test]
    fn decl_alloc_and_get() {
        let mut pool = NodePool::default();
        let d = Decl::Var(VarDecl {
            ty: Type::Int,
            name: Ident {
                name: "x".to_string(),
                span: dummy_span(),
            },
            init: None,
            span: dummy_span(),
            is_extern: false,
            is_static: false,
            section: None,
            alignment: None,
            is_weak: false,
            visibility: None,
            is_unused: false,
            register_name: None,
            home: None,
            cleanup_fn: None,
            is_thread_local: false,
            no_sanitize: Vec::new(),
            alias_name: None,
            copy_fn: None,
            is_retain: false,
            is_used_attr: false,
            quals: crate::ast::QualifierFlags::default(),
            addr_space: crate::ast::AddrSpace::None,
            tls_model: None,
            asm_label: None,
        });
        let h = pool.alloc_decl(d.clone());
        let got = pool.get_decl(h);
        // Decl 不实现 PartialEq, 用字段访问验证.
        match (got, &d) {
            (Decl::Var(a), Decl::Var(b)) => {
                assert_eq!(a.name.name, b.name.name);
                assert!(matches!(a.ty, Type::Int));
            },
            _ => panic!("expected Var variant"),
        }
        assert_eq!(pool.decl_count(), 1);
    }

    #[test]
    fn send_sync_bounds() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<NodePool>();
        assert_sync::<NodePool>();
    }
}
