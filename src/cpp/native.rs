//! C++ 原生后端桥接: `cpp::Sema → abi_layout → lower_ir → ast::Program`
//!
//! 目标: 让 `run_cpp_driver` 在 `--cxx-native=on|auto` 模式下,
//! 直接把 C++ 声明降到现有 dcc C 主流水线 (`ir::lower` → 优化 →
//! `codegen::emit`), 不再经过 legacy `Lower::lower_to_c` 临时 `.c` 中转.
//!
//! 桥接阶段 (Stage 3) 覆盖的子集:
//!   - free function
//!   - 普通 class (无继承 / 无虚函数) 作为 aggregate 进入 IR
//!   - 成员函数 / 构造函数 / 析构函数 (带显式 this 形参)
//!   - 引用类型 (T&) 在 ABI 层降级为指针
//!
//! 不支持的特性 (会返回 `Err` 让 auto-mode 回退 legacy):
//!   - 异常
//!   - namespace / using
//!   - Dependent / MemberPointer 类型

use std::collections::{HashMap, HashSet};

use crate::ast;
use crate::cpp::abi_layout::{AbiCalculator, ClassAbi, ItaniumAbiCalculator};
use crate::cpp::feature_matrix::{CppFeature, CppPath};
use crate::cpp::hir::HirTranslationUnit;
use crate::cpp::lower_ir::{symbol_link_name, IrLowerer, LowerIrError, LowerIrOutput};
use crate::cpp::mangle::MangleType;
use crate::cpp::parser::{
    ClassDecl, CppBody, CppDecl, CppExpr, CppStmt, DeclSpec, InitDeclarator, TemplateArgs,
};
use crate::cpp::sema::{Access, ClassId, CxxSymbol, CxxSymbolKind, CxxType, Sema, SymbolId};
use crate::cpp::template::{instantiated_class_name, TemplateRegistry};
use crate::diagnostics::Span;
use crate::ssa;

/// 模板实例化扫描的产出: `(改写后的原始声明, [(定义锚点, 实例化声明)])`.
type InstantiatedDecls = (Vec<CppDecl>, Vec<(usize, CppDecl)>);

/// 桥接入口错误
#[derive(Debug, Clone)]
pub enum NativeError {
    /// HIR/Sema 阶段不支持的 C++ 特性 (auto-mode 应 fallback legacy)
    Unsupported(String),
    /// 内部不一致 (owner class 缺失等)
    Internal(String),
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NativeError::Unsupported(s) => write!(f, "native backend unsupported: {}", s),
            NativeError::Internal(s) => write!(f, "native backend internal error: {}", s),
        }
    }
}

impl std::convert::From<LowerIrError> for NativeError {
    fn from(e: LowerIrError) -> Self {
        match e {
            LowerIrError::Unsupported(s) => NativeError::Unsupported(s),
            LowerIrError::Internal(s) => NativeError::Internal(s),
        }
    }
}

/// 桥接结果: 已生成的可走 C 主流水线的 ast::Program
pub struct NativeLowered {
    pub prog: ast::Program,
    pub vtables: Vec<(String, Vec<crate::cpp::lower_ir::VTableEntry>)>,
    pub rodata: Vec<String>,
}

// === Stage 8: 模板实例化 ===

/// 模板函数实例化后的 C 名字: `identity<int>` -> `__dcc_tplfn_identity_<cand>_int`
///
/// M51: 必须带重载候选下标 `cand`: 同名同实参的两个候选 (如带 `requires`
/// 的那个和不带的那个) 推导结果完全一样, 不带下标会让它们撞同一个符号,
/// 重载选择结果错 (g++ 交叉验证时发现).
fn tplfn_c_name(name: &str, cand: usize, args: &[MangleType]) -> String {
    use crate::cpp::template::type_key;
    let keys: Vec<String> = args.iter().map(type_key).collect();
    format!("__dcc_tplfn_{}_{}_{}", name, cand, keys.join("_"))
}

/// 从声明列表中收集所有模板声明, 构建 TemplateRegistry.
///
/// M51: 同时记录每个模板/概念的**定义锚点** = 其前面存活的 (不会被
/// `filter_template_decls` 剔除的) 顶层声明个数. 实例化出来的类/函数必须
/// 插回定义点之后: C++ 要求类定义先于使用, 而实例化类体里又会出现源码中
/// 在模板**之前**声明的类型 (如 `std::vector` 头里 `extern "C"` 块内的
/// `struct dccpp_vector;`). 把所有实例无条件排在最前会让他们找不到.
fn collect_templates(decls: &[CppDecl]) -> TemplateRegistry {
    let mut reg = TemplateRegistry::default();
    let mut live = 0usize;
    for d in decls {
        collect_templates_from_decl(d, live, &mut reg);
        if !is_template_like_decl(d) {
            live += 1;
        }
    }
    reg
}

/// 会被 `filter_template_decls` 从输出声明序列中剔除的模板声明
fn is_template_like_decl(d: &CppDecl) -> bool {
    matches!(
        d,
        CppDecl::Template { .. } | CppDecl::PartialSpec { .. } | CppDecl::Concept { .. }
    )
}

/// 取模板实体名 (类模板 / 函数模板)
fn template_entity_name(d: &CppDecl) -> Option<String> {
    match d {
        CppDecl::Class(c) => Some(c.name.clone()),
        CppDecl::FreeFn { name, .. } => Some(name.clone()),
        _ => None,
    }
}

/// 递归从单个声明中收集模板
fn collect_templates_from_decl(d: &CppDecl, anchor: usize, reg: &mut TemplateRegistry) {
    match d {
        CppDecl::Template {
            params,
            decl,
            requires,
            ..
        } => {
            // M51: `template <typename T> concept Integral = ...;` 的实体是
            // Concept: 它不是可实例化模板 (`decl_from` 不认它), 但约束体必须
            // 进入概念表, 否则使用点的 `requires Integral<T>` 永远查不到定义.
            if let CppDecl::Concept { name, body_text } = &**decl {
                let names: Vec<String> = params.params.iter().map(|p| p.name.clone()).collect();
                reg.register_concept(name, &names, body_text);
                reg.set_anchor(name, anchor);
                return;
            }
            // M53: 别名模板 (`template <...> using X = T;`, [temp.alias]) 既不是
            // 类模板也不是函数模板: 使用点已在 parser 类型解析处展开成目标类型,
            // 这里若再注册, 就得到一个永远实例化不出类体的伪类模板
            // (`__dcc_template_enable_if_t_...`).
            if let CppDecl::Typedef { .. } = &**decl {
                return;
            }
            reg.register(params, requires, decl);
            if let Some(n) = template_entity_name(decl) {
                reg.set_anchor(&n, anchor);
            }
            // 递归收集嵌套模板
            collect_templates_from_decl(decl, anchor, reg);
        },
        CppDecl::Concept { name, body_text } => {
            // 无模板头的裸概念 (形式上合法, 无自变量可绑定)
            reg.register_concept(name, &[], body_text);
            reg.set_anchor(name, anchor);
        },
        CppDecl::PartialSpec {
            params,
            pattern_text,
            decl,
        } => {
            if let CppDecl::Class(c) = &**decl {
                let names: Vec<String> = params.params.iter().map(|p| p.name.clone()).collect();
                reg.register_partial(c.name.clone(), &names, pattern_text.clone(), decl);
                reg.set_anchor(&c.name, anchor);
            }
        },
        // G13: Collect full specializations
        CppDecl::FullSpec {
            primary_name,
            specific_args,
            body,
        } => {
            reg.register_full(primary_name.clone(), specific_args.clone(), body);
            reg.set_anchor(primary_name, anchor);
        },
        CppDecl::Namespace { body, .. } => {
            for inner in body {
                collect_templates_from_decl(inner, anchor, reg);
            }
        },
        CppDecl::Class(c) => {
            for m in &c.members {
                collect_templates_from_decl(m, anchor, reg);
            }
        },
        _ => {},
    }
}

/// 重命名模板函数实例化后的函数名
fn rename_tpl_fn_inst(decl: &CppDecl, name: &str, cand: usize, args: &[MangleType]) -> CppDecl {
    let new_name = tplfn_c_name(name, cand, args);
    match decl {
        CppDecl::FreeFn {
            ret,
            name: _,
            params,
            body,
            is_noexcept,
            trailing_ret_type,
        } => CppDecl::FreeFn {
            ret: ret.clone(),
            name: new_name,
            params: params.clone(),
            body: body.clone(),
            is_noexcept: *is_noexcept,
            trailing_ret_type: trailing_ret_type.clone(),
        },
        other => other.clone(),
    }
}

/// 扫描声明中的模板类型引用 + 调用点模板实参推导, 产出实例化声明.
///
/// M51 重写要点:
/// 1. **工作表**: 旧实现每轮重新全量扇描 `all_decls` (含已实例化结果),
///    n 层嵌套模板就是 O(n^3~n^4) (解析超深嵌套模板时表现为挂起).
///    现在每个声明恰好出队一次, 类型树也只走一遍 → 线性.
/// 2. **隐式实参推导**: `identity(42)` 这类没有显式模板实参的调用也要触发
///    函数模板实例化 (旧实现只认 `foo<int>(...)`, 于是 concept / SFINAE
///    夹具全栽在 "undeclared identifier").
///
/// 返回 `(改写后的原始声明, [(定义锚点, 实例化声明)])`: 调用点被改写成单态化
/// 后的 C 链接名, 因此**原始声明列表也必须递回** (不只是实例化结果).
fn instantiate_templates_from_decls(
    reg: &mut TemplateRegistry,
    decls: &[CppDecl],
) -> Result<InstantiatedDecls, NativeError> {
    let mut r = TplResolver {
        reg,
        env: TplEnv::from_decls(decls),
        seen_class: HashSet::new(),
        seen_fn: HashSet::new(),
        out: Vec::new(),
        deps: Vec::new(),
        current_out: None,
        pending: Vec::new(),
        rewritten: Vec::with_capacity(decls.len()),
        in_template_body: false,
        no_match: None,
        queue: decls
            .iter()
            .cloned()
            .map(|decl| QueueItem {
                in_template_body: matches!(decl, CppDecl::Template { .. }),
                decl,
                out_index: None,
            })
            .collect(),
    };
    while let Some(mut item) = r.queue.pop_front() {
        r.in_template_body = item.in_template_body;
        // M53: 处理某实例期间新发现的实例都**被它依赖** (调用方 → 被调方),
        // 记一条边, 供 emit_order 做后序拓扑 (见该函数注释).
        r.current_out = item.out_index;
        r.pending.clear();
        // 1) 类模板引用 (内层先于外层, 见 collect_template_refs_from_type 后序);
        // 已入过表的 key 整棵子树跳过, 避免深嵌套类型被反复全量展开
        let mut refs: Vec<(String, Vec<MangleType>)> = Vec::new();
        collect_template_refs_from_decl(&item.decl, &r.seen_class, &mut refs);
        for (name, args) in refs {
            r.instantiate_class_ref(&name, &args);
        }
        // 2) 函数模板调用 (显式实参 + 隐式推导), 就地改写被调名
        r.decl(&mut item.decl);
        if let Some(i) = item.out_index {
            r.deps[i].append(&mut r.pending);
            r.out[i].1 = item.decl;
        } else {
            r.rewritten.push(item.decl);
        }
    }
    r.current_out = None;
    // M51: 全部候选被剔除的调用点 = 硬错误 (不能像以前那样静默漏过).
    if let Some(msg) = r.no_match.take() {
        return Err(NativeError::Unsupported(msg));
    }
    // G12/M52: 参数包摊平失败 = 硬错误. 它不能伪装成"候选集为空 (SFINAE)",
    // 也不能默默丢掉实例 (那会在下游变成莫名的未定义符号).
    if let Some(msg) = r.reg.pack_expand_error.take() {
        return Err(NativeError::Unsupported(format!("parameter pack expansion failed: {msg}")));
    }
    let out = r.finalize_out();
    Ok((r.rewritten, out))
}

/// M53: 把 `out` 按依赖边 (`deps[cur] = cur 依赖的实例下标`) 排成
/// **被依赖者在前** 的发射序列, 并把每个实例的锚点收紧为
/// "自身与全部传递依赖里最小的锚点", 使下游那条按锚点的稳定排序
/// 不会把调用方重新排到被调方之前.
fn emit_order(out_len: usize, deps: &[Vec<usize>]) -> Vec<usize> {
    // 0 = white (未访问), 1 = gray (在栈上), 2 = black (已发射)
    let mut state = vec![0u8; out_len];
    let mut order = Vec::with_capacity(out_len);
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for root in 0..out_len {
        if state[root] != 0 {
            continue;
        }
        state[root] = 1;
        stack.push((root, 0));
        while let Some((node, child_i)) = stack.pop() {
            if let Some(&child) = deps.get(node).and_then(|d| d.get(child_i)) {
                stack.push((node, child_i + 1));
                // gray 后端 = 环 (互相递归的模板), 忽略该回边保证终止
                if state[child] == 0 {
                    state[child] = 1;
                    stack.push((child, 0));
                }
            } else if state[node] != 2 {
                state[node] = 2;
                order.push(node);
            }
        }
    }
    order
}

/// 工作表一项: 声明 + (若是实例化结果) 它在 `out` 里的下标
struct QueueItem {
    decl: CppDecl,
    out_index: Option<usize>,
    /// 本项是否是一个原始模板声明 (体里名字仍是形参)
    in_template_body: bool,
}

/// 调用点模板实参推导所需的轻量类型环境.
///
/// sema 是类型信息的唯一真源, 但函数模板的隐式实例化必须发生在 sema **之前**
/// (native 后端是 AST 级单态化), 所以这里只重建"从实参类型反推模板形参"
/// 必需的那部分名字: 全局变量 / 函数返回类型 / 作作用域内局部变量与形参 /
/// 当前类数据成员. 推不出类型就不做隐式实例化 (退回给 sema 按普通名字查找).
struct TplEnv {
    /// 顶层 (含 namespace) 变量名 -> 类型
    globals: HashMap<String, MangleType>,
    /// 已有非模板函数名 -> 形参个数: 命中则不做模板推导,
    /// 让 sema 的常规重载决议接手 (非模板函数优先于模板)
    plain_fns: HashSet<String>,
    /// 函数名 -> 返回类型 (供嵌套调用 `identity(f(1))` 的实参类型推导)
    fn_ret: HashMap<String, MangleType>,
    /// 类名 -> 数据成员名 -> 类型
    class_fields: HashMap<String, HashMap<String, MangleType>>,
    /// M53: 类名 -> 成员函数名 -> 返回类型
    ///
    /// 与 `fn_ret` 同源但按类分名: 实例化类里 `Holder<int>::get` 的返回类型
    /// 是 `int`. 有了它, 嵌套函数模板调用才能把 `id(h.get())` 这种
    /// **“实参本身是成员调用”** 的形态推出实参类型 (之前返回 None 而
    /// 静默不实例化, 最后变成 sema 的 `undeclared identifier: id`).
    class_mfns: HashMap<String, HashMap<String, MangleType>>,
    /// 块作作用域栈
    scopes: Vec<HashMap<String, MangleType>>,
    /// 当前成员函数所属类 (None = 自由函数 / 全局上下文)
    class: Option<String>,
}

/// M53: 把一个类声明的字段 / 成员函数返回类型索引进环境.
///
/// `seed` (原始声明) 与 `instantiate_class_ref` (实例化产物) 以前各自写
/// 一遂字段循环, 现在收敛到这里, 避免两边行为漂移.
fn index_class(env: &mut TplEnv, c: &crate::cpp::parser::ClassDecl) {
    let mut fields = HashMap::new();
    let mut mfns = HashMap::new();
    for m in &c.members {
        match m {
            CppDecl::MemberVar { name, ty, .. } => {
                fields.insert(name.clone(), ty.clone());
            },
            CppDecl::MemberFn { name, ret, .. } => {
                mfns.insert(name.clone(), ret.clone());
            },
            _ => {},
        }
    }
    env.class_fields.insert(c.name.clone(), fields);
    env.class_mfns.insert(c.name.clone(), mfns);
}

/// M53: 从表达式类型取出“所属类名” (实例化后的 C 链接名).
fn class_owner_of(t: &MangleType) -> Option<String> {
    match t {
        MangleType::Class(n) => Some(n.clone()),
        MangleType::Template { name, args } => Some(instantiated_class_name(name, args)),
        _ => None,
    }
}

impl TplEnv {
    fn from_decls(decls: &[CppDecl]) -> Self {
        let mut env = TplEnv {
            globals: HashMap::new(),
            plain_fns: HashSet::new(),
            fn_ret: HashMap::new(),
            class_fields: HashMap::new(),
            class_mfns: HashMap::new(),
            scopes: Vec::new(),
            class: None,
        };
        for d in decls {
            env.seed(d);
        }
        env
    }

    /// 收录一个声明里的名字/类型 (不进入模板体: 模板体里的类型还是形参)
    fn seed(&mut self, d: &CppDecl) {
        match d {
            CppDecl::GlobalVar { name, ty, .. } => {
                self.globals.insert(name.clone(), ty.clone());
            },
            CppDecl::FreeFn {
                name, ret, params, ..
            } => {
                self.plain_fns.insert(name.clone());
                self.fn_ret.insert(name.clone(), ret.clone());
                let _ = params;
            },
            CppDecl::Class(c) => {
                index_class(self, c);
            },
            CppDecl::Namespace { body, .. } => {
                for inner in body {
                    self.seed(inner);
                }
            },
            _ => {},
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn declare(&mut self, name: String, ty: MangleType) {
        if let Some(top) = self.scopes.last_mut() {
            top.insert(name, ty);
        }
    }

    fn lookup(&self, name: &str) -> Option<MangleType> {
        for sc in self.scopes.iter().rev() {
            if let Some(t) = sc.get(name) {
                return Some(t.clone());
            }
        }
        if let Some(cls) = &self.class {
            if let Some(t) = self.class_fields.get(cls).and_then(|m| m.get(name)) {
                return Some(t.clone());
            }
        }
        self.globals.get(name).cloned()
    }
}

/// 模板实例化工作表状态 (扫描 + 改写共用一份作作用域环境)
struct TplResolver<'a> {
    reg: &'a mut TemplateRegistry,
    env: TplEnv,
    /// 已入过工作表的**类**实例化 key (保证终止 + 子树短路)
    seen_class: HashSet<String>,
    /// 已入过工作表的**函数**实例化 key
    ///
    /// M51: 两类 key 分开存放: `key_for` 只看 "名字+实参", 同名同类参的
    /// 类模板与函数模板 (如 `struct hash<T>` 与 `hash<T>()`) 共用一个集合
    /// 会让后者被静吞掉 → 调用点找不到符号.
    seen_fn: HashSet<String>,
    /// 输出: (模板定义锚点, 实例化声明)
    out: Vec<(usize, CppDecl)>,
    /// M53: `deps[i]` = `out[i]` 这个实例**依赖**的实例下标 (它调用了谁 /
    /// 它的成员用了哪个类模板). 发射时必须被依赖者先于依赖者.
    deps: Vec<Vec<usize>>,
    /// M53: 当前正在出队处理的项在 `out` 里的下标 (None = 原始声明)
    current_out: Option<usize>,
    /// M53: 本轮 `current_out` 期间新发现的实例下标暂存
    pending: Vec<usize>,
    /// 改写后的原始顶层声明 (与入参顺序一致)
    rewritten: Vec<CppDecl>,
    /// 待处理声明 (原始 + 新实例化出来的)
    queue: std::collections::VecDeque<QueueItem>,
    /// 当前正在走的数据是否来自**未实例化的模板体** (形参还是 T)
    ///
    /// 模板体内对依赖名字的调用在形参未定之前必然推不出实参, 不能据此
    /// 报"无匹配函数" (那是实例化点才成立的结论).
    in_template_body: bool,
    /// 候选集被全部剔除的调用点 (首个) —— 等价于 g++ 的
    /// "no matching function for call to ...", 必须硬报错而不是静默漏过.
    no_match: Option<String>,
}

impl<'a> TplResolver<'a> {
    /// 把一个新的实例化声明收进 `out`, 并登记“当前正在处理的实例 依赖 它”这条边.
    ///
    /// M53: 出队一项 (某个已产出的实例) 时, 它体内又发现了新实例 —— 那都是它的
    /// 被调方 / 成员类型, 必须排在它前面, 否则下游 sema 会在线性扫到调用点时
    /// 报 `undeclared identifier: __dcc_tplfn_xxx`.
    fn push_out(&mut self, anchor: usize, decl: CppDecl) -> usize {
        let idx = self.out.len();
        if self.current_out.is_some_and(|cur| cur != idx) {
            self.pending.push(idx);
        }
        self.out.push((anchor, decl));
        self.deps.push(Vec::new());
        idx
    }

    /// 按依赖边重排 `out` (见 `emit_order`), 返回最终 (锚点, 声明) 序列.
    fn finalize_out(&mut self) -> Vec<(usize, CppDecl)> {
        let order = emit_order(self.out.len(), &self.deps);
        let mut eff: Vec<usize> = self.out.iter().map(|(a, _)| *a).collect();
        // order 是后序 (被依赖者在前), 正向扫一遍就能把“传递依赖里的最小锚点”
        // 上传到调用方, 使下游那条按锚点的稳定排序不会又把它们倒过来.
        for &n in &order {
            let mut min_child = usize::MAX;
            for &c in &self.deps[n] {
                if eff[c] < min_child {
                    min_child = eff[c];
                }
            }
            if min_child < eff[n] {
                eff[n] = min_child;
            }
        }
        let mut slots: Vec<Option<(usize, CppDecl)>> = std::mem::take(&mut self.out)
            .into_iter()
            .map(Some)
            .collect();
        let mut out = Vec::with_capacity(slots.len());
        for n in order {
            let (_, d) = slots[n].take().expect("emit_order 返回的下标互不重复");
            out.push((eff[n], d));
        }
        out
    }

    /// 实例化一个类模板引用; 已处理过 / 名字不是类模板时跳过,
    /// **已知类模板实例化失败** 报硬错误 (见函数尾的 M52 注释).
    fn instantiate_class_ref(&mut self, name: &str, args: &[MangleType]) {
        let key = TemplateRegistry::key_for(name, args);
        if !self.seen_class.insert(key) {
            return;
        }
        let anchor = self.reg.anchor_of(name);
        if let Some(inst) = self.reg.instantiate_class(name, args) {
            // 实例化产物里可能残留「形参作限定前缀」的依赖类型 (见
            // `bind_dependent_qualifier`), 按模板形参名→实参再绑定一次。
            let tpl_params = self
                .reg
                .find(name)
                .map(|t| t.param_names.clone())
                .unwrap_or_default();
            let inst = if tpl_params.is_empty() {
                inst
            } else {
                bind_dependent_names(&inst, &tpl_params, args)
            };
            // 把实例化类的字段类型登记进环境: 后续成员访问实参要推类型
            if let CppDecl::Class(c) = &inst {
                index_class(&mut self.env, c);
            }
            let out_idx = self.push_out(anchor, inst.clone());
            self.queue.push_back(QueueItem {
                decl: inst,
                out_index: Some(out_idx),
                in_template_body: false,
            });
            return;
        }
        // M52: 已知类模板的**实例化失败**过去被静默丢弃 (`instantiate_class` 返回
        // None 就什么也不做), 下游只能在 sema 里报一句语焉不详的
        // `unknown class type: __dcc_template_fm_int_int`, 或直接降级成 Opaque.
        // 现在就地报硬错误 (g++ 同位置: "template argument deduction/substitution
        // failed"). 名字在注册表里根本不存在时仍静默跳过 —— 那是模板模板形参
        // 之类的占位引用, 不是失败; 未实例化的模板体里形参仍是 `T`, 同理.
        if self.no_match.is_none() && !self.in_template_body && self.reg.find(name).is_some() {
            let keys: Vec<String> = args.iter().map(crate::cpp::template::type_key).collect();
            self.no_match = Some(format!(
                "template argument deduction/substitution failed for class template \
                 '{name}<{}>'",
                keys.join(", ")
            ));
        }
    }

    /// 确保 `name<args>` 的函数模板实例已产出 (去重 + 入工作表)
    ///
    /// M51: `idx` 是重载集里选定的**候选下标**. 它必须参与去重 key 和实例
    /// C 名: 同名同实参的不同候选 (约束版 vs 无约束版) 是两个不同的函数.
    fn ensure_fn_instance(&mut self, name: &str, idx: usize, args: &[MangleType]) {
        let key = TemplateRegistry::fn_inst_key(idx, name, args);
        if !self.seen_fn.insert(key) {
            return;
        }
        let anchor = self.reg.anchor_of(name);
        if let Some(inst) = self.reg.instantiate_fn_at(idx, name, args) {
            let renamed = rename_tpl_fn_inst(&inst, name, idx, args);
            if let CppDecl::FreeFn { ret, name: rn, .. } = &renamed {
                self.env.fn_ret.insert(rn.clone(), ret.clone());
            }
            let out_idx = self.push_out(anchor, renamed.clone());
            self.queue.push_back(QueueItem {
                decl: renamed,
                out_index: Some(out_idx),
                in_template_body: false,
            });
        }
    }

    /// 处理一个调用点: 把被调模板名改写为单态化后的 C 链接名
    fn fn_call_site(&mut self, f: &mut CppExpr, args: &[CppExpr]) {
        // (a) 显式模板实参 `foo<int>(x)`
        if let CppExpr::TemplateIdent { name, args: targs } = &*f {
            let (name, targs) = (name.clone(), targs.clone());
            // 候选不可行 (约束不满足等) -> 不改写, 并记录"无匹配函数"
            let resolved = match self.reg.normalize_args(&name, &targs) {
                Some(r) => r,
                None => {
                    self.note_no_match(&name, &targs);
                    return;
                },
            };
            let (idx, resolved) = resolved;
            self.ensure_fn_instance(&name, idx, &resolved);
            *f = CppExpr::Ident(tplfn_c_name(&name, idx, &resolved));
            return;
        }
        // (b) 隐式实参推导 `identity(42)` / `std::print(fmt, args...)`
        //
        // M52: **限定名被调** 之前整个漏掉 —— namespace 里的函数模板按短名
        // 登记在注册表, 而调用点是 `QualifiedAccess(["std"], "print")`;
        // 不改写成单态化后的 C 名, 就会把模板名留给 sema 的 QualifiedAccess
        // 查找 (那里只认 Function/Variable 符号, 模板名查不到), 报
        // "'print' not found in qualified name" —— 整条 `namespace std` 的
        // 变参接口 (print/println/format/format_to) 因此不可用.
        // 限定路径必须是 namespace: `Foo::bar()` 这种类静态成员不在此列
        // (类名会在 class_fields / 类模板表里出现, 直接排除).
        let name = match &*f {
            CppExpr::Ident(n) => n.clone(),
            CppExpr::QualifiedAccess(path, n) => {
                if path.is_empty()
                    || path.iter().any(|p| {
                        self.env.class_fields.contains_key(p) || self.reg.has_template_named(p)
                    })
                {
                    return;
                }
                n.clone()
            },
            _ => return,
        };
        if !self.reg.has_template_named(&name) || self.env.plain_fns.contains(&name) {
            // 非模板名 / 已有同名普通函数: 让 sema 的常规重载决议接手
            return;
        }
        let mut tys: Vec<MangleType> = Vec::with_capacity(args.len());
        for a in args {
            match infer_expr_ty(a, &self.env) {
                Some(t) => tys.push(t),
                // 推不出某个实参类型: 不做隐式实例化 (保守, 也不能据此报无匹配)
                None => {
                    if std::env::var_os("DCC_TPL_DEBUG").is_some() {
                        eprintln!("[tpl] {name}: 实参类型推导失败");
                    }
                    return;
                },
            }
        }
        let Some((idx, resolved)) = self.reg.deduce_for_call(&name, &tys) else {
            if std::env::var_os("DCC_TPL_DEBUG").is_some() {
                eprintln!(
                    "[tpl] {name}: 全部候选推导/约束失败 (args={:?})",
                    tys.iter()
                        .map(crate::cpp::template::type_key)
                        .collect::<Vec<_>>()
                );
            }
            self.note_no_match(&name, &tys);
            return;
        };
        self.ensure_fn_instance(&name, idx, &resolved);
        *f = CppExpr::Ident(tplfn_c_name(&name, idx, &resolved));
    }

    /// M51: 记录一个"全部候选被剔除"的调用点.
    ///
    /// C++ 的 SFINAE 只作用于**候选集构造**: 单个候选替换失败是静默的,
    /// 但所有候选都不可行就是普通的"无匹配函数"硬错误 (g++ 同义).
    /// 两个门控:
    ///   - 未实例化的模板体里形参还是 T, 推不出实参不意味着重载集为空;
    ///   - 类模板名在调用位置是临时量构造/函数式转换, 不是函数调用.
    fn note_no_match(&mut self, name: &str, args: &[MangleType]) {
        if self.in_template_body || self.no_match.is_some() {
            return;
        }
        if !self.reg.only_fn_templates_named(name) {
            return;
        }
        let keys: Vec<String> = args.iter().map(crate::cpp::template::type_key).collect();
        self.no_match = Some(format!(
            "no matching function for call to '{name}({})': all template candidates \
             were excluded by substitution failure or unsatisfied constraints",
            keys.join(", ")
        ));
    }

    fn decl(&mut self, d: &mut CppDecl) {
        match d {
            CppDecl::FreeFn { params, body, .. } => {
                let prev = self.env.class.take();
                self.env.push_scope();
                for (n, t) in params.iter() {
                    self.env.declare(n.clone(), t.clone());
                }
                self.body(body);
                self.env.pop_scope();
                self.env.class = prev;
            },
            CppDecl::MemberFn {
                class,
                params,
                body,
                ..
            } => {
                let prev = self.env.class.replace(class.clone());
                self.env.push_scope();
                for (n, t) in params.iter() {
                    self.env.declare(n.clone(), t.clone());
                }
                self.body(body);
                self.env.pop_scope();
                self.env.class = prev;
            },
            CppDecl::Ctor {
                class,
                params,
                inits,
                body,
                ..
            } => {
                let prev = self.env.class.replace(class.clone());
                self.env.push_scope();
                for (n, t) in params.iter() {
                    self.env.declare(n.clone(), t.clone());
                }
                for (_, e) in inits.iter_mut() {
                    self.expr(e);
                }
                self.body(body);
                self.env.pop_scope();
                self.env.class = prev;
            },
            CppDecl::Dtor { class, body, .. } => {
                let prev = self.env.class.replace(class.clone());
                self.body(body);
                self.env.class = prev;
            },
            CppDecl::Class(c) => {
                let prev = self.env.class.replace(c.name.clone());
                for m in c.members.iter_mut() {
                    self.decl(m);
                }
                self.env.class = prev;
            },
            CppDecl::Namespace { body, .. } => {
                for inner in body.iter_mut() {
                    self.decl(inner);
                }
            },
            CppDecl::GlobalVar { ty, name, init, .. } => {
                if let Some(e) = init {
                    self.expr(e);
                }
                let (n, t) = (name.clone(), ty.clone());
                self.env.globals.insert(n, t);
            },
            _ => {},
        }
    }

    fn body(&mut self, b: &mut CppBody) {
        self.env.push_scope();
        for s in b.stmts.iter_mut() {
            self.stmt(s);
        }
        self.env.pop_scope();
    }

    fn stmt(&mut self, s: &mut CppStmt) {
        match s {
            CppStmt::Expr(e)
            | CppStmt::CoAwait(e)
            | CppStmt::CoYield(e)
            | CppStmt::Throw(Some(e)) => self.expr(e),
            CppStmt::Return(Some(e)) | CppStmt::CoReturn(Some(e)) => self.expr(e),
            CppStmt::If { cond, then, els } | CppStmt::IfConstexpr { cond, then, els } => {
                self.expr(cond);
                self.body(then);
                if let Some(b) = els {
                    self.body(b);
                }
            },
            CppStmt::While { cond, body } => {
                self.expr(cond);
                self.body(body);
            },
            CppStmt::DoWhile { body, cond } => {
                self.body(body);
                self.expr(cond);
            },
            CppStmt::For {
                init,
                cond,
                step,
                body,
            } => {
                self.env.push_scope();
                if let Some(i) = init {
                    self.stmt(i);
                }
                if let Some(c) = cond {
                    self.expr(c);
                }
                if let Some(st) = step {
                    self.stmt(st);
                }
                self.body(body);
                self.env.pop_scope();
            },
            CppStmt::Block(b) => self.body(b),
            CppStmt::ThisMember(_, inner) => self.stmt(inner),
            CppStmt::Labeled { stmt, .. } => self.stmt(stmt),
            CppStmt::LocalVar { ty, name, init, .. }
            | CppStmt::StaticLocal { ty, name, init, .. } => {
                if let Some(e) = init {
                    self.expr(e);
                }
                let (n, t) = (name.clone(), ty.clone());
                self.env.declare(n, t);
            },
            CppStmt::LocalVarList { spec, declarators } => {
                // 一条声明里的每个 declarator 都必须各自登记名字与类型, 不能只认
                // 第一个 —— 否则 `int a = 0, b = 1;` 里的 `b` 在后续查找中失踪。
                // 类型是共享 decl-specifier 叠加**它自己的** `*`/`&`/`[N]` 层级。
                for d in declarators.iter_mut() {
                    if let Some(e) = &mut d.init {
                        self.expr(e);
                    }
                }
                let declared: Vec<(String, MangleType)> = declarators
                    .iter()
                    .map(|d| (d.name.clone(), d.full_ty(spec)))
                    .collect();
                for (n, t) in declared {
                    self.env.declare(n, t);
                }
            },
            CppStmt::Try { body, catches } => {
                self.body(body);
                for c in catches.iter_mut() {
                    if let (Some(t), Some(n)) = (&c.ty, &c.name) {
                        self.env.push_scope();
                        self.env.declare(n.clone(), t.clone());
                        self.body(&mut c.body);
                        self.env.pop_scope();
                    } else {
                        self.body(&mut c.body);
                    }
                }
            },
            CppStmt::Switch { expr, arms, .. } => {
                self.expr(expr);
                for (_, b) in arms.iter_mut() {
                    self.body(b);
                }
            },
            CppStmt::StructuredBinding {
                names, ty, init, ..
            } => {
                self.expr(init);
                let t = ty.clone();
                for n in names.iter() {
                    self.env.declare(n.clone(), t.clone());
                }
            },
            CppStmt::Decl(d) => self.decl(d),
            _ => {},
        }
    }

    fn expr(&mut self, e: &mut CppExpr) {
        match e {
            CppExpr::Call(f, args) => {
                for a in args.iter_mut() {
                    self.expr(a);
                }
                // 先尝试把被调名当模板调用解析/改写, 再递归 callee 子表达式
                // (处理 `f(g(1))` 与 `obj.method()` 内嵌调用)
                self.fn_call_site(f, args);
                self.expr(f);
            },
            CppExpr::BinOp(_, l, r) => {
                self.expr(l);
                self.expr(r);
            },
            CppExpr::UnaryOp(_, x) | CppExpr::Ref(x) | CppExpr::AddrOf(x) => self.expr(x),
            CppExpr::Member(o, _) | CppExpr::PtrMember(o, _) => self.expr(o),
            CppExpr::MemberStar(o, _) | CppExpr::PtrMemberStar(o, _) => self.expr(o),
            CppExpr::Cast(_, inner)
            | CppExpr::StaticCast { expr: inner, .. }
            | CppExpr::ConstCast { expr: inner, .. }
            | CppExpr::ReinterpretCast { expr: inner, .. }
            | CppExpr::DynamicCast { expr: inner, .. }
            | CppExpr::Delete(inner)
            | CppExpr::DeleteArr(inner)
            | CppExpr::Noexcept(inner)
            | CppExpr::Typeid(inner) => self.expr(inner),
            CppExpr::New(_, args) | CppExpr::InitList(args) | CppExpr::Comma(args) => {
                for a in args.iter_mut() {
                    self.expr(a);
                }
            },
            CppExpr::NewArr { count, .. } => self.expr(count),
            CppExpr::PlacementNew { ptr, args, .. } => {
                self.expr(ptr);
                for a in args.iter_mut() {
                    self.expr(a);
                }
            },
            CppExpr::TypedInitList { elems, .. } => {
                for a in elems.iter_mut() {
                    self.expr(a);
                }
            },
            CppExpr::MultiIndex(obj, idxs) => {
                self.expr(obj);
                for i in idxs.iter_mut() {
                    self.expr(i);
                }
            },
            CppExpr::Ternary {
                cond, then, els, ..
            } => {
                self.expr(cond);
                self.expr(then);
                self.expr(els);
            },
            CppExpr::OpCall { args, .. } => {
                for a in args.iter_mut() {
                    self.expr(a);
                }
            },
            CppExpr::Lambda(lam) => {
                let prev = self.env.class.take();
                self.env.push_scope();
                for (n, t) in lam.params.iter() {
                    self.env.declare(n.clone(), t.clone());
                }
                self.body(&mut lam.body);
                self.env.pop_scope();
                self.env.class = prev;
            },
            _ => {},
        }
    }
}

/// 推导表达式的类型 (仅供模板实参推导).
/// 覆盖常量/变量/取址/解引用/转型/嵌套调用等常见形态; 推不出返回 None.
fn infer_expr_ty(e: &CppExpr, env: &TplEnv) -> Option<MangleType> {
    match e {
        CppExpr::IntLit(v) => Some(if *v > i32::MAX as i64 || *v < i32::MIN as i64 {
            MangleType::Long
        } else {
            MangleType::Int
        }),
        CppExpr::FloatLit(t) => {
            let lt = t.to_ascii_lowercase();
            Some(if lt.ends_with('f') {
                MangleType::Float
            } else {
                MangleType::Double
            })
        },
        CppExpr::StrLit(_) | CppExpr::StrConcat(_) => {
            Some(MangleType::Pointer(Box::new(MangleType::Char)))
        },
        CppExpr::Ident(n) => env.lookup(n),
        CppExpr::This => env.class.clone().map(MangleType::Class),
        CppExpr::ThisMember(m) => env
            .class
            .as_ref()
            .and_then(|c| env.class_fields.get(c))
            .and_then(|m2| m2.get(m))
            .cloned(),
        CppExpr::Member(o, m) => {
            let bt = infer_expr_ty(o, env)?;
            field_of(&bt, m, env)
        },
        CppExpr::PtrMember(o, m) => {
            let bt = infer_expr_ty(o, env)?;
            match bt {
                MangleType::Pointer(inner) => field_of(&inner, m, env),
                _ => None,
            }
        },
        CppExpr::Ref(x) => infer_expr_ty(x, env).map(|t| MangleType::Reference(Box::new(t))),
        CppExpr::AddrOf(x) => infer_expr_ty(x, env).map(|t| MangleType::Pointer(Box::new(t))),
        CppExpr::Cast(t, _)
        | CppExpr::StaticCast { ty: t, .. }
        | CppExpr::ConstCast { ty: t, .. }
        | CppExpr::ReinterpretCast { ty: t, .. }
        | CppExpr::DynamicCast { ty: t, .. }
        | CppExpr::TypeidType(t) => Some(t.clone()),
        CppExpr::TypedInitList { ty, .. } => Some(ty.clone()),
        CppExpr::Call(f, _) => match &**f {
            CppExpr::Ident(n) => env.fn_ret.get(n).cloned(),
            // M53: 成员调用 `h.get()` / `p->get()` 也是函数模板实参的常见形态.
            // 之前只当 None 回去, 使 `id(h.get())` 整个调用点不被实例化.
            CppExpr::Member(obj, m) | CppExpr::PtrMember(obj, m) => {
                let bt = infer_expr_ty(obj, env)?;
                let owner = match bt {
                    MangleType::Pointer(inner) | MangleType::Reference(inner) => {
                        class_owner_of(&inner)
                    },
                    other => class_owner_of(&other),
                }?;
                env.class_mfns.get(&owner)?.get(m.as_str()).cloned()
            },
            CppExpr::QualifiedAccess(_, n) => env.fn_ret.get(n.as_str()).cloned(),
            _ => None,
        },
        CppExpr::UnaryOp(op, x) => match op {
            crate::cpp::parser::UnaryOpKind::Not | crate::cpp::parser::UnaryOpKind::LNot => {
                Some(MangleType::Bool)
            },
            crate::cpp::parser::UnaryOpKind::Deref => {
                let t = infer_expr_ty(x, env)?;
                match t {
                    MangleType::Pointer(inner) | MangleType::Reference(inner) => Some(*inner),
                    _ => None,
                }
            },
            _ => infer_expr_ty(x, env),
        },
        CppExpr::BinOp(op, l, _r) => match op {
            crate::cpp::parser::BinOpKind::EqEq
            | crate::cpp::parser::BinOpKind::Ne
            | crate::cpp::parser::BinOpKind::Lt
            | crate::cpp::parser::BinOpKind::Gt
            | crate::cpp::parser::BinOpKind::Le
            | crate::cpp::parser::BinOpKind::Ge
            | crate::cpp::parser::BinOpKind::LAnd
            | crate::cpp::parser::BinOpKind::LOr => Some(MangleType::Bool),
            // `a[i]` 的元素类型 = 基类型解引用
            crate::cpp::parser::BinOpKind::Index => {
                let t = infer_expr_ty(l, env)?;
                match t {
                    MangleType::Pointer(inner) | MangleType::Reference(inner) => Some(*inner),
                    MangleType::Array(inner, _) => Some(*inner),
                    _ => None,
                }
            },
            _ => infer_expr_ty(l, env),
        },
        CppExpr::Ternary { then, .. } => infer_expr_ty(then, env),
        CppExpr::Comma(v) => v.last().and_then(|x| infer_expr_ty(x, env)),
        CppExpr::Sizeof(_) => Some(MangleType::Ulong),
        CppExpr::TemplateIdent { name, args } => Some(MangleType::Template {
            name: name.clone(),
            args: args.clone(),
        }),
        _ => None,
    }
}

/// 取类类型的字段类型 (`bt` 可以是 Class / Template / 已实例化名)
fn field_of(bt: &MangleType, field: &str, env: &TplEnv) -> Option<MangleType> {
    let owner = class_owner_of(bt)?;
    env.class_fields.get(&owner)?.get(field).cloned()
}

/// 从声明中收集所有模板类型引用 (类模板)
fn collect_template_refs_from_decl(
    d: &CppDecl,
    done: &HashSet<String>,
    out: &mut Vec<(String, Vec<MangleType>)>,
) {
    match d {
        CppDecl::GlobalVar { ty, .. } => {
            collect_template_refs_from_type(ty, done, out);
        },
        CppDecl::FreeFn {
            ret, params, body, ..
        } => {
            collect_template_refs_from_type(ret, done, out);
            for (_, t) in params {
                collect_template_refs_from_type(t, done, out);
            }
            collect_template_refs_from_body(body, done, out);
        },
        CppDecl::Class(c) => {
            // 基类中的模板引用
            for base in &c.bases {
                if let Some(args) = &base.template_args {
                    out.push((base.name.clone(), args.clone()));
                }
            }
            for m in &c.members {
                collect_template_refs_from_decl(m, done, out);
            }
        },
        CppDecl::MemberVar { ty, .. } => {
            collect_template_refs_from_type(ty, done, out);
        },
        CppDecl::MemberFn {
            ret, params, body, ..
        } => {
            collect_template_refs_from_type(ret, done, out);
            for (_, t) in params {
                collect_template_refs_from_type(t, done, out);
            }
            collect_template_refs_from_body(body, done, out);
        },
        CppDecl::Ctor { params, body, .. } => {
            for (_, t) in params {
                collect_template_refs_from_type(t, done, out);
            }
            collect_template_refs_from_body(body, done, out);
        },
        CppDecl::Dtor { body, .. } => {
            collect_template_refs_from_body(body, done, out);
        },
        CppDecl::Typedef { target, .. } => {
            collect_template_refs_from_type(target, done, out);
        },
        CppDecl::Namespace { body, .. } => {
            for inner in body {
                collect_template_refs_from_decl(inner, done, out);
            }
        },
        _ => {},
    }
}

/// 从类型中收集模板引用
///
/// M51: **后序** (先进内层实参, 再推自己): 保证 `Wrap<Wrap<int>>` 的实例化
/// 顺序上内层先于外层 —— `__dcc_template_Wrap_Wrap_int` 有一个类类型字段,
/// sema 必须先看到字段类型的定义.
///
/// M51: `done` = 已入过工作表的实例化 key 集. 命中时**整棵子树跳过**:
/// 一个实例被登记过, 说明它的内层实参在同一轮后序遍历里也已全部登记
/// (后序保证子先于父), 再走一遍只会重复克隆 O(d) 的实参树并重算 key.
/// 没有这个短路, `Wrap<...>` 899 层嵌套会让每个实例的字段类型 (仍是一棵
/// O(d) 的原始 `Template` 树) 被反复全量展开, 复杂度从 O(n²) 退化到 O(n³).
fn collect_template_refs_from_type(
    ty: &MangleType,
    done: &HashSet<String>,
    out: &mut Vec<(String, Vec<MangleType>)>,
) {
    match ty {
        MangleType::Template { name, args } => {
            if done.contains(&TemplateRegistry::key_for(name, args)) {
                return;
            }
            for a in args {
                collect_template_refs_from_type(a, done, out);
            }
            out.push((name.clone(), args.clone()));
        },
        MangleType::Pointer(t) | MangleType::Reference(t) | MangleType::Array(t, _) => {
            collect_template_refs_from_type(t, done, out)
        },
        MangleType::TplMember { name, args, .. } => {
            if done.contains(&TemplateRegistry::key_for(name, args)) {
                return;
            }
            for a in args {
                collect_template_refs_from_type(a, done, out);
            }
            out.push((name.clone(), args.clone()));
        },
        _ => {},
    }
}

/// 从函数体中收集模板类型引用
fn collect_template_refs_from_body(
    body: &CppBody,
    done: &HashSet<String>,
    out: &mut Vec<(String, Vec<MangleType>)>,
) {
    for stmt in &body.stmts {
        collect_template_refs_from_stmt(stmt, done, out);
    }
}

/// 从语句中收集模板类型引用
fn collect_template_refs_from_stmt(
    s: &CppStmt,
    done: &HashSet<String>,
    out: &mut Vec<(String, Vec<MangleType>)>,
) {
    match s {
        CppStmt::LocalVar { ty, init, .. } | CppStmt::StaticLocal { ty, init, .. } => {
            collect_template_refs_from_type(ty, done, out);
            if let Some(e) = init {
                collect_template_refs_from_expr(e, done, out);
            }
        },
        CppStmt::LocalVarList { spec, declarators } => {
            // 共享的 decl-specifier 只出现一次, 但每个 declarator 的初始化式都要扫。
            collect_template_refs_from_type(&spec.ty, done, out);
            for d in declarators {
                if let Some(e) = &d.init {
                    collect_template_refs_from_expr(e, done, out);
                }
            }
        },
        CppStmt::Expr(e) | CppStmt::Return(Some(e)) => {
            collect_template_refs_from_expr(e, done, out);
        },
        CppStmt::If { cond, then, els } => {
            collect_template_refs_from_expr(cond, done, out);
            collect_template_refs_from_body(then, done, out);
            if let Some(e) = els {
                collect_template_refs_from_body(e, done, out);
            }
        },
        CppStmt::While { cond, body } => {
            collect_template_refs_from_expr(cond, done, out);
            collect_template_refs_from_body(body, done, out);
        },
        CppStmt::For {
            init,
            cond,
            step,
            body,
        } => {
            if let Some(i) = init {
                collect_template_refs_from_stmt(i, done, out);
            }
            if let Some(c) = cond {
                collect_template_refs_from_expr(c, done, out);
            }
            if let Some(s) = step {
                collect_template_refs_from_stmt(s, done, out);
            }
            collect_template_refs_from_body(body, done, out);
        },
        CppStmt::Block(b) => collect_template_refs_from_body(b, done, out),
        CppStmt::Decl(d) => collect_template_refs_from_decl(d, done, out),
        _ => {},
    }
}

/// 从表达式中收集模板类型引用
fn collect_template_refs_from_expr(
    e: &CppExpr,
    done: &HashSet<String>,
    out: &mut Vec<(String, Vec<MangleType>)>,
) {
    match e {
        CppExpr::Cast(ty, inner) | CppExpr::StaticCast { ty, expr: inner } => {
            collect_template_refs_from_type(ty, done, out);
            collect_template_refs_from_expr(inner, done, out);
        },
        CppExpr::Call(f, args) => {
            collect_template_refs_from_expr(f, done, out);
            for a in args {
                collect_template_refs_from_expr(a, done, out);
            }
        },
        CppExpr::New(_, args) => {
            for a in args {
                collect_template_refs_from_expr(a, done, out);
            }
        },
        CppExpr::NewArr { count, .. } => {
            collect_template_refs_from_expr(count, done, out);
        },
        CppExpr::BinOp(_, l, r) => {
            collect_template_refs_from_expr(l, done, out);
            collect_template_refs_from_expr(r, done, out);
        },
        CppExpr::UnaryOp(_, x) | CppExpr::Ref(x) | CppExpr::AddrOf(x) => {
            collect_template_refs_from_expr(x, done, out);
        },
        CppExpr::Member(o, _) | CppExpr::PtrMember(o, _) => {
            collect_template_refs_from_expr(o, done, out);
        },
        CppExpr::TypedInitList { ty, elems } => {
            collect_template_refs_from_type(ty, done, out);
            for e in elems {
                collect_template_refs_from_expr(e, done, out);
            }
        },
        CppExpr::TplStaticAccess { name, args, .. }
            if !done.contains(&TemplateRegistry::key_for(name, args)) =>
        {
            out.push((name.clone(), args.clone()));
        },
        _ => {},
    }
}

/// 将声明中的模板类型替换为实例化后的类名
/// 例如: `Foo<int>` -> `__dcc_template_Foo_int`
fn replace_template_types_in_decls(decls: &[CppDecl], reg: &TemplateRegistry) -> Vec<CppDecl> {
    decls
        .iter()
        .map(|d| replace_template_types_in_decl(d, reg))
        .collect()
}

/// 替换单个声明中的模板类型
fn replace_template_types_in_decl(d: &CppDecl, reg: &TemplateRegistry) -> CppDecl {
    match d {
        CppDecl::GlobalVar {
            ty,
            name,
            init,
            is_static,
            is_volatile,
        } => CppDecl::GlobalVar {
            ty: replace_template_type(ty, reg),
            name: name.clone(),
            init: init
                .as_ref()
                .map(|e| replace_template_types_in_expr(e, reg)),
            is_static: *is_static,
            is_volatile: *is_volatile,
        },
        CppDecl::FreeFn {
            ret,
            name,
            params,
            body,
            is_noexcept,
            trailing_ret_type,
        } => CppDecl::FreeFn {
            ret: replace_template_type(ret, reg),
            name: name.clone(),
            params: params
                .iter()
                .map(|(n, t)| (n.clone(), replace_template_type(t, reg)))
                .collect(),
            body: replace_template_types_in_body(body, reg),
            is_noexcept: *is_noexcept,
            trailing_ret_type: trailing_ret_type
                .as_ref()
                .map(|t| replace_template_type(t, reg)),
        },
        CppDecl::Class(c) => {
            let new_bases = c
                .bases
                .iter()
                .map(|base| {
                    if let Some(args) = &base.template_args {
                        let new_name = reg
                            .instance_class_name(&base.name, args)
                            .unwrap_or_else(|| instantiated_class_name(&base.name, args));
                        crate::cpp::parser::BaseSpec {
                            name: new_name,
                            template_args: None,
                            ..base.clone()
                        }
                    } else {
                        base.clone()
                    }
                })
                .collect();
            CppDecl::Class(ClassDecl {
                name: c.name.clone(),
                is_struct: c.is_struct,
                is_union: c.is_union,
                bases: new_bases,
                members: c
                    .members
                    .iter()
                    .map(|m| replace_template_types_in_decl(m, reg))
                    .collect(),
                member_access: c.member_access.clone(),
            })
        },
        CppDecl::MemberVar {
            ty,
            name,
            is_mutable,
            is_volatile,
        } => CppDecl::MemberVar {
            ty: replace_template_type(ty, reg),
            name: name.clone(),
            is_mutable: *is_mutable,
            is_volatile: *is_volatile,
        },
        CppDecl::MemberFn {
            class,
            ret,
            name,
            params,
            body,
            is_virtual,
            is_override,
            is_const,
            is_final,
            is_noexcept,
            is_volatile,
            is_friend,
        } => CppDecl::MemberFn {
            class: class.clone(),
            ret: replace_template_type(ret, reg),
            name: name.clone(),
            params: params
                .iter()
                .map(|(n, t)| (n.clone(), replace_template_type(t, reg)))
                .collect(),
            body: replace_template_types_in_body(body, reg),
            is_virtual: *is_virtual,
            is_override: *is_override,
            is_const: *is_const,
            is_final: *is_final,
            is_noexcept: *is_noexcept,
            is_volatile: *is_volatile,
            is_friend: *is_friend,
        },
        CppDecl::Ctor {
            class,
            params,
            inits,
            body,
            is_virtual,
            is_explicit,
            is_noexcept,
        } => CppDecl::Ctor {
            class: class.clone(),
            params: params
                .iter()
                .map(|(n, t)| (n.clone(), replace_template_type(t, reg)))
                .collect(),
            inits: inits
                .iter()
                .map(|(n, e)| (n.clone(), replace_template_types_in_expr(e, reg)))
                .collect(),
            body: replace_template_types_in_body(body, reg),
            is_virtual: *is_virtual,
            is_explicit: *is_explicit,
            is_noexcept: *is_noexcept,
        },
        CppDecl::Dtor {
            class,
            body,
            is_virtual,
            is_noexcept,
        } => CppDecl::Dtor {
            class: class.clone(),
            body: replace_template_types_in_body(body, reg),
            is_virtual: *is_virtual,
            is_noexcept: *is_noexcept,
        },
        CppDecl::Typedef { target, name } => CppDecl::Typedef {
            target: replace_template_type(target, reg),
            name: name.clone(),
        },
        CppDecl::Namespace { name, body } => CppDecl::Namespace {
            name: name.clone(),
            body: replace_template_types_in_decls(body, reg),
        },
        // 模板声明本身保留（会被过滤掉），但内部不替换
        other => other.clone(),
    }
}

/// 把类型里的**依赖限定名前缀**绑定到模板实参。
///
/// `typedef typename Iter::value_type value_type;` 在模板体里被 parser 存成整体
/// 限定名 `MangleType::Class("Iter::value_type")`，而 `template::substitute_type`
/// 只替换「整名恰好等于形参」的类型，所以形参会作为前缀残留在实例类体里，
/// 导致 `traits<MyIt>::value_type` / `std::iterator_traits<It>::reference` 这类
/// 「模板成员 typedef」在 sema 里解析不出来 (报 unknown class type)。
///
/// 规则是通用的：仅当限定名的**首段**等于某个模板形参名，且对应实参是类类型时，
/// 把首段换成实参类名 (`Iter::value_type` → `MyIt::value_type`)；其它限定名
/// (`std::X` 等) 原样保留。不按任何具体类型名/夹具名特判。
fn bind_dependent_qualifier(ty: &MangleType, params: &[String], args: &[MangleType]) -> MangleType {
    /// 首段命中形参且实参为类类型时返回替换后的整体名
    fn rebind_prefix(n: &str, params: &[String], args: &[MangleType]) -> Option<String> {
        let pos = n.find("::")?;
        let idx = params.iter().position(|p| *p == n[..pos])?;
        match args.get(idx) {
            Some(MangleType::Class(an)) => Some(format!("{}{}", an, &n[pos..])),
            _ => None,
        }
    }
    match ty {
        MangleType::Class(n) => {
            if let Some(idx) = params.iter().position(|p| p == n) {
                if let Some(a) = args.get(idx) {
                    return a.clone();
                }
            }
            match rebind_prefix(n, params, args) {
                Some(new_name) => MangleType::Class(new_name),
                None => ty.clone(),
            }
        },
        MangleType::TplMember {
            name,
            args: ta,
            member,
        } => {
            // `Param::member` 的 TplMember 形态 (args 为空): 展开成实参类的嵌套名
            if ta.is_empty() {
                if let Some(idx) = params.iter().position(|p| p == name) {
                    if let Some(MangleType::Class(an)) = args.get(idx) {
                        return MangleType::Class(format!("{}::{}", an, member));
                    }
                }
            }
            MangleType::TplMember {
                name: name.clone(),
                args: ta
                    .iter()
                    .map(|a| bind_dependent_qualifier(a, params, args))
                    .collect(),
                member: member.clone(),
            }
        },
        MangleType::Pointer(t) => {
            MangleType::Pointer(Box::new(bind_dependent_qualifier(t, params, args)))
        },
        MangleType::Reference(t) => {
            MangleType::Reference(Box::new(bind_dependent_qualifier(t, params, args)))
        },
        MangleType::Array(t, n) => {
            MangleType::Array(Box::new(bind_dependent_qualifier(t, params, args)), *n)
        },
        MangleType::Template { name, args: ta } => MangleType::Template {
            name: name.clone(),
            args: ta
                .iter()
                .map(|a| bind_dependent_qualifier(a, params, args))
                .collect(),
        },
        other => other.clone(),
    }
}

/// 递归遍历声明的**签名级**类型字段，应用 [`bind_dependent_qualifier`]。
/// 只在类模板实例化产出上做这一次修正，函数体内部由 template.rs 的形参替换负责。
fn bind_dependent_names(decl: &CppDecl, params: &[String], args: &[MangleType]) -> CppDecl {
    let b = |t: &MangleType| bind_dependent_qualifier(t, params, args);
    let bp = |ps: &[(String, MangleType)]| -> Vec<(String, MangleType)> {
        ps.iter().map(|(n, t)| (n.clone(), b(t))).collect()
    };
    match decl {
        CppDecl::Typedef { target, name } => CppDecl::Typedef {
            target: b(target),
            name: name.clone(),
        },
        CppDecl::MemberVar {
            ty,
            name,
            is_mutable,
            is_volatile,
        } => CppDecl::MemberVar {
            ty: b(ty),
            name: name.clone(),
            is_mutable: *is_mutable,
            is_volatile: *is_volatile,
        },
        CppDecl::GlobalVar {
            ty,
            name,
            init,
            is_static,
            is_volatile,
        } => CppDecl::GlobalVar {
            ty: b(ty),
            name: name.clone(),
            init: init.clone(),
            is_static: *is_static,
            is_volatile: *is_volatile,
        },
        CppDecl::FreeFn {
            ret,
            name,
            params: ps,
            body,
            is_noexcept,
            trailing_ret_type,
        } => CppDecl::FreeFn {
            ret: b(ret),
            name: name.clone(),
            params: bp(ps),
            body: body.clone(),
            is_noexcept: *is_noexcept,
            trailing_ret_type: trailing_ret_type.as_ref().map(&b),
        },
        CppDecl::MemberFn {
            class,
            ret,
            name,
            params: ps,
            body,
            is_virtual,
            is_override,
            is_final,
            is_const,
            is_noexcept,
            is_volatile,
            is_friend,
        } => CppDecl::MemberFn {
            class: class.clone(),
            ret: b(ret),
            name: name.clone(),
            params: bp(ps),
            body: body.clone(),
            is_virtual: *is_virtual,
            is_override: *is_override,
            is_final: *is_final,
            is_const: *is_const,
            is_noexcept: *is_noexcept,
            is_volatile: *is_volatile,
            is_friend: *is_friend,
        },
        CppDecl::Ctor {
            class,
            params: ps,
            inits,
            body,
            is_virtual,
            is_explicit,
            is_noexcept,
        } => CppDecl::Ctor {
            class: class.clone(),
            params: bp(ps),
            inits: inits.clone(),
            body: body.clone(),
            is_virtual: *is_virtual,
            is_explicit: *is_explicit,
            is_noexcept: *is_noexcept,
        },
        CppDecl::OperatorFn {
            class,
            op,
            params: ps,
            body,
            ret,
            is_unary,
        } => CppDecl::OperatorFn {
            class: class.clone(),
            op: op.clone(),
            params: bp(ps),
            body: body.clone(),
            ret: b(ret),
            is_unary: *is_unary,
        },
        CppDecl::Class(c) => CppDecl::Class(ClassDecl {
            name: c.name.clone(),
            is_struct: c.is_struct,
            is_union: c.is_union,
            bases: c.bases.clone(),
            members: c
                .members
                .iter()
                .map(|m| bind_dependent_names(m, params, args))
                .collect(),
            member_access: c.member_access.clone(),
        }),
        CppDecl::AnonAggr {
            is_union,
            members,
            name,
        } => CppDecl::AnonAggr {
            is_union: *is_union,
            members: members
                .iter()
                .map(|m| bind_dependent_names(m, params, args))
                .collect(),
            name: name.clone(),
        },
        CppDecl::Namespace { name, body } => CppDecl::Namespace {
            name: name.clone(),
            body: body
                .iter()
                .map(|d| bind_dependent_names(d, params, args))
                .collect(),
        },
        other => other.clone(),
    }
}

/// 替换类型中的模板引用
fn replace_template_type(ty: &MangleType, reg: &TemplateRegistry) -> MangleType {
    match ty {
        MangleType::Template { name, args } => {
            // 检查是否是已实例化的类模板
            //
            // M52: 类名必须取自**缓存里的实例声明** (`instance_class_name`),
            // 不能就地用调用点实参重算: 省略默认尾部形参的 `fm<int,int>` 会算出
            // 与登记名不一致的 `__dcc_template_fm_int_int`, 替换静默失效.
            if let Some(inst_name) = reg.instance_class_name(name, args) {
                // 替换为实例化后的类名
                return MangleType::Class(inst_name);
            }
            // 未实例化，递归替换参数后保留
            let new_args: Vec<MangleType> =
                args.iter().map(|a| replace_template_type(a, reg)).collect();
            MangleType::Template {
                name: name.clone(),
                args: new_args,
            }
        },
        MangleType::Pointer(t) => MangleType::Pointer(Box::new(replace_template_type(t, reg))),
        MangleType::Reference(t) => MangleType::Reference(Box::new(replace_template_type(t, reg))),
        MangleType::TplMember { name, args, member } => {
            // 模板成员访问：先替换模板类名
            if let Some(inst) = reg.instance_class_name(name, args) {
                // 简化：转为普通类成员访问类型
                MangleType::Class(format!("{}::{}", inst, member))
            } else {
                let new_args: Vec<MangleType> =
                    args.iter().map(|a| replace_template_type(a, reg)).collect();
                MangleType::TplMember {
                    name: name.clone(),
                    args: new_args,
                    member: member.clone(),
                }
            }
        },
        other => other.clone(),
    }
}

/// 替换函数体中的模板类型
fn replace_template_types_in_body(body: &CppBody, reg: &TemplateRegistry) -> CppBody {
    CppBody {
        stmts: body
            .stmts
            .iter()
            .map(|s| replace_template_types_in_stmt(s, reg))
            .collect(),
        is_decl: body.is_decl,
    }
}

/// 替换语句中的模板类型
fn replace_template_types_in_stmt(s: &CppStmt, reg: &TemplateRegistry) -> CppStmt {
    match s {
        CppStmt::LocalVar {
            ty,
            name,
            init,
            is_volatile,
        } => CppStmt::LocalVar {
            ty: replace_template_type(ty, reg),
            name: name.clone(),
            init: init
                .as_ref()
                .map(|e| replace_template_types_in_expr(e, reg)),
            is_volatile: *is_volatile,
        },
        CppStmt::StaticLocal {
            ty,
            name,
            init,
            is_volatile,
        } => CppStmt::StaticLocal {
            ty: replace_template_type(ty, reg),
            name: name.clone(),
            init: init
                .as_ref()
                .map(|e| replace_template_types_in_expr(e, reg)),
            is_volatile: *is_volatile,
        },
        CppStmt::LocalVarList { spec, declarators } => {
            // 只替换共享的 decl-specifier 类型部分; 每个 declarator 的 `*`/`&`/`[N]`
            // 层级结构原样保留 (它们是声明符的一部分, 不携带模板名)。
            CppStmt::LocalVarList {
                spec: DeclSpec {
                    ty: replace_template_type(&spec.ty, reg),
                    is_static: spec.is_static,
                    is_volatile: spec.is_volatile,
                },
                declarators: declarators
                    .iter()
                    .map(|d| InitDeclarator {
                        layers: d.layers.clone(),
                        array_dims: d.array_dims.clone(),
                        name: d.name.clone(),
                        init: d
                            .init
                            .as_ref()
                            .map(|e| replace_template_types_in_expr(e, reg)),
                    })
                    .collect(),
            }
        },
        CppStmt::Expr(e) => CppStmt::Expr(replace_template_types_in_expr(e, reg)),
        CppStmt::Return(e) => {
            CppStmt::Return(e.as_ref().map(|x| replace_template_types_in_expr(x, reg)))
        },
        CppStmt::If { cond, then, els } => CppStmt::If {
            cond: replace_template_types_in_expr(cond, reg),
            then: Box::new(replace_template_types_in_body(then, reg)),
            els: els
                .as_ref()
                .map(|x| Box::new(replace_template_types_in_body(x, reg))),
        },
        CppStmt::While { cond, body } => CppStmt::While {
            cond: replace_template_types_in_expr(cond, reg),
            body: Box::new(replace_template_types_in_body(body, reg)),
        },
        CppStmt::For {
            init,
            cond,
            step,
            body,
        } => CppStmt::For {
            init: init
                .as_ref()
                .map(|x| Box::new(replace_template_types_in_stmt(x, reg))),
            cond: cond
                .as_ref()
                .map(|x| replace_template_types_in_expr(x, reg)),
            step: step
                .as_ref()
                .map(|x| Box::new(replace_template_types_in_stmt(x, reg))),
            body: Box::new(replace_template_types_in_body(body, reg)),
        },
        CppStmt::Block(b) => CppStmt::Block(replace_template_types_in_body(b, reg)),
        CppStmt::Decl(d) => CppStmt::Decl(replace_template_types_in_decl(d, reg)),
        other => other.clone(),
    }
}

/// 替换表达式中的模板类型
fn replace_template_types_in_expr(e: &CppExpr, reg: &TemplateRegistry) -> CppExpr {
    match e {
        CppExpr::Cast(ty, inner) => CppExpr::Cast(
            replace_template_type(ty, reg),
            Box::new(replace_template_types_in_expr(inner, reg)),
        ),
        CppExpr::StaticCast { ty, expr } => CppExpr::StaticCast {
            ty: replace_template_type(ty, reg),
            expr: Box::new(replace_template_types_in_expr(expr, reg)),
        },
        CppExpr::ConstCast { ty, expr } => CppExpr::ConstCast {
            ty: replace_template_type(ty, reg),
            expr: Box::new(replace_template_types_in_expr(expr, reg)),
        },
        CppExpr::ReinterpretCast { ty, expr } => CppExpr::ReinterpretCast {
            ty: replace_template_type(ty, reg),
            expr: Box::new(replace_template_types_in_expr(expr, reg)),
        },
        CppExpr::DynamicCast { ty, expr } => CppExpr::DynamicCast {
            ty: replace_template_type(ty, reg),
            expr: Box::new(replace_template_types_in_expr(expr, reg)),
        },
        CppExpr::TypeidType(ty) => CppExpr::TypeidType(replace_template_type(ty, reg)),
        CppExpr::Call(f, args) => CppExpr::Call(
            Box::new(replace_template_types_in_expr(f, reg)),
            args.iter()
                .map(|a| replace_template_types_in_expr(a, reg))
                .collect(),
        ),
        CppExpr::BinOp(op, l, r) => CppExpr::BinOp(
            *op,
            Box::new(replace_template_types_in_expr(l, reg)),
            Box::new(replace_template_types_in_expr(r, reg)),
        ),
        CppExpr::UnaryOp(op, x) => {
            CppExpr::UnaryOp(*op, Box::new(replace_template_types_in_expr(x, reg)))
        },
        CppExpr::Ref(x) => CppExpr::Ref(Box::new(replace_template_types_in_expr(x, reg))),
        CppExpr::AddrOf(x) => CppExpr::AddrOf(Box::new(replace_template_types_in_expr(x, reg))),
        CppExpr::Member(o, s) => {
            CppExpr::Member(Box::new(replace_template_types_in_expr(o, reg)), s.clone())
        },
        CppExpr::PtrMember(o, s) => {
            CppExpr::PtrMember(Box::new(replace_template_types_in_expr(o, reg)), s.clone())
        },
        CppExpr::TypedInitList { ty, elems } => CppExpr::TypedInitList {
            ty: replace_template_type(ty, reg),
            elems: elems
                .iter()
                .map(|x| replace_template_types_in_expr(x, reg))
                .collect(),
        },
        CppExpr::TplStaticAccess { name, args, member } => {
            // 模板静态成员访问：替换为实例化类的成员访问
            if let Some(class_name) = reg.instance_class_name(name, args) {
                CppExpr::Member(Box::new(CppExpr::Ident(class_name)), member.clone())
            } else {
                e.clone()
            }
        },
        CppExpr::TemplateIdent { name, args } => {
            // 模板标识：可能是函数模板调用，也可能是类模板构造
            // 1) 类模板实例: 缓存键就是 name<args>
            if let Some(class_name) = reg.instance_class_name(name, args) {
                // 类模板：替换为实例化后的类名（用于构造函数调用）
                return CppExpr::Ident(class_name);
            }
            // 2) 函数模板实例: 键里带候选下标, 反查已实例化的那个候选
            // (本阶段手上只有 name+args, 没有推导时的候选信息)
            match reg.cached_fn_candidate(name, args) {
                Some(idx) => CppExpr::Ident(tplfn_c_name(name, idx, args)),
                None => e.clone(),
            }
        },
        other => other.clone(),
    }
}

/// 过滤掉模板声明（Template / PartialSpec / Concept 等），只保留普通声明.
/// 需递归进入 namespace / export 块: 否则 `namespace std { template<class M>
/// class lock_guard {...} }` 里的未实例化模板类体会交给 Sema, 其模板参数
/// 类型 (如 `M`) 无法解析.
fn filter_template_decls(decls: &[CppDecl]) -> Vec<CppDecl> {
    decls
        .iter()
        .filter(|d| {
            !matches!(
                d,
                CppDecl::Template { .. } | CppDecl::PartialSpec { .. } | CppDecl::Concept { .. }
            )
        })
        .map(|d| match d {
            CppDecl::Namespace { name, body } => CppDecl::Namespace {
                name: name.clone(),
                body: filter_template_decls(body),
            },
            CppDecl::ExportBlock(inner) => CppDecl::ExportBlock(filter_template_decls(inner)),
            other => other.clone(),
        })
        .collect()
}

/// 把 C++ 顶层声明通过原生路径降级为 ast::Program.
///
/// 流程:
/// 0. 模板收集 + 实例化 (Stage 8)
/// 1. `Sema` 构建 HIR + 收集符号表
/// 2. 两阶段 ABI 计算: 先注册所有 class + method, 再 compute
/// 3. `IrLowerer` 产出 SSA 函数 + vtable 数据
/// 4. 把每个 SSA 函数 `rebuild_block` 回 ast::Block, 组装 FuncDecl
/// 5. 为每个 class 生成 `Decl::StructDef`, 名字/字段顺序与 `lower_type` 一致
/// 6. 把 vtable 数据降级为 `Decl::Var` (extern 数组) 占位 (Stage 5 完整发射)
pub fn lower_decls_native(decls: &[CppDecl]) -> Result<NativeLowered, NativeError> {
    // M52: C++20 协程 —— 在通用管线之前把协程重写为普通函数 + 状态机.
    // 只有 TU 里确实出现 co_await / co_yield / co_return 时才介入, 否则零影响.
    let coro_decls;
    let decls: &[CppDecl] = if crate::cpp::coroutine_lower::tu_has_coroutine(decls) {
        match crate::cpp::coroutine_lower::lower_coroutines(decls) {
            Ok(d) => {
                coro_decls = d;
                &coro_decls
            },
            Err(e) => return Err(NativeError::Unsupported(e)),
        }
    } else {
        decls
    };
    // Step 0: 收集模板并实例化 (Stage 8)
    let mut reg = collect_templates(decls);
    let (rewritten, instantiated) = instantiate_templates_from_decls(&mut reg, decls)?;

    // 合并声明: 实例化声明 + 原始声明（过滤掉模板声明）
    let filtered = filter_template_decls(&rewritten);
    // M51: 实例化声明按"模板定义锚点"插回定义点之后, 而不是无条件排在最前.
    // 锚点保证 C++ 的"定义先于使用" (实例化类体里引用的前置声明 / 外部类型
    // 已在模板定义点之前出现过); 把实例全排最前会让那些依赖还没被 sema 登记
    // (`std::vector<int>` 报 "unknown class type: dccpp_vector").
    // 稳排同锚点保持产生顺序 = 依赖顺序 (内层模板先于外层).
    let mut inst = instantiated;
    inst.sort_by_key(|(a, _)| *a);
    let capacity = filtered.len() + inst.len();
    let mut pending = inst.into_iter().peekable();
    let mut all_decls: Vec<CppDecl> = Vec::with_capacity(capacity);
    for (i, d) in filtered.into_iter().enumerate() {
        while let Some(&(anchor, _)) = pending.peek() {
            if anchor > i {
                break;
            }
            all_decls.push(pending.next().expect("peeked").1);
        }
        all_decls.push(d);
    }
    for (_, d) in pending {
        all_decls.push(d);
    }
    let all_decls = replace_template_types_in_decls(&all_decls, &reg);

    // Step 1: 构建 Sema + HIR
    let mut sema = Sema::new();
    if let Err(e) = sema.build_hir_from_decls(&all_decls) {
        return Err(NativeError::Unsupported(e));
    }
    // Debug: DCC_CXX_HIR_DUMP=path 时把 HIR 翻译单元 dump 出来
    if let Ok(p) = std::env::var("DCC_CXX_HIR_DUMP") {
        let _ = std::fs::write(&p, format!("{:#?}\n", sema.hir()));
    }
    // 先克隆符号表 (sema.into_hir 只消费 hir, 但 clone 更简洁安全)
    let symbols: Vec<CxxSymbol> = sema.symbols().to_vec();
    // M54: 全局对象的静态初始化式 (按 SymbolId 索引) 必须在 `into_hir` 前取出。
    let global_inits = sema.global_var_inits().clone();
    // 收集所有类的基类信息 (用于 ABI 布局)
    let (class_bases, class_virtual_bases): (
        HashMap<ClassId, Vec<ClassId>>,
        HashMap<ClassId, Vec<ClassId>>,
    ) = collect_class_bases(&sema, &symbols);
    // Step 2: 计算 ABI (两阶段: 先全部注册, 再 compute)
    let abi_map: HashMap<ClassId, ClassAbi> =
        build_class_abis(&sema, &symbols, &class_bases, &class_virtual_bases)?;
    // Debug: DCC_CXX_ABI_DUMP=path 时把 ABI 布局表 dump 出来
    if let Ok(p) = std::env::var("DCC_CXX_ABI_DUMP") {
        let mut s = String::new();
        for (cid, abi) in &abi_map {
            s.push_str(&format!("class {:?} -> {:#?}\n", cid, abi));
        }
        let _ = std::fs::write(&p, s);
    }
    let hir: HirTranslationUnit = sema.into_hir();

    // Step 3: lower HIR -> SSA
    let mut lowerer = IrLowerer::new(&hir, &symbols);
    for (cid, abi) in &abi_map {
        lowerer.register_abi(*cid, abi.clone());
    }
    lowerer.lower()?;
    let output: LowerIrOutput = lowerer.finish();

    // Step 4 + 5: SSA 函数 + struct 定义 -> ast::Program
    let prog = build_program(&output, &abi_map, &hir, &symbols, &global_inits)?;
    // Debug: DCC_CXX_IR_DUMP=path 时把最终 ast::Program dump 出来
    if let Ok(p) = std::env::var("DCC_CXX_IR_DUMP") {
        let _ = std::fs::write(&p, format!("{:#?}\n", prog));
    }

    // Step 6: vtable 数据暂作 rodata 记号, Stage 5 完整发射.
    Ok(NativeLowered {
        prog,
        vtables: output.vtables,
        rodata: output.rodata,
    })
}

// === ABI 计算 (两阶段) ===

/// 从 Sema 中收集每个类的直接基类列表（非虚基类和虚基类分开）。
/// 返回 (non_virtual_bases, virtual_bases)。
fn collect_class_bases(
    sema: &Sema,
    symbols: &[CxxSymbol],
) -> (HashMap<ClassId, Vec<ClassId>>, HashMap<ClassId, Vec<ClassId>>) {
    let mut non_virtual = HashMap::new();
    let mut virtual_bases = HashMap::new();
    for sym in symbols {
        if !matches!(sym.kind, CxxSymbolKind::Class) {
            continue;
        }
        let cid = ClassId::from(sym.id);
        let mut nv_bases = Vec::new();
        let mut v_bases = Vec::new();
        if let Some(info) = sema.get_class_info(cid) {
            for base in &info.bases {
                if base.is_virtual {
                    v_bases.push(base.class);
                } else {
                    nv_bases.push(base.class);
                }
            }
        }
        non_virtual.insert(cid, nv_bases);
        virtual_bases.insert(cid, v_bases);
    }
    (non_virtual, virtual_bases)
}

fn build_class_abis(
    sema: &Sema,
    symbols: &[CxxSymbol],
    class_bases: &HashMap<ClassId, Vec<ClassId>>,
    class_virtual_bases: &HashMap<ClassId, Vec<ClassId>>,
) -> Result<HashMap<ClassId, ClassAbi>, NativeError> {
    let mut calc = ItaniumAbiCalculator::new();

    // 第一遍: 注册所有 class (含基类), 即使基类也先注册避免 compute 递归失败
    for sym in symbols {
        if !matches!(sym.kind, CxxSymbolKind::Class) {
            continue;
        }
        let cid = ClassId::from(sym.id);
        let mut fields: Vec<(String, CxxType)> = Vec::new();
        for s2 in symbols {
            if s2.owner == Some(sym.id)
                && matches!(s2.kind, CxxSymbolKind::Field | CxxSymbolKind::Variable)
                && !s2.name.is_empty()
                // 虚基子对象句柄 (`__vb_<Base>`) 是 sema 为"取虚基子对象地址"
                // 登记的合成符号, 不代表真实数据成员: 子对象由布局层的
                // virtual_bases 列表放到尾部, 不能当成普通字段再排一次。
                && !crate::cpp::abi_layout::is_vbase_subobject_field(&s2.name)
            {
                fields.push((s2.name.clone(), s2.ty.clone()));
            }
        }
        let bases = class_bases.get(&cid).cloned().unwrap_or_default();
        let virtual_bases = class_virtual_bases.get(&cid).cloned().unwrap_or_default();
        // 是否多态 (自身/继承有虚函数): 决定 vptr / vtable / typeinfo 是否发射。
        let has_virtual = sema
            .get_class_info(cid)
            .map(|info| !info.virtual_methods.is_empty())
            .unwrap_or(false);
        // 真实类名是 RTTI/vtable 符号 mangle 的唯一来源, 必须注册;
        // 否则退回 `Class{id}` (TU 局部下标), 跨 TU 不稳定, weak 符号无法合并。
        calc.register_class_with_name(cid, &sym.name, bases, virtual_bases, fields, has_virtual);
        // ABI 符号 (_ZTV/_ZTI/_ZTS) 必须用**限定**名: `ns::Widget` 的 typeinfo
        // 是 `_ZTIN2ns6WidgetE`, 不是 `_ZTI6Widget` (后者会和全局 `Widget` 撞
        // 符号, 也无法与 g++ 产物互操作)。简单名继续供布局层合成 `__vb_Base`
        // 这类与 sema 协商的内部标识。
        calc.register_class_mangle_name(cid, sema.qualified_class_name(cid));
    }

    // 第二遍: 注册方法 (需要 class 已经注册好; 现在补充 has_virtual / method 列表)
    // 注: ItaniumAbiCalculator::register_method 会修改 has_virtual, 必须在 register_class 之后
    for sym in symbols {
        if !matches!(
            sym.kind,
            CxxSymbolKind::Method | CxxSymbolKind::Constructor | CxxSymbolKind::Destructor
        ) {
            continue;
        }
        let Some(owner_sym_id) = sym.owner else {
            continue;
        };
        let cid = ClassId::from(owner_sym_id);
        let is_virtual = sema
            .get_class_info(cid)
            .map(|info| info.virtual_methods.iter().any(|vm| vm.symbol == sym.id))
            .unwrap_or(false);
        let owner_class = sema
            .get_class_info(cid)
            .and_then(|info| {
                info.virtual_methods
                    .iter()
                    .find(|vm| vm.symbol == sym.id)
                    .and_then(|vm| vm.overrides)
            })
            .and_then(|base_method| symbols.iter().find(|s| s.id == base_method))
            .and_then(|base_sym| base_sym.owner)
            .map(ClassId::from)
            .unwrap_or(cid);
        // 布局层用**方法名**识别「同一条虚槽」: 析构函数在每个类里叫 `~C`,
        // 但沿继承链它是同一个槽 (Itanium: 一个对象只有一个析构)。传规范名 `~`
        // 下去, `collect_virtual_methods` 才能把派生类析构识别为基类析构的重写;
        // 否则 `_ZTV<D>` 的析构槽永远留着基类实现 (`delete (B*)d` 不销毁派生)。
        // vtable 里存的**实现符号**仍由 `symbol_link_name` 按真实符号名给出。
        let layout_method_name = if matches!(sym.kind, CxxSymbolKind::Destructor) {
            "~".to_string()
        } else {
            sym.name.clone()
        };
        calc.register_method_with_symbol(
            cid,
            layout_method_name,
            // vtable slot 里存的是函数符号地址, 必须用后端真正发射的名字
            // (成员函数为 `<Class>_<method>`, 见 lower_ir::symbol_link_name),
            // 用裸方法名会让不同类的同名虚函数指向同一个符号。
            symbol_link_name(symbols, sym),
            is_virtual,
            owner_class,
        );
    }

    // 第三遍: compute (顺序无关, 因为已注册所有依赖)
    let mut abis = HashMap::new();
    for sym in symbols {
        if !matches!(sym.kind, CxxSymbolKind::Class) {
            continue;
        }
        let cid = ClassId::from(sym.id);
        let abi = calc.compute_class(cid);
        abis.insert(cid, abi);
    }
    Ok(abis)
}

// === Program 组装 ===

fn build_program(
    output: &LowerIrOutput,
    abi_map: &HashMap<ClassId, ClassAbi>,
    hir: &HirTranslationUnit,
    symbols: &[CxxSymbol],
    global_inits: &HashMap<SymbolId, CppExpr>,
) -> Result<ast::Program, NativeError> {
    let mut prog = ast::Program {
        decls: Vec::new(),
        fenv_access: false,
        fp_contract: false,
        cx_limited_range: false,
    };

    // 先发射 struct 定义 (Decl::StructDef), 名字与字段顺序与 lower_type 一致
    let mut emitted_structs: HashSet<String> = HashSet::new();
    for sym in symbols {
        if !matches!(sym.kind, CxxSymbolKind::Class) {
            continue;
        }
        let cid = ClassId::from(sym.id);
        let Some(abi) = abi_map.get(&cid) else {
            continue;
        };
        let name = sym.name.clone();
        if emitted_structs.contains(&name) {
            continue;
        }
        emitted_structs.insert(name.clone());

        // 收集展平字段 (基类在前, 自身字段在后; 与 lower_ir::lower_class_type 保持一致)
        let fields = collect_flat_struct_fields(cid, abi, abi_map, symbols)?;
        let struct_def = ast::Decl::StructDef(ast::StructDef {
            name: ast::Ident {
                name,
                span: dummy_span(),
            },
            fields,
            is_union: false,
            span: dummy_span(),
        });
        prog.decls.push(struct_def);
    }

    // 静态存储对象 (函数局部 `static` 对象 / `__cxa_guard` 变量, 以及命名空间
    // 作用域的全局对象): 发成真实定义。没有它们时 `&__dcc_static_guard_0`
    // 只是未声明标识符, C 前端按隐式 int 局部处理, `__cxa_guard_acquire`
    // 收到未初始化栈槽 => SIGSEGV。全局对象同理: 不在这里发定义, 其存储
    // 只能由 C 后端从引用处隐式推导 (实测 `int g[10]` 只分到 4 字节)。
    //
    // `is_static: true` (内部链接) + 无初始化器 (.bss 零初始化, 正是 C++
    // 静态存储的零初始化要求, 也让 guard 初值为「未初始化」态)。不用 weak
    // 外部链接: 头文件里的 inline 函数会在多个 TU 各生成一份同名
    // `__dcc_static_<n>`, 弱符号外部链接会把不同流对象合并成同一个。
    // M54: 全局 / 命名空间作用域对象的**静态初值**表 (链接名 -> 初始化式)。
    //
    // `LowerIrOutput::static_objects` 只有名字与类型, 初值在 sema 侧被丢弃过
    // (旧代码 `let _ = init_id;`), 后果是 `int ga = 7;` 落在 .bss 读到 0。
    // 这里按符号把链接名 (`ga` / `ns_a`) 与初始化式对上, 发射时附上。
    // 同一链接名有多条声明 (`extern int g;` 在前, `int g = 5;` 在后) 时取
    // **第一个带初值**的那条 —— 无初值的声明不参与登记, 不会覆盖真初值。
    let mut obj_inits: HashMap<String, &CppExpr> = HashMap::new();
    for sym in symbols {
        if !matches!(sym.kind, CxxSymbolKind::Variable) {
            continue;
        }
        if sym.linkage_name.is_none() {
            continue;
        }
        let Some(e) = global_inits.get(&sym.id) else {
            continue;
        };
        obj_inits
            .entry(symbol_link_name(symbols, sym))
            .or_insert(e);
    }
    for so in &output.static_objects {
        // M54: 「有初值但发不出来」绝不能退化成零初始化 —— 那会让 `int ga = 7;`
        // 这类源码静默读到 0, 是比编译失败严重得多的错误。parser 侧已把
        // constexpr 调用（含聚合初值里的每一项）折成字面量, 仍然折不成的
        // 形态（真正的运行期动态初始化, 需要 .init_array）在此显式报错。
        let raw_init = obj_inits.get(&so.name).copied();
        let init = match raw_init {
            Some(e) => {
                let folded = cpp_static_initializer(e, &so.ty, symbols);
                if folded.is_none() {
                    return Err(NativeError::Unsupported(format!(
                        "global object '{}' has a static initializer that is not \
                         representable at link time (dynamic initialization is not \
                         emitted); refusing to silently zero-initialize it",
                        so.name
                    )));
                }
                folded
            }
            None => None,
        };
        prog.decls.push(ast::Decl::Var(ast::VarDecl {
            name: ast::Ident {
                name: so.name.clone(),
                span: dummy_span(),
            },
            ty: so.ty.clone(),
            init,
            span: dummy_span(),
            is_extern: false,
            is_static: true,
            section: None,
            alignment: None,
            is_weak: false,
            visibility: None,
            is_unused: false,
            register_name: None,
            cleanup_fn: None,
            is_thread_local: false,
            no_sanitize: Vec::new(),
            alias_name: None,
            copy_fn: None,
            is_retain: false,
            is_used_attr: false,
            quals: ast::QualifierFlags::NONE,
            addr_space: ast::AddrSpace::None,
            tls_model: None,
            asm_label: None,
        }));
    }

    // 再发射函数 (FuncDecl): SSA -> ast::Block -> FuncDecl
    //
    // 成员函数 / 构造 / 析构都在类体内定义 (= 隐式 inline), 多 TU 包含同一
    // 个类时会各生成一份定义 —— 必须 weak, 否则 multiple definition。
    let member_fns: HashSet<String> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                CxxSymbolKind::Method | CxxSymbolKind::Constructor | CxxSymbolKind::Destructor
            )
        })
        .map(|s| symbol_link_name(symbols, s))
        .collect();
    for func in &output.functions {
        let decl = build_func_decl(func, &member_fns)?;
        prog.decls.push(decl);
    }

    // vtable / vbtable 真实数据 (Stage 5/7): lower_ir 已按布局层的组件顺序
    // (vbase offsets → offset-to-top → typeinfo → 虚函数) 算好 entry 列表,
    // 这里发射成静态指针数组。vptr 存的是 `符号地址 + address_point`,
    // 所以数组下标 0 就是 vtable 起点。
    //
    // 必须 weak: 同一 class 的定义会出现在多个 TU (头文件里的内联定义),
    // strong 定义会 multiple definition; Itanium 就是把 vtable/typeinfo
    // 作为 weak(Comdat) 符号发射的。
    let slot_ty = ast::Type::Ptr(Box::new(ast::QualType::new(ast::Type::UChar)));
    for (name, entries) in &output.vtables {
        let inits: Vec<ast::InitItem> = entries
            .iter()
            .map(|e| {
                let expr = match e {
                    crate::cpp::lower_ir::VTableEntry::Int(v) => ast::Expr::Int {
                        v: *v as i128,
                        span: dummy_span(),
                        ty: ast::Type::LongLong,
                    },
                    crate::cpp::lower_ir::VTableEntry::SymAddr(sym) => ast::Expr::AddrOf {
                        e: Box::new(ast::Expr::Ident {
                            name: ast::Ident {
                                name: sym.clone(),
                                span: dummy_span(),
                            },
                            span: dummy_span(),
                            ty: slot_ty.clone(),
                        }),
                        span: dummy_span(),
                        ty: slot_ty.clone(),
                    },
                };
                ast::InitItem::Expr(expr)
            })
            .collect();
        let arr_ty = ast::Type::Array(
            Box::new(ast::QualType::new(slot_ty.clone())),
            ast::ArraySize::Const(inits.len().max(1)),
        );
        prog.decls.push(weak_static_var(
            name,
            arr_ty.clone(),
            ast::Expr::InitList {
                inits,
                span: dummy_span(),
                ty: arr_ty,
            },
        ));
    }

    // RTTI: 按 Itanium ABI 发射每个类的 typeinfo 对象与其名字字符串 (均 weak)。
    // vtable 的 TypeInfo slot 指向 `_ZTI<C>`, `typeid(T)` 静态路径也引用同一符号
    // —— 二者相等是 typeid 比较的根。
    //
    // typeinfo 对象的**首槽 (vptr) 不再是 0**: 布局层 (`RttiLayout::typeinfo_words`)
    // 按 `__class_type_info` / `__si_class_type_info` / `__vmi_class_type_info`
    // 三种具体形态给出完整槽序列, vptr 指向本 TU 发射的 `type_info` 虚表, 使
    // libstdc++ 的 `__dynamic_cast` / `__do_catch` 对 dcc 产物发起的虚调用有合法
    // 落点 (否则 `*(ti+0)=0` 直接空指针解引用)。
    let mut emitted_rtti: HashSet<String> = HashSet::new();
    let mut abi_cids: Vec<ClassId> = abi_map.keys().copied().collect();
    abi_cids.sort_by_key(|c| c.0);
    // 本 TU 发射的 RTTI 相关符号定义 (供区分「外部引用」与「本地定义」)。
    let mut rtti_defs: HashSet<String> = HashSet::new();
    // 被 typeinfo 对象/虚表引用、但本 TU 不定义的外部符号 (基类 `_ZTI`、
    // libstdc++ 导出的 type_info 虚槽实现)。逐个发成 weak **声明**:
    // 链到 libstdc++ 时解析为真实现, 纯 gcc 链入时 weak 未定义解析为 0
    // (不会因 undefined reference 断链)。
    let mut rtti_refs: Vec<String> = Vec::new();
    let mut used_ti_kinds: Vec<crate::cpp::abi_layout::TypeInfoKind> = Vec::new();
    // 类 -> `_ZTI` 符号: 优先取布局层结果 (带命名空间前缀), 基类未在本 TU
    // 登记布局时按类名现算同一编码。
    let ti_symbol = |cid: ClassId| -> String {
        if let Some(a) = abi_map.get(&cid) {
            return a.rtti.typeinfo_symbol.clone();
        }
        match symbols.get(cid.0) {
            Some(s) => crate::cpp::mangle::mangle_typeinfo(&s.name),
            None => String::new(),
        }
    };
    // `&sym` 形态的初始化项: 与 vtable 发射一致, 把符号当作 `unsigned char`
    // 对象取地址, 得到槽类型 `unsigned char*`, 避免与真实类型冲突。
    let addr_init = |sym: String| -> ast::InitItem {
        ast::InitItem::Expr(ast::Expr::AddrOf {
            e: Box::new(ast::Expr::Ident {
                name: ast::Ident {
                    name: sym,
                    span: dummy_span(),
                },
                span: dummy_span(),
                ty: slot_ty.clone(),
            }),
            span: dummy_span(),
            ty: slot_ty.clone(),
        })
    };
    for cid in abi_cids {
        let Some(abi) = abi_map.get(&cid) else {
            continue;
        };
        let ti_sym = abi.rtti.typeinfo_symbol.clone();
        let nm_sym = abi.rtti.typeinfo_name_symbol.clone();
        if emitted_rtti.contains(&ti_sym) {
            continue;
        }
        emitted_rtti.insert(ti_sym.clone());
        emitted_rtti.insert(nm_sym.clone());
        rtti_defs.insert(ti_sym.clone());
        rtti_defs.insert(nm_sym.clone());
        // typeinfo 名字字符串: `_ZTS` + mangled 编码, 内容就是该编码
        let encoding = nm_sym.strip_prefix("_ZTS").unwrap_or(nm_sym.as_str());
        let mut bytes = encoding.as_bytes().to_vec();
        bytes.push(0);
        let str_ty = ast::Type::Array(
            Box::new(ast::QualType::new(ast::Type::Char)),
            ast::ArraySize::Const(bytes.len()),
        );
        prog.decls.push(weak_static_var(
            &nm_sym,
            str_ty.clone(),
            ast::Expr::String {
                data: bytes,
                span: dummy_span(),
                ty: str_ty,
            },
        ));
        // typeinfo 对象: 槽序列由布局层单一真源给出。
        if !used_ti_kinds.contains(&abi.rtti.kind) {
            used_ti_kinds.push(abi.rtti.kind);
        }
        let mut inits: Vec<ast::InitItem> = Vec::new();
        for word in abi.rtti.typeinfo_words(ti_symbol) {
            match word {
                crate::cpp::abi_layout::TypeInfoWord::VTable(sym) => {
                    inits.push(addr_init(sym.to_string()))
                },
                crate::cpp::abi_layout::TypeInfoWord::Name(sym) => inits.push(addr_init(sym)),
                crate::cpp::abi_layout::TypeInfoWord::BaseTypeInfo(sym) => {
                    if !sym.is_empty() {
                        rtti_refs.push(sym.clone());
                    }
                    inits.push(addr_init(sym));
                },
                crate::cpp::abi_layout::TypeInfoWord::Const(v) => {
                    inits.push(ast::InitItem::Expr(ast::Expr::Int {
                        v: v as i128,
                        span: dummy_span(),
                        ty: ast::Type::LongLong,
                    }))
                },
            }
        }
        let ti_arr = ast::Type::Array(
            Box::new(ast::QualType::new(slot_ty.clone())),
            ast::ArraySize::Const(inits.len().max(1)),
        );
        prog.decls.push(weak_static_var(
            &ti_sym,
            ti_arr.clone(),
            ast::Expr::InitList {
                inits,
                span: dummy_span(),
                ty: ti_arr,
            },
        ));
    }

    // `type_info` 虚表: 每种被用到的具体形态发一张 weak 虚表, 9 个槽逐个指向
    // libstdc++ 导出的真实实现 (`__do_dyncast` / `__do_catch` / `__do_upcast` ...)。
    //
    // 为何用 dcc 私有符号名而不是标准的 `_ZTVN10__cxxabiv1...`: 实测把同名 weak
    // 定义放进可执行文件会 **interpose** 掉 libstdc++ 的表 (整个进程的 type_info
    // 对象 vptr 都会指向可执行文件里那份), 一旦槽值有偏差就会连带弄坏
    // `catch (std::exception&)` 等标准异常匹配。私有名 + 标准槽序 + 引用标准实现,
    // 既让 dcc 产物自洽, 又与 libstdc++ 零冲突。
    for kind in used_ti_kinds {
        let vt_sym = kind.vtable_symbol();
        if emitted_rtti.contains(vt_sym) {
            continue;
        }
        emitted_rtti.insert(vt_sym.to_string());
        rtti_defs.insert(vt_sym.to_string());
        let mut slot_inits: Vec<ast::InitItem> = Vec::new();
        for slot in kind.slot_symbols() {
            rtti_refs.push(slot.to_string());
            slot_inits.push(addr_init(slot.to_string()));
        }
        let vt_arr_ty = ast::Type::Array(
            Box::new(ast::QualType::new(slot_ty.clone())),
            ast::ArraySize::Const(slot_inits.len()),
        );
        prog.decls.push(weak_static_var(
            vt_sym,
            vt_arr_ty.clone(),
            ast::Expr::InitList {
                inits: slot_inits,
                span: dummy_span(),
                ty: vt_arr_ty,
            },
        ));
    }

    // 上面引用到的外部符号: 未被本 TU 定义的逐个补一条 weak 声明。
    rtti_refs.sort();
    rtti_refs.dedup();
    for sym in rtti_refs {
        if emitted_rtti.contains(&sym) || rtti_defs.contains(&sym) {
            continue;
        }
        emitted_rtti.insert(sym.clone());
        prog.decls.push(ast::Decl::Var(ast::VarDecl {
            name: ast::Ident {
                name: sym.clone(),
                span: dummy_span(),
            },
            ty: ast::Type::Ptr(Box::new(ast::QualType::new(ast::Type::UChar))),
            init: None,
            span: dummy_span(),
            is_extern: true,
            is_static: false,
            section: None,
            alignment: None,
            is_weak: true,
            visibility: None,
            is_unused: false,
            register_name: None,
            cleanup_fn: None,
            is_thread_local: false,
            no_sanitize: Vec::new(),
            alias_name: None,
            copy_fn: None,
            is_retain: false,
            is_used_attr: false,
            quals: ast::QualifierFlags::NONE,
            addr_space: ast::AddrSpace::None,
            tls_model: None,
            asm_label: None,
        }));
    }

    // 兜底: `rodata` 里出现但尚未发射定义的 typeinfo 符号 (布局未登记的类)
    // 仍发成 weak **声明**: 多 TU 下可被其它 TU 的 weak 定义合并,
    // 不会引入 strong 引用导致 multiple definition / 无法解析。
    for sym in &output.rodata {
        if !(sym.starts_with("_ZTI") || sym.starts_with("_ZTS")) {
            continue;
        }
        if emitted_rtti.contains(sym.as_str()) {
            continue;
        }
        emitted_rtti.insert(sym.clone());
        prog.decls.push(ast::Decl::Var(ast::VarDecl {
            name: ast::Ident {
                name: sym.clone(),
                span: dummy_span(),
            },
            ty: ast::Type::Ptr(Box::new(ast::QualType::new(ast::Type::UChar))),
            init: None,
            span: dummy_span(),
            is_extern: true,
            is_static: false,
            section: None,
            alignment: None,
            is_weak: true,
            visibility: None,
            is_unused: false,
            register_name: None,
            cleanup_fn: None,
            is_thread_local: false,
            no_sanitize: Vec::new(),
            alias_name: None,
            copy_fn: None,
            is_retain: false,
            is_used_attr: false,
            quals: ast::QualifierFlags::NONE,
            addr_space: ast::AddrSpace::None,
            tls_model: None,
            asm_label: None,
        }));
    }

    // 发射外部函数符号声明（operator new/delete `_Znwm`/`_ZdlPv`、`__cxa_*`、
    // `dccpp_*` runtime glue 等）。这些名字**全部是函数**，必须发成函数**声明**。
    //
    // 历史实现发成 `extern unsigned char sym;` 的 `Decl::Var` 占位，后果是:
    //   * de-SSA 后 `Operand::FnAddr(n)` 变成 `Expr::Ident{n}`，而同名 `Decl::Var`
    //     使 C 前端把 callee 判为“函数指针变量”，生成
    //     `mov sym(%rip),%rax; call *%r11` 的 load + 间接调用 —— 读到的不是函数
    //     地址而是函数体前 8 字节，跳转即 SIGSEGV;
    //   * 目标文件里同时出现 `OBJECT UND` 与 `FUNC DEFINED` 同名符号，污染链接。
    //
    // 原型优先级: 符号表里已有的声明 (stdlib 头 `extern "C"` 原型 / 用户声明)
    //   → 查不到时用 `void* f(...)` 变参兜底原型 (x86-64 SysV 下整型/指针参数
    //   位置与返回值寄存器完全一致)。不按任何夹具/符号名特判。
    let emitted_funcs: HashSet<String> = output.functions.iter().map(|f| f.name.clone()).collect();
    for sym in &output.external_symbols {
        if emitted_funcs.contains(sym.as_str()) {
            // 本 TU 已经发射过同名函数，不重复声明
            continue;
        }
        prog.decls.push(build_extern_func_decl(sym, symbols));
    }
    let _ = hir; // 当前未直接读 (已通过 lower_ir 用过), 保留参数便于扩展
    Ok(prog)
}

/// 与 lower_ir::lower_class_type 一致的字段展平顺序.
///
/// 展平规则本身不在此重算: 直接查询布局层 `ClassAbi::flatten_fields`
/// (含虚基共享子对象的静态偏移与 `__vb_` 句柄字段), 本函数只补类型。
fn collect_flat_struct_fields(
    cid: ClassId,
    abi: &ClassAbi,
    abi_map: &HashMap<ClassId, ClassAbi>,
    symbols: &[CxxSymbol],
) -> Result<Vec<ast::Field>, NativeError> {
    let mut out = Vec::new();
    for fl in abi.flatten_fields(abi_map) {
        let field_ty =
            lookup_field_type(fl.owner, &fl.name, symbols, abi_map).unwrap_or(ast::Type::Int);
        out.push(ast::Field {
            name: fl.name.clone(),
            ty: field_ty,
            offset: fl.offset as usize,
            bit_width: None,
            bit_offset: 0,
            quals: ast::QualifierFlags::NONE,
            ptr_quals: Vec::new(),
            field_align: None,
            field_packed: false,
        });
    }
    let _ = cid;
    Ok(out)
}

/// 在符号表里查字段类型 (owner == cid, name == f.name).
fn lookup_field_type(
    cid: ClassId,
    name: &str,
    symbols: &[CxxSymbol],
    abi_map: &HashMap<ClassId, ClassAbi>,
) -> Option<ast::Type> {
    let owner_sym = cid.as_symbol();
    symbols
        .iter()
        .find(|s| s.owner == Some(owner_sym) && s.name == name)
        .and_then(|s| cxx_ty_to_ast_ty(&s.ty, abi_map))
}

/// CxxType -> ast::Type 的最小子集映射 (与 lower_ir::lower_type 保持一致).
/// 失败时返回 None, 让 fallback 兜底.
///
/// class 字段类型必须借助已算好的布局表 (`abi_map`) 才能降级成 `ast::Type::Struct`,
/// 否则 `lower_type` 会因查不到 ABI 而失败, 字段被静默退化成 `Int`。
fn cxx_ty_to_ast_ty(ty: &CxxType, abi_map: &HashMap<ClassId, ClassAbi>) -> Option<ast::Type> {
    // 复用 lower_ir 的 lower_type: 构造一个临时 lowerer 并注册全部类布局。
    let hir = HirTranslationUnit::default();
    let symbols: Vec<CxxSymbol> = Vec::new();
    let mut lowerer = IrLowerer::new(&hir, &symbols);
    for (cid, abi) in abi_map {
        lowerer.register_abi(*cid, abi.clone());
    }
    lowerer.lower_type(ty).ok()
}

// === FuncDecl 组装 ===

fn build_func_decl(
    func: &ssa::Function,
    member_fns: &HashSet<String>,
) -> Result<ast::Decl, NativeError> {
    let name = func.name.clone();
    let is_member = member_fns.contains(&name);
    let ret_ty = func.ret_ty.clone();

    // SSA -> AST: 重建参数列表
    let mut params = Vec::new();
    for p in &func.params {
        params.push(ast::Param {
            ty: p.ty.clone(),
            name: Some(ast::Ident {
                name: p.base.clone(),
                span: dummy_span(),
            }),
            span: dummy_span(),
            quals: ast::QualifierFlags::NONE,
            vla_dim: None,
        });
    }

    // SSA -> AST: 函数体
    let body = ssa::rebuild_block(func);
    if std::env::var("DCC_DUMP_CPP_AST").is_ok() {
        eprintln!(
            "=== rebuilt {} ===\n{}\n--- AST ---\n{:?}",
            name,
            crate::ssa::dump_function(func),
            body
        );
    }

    // ast::FuncDecl 没有 Default, 需逐字段构造.
    // Stage 3 仅生成最小必要字段; 其他属性使用默认值.
    let func_decl = ast::FuncDecl {
        ret_ty,
        name: ast::Ident {
            name,
            span: dummy_span(),
        },
        params,
        body: Some(body),
        span: dummy_span(),
        is_extern: false,
        is_static: false,
        section: None,
        alignment: None,
        // 类内定义的成员函数 = 隐式 inline: 发成 weak, 允许多 TU 重复定义。
        is_weak: is_member,
        visibility: None,
        is_unused: false,
        is_cold: func.is_cold,
        is_hot: func.is_hot,
        is_noinline: false,
        is_always_inline: false,
        is_inline: false,
        is_variadic: false,
        is_constructor: false,
        is_destructor: false,
        alias_name: None,
        is_nested: false,
        upvars: Vec::new(),
        is_old_style: false,
        format_kind: None,
        format_string_idx: None,
        format_first_arg: None,
        no_sanitize: Vec::new(),
        target_clones: Vec::new(),
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
        is_global: true,
        is_device: false,
        is_host: false,
        is_naked: false,
        is_noreturn: func.is_noreturn,
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
    Ok(ast::Decl::Func(func_decl))
}

fn dummy_span() -> Span {
    Span::point("<native>", 1, 1)
}

/// 发射一个 weak 定义的全局静态对象 (vtable / typeinfo / 名字字符串)。
///
/// Itanium ABI 把 vtable / typeinfo 作为 COMDAT(weak) 对象发射: 多个 TU
/// 包含同一类定义时链接器只保留一份, 不会 multiple definition。
fn weak_static_var(name: &str, ty: ast::Type, init: ast::Expr) -> ast::Decl {
    ast::Decl::Var(ast::VarDecl {
        name: ast::Ident {
            name: name.to_string(),
            span: dummy_span(),
        },
        ty,
        init: Some(init),
        span: dummy_span(),
        is_extern: false,
        is_static: false,
        section: None,
        alignment: Some(8),
        is_weak: true,
        visibility: None,
        is_unused: false,
        register_name: None,
        cleanup_fn: None,
        is_thread_local: false,
        no_sanitize: Vec::new(),
        alias_name: None,
        copy_fn: None,
        is_retain: false,
        is_used_attr: false,
        quals: ast::QualifierFlags::NONE,
        addr_space: ast::AddrSpace::None,
        tls_model: None,
        asm_label: None,
    })
}

/// M54: 把一条全局变量的 C++ 初始化式转成 C 后端可接受的**静态初始化器**。
///
/// 只覆盖**链接期可表示**的常量形态：整/浮点/字符串字面量、`&具名全局对象`、
/// `&arr[k]` / `arr + k`、数组名到指针的衰减、大括号聚合初值（数组/结构体逐元素
/// 递归）。parser 已把 constexpr 函数调用等常量表达式折成字面量，因此这里不再做
/// 求值 —— 折不成的形态一律返回 `None`，调用方保持零初始化（不猜值）。
/// 动态初始化（需 `.init_array`）与类构造器调用仍属未建模项。
fn cpp_static_initializer(e: &CppExpr, ty: &ast::Type, symbols: &[CxxSymbol]) -> Option<ast::Expr> {
    let span = dummy_span();
    match e {
        CppExpr::IntLit(v) => Some(ast::Expr::Int {
            v: *v as i128,
            span,
            ty: ty.clone(),
        }),
        CppExpr::FloatLit(text) => {
            let fc = parse_float_initializer(text, ty)?;
            Some(ast::Expr::Float {
                c: fc,
                span,
                ty: ty.clone(),
            })
        },
        CppExpr::StrLit(s) => Some(string_literal_init_expr(std::slice::from_ref(s))),
        // 相邻字符串字面量拼接 (`const char *s = "a" "b";`) 与单个字面量同义。
        CppExpr::StrConcat(parts) => Some(string_literal_init_expr(parts)),
        CppExpr::InitList(items) | CppExpr::TypedInitList { elems: items, .. } => {
            let mut inits = Vec::with_capacity(items.len());
            for (i, it) in items.iter().enumerate() {
                let elem_ty = aggregate_element_ty(ty, i).unwrap_or(ast::Type::Int);
                inits.push(ast::InitItem::Expr(cpp_static_initializer(it, &elem_ty, symbols)?));
            }
            Some(ast::Expr::InitList {
                inits,
                span,
                ty: ty.clone(),
            })
        },
        CppExpr::AddrOf(inner) => static_addr_initializer(inner, ty, symbols),
        // 初值上的常量转换：去掉 cast 节点后形状不变（宽度由对象类型决定），
        // 因此可逐层剥开。其余形态（函数调用 / 成员访问 …）不建模。
        CppExpr::Cast(_, inner) | CppExpr::StaticCast { expr: inner, .. } => {
            cpp_static_initializer(inner, ty, symbols)
        },
        // M54: 数组名衰减成指针 (`int *q = garr;`) —— 词法上初值只是一个 `Ident`,
        // 能不能当地址用取决于被引用对象是不是**数组**：标量对象的“值”不是链接期
        // 常量, 必须落到调用方的不支持路径 (绝不静默零初始化)。
        CppExpr::Ident(name) => {
            if !named_object_is_array(name, symbols) {
                return None;
            }
            static_addr_from_base(name, 0, ty, symbols)
        },
        // M54: 指针常量算术 (`int *r = garr + 2;` / `arr + 2` / `2 + arr`)，偏移以
        // 元素为单位, 折成 `arr + 常量` 的地址初值交回 `static_addr_from_base`。
        CppExpr::BinOp(crate::cpp::parser::BinOpKind::Add, l, r) => {
            let (base, off) = match (l.as_ref(), r.as_ref()) {
                (CppExpr::Ident(n), CppExpr::IntLit(k)) => (n.as_str(), *k),
                (CppExpr::IntLit(k), CppExpr::Ident(n)) => (n.as_str(), *k),
                _ => return None,
            };
            if !named_object_is_array(base, symbols) {
                return None;
            }
            static_addr_from_base(base, off, ty, symbols)
        },
        // M54: 负浮点常量 (`double nd = -2.25;`) —— 词法上的 `FloatLit` 不带
        // 符号, `-2.25` 是 `UnaryOp(Neg, FloatLit)`, 整型那一侧由 parser 的常量
        // 折叠处理, 浮点这一侧只能在这里折进常数位模式; 不接就整个初值判为
        // 不可表示 (宁可报错也绝不静默归零)。
        CppExpr::UnaryOp(crate::cpp::parser::UnaryOpKind::Neg, inner) => {
            let fc = match inner.as_ref() {
                CppExpr::FloatLit(text) => parse_float_initializer(text, ty)?,
                _ => return None,
            };
            let negated = match fc {
                ast::FloatConst::F32(v) => ast::FloatConst::F32(-v),
                ast::FloatConst::F64(v) => ast::FloatConst::F64(-v),
                ast::FloatConst::LongDouble(b) => {
                    ast::FloatConst::LongDouble(crate::float80::negate(b))
                },
            };
            Some(ast::Expr::Float {
                c: negated,
                span,
                ty: ty.clone(),
            })
        },
        _ => None,
    }
}

/// 把**一个或多个**（拼接的）字符串字面量源文本解码成带 `\0` 的初值字节，
/// 并包成 `Expr::String`。
///
/// `StrLit` 存的是**已去引号、未解转义**的源文本；静态初值要的是真实字节
/// （与 C 前端一致，末尾补 `\0`）。指针初值与数组初值共用这一份字节真源。
fn string_literal_init_expr(parts: &[String]) -> ast::Expr {
    let span = dummy_span();
    let mut data = Vec::new();
    for p in parts {
        data.extend_from_slice(&crate::cpp::const_text::decode_string_escapes(p));
    }
    data.push(0);
    ast::Expr::String {
        data,
        span,
        ty: ast::Type::Ptr(Box::new(ast::QualType::new(ast::Type::Char))),
    }
}

/// `&<基对象>` 形态的静态地址初值：`&x` / `&arr[k]` / `&*p`。
///
/// 与顶层的数组衰减分支共用同一份“基对象 + 元素偏移”发射逻辑（偏移以**元素**
/// 为单位，链接期由 `R_X86_64_64` 的 addend 承担字节偏移）。
fn static_addr_initializer(
    inner: &CppExpr,
    ty: &ast::Type,
    symbols: &[CxxSymbol],
) -> Option<ast::Expr> {
    match inner {
        CppExpr::Ident(name) => static_addr_from_base(name, 0, ty, symbols),
        // `&arr[k]`：下标只接常量偏移，否则不是链接期可表示量。
        CppExpr::BinOp(crate::cpp::parser::BinOpKind::Index, base, idx) => {
            let (CppExpr::Ident(name), CppExpr::IntLit(k)) = (base.as_ref(), idx.as_ref()) else {
                return None;
            };
            static_addr_from_base(name, *k, ty, symbols)
        },
        // `&*p` 与 `p` 同值（p 本身必须是链接期地址，否则不接）。
        CppExpr::UnaryOp(crate::cpp::parser::UnaryOpKind::Deref, sub) => {
            static_addr_initializer(sub, ty, symbols)
        },
        _ => None,
    }
}

/// 以具名对象 `base` 为基、向后偏移 `off` 个元素，发射 `Expr::AddrOf` 初值。
///
/// 目标类型 `ty` 必须是指针（初值类型就是指针），其指向类型当作元素宽度真源：
/// `ast::Type::Ptr(elem)` -> `&base[off]` 的字节偏移 = `off * sizeof(elem)`。
fn static_addr_from_base(
    base: &str,
    off: i64,
    ty: &ast::Type,
    symbols: &[CxxSymbol],
) -> Option<ast::Expr> {
    let span = dummy_span();
    let link = global_variable_link_name(symbols, base)?;
    let pointee = match ty {
        ast::Type::Ptr(q) => q.base.clone(),
        _ => ast::Type::Void,
    };
    let ident = ast::Expr::Ident {
        name: ast::Ident {
            name: link,
            span: span.clone(),
        },
        span: span.clone(),
        ty: pointee.clone(),
    };
    let addressed = if off == 0 {
        ident
    } else {
        ast::Expr::Index {
            arr: Box::new(ident),
            idx: Box::new(ast::Expr::Int {
                v: i128::from(off),
                span: span.clone(),
                ty: ast::Type::Int,
            }),
            span: span.clone(),
            ty: pointee,
        }
    };
    Some(ast::Expr::AddrOf {
        e: Box::new(addressed),
        span,
        ty: ty.clone(),
    })
}

/// 名字引用的全局对象是否数组类型（数组名在初值里衰减为首元素地址）。
///
/// 解析规则与链接名取值共用 [`resolve_global_var`] 这一份真源：先按名字定位到
/// **唯一**符号，再看它的类型，避免拿另一个命名空间里同名对象的数组性来配本
/// 作用域对象的地址。
fn named_object_is_array(name: &str, symbols: &[CxxSymbol]) -> bool {
    matches!(resolve_global_var(symbols, name), Some(s) if matches!(s.ty, CxxType::Array(..)))
}

/// 聚合类型（`ty` 是数组/结构体/联合体）的第 `i` 个位置元素类型。
fn aggregate_element_ty(ty: &ast::Type, i: usize) -> Option<ast::Type> {
    match ty {
        ast::Type::Array(elem, _) => Some(elem.base.clone()),
        ast::Type::Struct(_, fields, _, _) | ast::Type::Union(_, fields, _, _) => {
            fields.get(i).map(|f| f.ty.clone())
        },
        _ => Some(ty.clone()),
    }
}

/// 静态初始化里的浮点字面量文本 -> `FloatConst`。
///
/// 解析失败一律返回 `None`（不发初值），绝不拿 `0.0` 当成真值。
fn parse_float_initializer(text: &str, ty: &ast::Type) -> Option<ast::FloatConst> {
    let trimmed = text.trim_end_matches(['f', 'F', 'l', 'L', 'u', 'U']);
    // `0x1.8p3` 这类十六进制浮点 `parse::<f64>()` 不支持：不支持则不发。
    if trimmed.contains("0x") || trimmed.contains("0X") {
        return None;
    }
    match ty {
        ast::Type::Float => trimmed.parse::<f32>().ok().map(ast::FloatConst::F32),
        _ => trimmed.parse::<f64>().ok().map(ast::FloatConst::F64),
    }
}

/// 按源名（`ga` / 命名空间变量的末段名）解析出**唯一**的全局变量符号。
///
/// 链接名规则的真源仍是 sema 的 `compute_linkage_name`（命名空间变量是 `ns_a`），
/// 本函数只**读**符号表结果，不再复刻那份前缀拼接 —— 避免第二套命名规则。
/// 名字在多个命名空间里重叠时视为歧义，返回 `None`。名字解析与类型判定必须
/// 共用这一份真源，否则 `int *p = arr;` 可能拿**另一个**同名对象的地址。
fn resolve_global_var<'a>(symbols: &'a [CxxSymbol], name: &str) -> Option<&'a CxxSymbol> {
    let is_var = |s: &CxxSymbol| matches!(s.kind, CxxSymbolKind::Variable);
    // 链接名本身就等于所写名字（文件作用域变量）时直接命中。
    if let Some(s) = symbols
        .iter()
        .find(|s| is_var(s) && s.linkage_name.as_deref() == Some(name))
    {
        return Some(s);
    }
    let mut cands = symbols.iter().filter(|s| is_var(s) && s.name == name);
    let first = cands.next()?;
    // 多个不同链接名的同名变量：不知道指哪一个 -> 不折叠。
    if cands.any(|s| s.linkage_name != first.linkage_name) {
        return None;
    }
    Some(first)
}

/// 按源名取其**发射用链接名**（[`resolve_global_var`] 的薄封装）。
fn global_variable_link_name(symbols: &[CxxSymbol], name: &str) -> Option<String> {
    resolve_global_var(symbols, name)?.linkage_name.clone()
}

/// 外部函数声明参数的“ABI 安全”类型映射。
///
/// 指针/引用/数组/类类型统一按 `void*` 声明: 在 x86-64 SysV 下它们与整型参数
/// 一样走 GP 寄存器，宽度相同，因此既不改变调用序列，也不依赖尚未注册的类 ABI
/// (避开 `cxx_ty_to_ast_ty` 对 Class 类型会失败的问题)。
fn extern_param_ast_type(ty: &CxxType) -> ast::Type {
    match ty {
        CxxType::Bool => ast::Type::Bool,
        CxxType::Integer { bits, signed } => match (bits, signed) {
            (8, true) => ast::Type::SChar,
            (8, false) => ast::Type::UChar,
            (16, true) => ast::Type::Short,
            (16, false) => ast::Type::UShort,
            (32, true) => ast::Type::Int,
            (32, false) => ast::Type::UInt,
            (64, true) => ast::Type::Long,
            (64, false) => ast::Type::ULong,
            _ => ast::Type::Long,
        },
        CxxType::Float { bits: 32 } => ast::Type::Float,
        CxxType::Float { .. } => ast::Type::Double,
        _ => ast::Type::Ptr(Box::new(ast::QualType::new(ast::Type::Void))),
    }
}

/// `ast::FuncDecl` 未派生 `Default`, 提供一份全中性值的模板给函数声明使用
/// (字段展开走 functional update, 避免重复 60 行字面量)。
fn blank_func_decl() -> ast::FuncDecl {
    ast::FuncDecl {
        ret_ty: ast::Type::Void,
        name: ast::Ident {
            name: String::new(),
            span: dummy_span(),
        },
        params: Vec::new(),
        body: None,
        span: dummy_span(),
        is_extern: true,
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
        upvars: Vec::new(),
        is_old_style: false,
        format_kind: None,
        format_string_idx: None,
        format_first_arg: None,
        no_sanitize: Vec::new(),
        target_clones: Vec::new(),
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
        is_global: true,
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
    }
}

/// 为一个外部符号构造 C 链接的**函数声明** (`body: None`)。
///
/// 原型从符号表 (单一真源) 取；取不到时用 `void* f(...)` 变参兜底原型，
/// 保证 callee 被识别为函数而得到直接 `call`，而不是函数指针间接调用。
fn build_extern_func_decl(name: &str, symbols: &[CxxSymbol]) -> ast::Decl {
    use crate::cpp::builtins::{self, BuiltinTy};

    // 1) 符号表里的显式声明 (stdlib 头 extern "C" 原型 / 用户声明 / 内置表)
    //    成员函数名在符号表里是裸方法名, 链接名是 `<Class>_<method>`:
    //    两侧都要能命中, 否则外部成员函数声明会退化成 `void* f(...)`。
    let proto = symbols.iter().find(|s| {
        matches!(
            s.kind,
            CxxSymbolKind::Function | CxxSymbolKind::Method | CxxSymbolKind::Destructor
        ) && (s.name == name
            || s.linkage_name.as_deref() == Some(name)
            || symbol_link_name(symbols, s) == name)
    });
    let (ret_ty, params) = match proto {
        Some(s) => match &s.ty {
            CxxType::Function { ret, params, .. } => {
                let r = if matches!(**ret, CxxType::Void) {
                    ast::Type::Void
                } else {
                    extern_param_ast_type(ret)
                };
                (r, params.iter().map(extern_param_ast_type).collect::<Vec<_>>())
            },
            _ => (ast::Type::Void, Vec::new()),
        },
        // 2) 内置 libc 函数表 (memcpy/malloc/... 单一真源)
        None if builtins::is_builtin(name) => {
            let bf = builtins::lookup(name).expect("is_builtin");
            let conv = |t: BuiltinTy| extern_param_ast_type(&t.to_cxx());
            (conv(bf.ret), bf.params.iter().copied().map(conv).collect::<Vec<_>>())
        },
        // 3) 兜底: `void* f(...)`
        None => (ast::Type::Ptr(Box::new(ast::QualType::new(ast::Type::Void))), Vec::new()),
    };
    let is_variadic = params.is_empty();
    let ast_params: Vec<ast::Param> = params
        .iter()
        .map(|t| ast::Param {
            ty: t.clone(),
            name: None,
            span: dummy_span(),
            quals: ast::QualifierFlags::NONE,
            vla_dim: None,
        })
        .collect();
    let func_decl = ast::FuncDecl {
        ret_ty,
        name: ast::Ident {
            name: name.to_string(),
            span: dummy_span(),
        },
        params: ast_params,
        body: None,
        span: dummy_span(),
        is_extern: true,
        is_static: false,
        is_variadic,
        is_old_style: false,
        ..blank_func_decl()
    };
    ast::Decl::Func(func_decl)
}

// === Feature detection (Stage 10) ===

/// 递归扫描声明列表，检测使用到的所有 C++ 特性。
/// 用于 auto-mode 下判断是否可以走 native path，以及输出诊断信息。
pub fn detect_features(decls: &[CppDecl]) -> HashSet<CppFeature> {
    let mut features = HashSet::new();
    for d in decls {
        detect_features_from_decl(d, &mut features);
    }
    features
}

/// 从单个声明中检测特性
fn detect_features_from_decl(d: &CppDecl, features: &mut HashSet<CppFeature>) {
    match d {
        CppDecl::FreeFn {
            ret, params, body, ..
        } => {
            features.insert(CppFeature::FreeFunction);
            detect_features_from_type(ret, features);
            for (_, t) in params {
                detect_features_from_type(t, features);
            }
            detect_features_from_body(body, features);
        },
        CppDecl::MemberFn {
            ret,
            params,
            body,
            is_virtual,
            ..
        } => {
            features.insert(CppFeature::MemberFunction);
            if *is_virtual {
                features.insert(CppFeature::VirtualFunction);
                features.insert(CppFeature::VPtrInit);
            }
            detect_features_from_type(ret, features);
            for (_, t) in params {
                detect_features_from_type(t, features);
            }
            detect_features_from_body(body, features);
        },
        CppDecl::Ctor { params, body, .. } => {
            features.insert(CppFeature::CtorDtor);
            for (_, t) in params {
                detect_features_from_type(t, features);
            }
            detect_features_from_body(body, features);
        },
        CppDecl::Dtor {
            body, is_virtual, ..
        } => {
            features.insert(CppFeature::CtorDtor);
            if *is_virtual {
                features.insert(CppFeature::VirtualFunction);
                features.insert(CppFeature::VPtrInit);
            }
            detect_features_from_body(body, features);
        },
        CppDecl::Class(c) => {
            features.insert(CppFeature::PlainClass);
            // 检查继承
            let has_base = !c.bases.is_empty();
            let has_virtual_base = c.bases.iter().any(|b| b.is_virtual);
            if has_virtual_base {
                features.insert(CppFeature::VirtualInheritance);
                // diamond 检测：只有当存在被多路径共享的虚基时才标记
                // 仅检查本层：若同一虚基类在 c.bases 中出现 ≥2 次，直接视为 diamond
                let mut vb_count: std::collections::HashMap<&str, usize> =
                    std::collections::HashMap::new();
                for b in &c.bases {
                    if b.is_virtual {
                        *vb_count.entry(b.name.as_str()).or_insert(0) += 1;
                    }
                }
                let has_diamond_in_self = vb_count.values().any(|&n| n >= 2);
                // 跨层 diamond 需要完整继承图分析；此处保守只在本层重复时标记
                // 跨层 diamond 由 can_compile_native 进一步判断
                if has_diamond_in_self {
                    features.insert(CppFeature::DiamondVirtualInheritance);
                }
            }
            if has_base {
                features.insert(CppFeature::NonVirtualInheritance);
                features.insert(CppFeature::BaseCtorOrder);
            }
            // 检查成员
            for m in &c.members {
                detect_features_from_decl(m, features);
            }
            // 检查是否有虚函数（用于 vptr 初始化）
            let has_virtual_method = c.members.iter().any(|m| {
                matches!(
                    m,
                    CppDecl::MemberFn {
                        is_virtual: true,
                        ..
                    } | CppDecl::Dtor {
                        is_virtual: true,
                        ..
                    }
                )
            });
            if has_virtual_method {
                features.insert(CppFeature::ThisAdjustThunk);
            }
        },
        CppDecl::Namespace { body, .. } => {
            features.insert(CppFeature::Namespace);
            for inner in body {
                detect_features_from_decl(inner, features);
            }
        },
        CppDecl::Using { .. } | CppDecl::UsingEnum { .. } => {
            features.insert(CppFeature::Namespace);
        },
        CppDecl::OperatorFn {
            body, ret, params, ..
        } => {
            features.insert(CppFeature::OperatorOverload);
            detect_features_from_type(ret, features);
            for (_, t) in params {
                detect_features_from_type(t, features);
            }
            detect_features_from_body(body, features);
        },
        CppDecl::Template { decl, .. } => {
            // 根据内部声明类型判断是函数模板还是类模板
            match &**decl {
                CppDecl::Class(_) => {
                    features.insert(CppFeature::ClassTemplate);
                },
                CppDecl::FreeFn { .. } | CppDecl::MemberFn { .. } => {
                    features.insert(CppFeature::FunctionTemplate);
                },
                _ => {},
            }
            detect_features_from_decl(decl, features);
        },
        CppDecl::PartialSpec { .. } => {
            features.insert(CppFeature::PartialSpecialization);
        },
        CppDecl::Concept { .. } => {
            features.insert(CppFeature::Concept);
        },
        CppDecl::Module { .. } | CppDecl::Import { .. } | CppDecl::PrivateFragment => {
            features.insert(CppFeature::Module);
        },
        CppDecl::ExportBlock(body) => {
            features.insert(CppFeature::Module);
            for inner in body {
                detect_features_from_decl(inner, features);
            }
        },
        CppDecl::GlobalVar { ty, init, .. } => {
            detect_features_from_type(ty, features);
            if let Some(e) = init {
                detect_features_from_expr(e, features);
            }
        },
        CppDecl::MemberVar { ty, .. } => {
            detect_features_from_type(ty, features);
        },
        CppDecl::Typedef { target, .. } => {
            detect_features_from_type(target, features);
        },
        CppDecl::AnonAggr { members, .. } => {
            for m in members {
                detect_features_from_decl(m, features);
            }
        },
        CppDecl::InheritingCtor { .. } => {
            features.insert(CppFeature::NonVirtualInheritance);
        },
        _ => {},
    }
}

/// 从类型中检测特性
fn detect_features_from_type(ty: &MangleType, features: &mut HashSet<CppFeature>) {
    match ty {
        MangleType::Reference(_) => {
            features.insert(CppFeature::ReferenceType);
        },
        MangleType::Template { args, .. } => {
            features.insert(CppFeature::ClassTemplate);
            for a in args {
                detect_features_from_type(a, features);
            }
        },
        MangleType::Pointer(t) | MangleType::Reference(t) => {
            detect_features_from_type(t, features);
        },
        MangleType::TplMember { args, .. } => {
            features.insert(CppFeature::ClassTemplate);
            for a in args {
                detect_features_from_type(a, features);
            }
        },
        _ => {},
    }
}

/// 从函数体中检测特性
fn detect_features_from_body(body: &CppBody, features: &mut HashSet<CppFeature>) {
    for stmt in &body.stmts {
        detect_features_from_stmt(stmt, features);
    }
}

/// 从语句中检测特性
fn detect_features_from_stmt(s: &CppStmt, features: &mut HashSet<CppFeature>) {
    match s {
        CppStmt::Expr(e) | CppStmt::Return(Some(e)) => {
            detect_features_from_expr(e, features);
        },
        CppStmt::If { cond, then, els } => {
            detect_features_from_expr(cond, features);
            detect_features_from_body(then, features);
            if let Some(e) = els {
                detect_features_from_body(e, features);
            }
        },
        CppStmt::IfConstexpr { cond, then, els } => {
            detect_features_from_expr(cond, features);
            detect_features_from_body(then, features);
            if let Some(e) = els {
                detect_features_from_body(e, features);
            }
        },
        CppStmt::While { cond, body } => {
            detect_features_from_expr(cond, features);
            detect_features_from_body(body, features);
        },
        CppStmt::For {
            init,
            cond,
            step,
            body,
        } => {
            if let Some(i) = init {
                detect_features_from_stmt(i, features);
            }
            if let Some(c) = cond {
                detect_features_from_expr(c, features);
            }
            if let Some(s) = step {
                detect_features_from_stmt(s, features);
            }
            detect_features_from_body(body, features);
        },
        CppStmt::Block(b) => detect_features_from_body(b, features),
        CppStmt::ThisMember(_, stmt) => {
            detect_features_from_stmt(stmt, features);
        },
        CppStmt::LocalVar { ty, init, .. } => {
            detect_features_from_type(ty, features);
            if let Some(e) = init {
                detect_features_from_expr(e, features);
            }
        },
        CppStmt::StaticLocal { ty, init, .. } => {
            features.insert(CppFeature::StaticInitGuard);
            detect_features_from_type(ty, features);
            if let Some(e) = init {
                detect_features_from_expr(e, features);
            }
        },
        CppStmt::LocalVarList { spec, declarators } => {
            if spec.is_static {
                features.insert(CppFeature::StaticInitGuard);
            }
            detect_features_from_type(&spec.ty, features);
            for d in declarators {
                if let Some(e) = &d.init {
                    detect_features_from_expr(e, features);
                }
            }
        },
        CppStmt::Try { body, catches } => {
            features.insert(CppFeature::Exception);
            features.insert(CppFeature::DtorCleanup);
            detect_features_from_body(body, features);
            for c in catches {
                detect_features_from_body(&c.body, features);
            }
        },
        CppStmt::Throw(_) => {
            features.insert(CppFeature::Exception);
        },
        CppStmt::Switch { expr, arms, .. } => {
            detect_features_from_expr(expr, features);
            for (_, body) in arms {
                detect_features_from_body(body, features);
            }
        },
        CppStmt::CoAwait(_) | CppStmt::CoYield(_) | CppStmt::CoReturn(_) => {
            features.insert(CppFeature::Coroutine);
        },
        CppStmt::Decl(d) => detect_features_from_decl(d, features),
        _ => {},
    }
}

/// 从表达式中检测特性
fn detect_features_from_expr(e: &CppExpr, features: &mut HashSet<CppFeature>) {
    match e {
        CppExpr::Typeid(_) | CppExpr::TypeidType(_) => {
            features.insert(CppFeature::TypeId);
        },
        CppExpr::DynamicCast { expr, .. } => {
            features.insert(CppFeature::DynamicCast);
            detect_features_from_expr(expr, features);
        },
        CppExpr::New(_, _)
        | CppExpr::NewArr { .. }
        | CppExpr::PlacementNew { .. }
        | CppExpr::Delete(_)
        | CppExpr::DeleteArr(_) => {
            features.insert(CppFeature::NewDelete);
        },
        CppExpr::InitList(_) | CppExpr::TypedInitList { .. } => {
            features.insert(CppFeature::InitializerList);
        },
        CppExpr::Call(f, args) => {
            detect_features_from_expr(f, features);
            for a in args {
                detect_features_from_expr(a, features);
            }
        },
        CppExpr::BinOp(_, l, r) => {
            detect_features_from_expr(l, features);
            detect_features_from_expr(r, features);
        },
        CppExpr::UnaryOp(_, x) | CppExpr::Ref(x) | CppExpr::AddrOf(x) => {
            detect_features_from_expr(x, features);
        },
        CppExpr::Member(o, _)
        | CppExpr::PtrMember(o, _)
        | CppExpr::MemberStar(o, _)
        | CppExpr::PtrMemberStar(o, _) => {
            detect_features_from_expr(o, features);
        },
        CppExpr::Cast(ty, inner)
        | CppExpr::StaticCast { ty, expr: inner }
        | CppExpr::ConstCast { ty, expr: inner }
        | CppExpr::ReinterpretCast { ty, expr: inner } => {
            detect_features_from_type(ty, features);
            detect_features_from_expr(inner, features);
        },
        CppExpr::TemplateIdent { args, .. } => {
            features.insert(CppFeature::FunctionTemplate);
            for a in args {
                detect_features_from_type(a, features);
            }
        },
        CppExpr::TplStaticAccess { args, .. } => {
            features.insert(CppFeature::ClassTemplate);
            for a in args {
                detect_features_from_type(a, features);
            }
        },
        CppExpr::OpCall { args, .. } => {
            features.insert(CppFeature::OperatorOverload);
            for a in args {
                detect_features_from_expr(a, features);
            }
        },
        CppExpr::Ternary { cond, then, els } => {
            detect_features_from_expr(cond, features);
            detect_features_from_expr(then, features);
            detect_features_from_expr(els, features);
        },
        CppExpr::Comma(elems) => {
            for e in elems {
                detect_features_from_expr(e, features);
            }
        },
        CppExpr::MultiIndex(obj, idxs) => {
            detect_features_from_expr(obj, features);
            for i in idxs {
                detect_features_from_expr(i, features);
            }
        },
        CppExpr::Lambda(_) => {
            // lambda 暂不单独作为 feature，降级为普通函数处理
        },
        // `sizeof` 只有类型形态需要扫特性: `sizeof...(包)` 不含类型,
        // `sizeof(表达式)` 的表达式也不在此递归。
        CppExpr::Sizeof(crate::cpp::parser::SizeofArg::Type(t)) => {
            detect_features_from_type(t, features);
        },
        _ => {},
    }
}

/// 检查声明列表是否可以全部走 native path。
/// 返回 Ok(()) 表示所有使用到的特性都已支持，
/// 返回 Err(Vec<CppFeature>) 列出不支持的特性。
pub fn can_compile_native(decls: &[CppDecl]) -> Result<(), Vec<CppFeature>> {
    let features = detect_features(decls);
    let mut unsupported: Vec<CppFeature> = features
        .iter()
        .copied()
        .filter(|f| !f.native_supported())
        .collect();
    // 按 name 排序，保证输出稳定
    unsupported.sort_by_key(|f| f.name());
    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(unsupported)
    }
}

// === Feature gating 辅助 ===

/// 检查一组声明是否全部走 native path 可行 (auto-mode 用作 fallback 决策).
/// 不支持特性 (虚函数/模板/异常/...) 任一出现 -> 返回 false.
pub fn native_supported(decls: &[CppDecl]) -> Result<(), NativeError> {
    // Stage 10: 复用 can_compile_native 的检测结果
    match can_compile_native(decls) {
        Ok(()) => Ok(()),
        Err(unsupported) => {
            let names: Vec<&str> = unsupported.iter().map(|f| f.name()).collect();
            Err(NativeError::Unsupported(format!("unsupported features: {}", names.join(", "))))
        },
    }
}

/// 当前 native path 标识 (用于 verbose 输出)
pub fn current_path() -> CppPath {
    CppPath::Native
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp::parser::CppParser;

    #[test]
    fn lower_free_function_native() {
        let src = b"int add(int a, int b) { return a + b; }\n\
int main() { return add(1, 2); }\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let out = lower_decls_native(&decls).expect("native lower");
        // 至少应该有 add 和 main 两个函数 + 没有额外 struct
        let fn_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::Func(_)))
            .count();
        assert_eq!(fn_count, 2);
    }

    #[test]
    fn lower_plain_class_native() {
        // Stage 3 基础: class 定义 + 成员函数 + 构造函数 (框架验证)
        // 隐式 this->field 访问和 sizeof(class) 留到 Stage 4,
        // 这里只验证 struct + 函数签名走通
        let src = b"\
class Point {\n\
public:\n\
    int x;\n\
    int y;\n\
    Point() {}\n\
    int dummy() { return 42; }\n\
};\n\
int global = 0;\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let out = lower_decls_native(&decls).expect("native lower");
        // 应该有 1 个 struct (Point)
        let struct_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::StructDef(_)))
            .count();
        assert_eq!(struct_count, 1);
        // 至少 2 个函数 (ctor + dummy = 2)
        let fn_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::Func(_)))
            .count();
        assert!(fn_count >= 2, "expected at least 2 functions, got {}", fn_count);
    }

    #[test]
    fn lower_single_inheritance_native() {
        // Stage 4: 单继承 - 验证基类字段被展平到派生类 struct
        let src = b"\
class Base {\n\
public:\n\
    int b;\n\
};\n\
class Derived : public Base {\n\
public:\n\
    int d;\n\
    Derived() {}\n\
};\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有 2 个 struct (Base + Derived)
        let struct_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::StructDef(_)))
            .count();
        assert_eq!(struct_count, 2);

        // 找到 Derived 的 struct 定义，验证包含基类字段 b
        let derived_struct = out.prog.decls.iter().find_map(|d| match d {
            ast::Decl::StructDef(sd) if sd.name.name == "Derived" => Some(sd),
            _ => None,
        });
        assert!(derived_struct.is_some(), "Derived struct not found");
        let sd = derived_struct.unwrap();
        // Derived 应该有 2 个字段: b (from Base) + d (自己)
        assert_eq!(sd.fields.len(), 2);
        assert_eq!(sd.fields[0].name, "b");
        assert_eq!(sd.fields[0].offset, 0);
        assert_eq!(sd.fields[1].name, "d");
        assert_eq!(sd.fields[1].offset, 4);
    }

    #[test]
    fn lower_multiple_inheritance_native() {
        // Stage 4: 多重非虚继承 - 验证两个基类都被展平
        let src = b"\
class A {\n\
public:\n\
    int a;\n\
};\n\
class B {\n\
public:\n\
    double b;\n\
};\n\
class C : public A, public B {\n\
public:\n\
    int c;\n\
    C() {}\n\
};\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let out = lower_decls_native(&decls).expect("native lower");

        // 找到 C 的 struct 定义
        let c_struct = out.prog.decls.iter().find_map(|d| match d {
            ast::Decl::StructDef(sd) if sd.name.name == "C" => Some(sd),
            _ => None,
        });
        assert!(c_struct.is_some(), "C struct not found");
        let sd = c_struct.unwrap();
        // C 应该有 3 个字段: a (from A) + b (from B) + c (自己)
        assert_eq!(sd.fields.len(), 3);
        assert_eq!(sd.fields[0].name, "a");
        assert_eq!(sd.fields[0].offset, 0);
        assert_eq!(sd.fields[1].name, "b");
        // B 对齐到 8 字节，所以 b 的 offset 是 8
        assert_eq!(sd.fields[1].offset, 8);
        assert_eq!(sd.fields[2].name, "c");
        assert_eq!(sd.fields[2].offset, 16);
    }

    #[test]
    fn native_supported_allows_non_virtual_inheritance() {
        // Stage 4: 非虚继承应该被 native_supported 接受
        let src = b"\
class Base { int x; };\n\
class Derived : public Base { int y; };\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let result = native_supported(&decls);
        assert!(result.is_ok(), "non-virtual inheritance should be supported");
    }

    #[test]
    fn lower_virtual_class_builds_vtable() {
        let src = b"\
class Base {\n\
public:\n\
    virtual int value() { return 1; }\n\
    Base() {}\n\
};\n\
class Derived : public Base {\n\
public:\n\
    int value() override { return 2; }\n\
    Derived() {}\n\
};\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");
        assert_eq!(out.vtables.len(), 2);
        assert!(out.vtables.iter().all(|(_, entries)| entries.len() >= 3));
        assert!(out
            .vtables
            .iter()
            .flat_map(|(_, entries)| entries)
            // vtable 槽里是成员函数的**链接名** (`<Class>_<method>`,
            // 见 lower_ir::symbol_link_name), 不是源语言裸名 `value`。
            .any(|entry| matches!(entry, crate::cpp::lower_ir::VTableEntry::SymAddr(name) if name.ends_with("value"))));
    }

    #[test]
    fn native_supported_allows_virtual_inheritance() {
        // Stage 6: 虚继承应该被 native_supported 接受
        let src = b"\
class Base { int x; };\n\
class Derived : public virtual Base { int y; };\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let result = native_supported(&decls);
        assert!(result.is_ok(), "virtual inheritance should be supported in Stage 6");
    }

    #[test]
    fn lower_virtual_inheritance_native() {
        // Stage 6: 简单虚继承 - 验证类能正确 lowering
        let src = b"\
class Base {\n\
public:\n\
    virtual int value() { return 1; }\n\
    int x;\n\
    Base() {}\n\
};\n\
class Derived : public virtual Base {\n\
public:\n\
    int y;\n\
    Derived() {}\n\
};\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有 2 个 struct (Base + Derived)
        let struct_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::StructDef(_)))
            .count();
        assert_eq!(struct_count, 2);

        // 找到 Derived 的 struct 定义
        let derived_struct = out.prog.decls.iter().find_map(|d| match d {
            ast::Decl::StructDef(sd) if sd.name.name == "Derived" => Some(sd),
            _ => None,
        });
        assert!(derived_struct.is_some(), "Derived struct not found");
        let sd = derived_struct.unwrap();

        // Derived 应该包含基类字段 x (virtual base) 和自己的字段 y
        // 注意: 虚基类字段在末尾
        let has_x = sd.fields.iter().any(|f| f.name == "x");
        let has_y = sd.fields.iter().any(|f| f.name == "y");
        assert!(has_x, "Derived should have field x (from virtual Base)");
        assert!(has_y, "Derived should have field y");

        // vtable 应该有 vbase offset 条目
        assert!(!out.vtables.is_empty());
    }

    #[test]
    fn lower_diamond_virtual_inheritance_native() {
        // Stage 6: 菱形虚继承
        let src = b"\
class A {\n\
public:\n\
    virtual void foo() {}\n\
    int a;\n\
    A() {}\n\
};\n\
class B : public virtual A {\n\
public:\n\
    int b;\n\
    B() {}\n\
};\n\
class C : public virtual A {\n\
public:\n\
    int c;\n\
    C() {}\n\
};\n\
class D : public B, public C {\n\
public:\n\
    int d;\n\
    D() {}\n\
};\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有 4 个 struct (A, B, C, D)
        let struct_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::StructDef(_)))
            .count();
        assert_eq!(struct_count, 4);

        // 找到 D 的 struct 定义
        let d_struct = out.prog.decls.iter().find_map(|d| match d {
            ast::Decl::StructDef(sd) if sd.name.name == "D" => Some(sd),
            _ => None,
        });
        assert!(d_struct.is_some(), "D struct not found");
        let sd = d_struct.unwrap();

        // D 应该包含字段: b (from B), c (from C), d (自己), a (from virtual A)
        let has_a = sd.fields.iter().any(|f| f.name == "a");
        let has_b = sd.fields.iter().any(|f| f.name == "b");
        let has_c = sd.fields.iter().any(|f| f.name == "c");
        let has_d = sd.fields.iter().any(|f| f.name == "d");
        assert!(has_a, "D should have field a (from virtual A)");
        assert!(has_b, "D should have field b (from B)");
        assert!(has_c, "D should have field c (from C)");
        assert!(has_d, "D should have field d");

        // 验证 vtable 存在
        assert!(!out.vtables.is_empty());
    }

    // === Stage 7: RTTI 测试 ===

    #[test]
    fn native_supported_allows_typeid() {
        // Stage 7: typeid 应该被 native_supported 接受
        let src = b"\
class Foo { int x; };\n\
void test() { typeid(Foo); }\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let result = native_supported(&decls);
        assert!(result.is_ok(), "typeid should be supported in Stage 7");
    }

    #[test]
    fn native_supported_allows_dynamic_cast() {
        // Stage 7: dynamic_cast 应该被 native_supported 接受
        let src = b"\
class Base { virtual void foo() {} };\n\
class Derived : public Base {};\n\
void test(Base* b) { dynamic_cast<Derived*>(b); }\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let result = native_supported(&decls);
        assert!(result.is_ok(), "dynamic_cast should be supported in Stage 7");
    }

    #[test]
    fn lower_typeid_emits_typeinfo_symbol() {
        // 验证 typeid 表达式会生成 typeinfo 符号（在 rodata 中）
        let src = b"\
class Foo {\n\
public:\n\
    virtual void bar() {}\n\
    Foo() {}\n\
};\n\
void* test() {\n\
    void* p = (void*)typeid(Foo);\n\
    return p;\n\
}\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // rodata 中应该有 _ZTI 开头的 typeinfo 符号
        let has_typeinfo = out.rodata.iter().any(|s| s.starts_with("_ZTI"));
        assert!(has_typeinfo, "should have typeinfo symbol in rodata");

        // 验证程序里声明了 typeinfo 符号: 布局已登记的类会直接发射 **weak
        // 定义** (`_ZTI3Foo` 作为可合并的弱符号对象), 只有布局未登记的类才
        // 落到 `_ZTI*` extern 兜底声明。两者都能满足多 TU weak 链接。
        let has_typeinfo_decl = out.prog.decls.iter().any(|d| match d {
            ast::Decl::Var(vd) => vd.name.name.starts_with("_ZTI") && (vd.is_extern || vd.is_weak),
            _ => false,
        });
        assert!(
            has_typeinfo_decl,
            "should declare the typeinfo symbol (weak definition or extern)"
        );
    }

    #[test]
    fn lower_dynamic_cast_polymorphic() {
        // 验证多态类型的 dynamic_cast 能正确 lowering
        let src = b"\
class Base {\n\
public:\n\
    virtual int value() { return 1; }\n\
    Base() {}\n\
};\n\
class Derived : public Base {\n\
public:\n\
    int value() override { return 2; }\n\
    Derived() {}\n\
};\n\
Derived* test(Base* b) {\n\
    return dynamic_cast<Derived*>(b);\n\
}\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有函数（test + 构造函数等）
        let fn_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::Func(_)))
            .count();
        assert!(fn_count >= 1, "should have at least test function");

        // rodata 中应该有 typeinfo 符号（src 和 dst 两个）
        let typeinfo_count = out.rodata.iter().filter(|s| s.starts_with("_ZTI")).count();
        assert!(typeinfo_count >= 2, "should have at least 2 typeinfo symbols (src and dst)");
    }

    // === Stage 8: 模板测试 ===

    #[test]
    fn native_supported_allows_templates() {
        // Stage 8: 模板声明应该被 native_supported 接受
        let src = b"\
template <typename T> class Box { public: T value; };
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let result = native_supported(&decls);
        assert!(result.is_ok(), "templates should be supported in Stage 8");
    }

    #[test]
    fn lower_class_template_simple() {
        // Stage 8: 简单类模板实例化（从变量声明触发）
        let src = b"\
template <typename T>
struct Box {
    T value;
};
int test() {
    Box<int> b;
    b.value = 42;
    return b.value;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有实例化的模板类 struct
        let has_template_class = out.prog.decls.iter().any(|d| match d {
            ast::Decl::StructDef(sd) => sd.name.name.starts_with("__dcc_template_Box"),
            _ => false,
        });
        assert!(has_template_class, "should have instantiated template class");

        // 应该有 test 函数
        let has_test_fn = out.prog.decls.iter().any(|d| match d {
            ast::Decl::Func(fd) => fd.name.name == "test",
            _ => false,
        });
        assert!(has_test_fn, "should have test function");
    }

    #[test]
    fn lower_class_template_global_var() {
        // Stage 8: 从全局变量触发类模板实例化
        let src = b"\
template <typename T>
struct Pair {
    T first;
    T second;
};
Pair<int> global_pair;
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有实例化的模板类 struct
        let has_template_class = out.prog.decls.iter().any(|d| match d {
            ast::Decl::StructDef(sd) => sd.name.name.starts_with("__dcc_template_Pair"),
            _ => false,
        });
        assert!(has_template_class, "should have instantiated Pair<int> class");
    }

    #[test]
    fn lower_class_template_multiple_params() {
        // Stage 8: 多类型参数的类模板
        let src = b"\
template <typename T, typename U>
struct Tuple2 {
    T first;
    U second;
};
Tuple2<int, double> global_tuple;
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有实例化的模板类 struct
        let has_template_class = out.prog.decls.iter().any(|d| match d {
            ast::Decl::StructDef(sd) => sd.name.name.starts_with("__dcc_template_Tuple2"),
            _ => false,
        });
        assert!(has_template_class, "should have instantiated Tuple2<int, double> class");
    }

    #[test]
    fn lower_class_template_inheritance() {
        // Stage 8: 模板基类的继承
        let src = b"\
template <typename T>
class Base {
public:
    T base_value;
};
class Derived : public Base<int> {
public:
    int derived_value;
};
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有 Derived struct
        let has_derived = out.prog.decls.iter().any(|d| match d {
            ast::Decl::StructDef(sd) => sd.name.name == "Derived",
            _ => false,
        });
        assert!(has_derived, "should have Derived class");

        // 应该有实例化的 Base<int> class
        let has_base = out.prog.decls.iter().any(|d| match d {
            ast::Decl::StructDef(sd) => sd.name.name.starts_with("__dcc_template_Base"),
            _ => false,
        });
        assert!(has_base, "should have instantiated Base<int> class");
    }

    #[test]
    fn lower_function_template_explicit() {
        // Stage 8: 显式指定模板实参的函数模板调用
        let src = b"\
template <typename T>
T identity(T x) {
    return x;
}
int test() {
    return identity<int>(42);
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有函数（test + 实例化的 identity<int>）
        let fn_count = out
            .prog
            .decls
            .iter()
            .filter(|d| matches!(d, ast::Decl::Func(_)))
            .count();
        assert!(fn_count >= 2, "should have at least 2 functions (test + identity<int>)");
    }

    /// 收集函数体顶层 `return <整型字面量>` 的值 (不递归: 实例化后的成员体就一条 return).
    fn returned_int_literals(f: &ast::FuncDecl) -> Vec<i128> {
        let mut out = Vec::new();
        let body = match f.body.as_ref() {
            Some(b) => b,
            None => return out,
        };
        for item in &body.items {
            if let ast::BlockItem::Stmt(ast::Stmt::Return {
                val: Some(ast::Expr::Int { v, .. }),
                ..
            }) = item
            {
                out.push(*v);
            }
        }
        out
    }

    #[test]
    fn lower_non_type_param_substituted_in_member_body() {
        // M53 任务 2: `template <int N> struct S { int n() const { return N; } }` ——
        // 实例化时非类型形参必须在**成员函数体**里被替换成实参值.
        // 旧行为: 体内 `N` 解析成 undeclared identifier (形参环里没有 NTTP),
        // 整条成员函数静默丢弃.
        let src = b"\
template <int N>
struct S {
    int n() const { return N; }
};
int test() {
    S<7> s;
    return s.n();
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());
        let out = lower_decls_native(&decls).expect("native lower");

        let has_inst_class = out.prog.decls.iter().any(|d| match d {
            ast::Decl::StructDef(sd) => sd.name.name.starts_with("__dcc_template_S"),
            _ => false,
        });
        assert!(has_inst_class, "S<7> 应实例化成 __dcc_template_S_*");

        let inst_fn = out
            .prog
            .decls
            .iter()
            .find_map(|d| match d {
                ast::Decl::Func(f)
                    if f.name.name.starts_with("__dcc_template_S")
                        && f.name.name.ends_with("_n") =>
                {
                    Some(f)
                },
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!(
                    "实例化类成员的函数应存在, got {:?}",
                    out.prog
                        .decls
                        .iter()
                        .filter_map(|d| match d {
                            ast::Decl::Func(f) => Some(f.name.name.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(
            returned_int_literals(inst_fn),
            vec![7],
            "成员体里的 `return N;` 必须被替换为 `return 7;`"
        );
    }

    #[test]
    fn lower_nested_function_template_call_instantiates_both_layers() {
        // M53 任务 5: 实参**本身**是待实例化的函数模板调用 (`pick(pick(5))`).
        // 实例化工作表得递归下钻调用表达式, 否则内层调用停在原处 ->
        // 链接期 undefined 的 `__dcc_tplfn_pick_*`. 无模块的单个普通 TU 同样适用.
        let src = b"\
template <typename T>
T pick(T x) {
    return x;
}
long both() {
    return pick(pick(5)) + pick(pick(7L));
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());
        let out = lower_decls_native(&decls).expect("native lower");

        let names: Vec<String> = out
            .prog
            .decls
            .iter()
            .filter_map(|d| match d {
                ast::Decl::Func(f) => Some(f.name.name.clone()),
                _ => None,
            })
            .collect();
        let pick_instances: Vec<&String> = names
            .iter()
            .filter(|n| n.starts_with("__dcc_tplfn_pick_"))
            .collect();
        assert!(
            pick_instances.iter().any(|n| n.ends_with("_int")),
            "内层 pick<int> 实例必须被发射: {:?}",
            pick_instances
        );
        assert!(
            pick_instances.iter().any(|n| n.ends_with("_long")),
            "外层 pick<long> 实例必须被发射: {:?}",
            pick_instances
        );
    }

    // === Stage 9: 异常处理测试 ===

    #[test]
    fn native_supported_allows_exception() {
        // Stage 9: 异常 (throw/catch) 应该被 native_supported 接受
        let src = b"\
void test() {
    try {
        throw 1;
    } catch (int x) {
    }
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let result = native_supported(&decls);
        assert!(result.is_ok(), "exceptions should be supported in Stage 9");
    }

    #[test]
    fn lower_try_catch_simple() {
        // 简单的 try/catch 语句 lowering
        let src = b"\
int test() {
    int result = 0;
    try {
        throw 42;
    } catch (int e) {
        result = e;
    }
    return result;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有 test 函数
        let has_test_fn = out.prog.decls.iter().any(|d| match d {
            ast::Decl::Func(fd) => fd.name.name == "test",
            _ => false,
        });
        assert!(has_test_fn, "should have test function");
    }

    #[test]
    fn lower_throw_expression() {
        // throw 表达式 lowering（无 catch，直接 throw）
        let src = b"\
void test() {
    throw 1;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        // 应该有 test 函数
        let has_test_fn = out.prog.decls.iter().any(|d| match d {
            ast::Decl::Func(fd) => fd.name.name == "test",
            _ => false,
        });
        assert!(has_test_fn, "should have test function");
    }

    #[test]
    fn lower_multiple_catch_handlers() {
        // 多个 catch handler
        let src = b"\
int test(int code) {
    int result = 0;
    try {
        if (code == 1) {
            throw 1;
        } else {
            throw 2;
        }
    } catch (int e) {
        result = e;
    } catch (...) {
        result = 99;
    }
    return result;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(native_supported(&decls).is_ok());

        let out = lower_decls_native(&decls).expect("native lower");

        let has_test_fn = out.prog.decls.iter().any(|d| match d {
            ast::Decl::Func(fd) => fd.name.name == "test",
            _ => false,
        });
        assert!(has_test_fn, "should have test function");
    }

    // === Stage 10: Feature detection 测试 ===

    #[test]
    fn detect_features_free_function() {
        let src = b"int add(int a, int b) { return a + b; }\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::FreeFunction));
        assert!(!features.contains(&CppFeature::PlainClass));
    }

    #[test]
    fn detect_features_plain_class() {
        let src = b"\
class Point {
public:
    int x;
    int y;
    int get_x() { return x; }
};
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::PlainClass));
        assert!(features.contains(&CppFeature::MemberFunction));
    }

    #[test]
    fn detect_features_virtual_function() {
        let src = b"\
class Base {
public:
    virtual int value() { return 1; }
};
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::VirtualFunction));
        assert!(features.contains(&CppFeature::VPtrInit));
    }

    #[test]
    fn detect_features_virtual_inheritance() {
        let src = b"\
class Base { int x; };
class Derived : public virtual Base { int y; };
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::VirtualInheritance));
        assert!(features.contains(&CppFeature::NonVirtualInheritance));
    }

    #[test]
    fn detect_features_namespace() {
        let src = b"\
namespace foo {
    int bar() { return 42; }
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::Namespace));
    }

    #[test]
    fn detect_features_using_decl() {
        let src = b"using std::vector;\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::Namespace));
    }

    #[test]
    fn detect_features_typeid() {
        let src = b"\
#include <typeinfo>
void test() { typeid(int); }
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::TypeId));
    }

    #[test]
    fn detect_features_dynamic_cast() {
        let src = b"\
class Base { virtual void foo() {} };
class Derived : public Base {};
void test(Base* b) { dynamic_cast<Derived*>(b); }
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::DynamicCast));
    }

    #[test]
    fn detect_features_exception() {
        let src = b"\
void test() {
    try {
        throw 1;
    } catch (int e) {
    }
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::Exception));
        assert!(features.contains(&CppFeature::DtorCleanup));
    }

    #[test]
    fn detect_features_template_class() {
        let src = b"\
template <typename T>
class Box {
public:
    T value;
};
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::ClassTemplate));
    }

    #[test]
    fn detect_features_template_function() {
        let src = b"\
template <typename T>
T identity(T x) { return x; }
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::FunctionTemplate));
    }

    #[test]
    fn detect_features_new_delete() {
        let src = b"\
void test() {
    int* p = new int(42);
    delete p;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::NewDelete));
    }

    #[test]
    fn detect_features_initializer_list() {
        // 使用 return {a, b} 形式的初始化列表来触发 InitializerList 特性检测
        // 这种语法在 C++ 中作为函数返回值的列表初始化
        let src = b"\
struct Point {
    int x;
    int y;
};
Point make_point(int a, int b) {
    return {a, b};
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(
            features.contains(&CppFeature::InitializerList),
            "expected InitializerList feature, got: {:?}",
            features.iter().map(|f| f.name()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn detect_features_static_local() {
        let src = b"\
int& counter() {
    static int c = 0;
    return c;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::StaticInitGuard));
    }

    #[test]
    fn detect_features_coroutine() {
        let src = b"\
#include <coroutine>
void test() {
    co_return;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::Coroutine));
    }

    #[test]
    fn detect_features_concept() {
        let src = b"\
template <typename T>
concept Addable = requires(T a, T b) { a + b; };
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::Concept));
    }

    #[test]
    fn detect_features_module() {
        let src = b"module foo;\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let features = detect_features(&decls);
        assert!(features.contains(&CppFeature::Module));
    }

    #[test]
    fn can_compile_native_simple() {
        let src = b"int add(int a, int b) { return a + b; }\n";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(can_compile_native(&decls).is_ok());
    }

    #[test]
    fn can_compile_native_with_class() {
        let src = b"\
class Point {
public:
    int x;
    int y;
    Point() {}
    int sum() { return x + y; }
};
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(can_compile_native(&decls).is_ok());
    }

    #[test]
    fn can_compile_native_with_virtual() {
        let src = b"\
class Base {
public:
    virtual int value() { return 1; }
    Base() {}
};
class Derived : public Base {
public:
    int value() override { return 2; }
    Derived() {}
};
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(can_compile_native(&decls).is_ok());
    }

    /// namespace: sema 已支持作用域内名字查找与 `ns_fn` 形式 linkage name,
    /// native 后端可直接编译。
    #[test]
    fn can_compile_native_ok_for_namespace() {
        let src = b"\
namespace foo {
    int bar() { return 42; }
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(can_compile_native(&decls).is_ok());
    }

    /// new/delete: native 后端按 Itanium ABI 调用 operator new / operator delete,
    /// 链接期由 C++ 运行时提供实现。
    #[test]
    fn can_compile_native_ok_for_new_delete() {
        let src = b"\
void test() {
    int* p = new int;
    delete p;
}
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(can_compile_native(&decls).is_ok());
    }

    /// concept: 仅作为约束声明参与重载决议, 不生成任何代码, native 后端可直接编译。
    #[test]
    fn can_compile_native_ok_for_concept() {
        let src = b"\
template <typename T>
concept Foo = true;
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        assert!(can_compile_native(&decls).is_ok());
    }
}
