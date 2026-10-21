// src/ssa/build.rs - AST -> SSA 转换器: 唯一入口与 `Builder`
//
// 放什么: `Builder` 结构及其 `impl` (表达式/语句 → SSA 的降级政策本体)、
//   `build_from_func` / `expr_shape` / `build_ssa` —— SSA pass 的唯一入口。
// 不放什么: 左值降级细节与 phi 插入在 `lower.rs`; 进 SSA 之前的 AST 侧政策
//   与降级在 `structure.rs`; SSA -> AST 的重建在 `rebuild.rs`/`expr_rebuild.rs`。
// 注: 这里的「入口」只负责把 AST 发射成 IR, 不代表它能把任意函数
//   忠实表达 —— 无法表达时由 `bail` 让整个函数留在 AST 通道。

use std::collections::{HashMap, HashSet};

use crate::ast::{BinOp, Block, BlockItem, Case, Decl, Expr, FuncDecl, QualType, Stmt, Type};
use crate::diagnostics::Span;

use crate::ssa::cfg::refresh_cfg;
use crate::ssa::inline_temps::is_synthetic_temp_base;
use crate::ssa::ir::{Function, Inst, InstKind, Operand, Phi, SsaVar, Terminator};
use crate::ssa::lower::{collect_local_decls, insert_label_phis, insert_loop_phis, lower_lvalue};
use crate::ssa::structure::is_structured;

pub(super) struct Builder {
    pub(super) func: Function,
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
    /// `goto *expr` 的源 BB。目标集合是**全部 label BB**，但 label 可能在 dispatch
    /// 之后才出现（典型就是解释器主循环：`goto *disptab[...]` 写在一堆 `L_OP_*:`
    /// 之前）。所以先记下源块，等整个函数降级完再一次性接边。
    pending_dispatch: Vec<String>,
    /// P0-4 Phase 2: 每个 label BB 入口处的变量版本快照 (label -> current 副本)
    label_var_snapshots: HashMap<String, HashMap<String, SsaVar>>,
    /// 本函数内**被取址**的对象根名 (由 `build_from_func` 的第一遍构建收集)。
    addr_taken: std::collections::HashSet<String>,
    /// 记下「该表达式无法在 SSA 里忠实表达」。非空 ⇒ `build_ssa` 放弃整个函数的
    /// SSA 化，该函数留在 AST 通道上。原因文本只在放弃这条冷路上构造。
    bail: Option<String>,
    /// analysis-only（`opt::run_ssa_passes` 的非结构化通道）：IR **不会被重建**，
    /// 只用来算活性/家。于是两类「急切求值」退避失效：它们的危害是重建把
    /// `Select`/右操作数发成无条件指令，而这条通道根本不重建，AST 仍是真分支。
    /// 反过来，真正会丢掉 def/use 的退避（聚合赋值、认不出的左值、无法降级的
    /// 表达式）在两条通道上都保留 —— 那些会让分析本身失真。
    analysis_only: bool,
}

impl Builder {
    fn new(func: Function, addr_taken: std::collections::HashSet<String>, analysis_only: bool) -> Self {
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
            pending_dispatch: Vec::new(),
            label_var_snapshots: HashMap::new(),
            addr_taken,
            bail: None,
            analysis_only,
        };
        b.func.blocks.get_mut(&b.cur).unwrap().term = Terminator::Unreachable;
        b
    }

    pub(super) fn fresh(&mut self, base: &str, ty: Type, span: &Span) -> SsaVar {
        self.tmp_count += 1;
        SsaVar {
            base: format!("_t{}_{}", base, self.tmp_count),
            version: 0,
            ty,
            span: span.clone(),
        }
    }

    pub(super) fn next_version(&mut self, base: &str) -> u32 {
        let v = self.versions.get(base).copied().unwrap_or(0);
        self.versions.insert(base.to_string(), v + 1);
        v + 1
    }

    fn lookup(&self, name: &str) -> Option<SsaVar> {
        self.current.get(name).cloned()
    }

    pub(super) fn define(&mut self, var: SsaVar) {
        self.current.insert(var.base.clone(), var);
    }

    /// 本表达式无法降级 ⇒ 记下原因，并返回一个**从不进产物**的占位值。
    ///
    /// 旧行为是静默 `Operand::Int(0)`：那个表达式在产物里真的变成 0，于是一批
    /// `_Complex` / `VaArg` / 复合字面量 / `&下标` `&成员` 形态在 -O2 静默误编译。
    /// 宁可整个函数不走 SSA，
    /// 也不保留「看着像优化、其实是错值」的形态。穷举的 catch-all 保证以后新增的
    /// `Expr` 变体也默认走这条路，而不是静默变 0。
    fn unsupported(&mut self, why: &str) {
        if self.bail.is_none() {
            self.bail = Some(why.to_string());
        }
    }

    /// 当前块已有的指令数（用来框出「刚降级出来的那段指令」）。
    fn cur_inst_count(&self) -> usize {
        self.func
            .blocks
            .get(&self.cur)
            .map(|b| b.insts.len())
            .unwrap_or(usize::MAX)
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
                InstKind::Un { .. }
                | InstKind::Cast { .. }
                | InstKind::Select { .. }
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
    pub(super) fn is_local_object(&self, name: &str) -> bool {
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
    pub(super) fn is_mem_only_object(&self, name: &str, ty: &Type) -> bool {
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
        let addr = self.fresh("naddr", Type::Ptr(Box::new(QualType::new(obj_ty.clone()))), span);
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
    pub(super) fn store_to_named_object(
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
        if matches!(obj_ty, Type::Array(_, _) | Type::Struct(_, _, _, _) | Type::Union(_, _, _, _))
        {
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
    pub(super) fn emit(&mut self, inst: Inst) {
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
    pub(super) fn lower_expr(&mut self, e: &Expr) -> Operand {
        self.lower_expr_inner(e, None)
    }

    /// 成员访问的**基址操作数**是不是「存放指针值的临时变量」（而不是对象本身）。
    ///
    /// `MemberLoad`/`MemberStore`/`MemberAddr` 的 `is_arrow` 与 `obj_ty` 必须跟着操作数
    /// 的**物化形态**走，而不是跟着源码写 `.` 还是 `->`：
    ///   * 下标基址（`g[0]`）经 `lower_lvalue_base_addr` 发 `IndexAddr`，交回指向元素的
    ///     指针；数组成员基址（`gd.a`）按 C 衰减交回首元素地址 —— 两者都是指针值。
    ///   * 这两种形态若继续按 dot 重建，codegen 会去取「那个指针变量自己的栈槽地址」再
    ///     加字段偏移 ⇒ 读与写都落在帧临时上，真对象一动不动（实测 -O2/-O3 下
    ///     `g[0].z = p`、`gd.a->z = p` 丢写、读回 0）。
    ///   * 具名数组对象（`ga`）是反例：它的操作数就是对象本身，codegen 直接取符号地址；
    ///     按 `Ptr` 重建反而变成「先 load 数组头 8 字节再当指针」⇒ 野指针（实测
    ///     `ga->z = p` 在 -O2/-O3 段错误）。
    ///
    /// 命中时 `obj_ty` 取 `Ptr(元素类型)`（数组基址的元素类型是数组的本体元素，下标基址
    /// 的就是下标结果类型），与交回的那个临时变量自身的类型一致，重建时两处类型不再互相
    /// 顶掉。
    ///
    /// 只有**聚合类型**的基址才需要这套改写：基址类型是标量（指针在内）时 `lower_expr`
    /// 交回的就是那个标量**值**，与源码形态本来就吻合，按 `Ptr(...)` 再包一层等于把
    /// 「指针槽的地址」当指针用。实测 `struct T *pa[3]; pa[i]->k = v` 在 -O2/-O3 下
    /// 读写落在槽位偏移上（`k` 读回 4210698 而不是 32）。
    ///
    /// 聚合基址里 `Member`/`Deref` 两种形态与 `Index` 同一条理由，且更致命：成员访存在
    /// SSA 层是 `MemberLoad`，基址类型是 struct/union 时它发的是**整块拷贝**（实测
    /// sqlite `yy_shift` 的 `yytos->minor.yy0 = yyMinor` 编成 `rep movsq` 16 字节到
    /// `-0x78(%r13)`），于是 `MemberStore` 写进的是帧临时里的副本、`yypParser->yystack`
    /// 一动不动 —— 整条 `o.a.b = v` / `p->u.f = v`（嵌套聚合成员赋值）在 -O2/-O3 丢写，
    /// 解析器随即拿垃圾 stateno/major 而崩。改走地址形态后由 `MemberAddr`/`IndexAddr`
    /// 逐层给出**对象地址**，拷贝仍归 codegen 的聚合成员写。
    pub(super) fn member_base_is_ptr_operand(base: &Expr, base_ty: &Type) -> bool {
        if matches!(base, Expr::Ident { .. }) {
            return false;
        }
        match base_ty {
            Type::Array(..) => true,
            Type::Struct(..) | Type::Union(..) => {
                matches!(base, Expr::Index { .. } | Expr::Member { .. } | Expr::Deref { .. })
            },
            _ => false,
        }
    }

    /// `&o.f` / `&p->f` 的**成员地址**降级 —— `InstKind::MemberAddr` 的唯一发射点。
    ///
    /// 基址的物化形态由 `member_base_is_ptr_operand` 决定（判据与实测见该函数）：交回
    /// 指针形态（计算下标、数组成员、嵌套聚合成员、解引用）时成员寻址走箭头，交回对象
    /// 形态（具名变量）时沿用源码写的 `.`/`->`。`Expr::Member` 的读分支与
    /// `lower_lvalue` 的写分支共用本函数的同一套归一化，避免「读对写错」这种只改一侧的
    /// 分歧（sqlite `yy_shift` 丢写即写侧另开一条按值降级所致）。
    pub(super) fn lower_member_addr(
        &mut self,
        expr: &Expr,
        field: &crate::ast::Ident,
        m_sp: &Span,
        f_ty: &Type,
        is_arrow: bool,
        target: Option<SsaVar>,
    ) -> Operand {
        let obj_ty = expr.ty();
        let base_is_ptr = Self::member_base_is_ptr_operand(expr, &obj_ty);
        let obj = if base_is_ptr {
            self.lower_lvalue_base_addr(expr)
        } else {
            self.lower_expr(expr)
        };
        let pointee = match &obj_ty {
            Type::Array(elem, _) => elem.base.clone(),
            other => other.clone(),
        };
        let (addr_obj_ty, addr_is_arrow) = if base_is_ptr {
            (Type::Ptr(Box::new(QualType::new(pointee))), true)
        } else {
            (obj_ty.clone(), is_arrow)
        };
        let ptr_ty = Type::Ptr(Box::new(QualType::new(f_ty.clone())));
        let dest = target.unwrap_or_else(|| self.fresh("subaddr", ptr_ty, m_sp));
        self.emit(Inst {
            dest: Some(dest.clone()),
            kind: InstKind::MemberAddr {
                obj,
                obj_ty: addr_obj_ty,
                field: field.name.clone(),
                ty: f_ty.clone(),
                is_arrow: addr_is_arrow,
            },
            span: m_sp.clone(),
        });
        self.define(dest.clone());
        Operand::Var(dest)
    }

    /// 求一个**左值表达式**的地址操作数，供 `IndexAddr` 的基址（`arr`）槽与
    /// `MemberAddr` 的基址（`obj`）槽使用。
    ///
    /// 与 `lower_expr` 的区别：当基址本身是「计算出来的**对象**」（内层下标或成员，
    /// 其结果类型是数组行 / 结构体 / 联合体，如 `&m[i][j]` 里的 `m[i]`、
    /// `&db->aDb[0].pBt` 里的 `aDb[0]`）时，`lower_expr` 会把该对象**当值载入**
    /// （数组行进帧临时、结构体整块拷贝）—— 那是错的，我们要的是对象的**地址**。
    /// 这里对这种形状递归发一条 `IndexAddr`（或委托 `lower_member_addr`）：元素类型取
    /// 内层表达式的结果类型，行步长交给 codegen 的 `AddrOf(Index)` 用 `sizeof(row)`
    /// 算，SSA 层不复制任何布局算术（单一真源）。
    /// **标量与指针元素是反例**（与 `Member` 分支同判据）：`u32 **apWiData` 的
    /// `apWiData[0]` 是 `u32 *` **值**，`&(apWiData[0][k])` 的基址是那个指针值，
    /// 必须走 `lower_expr` 的 `IndexLoad`。其余基址形态（具名数组/指针、解引用基址）
    /// 本就由各自的衰减分支给出地址，直接委托 `lower_expr`。
    pub(super) fn lower_lvalue_base_addr(&mut self, e: &Expr) -> Operand {
        if let Expr::Index {
            arr,
            idx,
            span,
            ty: row_ty,
        } = e
        {
            // 与下面 `Member` 分支同判据: 只有内层下标的**结果类型是对象**（数组行 /
            // 结构体 / 联合体元素）时, 基址才是「行/元素的地址」。标量与指针元素必须
            // 交出元素里的**值** —— `u32 **apWiData` 的 `apWiData[0]` 是 `u32 *` 值,
            // `&(apWiData[0][k])` 的下标基址是那个指针值, 不是 `&apWiData[0]`。
            // 实测 sqlite `walCkptInfo` (-O2): 旧实现对任意内层下标都发 `IndexAddr`,
            // 少了一次取指针值的加载, 返回 `apWiData+96` 而非 `apWiData[0]+96`,
            // `WalCkptInfo` 的写落进 `apWiData` 指针数组旁边的堆内存 ⇒
            // `malloc(): invalid next size` 中止 (-O0/-O1 与 gcc 均正常)。
            // 结构体元素（`&db->aDb[0].pBt`）仍必须走 `IndexAddr`: 按值降级会把元素
            // 拷进帧临时, 写落进副本（sqlite3BtreeOpen 段错误, 见 `AddrOf` 分支注释）。
            if matches!(row_ty, Type::Array(..) | Type::Struct(..) | Type::Union(..)) {
                let base = self.lower_lvalue_base_addr(arr);
                let i = self.lower_expr(idx);
                let ptr_ty = Type::Ptr(Box::new(QualType::new(row_ty.clone())));
                let dest = self.fresh("idxaddr", ptr_ty, span);
                self.emit(Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::IndexAddr {
                        arr: base,
                        idx: i,
                        ty: row_ty.clone(),
                    },
                    span: span.clone(),
                });
                self.define(dest.clone());
                return Operand::Var(dest);
            }
        }
        if let Expr::Member {
            expr,
            field,
            span,
            ty,
            is_arrow,
        } = e
        {
            // 只有字段**本身是对象**（聚合或数组）时，「基址」才等于字段的地址。
            // 标量字段（含指针）必须交出字段里的**值**：`&p->zText[k]`
            // （`sqlite3_str_append` 的 memcpy 目标）的下标基址是 `char *` 字段的内容，
            // 按 `MemberAddr` 降级就变成「结构体地址 + 字段偏移 + k」⇒ 整块字符串被
            // 写进 StrAccum 自己身上，实测 -O2 下 `sqlite3_mprintf` 必段错误。
            // `lower_expr` 的 `Member` 读分支发的正是 `MemberLoad`，与源码形态一致。
            if matches!(ty, Type::Struct(..) | Type::Union(..) | Type::Array(..)) {
                return self.lower_member_addr(expr, field, span, ty, *is_arrow, None);
            }
        }
        if let Expr::Deref { e: inner, ty, .. } = e {
            // `(*pq).o` 这类**解引用基址**：对象的地址就是 `pq` 里的指针值。这里绝不能
            // 委托 `lower_expr(e)` —— 标量解引用发的是 `Load`，聚合类型解引用发的是
            // **整块拷贝**（帧临时），于是「地址」成了副本地址、写落进临时。实测夹具
            // `(*pq).o.in.a = 8` 在 -O2 被编成「从 `gtop` 的头 8 字节取指针再写」⇒ 段错误。
            // 数组类型的解引用不用管：`lower_expr` 的衰减分支本就交回指针值。
            if matches!(ty, Type::Struct(..) | Type::Union(..)) {
                return self.lower_expr(inner);
            }
        }
        self.lower_expr(e)
    }

    /// 该类型是不是「标量整数或指针」—— 即 `Builder::lower_mem_rmw` 能造出正确 Bin 的那个域。
    ///
    /// 浮点/聚合/复数不在内：它们的 compound assign 旧路径已经在产出正确的类型组合，
    /// 而这里发的 `InstKind::Bin` 拿不到那些类型信息（实测把 C++ 的 `clog << ws` 编成
    /// 一条 float 上的 `Shl` ⇒ `E1003 unsupported floating-point operation`）。
    fn is_scalar_int_or_ptr(ty: &Type) -> bool {
        matches!(
            ty,
            Type::Bool
                | Type::Char
                | Type::UChar
                | Type::Short
                | Type::UShort
                | Type::Int
                | Type::UInt
                | Type::Long
                | Type::ULong
                | Type::LongLong
                | Type::ULongLong
                | Type::Int128
                | Type::UInt128
                | Type::Ptr(_)
                | Type::Enum(..)
        )
    }

    /// 该左值是不是**位域**成员访问 (`s.bf` / `p->bf` / `a[i].bf`)。
    ///
    /// `Builder::lower_mem_rmw` 靠 `&lv` 求地址再做 Load—Bin—Store，而位域没有自己的
    /// 存储地址：codegen 的 `&obj.bf` 给出的是**容器**（`StorageUnit`）的地址。于是
    /// RMW 会读写整个容器而不是那几位 —— 进位溢出到相邻位域。实测 -O2/-O3 下
    /// `struct {unsigned b:3; unsigned pad:29;} v; v.b=7; v.pad=5; v.b+=1;` 得到
    /// `b=0 pad=6`（gcc 是 `pad=5`），`arr[i].n++` 同一条错（pr55750 在 -O2 abort）。
    /// 判据与 codegen 侧 `expr_bitfield_width` 一致：字段 `bit_width` 非空即为位域。
    /// 命中后由调用方按既有政策处理（`unsupported` ⇒ 整函数留在 AST 通道，正确但不优化），
    /// 绝不允许「按容器宽度做 RMW」。
    pub(super) fn is_bitfield_lvalue(lv: &Expr) -> bool {
        let Expr::Member { expr, field, .. } = lv else {
            return false;
        };
        let base_ty = expr.ty();
        let struct_ty: &Type = match &base_ty {
            Type::Ptr(p) => &p.base,
            Type::Array(elem, _) => &elem.base,
            other => other,
        };
        let fields = match struct_ty {
            Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, false) => fields,
            _ => return false,
        };
        crate::ast::find_field(fields, &field.name).is_some_and(|(f, _)| f.bit_width.is_some())
    }

    /// 内存左值访问的 volatile 判据 —— 与 `Expr::Deref` / `Expr::Index` / `Expr::Member`
    /// 三条**右值**降级各自的判据一致（集中到这里，避免读侧与写侧用两套口径）。
    fn lvalue_is_volatile(&self, lv: &Expr) -> bool {
        match lv {
            Expr::Deref { e, .. } => matches!(&e.ty(), Type::Ptr(qt) if qt.is_volatile()),
            Expr::Index { arr, .. } => matches!(arr.ty(), Type::Array(qt, _) if qt.is_volatile()),
            Expr::Member { expr, is_arrow, .. } => {
                if *is_arrow {
                    matches!(&expr.ty(), Type::Ptr(qt) if qt.is_volatile())
                } else if let Expr::Ident { name, .. } = expr.as_ref() {
                    self.func
                        .local_quals
                        .get(name.name.as_str())
                        .map(|q| q.is_volatile())
                        .unwrap_or(false)
                } else {
                    false
                }
            },
            _ => false,
        }
    }

    /// 内存左值（`*p` / `a[i]` / `s.f` / `q->g`）的**读—改—写**降级：自增自减与复合
    /// 赋值共用这一条。地址只求一次，旧值与新值各是一个 SSA 临时值，写回同一个地址。
    ///
    /// 为什么必须有它（实测，不是推的）：`Expr::PostInc/PreInc` 那条 arm 过去只认
    /// `Expr::Ident` 操作数，其余形态直接 `return Operand::Int(one)` —— 既不读也不写，
    /// 也不标 `unsupported`，函数照样进 SSA。于是 -O2 下逐条与 gcc 对照：
    ///   `int r = ++*p;` → r 恒为 1、`a[0]` 根本没被写（gcc：r=1、a[0]=1）；
    ///   `int r = a[3]++;` → r=1（应为旧值 0）且 a[3] 不写；`++s.f` / `++q->g` 同构。
    /// 复合赋值那半边（`a[i] += 7` 的返回值是未初始化垃圾）见 `lower_expr_inner` 的
    /// `Expr::Assign` 分支。
    ///
    /// 地址从 `Expr::AddrOf` 那条已验证的降级拿（Ident / `&s.f` / `&a[i]` 分别由
    /// `AddrOf` / `MemberAddr` / `IndexAddr` 表达），不在这里重抄一份形状判据；
    /// `*p` 则直接取指针值（`&*p == p`，走 AddrOf 会换来一次不必要的退 SSA）。
    ///
    /// store 宽度真源：`InstKind::Store` 不带被写对象类型，重建时由
    /// [`store_pointee_ty`] 从地址操作数的类型还原 ⇒ 地址临时值的类型必须是
    /// `Ptr(对象类型)`，给错就是越界写。
    fn lower_mem_rmw(
        &mut self,
        lv: &Expr,
        op: BinOp,
        rhs: Operand,
        res_ty: Option<&Type>,
        span: &Span,
    ) -> Option<(Operand, Operand)> {
        let ty = lv.ty();
        if matches!(ty, Type::Struct(_, _, _, _) | Type::Union(_, _, _, _) | Type::Array(_, _)) {
            // 聚合左值没有标量 RMW 语义（`(*ps)++` 不合法），落到这里说明形态意外，
            // 保守退 SSA而不是猜一个宽度。
            return None;
        }
        if Self::is_bitfield_lvalue(lv) {
            // 位域没有独立地址，`&lv` 只能给出容器 ⇒ RMW 会连相邻位域一起改。
            return None;
        }
        // `new` 的类型由调用方给（复合赋值时是原 `Binop` 自己的 `ty`）：这里造的是一
        // 条 IR 里的 Bin，AST 上那一层的类型信息不会自己流过来，写错了下游的宽度/
        // 符号性就跟着错。
        let new_ty = res_ty.cloned().unwrap_or_else(|| ty.clone());
        let is_volatile = self.lvalue_is_volatile(lv);
        let addr: Operand = match lv {
            Expr::Deref { e, .. } => self.lower_expr(e),
            other => {
                let addr_node = Expr::AddrOf {
                    e: Box::new(other.clone()),
                    span: span.clone(),
                    ty: Type::Ptr(Box::new(QualType::new(ty.clone()))),
                };
                self.lower_expr(&addr_node)
            },
        };
        let old = self.fresh("rmwld", ty.clone(), span);
        self.emit(Inst {
            dest: Some(old.clone()),
            kind: InstKind::Load {
                e: addr.clone(),
                ty: ty.clone(),
                is_volatile,
            },
            span: span.clone(),
        });
        self.define(old.clone());
        let new = self.fresh("rmwop", new_ty, span);
        self.emit(Inst {
            dest: Some(new.clone()),
            kind: InstKind::Bin {
                op,
                l: Operand::Var(old.clone()),
                r: rhs,
            },
            span: span.clone(),
        });
        self.define(new.clone());
        self.emit(Inst {
            dest: None,
            kind: InstKind::Store {
                e: addr,
                src: Operand::Var(new.clone()),
                is_volatile,
            },
            span: span.clone(),
        });
        Some((Operand::Var(old), Operand::Var(new)))
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
                // `&&` / `||` 的右操作数只在左操作数短路时才该求值, 但 SSA 通道不为
                // 短路求值造块: 整棵条件是一棵交给重建的表达式树, 于是右操作数会被
                // **急切求值**成块里的独立指令 —— 与三元 `Cond` 降级出的 `Select`
                // 同一病灶 (判据与理由见 `emitted_is_pure`)。
                // 实测 sqlite `walIndexPage` 的
                // `pWal->nWiData<=iPage || (*ppPage = pWal->apWiData[iPage])==0`:
                // -O2 下即使 `nWiData<=iPage` 已短路, apWiData==NULL 的右操作数照样
                // 求值, `mov (%rax),%rax` 打空段错误 (-O0/-O1 与 gcc 均正常)。
                // 判据同样保守: 右操作数降级出「不纯或可陷获」的指令 (store / load /
                // 调用 / 除法) ⇒ 整函数退出 SSA 往返, 留在有真短路的 AST 通道。
                let short_circuit_rhs = matches!(op, BinOp::AndAnd | BinOp::OrOr);
                let rhs_from = self.cur_inst_count();
                let ro = self.lower_expr(r);
                if short_circuit_rhs && !self.emitted_is_pure(rhs_from) && !self.analysis_only {
                    self.unsupported("&& / || 右操作数带副作用或可陷获（急切求值改变语义）");
                    return Operand::Int(0);
                }
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
                        return self.store_to_named_object(
                            &name.name,
                            var_ty,
                            ro,
                            span,
                            is_volatile,
                        );
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
                // 复合赋值 `LV op= R`：parser 把它展开成 `Assign{l: LV, r: Binop{op, LV 拷贝, R}}`
                // ——`BinOp` 里没有 `Set` 变体，所以「是不是复合赋值」只能靠「r 的某一半边
                // 与 l 结构相同」判（判据与 codegen 共用 `crate::semantics` 那一份实现）。
                // 旧路径把整棵 `r` 降下来再写 `l`，等于把 LV 求值**两遍**：无副作用时两个
                // 地址碰巧相同，有副作用时（`a[i++] += 5`、`*--p += 1`）它们不同 ⇒ 写错位置；
                // 而赋值结果那个临时值在新版本里连定义都没有 —— 实测 -O2 下
                // `int r = a[i] += 7;` 的 `r` 是未初始化垃圾（gcc 给 7）。
                // 正确做法与自增自减共用一条机器：地址只求一次的读—改—写。
                if let Expr::Binop {
                    op: bop,
                    l: bl,
                    r: br,
                    ty: bty,
                    ..
                } = r.as_ref()
                {
                    let rhs_e: Option<&Expr> = if crate::semantics::exprs_structurally_equal(l, bl)
                    {
                        Some(br.as_ref())
                    } else if crate::semantics::exprs_structurally_equal(l, br) {
                        // parser 对可交换运算符会把操作数交换位置（read 在右）
                        Some(bl.as_ref())
                    } else {
                        None
                    };
                    if let Some(rhs_e) = rhs_e {
                        // 只对**标量整数/指针**左值走读—改—写。其余形态（聚合、浮点、
                        // C++ 流对象当左值用等）留在旧路径上：那些形状下旧路径本来就
                        // 产生正确的类型组合，而本函数造的 Bin 拿不出它们的类型
                        //（实测把 `std::clog << ws` 编成“floating-point Shl”直接报错）。
                        if Self::is_scalar_int_or_ptr(&l.ty()) {
                            let rhs_op = self.lower_expr(rhs_e);
                            return match self.lower_mem_rmw(l, *bop, rhs_op, Some(bty), span) {
                                Some((_old, new)) => new,
                                None => {
                                    // 地址给不出来（位域、聚合元素等）：整函数退 SSA。
                                    // 不能退回旧路径 —— 那条路径就是这次的错值本身。
                                    self.unsupported("复合赋值的左值形态给不出地址（读—改—写）");
                                    Operand::Int(0)
                                },
                            };
                        }
                    }
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
                let outer: Option<SsaVar> = target.as_ref().filter(|t| &t.ty != ty).cloned();
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
                if !(then_ok && else_ok) && !self.analysis_only {
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
                // 成员寻址用 dot 还是箭头跟着**基址操作数怎么物化**走（判据与实测见
                // `member_base_is_ptr_operand`：下标基址与数组成员基址交回的是指针值，
                // 按 dot 重建就把读写搬进帧临时；具名数组对象交回的是对象本身）。
                let base_is_ptr = Self::member_base_is_ptr_operand(expr.as_ref(), &obj_ty);
                let obj = if base_is_ptr {
                    self.lower_lvalue_base_addr(expr)
                } else {
                    self.lower_expr(expr)
                };
                let pointee = match &obj_ty {
                    Type::Array(elem, _) => elem.base.clone(),
                    other => other.clone(),
                };
                let (member_obj_ty, member_is_arrow) = if base_is_ptr {
                    (Type::Ptr(Box::new(QualType::new(pointee))), true)
                } else {
                    (obj_ty.clone(), *is_arrow)
                };
                // 数组类型表达式的**值就是它的地址**（C 的衰减规则）。`z->data[i]`、
                // `f->data[1] = 7`、`&z->data`、把 `z->data` 当指针传参 —— 四处要的都是
                // 「对象地址 + 字段偏移」，因此这里必须发 `MemberAddr`，而不是走下面
                // 那条把字段当**值**读的路径：后者对柔性数组/零长数组/VLA 成员拿到的是
                // 字段内容而非基址，实测 -O2 读出栈垃圾而 -O0 对（静默误编译，§7.29）。
                //
                // 只改数组类型成员：标量/结构体成员的既有路径不动，否则就是拿覆盖面
                // 换一个不存在的洞。
                if let Type::Array(elem_qt, _) = ty {
                    let ptr_ty = Type::Ptr(elem_qt.clone());
                    let dest = target.unwrap_or_else(|| self.fresh("subaddr", ptr_ty, span));
                    self.emit(Inst {
                        dest: Some(dest.clone()),
                        kind: InstKind::MemberAddr {
                            obj,
                            obj_ty: member_obj_ty,
                            field: field.name.clone(),
                            ty: ty.clone(),
                            is_arrow: member_is_arrow,
                        },
                        span: span.clone(),
                    });
                    self.define(dest.clone());
                    return Operand::Var(dest);
                }
                let dest = target.unwrap_or_else(|| self.fresh("mem", ty.clone(), span));
                // 箭头访问 `p->fld`：`p` 本身就是结构体基址指针，直接把指针值作为
                // `MemberLoad` 的 `obj` 并保留 `is_arrow`，让 codegen 在指针值上加字段偏移
                // 并按字段宽度加载。以前这里前置一条 `Load { e: p, ty: fld_ty }`，把 `p`
                // 指向的内存按**字段类型**读成了一个值再当基址用 —— 实际读的是
                // `*(fld_ty*)p`（offset 0 的 zDbSName）而不是 `p->fld`，实测
                // `pX = pDb->pBt` 被编成 `movslq (%pDb),%rax`，sqlite3BtreeCursor 收到
                // 垃圾指针而段错误。
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
                self.emit(Inst {
                    dest: Some(dest.clone()),
                    kind: InstKind::MemberLoad {
                        obj,
                        obj_ty: member_obj_ty,
                        field: field.name.clone(),
                        ty: ty.clone(),
                        is_volatile: obj_is_volatile,
                        is_arrow: member_is_arrow,
                    },
                    span: span.clone(),
                });
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
            Expr::AddrOf { e, span, ty } => {
                if let Expr::Ident { name, .. } = e.as_ref() {
                    // `&ident` 的 dest 类型必须带出**函数指针**的指真类型。旧实现恒
                    // `Ptr(void)` (上面的 `ty: _` 把 AddrOf 节点自己的类型丢了), 把
                    // `&func` 的类型抹掉: 重建端 `inst_to_expr_resolved` 的内层 Ident
                    // 因此拿到 `Void` 而非 `Func`, 而 `collect_free_idents` 只对
                    // `Type::Func` 跳过注册 —— 函数名被 `register_var` 收进 `var_map`
                    // 当普通局部, codegen 于是把 `&func` 发成 `lea 局部槽(%r13)`, 即
                    // 一个从未赋值的槽的**地址**, 而不是函数符号地址。实测形状: Lua
                    // liolib 的 `p->closef = &io_noclose` 把栈地址存进函数指针字段,
                    // GC 终结器 `aux_close` 的 `(*cf)(L)` 跳栈段错误。AddrOf 节点自己的
                    // ty 就是 `Ptr(pointee)`, 函数情形原样带出; 其余维持既有行为。
                    let addr_ty = match ty {
                        Type::Ptr(inner) if matches!(inner.base, Type::Func(_, _)) => ty.clone(),
                        _ => Type::Ptr(Box::new(QualType::new(Type::Void))),
                    };
                    let dest = target.unwrap_or_else(|| self.fresh("addr", addr_ty, span));
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
                    // `&expr.fld`：`expr` 必须是左值基址（对象地址），而不是右值。
                    // 若 `expr` 是 `Index{aDb[i]}` 这类计算下标，走 `lower_expr` 会把
                    // 元素结构体按值拷贝到栈临时，随后 `MemberAddr` 对栈副本取字段
                    // 地址，写到栈槽而非堆上目标对象 —— 实测 sqlite3BtreeOpen 的
                    // 第 4 参 `&db->aDb[0].pBt` 被写成栈临时，堆上 pBt 仍是垃圾值，
                    // 后续 `sqlite3VdbeExec` 读 pBt 读到野指针而段错误。
                    //
                    // 归一化（dot/箭头、指针基址 vs 对象基址）与读侧共用 `lower_member_addr`，
                    // 基址一律经 `lower_lvalue_base_addr` 取**地址**（`IndexAddr`/`MemberAddr`），
                    // 判据见 `member_base_is_ptr_operand`。
                    return self.lower_member_addr(expr, field, m_sp, f_ty, *is_arrow, target);
                }
                // `&a[i]` / `(*p)[i]` / `s.f[i]` / `&m[i][j]` / `&m[i][j][k]`：下标的
                // **左值地址**，由 `InstKind::IndexAddr` 表达。基址走 `lower_lvalue_base_addr`：
                //   * 具名数组 / 指针 / 数组成员 / 解引用基址 —— 既有衰减分支本就给出地址，
                //     与 `lower_expr` 同义；
                //   * **计算出来的数组行**（内层下标 `m[i]`，其类型仍是数组）—— 递归再发一条
                //     `IndexAddr`，那一层的元素类型取内层下标的**结果类型**（这里是行数组类型），
                //     于是行步长由 codegen 的 `AddrOf(Index)` 用 `sizeof(row)` 算，SSA 层不碰
                //     任何布局算术（单一真源）。
                //
                // 每层 `IndexAddr` 都重建为 `&<base>[idx]`。作为下一层基址的地址类临时值
                // **不会被内联**（见 `plan_inlined_temps` 的 addr-slot 守卫），所以重建出的
                // AST 始终是 `&<rowvar>[j]`（codegen 走「载入指针值再偏移」分支），而不是历史
                // 上那条把内层行地址内联成 `&(&m[i])[j]` 嵌套取址、从错值恶化成段错误的失败路径。
                if let Expr::Index {
                    arr,
                    idx,
                    span: isp,
                    ty: elem_ty,
                } = e.as_ref()
                {
                    let a = self.lower_lvalue_base_addr(arr);
                    let i = self.lower_expr(idx);
                    let ptr_ty = Type::Ptr(Box::new(QualType::new(elem_ty.clone())));
                    let dest = target.unwrap_or_else(|| self.fresh("idxaddr", ptr_ty, isp));
                    self.emit(Inst {
                        dest: Some(dest.clone()),
                        kind: InstKind::IndexAddr {
                            arr: a,
                            idx: i,
                            ty: elem_ty.clone(),
                        },
                        span: isp.clone(),
                    });
                    self.define(dest.clone());
                    return Operand::Var(dest);
                }
                // 其余形态（`&f()`、`&&label`、复合字面量取址等）仍需更一般的左值地址，
                // IR 表达不了就整函数退 SSA（正确但不优化）。
                self.unsupported("取址的对象不是简单变量/成员/下标");
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
                // 非 Ident 的内存左值（`++*p` / `(*p)++` / `a[i]++` / `++s.f` / `++q->g` /
                // `++p[i]`）：读—改—写，地址只求一次。旧实现在这里 `return
                // Operand::Int(one)` —— 既不读也不写、连 `unsupported` 都不标 ⇒ -O2 静默
                // 误编译（逐条与 gcc 对照的实测数字见 `lower_mem_rmw`）。
                let (old_val, new_val) = match self.lower_mem_rmw(
                    operand.as_ref(),
                    BinOp::Add,
                    Operand::Int(one),
                    None,
                    span,
                ) {
                    Some(pair) => pair,
                    None => {
                        // 地址形态给不出来（位域 `++s.bf`、`++f()` 的返回左值等）：
                        // 与「不可降级表达式」同一处置，整函数退 SSA，绝不允许「只做不写」。
                        self.unsupported("自增/自减的左值形态给不出地址");
                        return Operand::Int(0);
                    },
                };
                let result = if is_pre { new_val } else { old_val };
                // `int v = x++;` 这种预派发 target：结果必须落进去，否则那个变量只有
                // define 没有赋值（与 Ident 分支同一条理由）。
                if let Some(t) = target {
                    self.emit(Inst {
                        dest: Some(t.clone()),
                        kind: InstKind::Bin {
                            op: BinOp::Add,
                            l: result,
                            r: Operand::Int(0),
                        },
                        span: span.clone(),
                    });
                    self.define(t.clone());
                    return Operand::Var(t);
                }
                result
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
                self.unsupported(&format!(
                    "表达式无法在 SSA 里忠实表达（{:?}）",
                    expr_shape(other)
                ));
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
                // computed goto: 表达式是目标地址（`*(pc++)`、`disptab[i]` 之类）。
                // 目标集合是函数里**全部** label BB —— 这里只记源块，接边放在
                // `build_ssa` 末尾（label 可能全部出现在这条语句之后）。
                let _ = self.lower_expr(expr);
                self.pending_dispatch.push(self.cur.clone());
                self.set_term(Terminator::Unreachable);
                let dead = self.func.unique_bb(&format!("{}.dead", self.cur));
                self.cur = dead;
            },
            Stmt::Switch {
                expr, cases, span, ..
            } => {
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

/// 构建一个函数的 SSA IR。
///
/// 为什么要跑两遍: 「本函数哪些对象被取址」是一条**全函数**别名事实, 而 C 允许
/// 在读写之前很远的地方才出现 `&x` (甚至在读之后), 单遇构建无法预知。第一遍只为了
/// 收集事实: 直接读它发射的 `InstKind::AddrOf { base }` —— 与第二遍走的是同一份
/// lowering, 因此不可能漏掉某种 AST 形态 (手写 AST 扫描则必须对 `Expr` 穷举)。
/// 没出现任何取址 (绝大多数函数) 时直接用第一遍结果, 不付出额外构建代价。
pub fn build_from_func(f: &FuncDecl) -> Option<Function> {
    build_from_func_mode(f, false)
}

/// 非结构化函数的构建入口（`ssa::regalloc` 的 analysis-only 通道）。
///
/// 与 [`build_from_func`] 唯一的差别：不因「急切求值」退避。那些退避的
/// 危害在重建后的产物上（`Select` 两分支都执行 / `&&` 右操作数无条件求值），
/// 而这条通道不重建、不产代码 —— 见 `Builder::analysis_only`。
///
/// 两遍构建（探针 + 取址副作用）照旧：`addr_taken` 决定哪些局部走内存，
/// 这条事实在 analysis-only 上同样是活性与家的输入。
pub fn build_from_func_analysis(f: &FuncDecl) -> Option<Function> {
    build_from_func_mode(f, true)
}

fn build_from_func_mode(f: &FuncDecl, analysis_only: bool) -> Option<Function> {
    let probe = build_ssa(f, HashSet::new(), analysis_only)?;
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
    build_ssa(f, addr_taken, analysis_only)
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

fn build_ssa(f: &FuncDecl, addr_taken: HashSet<String>, analysis_only: bool) -> Option<Function> {
    let body = f.body.as_ref()?;
    if !is_structured(body) {
        if std::env::var("DCC_SSA_BAIL").is_ok() {
            let why = crate::ssa::structure::structured_reject(body)
                .unwrap_or_else(|| "未列举形态".to_string());
            eprintln!("[SSA] {} 不进管线：{why}", f.name.name);
        }
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
    let mut builder = Builder::new(func, addr_taken, analysis_only);
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
    // `goto *expr` 的边放在 phi 插入**之后**接：目标集合是全部 label BB，
    // 若在 phi 插入前接上，解释器主循环会得到 85×85 条两两互达的边，`is_back_edge`
    // 会把其中绝大多数判成回边，`insert_loop_phis` 于是对每条回边跑一遍
    // `find_loop_body` + `rename_uses_in`（实测 O(边数×函数体)，不可接受）。
    //
    // 晚接的代价只是 phi 少了「dispatch 前驱」那个入值 —— analysis-only 路径不重建，
    // 也不据此共用寄存器（见 `regalloc::plan_for` 的 `shared_slots`），而活性的
    // 保守性由 `Dispatch` 本身的后继集合保证。
    if !builder.pending_dispatch.is_empty() {
        let mut targets: Vec<String> = builder.label_bbs.values().cloned().collect();
        targets.sort();
        targets.dedup();
        for bb in &builder.pending_dispatch {
            if let Some(b) = func.blocks.get_mut(bb) {
                b.term = Terminator::Dispatch {
                    targets: targets.clone(),
                };
            }
        }
    }
    // 上面的 phi 插入与构建期的循环旋转都改过 terminator, 此处统一刷新 CFG 缓存:
    // `find_loop_body_multi` 等分析沿 `preds` 反向走, 看到旧边就会把循环体算错。
    refresh_cfg(&mut func);
    Some(func)
}
