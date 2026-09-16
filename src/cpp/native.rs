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
//!   - 虚函数 / 虚继承 / vtable 发射
//!   - 多继承 / 成员指针
//!   - 模板 / 异常 / RTTI
//!   - Dependent / MemberPointer 类型

use std::collections::{HashMap, HashSet};

use crate::ast;
use crate::cpp::abi_layout::{AbiCalculator, ClassAbi, ItaniumAbiCalculator};
use crate::cpp::feature_matrix::{CppFeature, CppPath};
use crate::cpp::hir::HirTranslationUnit;
use crate::cpp::lower_ir::{IrLowerer, LowerIrError, LowerIrOutput};
use crate::cpp::parser::CppDecl;
use crate::cpp::sema::{Access, ClassId, CxxSymbol, CxxSymbolKind, CxxType, Sema};
use crate::diagnostics::Span;
use crate::ssa;

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

/// 把 C++ 顶层声明通过原生路径降级为 ast::Program.
///
/// 流程:
/// 1. `Sema` 构建 HIR + 收集符号表
/// 2. 两阶段 ABI 计算: 先注册所有 class + method, 再 compute
/// 3. `IrLowerer` 产出 SSA 函数 + vtable 数据
/// 4. 把每个 SSA 函数 `rebuild_block` 回 ast::Block, 组装 FuncDecl
/// 5. 为每个 class 生成 `Decl::StructDef`, 名字/字段顺序与 `lower_type` 一致
/// 6. 把 vtable 数据降级为 `Decl::Var` (extern 数组) 占位 (Stage 5 完整发射)
pub fn lower_decls_native(decls: &[CppDecl]) -> Result<NativeLowered, NativeError> {
    // Step 1: 构建 Sema + HIR
    let mut sema = Sema::new();
    if let Err(e) = sema.build_hir_from_decls(decls) {
        return Err(NativeError::Unsupported(e));
    }
    // 先克隆符号表 (sema.into_hir 只消费 hir, 但 clone 更简洁安全)
    let symbols: Vec<CxxSymbol> = sema.symbols().to_vec();
    // 收集所有类的基类信息 (用于 ABI 布局)
    let class_bases: HashMap<ClassId, Vec<ClassId>> = collect_class_bases(&sema, &symbols);
    let hir: HirTranslationUnit = sema.into_hir();

    // Step 2: 计算 ABI (两阶段: 先全部注册, 再 compute)
    let abi_map: HashMap<ClassId, ClassAbi> = build_class_abis(&symbols, &class_bases)?;

    // Step 3: lower HIR -> SSA
    let mut lowerer = IrLowerer::new(&hir, &symbols);
    for (cid, abi) in &abi_map {
        lowerer.register_abi(*cid, abi.clone());
    }
    lowerer.lower()?;
    let output: LowerIrOutput = lowerer.finish();

    // Step 4 + 5: SSA 函数 + struct 定义 -> ast::Program
    let prog = build_program(&output, &abi_map, &hir, &symbols)?;

    // Step 6: vtable 数据暂作 rodata 记号, Stage 5 完整发射.
    Ok(NativeLowered {
        prog,
        vtables: output.vtables,
        rodata: output.rodata,
    })
}

// === ABI 计算 (两阶段) ===

/// 从 Sema 中收集每个类的直接非虚基类列表。
fn collect_class_bases(sema: &Sema, symbols: &[CxxSymbol]) -> HashMap<ClassId, Vec<ClassId>> {
    let mut result = HashMap::new();
    for sym in symbols {
        if !matches!(sym.kind, CxxSymbolKind::Class) {
            continue;
        }
        let cid = ClassId::from(sym.id);
        let mut bases = Vec::new();
        if let Some(info) = sema.get_class_info(cid) {
            for base in &info.bases {
                if !base.is_virtual {
                    bases.push(base.class);
                }
            }
        }
        result.insert(cid, bases);
    }
    result
}

fn build_class_abis(
    symbols: &[CxxSymbol],
    class_bases: &HashMap<ClassId, Vec<ClassId>>,
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
            {
                fields.push((s2.name.clone(), s2.ty.clone()));
            }
        }
        let bases = class_bases.get(&cid).cloned().unwrap_or_default();
        // Stage 4: has_virtual 由方法注册阶段决定，这里先置 false
        calc.register_class(cid, bases, fields, false);
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
        let Some(owner_sym_id) = sym.owner else { continue };
        let cid = ClassId::from(owner_sym_id);
        // 判断是否为虚函数: 通过 sema 的 virtual_methods 列表
        // 这里简化: 暂时都按非虚处理, Stage 5 再完善虚函数
        let is_virtual = false;
        calc.register_method_with_symbol(
            cid,
            sym.name.clone(),
            sym.name.clone(), // Stage 4: 用 mangle.rs 的 Itanium 修饰名
            is_virtual,
            cid,
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
        let Some(abi) = abi_map.get(&cid) else { continue };
        let name = sym.name.clone();
        if emitted_structs.contains(&name) {
            continue;
        }
        emitted_structs.insert(name.clone());

        // 收集展平字段 (基类在前, 自身字段在后; 与 lower_ir::lower_class_type 保持一致)
        let fields = collect_flat_struct_fields(cid, abi, abi_map, symbols)?;
        let struct_def = ast::Decl::StructDef(ast::StructDef {
            name: ast::Ident { name, span: dummy_span() },
            fields,
            is_union: false,
            span: dummy_span(),
        });
        prog.decls.push(struct_def);
    }

    // 再发射函数 (FuncDecl): SSA -> ast::Block -> FuncDecl
    for func in &output.functions {
        let decl = build_func_decl(func)?;
        prog.decls.push(decl);
    }

    // 最后发射 vtable 占位 (Decl::Var, extern, Stage 5 替换为真实数据)
    for (name, _entries) in &output.vtables {
        prog.decls.push(ast::Decl::Var(ast::VarDecl {
            name: ast::Ident {
                name: name.clone(),
                span: dummy_span(),
            },
            ty: ast::Type::Ptr(Box::new(ast::QualType::new(ast::Type::UChar))),
            init: None,
            span: dummy_span(),
            is_extern: true,
            is_static: false,
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
    let _ = hir; // 当前未直接读 (已通过 lower_ir 用过), 保留参数便于扩展
    Ok(prog)
}

/// 与 lower_ir::lower_class_type 一致的字段展平顺序.
fn collect_flat_struct_fields(
    cid: ClassId,
    abi: &ClassAbi,
    abi_map: &HashMap<ClassId, ClassAbi>,
    symbols: &[CxxSymbol],
) -> Result<Vec<ast::Field>, NativeError> {
    let mut out = Vec::new();
    // 基类在前
    for base in &abi.bases {
        let Some(base_abi) = abi_map.get(&base.base) else {
            return Err(NativeError::Internal(format!(
                "missing ABI for base class {}",
                base.base.0
            )));
        };
        let mut sub = collect_flat_struct_fields(base.base, base_abi, abi_map, symbols)?;
        // 把子字段的 offset 加上 base.offset (lower_ir 同样做法)
        for f in &mut sub {
            f.offset += base.offset as usize;
        }
        out.extend(sub);
    }
    // 自身字段 (从 symbols 表取真实类型)
    for fl in &abi.fields {
        let field_ty = lookup_field_type(cid, &fl.name, symbols).unwrap_or(ast::Type::Int);
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
    Ok(out)
}

/// 在符号表里查字段类型 (owner == cid, name == f.name).
fn lookup_field_type(cid: ClassId, name: &str, symbols: &[CxxSymbol]) -> Option<ast::Type> {
    let owner_sym = cid.as_symbol();
    symbols
        .iter()
        .find(|s| s.owner == Some(owner_sym) && s.name == name)
        .and_then(|s| cxx_ty_to_ast_ty(&s.ty))
}

/// CxxType -> ast::Type 的最小子集映射 (与 lower_ir::lower_type 保持一致).
/// 失败时返回 None, 让 fallback 兜底.
fn cxx_ty_to_ast_ty(ty: &CxxType) -> Option<ast::Type> {
    // 复用 lower_ir 的 lower_type: 构造一个临时 lowerer.
    // 如果类型涉及 Class 且没有注册 ABI, 会失败并返回 None.
    let hir = HirTranslationUnit::default();
    let symbols: Vec<CxxSymbol> = Vec::new();
    let lowerer = IrLowerer::new(&hir, &symbols);
    match lowerer.lower_type(ty) {
        Ok(t) => Some(t),
        Err(_) => None,
    }
}

// === FuncDecl 组装 ===

fn build_func_decl(func: &ssa::Function) -> Result<ast::Decl, NativeError> {
    let name = func.name.clone();
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
        is_weak: false,
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

// === Feature gating 辅助 ===

/// 检查一组声明是否全部走 native path 可行 (auto-mode 用作 fallback 决策).
/// 不支持特性 (虚函数/模板/异常/...) 任一出现 -> 返回 false.
pub fn native_supported(decls: &[CppDecl]) -> Result<(), NativeError> {
    for d in decls {
        match d {
            CppDecl::MemberFn { is_virtual: true, .. } => {
                if !CppFeature::MemberFunction.native_supported() {
                    return Err(NativeError::Unsupported(
                        "virtual member function".to_string(),
                    ));
                }
            }
            // Stage 4: 非虚继承走 native; 虚继承 (Stage 6) 拒绝
            CppDecl::Class(cd) => {
                if cd.bases.iter().any(|b| b.is_virtual) {
                    return Err(NativeError::Unsupported(
                        "virtual inheritance".to_string(),
                    ));
                }
                let has_base = !cd.bases.is_empty();
                if has_base && !CppFeature::NonVirtualInheritance.native_supported() {
                    return Err(NativeError::Unsupported(
                        "non-virtual inheritance".to_string(),
                    ));
                }
            }
            CppDecl::Namespace { .. } => {
                return Err(NativeError::Unsupported("namespace".to_string()));
            }
            CppDecl::Using { .. } => {
                return Err(NativeError::Unsupported("using declaration".to_string()));
            }
            CppDecl::Template { .. } => {
                return Err(NativeError::Unsupported("template".to_string()));
            }
            _ => {}
        }
    }
    let _ = Access::Public; // 抑制未使用警告
    Ok(())
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
    fn native_supported_rejects_virtual_inheritance() {
        // Stage 6 之前: 虚继承应该被 native_supported 拒绝
        let src = b"\
class Base { int x; };\n\
class Derived : public virtual Base { int y; };\n\
";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().expect("parse");
        let result = native_supported(&decls);
        assert!(result.is_err(), "virtual inheritance should be rejected");
        match result {
            Err(NativeError::Unsupported(s)) => {
                assert!(s.contains("virtual inheritance"));
            }
            _ => panic!("expected Unsupported error for virtual inheritance"),
        }
    }
}