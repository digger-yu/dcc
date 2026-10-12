// src/codegen/x86_64/ifunc.rs
//
// `__attribute__((target_clones(...)))` 与 x86 CPU 内置函数的 **codegen 侧真实现**。
//
// 本模块干三件事，全部落在 `.o` 里，宿主 `cc`/`ld` 可原生处理：
//
// 1. [`expand_target_clones`]：driver 在 `ir::lower` **之前**调用的 AST 展开。
//    把 `f(target_clones("avx,sse4.2,default"))` 变成
//      * `f.default` / `f.avx` / `f.sse4_2` —— 各自独立函数体、独立地址（LOCAL + `used`）
//      * `f` —— 保留原型的**无体声明**，`target_clones` 字段留作「我是 IFUNC 基名」的标记
// 2. [`Codegen::emit_ifunc_synthesis`]：在 `.text` 尾部合成
//      * `__dcc_cpu_model`（`.data` 里的 LOCAL OBJECT，CPUID 结果缓存；放 `.data` 而不是
//        gcc 那样放 `.bss` 的理由写在 [`Codegen::ensure_cpu_model`] 的注释里）
//      * `__dcc_cpu_indicator_init`（LOCAL FUNC，按 `cpu_isa::PROBES` 逐条做 CPUID/xgetbv 填缓存）
//      * `<f>.resolver`（FUNC，真做特性判定并返回**选中变体的地址**）
//    并登记 `<f>` 为 `STT_GNU_IFUNC`，`st_value`/`st_size` 取自 `<f>.resolver`
//    （由 `elf/rel_symbols.rs` 写入 symtab）。
// 3. [`Codegen::emit_cpu_supports`] / [`Codegen::emit_cpu_is`]：`__builtin_cpu_supports` /
//    `__builtin_cpu_is` / `__builtin_cpu_init` 的真实现 —— 与 resolver 消费**同一份**
//    `cpu_isa` 规格与同一个缓存对象、同一条判定发射器 [`emit_feature_check`]。
//
// ## 为什么 CPUID 检测放在 resolver 体内（而不是 `.init_array`）
//
// gcc 的做法是把检测结果缓存在 libgcc 的 `__cpu_model` 里，并用 `.init_array`
// 里的 `__cpu_indicator_init` 预热；`__builtin_cpu_supports` 展开成「查表」。
// 但 **resolver 是被动态链接器在重定位阶段调用的，那一刻 `.init_array` 还没跑**
// （glibc 处理 R_X86_64_IRELATIVE 早于执行 init_array）。所以 gcc 自己的 resolver
// 里第一条就是 `call __cpu_indicator_init`（实测 `objdump -d` 于
// `.text.compute.resolver`：`callq *<__cpu_indicator_init@plt>` 紧跟
// `mov 0xc(%rip),%eax` 之类的 `__cpu_model` 读表），init_array 只是给「没有
// resolver 的普通调用点」预热。
//
// dcc 选择：**resolver 与 builtin 都在使用时 `call __dcc_cpu_indicator_init`**，
// 该函数内部带 `SLOT_INIT_DONE` 幂等守卫（多次调用只测一次），并且
// **不注册 `.init_array`**。理由：
//   * 少一个依赖：不注册 init_array 就不需要构造函数遍历机制，产物对「谁先跑」
//     没有任何假设；内置链接器那条路径也不必实现 init_array 调度。
//   * 惰性检查是**必做**的（resolver 路径本来就不能等 init），再加一份
//     init_array 预热只是重复劳动。
//   * 不依赖 libgcc 符号：`__cpu_model`/`__cpu_indicator_init` 是 libgcc 里的
//     WEAK GLOBAL，走那条路会让 `.o` 产生外部 UND 符号并要求 `-lgcc`；dcc 的
//     产物必须自包含（这是「宿主链接器原生处理」的前提，也是本轮改造的动机）。
//   * `__dcc_cpu_indicator_init` 是 **STB_LOCAL**，所以调用它发的是
//     `R_X86_64_PC32` 而不是 PLT32（见 `elf/resolve.rs` 的 bind 分派），
//     动态链接期不需要任何符号解析。
// 代价：第一次调用 `__builtin_cpu_supports` 多一个 call + 一次查表 —— 与 gcc 的
// `__cpu_indicator_init` 惰性检查完全同形，不是额外负担。
//
// ## 单一真源
//
// leaf/位号、XCR0 需求、变体优先级、缓存槽布局**只在 `crate::semantics::cpu_isa`
// 里定义一次**（见该模块头注释）。本模块只负责「照着表发射机器码」，**不列任何
// leaf 号、位号或优先级数字**；内置链接器那条旧路径 (`src/link/target_clones.rs`)
// 也已改成消费同一份表，并在那里写明了它为什么仍需保留。
// 名字映射（`mangle_variant_sym` / `resolver_sym`）也只在这里定义一次，
// `elf/rel_symbols.rs` 与 `src/link/mod.rs` 都调用它，不会各写一份。

#![allow(clippy::too_many_arguments)]

use crate::ast::{Decl, FuncDecl, Ident, Program};
use crate::codegen::shared::elf::{R_X86_64_PC32, STB_LOCAL, STB_WEAK};
use crate::codegen::x86_64::insn::Insn;
use crate::diagnostics::{Diagnostic, Severity, Span};
use crate::semantics::cpu_isa;

use super::{Codegen, GlobalVar, JumpKind, PendingCodeRel};

/// CPUID 结果缓存对象的名字（`.data` 里的 LOCAL OBJECT；不放 `.bss`，理由见
/// [`Codegen::ensure_cpu_model`] 的注释 —— 内置链接器的静态路径不映射 NOBITS 节）。
pub const CPU_MODEL_NAME: &str = "__dcc_cpu_model";
/// 惰性 CPUID 检测函数的名字（LOCAL FUNC）。
pub const CPU_INIT_NAME: &str = "__dcc_cpu_indicator_init";
/// resolver 符号后缀（与 gcc 实测一致：`compute.resolver`）。
pub const RESOLVER_SUFFIX: &str = ".resolver";

/// 变体符号名：`compute` + `sse4.2` → `compute.sse4_2`。
///
/// 这个映射**只在这里定义一次**：展开侧（造函数名）、合成侧（在标号表里找变体）
/// 与符号表侧都调它，两边不可能漂移。取值形状来自对 gcc 13.3 的实测
/// （`readelf -sW` 在 `gcc -c` 的产物里看到 `compute.default` / `compute.sse4_2`
/// / `multiply.avx` / `f.avx`，即 ISA 名里的 `.` 与 `-` 被换成 `_`，基名与 ISA 名
/// 之间保留 `.`）。
pub fn mangle_variant_sym(base: &str, arch: &str) -> String {
    let mut mangled = String::with_capacity(base.len() + arch.len() + 1);
    mangled.push_str(base);
    mangled.push('.');
    for ch in arch.chars() {
        match ch {
            '.' | '-' => mangled.push('_'),
            other => mangled.push(other),
        }
    }
    mangled
}

/// resolver 符号名：`compute` → `compute.resolver`。
pub fn resolver_sym(base: &str) -> String {
    format!("{base}{RESOLVER_SUFFIX}")
}

/// `target_clones` / CPU 内置函数被判定为非法形状时的**用户面**错误码。
pub const DIAG_BAD_CLONE: &str = "E1125";
/// 缓存槽 / 位表不变量被破坏（编译器自己的错，不是用户代码的错）。
pub const DIAG_INTERNAL: &str = "E1126";
/// 非可重定位（ET_EXEC 直出）路径不支持 target_clones / CPU 内置函数。
pub const DIAG_NO_EXEC_SUPPORT: &str = "E1127";

/// 合成出来的函数（symtab 里必须以「已定义符号」出现，且不在 `prog.decls` 里，
/// 所以 `elf/rel_symbols.rs` 需要一个显式清单）。
#[derive(Debug, Clone)]
pub struct SynthFunc {
    /// 符号名（同时是 `self.labels` 里的标号名）
    pub name: String,
    /// 符号绑定：`STB_LOCAL`（init 函数）/ `STB_LOCAL|STB_WEAK`（resolver）
    pub bind: u8,
}

/// 一个 IFUNC 基符号：它的值/大小取自 `<base>.resolver` 的合成体。
#[derive(Debug, Clone)]
pub struct IfuncDef {
    /// IFUNC 符号名（asm label 解析后的名字）
    pub name: String,
    /// 绑定：`STB_GLOBAL`，`static` 基名时为 `STB_LOCAL`
    pub bind: u8,
    /// resolver 符号名（用于在标号表里找地址）
    pub resolver: String,
    /// 基名是否为 `static`
    pub is_static: bool,
    /// 基名的 `visibility`（透传给 symtab）
    pub visibility: Option<String>,
}

/// `target_clones` 的判定结果：一个基名 + 它的变体清单。
///
/// 从 AST 反推（基名 decl 的 `target_clones` 字段就是唯一载体），而不是把一份
/// 侧表从 driver 传到 codegen —— 少一条「两处状态可能不一致」的通道。
#[derive(Debug, Clone)]
pub struct CloneBase {
    pub base: String,
    /// ISA 名（原样，来自 `target_clones` 属性，含 `"default"`）
    pub arches: Vec<String>,
    pub default_arch: String,
    /// 按 `cpu_isa::dispatch_order` 排好的**非 default** ISA 名（先测者先选中）
    pub dispatch: Vec<String>,
    /// 基名的 `is_static`（决定 IFUNC 与 resolver 的符号绑定）
    pub is_static: bool,
    pub is_weak: bool,
    pub visibility: Option<String>,
}

impl CloneBase {
    pub fn variant_sym(&self, arch: &str) -> String {
        mangle_variant_sym(&self.base, arch)
    }
    pub fn default_sym(&self) -> String {
        mangle_variant_sym(&self.base, &self.default_arch)
    }
    /// resolver 的**符号名**（asm label 不参与：合成体自己决定标号名）。
    pub fn resolver_sym(&self) -> String {
        resolver_sym(&self.base)
    }
}

/// 扫 AST，找出「已被展开的 target_clones 基名」。
///
/// 判据（三条同时成立）：`target_clones` 非空、**无函数体**、列表里有 `"default"`。
/// 「有体的」= 展开还没跑过（例如别的 pass 重建了 decl），那种形状不属于本模块的
/// 输入 —— 展开侧保证了基名一定被替换成无体声明，所以这里静默跳过是安全的；
/// `expand_target_clones` 的单元测试钉住「展开后基名无体」。
pub fn collect_clone_bases(prog: &Program) -> Vec<CloneBase> {
    let mut out: Vec<CloneBase> = Vec::new();
    for decl in &prog.decls {
        let Decl::Func(f) = decl else { continue };
        if f.target_clones.is_empty() || f.body.is_some() {
            continue;
        }
        let Some(default_arch) = f.target_clones.iter().find(|a| *a == "default").cloned() else {
            continue;
        };
        let arches: Vec<String> = f.target_clones.clone();
        let dispatch: Vec<String> = {
            let dispatch_arches: Vec<&str> = arches
                .iter()
                .map(|a| a.as_str())
                .filter(|a| *a != "default")
                .collect();
            cpu_isa::dispatch_order(&dispatch_arches)
                .into_iter()
                .map(|s| s.to_string())
                .collect()
        };
        out.push(CloneBase {
            base: f.name.name.clone(),
            arches,
            default_arch,
            dispatch,
            is_static: f.is_static,
            is_weak: f.is_weak,
            visibility: f.visibility.clone(),
        });
    }
    out
}

/// 本 TU 是否需要「IFUNC / CPUID 合成」（用于 ET_EXEC 路径的显式拒绝）。
pub fn needs_ifunc_synthesis(prog: &Program) -> bool {
    !collect_clone_bases(prog).is_empty()
        || prog
            .decls
            .iter()
            .any(|d| matches!(d, Decl::Func(f) if f.ifunc_resolver.is_some()))
}

// ============================================================================
// AST 展开（driver 在 ir::lower 之前调用）
// ============================================================================

/// 校验一个 ISA 名，返回人类可读的拒绝理由。
fn validate_clone_arch(arch: &str) -> Result<(), String> {
    if arch == "default" {
        return Ok(());
    }
    if cpu_isa::lookup(arch).is_none() {
        return Err(format!(
            "ISA '{}' is not known to dcc (spelling? the cloneable set is: {})",
            arch,
            cpu_isa::cloneable_names().join(", ")
        ));
    }
    if !cpu_isa::is_cloneable(arch) {
        return Err(format!(
            "ISA '{}' is known but cannot be used as a target_clones variant (gcc rejects it too; use one of: {})",
            arch,
            cpu_isa::cloneable_names().join(", ")
        ));
    }
    Ok(())
}

fn push_diag(diags: &mut Vec<Diagnostic>, sev: Severity, msg: String, span: Span, hint: String) {
    diags.push(Diagnostic {
        sev,
        code: DIAG_BAD_CLONE,
        msg,
        span,
        hint: Some(hint),
        source_line: None,
    });
}

/// 就地展开 `prog` 里所有 `target_clones` 函数。
///
/// 诊断策略（与 gcc 对齐，逐条实测过，取数过程记在 `docs/wp_ifunc_real_fix.md`）：
///   * 只有一个变体（`target_clones("avx")`）→ gcc **warning**「single
///     target_clones attribute is ignored」→ dcc 同样只警告，并按普通函数处理
///     （清空属性，不产 IFUNC）。
///   * 多变体但没有 `"default"` → gcc **error**「'default' target was not set」→ E1125。
///   * ISA 名不认识 / 不可克隆 → E1125（不是 warning：发一条永不命中的检测等于
///     静默让该变体永远选不到）。
///   * 属性挂在**没有函数体**的声明上 → gcc 忽略并给 warning，dcc 同样。
///   * 体内定义 GNU 嵌套函数 → E1125。为什么必须拒：展开会把同一个体复制 N 份，
///     而嵌套函数的符号名是**按名字**登记的（`nested_funcs` / trampoline 槽都以
///     名字为键），复制体会让两份同名嵌套函数互相覆盖 —— 那是静默误编译。
///     「什么算嵌套函数」直接复用 codegen 自己的遍历器 [`Codegen::collect_nested_funcs`]，
///     不在本模块另写一份 AST 走查（两份遍历必然漂移）。
///
/// 本 pass 同时负责一件看起来不相关但必须同处做的事：把 `__attribute__((ifunc("r")))`
/// 引用的 resolver `r` 标成「不可删」（见下面第 0 步）。
pub fn expand_target_clones(prog: &mut Program, diags: &mut Vec<Diagnostic>) {
    // ---- 0) `__attribute__((ifunc("r")))` 的 resolver 必须存活到 codegen ----
    //
    // resolver 只被属性引用、不被任何 C 调用点引用，所以 `static` resolver 会被
    // 「按引用可达性删」的 DCE（`ssa::passes::cross_function_dce`）当死代码删掉：实测
    // gcc 13 在 -O2/-O3 仍发 `FUNC LOCAL resolve_a` + `IFUNC GLOBAL ifunc_a` 同址，而
    // dcc 在 -O2/-O3 上 resolver 已消失 → IFUNC 符号失去落点（只能报 E1125，产物没有
    // IFUNC）。GCC 的语义相当于把这种 resolver 隐式当 `__attribute__((used))`。这里给
    // 同一效果：标 `is_used_attr` —— 那正是那条 DCE 认的保留位（roots 与 retain 两个
    // 判定点都读它），所以不需要在优化器里另列一份名字白名单。
    retain_ifunc_resolvers(prog);

    // 先收集（避免边改边迭代的借用问题），再按下标替换。
    let candidates: Vec<(usize, FuncDecl)> = prog
        .decls
        .iter()
        .enumerate()
        .filter_map(|(i, d)| match d {
            Decl::Func(f) if !f.target_clones.is_empty() => Some((i, f.clone())),
            _ => None,
        })
        .collect();
    if candidates.is_empty() {
        return;
    }
    // 同名只展开一次（声明 + 定义会在 decls 里出现两遍）。
    let mut done: Vec<String> = Vec::new();
    let mut replacements: Vec<(usize, Vec<Decl>)> = Vec::new();
    let mut hard_error = false;
    for (idx, f) in candidates {
        if done.contains(&f.name.name) {
            // 同名第二份：清掉属性，避免它被认成第二个 IFUNC 基名。
            if let Decl::Func(target) = &mut prog.decls[idx] {
                target.target_clones.clear();
            }
            continue;
        }
        done.push(f.name.name.clone());
        let span = f.span.clone();
        let arches: Vec<String> = f.target_clones.clone();
        for a in &arches {
            if let Err(why) = validate_clone_arch(a) {
                push_diag(
                    diags,
                    Severity::Error,
                    format!("target_clones on '{}': {why}", f.name.name),
                    span.clone(),
                    "every variant must be one of gcc's cloneable ISA names, and \"default\" \
                     must be present when there is more than one variant"
                        .to_string(),
                );
                hard_error = true;
            }
        }
        if hard_error {
            // 已报错就不再基于坏输入展开（否则会发出指向不存在符号的 resolver）。
            return;
        }
        if f.body.is_none() {
            push_diag(
                diags,
                Severity::Warning,
                format!(
                    "target_clones attribute is ignored on '{}': the declaration has no body \
                     to clone",
                    f.name.name
                ),
                span.clone(),
                "put the attribute on the function definition".to_string(),
            );
            if let Decl::Func(target) = &mut prog.decls[idx] {
                target.target_clones.clear();
            }
            continue;
        }
        let non_default: Vec<&String> = arches.iter().filter(|a| *a != "default").collect();
        if non_default.is_empty() {
            // 只有 "default"：没有可派发的变体，按普通函数处理（与 gcc 一致）。
            if let Decl::Func(target) = &mut prog.decls[idx] {
                target.target_clones.clear();
            }
            continue;
        }
        if arches.len() == 1 {
            // 单变体（无 default 也没关系：上面 non_default 非空已成立）
            push_diag(
                diags,
                Severity::Warning,
                format!(
                    "single target_clones attribute on '{}' is ignored (gcc needs at least one \
                     non-default variant plus \"default\")",
                    f.name.name
                ),
                span.clone(),
                format!("write target_clones(\"{}\", \"default\")", non_default[0]),
            );
            if let Decl::Func(target) = &mut prog.decls[idx] {
                target.target_clones.clear();
            }
            continue;
        }
        if non_default.len() + 1 != arches.len() {
            // 多变体里没有 "default"
            push_diag(
                diags,
                Severity::Error,
                format!(
                    "target_clones on '{}': 'default' target was not set (gcc requires it)",
                    f.name.name
                ),
                span.clone(),
                "add \"default\" to the target_clones list; it is the fallback implementation \
                 used when no CPU feature check matches"
                    .to_string(),
            );
            hard_error = true;
            continue;
        }
        let mut nested: Vec<FuncDecl> = Vec::new();
        Codegen::collect_nested_funcs(&f.body.as_ref().unwrap().items, &mut nested);
        if !nested.is_empty() {
            push_diag(
                diags,
                Severity::Error,
                format!(
                    "target_clones on '{}' cannot be expanded: its body defines nested \
                     function(s) {}, and cloning that body would emit the same nested symbol \
                     more than once",
                    f.name.name,
                    nested
                        .iter()
                        .map(|n| format!("'{}'", n.name.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                span.clone(),
                "move the nested function out of the cloned function".to_string(),
            );
            hard_error = true;
            continue;
        }

        // ---- 展开 ----
        let base_name = f.name.name.clone();
        let mut clones: Vec<Decl> = Vec::new();
        for arch in &arches {
            let sym = mangle_variant_sym(&base_name, arch);
            let mut cf = f.clone();
            cf.name = Ident {
                name: sym.clone(),
                span: span.clone(),
            };
            cf.target_clones.clear();
            // 变体实现体只被本 TU 的合成 resolver 引用 → LOCAL（与 gcc 实测一致：
            // `compute.sse4_2` 在 symtab 里是 LOCAL FUNC，地址各不相同）。
            cf.is_static = true;
            // `used` 让「没有 C 调用点引用变体」不会被当成可删死代码：resolver 是
            // 合成体、不是 AST 调用点，任何「按引用可达性删除」的 pass 都会误杀它。
            cf.is_used_attr = true;
            cf.is_weak = false;
            cf.is_inline = false;
            cf.is_always_inline = false;
            // 变体不带 asm label：合成侧按 **C 名**找标号，改了符号名就找不到了。
            cf.asm_label = None;
            cf.alias_name = None;
            cf.ifunc_resolver = None;
            clones.push(Decl::Func(cf));
        }
        // 基名声明：无体 + 保留 `target_clones` 作为「我是 IFUNC 基名」的唯一标记。
        let mut base = f.clone();
        base.body = None;
        base.is_extern = false;
        base.alias_name = None;
        // `asm_label` **保留**：IFUNC 符号名走 GNU asm label 语义（`__asm__("g")` →
        // 符号 `g` 是 IFUNC），调用点仍由 `asm_label_map` 解析到它。
        let mut v = Vec::with_capacity(clones.len() + 1);
        v.push(Decl::Func(base));
        v.extend(clones);
        replacements.push((idx, v));
    }
    if hard_error {
        return;
    }
    // 倒序替换，避免前面的替换使后面的下标失效。
    for (idx, decls) in replacements.into_iter().rev() {
        prog.decls.splice(idx..idx + 1, decls);
    }
}

/// 把被 `__attribute__((ifunc("r")))` 引用的 resolver `r` 标为「不可删」。
///
/// 为什么必须在 AST 层做（而不是在 codegen 找 label 时兼容）：一旦优化 pass 把
/// `static` resolver 删了，到 `.o` 发射阶段只剩下一条“没有落点”的错误可选，而产物
/// 会静默失去 IFUNC 语义。先收集再改写，避开对 `prog` 的借用冲突。
pub fn retain_ifunc_resolvers(prog: &mut Program) {
    let wanted: Vec<String> = prog
        .decls
        .iter()
        .filter_map(|d| match d {
            Decl::Func(f) => f.ifunc_resolver.clone(),
            _ => None,
        })
        .collect();
    if wanted.is_empty() {
        return;
    }
    for decl in &mut prog.decls {
        if let Decl::Func(f) = decl {
            if wanted.contains(&f.name.name) {
                f.is_used_attr = true;
            }
        }
    }
}

/// 从一条表达式里取字符串字面量（`__builtin_cpu_supports("avx")` 的参数）。
///
/// 只认字面量：gcc 也要求参数是字符串字面量，任何非常量参数在 gcc 里是
/// 「error: parameter to builtin not valid」。这里返回 `None` 让调用方发诊断，
/// **不得**把非字面量当空串静默处理。
///
/// 返回体里带上字面量自己的 `Span`：诊断必须能指到调用点，不能只报个
/// 「E1125 在某文件第 0 行」。
pub(crate) fn string_lit_expr(e: &crate::ast::Expr) -> Option<(Vec<u8>, Span)> {
    match e {
        crate::ast::Expr::String { data, span, .. } => Some((data.clone(), span.clone())),
        crate::ast::Expr::Cast { e, .. } => string_lit_expr(e),
        _ => None,
    }
}
impl Codegen {
    // ========================================================================
    // CPUID 缓存对象
    // ========================================================================

    /// 在 `.data` 里注册 `__dcc_cpu_model`（LOCAL OBJECT，`cpu_isa::MODEL_BYTES` 字节）。
    ///
    /// 只登记一次；`elf/rel_symbols.rs` 的「`globals_raw` 里但不在 `prog.decls` 里」
    /// 那一轮会自动把它发成 `STB_LOCAL` + `STT_OBJECT`，所以符号本体不需要在这里
    /// 手工入 symtab（手工入会发两份同名符号）。
    ///
    /// **为什么是 `.data` 而不是 gcc 那样的 `.bss`**：`.bss` 是 NOBITS，未初始化缓存
    /// 放它本来最贴 gcc 形状（`__cpu_model` 就在 libgcc 的 `.bss`），但 dcc 自己的内置
    /// 链接器现在把 NOBITS 输出节铺错：`src/link/layout.rs::fill_section_data` 在
    /// `sec.ty == SHT_NOBITS` 时直接 `continue`（既不搬数据也**不累加 size**），
    /// `src/link/elf_exec.rs::segment_range_actual` 又把 NOBITS 节整个跳过，于是
    /// `-fbuiltin-linker` 出的 ET_EXEC 里 `.bss` size=0、RW `PT_LOAD` 是 0/0/0/0，
    /// 那一页根本没映射 —— `__dcc_cpu_indicator_init` 第一个写就 SIGSEGV（实测：
    /// 只调 `__builtin_cpu_supports("sse2")` 的程序 `rc=139`，任何有未初始化全局的
    /// 程序都同形，与本模块无关的既有缺陷，已写进 `docs/wp_ifunc_real_fix.md` 要求
    /// 主控在 `layout.rs`/`elf_exec.rs`（不属本文件所有权）修）。把缓存放到被正
    /// 确铺设的 `.data`（多 68 字节文件尺寸）能让两条链路径（宿主 `ld` 动态与内置
    /// 静态）都真能跑 CPUID，而不是“在宿主链下能跑、在自家链下崩”。
    pub fn ensure_cpu_model(&mut self) {
        if self.cpu_model_registered {
            return;
        }
        self.cpu_model_registered = true;
        // 8 字节对齐：槽全是 dword，但 gcc 的 `__cpu_model` 也是 8 字节对齐对象，
        // 与它同形可以让 `readelf -d`/布局比对保持简单。
        self.data_align = self.data_align.max(8);
        let off = (self.data_section.len() + 7) & !7;
        while self.data_section.len() < off {
            self.data_section.push(0);
        }
        // 显式零填充：`.data` 是 PROGBITS，不 `resize` 到足够长度就会让后面的
        // 已初始化全局变量盖到本对象的字节上（静态初值就是 0，与 `.bss` 同语义）。
        self.data_section.resize(off + cpu_isa::MODEL_BYTES, 0);
        self.globals_raw.insert(
            CPU_MODEL_NAME.to_string(),
            GlobalVar {
                offset: off as u64,
                size: cpu_isa::MODEL_BYTES,
                // 类型在这里只承载「这是个对象」；符号的 `st_size` 取自上面的
                // `size` 字段（`cpu_isa::MODEL_BYTES`），不是取自这个占位类型。
                ty: crate::ast::Type::ULong,
                init: None,
                section: 3, // .data
                is_tls: false,
                in_data_section: true,
            },
        );
    }

    // ========================================================================
    // `__dcc_cpu_indicator_init`：照 `cpu_isa::PROBES` 发射的检测体
    // ========================================================================

    /// 合成 `__dcc_cpu_indicator_init`：逐条消费 `cpu_isa::PROBES` 填 `__dcc_cpu_model`。
    ///
    /// leaf 清单、子 leaf、结果落哪个槽、跳过条件（最大 leaf 号门控 / OSXSAVE 门控）
    /// **全部来自那张表**，本函数不列任何 leaf 号；OSXSAVE 的位号也经由
    /// `cpu_isa::osxsave_test()` 从特性表反查，不在这里写 `1 << 27`。
    ///
    /// 序言/结语保存恢复 `rbx`：`cpuid` 会破坏 `ebx`，而 `ebx` 在 SysV 是
    /// callee-saved；resolver 是被 ld-linux 调用的，毁掉它的 `rbx` 会让动态链接器
    /// 自己出错（gcc 的 `__cpu_indicator_init` 同样 `push %rbx`）。
    pub fn emit_cpu_indicator_init(&mut self) {
        self.align(16);
        let start = self.pos();
        self.label(CPU_INIT_NAME);
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Push,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    3,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // push rbx
            // 幂等守卫: if (model.init_done) goto done
        let l_done = self.fresh_label();
        self.emit_model_rip(&RIP_MOV_EAX_M32, cpu_isa::SLOT_INIT_DONE, None);
        self.emit_insn(&Insn::test_rm_reg(crate::codegen::shared::asm_operand::Width::B32, 0, 0)); // test eax, eax
        self.jmp_rel32(&l_done, JumpKind::Jne);

        for probe in cpu_isa::PROBES {
            // ---- 跳过条件 ----
            let l_skip = self.fresh_label();
            match probe.gate {
                cpu_isa::Gate::None => {},
                cpu_isa::Gate::LeafAtLeast { slot, min } => {
                    // 超过最大 leaf 号的 CPUID 返回最高基本 leaf 的内容,
                    // 不门控就会把别人的特性字当本 leaf 的结果读。
                    self.emit_model_rip(&RIP_MOV_EAX_M32, slot, None);
                    self.cmp_eax_imm32(min);
                    self.jmp_rel32(&l_skip, JumpKind::Jb);
                },
                cpu_isa::Gate::OSXSAVE => {
                    // xgetbv 在 OSXSAVE=0 时 #UD。位号从特性表反查（不是这里的常量）。
                    match cpu_isa::osxsave_test() {
                        Some((slot, mask)) => {
                            self.emit_model_rip(&RIP_TEST_M32_IMM32, slot, Some(mask));
                            self.jmp_rel32(&l_skip, JumpKind::Je);
                        },
                        None => {
                            self.report_internal_bug(
                                DIAG_INTERNAL,
                                "cpu_isa::osxsave_test() has no bit for 'osxsave'".to_string(),
                                "PROBES gates on OSXSAVE; the FEATURES table must carry the \
                                 osxsave bit so the gate can be emitted from the table",
                            );
                            self.jmp_rel32(&l_skip, JumpKind::Jmp);
                        },
                    }
                },
            }
            // ---- 检测动作 ----
            if probe.leaf == cpu_isa::XGETBV_LEAF {
                self.xor_ecx_ecx(); // XCR0
                self.xgetbv();
            } else {
                self.mov_eax_imm32(probe.leaf);
                if probe.subleaf == 0 {
                    self.xor_ecx_ecx();
                } else {
                    self.mov_ecx_imm32(probe.subleaf);
                }
                self.cpuid();
            }
            // ---- 结果落槽 ----
            if let Some(slot) = probe.out_eax {
                self.emit_model_rip(&RIP_MOV_M32_EAX, slot, None);
            }
            if let Some(slot) = probe.out_ebx {
                self.emit_model_rip(&RIP_MOV_M32_EBX, slot, None);
            }
            if let Some(slot) = probe.out_ecx {
                self.emit_model_rip(&RIP_MOV_M32_ECX, slot, None);
            }
            if let Some(slot) = probe.out_edx {
                self.emit_model_rip(&RIP_MOV_M32_EDX, slot, None);
            }
            self.label(&l_skip);
        }

        // ---- 厂商串 → SLOT_VENDOR_KIND ----
        self.emit_vendor_kind();

        // ---- family/model 解码 + 型号分类链 → SLOT_CPU_* ----
        //
        // 为什么**无条件**发（不按「本 TU 是否用了型号名」门控）：门控的唯一依据只能
        // 是「有没有人读 `SLOT_CPU_TYPE`」，而那份状态在并行 codegen 下住在 worker 的
        // `Codegen` 实例里、不会并回 master（本文件
        // [`emit_ifunc_synthesis`] 上方为同一件事踩过 `cpu_builtin_used` 的坑）；
        // 门控漏判的后果是型号名**静默答 0**（正是要修的缺陷形状），不是报错。
        // 代价是 `.text` 多约 1 KB —— gcc 把这段放在 libgcc 的 `__cpu_indicator_init`
        // 里，dcc 的 `.o` 必须自包含（不依赖 libgcc 符号是「宿主链接器原生处理」的
        // 前提），本来就已经整份复制 leaf 采集序列，这条链不是新的量级变化。
        self.emit_family_model_decode();
        self.emit_classify_chain();

        // model.init_done = 1
        self.emit_model_rip(&RIP_MOV_M32_IMM32, cpu_isa::SLOT_INIT_DONE, Some(1));
        self.label(&l_done);
        self.emit_insn(&Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Pop,
                width: crate::codegen::shared::asm_operand::Width::B64,
                dst: crate::codegen::x86_64::opcode::Operand::Reg(
                    3,
                    crate::codegen::shared::asm_operand::Width::B64,
                ),
            },
            &[],
        )); // pop rbx
        self.ret();
        let size = self.pos() - start;
        self.func_sizes.insert(CPU_INIT_NAME.to_string(), size);
        self.ifunc_synth.push(SynthFunc {
            name: CPU_INIT_NAME.to_string(),
            bind: STB_LOCAL,
        });
    }

    /// 把 leaf 0 的三个厂商 dword 比成 kind 值，结果写 `SLOT_VENDOR_KIND`。
    ///
    /// 比较表**只有** `cpu_isa::VENDOR_SIGS` 一份：四个厂商（Intel / AMD /
    /// Centaur / Zhaoxin(Shanghai)）的三 dword 组合与它们各自的 kind 都由那张表
    /// 给出，本函数不列任何厂商字符串或 kind 数字。参考求值器
    /// `cpu_isa::vendor_kind()` 读的是同一张表，所以「机器码认出的厂商」与
    /// 「测试里用来对账的厂商」不可能不一致。
    ///
    /// 三个 dword 都比：厂商串是 EBX,EDX,ECX 三段拼的，只比 BX 会把 "Genu***"
    /// 的仿冒串当成 Intel（gcc 的 `cpu_indicator_init` 只比 BX，差别仅在仿冒串上，
    /// 见 `cpu_isa::vendor_kind_of_bx` 的注释）。
    fn emit_vendor_dwords(&mut self, bx: u32, dx: u32, cx: u32, fail: &str) {
        for (slot, want) in [
            (cpu_isa::SLOT_VENDOR_BX, bx),
            (cpu_isa::SLOT_VENDOR_DX, dx),
            (cpu_isa::SLOT_VENDOR_CX, cx),
        ] {
            self.emit_model_rip(&RIP_MOV_EAX_M32, slot, None);
            self.cmp_eax_imm32(want);
            self.jmp_rel32(fail, JumpKind::Jne);
        }
    }

    fn emit_vendor_kind(&mut self) {
        // `__dcc_cpu_model` 在 `.data` 里是显式零填充（等价于 `.bss` 的静态初值），
        // `SLOT_VENDOR_KIND` 的初值就是 `VENDOR_UNKNOWN`(0)，所以只在「命中」时写入，
        // 不需要显式清 0 —— 认不出厂商的机器既不写 kind，也不会进任何分类组。
        let l_end = self.fresh_label();
        for sig in cpu_isa::VENDOR_SIGS {
            let l_next = self.fresh_label();
            self.emit_vendor_dwords(sig.bx, sig.dx, sig.cx, &l_next);
            self.emit_model_rip(&RIP_MOV_M32_IMM32, cpu_isa::SLOT_VENDOR_KIND, Some(sig.kind));
            self.jmp_rel32(&l_end, JumpKind::Jmp);
            self.label(&l_next);
        }
        self.label(&l_end);
    }

    // ========================================================================
    // family/model 解码 + 型号分类链
    // ========================================================================
    //
    // `__builtin_cpu_is("<微架构名>")` 的真实现分两步：
    //   1. [`emit_family_model_decode`]：按 `cpu_isa::DECODE_RULES` 从 leaf1/EAX 解出
    //      **厂商相关调整后**的 family/model（gcc 的
    //      `if (family == 0x0f) {family += extended_family; model += extended_model;}
    //      else if (family == 0x06) model += extended_model;`），写
    //      `SLOT_CPU_FAMILY`/`SLOT_CPU_MODEL`；再按 `cpu_isa::KIND_PROMOTES` 把
    //      「Centaur 串 + family>=7」提升成 Zhaoxin（gcc 的 `else if` 分支顺序）。
    //   2. [`emit_classify_chain`]：照 `cpu_isa::CLASSIFY` 逐组逐规则判型号，命中的那条
    //      把 `type_id`/`subtype_id` 写进 `SLOT_CPU_TYPE`/`SLOT_CPU_SUBTYPE`。
    //
    // **id 不写在这里**：`cpu_isa::type_id`/`subtype_id` 按「首次出现在 CLASSIFY 的
    // 顺序」现算，写入端（本函数）与比较端（`emit_cpu_is` 的立即数）调同一个函数，
    // 所以不存在「两张表各编号、差一个」这种错。集成判据
    // `attr_target_clones_cpu_indicator_classify_ids_come_from_classify_table`
    // 直接反回 `.o` 字节：init 体里写进那两个槽的立即数集合必须恰好等于表派生的 id 集合。
    //
    // 寄存器分配（整段只用 caller-saved 寄存器；`rbx` 已由序言保存，不碰 `rsp`/`rbp`
    // 与 callee-saved 的 `r12-r15`，因为 resolver 是被 ld-linux 调用的，毁掉它们会让
    // 动态链接器自己出错）：
    //   * `esi` = 调整后的 model，`edx` = 调整后的 family
    //   * `ecx` = 扩展 model，`edi` = 扩展 family；解码结束后 `edi` 改放 kind
    //     （分类链的守卫会调 `emit_feature_check`，它只毁 `eax`，故 kind 不能放 `eax`）
    //   * `eax` = 临时（leaf1/EAX、kind、以及 XCR0 门控里的中间量）

    /// `dst = (eax >> shift) & mask`：`mov`/`shr`/`and` 三条，参数全部来自
    /// `cpu_isa::RAW_*`（与参考求值器 `decode_family_model` 同一组常量）。
    fn split_eax_field(&mut self, dst: u8, shift: u32, mask: u32) {
        self.mov_r32_r32(dst, R_EAX);
        self.shr_r32_imm8(dst, shift as u8);
        self.and_r32_imm32(dst, mask);
    }

    /// 照 `cpu_isa::DECODE_RULES` + `KIND_PROMOTES` 发射 family/model 解码。
    ///
    /// leaf1 没被采集过（最大 leaf < 1）时 `SLOT_LEAF1_EAX` 是 `.data` 的 0，于是
    /// family/model 都是 0、分类链一条都不命中 —— 与 gcc 的
    /// 「`max_level < 1` → 只记 VENDOR_OTHER、不分类」同形，不需要额外的「跳过」分支。
    fn emit_family_model_decode(&mut self) {
        self.emit_model_rip(&RIP_MOV_EAX_M32, cpu_isa::SLOT_LEAF1_EAX, None);
        self.split_eax_field(R_ESI, cpu_isa::RAW_MODEL_LOW_SHIFT, cpu_isa::RAW_MODEL_LOW_MASK);
        self.split_eax_field(R_EDX, cpu_isa::RAW_FAMILY_LOW_SHIFT, cpu_isa::RAW_FAMILY_LOW_MASK);
        self.split_eax_field(R_ECX, cpu_isa::RAW_EXT_MODEL_SHIFT, cpu_isa::RAW_EXT_MODEL_MASK);
        self.split_eax_field(R_EDI, cpu_isa::RAW_EXT_FAMILY_SHIFT, cpu_isa::RAW_EXT_FAMILY_MASK);
        self.emit_model_rip(&RIP_MOV_EAX_M32, cpu_isa::SLOT_VENDOR_KIND, None);

        let mut kinds: Vec<u32> = Vec::new();
        for r in cpu_isa::DECODE_RULES {
            if !kinds.contains(&r.kind) {
                kinds.push(r.kind);
            }
        }
        for kind in kinds {
            let l_end = self.fresh_label();
            self.cmp_r32_imm32(R_EAX, kind);
            self.jmp_rel32(&l_end, JumpKind::Jne);
            // 块内首条命中即止：gcc 是 `if/else if`，且比的是**未调整**的 family。
            // 所以命中并应用完调整就必须跳到块尾，不能再让下一条 `cmp` 看到
            // 已经加过扩展值的 family。
            let mut next: Option<String> = None;
            for r in cpu_isa::DECODE_RULES.iter().filter(|r| r.kind == kind) {
                if let Some(l) = next.take() {
                    self.label(&l);
                }
                self.cmp_r32_imm32(R_EDX, r.family_eq);
                let l = self.fresh_label();
                next = Some(l.clone());
                self.jmp_rel32(&l, JumpKind::Jne);
                for op in r.ops {
                    match op {
                        cpu_isa::Adjust::AddExtFamily => self.add_r32_r32(R_EDX, R_EDI),
                        cpu_isa::Adjust::AddExtModel => self.add_r32_r32(R_ESI, R_ECX),
                    }
                }
                self.jmp_rel32(&l_end, JumpKind::Jmp);
            }
            if let Some(l) = next.take() {
                self.label(&l);
            }
            self.label(&l_end);
        }

        self.emit_model_rip(&RIP_MOV_M32_EDX, cpu_isa::SLOT_CPU_FAMILY, None);
        self.emit_model_rip(&RIP_MOV_M32_ESI, cpu_isa::SLOT_CPU_MODEL, None);

        // `family >= family_min`（无符号）：写成「below 则跳过」，因为 `JumpKind`
        // 没有 `Jae`。条件读的是**调整后**的 family，与参考求值器
        // `promote_vendor_kind(kind, family)` 的入参一致。
        for p in cpu_isa::KIND_PROMOTES {
            let l_skip = self.fresh_label();
            self.cmp_r32_imm32(R_EAX, p.from);
            self.jmp_rel32(&l_skip, JumpKind::Jne);
            self.cmp_r32_imm32(R_EDX, p.family_min);
            self.jmp_rel32(&l_skip, JumpKind::Jb);
            self.mov_r32_imm32(R_EAX, p.to);
            self.label(&l_skip);
        }
        // 把（可能已提升的）kind 写回缓存：`SLOT_VENDOR_KIND` 必须等于参考求值器
        // `host_processor_class()` 返回的 kind，否则 `__builtin_cpu_is("intel"/"amd")`
        // 的比较基准与测试通路就不是同一个值。
        self.emit_model_rip(&RIP_MOV_M32_EAX, cpu_isa::SLOT_VENDOR_KIND, None);
    }

    /// 取分类表里某个类型/子型名的 id（唯一出口是 `cpu_isa::type_id`/`subtype_id`）。
    ///
    /// 解不出 id 时报 E1126 并返回 `None`，调用方**不得**退化成「写 0」：那会把
    /// 「表与发射不一致」变成「所有型号名都答 0」的静默错答。不变量
    /// `classify_every_id_is_addressable`（semantics.rs）保证正常路径到不了这里。
    fn class_id_or_diag(&mut self, name: &str, is_type: bool) -> Option<u32> {
        let got = if is_type {
            cpu_isa::type_id(name)
        } else {
            cpu_isa::subtype_id(name)
        };
        got.or_else(|| {
            self.report_internal_bug(
                DIAG_INTERNAL,
                format!(
                    "classification {} name '{name}' is not in cpu_isa::CLASSIFY",
                    if is_type { "type" } else { "subtype" }
                ),
                "ids are derived from the table by first appearance; a name that is not in \
                 the table has no id, so the emitter must not fall back to 0",
            );
            None
        })
    }

    /// 发「`reg` 满足这个谓词吗；不满足就跳到 `fail`」。
    ///
    /// 只用 `cmp` + `jne`/`je`/`jb`/`ja`/`jmp`（`JumpKind` 没有 `Jbe`/`Jae`），
    /// 所以 `<=` 写成「above 则 fail」、`>=` 写成「below 则 fail」。两者都是
    /// **无符号**比较，与 gcc 的 `unsigned int model` 一致。
    fn emit_mpred_gate(&mut self, pred: &cpu_isa::MPred, reg: u8, fail: &str) {
        match pred {
            cpu_isa::MPred::Any => {},
            cpu_isa::MPred::Eq(v) => {
                self.cmp_r32_imm32(reg, *v);
                self.jmp_rel32(fail, JumpKind::Jne);
            },
            cpu_isa::MPred::In(list) => {
                // 多值 `case`：逐值 `je pass`，一条都不中则 fail（`pass` 之后就是
                // 该规则的正文）。
                let l_pass = self.fresh_label();
                for v in *list {
                    self.cmp_r32_imm32(reg, *v);
                    self.jmp_rel32(&l_pass, JumpKind::Je);
                }
                self.jmp_rel32(fail, JumpKind::Jmp);
                self.label(&l_pass);
            },
            cpu_isa::MPred::Le(hi) => {
                self.cmp_r32_imm32(reg, *hi);
                self.jmp_rel32(fail, JumpKind::Ja);
            },
            cpu_isa::MPred::Ge(lo) => {
                self.cmp_r32_imm32(reg, *lo);
                self.jmp_rel32(fail, JumpKind::Jb);
            },
            cpu_isa::MPred::Range { lo, hi } => {
                self.cmp_r32_imm32(reg, *lo);
                self.jmp_rel32(fail, JumpKind::Jb);
                self.cmp_r32_imm32(reg, *hi);
                self.jmp_rel32(fail, JumpKind::Ja);
            },
        }
    }

    /// 照 `cpu_isa::CLASSIFY` 发射型号分类链（首条命中即止）。
    ///
    /// 与参考求值器 `cpu_isa::classify` 同形：先进组（厂商 kind + family 谓词），
    /// 组内逐规则（model 谓词 + 特性守卫），命中就写 type/subtype 并结束整条链；
    /// 组内一条都不中时只留组级类型、**不再试别的组**（`switch (family)` 只进一个
    /// case）。守卫的位号经由 `emit_feature_check` 从特性表取，本函数不写任何位号。
    fn emit_classify_chain(&mut self) {
        self.emit_model_rip(&RIP_MOV_EAX_M32, cpu_isa::SLOT_VENDOR_KIND, None);
        self.mov_r32_r32(R_EDI, R_EAX);
        let l_end = self.fresh_label();
        for g in cpu_isa::CLASSIFY {
            let l_next_group = self.fresh_label();
            self.cmp_r32_imm32(R_EDI, g.vendor);
            self.jmp_rel32(&l_next_group, JumpKind::Jne);
            self.emit_mpred_gate(&g.family, R_EDX, &l_next_group);
            if !g.group_type.is_empty() {
                if let Some(id) = self.class_id_or_diag(g.group_type, true) {
                    self.emit_model_rip(&RIP_MOV_M32_IMM32, cpu_isa::SLOT_CPU_TYPE, Some(id));
                }
            }
            for r in g.rules {
                let l_skip = self.fresh_label();
                self.emit_mpred_gate(&r.model, R_ESI, &l_skip);
                match r.guard {
                    cpu_isa::Guard::None => {},
                    cpu_isa::Guard::Has(name) => {
                        if !self.emit_feature_check(name, &l_skip) {
                            // 特性表里没有这条守卫的位（E1126 已报）：本规则**不可
                            // 命中**。不能退化成「守卫视为真」—— 那会把型号分错到
                            // 一个更靠前的子型上而无人报错。
                            self.jmp_rel32(&l_skip, JumpKind::Jmp);
                        }
                    },
                }
                if !r.cpu_type.is_empty() {
                    if let Some(id) = self.class_id_or_diag(r.cpu_type, true) {
                        self.emit_model_rip(&RIP_MOV_M32_IMM32, cpu_isa::SLOT_CPU_TYPE, Some(id));
                    }
                }
                if !r.cpu_subtype.is_empty() {
                    if let Some(id) = self.class_id_or_diag(r.cpu_subtype, false) {
                        self.emit_model_rip(
                            &RIP_MOV_M32_IMM32,
                            cpu_isa::SLOT_CPU_SUBTYPE,
                            Some(id),
                        );
                    }
                }
                self.jmp_rel32(&l_end, JumpKind::Jmp);
                self.label(&l_skip);
            }
            self.jmp_rel32(&l_end, JumpKind::Jmp);
            self.label(&l_next_group);
        }
        self.label(&l_end);
    }

    // ========================================================================
    // resolver 体
    // ========================================================================

    /// 合成一个 `<base>.resolver`：CPUID 判定 + 返回**选中变体的地址**。
    ///
    /// 形状（与 gcc 实测同构，见 docs/wp_ifunc_real_fix.md 的反汇编对照）：
    /// ```text
    /// push/…                (经由 call 的 init 自己保存 rbx)
    /// call __dcc_cpu_indicator_init
    /// for arch in dispatch_order:        # 优先级高者先测
    ///     <feature check(arch)>          # test model[slot], 1<<bit … je .Lfail
    ///     lea  rax, [rip + base.arch]
    ///     jmp  .Lret
    /// .Lfail:
    /// lea  rax, [rip + base.default]
    /// .Lret: ret
    /// ```
    /// 返回的是**被选中变体的地址**（resolver 的返回类型是函数指针），动态链接器把它
    /// 写进 GOT / 完成 IRELATIVE —— 这正是「IFUNC 符号 `st_value` 指向真 resolver 体」
    /// 与「运行时真的选中了非 default 变体」两件事的实现点。
    pub fn emit_resolver_body(&mut self, cb: &CloneBase) {
        let name = cb.resolver_sym();
        self.align(16);
        let start = self.pos();
        self.label(&name);
        // 惰性检测：resolver 由 ld-linux 在重定位阶段调用, 那一刻没有任何 .init 跑过。
        self.call_cpu_init_with_ext_home_window();
        let l_ret = self.fresh_label();
        let mut emitted = 0usize;
        for arch in &cb.dispatch {
            let variant = cb.variant_sym(arch);
            if !self.labels.contains_key(&variant) {
                // 变体实现体不在标号表里 = 展开与发射不一致。发一条指向不存在符号的
                // `lea` 只能得到「链接期 undefined reference」或更糟的空符号, 这里
                // 直接把不一致说出来。
                self.report_internal_bug(
                    DIAG_INTERNAL,
                    format!(
                        "resolver for '{}' references variant '{variant}' which has no label \
                         in this TU",
                        cb.base
                    ),
                    "expand_target_clones must emit one defined function per clone variant; \
                     do not emit a resolver that can never be relocated",
                );
                continue;
            }
            let l_fail = self.fresh_label();
            if !self.emit_feature_check(arch, &l_fail) {
                // 位表/槽表不匹配时 emit_feature_check 已经报过 E1126；不再为该变体
                // 发「永远命中」的分支，直接跳过（default 兜底仍在）。
                continue;
            }
            self.emit_lea_sym(&variant);
            self.jmp_rel32(&l_ret, JumpKind::Jmp);
            self.label(&l_fail);
            emitted += 1;
        }
        let default_sym = cb.default_sym();
        if !self.labels.contains_key(&default_sym) {
            self.report_internal_bug(
                DIAG_INTERNAL,
                format!(
                    "resolver for '{}' has no default variant '{}' in this TU",
                    cb.base, default_sym
                ),
                "the resolver's fallback branch must reference a real body",
            );
            self.xor_eax_eax();
        } else {
            self.emit_lea_sym(&default_sym);
        }
        self.label(&l_ret);
        self.ret();
        let size = self.pos() - start;
        self.func_sizes.insert(name.clone(), size);
        let bind = if cb.is_static { STB_LOCAL } else { STB_WEAK };
        self.ifunc_synth.push(SynthFunc {
            name: name.clone(),
            bind,
        });
        // IFUNC 基符号：`st_value`/`st_size` 取自刚合成的 resolver（gcc 实测同形：
        // `IFUNC f` 与 `FUNC f.resolver` 的 value/size 完全一致）。
        let sym = self.resolve_asm_label(&cb.base).to_string();
        self.ifunc_defs.push(IfuncDef {
            name: sym,
            bind: if cb.is_static {
                STB_LOCAL
            } else if cb.is_weak {
                STB_WEAK
            } else {
                crate::codegen::shared::elf::STB_GLOBAL
            },
            resolver: name,
            is_static: cb.is_static,
            visibility: cb.visibility.clone(),
        });
        if emitted == 0 {
            // 一个非 default 分支都没发出去 = resolver 只能返回 default。那是
            // 「看起来修了、其实永远选不中」的形状, 必须明说。
            self.report_internal_bug(
                DIAG_INTERNAL,
                format!(
                    "resolver for '{}' emits no feature-check branch (dispatch list: {:?})",
                    cb.base, cb.dispatch
                ),
                "expand_target_clones must leave at least one non-default variant; a resolver \
                 that can only return the default body is the defect this module exists to fix",
            );
        }
    }

    /// `.text` 尾部的全部合成：CPUID 缓存 + init 函数 + 每个克隆基名的 resolver。
    ///
    /// 调用点必须在 `compile_rel` 里 `move_ranges` 之后、`patch_local_jumps` 之前：
    ///   * 之后 —— 自定义节搬迁要覆盖的是用户函数区间, 合成代码不能被搬走；
    ///   * 之前 —— 合成体里的 `.L` 前向跳转必须和别的函数一起被回填（未定义即 E1102）。
    /// 另外必须在 `collect_rel_symbols` 之前：resolver 的 `st_value`/`st_size`、IFUNC
    /// 符号与 `__dcc_cpu_model` 的注册都要在那一轮进表。
    pub fn emit_ifunc_synthesis(&mut self, prog: &Program) {
        let bases = collect_clone_bases(prog);
        // 「本 TU 需不需要 CPUID 部分」不能只看 `cpu_builtin_used` 这个发射时累加的
        // 旁路标志：并行 codegen（`parallel/orchestration.rs`）把函数分给 worker 自己
        // 的 `Codegen` 实例去发射，标志留在 worker 里不会并回 master。实测 -O2：
        // `resolve_a` 里已经发好了 `call __dcc_cpu_indicator_init`，master 的
        // `cpu_builtin_used` 仍是 `false` → init 函数与 `__dcc_cpu_model` 全部缺失，
        // 链接报 `undefined reference to '__dcc_cpu_model'`。因此以「已发射的代码里确实
        // 引用了这两个名字」为准：它在串行与并行两条路径上都成立（chunk 合并就是把这些
        // pending 表并回 master），也不需要往 chunk 结构里再加一份旁路字段。
        if bases.is_empty() && !self.cpu_cache_referenced() {
            return;
        }
        self.ensure_cpu_model();
        self.emit_cpu_indicator_init();
        for cb in &bases {
            self.emit_resolver_body(cb);
        }
    }

    /// 已发射的代码里是否引用了 CPUID 缓存（`__dcc_cpu_indicator_init` /
    /// `__dcc_cpu_model`）。见 [`emit_ifunc_synthesis`](Self::emit_ifunc_synthesis) 的注释。
    ///
    /// 查两张「待解析引用」表而不是扫机器码：`call_label` 进 `pending`（目标名就是标号
    /// 名），`emit_model_rip_sym` 进 `pending_code_rels`（`symbol` 就是 `__dcc_cpu_model`），
    /// 两者都是并行 chunk 合并时必并回 master 的表。
    fn cpu_cache_referenced(&self) -> bool {
        self.cpu_builtin_used
            || self
                .pending
                .iter()
                .any(|p| p.target == CPU_INIT_NAME || p.target == CPU_MODEL_NAME)
            || self
                .pending_code_rels
                .iter()
                .any(|r| r.symbol == CPU_INIT_NAME || r.symbol == CPU_MODEL_NAME)
    }

    /// `-S` 渲染合成体：绑定指示（`.globl` / `.weak`，LOCAL 什么都不发）+
    /// `.type @function` + 反汇编 + `.size`。
    ///
    /// 它们不在 `prog.decls` 里，所以 `build_asm_output` 两个渲染路径的
    /// `collect_all_funcs` 循环都看不到它们。这个渲染**只为可读性**：判断机器码
    /// 语义必须用 `objdump -d` 读 `.o`（自家反汇编器不可信，项目里反复踩过）。
    ///
    /// resolver 在 `.o` 侧是 `STB_WEAK`（与 gcc 13.3 实测同形），所以这里必须发
    /// `.weak` 而不是 `.globl`：发错了会把「同一个基名被两个 TU 展开」变成重复定义。
    pub fn render_ifunc_synth_asm(&self) -> String {
        if self.ifunc_synth.is_empty() {
            return String::new();
        }
        let labels = crate::codegen::x86_64::render_one::label_map(
            self.labels.iter().map(|(n, o)| (o, n.as_str())),
        );
        let directives = crate::codegen::x86_64::render_one::directive_map(
            self.asm_directives_at.iter().map(|(o, l)| (o, l.as_str())),
        );
        let mut out = String::new();
        out.push_str("\t.text\n");
        for sf in &self.ifunc_synth {
            let Some(layout) = self.func_asm_layout.get(&sf.name) else {
                continue;
            };
            match sf.bind {
                STB_LOCAL => {},
                crate::codegen::shared::elf::STB_WEAK => {
                    out.push_str(&format!("\t.weak {}\n", sf.name));
                },
                _ => out.push_str(&format!("\t.globl {}\n", sf.name)),
            }
            out.push_str(&format!("\t.type {}, @function\n", sf.name));
            out.push_str(&super::Codegen::render_byte_range(
                &layout.bytes,
                0,
                layout.bytes.len(),
                layout.text_start,
                &labels,
                &directives,
                &self.disasm_calls,
                &self.disasm_data,
            ));
            out.push_str(&format!("\t.size {}, .-{}\n", sf.name, sf.name));
        }
        // `__dcc_cpu_model`：`ensure_cpu_model` 只把它登记进 `globals_raw`，`.o` 侧由
        // `elf/rel_symbols.rs` 的「`globals_raw` 里但不在 `prog.decls` 里」那一轮发成
        // `STB_LOCAL STT_OBJECT`；`-S` 的两条渲染路径都不遍历 `globals_raw`（既有
        // 缺口，`int gi = 7;` 这类普通全局同样不发 `.data`，见 wp_ifunc_real_fix.md），
        // 于是 `as` 回来它会变 UND，`-S` → `as` → 链接这条链就断了。这里只补我自己
        // 合成的那一个对象，不动通用数据段渲染。必须与 `ensure_cpu_model` 同节
        // （`.data`，不是 `.bss`）：两边不一致时 `-S` 出的 `.o` 里缓存会跟重定位不同址。
        out.push_str("\t.section .data,\"aw\",@progbits\n");
        out.push_str("\t.align 8\n");
        out.push_str(&format!("\t.type {CPU_MODEL_NAME}, @object\n"));
        out.push_str(&format!("{CPU_MODEL_NAME}:\n"));
        out.push_str(&format!("\t.zero {}\n", cpu_isa::MODEL_BYTES));
        out.push_str(&format!("\t.size {CPU_MODEL_NAME}, {}\n", cpu_isa::MODEL_BYTES));
        out
    }

    // ========================================================================
    // `__builtin_cpu_supports` / `__builtin_cpu_is` / `__builtin_cpu_init`
    // ========================================================================

    /// `CPU_INIT_NAME` 唯一的 wrapper：无参、无返回值、不能走 `emit_sysv_call`（它是
    /// ifunc 惰性检测入口，自己摆的参数会被 resolver 的静态链布局假设破坏）。
    /// 但 `call_label` primitive 会撞上 E1136 —— 本函数若住在延伸池寄存器里，这次裸 call
    /// 会把家毁掉。所以统一在这里开/关 persist/restore 窗口，与 `emit_sysv_call` 同源。
    fn call_cpu_init_with_ext_home_window(&mut self) {
        let n = self.persist_ext_homes();
        self.call_label(CPU_INIT_NAME);
        self.restore_ext_homes(n);
    }

    /// `__builtin_cpu_init()`：确保检测跑过（与 gcc 一样返回 void，展开成一次调用）。
    pub fn emit_cpu_init_builtin(&mut self) {
        self.cpu_builtin_used = true;
        self.call_cpu_init_with_ext_home_window();
    }

    /// `__builtin_cpu_supports("<isa>")` → 0 / 1。
    ///
    /// 与 resolver **共用同一条 [`emit_feature_check`](Self::emit_feature_check)**，
    /// 所以「运行时 builtin 的答案」与「链接期 resolver 的选型」在任何机器上都
    /// 同真同假 —— 这条同源性就是集成测试能拿 builtin 钉「变体真被选中」的依据。
    ///
    /// 返回值语义的差异（dcc 给 0/1, gcc 给 `__cpu_model` 里的原始位值，实测
    /// `avx=512`/`sse4.2=256`）记在 `docs/wp_ifunc_real_fix.md`：C 只承诺
    /// 「非零 = 支持」，原始数值是 libgcc 字段布局的实现细节，夹具用 `!= 0` 归一
    /// 后两边逐字相同。
    pub fn emit_cpu_supports(&mut self, arg: &crate::ast::Expr, span: Span, dst: u8) {
        let (bytes, lit_span) = match string_lit_expr(arg) {
            Some((b, s)) => (b, s),
            None => {
                self.cpu_user_diag(
                    DIAG_BAD_CLONE,
                    "__builtin_cpu_supports requires a string literal feature name".to_string(),
                    span,
                    "gcc also rejects a non-literal argument; write __builtin_cpu_supports(\"avx\")"
                        .to_string(),
                );
                self.mov_imm(dst, 0);
                return;
            },
        };
        let name = c_string_name(&bytes);
        if cpu_isa::lookup(&name).is_none() {
            self.cpu_user_diag(
                DIAG_BAD_CLONE,
                format!("__builtin_cpu_supports(\"{name}\"): invalid parameter"),
                lit_span,
                format!(
                    "gcc accepts only its own feature names; dcc's known set is in \
                     semantics::cpu_isa (cloneable subset: {})",
                    cpu_isa::cloneable_names().join(", ")
                ),
            );
            self.mov_imm(dst, 0);
            return;
        }
        self.cpu_builtin_used = true;
        self.call_cpu_init_with_ext_home_window();
        let l_fail = self.fresh_label();
        let l_end = self.fresh_label();
        if !self.emit_feature_check(&name, &l_fail) {
            // 表里没有可发射的位（内部不一致, 已报）→ 明确返回 0 而不是发一段
            // 「永不命中」的检测。注意必须是 0：这里落在「编译器自己答不出」的分支上，
            // 返回 1 就是拿「CPU 支持该特性」冒充「未知」，比恒假桩更危 —— 它会
            // 让上层真的去跑一条本机不支持的指令。
            self.label(&l_fail);
            self.mov_imm(dst, 0);
            self.label(&l_end);
            return;
        }
        self.mov_imm(dst, 1);
        self.jmp_rel32(&l_end, JumpKind::Jmp);
        self.label(&l_fail);
        self.mov_imm(dst, 0);
        self.label(&l_end);
    }

    /// `__builtin_cpu_is("<name>")` → 0 / 1，**一个名字只比一个 dword**。
    ///
    /// gcc 13.3 的展开形状（本机 `gcc -S` 实测，每个名字单独编译）就是
    /// `mov __cpu_model+offset(%rax),%eax; cmp $imm,%eax; sete`，其中
    /// `off` 只可能是 0（`__cpu_vendor`）/ 4（`__cpu_type`）/ 8（`__cpu_subtype`）。
    /// 哪个名字比哪个槽、比什么值，全部由 `cpu_isa::CPU_IS_NAMES` 一张表给出：
    ///   * `Vendor(kind)`  → `SLOT_VENDOR_KIND`，值 = leaf0 三个厂商 dword 的识别结果
    ///   * `Type(t)`       → `SLOT_CPU_TYPE`，值 = `cpu_isa::type_id(t)`
    ///   * `Subtype(s)`    → `SLOT_CPU_SUBTYPE`，值 = `cpu_isa::subtype_id(s)`
    /// 两个槽由 [`Codegen::emit_cpu_indicator_init`] 里的 family/model 解码 +
    /// 分类链填写（同一张 `cpu_isa::CLASSIFY` 表），所以比较基准与写入值是
    /// 由同一次表遍历派生的 —— 这里不写任何型号名、id 或槽偏移的数字。
    ///
    /// 名字不在表里（gcc 也拒）→ 逐调用点 E1125，**不发「恒 0」的比较**：
    /// 表里没有的名字在 gcc 里是编译错误，dcc 若默默返回 0 就等于把用户代码里
    /// 的拼写错误变成运行时「这台 CPU 不是 haswell」的假答案。
    pub fn emit_cpu_is(&mut self, arg: &crate::ast::Expr, span: Span, dst: u8) {
        let (bytes, lit_span) = match string_lit_expr(arg) {
            Some((b, s)) => (b, s),
            None => {
                self.cpu_user_diag(
                    DIAG_BAD_CLONE,
                    "__builtin_cpu_is requires a string literal vendor/model name".to_string(),
                    span,
                    "write __builtin_cpu_is(\"intel\")".to_string(),
                );
                self.mov_imm(dst, 0);
                return;
            },
        };
        let name = c_string_name(&bytes);
        let Some(matched) = cpu_isa::cpu_is_match(&name) else {
            self.cpu_user_diag(
                DIAG_BAD_CLONE,
                format!("__builtin_cpu_is(\"{name}\"): invalid parameter"),
                lit_span,
                format!(
                    "gcc 13.3 accepts exactly these {} names (measured one by one): {}",
                    cpu_isa::CPU_IS_NAMES.len(),
                    cpu_isa::cpu_is_names().join(", ")
                ),
            );
            self.mov_imm(dst, 0);
            return;
        };
        // (槽, 期望值)：id 解不出来说明 `CPU_IS_NAMES` 与 `CLASSIFY` 不一致
        // （不变量测试 `classify_every_id_is_addressable` 钉住了这件事）。
        // 这时必须报内部错并返回，**不能**退化成「比 0」—— 比 0 会让所有型号名
        // 在未分类 CPU 上答 1、在已分类 CPU 上答 0，是两个方向的错答。
        let compare = match *matched {
            cpu_isa::CpuIsMatch::Vendor(kind) => (cpu_isa::SLOT_VENDOR_KIND, kind),
            cpu_isa::CpuIsMatch::Type(t) => match cpu_isa::type_id(t) {
                Some(id) => (cpu_isa::SLOT_CPU_TYPE, id),
                None => {
                    self.report_internal_bug(
                        DIAG_INTERNAL,
                        format!("cpu_is name '{name}' compares type '{t}' which has no id"),
                        "every CpuIsMatch::Type target must appear in cpu_isa::CLASSIFY so \
                         type_id() can derive its id; do not emit a comparison against 0",
                    );
                    self.mov_imm(dst, 0);
                    return;
                },
            },
            cpu_isa::CpuIsMatch::Subtype(s) => match cpu_isa::subtype_id(s) {
                Some(id) => (cpu_isa::SLOT_CPU_SUBTYPE, id),
                None => {
                    self.report_internal_bug(
                        DIAG_INTERNAL,
                        format!("cpu_is name '{name}' compares subtype '{s}' which has no id"),
                        "every CpuIsMatch::Subtype target must appear in cpu_isa::CLASSIFY so \
                         subtype_id() can derive its id; do not emit a comparison against 0",
                    );
                    self.mov_imm(dst, 0);
                    return;
                },
            },
        };
        let (slot, want) = compare;
        self.cpu_builtin_used = true;
        self.call_cpu_init_with_ext_home_window();
        let l_fail = self.fresh_label();
        let l_end = self.fresh_label();
        self.emit_model_rip(&RIP_MOV_EAX_M32, slot, None);
        self.cmp_eax_imm32(want);
        self.jmp_rel32(&l_fail, JumpKind::Jne);
        self.mov_imm(dst, 1);
        self.jmp_rel32(&l_end, JumpKind::Jmp);
        self.label(&l_fail);
        self.mov_imm(dst, 0);
        self.label(&l_end);
    }

    /// `__builtin_cpu_supports()` 无参：gcc 是 hard error（「missing required argument」）。
    /// 这里同样报错，而不是发一个「返回 0」的静默桩。
    pub fn emit_cpu_supports_missing_arg(&mut self, span: Span, dst: u8) {
        self.cpu_user_diag(
            DIAG_BAD_CLONE,
            "__builtin_cpu_supports requires exactly one argument".to_string(),
            span,
            "write __builtin_cpu_supports(\"avx\")".to_string(),
        );
        self.mov_imm(dst, 0);
    }

    /// `__builtin_cpu_is()` 无参：同上。
    pub fn emit_cpu_is_missing_arg(&mut self, span: Span, dst: u8) {
        self.cpu_user_diag(
            DIAG_BAD_CLONE,
            "__builtin_cpu_is requires exactly one argument".to_string(),
            span,
            "write __builtin_cpu_is(\"intel\")".to_string(),
        );
        self.mov_imm(dst, 0);
    }

    fn cpu_user_diag(&mut self, code: &'static str, msg: String, span: Span, hint: String) {
        self.diagnostics.push(Diagnostic {
            sev: Severity::Error,
            code,
            msg,
            span,
            hint: Some(hint),
            source_line: None,
        });
    }

    /// ET_EXEC 直出路径不支持：明说而不是静默降级。
    pub fn report_no_exec_ifunc_support(&mut self) {
        self.diagnostics.push(Diagnostic {
            sev: Severity::Error,
            code: DIAG_NO_EXEC_SUPPORT,
            msg: "IFUNC / CPUID synthesis (target_clones, __attribute__((ifunc)), \
                  __builtin_cpu_*) is only emitted for relocatable output"
                .to_string(),
            span: Span::point("<codegen>", 0, 0),
            hint: Some(
                "compile to a .o and link it (the default `dcc prog.c -o prog` path \
                        produces a .o and links with the host linker, which resolves IFUNC \
                        natively); -fbuiltin-linker / direct ET_EXEC codegen has no \
                        R_X86_64_IRELATIVE machinery"
                    .to_string(),
            ),
            source_line: None,
        });
    }

    // ========================================================================
    // 判定发射器（resolver 与 builtin 共用）
    // ========================================================================

    /// 发一段「`arch` 在当前 CPU 上可用吗」的判定序列，不可用时跳到 `fail`。
    ///
    /// 语义：`cpu_isa::cpuid_bits(arch)` 里每一位都必须置位（逐位
    /// `test model[slot], 1<<bit` + `je fail`；**不聚合成多位掩码后只查非零** ——
    /// 那会让「只置了其中一位」也判通过），`xcr0_mask` 非零时要求
    /// `(XCR0 & mask) == mask`（`and`/`cmp`/`jne`，与内置链接器旧路径同形）。
    ///
    /// 返回 `false` = 表里没有可发射的位（编译器内部不一致，已报 E1126/E1127），
    /// 调用方不得把「没有检测」当成「检测通过」。
    pub fn emit_feature_check(&mut self, arch: &str, fail: &str) -> bool {
        let bits = cpu_isa::cpuid_bits(arch);
        if bits.is_empty() {
            self.report_internal_bug(
                DIAG_INTERNAL,
                format!("cpu_isa has no CPUID bits for ISA '{arch}'"),
                "collect/validate must reject unknown or non-cloneable ISA names before \
                 synthesis; an empty bit list would emit no check at all",
            );
            return false;
        }
        let mut ok = true;
        for b in bits {
            let Some(slot) = cpu_isa::cache_slot(b.leaf, b.subleaf, b.reg) else {
                self.report_internal_bug(
                    DIAG_INTERNAL,
                    format!(
                        "cpu_isa bit (leaf {:#x}, sub-leaf {}, reg {}, bit {}) for '{arch}' has no cache slot in __dcc_cpu_model",
                        b.leaf, b.subleaf, b.reg, b.bit
                    ),
                    "cache_slot() must cover every (leaf, sub-leaf, reg) triple used by FEATURES; otherwise the emitted check reads a slot that __dcc_cpu_indicator_init never fills",
                );
                ok = false;
                continue;
            };
            if !cpu_isa::probe_covers(b.leaf, b.subleaf, b.reg) {
                self.report_internal_bug(
                    DIAG_INTERNAL,
                    format!(
                        "no CPUID probe fills leaf {:#x} sub-leaf {} reg {} (needed by '{arch}')",
                        b.leaf, b.subleaf, b.reg
                    ),
                    "cpu_isa::PROBES must fill every slot that cache_slot() can hand out",
                );
                ok = false;
                continue;
            }
            self.emit_model_rip(&RIP_TEST_M32_IMM32, slot, Some(1u32 << b.bit as u32));
            self.jmp_rel32(fail, JumpKind::Je);
        }
        let xcr0 = cpu_isa::xcr0_mask(arch);
        if xcr0 != 0 {
            self.emit_model_rip(&RIP_MOV_EAX_M32, cpu_isa::SLOT_XCR0_LOW, None);
            self.and_eax_imm32(xcr0);
            self.cmp_eax_imm32(xcr0);
            self.jmp_rel32(fail, JumpKind::Jne);
        }
        ok
    }
}

/// C 字符串字面量 → Rust 名字（去掉结尾 `\0`，非法 UTF-8 返回空串由调用方报错）。
fn c_string_name(bytes: &[u8]) -> String {
    let trimmed = match bytes.last() {
        Some(0) => &bytes[..bytes.len() - 1],
        _ => bytes,
    };
    String::from_utf8_lossy(trimmed).into_owned()
}

// ============================================================================
// 机器码发射原语（RIP-relative 形式与它们各自的 `L - f` 加数）
// ============================================================================
//
// R_X86_64_PC32 的语义是 `disp32 = S + A - P`，P 是 **disp32 字段自身**的地址。
// CPU 计算 RIP 相对有效地址用的是「本指令之后」的地址，即 `start + L`（L = 整条
// 指令长度）。于是 `EA = S + A + (L - f)`，`f` 是 disp32 字段相对指令起始的偏移。
// 想要 `EA = S + off`，必须 `A = off - (L - f)`。
//
// 这一条在 `F7 05 <disp32> <imm32>`（test m32,imm32：L=10、f=2 → +8）与
// `8B 05 <disp32>`（mov r32,m32：L=6、f=2 → +4）之间**不同**：把两者都写成 -4 是最
// 容易「看起来对、跑起来错」的地方（disp32 会读/写到目标之后 4 字节的槽），所以每个
// 发射形式自带 `delta = L - f`，而不是共用一个常量。

/// 一条以 disp32 为操作数的指令的编码形式。
struct RipForm {
    /// disp32 之前要发的字节
    prefix: &'static [u8],
    /// `L - f`：disp32 字段到「下一条指令起始」的距离
    delta: i64,
    /// disp32 之后是否还有 imm32
    imm_after: bool,
}

const RIP_MOV_EAX_M32: RipForm = RipForm {
    prefix: &[0x8B, 0x05],
    delta: 4,
    imm_after: false,
};
const RIP_MOV_M32_EAX: RipForm = RipForm {
    prefix: &[0x89, 0x05],
    delta: 4,
    imm_after: false,
};
const RIP_MOV_M32_EBX: RipForm = RipForm {
    prefix: &[0x89, 0x1D],
    delta: 4,
    imm_after: false,
};
const RIP_MOV_M32_ECX: RipForm = RipForm {
    prefix: &[0x89, 0x0D],
    delta: 4,
    imm_after: false,
};
const RIP_MOV_M32_EDX: RipForm = RipForm {
    prefix: &[0x89, 0x15],
    delta: 4,
    imm_after: false,
};
// `mov disp32(%rip), %esi` / `%edi`（`89 /r`，ModRM = reg<<3 | 0x05）：分类链把
// 调整后的 model 放在 `esi`，需要它自己的形式 —— `delta` 与其余 `8B/89` 形式一样是 4。
const RIP_MOV_M32_ESI: RipForm = RipForm {
    prefix: &[0x89, 0x35],
    delta: 4,
    imm_after: false,
};
const RIP_TEST_M32_IMM32: RipForm = RipForm {
    prefix: &[0xF7, 0x05],
    delta: 8,
    imm_after: true,
};
const RIP_MOV_M32_IMM32: RipForm = RipForm {
    prefix: &[0xC7, 0x05],
    delta: 8,
    imm_after: true,
};
const RIP_LEA_RAX: RipForm = RipForm {
    prefix: &[0x48, 0x8D, 0x05],
    delta: 4,
    imm_after: false,
};

impl Codegen {
    /// 发一条以 `__dcc_cpu_model` 为基址的 RIP-relative 访存指令。
    fn emit_model_rip(&mut self, form: &RipForm, slot: usize, imm: Option<u32>) {
        self.emit_model_rip_sym(form, CPU_MODEL_NAME, cpu_isa::slot_offset(slot) as i64, imm);
    }

    /// [`Self::emit_model_rip`] 的底层形式：符号名与「符号内字节偏移」显式给出。
    fn emit_model_rip_sym(&mut self, form: &RipForm, sym: &str, off_in_sym: i64, imm: Option<u32>) {
        self.emit_insn(&Insn::raw_bytes(form.prefix));
        let disp_pos = self.pos();
        self.emit_insn(&Insn::raw_bytes(&[0, 0, 0, 0]));
        let addend = off_in_sym - form.delta;
        self.pending_code_rels.push(PendingCodeRel {
            offset: disp_pos,
            symbol: sym.to_string(),
            rel_type: R_X86_64_PC32,
            addend,
        });
        // 只服务 `-S` 渲染的可读性，不参与任何语义判定。
        self.disasm_data.insert(disp_pos, sym.to_string());
        if form.imm_after {
            let v = imm.unwrap_or(0);
            self.emit_insn(&Insn::raw_bytes(&v.to_le_bytes()));
        }
    }

    /// 引用一个**已定义标号**（变体实现体）：`lea rax, [rip + <sym>]`。
    fn emit_lea_sym(&mut self, sym: &str) {
        self.emit_insn(&Insn::raw_bytes(RIP_LEA_RAX.prefix));
        let disp_pos = self.pos();
        self.emit_insn(&Insn::raw_bytes(&[0, 0, 0, 0]));
        self.pending_code_rels.push(PendingCodeRel {
            offset: disp_pos,
            symbol: sym.to_string(),
            rel_type: R_X86_64_PC32,
            addend: -RIP_LEA_RAX.delta,
        });
        self.disasm_data.insert(disp_pos, sym.to_string());
    }

    fn mov_eax_imm32(&mut self, v: u32) {
        self.emit_insn(&Insn::mov_eax_imm32(v));
    }

    fn mov_ecx_imm32(&mut self, v: u32) {
        self.emit_insn(&Insn::mov_ecx_imm32(v));
    }

    /// `and eax, imm32`。不能用 `83 E0 ib`：avx512 的 XCR0 掩码 `0xE6` 超出有符号
    /// imm8 的范围（`0x83 /4 ib` 的 imm8 是 sign-extended，`0xE6` 会变成 `-26`）。
    fn and_eax_imm32(&mut self, v: u32) {
        self.emit_insn(&Insn::and_eax_imm32(v));
    }

    fn cmp_eax_imm32(&mut self, v: u32) {
        self.emit_insn(&Insn::cmp_eax_imm32(v as i32));
    }

    fn cpuid(&mut self) {
        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Cpuid));
    }

    fn xgetbv(&mut self) {
        self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::Xgetbv));
    }

    fn xor_ecx_ecx(&mut self) {
        self.emit_insn(&Insn::alu_rm_reg_reg(
            crate::codegen::x86_64::opcode::AluOp::Xor,
            crate::codegen::shared::asm_operand::Width::B32,
            1,
            1,
        ));
    }

    fn xor_eax_eax(&mut self) {
        self.emit_insn(&Insn::alu_rm_reg_reg(
            crate::codegen::x86_64::opcode::AluOp::Xor,
            crate::codegen::shared::asm_operand::Width::B32,
            0,
            0,
        ));
    }

    // ---- 分类链需要的形式（32 位通用寄存器间运算）----
    //
    // ModRM 一律按 `reg<<3 | rm` 现算，不把六个寄存器的编码写死成查表：查表就是
    // 第二份真源（新增一个寄存器时查表会漏，而公式不会）。

    /// `mov dst, src`（`89 /r`，`/r` 的 reg 字段是**源**）。
    fn mov_r32_r32(&mut self, dst: u8, src: u8) {
        self.emit_insn(&Insn::mov_rm_reg(
            crate::codegen::shared::asm_operand::Width::B32,
            dst,
            src,
        ));
    }

    /// `add dst, src`（`01 /r`）。
    fn add_r32_r32(&mut self, dst: u8, src: u8) {
        self.emit_insn(&Insn::alu_rm_reg_reg(
            crate::codegen::x86_64::opcode::AluOp::Add,
            crate::codegen::shared::asm_operand::Width::B32,
            dst,
            src,
        ));
    }

    /// `mov r32, imm32`（`B8 + r`）。
    fn mov_r32_imm32(&mut self, r: u8, v: u32) {
        use crate::codegen::shared::asm_operand::Width;
        use crate::codegen::x86_64::opcode::{Operand, X86_64Opcode};
        let insn = Insn::new(
            X86_64Opcode::MovRm {
                rip_relative: false,
                width: Width::B32,
                dst: Operand::Reg(r, Width::B32),
                src: Operand::Imm(v as i64),
            },
            &[],
        );
        self.emit_insn(&insn);
    }

    /// `shr r32, imm8`（`C1 /5 ib`）。
    fn shr_r32_imm8(&mut self, r: u8, n: u8) {
        use crate::codegen::shared::asm_operand::Width;
        use crate::codegen::x86_64::opcode::{Operand, ShiftAmount, ShiftOp, X86_64Opcode};
        let insn = Insn::new(
            X86_64Opcode::Shift {
                op: ShiftOp::Shr,
                width: Width::B32,
                dst: Operand::Reg(r, Width::B32),
                amount: ShiftAmount::Imm8(n),
            },
            &[],
        );
        self.emit_insn(&insn);
    }

    /// `and r32, imm32`（`81 /4 id`）。不能用 `83 /4 ib`：扩展 model 的掩码
    /// `0xf0` 与扩展 family 的 `0xff` 超出有符号 imm8 的范围。
    fn and_r32_imm32(&mut self, r: u8, v: u32) {
        // 手写形态固定是 81 /4 id (mod=11, 4 字节 imm)。encoder 里 AluRm 只在 imm 超出 i8
        // 时才走 81 / id; 对 imm8 范围内的值会走 83 / ib。为保持快照逐字节一致,
        // 本函数直接发手写字节, 不走 typed 路径。
        self.emit_insn(&Insn::raw_bytes(&[0x81, 0xE0 | r]));
        self.emit_insn(&Insn::raw_bytes(&v.to_le_bytes()));
    }

    /// `cmp r32, imm32`（`81 /7 id`）。同上, 手写固定 81 /7 id, 不走 typed。
    fn cmp_r32_imm32(&mut self, r: u8, v: u32) {
        self.emit_insn(&Insn::raw_bytes(&[0x81, 0xF8 | r]));
        self.emit_insn(&Insn::raw_bytes(&v.to_le_bytes()));
    }

    fn ret(&mut self) {
        // 步骤 3: 走类型化 Ret{add_imm: None} (C3).
        let insn =
            Insn::new(crate::codegen::x86_64::opcode::X86_64Opcode::Ret { add_imm: None }, &[]);
        if !self.emit_insn(&insn) {
            self.emit_insn(&Insn::noarg(crate::codegen::x86_64::opcode::NoArgOp::RetNear));
        }
    }
}

/// 32 位寄存器的 ModRM 编号（`eax`=0 … `edi`=7），只在合成 CPUID/分类体时用。
/// 这里只列用到的那几个：`rbx` 由序言保存，`esp`/`ebp` 不能碰，`r8-r15` 需要 REX
/// 前缀而 `emit_feature_check` 只用 `eax`，所以四个低位寄存器足够。
const R_EAX: u8 = 0;
const R_ECX: u8 = 1;
const R_EDX: u8 = 2;
const R_ESI: u8 = 6;
const R_EDI: u8 = 7;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Severity;

    fn parse_expand(src: &str) -> (Program, Vec<Diagnostic>) {
        let (mut prog, mut diags) = crate::parser::parse(src.as_bytes(), "t.c");
        let before = diags.len();
        expand_target_clones(&mut prog, &mut diags);
        let _ = before;
        // 丢掉 parse 阶段的诊断，只留展开阶段的（用例只关心 E1125/W 的形状）。
        let mut out = Vec::new();
        for d in diags.into_iter().skip(before.max(0)) {
            out.push(d);
        }
        (prog, out)
    }

    fn decl_names(prog: &Program) -> Vec<String> {
        prog.decls
            .iter()
            .filter_map(|d| match d {
                Decl::Func(f) => Some(f.name.name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn variant_symbol_name_matches_gcc_measured_shape() {
        // 实测 gcc 13.3: compute.default / compute.sse4_2 / multiply.avx / f.avx
        assert_eq!(mangle_variant_sym("compute", "default"), "compute.default");
        assert_eq!(mangle_variant_sym("compute", "sse4.2"), "compute.sse4_2");
        assert_eq!(mangle_variant_sym("multiply", "avx"), "multiply.avx");
        assert_eq!(mangle_variant_sym("f", "avx512f"), "f.avx512f");
        assert_eq!(resolver_sym("compute"), "compute.resolver");
    }

    #[test]
    fn expansion_emits_one_body_per_variant_and_a_bodiless_base() {
        let src = r#"
            __attribute__((target_clones("avx,sse4.2,default")))
            int multiply(int a, int b) { return a * b; }
        "#;
        let (prog, diags) = parse_expand(src);
        assert!(
            !diags.iter().any(|d| matches!(d.sev, Severity::Error)),
            "unexpected errors: {diags:?}"
        );
        let names = decl_names(&prog);
        for want in [
            "multiply",
            "multiply.avx",
            "multiply.sse4_2",
            "multiply.default",
        ] {
            assert!(names.iter().any(|n| n == want), "missing '{want}' in {names:?}");
        }
        for d in &prog.decls {
            if let Decl::Func(f) = d {
                if f.name.name == "multiply" {
                    assert!(f.body.is_none(), "base name must stay bodiless");
                    assert_eq!(f.target_clones.len(), 3, "base keeps the marker");
                } else {
                    assert!(f.body.is_some(), "variant '{}' needs its own body", f.name.name);
                    assert!(f.is_static && f.is_used_attr);
                    assert!(f.target_clones.is_empty());
                }
            }
        }
        let bases = collect_clone_bases(&prog);
        assert_eq!(bases.len(), 1);
        // 优先级来自 cpu_isa::dispatch_order：avx(65) 先于 sse4.2(45)
        assert_eq!(bases[0].dispatch, vec!["avx".to_string(), "sse4.2".to_string()]);
        assert_eq!(bases[0].default_arch, "default");
    }

    #[test]
    fn missing_default_is_an_error() {
        let src = r#"
            __attribute__((target_clones("avx,avx2")))
            int f(void) { return 0; }
        "#;
        let (prog, diags) = parse_expand(src);
        assert!(
            diags
                .iter()
                .any(|d| d.code == DIAG_BAD_CLONE && matches!(d.sev, Severity::Error)),
            "expected E1125, got {diags:?}"
        );
        // 报错后不得留下半个展开结果
        assert!(collect_clone_bases(&prog).is_empty());
    }

    #[test]
    fn unknown_isa_name_is_an_error_not_a_silent_never_matching_check() {
        let src = r#"
            __attribute__((target_clones("avx999,default")))
            int f(void) { return 0; }
        "#;
        let (_, diags) = parse_expand(src);
        assert!(
            diags.iter().any(|d| d.code == DIAG_BAD_CLONE
                && d.msg.contains("avx999")
                && matches!(d.sev, Severity::Error)),
            "expected E1125 about avx999, got {diags:?}"
        );
    }

    #[test]
    fn single_variant_is_a_warning_and_stays_a_normal_function() {
        let src = r#"
            __attribute__((target_clones("avx")))
            int f(void) { return 0; }
        "#;
        let (prog, diags) = parse_expand(src);
        assert!(
            diags
                .iter()
                .any(|d| d.code == DIAG_BAD_CLONE && matches!(d.sev, Severity::Warning)),
            "expected a warning, got {diags:?}"
        );
        assert!(collect_clone_bases(&prog).is_empty(), "no IFUNC must be produced");
    }

    #[test]
    fn nested_function_in_cloned_body_is_rejected() {
        let src = r#"
            __attribute__((target_clones("avx,default")))
            int f(int n) {
                int g(void) { return n; }
                return g();
            }
        "#;
        let (prog, diags) = parse_expand(src);
        assert!(
            diags.iter().any(|d| d.code == DIAG_BAD_CLONE
                && d.msg.contains("nested")
                && matches!(d.sev, Severity::Error)),
            "expected E1125 about nested functions, got {diags:?}"
        );
        assert!(collect_clone_bases(&prog).is_empty());
    }

    #[test]
    fn feature_check_reads_only_table_derived_slots() {
        // 目的：证明 `emit_feature_check` 能拿到的每一位都只可能来自 `cpu_isa`。
        // 本模块不直接手推位号：它只调 `cpuid_bits` / `cache_slot` / `slot_masks` /
        // `xcr0_mask`，所以只要下面这个「槽聚合与位表一致」的性质成立，发射器就不
        // 可能读到另一份表。“哪一位真正进了哪条机器码”的可证伪断言在集成测试
        // `attr_target_clones_resolver_bits_come_from_cpu_isa_table`（直接反回 `.o` 里的
        // resolver 字节：从指令形式算出 `L - f`、把重定位加数还原成槽号，再与位表的
        // `(cache_slot(leaf,sub-leaf,reg), bit)` 逐条比对），以及
        // `attr_target_clones_select_resolver_dispatch_order`（判定链顺序）。
        for arch in ["avx", "avx2", "sse4.2", "avx512f", "fma"] {
            let bits = cpu_isa::cpuid_bits(arch);
            assert!(!bits.is_empty(), "{arch} 在 cpu_isa 里没有位");
            let masks = cpu_isa::slot_masks(arch).expect("slot_masks 对合法 ISA 不返回 None");
            for b in bits {
                let slot = cpu_isa::cache_slot(b.leaf, b.subleaf, b.reg)
                    .unwrap_or_else(|| panic!("{arch} 的位 {b:?} 没有缓存槽"));
                assert!(
                    cpu_isa::probe_covers(b.leaf, b.subleaf, b.reg),
                    "{arch} 的位 {b:?} 不被 PROBES 采集：init 体从不填这一槽"
                );
                assert!(
                    masks
                        .iter()
                        .any(|(s, m)| *s == slot && (m & (1u32 << b.bit)) != 0),
                    "{arch}: 位 {b:?} 没进槽 {slot} 的聚合掩码 {masks:?}"
                );
            }
            // XCR0 需求也必须来自表（本模块不得自己认“哪几个名字算 avx 家族”）。
            // 下面的期望值是**独立 oracle**（Intel SDM 的 XCR0 位定义：bit1=SSE/YMM 状态、
            // bit2=AVX 状态、bit5/6/7=opmask/ZMM_hi256/hi16_ZMM），写在这里的作用是让
            // `cpu_isa` 的 XCR0 列被改错时本测试变红，不是第二份实现。
        }
        assert_eq!(cpu_isa::xcr0_mask("avx"), 0x6);
        assert_eq!(cpu_isa::xcr0_mask("avx2"), 0x6);
        assert_eq!(cpu_isa::xcr0_mask("avx512f"), 0xE6);
        assert_eq!(cpu_isa::xcr0_mask("sse4.2"), 0);
        assert_eq!(cpu_isa::xcr0_mask("default"), 0);
    }
}
