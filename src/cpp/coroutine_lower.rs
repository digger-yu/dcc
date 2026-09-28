//! M52: C++20 协程 native 降级 —— 协程帧 + 挂起点 CFG→状态机 + promise/awaiter 协议
//!
//! 这是 `src/cpp/coroutine.rs` (legacy 字符串模板路径) 的 native 后端替代实现:
//! 在 [`crate::cpp::native::lower_decls_native`] 入口之前, 把整个 TU 的 AST 重写为
//! **不含任何 `co_await` / `co_yield` / `co_return` 语句**的等价 C++20 代码, 之后
//! 交给通用 native 管线 (Sema → ABI → IR → x86_64). 因此本模块产出的就是普通
//! 函数 + 普通结构体, 挂起/恢复语义完全由状态机承载.
//!
//! ## 三遍重写
//!
//! * **Pass 1 (类型)**: `std::coroutine_handle<P>` → `P*`.
//!   "协程帧 == promise 对象" 是本实现的帧模型 (见下), 所以 handle 就是 promise
//!   指针, 不需要额外的 handle 结构体; `<coroutine>` 桩头里的模板因此永远不会
//!   被实例化.
//! * **Pass 2 (handle 调用)**: 对类型为 `P*` 且 `P` 确实是某个协程的 promise 的
//!   变量, `h.resume()` / `h.done()` / `h.destroy()` / `h.address()` / `h.promise()`
//!   / `coroutine_handle<P>::from_promise(*p)` 改写为对生成的帧入口函数的调用.
//! * **Pass 3 (状态机)**: 每个协程函数 → 帧字段追加 + `dcc_coro_step_<P>` /
//!   `dcc_coro_done_<P>` / `dcc_coro_destroy_<P>` 三个新函数 + 协程函数体替换为
//!   ramp (建帧 → `get_return_object()` → 首次 step). 同一 promise_type 被多个
//!   协程函数共用时, 每台状态机各自成函数 (`dcc_coro_step_<P>_<f>`),
//!   `dcc_coro_step_<P>` 退化为按帧内归属号 `dcc_coro_fn` 分发的跳板.
//!
//! ## 帧模型
//!
//! 真实实现里帧由编译器私有布局 (`__coroutine_frame`) 承载. 这里把帧状态**追加为
//! promise 类的成员** (`dcc_coro_state` / `dcc_coro_done` / 提升的形参与局部量 /
//! 每个挂起点的 awaiter 槽). 因为 promise 对象本身就在帧上, 该模型与标准等价,
//! 且让 `coroutine_handle<P>` 直接退化为 `P*`, 所有帧访问都是单层 `fp->field`.
//!
//! ## 挂起点重排 (Duff's device 的 CFG 等价形式)
//!
//! ```c
//! void dcc_coro_step_P(void* pp) {
//!     P* fp; int again; Aw d3;
//!     fp = (P*)pp; again = 1;
//!     while (again) {
//!         again = 0;
//!         switch (fp->dcc_coro_state) {
//!         case 3: { fp->dcc_coro_state = 4; again = 1; break; }   // 顺序推进
//!         case 7: {                                               // 挂起点
//!             fp->dcc_coro_aw_f_2 = fp->yield_value(...);         //   awaiter 落帧
//!             fp->dcc_coro_state = 8;                             //   恢复位置
//!             if (!fp->dcc_coro_aw_f_2.await_ready()) { fp->dcc_coro_aw_f_2.await_suspend(fp); }
//!             else { again = 1; }
//!         }
//!         case 8: { fp->dcc_coro_aw_f_2.await_resume(); ... break; } // 恢复臂
//!         default: { break; }
//!         }
//!     }
//! }
//! ```
//! 每个臂是一个基本块, 块间跳转经 `dcc_coro_state` 落帧, 因而**跨调用保持挂起位置**;
//! `again` 让 "awaiter 立即就绪" (`await_ready() == true`) 与顺序推进在同一次
//! `resume()` 内继续执行, 精确复刻标准语义 (含 `await_ready` 为真时跳过
//! `await_suspend` 但仍调用 `await_resume`).
//!
//! 三个协议钩子一律在 `fp->dcc_coro_aw_<n>` 这个 glvalue 上调用: [basic.coro.await]
//! 要求 `await_ready` / `await_suspend` / `await_resume` 操作**同一个 awaiter 对象**,
//! 于是 `await_suspend` 里对 awaiter 自身字段的写必须在恢复后的 `await_resume` 里
//! 可见. 曾有过 "先 `d = fp->slot` 拷出局部量、钩子都打在 `d` 上" 的写法, 那会让
//! 写落进一个随臂结束的副本 (实测 24/32 字节 awaiter 的 `await_suspend` 改写被吃掉,
//! 与 g++ 输出不一致), 故帧槽是 awaiter 的**唯一**存放处, 不再拷进拷贝。
//!
//! 反向编译 (`compile_seq` 从语句序列尾部向前) 使回边 (`while` / `for` / `continue`)
//! 无需 patch: 循环条件臂先于循环体分配, 循环体的 `exit` 直接就是条件臂号.
//!
//! ## 已知边界 (后端其它域的缺陷, 本模块显式报 Unsupported, 不静默误编译)
//!
//! 帧槽 (含 awaiter 槽) 的读写是 `fp->slot = aw` / `d = fp->slot` 形式的**裸整块
//! 复制**, 后端按 8 字节分块搬运而不调用用户拷贝赋值/析构. 因此约束不是尺寸
//! (x86_64 后端的 "多 quad 值加载丢第二个字" 缺陷已修, 见
//! `codegen::x86_64::emit::expr::aggregate`), 而是 **可平凡复制**:
//! 无基类 / 无虚函数 / 无用户声明析构与 `operator=` / 数据成员递归满足.
//! 单一真源见 `frame_slot_ty_ok`.

use crate::cpp::mangle::{MangleType, Operator};
use crate::cpp::parser::{
    BinOpKind, ClassDecl, CppBody, CppDecl, CppExpr, CppStmt, SizeofArg, UnaryOpKind,
};
use crate::diagnostics::Span;
use std::collections::{HashMap, HashSet};

/// 类内成员函数签名: `(返回类型, 形参列表, 是否只有声明没有函数体)`.
type MemberFnSig = (MangleType, Vec<(String, MangleType)>, bool);

/// 帧指针 (step 函数里的 promise 指针局部量) 名.
const FP: &str = "dcc_coro_fp";
/// 状态机外层循环的"需要再派发一次"标志局部量名.
const AGAIN: &str = "dcc_coro_again";
/// 帧字段: 挂起位置 (状态号).
const STATE: &str = "dcc_coro_state";
/// 帧字段: 协程是否已 final_suspend (handle::done()).
const DONE: &str = "dcc_coro_done";
/// 帧字段: 同一 promise_type 被多个协程函数共用时, 帧属于哪一个函数
/// (ramp 写入, `dcc_coro_step_<P>` 路由读). 单一协程函数的 promise 不会生成此字段.
const TAG: &str = "dcc_coro_fn";
/// 帧字段前缀: 提升的形参.
const PARAM_PREFIX: &str = "dcc_coro_p_";
/// 帧字段前缀: 提升的局部量.
const LOCAL_PREFIX: &str = "dcc_coro_l_";
/// 帧字段前缀: 挂起点 awaiter 槽.
const SLOT_PREFIX: &str = "dcc_coro_aw_";
/// 生成的帧入口函数前缀.
const STEP_PREFIX: &str = "dcc_coro_step_";
const DONE_PREFIX: &str = "dcc_coro_done_";
const DESTROY_PREFIX: &str = "dcc_coro_destroy_";
/// 零数据成员 awaiter 的占位字段 (保证槽可整体存取).
const AWAITER_PAD: &str = "dcc_coro_pad";

// ---------------------------------------------------------------------------
// 小构造器
// ---------------------------------------------------------------------------

fn ident(n: &str) -> CppExpr {
    CppExpr::Ident(n.to_string())
}
fn int_lit(v: i64) -> CppExpr {
    CppExpr::IntLit(v)
}
fn member(base: CppExpr, m: &str) -> CppExpr {
    CppExpr::Member(Box::new(base), m.to_string())
}
fn ptr_member(base: CppExpr, m: &str) -> CppExpr {
    CppExpr::PtrMember(Box::new(base), m.to_string())
}
fn frame(m: &str) -> CppExpr {
    ptr_member(ident(FP), m)
}
fn call(f: CppExpr, args: Vec<CppExpr>) -> CppExpr {
    CppExpr::Call(Box::new(f), args)
}
fn call0(f: &str) -> CppExpr {
    call(ident(f), Vec::new())
}
fn assign(l: CppExpr, r: CppExpr) -> CppExpr {
    CppExpr::BinOp(BinOpKind::Assign, Box::new(l), Box::new(r))
}
fn stmt(e: CppExpr) -> CppStmt {
    CppStmt::Expr(e)
}
fn set_state(label: i64) -> CppStmt {
    stmt(assign(frame(STATE), int_lit(label)))
}
fn set_again() -> CppStmt {
    stmt(assign(ident(AGAIN), int_lit(1)))
}
fn body_of(stmts: Vec<CppStmt>) -> CppBody {
    CppBody {
        stmts,
        is_decl: false,
    }
}
fn if_stmt(cond: CppExpr, then: Vec<CppStmt>, els: Option<Vec<CppStmt>>) -> CppStmt {
    CppStmt::If {
        cond,
        then: Box::new(body_of(then)),
        els: els.map(|v| Box::new(body_of(v))),
    }
}
fn decl_local(ty: MangleType, name: &str) -> CppStmt {
    CppStmt::LocalVar {
        ty,
        name: name.to_string(),
        init: None,
        is_volatile: false,
        is_const: false,
    }
}
fn member_var(ty: MangleType, name: &str) -> CppDecl {
    CppDecl::MemberVar {
        ty,
        name: name.to_string(),
        is_mutable: false,
        is_volatile: false,
        is_const: false,
        is_static: false,
        init: None,
    }
}
fn free_fn_decl(ret: MangleType, name: &str, params: Vec<(String, MangleType)>) -> CppDecl {
    CppDecl::FreeFn {
        ret,
        name: name.to_string(),
        params,
        body: CppBody {
            stmts: Vec::new(),
            is_decl: true,
        },
        is_noexcept: false,
        trailing_ret_type: None,
    }
}
fn span() -> Span {
    Span::point("<dcc-coroutine-lowering>", 0, 0)
}
fn void_ptr(e: CppExpr) -> CppExpr {
    CppExpr::Cast(MangleType::Pointer(Box::new(MangleType::Void)), Box::new(e))
}

/// 标量 (可用一条 mov 整体存取) 类型判定 —— 决定能否被提升进协程帧 / 作 awaiter 数据成员.
fn is_scalar_ty(t: &MangleType) -> bool {
    matches!(
        t,
        MangleType::Bool
            | MangleType::Char
            | MangleType::Schar
            | MangleType::Uchar
            | MangleType::Short
            | MangleType::Ushort
            | MangleType::Int
            | MangleType::Uint
            | MangleType::Long
            | MangleType::Ulong
            | MangleType::LongLong
            | MangleType::UlongLong
            | MangleType::Float
            | MangleType::Double
            | MangleType::Pointer(_)
    )
}

// ---------------------------------------------------------------------------
// AST 可变遍历器 (Pass 1 / Pass 2 共用)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `std::coroutine_handle<P>` 类型 → `P*`
    HandleTypes,
    /// handle 上的协议方法调用 → 生成的帧入口函数调用
    HandleCalls,
    /// 协程体内的形参 / 局部量引用 → 帧字段 `fp->dcc_coro_*`
    Promote,
}

/// 递归遍历器: 后序 (子先于父) 应用当前 Mode 的重写.
struct Rewriter<'a> {
    mode: Mode,
    /// promise 类名集合 (确有协程函数使用它的 promise)
    promises: &'a HashSet<String>,
    /// 当前函数的形参 + 局部量类型表 (HandleCalls 模式用于识别 handle 变量)
    env: HashMap<String, MangleType>,
    /// 类索引 (HandleCalls 模式据此避免把 promise 自己的同名方法当 handle 方法)
    classes: &'a HashMap<String, ClassDecl>,
    /// Promote 模式: 变量名 → 帧字段名
    promote: HashMap<String, String>,
    /// 遍历中遇到的硬错误 (Unsupported)
    err: Option<String>,
}

impl<'a> Rewriter<'a> {
    fn new(
        mode: Mode,
        promises: &'a HashSet<String>,
        classes: &'a HashMap<String, ClassDecl>,
    ) -> Self {
        Rewriter {
            mode,
            promises,
            env: HashMap::new(),
            classes,
            promote: HashMap::new(),
            err: None,
        }
    }

    fn ty(&mut self, t: &mut MangleType) {
        match t {
            MangleType::Pointer(inner) | MangleType::Reference(inner) => self.ty(inner),
            MangleType::Array(inner, _) => self.ty(inner),
            MangleType::Template { args, .. }
            | MangleType::TplMember { args, .. }
            | MangleType::VariadicPack(args) => {
                for a in args.iter_mut() {
                    self.ty(a);
                }
            },
            _ => {},
        }
        if matches!(self.mode, Mode::HandleTypes) {
            let hit = match t {
                MangleType::Template { name, args }
                    if name == "coroutine_handle" && !args.is_empty() =>
                {
                    Some(args[0].clone())
                },
                _ => None,
            };
            if let Some(inner) = hit {
                *t = MangleType::Pointer(Box::new(inner));
            }
        }
    }

    fn ex(&mut self, e: &mut CppExpr) {
        match e {
            CppExpr::Member(a, _)
            | CppExpr::PtrMember(a, _)
            | CppExpr::MemberStar(a, _)
            | CppExpr::PtrMemberStar(a, _)
            | CppExpr::Ref(a)
            | CppExpr::AddrOf(a)
            | CppExpr::UnaryOp(_, a)
            | CppExpr::Delete(a)
            | CppExpr::DeleteArr(a)
            | CppExpr::Noexcept(a)
            | CppExpr::Typeid(a)
            | CppExpr::NewArr { count: a, .. } => self.ex(a),
            CppExpr::BinOp(_, a, b) => {
                self.ex(a);
                self.ex(b);
            },
            CppExpr::InitList(v) | CppExpr::Comma(v) | CppExpr::OpCall { args: v, .. } => {
                for x in v.iter_mut() {
                    self.ex(x);
                }
            },
            CppExpr::New(_, args) | CppExpr::MultiIndex(_, args) => {
                for x in args.iter_mut() {
                    self.ex(x);
                }
            },
            CppExpr::PlacementNew { ptr, args, .. } => {
                self.ex(ptr);
                for x in args.iter_mut() {
                    self.ex(x);
                }
            },
            CppExpr::TypedInitList { ty, elems } => {
                self.ty(ty);
                for x in elems.iter_mut() {
                    self.ex(x);
                }
            },
            CppExpr::Call(f, args) => {
                self.ex(f);
                for x in args.iter_mut() {
                    self.ex(x);
                }
            },
            CppExpr::Cast(t, inner) => {
                self.ty(t);
                self.ex(inner);
            },
            CppExpr::StaticCast { ty, expr }
            | CppExpr::ConstCast { ty, expr }
            | CppExpr::ReinterpretCast { ty, expr }
            | CppExpr::DynamicCast { ty, expr } => {
                self.ty(ty);
                self.ex(expr);
            },
            CppExpr::TemplateIdent { args, .. } => {
                for x in args.iter_mut() {
                    self.ty(x);
                }
            },
            CppExpr::TplStaticAccess { args, .. } => {
                for x in args.iter_mut() {
                    self.ty(x);
                }
            },
            CppExpr::Sizeof(arg) => match arg {
                SizeofArg::Type(t) => self.ty(t),
                SizeofArg::Expr(x) => self.ex(x),
                // pack 长度由 template 层单态化替掉, 此处无需改写
                SizeofArg::Pack(_) => {},
            },
            CppExpr::TypeidType(t) => self.ty(t),
            CppExpr::Ternary { cond, then, els } => {
                self.ex(cond);
                self.ex(then);
                self.ex(els);
            },
            // 无子节点: IntLit / FloatLit / StrLit / Ident / This / ThisMember /
            // QualifiedAccess / MemberPtr / StrConcat; Lambda 体内不含协程协议调用.
            _ => {},
        }
        match self.mode {
            Mode::HandleTypes => {},
            Mode::HandleCalls => self.rewrite_handle_call(e),
            Mode::Promote => self.promote_node(e),
        }
    }

    /// 后序: 协程体内的提升变量引用改写为帧字段访问.
    fn promote_node(&mut self, e: &mut CppExpr) {
        if self.err.is_some() {
            return;
        }
        if matches!(e, CppExpr::Lambda(..)) {
            self.err = Some("a lambda inside a coroutine body is not lowered".to_string());
            return;
        }
        if let CppExpr::Ident(n) = e {
            if let Some(field) = self.promote.get(n).cloned() {
                *e = frame(&field);
                return;
            }
        }
        // `&fp->field` 的取址在后端会丢偏移, 必须显式拒绝
        let bad = match e {
            CppExpr::AddrOf(inner) | CppExpr::Ref(inner) => matches!(
                &**inner,
                CppExpr::PtrMember(b, f)
                    if matches!(&**b, CppExpr::Ident(n) if n.as_str() == FP)
                        && f.starts_with("dcc_coro_")
            ),
            _ => false,
        };
        if bad {
            self.err = Some(
                "taking the address of a coroutine frame field is not supported (non-zero \
                 member `&` loses the offset in the native backend); keep the value in a \
                 promoted scalar instead"
                    .to_string(),
            );
        }
    }

    /// 后序: `h.done()` / `h.resume()` / `H::from_promise(*p)` 等形态改写.
    fn rewrite_handle_call(&mut self, e: &mut CppExpr) {
        if self.err.is_some() {
            return;
        }
        let (callee, args) = match e {
            CppExpr::Call(f, args) => (f, args),
            _ => return,
        };
        // `std::coroutine_handle<P>::from_promise(*p)` / `::from_address(a)`
        if let CppExpr::TplStaticAccess { name, member, .. } = &**callee {
            if name == "coroutine_handle" {
                match member.as_str() {
                    "from_promise" => {
                        if args.len() == 1 {
                            let a0 = args[0].clone();
                            *e = match a0 {
                                CppExpr::UnaryOp(UnaryOpKind::Deref, inner) => *inner,
                                other => void_ptr(other),
                            };
                            return;
                        }
                        self.err = Some(
                            "coroutine_handle::from_promise expects exactly one argument"
                                .to_string(),
                        );
                        return;
                    },
                    "from_address" => {
                        if args.len() == 1 {
                            let pty = match &**callee {
                                CppExpr::TplStaticAccess { args, .. } => args.first().cloned(),
                                _ => None,
                            };
                            // `coroutine_handle<void>` 是类型擦除句柄: 帧上没留下
                            // promise 类型信息就无法派发 step/done/destroy.
                            if matches!(&pty, Some(MangleType::Void)) {
                                self.err = Some(
                                    "type-erased coroutine_handle<void> cannot be lowered (the \
                                     resume/destroy dispatch needs the promise type); use \
                                     coroutine_handle<P>"
                                        .to_string(),
                                );
                                return;
                            }
                            let a0 = args[0].clone();
                            let inner = match pty {
                                Some(t) => CppExpr::Cast(t, Box::new(a0)),
                                None => a0,
                            };
                            *e = inner;
                            return;
                        }
                        self.err = Some(
                            "coroutine_handle::from_address expects exactly one argument"
                                .to_string(),
                        );
                        return;
                    },
                    _ => {},
                }
            }
        }
        // `h.<协议方法>()`: h 必须是类型为 `P*` (P 为确有协程的 promise) 的变量
        let (var, meth) = match &**callee {
            CppExpr::Member(base, m) => match &**base {
                CppExpr::Ident(v) => (v.clone(), m.clone()),
                _ => return,
            },
            CppExpr::PtrMember(base, m) => match &**base {
                CppExpr::Ident(v) => (v.clone(), m.clone()),
                _ => return,
            },
            _ => return,
        };
        let promise = match self.env.get(&var) {
            Some(MangleType::Pointer(inner)) => match &**inner {
                MangleType::Class(c) => {
                    let bare = canonical_class_name(self.classes, c);
                    if self.promises.contains(&bare) {
                        bare
                    } else {
                        return;
                    }
                },
                // Pass 1 已把 `coroutine_handle<P>` 变成 `P*`; 剩下 `void*` 的只能是
                // 类型擦除句柄 —— 明确报错, 不能留一个 `((void*)0).resume()` 给 sema.
                MangleType::Void
                    if ["resume", "done", "destroy", "promise", "address"]
                        .contains(&meth.as_str()) =>
                {
                    self.err = Some(format!(
                        "coroutine_handle<void>::{}() cannot be lowered: the handle is type-erased, \
                         so the coroutine frame entry (dcc_coro_..._<Promise>) is unknown; keep \
                         the handle as coroutine_handle<P>",
                        meth
                    ));
                    return;
                },
                _ => return,
            },
            _ => return,
        };
        // promise 类自己声明了同名方法 => 是真实成员调用, 不是 handle 协议
        if let Some(cd) = self.classes.get(&promise) {
            if cd.members.iter().any(|d| {
                matches!(
                    d,
                    CppDecl::MemberFn { name, .. } if *name == meth
                ) || matches!(
                    d,
                    CppDecl::OperatorFn { .. } | CppDecl::Ctor { .. } | CppDecl::Dtor { .. }
                )
            }) {
                return;
            }
        }
        let h = ident(&var);
        let repl = match meth.as_str() {
            "resume" => call(ident(&format!("{}{}", STEP_PREFIX, promise)), vec![void_ptr(h)]),
            "done" => call(ident(&format!("{}{}", DONE_PREFIX, promise)), vec![void_ptr(h)]),
            "destroy" => call(ident(&format!("{}{}", DESTROY_PREFIX, promise)), vec![void_ptr(h)]),
            "address" => void_ptr(h),
            "promise" => CppExpr::UnaryOp(UnaryOpKind::Deref, Box::new(h)),
            other => {
                self.err = Some(format!(
                    "unsupported coroutine_handle member '{}' (supported: resume / done / \
                     destroy / address / promise / from_promise / from_address)",
                    other
                ));
                return;
            },
        };
        *e = repl;
    }

    fn st(&mut self, s: &mut CppStmt) {
        match s {
            CppStmt::Expr(e) | CppStmt::CoAwait(e) | CppStmt::CoYield(e) => self.ex(e),
            CppStmt::Return(Some(e)) | CppStmt::CoReturn(Some(e)) | CppStmt::Throw(Some(e)) => {
                self.ex(e)
            },
            CppStmt::If { cond, then, els } | CppStmt::IfConstexpr { cond, then, els } => {
                self.ex(cond);
                self.body(then);
                if let Some(e) = els {
                    self.body(e);
                }
            },
            CppStmt::While { cond, body } => {
                self.ex(cond);
                self.body(body);
            },
            CppStmt::DoWhile { body, cond } => {
                self.body(body);
                self.ex(cond);
            },
            CppStmt::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(i) = init {
                    self.st(i);
                }
                if let Some(c) = cond {
                    self.ex(c);
                }
                if let Some(x) = step {
                    self.st(x);
                }
                self.body(body);
            },
            CppStmt::Block(b) => self.body(b),
            CppStmt::ThisMember(_, inner) => self.st(inner),
            CppStmt::LocalVar { ty, init, .. } | CppStmt::StaticLocal { ty, init, .. } => {
                self.ty(ty);
                if let Some(i) = init {
                    self.ex(i);
                }
            },
            // 多 init-declarator: 共享 decl-specifier 扫一次, 每个 declarator
            // 的初始化式各自扫一遍 (只扫第一个会漏掉后续变量的 await 点)。
            CppStmt::LocalVarList { spec, declarators } => {
                self.ty(&mut spec.ty);
                for d in declarators.iter_mut() {
                    if let Some(i) = &mut d.init {
                        self.ex(i);
                    }
                }
            },
            CppStmt::StructuredBinding { ty, init, .. } => {
                self.ty(ty);
                self.ex(init);
            },
            CppStmt::Try { body, catches } => {
                self.body(body);
                for c in catches.iter_mut() {
                    if let Some(t) = &mut c.ty {
                        self.ty(t);
                    }
                    self.body(&mut c.body);
                }
            },
            CppStmt::Switch { expr, arms, .. } => {
                self.ex(expr);
                for (_, b) in arms.iter_mut() {
                    self.body(b);
                }
            },
            CppStmt::Labeled { stmt: inner, .. } => self.st(inner),
            CppStmt::Decl(d) => self.decl(d),
            _ => {},
        }
    }

    fn body(&mut self, b: &mut CppBody) {
        for s in b.stmts.iter_mut() {
            self.st(s);
        }
    }

    fn decl(&mut self, d: &mut CppDecl) {
        match d {
            CppDecl::FreeFn {
                ret,
                params,
                body,
                trailing_ret_type,
                ..
            } => {
                self.ty(ret);
                for (_, t) in params.iter_mut() {
                    self.ty(t);
                }
                if let Some(t) = trailing_ret_type {
                    self.ty(t);
                }
                self.fn_body(params, body);
            },
            CppDecl::MemberFn {
                ret, params, body, ..
            } => {
                self.ty(ret);
                for (_, t) in params.iter_mut() {
                    self.ty(t);
                }
                self.fn_body(params, body);
            },
            CppDecl::Ctor { params, body, .. } | CppDecl::OperatorFn { params, body, .. } => {
                for (_, t) in params.iter_mut() {
                    self.ty(t);
                }
                self.fn_body(params, body);
            },
            CppDecl::Dtor { body, .. } => self.fn_body(&[], body),
            CppDecl::MemberVar { ty, .. } | CppDecl::Typedef { target: ty, .. } => self.ty(ty),
            CppDecl::GlobalVar { ty, init, .. } => {
                self.ty(ty);
                if let Some(i) = init {
                    self.ex(i);
                }
            },
            CppDecl::Class(cd) => {
                for m in cd.members.iter_mut() {
                    self.decl(m);
                }
            },
            CppDecl::Namespace { body, .. } | CppDecl::InlineNamespace { body, .. } => {
                for x in body.iter_mut() {
                    self.decl(x);
                }
            },
            CppDecl::ExportBlock(v) => {
                for x in v.iter_mut() {
                    self.decl(x);
                }
            },
            CppDecl::AnonAggr { members, .. } => {
                for m in members.iter_mut() {
                    self.decl(m);
                }
            },
            CppDecl::StaticAssert { condition, .. } => self.ex(condition),
            // 模板 / 特化体内是未实例化的泛型代码, 不参与协程协议重写
            _ => {},
        }
    }

    /// 函数体前先把形参 / 局部量类型登记进 env (HandleCalls 模式的 handle 识别依据)
    fn fn_body(&mut self, params: &[(String, MangleType)], body: &mut CppBody) {
        if !matches!(self.mode, Mode::HandleCalls) {
            self.body(body);
            return;
        }
        let saved = std::mem::take(&mut self.env);
        self.env = HashMap::new();
        for (n, t) in params {
            self.env.insert(n.clone(), t.clone());
        }
        collect_ty_env(body, &mut self.env);
        self.body(body);
        self.env = saved;
    }
}

/// 收集函数体内所有 `LocalVar` 声明的类型 (含嵌套块 / 循环 init; 后声明覆盖同名).
fn collect_ty_env(body: &CppBody, env: &mut HashMap<String, MangleType>) {
    for s in &body.stmts {
        collect_stmt_env(s, env);
    }
}

fn collect_stmt_env(s: &CppStmt, env: &mut HashMap<String, MangleType>) {
    match s {
        CppStmt::LocalVar { ty, name, .. } | CppStmt::StaticLocal { ty, name, .. } => {
            env.insert(name.clone(), ty.clone());
        },
        // 多 declarator 的每一个名字都要登记, 且类型是共享 decl-specifier 叠加
        // 它自己的指针/数组层级 —— 只认第一个会让 `int a = 0, b = 1;` 里的 `b`
        // 在成员访问 / 类型推导时查不到类型。
        CppStmt::LocalVarList { spec, declarators } => {
            for d in declarators {
                env.insert(d.name.clone(), d.full_ty(spec));
            }
        },
        CppStmt::StructuredBinding { names, .. } => {
            for n in names {
                env.insert(n.clone(), MangleType::Auto);
            }
        },
        CppStmt::If { then, els, .. } | CppStmt::IfConstexpr { then, els, .. } => {
            collect_ty_env(then, env);
            if let Some(e) = els {
                collect_ty_env(e, env);
            }
        },
        CppStmt::While { body, .. } | CppStmt::DoWhile { body, .. } => {
            collect_ty_env(body, env);
        },
        CppStmt::Block(body) => {
            collect_ty_env(body, env);
        },
        CppStmt::For {
            init, body, step, ..
        } => {
            if let Some(i) = init {
                collect_stmt_env(i, env);
            }
            if let Some(x) = step {
                collect_stmt_env(x, env);
            }
            collect_ty_env(body, env);
        },
        CppStmt::Switch { arms, .. } => {
            for (_, b) in arms {
                collect_ty_env(b, env);
            }
        },
        CppStmt::Try { body, catches } => {
            collect_ty_env(body, env);
            for c in catches {
                collect_ty_env(&c.body, env);
            }
        },
        CppStmt::Labeled { stmt, .. } | CppStmt::ThisMember(_, stmt) => collect_stmt_env(stmt, env),
        CppStmt::Decl(CppDecl::Typedef { target, name }) => {
            env.insert(name.clone(), target.clone());
        },
        _ => {},
    }
}

// ---------------------------------------------------------------------------
// 声明索引 / promise 解析
// ---------------------------------------------------------------------------

/// 递归收集 TU 里所有类定义, 同时以**裸名**与**限定名** (`std::suspend_always`)
/// 两个键登记 —— dcc 的解析器把限定类型名原样存进 `Class("std::X")`, 故 promise
/// / awaiter 的类名查找必须两种拼法都能命中.
fn index_classes(decls: &[CppDecl], out: &mut HashMap<String, ClassDecl>) {
    index_classes_scoped(decls, "", out)
}

fn index_classes_scoped(decls: &[CppDecl], ns: &str, out: &mut HashMap<String, ClassDecl>) {
    for d in decls {
        match d {
            CppDecl::Class(cd) => {
                let full = if ns.is_empty() {
                    cd.name.clone()
                } else {
                    format!("{}::{}", ns, cd.name)
                };
                // 同名多次插入时后到者赢 (前向 `struct P;` 是 Typedef, 不占此表)
                out.insert(cd.name.clone(), cd.clone());
                if full != cd.name {
                    out.insert(full.clone(), cd.clone());
                }
                index_classes_scoped(&cd.members, &full, out);
            },
            CppDecl::Namespace { body, name } | CppDecl::InlineNamespace { body, name } => {
                let sub = match name {
                    Some(n) if ns.is_empty() => n.clone(),
                    Some(n) => format!("{}::{}", ns, n),
                    None => ns.to_string(),
                };
                index_classes_scoped(body, &sub, out)
            },
            CppDecl::ExportBlock(v) => index_classes_scoped(v, ns, out),
            _ => {},
        }
    }
}

/// 顶层 typedef 别名表 (返回类型可能是 `using` 出来的别名).
fn index_typedefs(decls: &[CppDecl], out: &mut HashMap<String, MangleType>) {
    for d in decls {
        match d {
            CppDecl::Typedef { target, name } => {
                out.insert(name.clone(), target.clone());
            },
            CppDecl::Namespace { body, .. } | CppDecl::InlineNamespace { body, .. } => {
                index_typedefs(body, out)
            },
            CppDecl::ExportBlock(v) => index_typedefs(v, out),
            _ => {},
        }
    }
}

/// 类内的 `typedef X promise_type;` → Some(X)
fn class_promise_target(cd: &ClassDecl) -> Option<String> {
    for m in &cd.members {
        if let CppDecl::Typedef { target, name } = m {
            if name == "promise_type" {
                if let MangleType::Class(c) = target {
                    return Some(c.clone());
                }
            }
        }
    }
    None
}

/// 把协程函数的返回类型解析为 promise 类名.
fn promise_of_ret(
    ret: &MangleType,
    classes: &HashMap<String, ClassDecl>,
    typedefs: &HashMap<String, MangleType>,
    depth: u32,
) -> Option<String> {
    if depth > 4 {
        return None;
    }
    match ret {
        MangleType::Class(c) => {
            // 顶层 typedef 别名 (`using Gen = Generator;`) 先展开一层
            if let Some(t) = typedefs.get(c) {
                if !matches!(t, MangleType::Class(n) if n == c) {
                    return promise_of_ret(t, classes, typedefs, depth + 1);
                }
            }
            let cd = classes.get(c)?;
            class_promise_target(cd)
        },
        MangleType::Template { name, .. } => {
            let cd = classes.get(name)?;
            class_promise_target(cd)
        },
        _ => None,
    }
}

/// 把 (可能是限定名 `std::X` 的) 类名规范为 decl 树里 `ClassDecl::name` 的裸名,
/// 供后续按名字回查类定义追加帧字段使用.
fn canonical_class_name(classes: &HashMap<String, ClassDecl>, name: &str) -> String {
    match classes.get(name) {
        Some(cd) => cd.name.clone(),
        None => name.to_string(),
    }
}

/// 取 promise 类里某个方法的返回类型 (要求类内定义).
fn promise_member_ret(cd: &ClassDecl, meth: &str) -> Option<MangleType> {
    for m in &cd.members {
        if let CppDecl::MemberFn {
            name, ret, body, ..
        } = m
        {
            if name == meth && !body.is_decl {
                return Some(ret.clone());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 协程语句检测
// ---------------------------------------------------------------------------

/// 语句序列 (递归) 是否含协程挂起/返回语句.
fn stmts_have_coro(body: &CppBody) -> bool {
    body.stmts.iter().any(stmt_has_coro)
}

fn stmt_has_coro(s: &CppStmt) -> bool {
    match s {
        CppStmt::CoAwait(_) | CppStmt::CoYield(_) | CppStmt::CoReturn(_) => true,
        CppStmt::If { then, els, .. } | CppStmt::IfConstexpr { then, els, .. } => {
            stmts_have_coro(then) || els.as_ref().map(|e| stmts_have_coro(e)).unwrap_or(false)
        },
        CppStmt::While { body, .. } | CppStmt::DoWhile { body, .. } => stmts_have_coro(body),
        CppStmt::Block(body) => stmts_have_coro(body),
        CppStmt::Try { body, .. } => stmts_have_coro(body),
        CppStmt::For {
            init, body, step, ..
        } => {
            stmts_have_coro(body)
                || init.as_ref().map(|i| stmt_has_coro(i)).unwrap_or(false)
                || step.as_ref().map(|i| stmt_has_coro(i)).unwrap_or(false)
        },
        CppStmt::Switch { arms, .. } => arms.iter().any(|(_, b)| stmts_have_coro(b)),
        CppStmt::Labeled { stmt, .. } | CppStmt::ThisMember(_, stmt) => stmt_has_coro(stmt),
        CppStmt::Decl(CppDecl::Class(cd)) => cd.members.iter().any(stmt_decl_has_coro),
        _ => false,
    }
}

fn stmt_decl_has_coro(d: &CppDecl) -> bool {
    match d {
        CppDecl::FreeFn { body, .. } | CppDecl::MemberFn { body, .. } => stmts_have_coro(body),
        CppDecl::Ctor { body, .. } | CppDecl::Dtor { body, .. } => stmts_have_coro(body),
        _ => false,
    }
}

/// TU 是否含协程 (决定 native 管线是否启用本模块).
pub fn tu_has_coroutine(decls: &[CppDecl]) -> bool {
    decls.iter().any(decl_has_coro)
}

fn decl_has_coro(d: &CppDecl) -> bool {
    match d {
        CppDecl::FreeFn { body, .. } | CppDecl::MemberFn { body, .. } => {
            !body.is_decl && stmts_have_coro(body)
        },
        CppDecl::Ctor { body, .. } | CppDecl::Dtor { body, .. } => stmts_have_coro(body),
        CppDecl::OperatorFn { body, .. } => stmts_have_coro(body),
        CppDecl::Class(cd) => cd.members.iter().any(decl_has_coro),
        CppDecl::Namespace { body, .. } | CppDecl::InlineNamespace { body, .. } => {
            body.iter().any(decl_has_coro)
        },
        CppDecl::ExportBlock(v) => v.iter().any(decl_has_coro),
        CppDecl::Template { decl, .. }
        | CppDecl::PartialSpec { decl, .. }
        | CppDecl::ExternTemplate(decl) => decl_has_coro(decl),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Pass 3: 挂起点 CFG → 状态机
// ---------------------------------------------------------------------------

/// 一个臂 (状态机基本块) 的出口动作.
#[derive(Clone, Debug, Default)]
enum Term {
    /// 未填 (内部不一致)
    #[default]
    Unset,
    /// 协程结束: 跳出 switch (again 已为 0, while 随之退出)
    Halt,
    /// 顺序推进到下一个臂
    Goto(usize),
    /// 条件分派
    Branch {
        cond: CppExpr,
        then: usize,
        els: usize,
    },
    /// 挂起点: awaiter 已在帧槽里, 求 await_ready; 为假则 await_suspend 并返回,
    /// 为真则置 again 继续派发到 `resume` 臂 (执行 await_resume).
    Suspend {
        slot: usize,
        resume: usize,
        /// true = final_suspend: 挂起前置 done 标志 (handle::done() 变真)
        done: bool,
    },
}

#[derive(Clone, Default)]
struct Arm {
    stmts: Vec<CppStmt>,
    term: Term,
}

/// 挂起点 awaiter 在帧里的落点.
#[derive(Clone, Debug)]
struct Slot {
    /// 帧槽字段名 `dcc_coro_aw_<fn>_<n>` —— awaiter 对象的唯一存放处
    field: String,
    /// awaiter 类名 (诊断与占位字段注入用)
    ty: String,
    /// await_suspend 收 `void*` 而非 `Promise*`
    void_arg: bool,
}

/// 语句种类短名 (诊断用).
fn stmt_kind(s: &CppStmt) -> &'static str {
    match s {
        CppStmt::Decl(_) => "nested declaration",
        CppStmt::Expr(_) => "expression statement",
        CppStmt::Return(_) => "return",
        CppStmt::If { .. } => "if",
        CppStmt::IfConstexpr { .. } => "if constexpr",
        CppStmt::While { .. } => "while",
        CppStmt::For { .. } => "for",
        CppStmt::Block(_) => "block",
        CppStmt::ThisMember(..) => "this-> member",
        CppStmt::LocalVar { .. } => "local variable",
        CppStmt::StaticLocal { .. } => "static local",
        CppStmt::LocalVarList { .. } => "local variable list",
        CppStmt::Try { .. } => "try",
        CppStmt::Throw(_) => "throw",
        CppStmt::DoWhile { .. } => "do-while",
        CppStmt::Goto(_) => "goto",
        CppStmt::Labeled { .. } => "labeled statement",
        CppStmt::Break => "break",
        CppStmt::Continue => "continue",
        CppStmt::Fallthrough { .. } => "[[fallthrough]]",
        CppStmt::Switch { .. } => "switch",
        CppStmt::CoAwait(_) => "co_await",
        CppStmt::CoYield(_) => "co_yield",
        CppStmt::CoReturn(_) => "co_return",
        CppStmt::StructuredBinding { .. } => "structured binding",
        CppStmt::OpaqueStmt(_) => "unparsed statement",
    }
}

/// 单个协程函数的状态机编译器.
struct Machine {
    promise: String,
    fname: String,
    promise_cls: ClassDecl,
    classes: HashMap<String, ClassDecl>,
    fn_rets: HashMap<String, MangleType>,
    /// 顶层 typedef 别名表: 把 `typedef std::suspend_always Susp;` 之类的别名解到真类名
    typedefs: HashMap<String, MangleType>,
    arms: Vec<Arm>,
    slots: Vec<Slot>,
    /// 待追加到 promise 类的帧字段 (按登记顺序)
    fields: Vec<(MangleType, String)>,
    have_fields: HashSet<String>,
    /// 需要注入占位字段的 0 数据成员 awaiter 类
    pad_classes: HashSet<String>,
    /// initial_suspend 臂 / 其恢复臂 / final_suspend 臂 / 终态臂
    init_arm: usize,
    init_resume: usize,
    final_arm: usize,
    final_resume: usize,
    /// 臂号偏移 (见 [`Machine::state_val`]): 保证所有臂号十进制位数相同.
    state_base: usize,
    /// 本机器步进函数名: 独占 promise 时是 `dcc_coro_step_<P>`,
    /// 与其他协程函数共用 promise 时是 `dcc_coro_step_<P>_<f>` (由路由函数分发).
    step_name: String,
    /// 共用 promise 时的归属编号 (None = 本 promise 只有一个协程函数).
    tag: Option<i64>,
}

impl Machine {
    fn new(
        promise: String,
        fname: String,
        promise_cls: ClassDecl,
        classes: HashMap<String, ClassDecl>,
        fn_rets: HashMap<String, MangleType>,
        typedefs: HashMap<String, MangleType>,
    ) -> Self {
        Machine {
            init_arm: 0,
            init_resume: 1,
            final_arm: 2,
            final_resume: 3,
            step_name: format!("{}{}", STEP_PREFIX, promise),
            promise,
            fname,
            promise_cls,
            classes,
            fn_rets,
            typedefs,
            arms: Vec::new(),
            slots: Vec::new(),
            fields: Vec::new(),
            have_fields: HashSet::new(),
            pad_classes: HashSet::new(),
            state_base: 0,
            tag: None,
        }
    }

    /// 标记本机器与同 promise 的其它协程函数共存: 帧里多一个归属字段,
    /// 步进函数改名为 per-function, 由 `dcc_coro_step_<P>` 路由按归属分发.
    fn set_shared(&mut self, tag: i64) {
        self.tag = Some(tag);
        self.step_name = format!("{}{}_{}", STEP_PREFIX, self.promise, self.fname);
    }

    fn alloc(&mut self) -> usize {
        self.arms.push(Arm::default());
        self.arms.len() - 1
    }

    /// 登记一个帧字段 (同名只登记一次).
    fn field(&mut self, ty: MangleType, name: &str) {
        if self.have_fields.insert(name.to_string()) {
            self.fields.push((ty, name.to_string()));
        }
    }

    fn alloc_slot(&mut self, aw_ty: &str) -> usize {
        let n = self.slots.len();
        // 帧槽名带函数名: 共用 promise 的不同协程函数, 其 slot 0 类型可以完全不同
        let field = format!("{}{}_{}", SLOT_PREFIX, self.fname, n);
        let void_arg = self.await_suspend_takes_void(aw_ty);
        self.field(MangleType::Class(aw_ty.to_string()), &field);
        self.slots.push(Slot {
            field,
            ty: aw_ty.to_string(),
            void_arg,
        });
        n
    }

    fn await_suspend_takes_void(&self, aw_ty: &str) -> bool {
        match self.classes.get(aw_ty) {
            Some(cd) => member_fn(cd, "await_suspend")
                .map(|(_, params, _)| {
                    matches!(
                        params.first(),
                        Some((_, MangleType::Pointer(inner)))
                            if matches!(**inner, MangleType::Void)
                    )
                })
                .unwrap_or(false),
            None => false,
        }
    }

    /// 把一个类型名解析为真实类名: 沿 typedef 别名链展开 (`typedef
    /// std::suspend_always Susp;` 之后 promise 方法可以返回 `Susp`).
    fn resolve_class_name(&self, ty: &MangleType, depth: u32) -> Option<String> {
        if depth > 4 {
            return None;
        }
        let name = match ty {
            MangleType::Class(c) => c.clone(),
            MangleType::Template { name, .. } => name.clone(),
            _ => return None,
        };
        if let Some(cd) = self.classes.get(&name) {
            // 返回 ClassDecl 自己的裸名: 后面按名字回查 decl 树追加帧字段时只认 `cd.name`
            return Some(cd.name.clone());
        }
        match self.typedefs.get(&name) {
            Some(t) => self.resolve_class_name(t, depth + 1),
            None => None,
        }
    }

    /// 校验 awaitable 类型并返回其类名.
    ///
    /// 帧槽里放的是 awaiter **对象整体**, 读写都降级为按位整块复制, 所以准入
    /// 判据是可平凡复制而非尺寸 (见 [`frame_slot_ty_ok`]).
    fn validate_awaitable(&mut self, ty: &MangleType, ctx: &str) -> Result<String, String> {
        let name = match ty {
            MangleType::Class(_) | MangleType::Template { .. } => {
                match self.resolve_class_name(ty, 0) {
                    Some(n) => n,
                    None => {
                        let shown = match ty {
                            MangleType::Class(c) => c.clone(),
                            MangleType::Template { name, .. } => format!("{}<...>", name),
                            _ => String::new(),
                        };
                        return Err(format!(
                        "{} class '{}' is not defined in this TU (a typedef alias must resolve to \
                         a class defined in the TU)",
                        ctx, shown
                    ));
                    },
                }
            },
            other => {
                return Err(format!(
                    "{} must be a class type with await_ready/await_suspend/await_resume; \
                     got `{}`",
                    ctx,
                    ty_text(other)
                ))
            },
        };
        let cd = self
            .classes
            .get(&name)
            .ok_or_else(|| format!("{} class '{}' is not defined in this TU", ctx, name))?
            .clone();
        for m in ["await_ready", "await_suspend", "await_resume"] {
            match member_fn(&cd, m) {
                None => {
                    return Err(format!(
                        "{} class '{}' lacks an in-class definition of {}",
                        ctx, name, m
                    ))
                },
                Some((ret, _, body_declared)) => {
                    if body_declared {
                        return Err(format!(
                            "{} class '{}' declares {} out of class; the coroutine lowering \
                             requires the definition inside the class body",
                            ctx, name, m
                        ));
                    }
                    if m == "await_ready" && matches!(ret, MangleType::Void) {
                        return Err(format!("{}.await_ready must return a value", name));
                    }
                    if m == "await_suspend" && !matches!(ret, MangleType::Void) {
                        return Err(format!(
                            "non-void {}.await_suspend (handle-overload selection) is not \
                             supported",
                            name
                        ));
                    }
                },
            }
        }
        let data: Vec<&MangleType> = cd
            .members
            .iter()
            .filter_map(|d| match d {
                CppDecl::MemberVar { ty, .. } => Some(ty),
                _ => None,
            })
            .collect();
        // 帧槽整块复制的准入判据 = 可平凡复制 (单一真源 `frame_slot_ty_ok`).
        // 尺寸不再是理由: x86_64 后端 "多 quad 聚合值加载漏第二字" 已修,
        // 24/32 字节 awaiter 与 g++ 逐字节对照通过 (cpp_coroutine_fat_awaiter_slot_run).
        if !frame_slot_ty_ok(&MangleType::Class(name.clone()), &self.classes) {
            let members: Vec<String> = data.iter().map(|t| ty_text(t)).collect();
            return Err(format!(
                "awaiter class '{}' cannot live in the coroutine frame slot: the slot is \
                 copied as a whole object, so the class must be trivially copyable (no bases, \
                 no virtual functions, no user destructor or `operator=`, every data member \
                 scalar/pointer or itself trivially copyable); data members: [{}]",
                name,
                members.join(", ")
            ));
        }
        if data.is_empty() {
            // 0 数据成员的类在 C++ 里大小是 1, 帧里整体存取的尺寸不确定 -> 注入占位字段.
            self.pad_classes.insert(name.clone());
        }
        Ok(name)
    }

    /// `co_await e` / `co_yield e` 的 awaiter 表达式与类型.
    fn awaiter_for(&self, e: &CppExpr, yield_: bool) -> Result<(CppExpr, MangleType), String> {
        if yield_ {
            if let Some(ret) = promise_member_ret(&self.promise_cls, "yield_value") {
                return Ok((call(ptr_member(ident(FP), "yield_value"), vec![e.clone()]), ret));
            }
            if promise_member_ret(&self.promise_cls, "await_transform").is_none() {
                return Err(format!(
                    "promise_type '{}' has neither yield_value nor await_transform, so co_yield \
                     is ill-formed",
                    self.promise
                ));
            }
        }
        if let Some(ret) = promise_member_ret(&self.promise_cls, "await_transform") {
            return Ok((call(ptr_member(ident(FP), "await_transform"), vec![e.clone()]), ret));
        }
        // 无 await_transform: 操作数本身就是 awaitable, 需要能推断其类型
        match self.infer_ty(e) {
            Some(t) => Ok((e.clone(), t)),
            None => Err(format!(
                "promise_type '{}' has no await_transform and the co_await operand type cannot \
                 be inferred by the coroutine lowering",
                self.promise
            )),
        }
    }

    fn infer_ty(&self, e: &CppExpr) -> Option<MangleType> {
        match e {
            // 提升已完成: 变量引用现在是 `fp->dcc_coro_*`
            CppExpr::PtrMember(base, field) => {
                if !matches!(&**base, CppExpr::Ident(n) if n.as_str() == FP) {
                    return None;
                }
                self.fields
                    .iter()
                    .find(|(_, fl)| fl.as_str() == field.as_str())
                    .map(|(t, _)| t.clone())
            },
            CppExpr::Cast(t, _) => Some(t.clone()),
            CppExpr::Call(f, _) => match &**f {
                CppExpr::Ident(n) => self.fn_rets.get(n).cloned(),
                _ => None,
            },
            _ => None,
        }
    }

    /// 恢复臂: `await_resume` 打在帧槽 glvalue 上, 因而看得到 `await_suspend`
    /// 期间对 awaiter 的写 (见模块文档 [basic.coro.await] 段落)。
    fn resume_stmts(&self, slot: usize) -> Vec<CppStmt> {
        let s = &self.slots[slot];
        vec![stmt(call(
            member(frame(&s.field), "await_resume"),
            Vec::new(),
        ))]
    }

    /// 挂起臂前奏: awaiter 只求值一次并落帧槽, 不再拷出副本。
    fn suspend_stmts(&self, slot: usize, aw_expr: CppExpr) -> Vec<CppStmt> {
        let s = &self.slots[slot];
        vec![stmt(assign(frame(&s.field), aw_expr))]
    }

    fn suspend_arg(&self, slot: usize) -> CppExpr {
        let s = &self.slots[slot];
        let fp = ident(FP);
        if s.void_arg {
            void_ptr(fp)
        } else {
            fp
        }
    }

    // ------------------------------------------------------------------
    // 反向编译: 语句序列 → 臂图
    // ------------------------------------------------------------------

    /// 编译 `stmts`, 正常结束流向 `exit`; 返回该序列的入口臂号.
    /// `brk` / `cont` 是外层 break / continue 目标 (None = 不在循环内).
    fn compile_seq(
        &mut self,
        stmts: &[CppStmt],
        exit: usize,
        brk: Option<usize>,
        cont: Option<usize>,
    ) -> Result<usize, String> {
        let mut next = exit;
        let mut buf: Vec<CppStmt> = Vec::new();
        for s in stmts.iter().rev() {
            match s {
                CppStmt::Expr(e) => buf.push(stmt(e.clone())),
                CppStmt::LocalVar { name, init, .. } => {
                    let field = self.local_field(name);
                    if let Some(v) = init {
                        buf.push(stmt(assign(frame(&field), v.clone())))
                    }
                },
                // `[[fallthrough]]` 只是文档性标记: 本实现用"上一臂的 fall 目标 =
                // 下一臂入口"自然表达贯穿, 无需发射代码.
                CppStmt::Fallthrough { .. } => {},
                CppStmt::Decl(CppDecl::Typedef { .. }) => {},
                other => {
                    if !buf.is_empty() {
                        next = self.alloc_block(std::mem::take(&mut buf), next);
                    }
                    next = self.compile_stmt(other, next, brk, cont)?;
                },
            }
        }
        if !buf.is_empty() {
            next = self.alloc_block(buf, next);
        }
        Ok(next)
    }

    fn alloc_block(&mut self, stmts: Vec<CppStmt>, next: usize) -> usize {
        let a = self.alloc();
        self.arms[a] = Arm {
            stmts,
            term: Term::Goto(next),
        };
        a
    }

    fn local_field(&self, name: &str) -> String {
        format!("{}{}_{}", LOCAL_PREFIX, self.fname, name)
    }

    fn compile_stmt(
        &mut self,
        s: &CppStmt,
        next: usize,
        brk: Option<usize>,
        cont: Option<usize>,
    ) -> Result<usize, String> {
        match s {
            CppStmt::Expr(e) => Ok(self.alloc_block(vec![stmt(e.clone())], next)),
            CppStmt::LocalVar { name, init, .. } => {
                let field = self.local_field(name);
                let st = match init {
                    Some(v) => vec![stmt(assign(frame(&field), v.clone()))],
                    None => Vec::new(),
                };
                Ok(self.alloc_block(st, next))
            },
            CppStmt::LocalVarList { spec, declarators } => {
                // 静态存储期在协程里本来就不支持 (见 StaticLocal), 多 declarator
                // 的 static 声明同样显式报错, 不能静默丢初始化式。
                if spec.is_static {
                    return Err(
                        "function-local static storage duration inside a coroutine is not supported"
                            .to_string(),
                    );
                }
                // 每个 declarator 的初始化式都是一次对帧槽的赋值; 无初始化式的
                // 变量已由 collect_promoted_locals 落帧, 此处无需发射。
                let mut st: Vec<CppStmt> = Vec::new();
                for d in declarators {
                    if let Some(v) = &d.init {
                        let field = self.local_field(&d.name);
                        st.push(stmt(assign(frame(&field), v.clone())));
                    }
                }
                Ok(self.alloc_block(st, next))
            },
            CppStmt::Fallthrough { .. } => Ok(next),
            CppStmt::Decl(CppDecl::Typedef { .. }) => Ok(next),
            CppStmt::Block(b) => self.compile_seq(&b.stmts, next, brk, cont),
            CppStmt::Labeled { .. } => {
                Err("goto / labeled statements are not supported in a coroutine body".to_string())
            },
            CppStmt::Goto(_) => Err("goto is not supported in a coroutine body".to_string()),
            CppStmt::Return(_) => Err(
                "[dcl.fct.def.coroutine] a coroutine body shall not contain a return statement; \
                 use co_return"
                    .to_string(),
            ),
            CppStmt::Try { .. } | CppStmt::Throw(_) => {
                Err("co_await / co_yield inside a try / catch or throw expression is not yet \
                 lowered (the exception edge needs promise::unhandled_exception)"
                    .to_string())
            },
            CppStmt::StaticLocal { .. } => {
                Err("function-local static storage duration inside a coroutine is not supported"
                    .to_string())
            },
            CppStmt::StructuredBinding { .. } => {
                Err("structured bindings inside a coroutine are not supported".to_string())
            },
            CppStmt::OpaqueStmt(t) => {
                Err(format!("unparsed statement inside a coroutine body: {}", t.trim()))
            },
            CppStmt::ThisMember(..) => Err("`this->` member access is not supported in a \
                                            coroutine body"
                .to_string()),
            CppStmt::If { cond, then, els } => {
                let t = self.compile_seq(&then.stmts, next, brk, cont)?;
                let f = match els {
                    Some(e) => self.compile_seq(&e.stmts, next, brk, cont)?,
                    None => next,
                };
                let a = self.alloc();
                self.arms[a] = Arm {
                    stmts: Vec::new(),
                    term: Term::Branch {
                        cond: cond.clone(),
                        then: t,
                        els: f,
                    },
                };
                Ok(a)
            },
            CppStmt::IfConstexpr { cond, then, els } => {
                // 协程体里的 `if constexpr` 退化为运行期分支 (两分支都必须良构).
                self.compile_stmt(
                    &CppStmt::If {
                        cond: cond.clone(),
                        then: then.clone(),
                        els: els.clone(),
                    },
                    next,
                    brk,
                    cont,
                )
            },
            CppStmt::While { cond, body } => {
                let c = self.alloc();
                let b = self.compile_seq(&body.stmts, c, Some(next), Some(c))?;
                self.arms[c] = Arm {
                    stmts: Vec::new(),
                    term: Term::Branch {
                        cond: cond.clone(),
                        then: b,
                        els: next,
                    },
                };
                Ok(c)
            },
            CppStmt::DoWhile { body, cond } => {
                let c = self.alloc();
                let b = self.compile_seq(&body.stmts, c, Some(next), Some(c))?;
                self.arms[c] = Arm {
                    stmts: Vec::new(),
                    term: Term::Branch {
                        cond: cond.clone(),
                        then: b,
                        els: next,
                    },
                };
                let e = self.alloc();
                self.arms[e] = Arm {
                    stmts: Vec::new(),
                    term: Term::Goto(b),
                };
                Ok(e)
            },
            CppStmt::For {
                init,
                cond,
                step,
                body,
            } => {
                let c = self.alloc();
                let step_entry = match step {
                    Some(s) => self.compile_stmt(s, c, Some(next), Some(c))?,
                    None => c,
                };
                let b = self.compile_seq(&body.stmts, step_entry, Some(next), Some(step_entry))?;
                let true_lit = int_lit(1);
                self.arms[c] = Arm {
                    stmts: Vec::new(),
                    term: Term::Branch {
                        cond: cond.clone().unwrap_or(true_lit),
                        then: b,
                        els: next,
                    },
                };
                match init {
                    Some(i) => self.compile_stmt(i, c, Some(next), Some(step_entry)),
                    None => Ok(c),
                }
            },
            CppStmt::Switch { expr, arms, .. } => {
                // 转成 `if (sel == v) goto e_k` 链: 与 switch 等价, 且天然支持
                // case 尾无 break 的贯穿 (上一 case 的 fall 目标 = 下一 case 入口).
                let mut fall = next;
                let mut cases: Vec<(Option<CppExpr>, usize)> = Vec::with_capacity(arms.len());
                for (val, b) in arms.iter().rev() {
                    let e = self.compile_seq(&b.stmts, fall, Some(next), cont)?;
                    cases.push((val.clone(), e));
                    fall = e;
                }
                cases.reverse();
                let mut dispatch = next;
                for (val, e) in cases.iter().rev() {
                    match val {
                        Some(v) => {
                            let a = self.alloc();
                            self.arms[a] = Arm {
                                stmts: Vec::new(),
                                term: Term::Branch {
                                    cond: CppExpr::BinOp(
                                        BinOpKind::EqEq,
                                        Box::new(expr.clone()),
                                        Box::new(v.clone()),
                                    ),
                                    then: *e,
                                    els: dispatch,
                                },
                            };
                            dispatch = a;
                        },
                        None => dispatch = *e,
                    }
                }
                // default 必须在所有 case 比较之后才落到
                let has_default = arms.iter().any(|(v, _)| v.is_none());
                if !has_default {
                    // dispatch 已经以 next 收尾
                }
                Ok(dispatch)
            },
            CppStmt::Break => match brk {
                Some(b) => {
                    let a = self.alloc();
                    self.arms[a] = Arm {
                        stmts: Vec::new(),
                        term: Term::Goto(b),
                    };
                    Ok(a)
                },
                None => Err("`break` outside a loop or switch inside a coroutine".to_string()),
            },
            CppStmt::Continue => match cont {
                Some(c) => {
                    let a = self.alloc();
                    self.arms[a] = Arm {
                        stmts: Vec::new(),
                        term: Term::Goto(c),
                    };
                    Ok(a)
                },
                None => Err("`continue` outside a loop inside a coroutine".to_string()),
            },
            CppStmt::CoYield(e) => self.compile_suspend(e, true, next),
            CppStmt::CoAwait(e) => self.compile_suspend(e, false, next),
            CppStmt::CoReturn(v) => {
                let st = match v {
                    Some(x) => {
                        if promise_member_ret(&self.promise_cls, "return_value").is_none() {
                            return Err(format!(
                                "co_return with a value needs promise_type '{}'::return_value",
                                self.promise
                            ));
                        }
                        vec![stmt(call(
                            ptr_member(ident(FP), "return_value"),
                            vec![x.clone()],
                        ))]
                    },
                    None => {
                        if promise_member_ret(&self.promise_cls, "return_void").is_none() {
                            return Err(format!(
                                "co_return without a value needs promise_type '{}'::return_void",
                                self.promise
                            ));
                        }
                        vec![stmt(call(ptr_member(ident(FP), "return_void"), Vec::new()))]
                    },
                };
                let a = self.alloc();
                self.arms[a] = Arm {
                    stmts: st,
                    term: Term::Goto(self.final_arm),
                };
                Ok(a)
            },
            // 嵌套声明 / 其余未被上面覆盖的语句: 显式报错, 不静默丢弃
            other => Err(format!(
                "coroutine lowering does not support this statement in a coroutine body: {}",
                stmt_kind(other)
            )),
        }
    }

    fn compile_suspend(&mut self, e: &CppExpr, yield_: bool, next: usize) -> Result<usize, String> {
        let ctx = if yield_ { "co_yield operand" } else { "co_await operand" };
        let (aw_expr, aw_ty) = self.awaiter_for(e, yield_)?;
        let ty = self.validate_awaitable(&aw_ty, ctx)?;
        let slot = self.alloc_slot(&ty);
        let r = self.alloc();
        self.arms[r] = Arm {
            stmts: self.resume_stmts(slot),
            term: Term::Goto(next),
        };
        let a = self.alloc();
        self.arms[a] = Arm {
            stmts: self.suspend_stmts(slot, aw_expr),
            term: Term::Suspend {
                slot,
                resume: r,
                done: false,
            },
        };
        Ok(a)
    }

    // ------------------------------------------------------------------
    // 臂图 → CppStmt
    // ------------------------------------------------------------------

    /// 臂号 → `dcc_coro_state` 里落存的 case 常量.
    ///
    /// **必须保证所有臂号的十进制位数相同**: native 后端 `lower_ir::lower_switch_stmt`
    /// 是用 `ssa.blocks.keys().find(|k| k.starts_with("case_<值>"))` 找 case 块的,
    /// 而 `blocks` 是 `HashMap` —— 一旦某个臂号是另一个的前缀 (`case_1` / `case_10`),
    /// 同一个进程每次运行会随机绑到不同块上, 轻则臂体接错, 重则整个 `await_suspend`
    /// 调用被丢空 (实测 11 臂 generator 退出码 0/7 随机翻转). 等位数编号从根上消除
    /// 前缀关系: ≤ 9 臂用 0..8, 10..90 臂用 10..99, 91..900 臂用 100..999 …
    fn state_val(&self, arm: usize) -> i64 {
        (self.state_base as i64) + arm as i64
    }

    fn term_stmts(&self, t: &Term) -> Vec<CppStmt> {
        match t {
            Term::Unset => vec![],
            Term::Halt => vec![],
            Term::Goto(i) => vec![set_state(self.state_val(*i)), set_again()],
            Term::Branch { cond, then, els } => vec![if_stmt(
                cond.clone(),
                vec![set_state(self.state_val(*then)), set_again()],
                Some(vec![set_state(self.state_val(*els)), set_again()]),
            )],
            Term::Suspend { slot, resume, done } => {
                let s = &self.slots[*slot];
                let mut v = Vec::new();
                if *done {
                    v.push(stmt(assign(frame(DONE), int_lit(1))));
                }
                v.push(set_state(self.state_val(*resume)));
                // ready / suspend 同样打在帧槽 glvalue 上 (单一对象, 见模块文档)
                v.push(if_stmt(
                    CppExpr::UnaryOp(
                        UnaryOpKind::LNot,
                        Box::new(call(member(frame(&s.field), "await_ready"), Vec::new())),
                    ),
                    vec![stmt(call(
                        member(frame(&s.field), "await_suspend"),
                        vec![self.suspend_arg(*slot)],
                    ))],
                    Some(vec![set_again()]),
                ));
                v
            },
        }
    }

    fn emit_switch(&self) -> CppStmt {
        let mut arms: Vec<(Option<CppExpr>, CppBody)> = Vec::with_capacity(self.arms.len() + 1);
        let mut spans: Vec<Span> = Vec::with_capacity(self.arms.len() + 1);
        for (i, a) in self.arms.iter().enumerate() {
            let mut body = a.stmts.clone();
            body.extend(self.term_stmts(&a.term));
            body.push(CppStmt::Break);
            arms.push((Some(int_lit(self.state_val(i))), body_of(body)));
            spans.push(span());
        }
        arms.push((None, body_of(vec![CppStmt::Break])));
        spans.push(span());
        CppStmt::Switch {
            expr: frame(STATE),
            arms,
            label_spans: spans,
        }
    }

    fn build(&mut self, body: &CppBody) -> Result<(), String> {
        if !self.arms.is_empty() {
            return Err("Machine::build called twice".to_string());
        }
        for _ in 0..4 {
            self.alloc();
        }
        let init_ty = promise_member_ret(&self.promise_cls, "initial_suspend")
            .ok_or_else(|| format!("promise_type '{}' lacks initial_suspend()", self.promise))?;
        let final_ty = promise_member_ret(&self.promise_cls, "final_suspend")
            .ok_or_else(|| format!("promise_type '{}' lacks final_suspend()", self.promise))?;
        let i_aw = self.validate_awaitable(&init_ty, "initial_suspend result")?;
        let f_aw = self.validate_awaitable(&final_ty, "final_suspend result")?;
        // 槽 0 = initial_suspend, 槽 1 = final_suspend (与预留臂号顺序一致)
        let si = self.alloc_slot(&i_aw);
        let sf = self.alloc_slot(&f_aw);
        if si != 0 || sf != 1 {
            return Err("internal: suspend slot numbering invariant violated".to_string());
        }
        let body_entry = self.compile_seq(&body.stmts, self.final_arm, None, None)?;
        let init_call = stmt(assign(
            frame(&self.slots[0].field),
            call(ptr_member(ident(FP), "initial_suspend"), Vec::new()),
        ));
        let final_call = stmt(assign(
            frame(&self.slots[1].field),
            call(ptr_member(ident(FP), "final_suspend"), Vec::new()),
        ));
        let pre = vec![init_call];
        self.arms[self.init_arm] = Arm {
            stmts: pre,
            term: Term::Suspend {
                slot: 0,
                resume: self.init_resume,
                done: false,
            },
        };
        self.arms[self.init_resume] = Arm {
            stmts: self.resume_stmts(0),
            term: Term::Goto(body_entry),
        };
        let pfe = vec![final_call];
        self.arms[self.final_arm] = Arm {
            stmts: pfe,
            term: Term::Suspend {
                slot: 1,
                resume: self.final_resume,
                done: true,
            },
        };
        let mut post = self.resume_stmts(1);
        post.push(stmt(assign(frame(DONE), int_lit(1))));
        self.arms[self.final_resume] = Arm {
            stmts: post,
            term: Term::Halt,
        };
        for (i, a) in self.arms.iter().enumerate() {
            if matches!(a.term, Term::Unset) {
                return Err(format!(
                    "internal: state machine arm {} has no terminator ({} in '{}')",
                    i, self.promise, self.fname
                ));
            }
        }
        // 臂号偏移: 取最小的 10^k 使 [base, base+n) 全落在同一十进制位宽内
        // (n <= 9*base, base>=10; n<=9 时直接用 0..n-1).
        if self.arms.len() > 900_000 {
            return Err(format!(
                "coroutine '{}' has too many suspension points ({} > 900000)",
                self.fname,
                self.arms.len()
            ));
        }
        let mut cap = 9usize;
        let mut base = 0usize;
        while self.arms.len() > cap {
            cap *= 10;
            base = if base == 0 { 10 } else { base * 10 };
        }
        self.state_base = base;
        Ok(())
    }

    /// 生成的状态机步进函数.
    fn step_fn(&self) -> CppDecl {
        let pty = MangleType::Class(self.promise.clone());
        let st: Vec<CppStmt> = vec![
            decl_local(MangleType::Pointer(Box::new(pty.clone())), FP),
            decl_local(MangleType::Int, AGAIN),
            stmt(assign(
                ident(FP),
                CppExpr::Cast(MangleType::Pointer(Box::new(pty.clone())), Box::new(ident("pp"))),
            )),
            stmt(assign(ident(AGAIN), int_lit(1))),
            CppStmt::While {
                cond: ident(AGAIN),
                body: Box::new(body_of(vec![
                    stmt(assign(ident(AGAIN), int_lit(0))),
                    self.emit_switch(),
                ])),
            },
            CppStmt::Return(None),
        ];
        CppDecl::FreeFn {
            ret: MangleType::Void,
            name: self.step_name.clone(),
            params: vec![("pp".to_string(), MangleType::Pointer(Box::new(MangleType::Void)))],
            body: body_of(st),
            is_noexcept: false,
            trailing_ret_type: None,
        }
    }

    fn done_fn(&self) -> CppDecl {
        let pty = MangleType::Class(self.promise.clone());
        CppDecl::FreeFn {
            ret: MangleType::Int,
            name: format!("{}{}", DONE_PREFIX, self.promise),
            params: vec![("pp".to_string(), MangleType::Pointer(Box::new(MangleType::Void)))],
            body: body_of(vec![
                decl_local(MangleType::Pointer(Box::new(pty.clone())), FP),
                stmt(assign(
                    ident(FP),
                    CppExpr::Cast(MangleType::Pointer(Box::new(pty)), Box::new(ident("pp"))),
                )),
                CppStmt::Return(Some(frame(DONE))),
            ]),
            is_noexcept: false,
            trailing_ret_type: None,
        }
    }

    fn destroy_fn(&self) -> CppDecl {
        let pty = MangleType::Class(self.promise.clone());
        CppDecl::FreeFn {
            ret: MangleType::Void,
            name: format!("{}{}", DESTROY_PREFIX, self.promise),
            params: vec![("pp".to_string(), MangleType::Pointer(Box::new(MangleType::Void)))],
            body: body_of(vec![
                decl_local(MangleType::Pointer(Box::new(pty.clone())), FP),
                stmt(assign(
                    ident(FP),
                    CppExpr::Cast(
                        MangleType::Pointer(Box::new(pty.clone())),
                        Box::new(ident("pp")),
                    ),
                )),
                stmt(assign(frame(DONE), int_lit(1))),
                stmt(call(ident("free"), vec![void_ptr(ident(FP))])),
            ]),
            is_noexcept: false,
            trailing_ret_type: None,
        }
    }

    /// 协程函数体替换为 ramp: 建帧 → 参数入帧 → get_return_object → 首次步进.
    fn ramp(&self, ret: &MangleType, params: &[(String, MangleType)]) -> Result<CppBody, String> {
        let pty = MangleType::Class(self.promise.clone());
        let pptr = MangleType::Pointer(Box::new(pty.clone()));
        let f = "dcc_coro_frm";
        let r = "dcc_coro_ret";
        let mut st: Vec<CppStmt> = Vec::new();
        st.push(decl_local(pptr.clone(), f));
        if !matches!(ret, MangleType::Void) {
            st.push(decl_local(ret.clone(), r));
        }
        st.push(stmt(assign(
            ident(f),
            CppExpr::Cast(
                pptr.clone(),
                Box::new(call(
                    ident("malloc"),
                    vec![CppExpr::Sizeof(SizeofArg::Type(pty.clone()))],
                )),
            ),
        )));
        st.push(stmt(assign(ptr_member(ident(f), STATE), int_lit(self.state_val(0)))));
        st.push(stmt(assign(ptr_member(ident(f), DONE), int_lit(0))));
        if let Some(t) = self.tag {
            st.push(stmt(assign(ptr_member(ident(f), TAG), int_lit(t))));
        }
        for (n, _t) in params {
            let field = format!("{}{}_{}", PARAM_PREFIX, self.fname, n);
            st.push(stmt(assign(ptr_member(ident(f), &field), ident(n))));
        }
        let gro = call(ptr_member(ident(f), "get_return_object"), Vec::new());
        if matches!(ret, MangleType::Void) {
            st.push(stmt(gro));
        } else {
            st.push(stmt(assign(ident(r), gro)));
        }
        st.push(stmt(call(ident(&self.step_name), vec![void_ptr(ident(f))])));
        st.push(CppStmt::Return(if matches!(ret, MangleType::Void) {
            None
        } else {
            Some(ident(r))
        }));
        Ok(body_of(st))
    }
}

/// 类内成员函数签名 (返回类型, 形参, 是否只有声明).
fn member_fn(cd: &ClassDecl, name: &str) -> Option<MemberFnSig> {
    for m in &cd.members {
        if let CppDecl::MemberFn {
            name: n,
            ret,
            params,
            body,
            ..
        } = m
        {
            if n == name {
                return Some((ret.clone(), params.clone(), body.is_decl));
            }
        }
    }
    None
}

fn ty_text(t: &MangleType) -> String {
    match t {
        MangleType::Class(c) => c.clone(),
        MangleType::Void => "void".to_string(),
        MangleType::Int => "int".to_string(),
        MangleType::Long => "long".to_string(),
        MangleType::Bool => "bool".to_string(),
        MangleType::Pointer(inner) => format!("{}*", ty_text(inner)),
        MangleType::Template { name, .. } => format!("{}<...>", name),
        other => format!("{:?}", other),
    }
}

// ---------------------------------------------------------------------------
// 提升 (promotion): 形参 / 局部量 → 帧字段
// ---------------------------------------------------------------------------

/// 帧槽 (awaiter 槽 / 提升的形参与局部量) 可整体存取的类型 —— **单一真源**.
///
/// 判定不是尺寸而是 **可平凡复制**: 帧槽读写被降级成 `fp->slot = aw` /
/// `d = fp->slot` 的裸整块复制, 后端按 8 字节分块 `mov` 搬运对象表示, 既不会
/// 调用户 `operator=` / 析构, 也不处理子对象与 vptr 之外的语义. 于是:
///  * 标量 / 指针 —— 一条 mov 完成;
///  * 类类型 —— 非 union (激活成员不确定)、无基类 (子对象不能各管各地赋)、
///    无虚函数/虚析构 (vptr 不该被裸抄)、无用户声明析构与 `operator=`、
///    无匿名聚合实例, 且每个非静态数据成员递归满足本判据;
///  * 仍拒 引用 (帧里存引用无意义)、数组 (帧字段提升本身未支持)、
///    `long double` (走 x87 st(0) 通道, 不是一条 mov)、本 TU 查不到的类.
///
/// 尺寸上限早已不是理由: x86_64 后端 "多 quad 聚合值加载漏第二个字" 的缺陷已修
/// (见模块文档 “已知边界”), 12/16/24/32 字节 awaiter 均有 gcc 对照验证.
fn frame_slot_ty_ok(t: &MangleType, classes: &HashMap<String, ClassDecl>) -> bool {
    match t {
        MangleType::Void | MangleType::Reference(_) | MangleType::Array(..) => false,
        // long double 属于 x87 通道, 帧槽整体存取需 fldp/fstpt 而非 mov。
        MangleType::LongDouble => false,
        _ if is_scalar_ty(t) => true,
        MangleType::Class(c) => classes
            .get(c)
            .is_some_and(|cd| cls_is_trivially_copyable(cd, classes, 0)),
        _ => false,
    }
}

/// 类体是否可平凡整体复制 (见 [`frame_slot_ty_ok`] 的逐条理由). `depth` 防递归属环.
fn cls_is_trivially_copyable(
    cd: &ClassDecl,
    classes: &HashMap<String, ClassDecl>,
    depth: u8,
) -> bool {
    const MAX_DEPTH: u8 = 4;
    if cd.is_union || !cd.bases.is_empty() || depth > MAX_DEPTH {
        return false;
    }
    for d in &cd.members {
        match d {
            // 成员函数本身无碍, 但 `virtual` 会在对象里插 vptr —— 裸抄不定.
            CppDecl::MemberFn { is_virtual, .. } => {
                if *is_virtual {
                    return false;
                }
            },
            CppDecl::Dtor { .. } => return false,
            // 普通用户构造函数只影响初始化, 不影响整体复制; 但**拷贝构造**与
            // 拷贝赋值同构, 帧槽的裸 `mov` 不会调用它.
            CppDecl::Ctor { class, params, .. } => {
                if params.len() == 1 && ctor_param_aliases_class(&params[0].1, class) {
                    return false;
                }
            },
            // 用户 `operator=` 意味着拷贝需要语义代码, 而帧槽只会按位搬运.
            CppDecl::OperatorFn { op, .. } => {
                if matches!(op, Operator::Assign) {
                    return false;
                }
            },
            // 匿名聚合实例: 上面的成员收集看不到它, 尺寸不确定.
            CppDecl::AnonAggr { .. } => return false,
            CppDecl::MemberVar {
                ty, is_volatile, ..
            } => {
                if *is_volatile {
                    return false;
                }
                if ty.is_reference_or_bad_member_ty(classes) {
                    return false;
                }
            },
            // 嵌套类定义/ typedef 等不影响对象尺寸.
            _ => {},
        }
    }
    true
}

/// 构造形参是否就是本类自身 (即该 `Ctor` 是拷贝构造). 模板类按裸名比.
fn ctor_param_aliases_class(t: &MangleType, class: &str) -> bool {
    let inner = match t {
        MangleType::Reference(r) => r.as_ref().clone(),
        other => other.clone(),
    };
    match inner {
        MangleType::Class(c) => c == class,
        MangleType::Template { name, .. } => name == class,
        _ => false,
    }
}

impl MangleType {
    /// 成员位置的类型递归检查: 与 [`frame_slot_ty_ok`] 同源, 但额外拒引用成员
    /// (引用成员使对象不可赋值).
    fn is_reference_or_bad_member_ty(&self, classes: &HashMap<String, ClassDecl>) -> bool {
        match self {
            MangleType::Reference(_) => true,
            MangleType::Array(inner, _) => {
                // 数组成员按位整体拷贝语义上合法, 逐元素同构 → 只看元素类型.
                inner.is_reference_or_bad_member_ty(classes)
            },
            _ => !frame_slot_ty_ok(self, classes),
        }
    }
}

/// 可安全落帧的变量类型: 单一真源见 [`frame_slot_ty_ok`].
fn frame_field_ty_ok(t: &MangleType, classes: &HashMap<String, ClassDecl>) -> bool {
    frame_slot_ty_ok(t, classes)
}

/// 收集协程体内所有 `LocalVar` 声明 (含嵌套块 / 循环 init), 保序并检出重名.
fn collect_promoted_locals(body: &CppBody, out: &mut Vec<(String, MangleType)>) {
    for s in &body.stmts {
        collect_locals_stmt(s, out);
    }
}

fn collect_locals_stmt(s: &CppStmt, out: &mut Vec<(String, MangleType)>) {
    match s {
        CppStmt::LocalVar { ty, name, .. } | CppStmt::StaticLocal { ty, name, .. } => {
            out.push((name.clone(), ty.clone()));
        },
        // 逐 declarator 收集 (共享 decl-specifier + 各自的 `*`/`&`/`[N]` 层级):
        // 只收第一个会让多 declarator 声明的后续变量没有帧槽。
        CppStmt::LocalVarList { spec, declarators } => {
            for d in declarators {
                out.push((d.name.clone(), d.full_ty(spec)));
            }
        },
        CppStmt::If { then, els, .. } | CppStmt::IfConstexpr { then, els, .. } => {
            collect_promoted_locals(then, out);
            if let Some(e) = els {
                collect_promoted_locals(e, out);
            }
        },
        CppStmt::While { body, .. } | CppStmt::DoWhile { body, .. } => {
            collect_promoted_locals(body, out);
        },
        CppStmt::Block(body) => collect_promoted_locals(body, out),
        CppStmt::For {
            init, body, step, ..
        } => {
            if let Some(i) = init {
                collect_locals_stmt(i, out);
            }
            if let Some(x) = step {
                collect_locals_stmt(x, out);
            }
            collect_promoted_locals(body, out);
        },
        CppStmt::Switch { arms, .. } => {
            for (_, b) in arms {
                collect_promoted_locals(b, out);
            }
        },
        CppStmt::Try { body, .. } => collect_promoted_locals(body, out),
        CppStmt::Labeled { stmt, .. } | CppStmt::ThisMember(_, stmt) => {
            collect_locals_stmt(stmt, out)
        },
        _ => {},
    }
}

/// 把 `name` 指向的类成员追加进 decls 树里对应的类定义 (顶层 / 命名空间内 / 嵌套类);
/// 协程帧字段就追加在 promise 类体末尾, 只认 `ClassDecl::name` 裸名. 返回是否找到.
fn append_class_members(decls: &mut [CppDecl], name: &str, extra: &[CppDecl]) -> bool {
    for d in decls.iter_mut() {
        match d {
            CppDecl::Class(cd) => {
                if cd.name == name {
                    for e in extra {
                        cd.members.push(e.clone());
                    }
                    return true;
                }
                if append_class_members(&mut cd.members, name, extra) {
                    return true;
                }
            },
            CppDecl::Namespace { body, .. } | CppDecl::InlineNamespace { body, .. } => {
                if append_class_members(body, name, extra) {
                    return true;
                }
            },
            CppDecl::ExportBlock(v) => {
                if append_class_members(v, name, extra) {
                    return true;
                }
            },
            _ => {},
        }
    }
    false
}

/// 登记自由函数返回类型 (供 awaiter 类型推断).
fn index_fn_rets(decls: &[CppDecl], out: &mut HashMap<String, MangleType>) {
    for d in decls {
        match d {
            CppDecl::FreeFn { name, ret, .. } => {
                out.insert(name.clone(), ret.clone());
            },
            CppDecl::Namespace { body, .. } | CppDecl::InlineNamespace { body, .. } => {
                index_fn_rets(body, out)
            },
            CppDecl::ExportBlock(v) => index_fn_rets(v, out),
            _ => {},
        }
    }
}

/// 非顶层 (成员函数 / 构造函数 / 类内) 协程: 本实现只处理顶层自由函数.
fn reject_nested_coroutines(decls: &[CppDecl]) -> Result<(), String> {
    for d in decls {
        match d {
            CppDecl::Class(cd) => {
                for m in &cd.members {
                    if decl_has_coro(m) {
                        return Err(format!(
                            "coroutine member function '{}'::... is not supported; only \
                             top-level coroutine functions can be lowered to a state machine",
                            cd.name
                        ));
                    }
                }
                reject_nested_coroutines(&cd.members)?;
            },
            CppDecl::Ctor { class, body, .. } | CppDecl::Dtor { class, body, .. } => {
                if stmts_have_coro(body) {
                    return Err(format!(
                        "constructor/destructor '{}' cannot be a coroutine",
                        class
                    ));
                }
            },
            CppDecl::Template { decl, .. } | CppDecl::PartialSpec { decl, .. }
                if decl_has_coro(decl) =>
            {
                return Err(
                    "a coroutine inside a template is not supported by the lowering".to_string()
                );
            },
            _ => {},
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 把含协程的 TU 重写为等价的普通 C++20 AST.
///
/// 调用方 ([`crate::cpp::native::lower_decls_native`]) 在 Sema 之前替换自己的
/// 输入声明列表; 返回的列表里不再含任何 `CoAwait` / `CoYield` / `CoReturn`.
pub fn lower_coroutines(decls: &[CppDecl]) -> Result<Vec<CppDecl>, String> {
    let mut out = decls.to_vec();
    reject_nested_coroutines(&out)?;

    let mut classes = HashMap::new();
    index_classes(&out, &mut classes);
    let mut typedefs = HashMap::new();
    index_typedefs(&out, &mut typedefs);

    // ---- 协程函数清单 + promise 归属 -------------------------------------
    let mut coros: Vec<(usize, String, String)> = Vec::new();
    for (i, d) in out.iter().enumerate() {
        if let CppDecl::FreeFn {
            name,
            ret,
            body,
            is_noexcept,
            ..
        } = d
        {
            if body.is_decl || !stmts_have_coro(body) {
                continue;
            }
            if *is_noexcept {
                return Err(format!("coroutine '{}' cannot be declared noexcept", name));
            }
            let p = promise_of_ret(ret, &classes, &typedefs, 0).ok_or_else(|| {
                format!(
                    "coroutine '{}': its return type must be a class type declaring a \
                 promise_type member",
                    name
                )
            })?;
            // 限定名 (`std::Foo`) 规范为裸名: 生成的帧入口符号与类回查都只认裸名
            let p = canonical_class_name(&classes, &p);
            coros.push((i, name.clone(), p));
        }
    }
    if coros.is_empty() {
        return Err("no top-level coroutine function found for the lowering".to_string());
    }
    let mut promises: HashSet<String> = HashSet::new();
    // 同一 promise_type 可以被多个协程函数共用. 本实现里"帧 == promise 对象", 所以
    // 一个 promise 类只能有一份帧字段布局: 给每个协程函数分配一个归属号, 帧里落
    // `dcc_coro_fn`, 各函数自己的状态机改名为 `dcc_coro_step_<P>_<f>`, 而 handle 协议
    // 入口 (只认 promise 类型) `dcc_coro_step_<P>` 退化为按归属号路由的跳板.
    let mut group_n: HashMap<String, usize> = HashMap::new();
    for (_, _, p) in &coros {
        *group_n.entry(p.clone()).or_insert(0) += 1;
    }
    let mut group_seen: HashMap<String, usize> = HashMap::new();
    let mut coro_tag: Vec<Option<i64>> = Vec::with_capacity(coros.len());
    for (_, _, p) in &coros {
        if !classes.contains_key(p) {
            return Err(format!("promise_type class '{}' is not defined", p));
        }
        promises.insert(p.clone());
        let n = group_seen.entry(p.clone()).or_insert(0);
        *n += 1;
        coro_tag.push(if group_n.get(p).copied().unwrap_or(1) > 1 {
            Some((*n - 1) as i64)
        } else {
            None
        });
    }

    // ---- Pass 1 / Pass 2 --------------------------------------------------
    let mut rw = Rewriter::new(Mode::HandleTypes, &promises, &classes);
    for d in out.iter_mut() {
        rw.decl(d);
    }
    if let Some(e) = rw.err {
        return Err(e);
    }
    let mut rw = Rewriter::new(Mode::HandleCalls, &promises, &classes);
    for d in out.iter_mut() {
        rw.decl(d);
    }
    if let Some(e) = rw.err {
        return Err(e);
    }

    // Pass 1 改过成员签名 (await_suspend 的 handle 参数), 索引必须重建
    classes.clear();
    index_classes(&out, &mut classes);
    let mut fn_rets = HashMap::new();
    index_fn_rets(&out, &mut fn_rets);

    // ---- Pass 3: 每个协程函数 → 帧字段 + 状态机 + ramp ---------------------
    let mut pad_classes: HashSet<String> = HashSet::new();
    let mut generated: Vec<CppDecl> = Vec::new();
    let mut protos: Vec<CppDecl> = Vec::new();
    // 已追加到各 promise 类的帧字段名 (同 promise 的第二个函数不得重复追加)
    let mut appended: HashMap<String, HashSet<String>> = HashMap::new();
    // 已生成 done/destroy 入口的 promise (它们只读共享的 done 标志, 一个 promise 一仓)
    let mut shared_fns: HashSet<String> = HashSet::new();
    // 共用 promise 的路由表: promise → (归属号, 步进函数名), 按源序
    let mut routes: HashMap<String, Vec<(i64, String)>> = HashMap::new();
    let mut route_order: Vec<String> = Vec::new();

    for (ci, (idx, fname, promise)) in coros.clone().into_iter().enumerate() {
        let (ret, params, body) = match &out[idx] {
            CppDecl::FreeFn {
                ret, params, body, ..
            } => (ret.clone(), params.clone(), body.clone()),
            _ => return Err("internal: coroutine decl index drifted".to_string()),
        };
        let promise_cls = classes
            .get(&promise)
            .ok_or_else(|| format!("promise_type '{}' disappeared", promise))?
            .clone();

        let mut m = Machine::new(
            promise.clone(),
            fname.clone(),
            promise_cls,
            classes.clone(),
            fn_rets.clone(),
            typedefs.clone(),
        );
        m.field(MangleType::Int, STATE);
        m.field(MangleType::Int, DONE);
        if let Some(t) = coro_tag[ci] {
            m.set_shared(t);
            m.field(MangleType::Int, TAG);
        }

        // 形参入帧
        let mut promote: HashMap<String, String> = HashMap::new();
        for (n, t) in &params {
            if !frame_field_ty_ok(t, &classes) {
                return Err(format!(
                    "coroutine '{}': parameter '{}' has type `{}` which cannot be stored in \
                     the coroutine frame (the frame slot is copied as a whole object: needs a \
                     scalar, pointer, or trivially copyable class)",
                    fname,
                    n,
                    ty_text(t)
                ));
            }
            let field = format!("{}{}_{}", PARAM_PREFIX, fname, n);
            m.field(t.clone(), &field);
            promote.insert(n.clone(), field);
        }
        // 局部量入帧
        let mut locals: Vec<(String, MangleType)> = Vec::new();
        collect_promoted_locals(&body, &mut locals);
        for (n, t) in &locals {
            if promote.contains_key(n) {
                return Err(format!(
                    "coroutine '{}': local '{}' shadows a parameter or another local; the \
                     frame promotion needs unique names",
                    fname, n
                ));
            }
            if !frame_field_ty_ok(t, &classes) {
                return Err(format!(
                    "coroutine '{}': local '{}' has type `{}` which cannot be stored in the \
                     coroutine frame (the frame slot is copied as a whole object: needs a \
                     scalar, pointer, or trivially copyable class)",
                    fname,
                    n,
                    ty_text(t)
                ));
            }
        }
        // 0 数据成员的类局部量也要占位字段, 否则帧槽整体存取尺寸不确定
        for (n, t) in &locals {
            if let MangleType::Class(c) = t {
                if classes
                    .get(c)
                    .map(|cd| {
                        !cd.members
                            .iter()
                            .any(|d| matches!(d, CppDecl::MemberVar { .. }))
                    })
                    .unwrap_or(false)
                {
                    pad_classes.insert(c.clone());
                }
            }
            let _ = n;
        }
        for (n, t) in &locals {
            let field = format!("{}{}_{}", LOCAL_PREFIX, fname, n);
            m.field(t.clone(), &field);
            promote.insert(n.clone(), field);
        }
        // 体重写: 标识符 → 帧字段引用
        let mut body = body;
        {
            let mut rw = Rewriter::new(Mode::Promote, &promises, &classes);
            rw.promote = promote;
            rw.body(&mut body);
            if let Some(e) = rw.err {
                return Err(format!("coroutine '{}': {}", fname, e));
            }
        }

        m.build(&body)?;
        pad_classes.extend(m.pad_classes.clone());
        let step_name = m.step_name.clone();
        generated.push(m.step_fn());
        let first_of_promise = shared_fns.insert(promise.clone());
        if first_of_promise {
            generated.push(m.done_fn());
            generated.push(m.destroy_fn());
        }
        let ramp = m.ramp(&ret, &params)?;
        let mut extra: Vec<CppDecl> = Vec::new();
        for (t, n) in m.fields.iter() {
            if appended
                .entry(promise.clone())
                .or_default()
                .insert(n.clone())
            {
                extra.push(member_var(t.clone(), n));
            }
        }
        if !append_class_members(&mut out, &promise, &extra) {
            return Err(format!("promise_type '{}' is not a top-level class in this TU", promise));
        }
        out[idx] = CppDecl::FreeFn {
            ret,
            name: fname.clone(),
            params,
            body: ramp,
            is_noexcept: false,
            trailing_ret_type: None,
        };
        let pp_void = || vec![("pp".to_string(), MangleType::Pointer(Box::new(MangleType::Void)))];
        protos.push(free_fn_decl(MangleType::Void, &step_name, pp_void()));
        if let Some(t) = coro_tag[ci] {
            if !routes.contains_key(&promise) {
                route_order.push(promise.clone());
                // 路由跳板本身 (只在首一个函数时声一次)
                protos.push(free_fn_decl(
                    MangleType::Void,
                    &format!("{}{}", STEP_PREFIX, promise),
                    pp_void(),
                ));
            }
            routes
                .entry(promise.clone())
                .or_default()
                .push((t, step_name));
        } else if first_of_promise {
            protos.push(free_fn_decl(
                MangleType::Void,
                &format!("{}{}", STEP_PREFIX, promise),
                pp_void(),
            ));
        }
        if first_of_promise {
            protos.push(free_fn_decl(
                MangleType::Int,
                &format!("{}{}", DONE_PREFIX, promise),
                pp_void(),
            ));
            protos.push(free_fn_decl(
                MangleType::Void,
                &format!("{}{}", DESTROY_PREFIX, promise),
                pp_void(),
            ));
        }
    }

    // ---- 共用 promise 的路由跳板 -------------------------------------------
    // `std::coroutine_handle<P>::resume()` 在降级后只能按 promise 类型找入口,
    // 而帧 == promise 对象无法区分它属于哪个协程函数 —— 用归属号补上这一信息.
    for p in &route_order {
        let targets = match routes.get(p) {
            Some(t) if t.len() > 1 => t.clone(),
            _ => continue,
        };
        let pty = MangleType::Class(p.clone());
        let pptr = MangleType::Pointer(Box::new(pty.clone()));
        let mut st: Vec<CppStmt> = Vec::new();
        st.push(decl_local(pptr.clone(), FP));
        st.push(stmt(assign(ident(FP), CppExpr::Cast(pptr, Box::new(ident("pp"))))));
        for (t, name) in &targets {
            st.push(if_stmt(
                CppExpr::BinOp(BinOpKind::EqEq, Box::new(frame(TAG)), Box::new(int_lit(*t))),
                vec![
                    stmt(call(ident(name), vec![void_ptr(ident(FP))])),
                    CppStmt::Return(None),
                ],
                None,
            ));
        }
        st.push(CppStmt::Return(None));
        generated.push(CppDecl::FreeFn {
            ret: MangleType::Void,
            name: format!("{}{}", STEP_PREFIX, p),
            params: vec![("pp".to_string(), MangleType::Pointer(Box::new(MangleType::Void)))],
            body: body_of(st),
            is_noexcept: false,
            trailing_ret_type: None,
        });
    }

    // ---- awaiter 占位字段注入 --------------------------------------------
    // 排序后注入: HashSet 迭代序按进程随机, 必须固定否则同源码两次编译产物不同.
    let mut pad_sorted: Vec<&String> = pad_classes.iter().collect();
    pad_sorted.sort();
    for c in pad_sorted {
        let extra = vec![member_var(MangleType::Long, AWAITER_PAD)];
        if !append_class_members(&mut out, c.as_str(), &extra) {
            return Err(format!("awaitable class '{}' is not defined in this TU", c));
        }
    }

    // ---- 帧分配器与帧入口前向声明 (仅当 TU 里还没有时注入) -----------------
    let mut existing: HashSet<String> = HashSet::new();
    for d in &out {
        if let CppDecl::FreeFn { name, .. } = d {
            existing.insert(name.clone());
        }
    }
    let mut header: Vec<CppDecl> = Vec::new();
    let mut want = vec![
        free_fn_decl(
            MangleType::Pointer(Box::new(MangleType::Void)),
            "malloc",
            vec![("n".to_string(), MangleType::Ulong)],
        ),
        free_fn_decl(
            MangleType::Void,
            "free",
            vec![("p".to_string(), MangleType::Pointer(Box::new(MangleType::Void)))],
        ),
    ];
    want.extend(protos);
    for d in want {
        let n = match &d {
            CppDecl::FreeFn { name, .. } => name.clone(),
            _ => continue,
        };
        if !existing.contains(&n) {
            header.push(d);
        }
    }
    header.extend(out);
    header.extend(generated);
    Ok(header)
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp::parser::CppParser;

    /// 共享夹具前奏: 一个 lazy generator 的 promise / awaiter / handle 协议包装.
    const PRE: &str = r#"
struct GenPromise;
struct SuspEnd {
    bool await_ready() noexcept { return false; }
    void await_suspend(std::coroutine_handle<GenPromise> h) noexcept { (void)h; }
    void await_resume() noexcept {}
};
bool coro_next(GenPromise* p);
int coro_value(GenPromise* p);
void coro_release(GenPromise* p);
struct Generator {
    typedef GenPromise promise_type;
    GenPromise* p;
    bool next() { return coro_next(p); }
    int value() { return coro_value(p); }
    void release() { coro_release(p); }
};
struct GenPromise {
    int yielded;
    Generator get_return_object() { Generator g; g.p = this; yielded = 0; return g; }
    SuspEnd initial_suspend() { SuspEnd a; return a; }
    SuspEnd final_suspend() noexcept { SuspEnd a; return a; }
    SuspEnd yield_value(int v) { yielded = v; SuspEnd a; return a; }
    void return_void() {}
    void unhandled_exception() {}
};
bool coro_next(GenPromise* p) {
    std::coroutine_handle<GenPromise> h = std::coroutine_handle<GenPromise>::from_promise(*p);
    if (h.done()) return false;
    h.resume();
    return !h.done();
}
int coro_value(GenPromise* p) { return p->yielded; }
void coro_release(GenPromise* p) {
    std::coroutine_handle<GenPromise> h = std::coroutine_handle<GenPromise>::from_promise(*p);
    h.destroy();
}
"#;

    fn parse(src: &str) -> Vec<CppDecl> {
        let mut p = CppParser::new(src.as_bytes(), "unit.cpp");
        p.parse_top_level().expect("parse fixture")
    }

    fn lowered(body: &str) -> Vec<CppDecl> {
        lower_coroutines(&parse(&format!("{}{}", PRE, body))).expect("lower")
    }

    /// 取生成函数名下的语句列表 (前插的**原型声明** body 为空, 必须跳过).
    fn fn_stmts(decls: &[CppDecl], name: &str) -> Vec<CppStmt> {
        let mut found: Option<Vec<CppStmt>> = None;
        for d in decls {
            if let CppDecl::FreeFn { name: n, body, .. } = d {
                if n == name && !body.is_decl && !body.stmts.is_empty() {
                    found = Some(body.stmts.clone());
                }
            }
        }
        found.unwrap_or_else(|| panic!("generated function {} missing (or empty)", name))
    }

    fn has_decl(decls: &[CppDecl], name: &str) -> bool {
        decls.iter().any(|d| match d {
            CppDecl::FreeFn { name: n, .. } => n == name,
            _ => false,
        })
    }

    /// 类定义里的数据成员名 (按声明序).
    fn class_fields(decls: &[CppDecl], cls: &str) -> Vec<String> {
        fn walk(members: &[CppDecl], out: &mut Vec<String>) {
            for m in members {
                if let CppDecl::MemberVar { name, .. } = m {
                    out.push(name.clone());
                }
            }
        }
        let mut v = Vec::new();
        for d in decls {
            if let CppDecl::Class(cd) = d {
                if cd.name == cls {
                    walk(&cd.members, &mut v);
                }
            }
        }
        v
    }

    /// 递归收集语句树里每个 switch 的整数 case 常量 (一组 = 一个 switch).
    fn switches(stmts: &[CppStmt], out: &mut Vec<Vec<i64>>) {
        for s in stmts {
            match s {
                CppStmt::Switch { arms, .. } => {
                    out.push(
                        arms.iter()
                            .filter_map(|(v, _)| match v {
                                Some(CppExpr::IntLit(n)) => Some(*n),
                                _ => None,
                            })
                            .collect(),
                    );
                    for (_, b) in arms {
                        switches(&b.stmts, out);
                    }
                },
                CppStmt::Block(b) => switches(&b.stmts, out),
                CppStmt::While { body, .. } => switches(&body.stmts, out),
                CppStmt::If { then, els, .. } => {
                    switches(&then.stmts, out);
                    if let Some(e) = els {
                        switches(&e.stmts, out);
                    }
                },
                _ => {},
            }
        }
    }

    /// 一个 switch 的所有 case 常量十进制位数相同 ⇔ 互不为前缀.
    ///
    /// 后端 `lower_switch_stmt` 用 `blocks.keys().find(|k| k.starts_with("case_<v>"))`
    /// 定位 case 块, 而 `blocks` 是 HashMap: 只要两个 case 值成前缀关系 (1 / 10),
    /// 同一份源码每次编译会随机绑到不同块, 产出不同目标码 (实测退出码 0/7 翻转).
    fn same_digit_width(vals: &[i64]) -> bool {
        let mut w = None;
        for v in vals {
            let d = v.to_string().len();
            match w {
                None => w = Some(d),
                Some(x) if x != d => return false,
                _ => {},
            }
        }
        true
    }

    fn text<T: std::fmt::Debug>(v: &T) -> String {
        format!("{:?}", v)
    }

    /// PRE 的变体: 把 `Generator` 的 promise_type / 指针成员换成 promise `p`,
    /// 并前置 `struct p;` 前向声明 —— promise 协议诊断用例要自带一个 promise 类,
    /// 而 promise 是由**返回类型**的 `promise_type` 成员选定的, 不改这里就会选中
    /// PRE 的 GenPromise, 诊断根本不会触发.
    fn pre_with(p: &str) -> String {
        format!(
            "struct {};\n{}",
            p,
            PRE.replace("typedef GenPromise promise_type", &format!("typedef {} promise_type", p))
                .replace("GenPromise* p;", &format!("{}* p;", p))
        )
    }

    fn err_with(p: &str, body: &str) -> String {
        match lower_coroutines(&parse(&format!("{}{}", pre_with(p), body))) {
            Ok(v) => panic!("期望被拒绝, 却成功产出 {} 个声明 (promise {})", v.len(), p),
            Err(e) => e,
        }
    }

    /// 以 promise `p` 为返回类型 promise_type 的夹具做正常降级.
    fn lowered_with(p: &str, body: &str) -> Vec<CppDecl> {
        lower_coroutines(&parse(&format!("{}{}", pre_with(p), body))).expect("lower")
    }

    /// PRE + 额外源码直接降级 (多个协程函数共用 PRE 的 Generator / GenPromise).
    fn lowered_with_src(src: &str) -> Vec<CppDecl> {
        lower_coroutines(&parse(&format!("{}{}", PRE, src))).expect("lower")
    }

    /// 名为 `name` 的函数**定义** (非前向声明) 个数.
    fn count_fn_defs(decls: &[CppDecl], name: &str) -> usize {
        decls
            .iter()
            .filter(|d| {
                matches!(d, CppDecl::FreeFn { name: n, body, .. }
                    if n.as_str() == name && !body.is_decl)
            })
            .count()
    }

    /// 小型 generator: 3 个挂起点 (initial / co_yield / final).
    const FX_SIMPLE: &str = "Generator f() { co_yield 1; co_yield 2; co_return; }\n";
    /// 带循环与回边的 generator: 臂数 > 9, 必然踩到多位臂号编号路径.
    const FX_LOOP: &str = "Generator counter(int n) {\n\
         int i = 1;\n\
         int total = 0;\n\
         while (i <= n) { total = total + i; co_yield i; i = i + 1; }\n\
         for (int k = 0; k < 1; k = k + 1) { co_yield -total; }\n\
         co_return;\n\
         }\n";

    // ---- 状态机形状 ----------------------------------------------------

    #[test]
    fn arm_case_values_never_prefix_collide() {
        // 小机器: 单位臂号 0..k; 大机器 (>=10 臂): 等十进制位宽偏移编号.
        for fx in [FX_SIMPLE, FX_LOOP] {
            let d = lowered(fx);
            let ss = fn_stmts(&d, "dcc_coro_step_GenPromise");
            let mut sw = Vec::new();
            switches(&ss, &mut sw);
            assert_eq!(sw.len(), 1, "步进函数只能有一个派发 switch");
            let vals = &sw[0];
            assert!(
                same_digit_width(vals),
                "臂号 {:?} 存在前缀关系 (后端 case_<v> 前缀匹配会随机接错块)",
                vals
            );
            let mut uniq: Vec<i64> = vals.clone();
            uniq.sort();
            uniq.dedup();
            assert_eq!(uniq.len(), vals.len(), "臂号重复: {:?}", vals);
        }
        // 循环夹具确实跨过了 >=10 臂的门限 (否则本回归无意义)
        let d = lowered(FX_LOOP);
        let vals = {
            let mut sw = Vec::new();
            switches(&fn_stmts(&d, "dcc_coro_step_GenPromise"), &mut sw);
            sw[0].clone()
        };
        assert!(vals.len() >= 11, "11 臂 generator 实际只产生 {:?}", vals);
        assert!(vals.iter().all(|v| *v >= 10), ">=10 臂应从 10 起编号: {:?}", vals);
    }

    #[test]
    fn dispatch_switch_reads_frame_state_and_every_arm_breaks() {
        let d = lowered(FX_SIMPLE);
        let ss = fn_stmts(&d, "dcc_coro_step_GenPromise");
        let t = text(&ss);
        // switch (fp->dcc_coro_state)
        assert!(
            t.contains("Switch { expr: PtrMember(Ident(\"dcc_coro_fp\"), \"dcc_coro_state\")"),
            "缺少状态分发: {}",
            &t[..t.len().min(200)]
        );
        // 每个臂都以显式 break 收尾 (臂间不靠 fallthrough 隐式推进)
        let body = match ss.iter().find(|s| matches!(s, CppStmt::While { .. })) {
            Some(CppStmt::While { body, .. }) => body,
            other => panic!("步进函数里没有外层 while: {:?}", other),
        };
        let arms_of_switch = match &body.stmts[1] {
            CppStmt::Switch { arms, .. } => arms.clone(),
            other => panic!("while 体第二条语句不是 switch: {:?}", other),
        };
        for (i, (_, b)) in arms_of_switch.iter().enumerate() {
            assert!(matches!(b.stmts.last(), Some(CppStmt::Break)), "臂 {} 未以 break 收尾", i);
        }
        // default 臂必须存在 (未知状态号不得静默跑到别处)
        assert!(arms_of_switch.iter().any(|(v, _)| v.is_none()), "缺少 default 臂");
    }

    #[test]
    fn outer_loop_is_gated_by_again_flag() {
        let d = lowered(FX_SIMPLE);
        let ss = fn_stmts(&d, "dcc_coro_step_GenPromise");
        let t = text(&ss);
        assert!(
            t.contains("While { cond: Ident(\"dcc_coro_again\")"),
            "外层循环未由 again 标志驱动: {}",
            t
        );
        assert_eq!(
            t.matches("BinOp(Assign, Ident(\"dcc_coro_again\"), IntLit(0))")
                .count(),
            1,
            "again 应在循环头被清零恰好一次"
        );
        assert!(
            t.contains("BinOp(Assign, Ident(\"dcc_coro_again\"), IntLit(1))"),
            "就绪/推进路径应置 again=1"
        );
    }

    #[test]
    fn ramp_initializes_state_done_and_promotes_params() {
        let d = lowered(FX_LOOP);
        // 原函数被改写成 ramp: 建帧 + 字段初始化 + get_return_object + 首次 step
        let t = text(&fn_stmts(&d, "counter"));
        assert!(
            t.contains("Call(Ident(\"malloc\"), [Sizeof(Type(Class(\"GenPromise\")))])"),
            "ramp 未分配帧: {}",
            t
        );
        assert!(
            t.contains("PtrMember(Ident(\"dcc_coro_frm\"), \"dcc_coro_done\"), IntLit(0))"),
            "ramp 未清零 done"
        );
        assert!(t.contains("get_return_object"), "ramp 未调 get_return_object");
        assert!(
            t.contains(
                "PtrMember(Ident(\"dcc_coro_frm\"), \"dcc_coro_p_counter_n\"), Ident(\"n\"))"
            ),
            "形参 n 未提升进帧: {}",
            t
        );
        assert!(t.contains("Call(Ident(\"dcc_coro_step_GenPromise\")"), "ramp 未驱动首次 step");
        let mut sw = Vec::new();
        switches(&fn_stmts(&d, "dcc_coro_step_GenPromise"), &mut sw);
        let first = *sw[0].iter().min().unwrap();
        assert!(
            t.contains(&format!(
                "PtrMember(Ident(\"dcc_coro_frm\"), \"dcc_coro_state\"), IntLit({}))",
                first
            )),
            "ramp 应把初始状态置为首臂 {} (臂表 {:?})",
            first,
            sw[0]
        );
    }

    #[test]
    fn frame_fields_are_appended_to_the_promise_class() {
        let d = lowered(FX_LOOP);
        let f = class_fields(&d, "GenPromise");
        for want in [
            "yielded",
            STATE,
            DONE,
            "dcc_coro_p_counter_n",
            "dcc_coro_l_counter_i",
            "dcc_coro_l_counter_total",
            "dcc_coro_l_counter_k",
        ] {
            assert!(f.contains(&want.to_string()), "帧缺少字段 {}: {:?}", want, f);
        }
        // 每个挂起点一个专属 awaiter 槽 (槽名带函数名: 共用 promise 时不得撞车)
        assert!(f.contains(&"dcc_coro_aw_counter_0".to_string()), "{:?}", f);
        assert!(f.iter().any(|x| x.starts_with(SLOT_PREFIX)), "{:?}", f);
        // 共用 promise 才需要的归属字段在单一函数时不应出现
        assert!(!f.iter().any(|x| x == TAG), "独占 promise 不应生成归属字段: {:?}", f);
    }

    #[test]
    fn suspend_arms_run_the_awaiter_protocol() {
        let d = lowered(FX_LOOP);
        let t = text(&fn_stmts(&d, "dcc_coro_step_GenPromise"));
        // co_yield -> promise.yield_value(v) 写进帧槽, 再拷到局部 awaiter 走协议
        assert!(
            t.contains("Call(PtrMember(Ident(\"dcc_coro_fp\"), \"yield_value\")"),
            "未调 yield_value: {}",
            t
        );
        assert!(t.contains("\"await_ready\")"), "未调 await_ready");
        assert!(t.contains("\"await_suspend\")"), "未调 await_suspend");
        let resumed = t.matches("await_resume").count();
        assert!(resumed >= 3, "每个挂起点的恢复臂都应调 await_resume: {}", resumed);
        // 挂起前先把恢复位置写进帧 (跳回同一个 step 时才能接上)
        assert!(
            t.contains("PtrMember(Ident(\"dcc_coro_fp\"), \"dcc_coro_state\"), IntLit"),
            "挂起臂未落恢复状态号"
        );
    }

    #[test]
    fn final_suspend_marks_done_before_suspending() {
        let d = lowered(FX_SIMPLE);
        let t = text(&fn_stmts(&d, "dcc_coro_step_GenPromise"));
        assert!(t.contains("\"final_suspend\")"), "未调 final_suspend");
        assert!(
            t.contains("Call(PtrMember(Ident(\"dcc_coro_fp\"), \"return_void\")"),
            "co_return 未调 return_void"
        );
        assert!(
            t.contains("PtrMember(Ident(\"dcc_coro_fp\"), \"dcc_coro_done\"), IntLit(1))"),
            "final_suspend 臂应把 done 置 1"
        );
        // done() / destroy() 入口都生成了
        assert!(has_decl(&d, "dcc_coro_done_GenPromise"));
        assert!(has_decl(&d, "dcc_coro_destroy_GenPromise"));
    }

    #[test]
    fn await_ready_true_path_does_not_call_suspend_but_keeps_dispatching() {
        // awaiter 立即就绪: 跳过 await_suspend, 但当次仍需 await_resume 并继续往下派发给下一臂
        let pre = r#"
struct Ready {
    bool await_ready() noexcept { return true; }
    void await_suspend(std::coroutine_handle<RP> h) noexcept { (void)h; }
    void await_resume() noexcept {}
};
struct RP {
    int v;
    Generator get_return_object() { Generator g; g.p = this; v = 0; return g; }
    Ready initial_suspend() { Ready a; return a; }
    Ready final_suspend() noexcept { Ready a; return a; }
    Ready yield_value(int x) { v = x; Ready a; return a; }
    void return_void() {}
};
Generator rp() { co_yield 7; co_return; }
"#;
        let d = lowered_with("RP", pre);
        let name = format!("{}{}", STEP_PREFIX, "RP");
        let t = text(&fn_stmts(&d, &name));
        // awaiter 不存在副本: 协议钩子全部打在帧槽 glvalue `fp->dcc_coro_aw_rp_0` 上
        // (旧写法先 `Ready d0 = fp->slot` 拷进局部量, `await_suspend` 里的写会随副本消失)
        assert!(
            t.contains(
                "Member(PtrMember(Ident(\"dcc_coro_fp\"), \"dcc_coro_aw_rp_0\"), \"await_ready\")"
            ),
            "await_ready 未打在帧槽上: {}",
            t
        );
        assert!(
            !t.contains("LocalVar { ty: Class(\"Ready\")"),
            "awaiter 不应再拷进 step 函数局部量: {}",
            t
        );
        // await_ready 为真的臂仍 emit 了 await_suspend 分支 (运行时走 else), 且 else 置 again
        assert!(t.contains("\"await_suspend\")"), "就绪判定也应 emit await_suspend 分支");
        assert!(
            t.contains("BinOp(Assign, Ident(\"dcc_coro_again\"), IntLit(1))"),
            "await_ready 为真时应置 again 继续派发: {}",
            t
        );
        assert!(t.contains("\"dcc_coro_done\""), "应发射终态 done 标记");
    }

    // ---- promise / awaiter 协议识别 -------------------------------------

    #[test]
    fn handle_protocol_calls_are_rewritten_to_frame_entries() {
        let d = lowered(FX_SIMPLE);
        let t = text(&fn_stmts(&d, "coro_next"));
        assert!(!t.contains("coroutine_handle"), "handle 类型未去糖: {}", t);
        assert!(t.contains("dcc_coro_done_GenPromise"), "h.done() 未重写");
        assert!(t.contains("dcc_coro_step_GenPromise"), "h.resume() 未重写");
        // from_promise(*p) 退化为直接赋值 p: handle 局部量就是 promise 指针
        assert!(
            t.contains("LocalVar { ty: Pointer(Class(\"GenPromise\")), name: \"h\", init: Some(Ident(\"p\"))"),
            "from_promise 未退化: {}",
            t
        );
        // destroy() 落在另一个包装函数里
        let u = text(&fn_stmts(&d, "coro_release"));
        assert!(u.contains("dcc_coro_destroy_GenPromise"), "h.destroy() 未重写: {}", u);
    }

    #[test]
    fn co_yield_does_not_go_through_await_transform() {
        // 标准语义: co_yield e 等价 co_await promise.yield_value(e), 不得再对
        // yield_value 的结果套一层 await_transform (否则 awaiter 会被重建, 挂起位置错)
        let pre = r#"
struct SuspYP {
    bool await_ready() noexcept { return false; }
    void await_suspend(std::coroutine_handle<YP> h) noexcept { (void)h; }
    void await_resume() noexcept {}
};
struct Step2 {
    bool await_ready() noexcept { return false; }
    void await_suspend(std::coroutine_handle<YP> h) noexcept { (void)h; }
    void await_resume() noexcept {}
};
struct YP {
    int v;
    Generator get_return_object() { Generator g; g.p = this; v = 0; return g; }
    SuspYP initial_suspend() { SuspYP a; return a; }
    SuspYP final_suspend() noexcept { SuspYP a; return a; }
    SuspYP yield_value(int x) { v = x; SuspYP a; return a; }
    Step2 await_transform(int t) { Step2 s; v = v + t; return s; }
    void return_void() {}
};
Generator yp() { co_yield 1; co_return; }
"#;
        let d = lowered_with("YP", pre);
        let t = text(&fn_stmts(&d, "dcc_coro_step_YP"));
        assert!(t.contains("yield_value"), "co_yield 未走 yield_value");
        assert!(!t.contains("await_transform"), "co_yield 错误地过了 await_transform: {}", t);
    }

    #[test]
    fn co_await_goes_through_await_transform() {
        // 与上一个测试成对: co_await e 必须走 promise.await_transform(e),
        // 其返回值才是 awaiter (夹具 3 靠它把多轮 co_await 的指纹压进 promise).
        let pre = r#"
struct StepA {
    bool await_ready() noexcept { return false; }
    void await_suspend(std::coroutine_handle<AT> h) noexcept { (void)h; }
    void await_resume() noexcept {}
};
struct AT {
    int v;
    Generator get_return_object() { Generator g; g.p = this; v = 0; return g; }
    StepA initial_suspend() { StepA a; return a; }
    StepA final_suspend() noexcept { StepA a; return a; }
    StepA await_transform(int t) { v = v + t; StepA a; return a; }
    void return_void() {}
};
Generator at() { co_await 1; co_await 2; co_return; }
"#;
        let d = lowered_with("AT", pre);
        let t = text(&fn_stmts(&d, "dcc_coro_step_AT"));
        assert!(
            t.contains("Call(PtrMember(Ident(\"dcc_coro_fp\"), \"await_transform\")"),
            "co_await 未走 await_transform: {}",
            t
        );
        // await_transform 的结果类型成为帧槽字段类型 (awaiter 对象只住在帧里, 无局部副本)
        assert!(
            text(&d).contains("Class(\"StepA\")"),
            "await_transform 的结果类型未成为 awaiter"
        );
        // awaiter 只求值一次落帧槽, 之后 ready/suspend/resume 都打在同一个槽上
        assert!(
            t.contains("BinOp(Assign, PtrMember(Ident(\"dcc_coro_fp\"), \"dcc_coro_aw_at_2\"), Call(PtrMember(Ident(\"dcc_coro_fp\"), \"await_transform\")"),
            "co_await 的 awaiter 未落帧槽: {}",
            t
        );
        assert!(
            t.contains("Member(PtrMember(Ident(\"dcc_coro_fp\"), \"dcc_coro_aw_at_2\"), \"await_suspend\")"),
            "await_suspend 应打在帧槽 glvalue 上 (不能有中间副本): {}",
            t
        );
        // initial / final / 两次 co_await = 4 个专属帧槽
        let slots = t.matches("dcc_coro_aw_").count();
        assert!(slots >= 8, "4 个挂起点应各有帧槽字段读写: {}", slots);
    }

    #[test]
    fn lowering_is_deterministic_across_runs() {
        // 注入到 promise 类的帧字段 / awaiter pad 字段一旦来自 HashSet 迭代序,
        // 同一份源码每次进程会得到不同的成员顺序 → 不同的目标码 (实测踩到过).
        let want = text(&lowered(FX_LOOP));
        for _ in 0..16 {
            assert_eq!(want, text(&lowered(FX_LOOP)), "降级结果不具确定性");
        }
    }

    #[test]
    fn zero_member_awaiter_gets_a_pad_slot() {
        let d = lowered(FX_SIMPLE);
        let f = class_fields(&d, "SuspEnd");
        assert_eq!(f, vec![AWAITER_PAD.to_string()], "SuspEnd 应被注入占位字段: {:?}", f);
    }

    #[test]
    fn typedef_aliased_qualified_awaiter_resolves() {
        // 降级 pass 沿 typedef 别名链 (`typedef std::suspend_always Susp;` 后返回
        // `Susp`) 找到真实 awaiter 类 —— 早期解析器不接受类内限定名返回类型
        // (`std::suspend_always m() {}` 会退化为 Opaque), 所以桩头一度要求这种写法.
        // 该解析限制已解除 (见 `qualified_awaiter_return_type_needs_no_typedef`),
        // 但 typedef 形式仍是合法 C++, 用户代码也在用, 本测试固定这条解析链.
        let src = r#"
namespace std {
struct suspend_always {
    bool await_ready() noexcept { return false; }
    void await_suspend(void* h) noexcept { (void)h; }
    void await_resume() noexcept {}
};
}
typedef std::suspend_always Susp;
struct AP;
struct Generator {
    typedef AP promise_type;
    AP* p;
};
struct AP {
    int v;
    Generator get_return_object() { Generator g; g.p = this; v = 0; return g; }
    Susp initial_suspend() { Susp a; return a; }
    Susp final_suspend() noexcept { Susp a; return a; }
    Susp yield_value(int x) { v = x; Susp a; return a; }
    void return_void() {}
};
Generator ap() { co_yield 1; co_return; }
"#;
        let d = lower_coroutines(&parse(src)).expect("lower typedef awaiter");
        assert!(has_decl(&d, "dcc_coro_step_AP"));
        // void 形参的 await_suspend 应传 (void*)fp
        let t = text(&fn_stmts(&d, "dcc_coro_step_AP"));
        assert!(t.contains("await_suspend"), "{}", t);
    }

    #[test]
    fn qualified_awaiter_return_type_needs_no_typedef() {
        // 标准形式: promise 成员函数直接用**限定名**返回 awaiter
        // (`std::suspend_always initial_suspend()`), 不借道 typedef 别名.
        // 解析器必须把它当成真的成员函数声明 (而不是 Opaque 降级), 否则
        // 本 pass 看不到 yield_value / initial_suspend, 协程协议就不成立.
        // 与 tests/fixtures/cpp/coroutine_std_qualified_awaiter_return.cpp 同源.
        let src = r#"
namespace std {
struct suspend_always {
    bool await_ready() noexcept { return false; }
    void await_suspend(void* h) noexcept { (void)h; }
    void await_resume() noexcept {}
};
}
struct AP;
struct Generator {
    typedef AP promise_type;
    AP* p;
};
struct AP {
    int v;
    Generator get_return_object() { Generator g; g.p = this; v = 0; return g; }
    std::suspend_always initial_suspend() { return {}; }
    std::suspend_always final_suspend() noexcept { return {}; }
    std::suspend_always yield_value(int x) { v = x; return {}; }
    void return_void() {}
};
Generator ap() { co_yield 1; co_return; }
"#;
        let decls = parse(src);
        // 前提: 限定名签名没有被容错路径吞成 Opaque 声明.
        let ap = decls
            .iter()
            .find_map(|d| match d {
                CppDecl::Class(cd) if cd.name == "AP" => Some(cd),
                _ => None,
            })
            .unwrap_or_else(|| panic!("promise 类 AP 未以类声明形态出现 (限定名返回类型被降级?)"));
        let got: Vec<String> = ap
            .members
            .iter()
            .filter_map(|m| match m {
                CppDecl::MemberFn { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        for want in [
            "initial_suspend",
            "final_suspend",
            "yield_value",
            "return_void",
        ] {
            assert!(got.iter().any(|m| m == want), "AP 缺少 {}: {:?}", want, got);
        }
        // 结论: 降级 pass 能看到限定名 awaiter, 产出真正的状态机.
        let d = lower_coroutines(&decls).expect("lower 限定名 awaiter 协程");
        assert!(has_decl(&d, "dcc_coro_step_AP"), "{}", text(&d));
        let t = text(&fn_stmts(&d, "dcc_coro_step_AP"));
        assert!(t.contains("await_suspend"), "{}", t);
    }

    #[test]
    fn pass_is_inert_without_coroutines() {
        // driver 先用 tu_has_coroutine 门控: 不含协程的 TU 不会进入本 pass
        assert!(!tu_has_coroutine(&parse(PRE)));
        assert!(tu_has_coroutine(&parse(&format!("{}{}", PRE, FX_SIMPLE))));
        // 直接调用时显式报错, 不静默返回原 decls (避免“降级失败当成功”)
        let e = lower_coroutines(&parse(PRE)).unwrap_err();
        assert!(e.contains("no top-level coroutine function"), "{}", e);
    }

    // ---- 诊断 (不静默误编译) -----------------------------------------

    #[test]
    fn missing_initial_suspend_is_reported() {
        let src = r#"
struct NP {
    int v;
    Generator get_return_object() { Generator g; g.p = this; v = 0; return g; }
    SuspEnd final_suspend() noexcept { SuspEnd a; return a; }
    SuspEnd yield_value(int x) { v = x; SuspEnd a; return a; }
    void return_void() {}
};
Generator np() { co_yield 1; co_return; }
"#;
        let e = err_with("NP", src);
        assert!(e.contains("lacks initial_suspend"), "{}", e);
    }

    #[test]
    fn awaiter_lacking_protocol_is_reported() {
        let src = r#"
struct Bad {
    bool await_ready() noexcept { return false; }
    void await_suspend(std::coroutine_handle<BP> h) noexcept { (void)h; }
};
struct BP {
    int v;
    Generator get_return_object() { Generator g; g.p = this; v = 0; return g; }
    Bad initial_suspend() { Bad a; return a; }
    SuspEnd final_suspend() noexcept { SuspEnd a; return a; }
    SuspEnd yield_value(int x) { v = x; SuspEnd a; return a; }
    void return_void() {}
};
Generator bp() { co_yield 1; co_return; }
"#;
        let e = err_with("BP", src);
        assert!(e.contains("lacks an in-class definition of await_resume"), "{}", e);
    }

    #[test]
    fn fat_awaiter_slot_is_accepted() {
        // 旧的 "awaiter 槽必须 ≤ 8 字节" 限制来自 x86_64 后端 "多 quad 聚合值加载
        // 漏第二字" 的缺陷, 不是 C++ 语义要求. 后端现在按 `AggDst` 分块整块复制,
        // 所以 24 字节 awaiter 必须能降级 (运行期与 g++ 逐字节对照见集成测试
        // cpp_coroutine_fat_awaiter_slot_run).
        let src = r#"
struct Fat3 {
    long a;
    long b;
    long* c;
    bool await_ready() noexcept { return false; }
    void await_suspend(std::coroutine_handle<FP3> h) noexcept { (void)h; }
    long await_resume() noexcept { return a + b + *c; }
};
struct FP3 {
    int v;
    Generator get_return_object() { Generator g; g.p = this; v = 0; return g; }
    Fat3 initial_suspend() { Fat3 a; a.c = &a.a; return a; }
    SuspEnd final_suspend() noexcept { SuspEnd a; return a; }
    SuspEnd yield_value(int x) { v = x; SuspEnd a; return a; }
    void return_void() {}
};
Generator fp3() { co_yield 1; co_return; }
"#;
        let d = lowered_with("FP3", src);
        let fields = class_fields(&d, "FP3");
        assert!(
            fields.iter().any(|x| x.starts_with("dcc_coro_aw_fp3_")),
            "24 字节 awaiter 未落帧: {:?}",
            fields
        );
    }

    #[test]
    fn non_trivially_copyable_awaiter_is_reported() {
        // 帧槽读写是裸整块 `mov`, 不会调用用户拷贝赋值/维护 vptr —— 非平凡可复制
        // 的 awaiter 必须显式拒绝, 不能静默按位拄.
        let src = r#"
struct VFat {
    long a;
    long b;
    virtual bool await_ready() noexcept { return false; }
    void await_suspend(std::coroutine_handle<VP2> h) noexcept { (void)h; }
    void await_resume() noexcept {}
};
struct VP2 {
    int v;
    Generator get_return_object() { Generator g; g.p = this; v = 0; return g; }
    VFat initial_suspend() { VFat a; return a; }
    SuspEnd final_suspend() noexcept { SuspEnd a; return a; }
    SuspEnd yield_value(int x) { v = x; SuspEnd a; return a; }
    void return_void() {}
};
Generator vp2() { co_yield 1; co_return; }
"#;
        let e = err_with("VP2", src);
        assert!(e.contains("trivially copyable"), "{}", e);
    }

    #[test]
    fn nonvoid_awaiter_member_is_reported() {
        let src = r#"
struct NV {
    bool await_ready() noexcept { return false; }
    int await_suspend(std::coroutine_handle<NVP> h) noexcept { (void)h; return 0; }
    void await_resume() noexcept {}
};
struct NVP {
    int v;
    Generator get_return_object() { Generator g; g.p = this; v = 0; return g; }
    NV initial_suspend() { NV a; return a; }
    SuspEnd final_suspend() noexcept { SuspEnd a; return a; }
    SuspEnd yield_value(int x) { v = x; SuspEnd a; return a; }
    void return_void() {}
};
Generator nvp() { co_yield 1; co_return; }
"#;
        let e = err_with("NVP", src);
        assert!(e.contains("non-void") && e.contains("await_suspend"), "{}", e);
    }

    #[test]
    fn type_erased_handle_is_reported() {
        let src = r#"
void erase(std::coroutine_handle<void> h) { h.resume(); }
"#;
        let e = lower_coroutines(&parse(&format!("{}{}{}", PRE, FX_SIMPLE, src))).unwrap_err();
        assert!(e.contains("coroutine_handle<void>"), "{}", e);
    }

    #[test]
    fn multiple_coroutines_can_share_one_promise() {
        // 帧 == promise 对象, 所以共用同一 promise 的多个协程函数必须靠帧内归属号
        // 区分: `dcc_coro_step_<P>` 成跳板, 每台状态机自己的 `dcc_coro_step_<P>_<f>`.
        let src = r#"
Generator g2() { co_yield 3; co_return; }
"#;
        let d = lowered_with_src(&format!("{}{}", FX_SIMPLE, src));
        // 1) 归属跳板 + 两台状态机都在
        assert!(has_decl(&d, "dcc_coro_step_GenPromise"));
        assert!(has_decl(&d, "dcc_coro_step_GenPromise_f"));
        assert!(has_decl(&d, "dcc_coro_step_GenPromise_g2"));
        // 2) done/destroy 只发一份 (重复定义会直接编译失败)
        assert_eq!(count_fn_defs(&d, "dcc_coro_done_GenPromise"), 1);
        assert_eq!(count_fn_defs(&d, "dcc_coro_destroy_GenPromise"), 1);
        // 3) 跳板按归属号分发
        let t = text(&fn_stmts(&d, "dcc_coro_step_GenPromise"));
        assert!(
            t.contains("PtrMember(Ident(\"dcc_coro_fp\"), \"dcc_coro_fn\")"),
            "跳板未读归属字段: {}",
            t
        );
        assert!(t.contains("dcc_coro_step_GenPromise_f"), "跳板未分发到 f");
        assert!(t.contains("dcc_coro_step_GenPromise_g2"), "跳板未分发到 g2");
        // 4) 每个 ramp 写自己的归属号, 且帧里有该字段
        let tf = text(&fn_stmts(&d, "f"));
        let tg = text(&fn_stmts(&d, "g2"));
        assert!(tf.contains("dcc_coro_fn\"), IntLit(0))"), "{}", tf);
        assert!(tg.contains("dcc_coro_fn\"), IntLit(1))"), "{}", tg);
        let fields = class_fields(&d, "GenPromise");
        assert!(fields.iter().any(|x| x == TAG), "{:?}", fields);
        // 共享字段只追加一次
        assert_eq!(
            fields.iter().filter(|x| *x == STATE).count(),
            1,
            "STATE 字段重复: {:?}",
            fields
        );
        // 两台机器的帧槽互不同名
        assert!(fields.iter().any(|x| x.starts_with("dcc_coro_aw_f_")), "{:?}", fields);
        assert!(fields.iter().any(|x| x.starts_with("dcc_coro_aw_g2_")), "{:?}", fields);
        // 提升的形参/局部量本来就按函数名区分
        assert_eq!(fields.iter().filter(|x| x.starts_with(STATE)).count(), 1);
    }

    #[test]
    fn shared_promise_machines_keep_independent_numbering() {
        // 共用 promise 的两台状态机各自编号 (一台 ≥ 11 臂走等位宽偏移, 一台小机器),
        // 它们读写同一个 `dcc_coro_state` —— 正确性完全由归属号跳板保证.
        let d =
            lowered_with_src(&format!("{}Generator g2() {{ co_yield 3; co_return; }}\n", FX_LOOP));
        let mut n_switch = 0;
        for step in [
            "dcc_coro_step_GenPromise_counter",
            "dcc_coro_step_GenPromise_g2",
        ] {
            let mut sw = Vec::new();
            switches(&fn_stmts(&d, step), &mut sw);
            assert_eq!(sw.len(), 1, "{} 应只有一个派发 switch", step);
            assert!(same_digit_width(&sw[0]), "{} 臂号 {:?} 存在前缀关系", step, sw[0]);
            n_switch += 1;
        }
        assert_eq!(n_switch, 2);
        // 两台机器都登记了自己的专属局部量帧字段
        let fields = class_fields(&d, "GenPromise");
        assert!(fields.iter().any(|x| x == "dcc_coro_l_counter_total"), "{:?}", fields);
        assert_eq!(
            fields.iter().filter(|x| x.as_str() == DONE).count(),
            1,
            "DONE 字段重复: {:?}",
            fields
        );
    }

    #[test]
    fn co_return_value_needs_return_value() {
        let src = r#"
Generator rv() { co_return 42; }
"#;
        let e = lower_coroutines(&parse(&format!("{}{}", PRE, src))).unwrap_err();
        assert!(e.contains("return_value"), "{}", e);
    }

    #[test]
    fn coroutine_in_class_member_is_reported() {
        let src = r#"
struct Holder {
    Generator hm() { co_yield 1; co_return; }
};
"#;
        let e = lower_coroutines(&parse(&format!("{}{}", PRE, src))).unwrap_err();
        // 成员协程需要 this 进帧, 目前显式拒绝而非静默错编译
        assert!(e.contains("member") || e.contains("this"), "{}", e);
    }
}
