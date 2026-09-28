//! G12: Variadic Templates + Parameter Packs 测试
//!
//! 验证可变参数模板的解析和实例化

use dcc::cpp::mangle::MangleType;
use dcc::cpp::parser::{Access, ClassDecl, CppDecl, CppParser, TemplateArgs, TemplateParam};
use dcc::cpp::template::TemplateRegistry;

/// 构造一个空成员的 `ClassDecl` (G22 起 `member_access` 是必选字段,
/// 合成的模板体没有访问说明符, 表为空 -> sema 按语言默认回退).
fn empty_class(name: &str, is_struct: bool) -> ClassDecl {
    ClassDecl {
        name: name.to_string(),
        is_struct,
        is_union: false,
        bases: vec![],
        members: vec![],
        member_access: vec![],
    }
}

/// 测试基础 variadic template 解析
#[test]
fn test_variadic_template_declaration() {
    // 模拟解析 `template<typename... T> struct Tuple`
    let mut registry = TemplateRegistry::default();

    // 创建 varaiadic param: typename... T (is_pack = true)
    let params = vec![TemplateParam {
        name: "T".to_string(),
        default: None,
        non_type: None,
        is_pack: true,
        template_params: None,
    }];

    // 创建一个简单 class 作为模板体
    let class_decl = CppDecl::Class(empty_class("Tuple", true));

    let targs = TemplateArgs { params };
    registry.register(&targs, &None, &class_decl);

    // 注册本身必须真正生效: 变参模板名要能在注册表里查到, 且标记为 pack
    let tmpl = registry
        .find("Tuple")
        .expect("variadic class template must be registered, not silently dropped");
    assert_eq!(tmpl.param_names, vec!["T".to_string()]);
    assert_eq!(tmpl.is_variadic_param, vec![true]);
    assert!(registry.has_variadic());
}

/// 测试实例化 variadic template: `Tuple<int, double>`
#[test]
fn test_instantiate_variadic_tuple() {
    let mut registry = TemplateRegistry::default();

    // 注册 variadic template
    let params = vec![TemplateParam {
        name: "T".to_string(),
        default: None,
        non_type: None,
        is_pack: true,
        template_params: None,
    }];

    let class_decl = CppDecl::Class(empty_class("Tuple", true));

    let targs = TemplateArgs { params };
    registry.register(&targs, &None, &class_decl);

    // 实例化：Tuple<int, double, char> —— 单个 pack 形参吃掉全部实参
    let args = vec![MangleType::VariadicPack(vec![
        MangleType::Int,
        MangleType::Double,
        MangleType::Char,
    ])];

    let result = registry.instantiate_class("Tuple", &args);
    assert!(result.is_some());

    // Verify instantiation worked
    let inst = result.unwrap();
    match inst {
        CppDecl::Class(c) => {
            // Instantiate returns a class with actual name, no prefix added
            assert!(!c.name.is_empty());
        },
        _ => panic!("Expected Class decl"),
    }
}

/// 测试类型 key 编码包含 pack
#[test]
fn test_type_key_with_pack() {
    let pack =
        MangleType::VariadicPack(vec![MangleType::Int, MangleType::Double, MangleType::Char]);

    let key = dcc::cpp::template::type_key(&pack);

    // Key should start with "pack_" and contain each type
    assert!(key.starts_with("pack_"));
    assert!(key.contains("int"));
    assert!(key.contains("double"));
    assert!(key.contains("char"));
}

/// 测试 merge_expanded_args
#[test]
fn test_merge_expanded_args() {
    use dcc::cpp::template::merge_expanded_args;

    let args = vec![
        MangleType::Int,
        MangleType::VariadicPack(vec![MangleType::Double, MangleType::Float]),
        MangleType::Char,
    ];

    let expanded = merge_expanded_args(&args);

    // Should be [int, double, float, char] - 4 types total
    assert_eq!(expanded.len(), 4);
    assert!(matches!(expanded[0], MangleType::Int));
    assert!(matches!(expanded[1], MangleType::Double));
    assert!(matches!(expanded[2], MangleType::Float));
    assert!(matches!(expanded[3], MangleType::Char));
}

/// 测试非 variadic template still works
#[test]
fn test_regular_template_still_works() {
    let mut registry = TemplateRegistry::default();

    // Regular template with single param (not variadic)
    let params = vec![TemplateParam {
        name: "T".to_string(),
        default: None,
        non_type: None,
        is_pack: false,
        template_params: None,
    }];

    let class_decl = CppDecl::Class(empty_class("Box", true));

    let targs = TemplateArgs { params };
    registry.register(&targs, &None, &class_decl);

    // Instantiate with single type
    let args = vec![MangleType::Int];
    let result = registry.instantiate_class("Box", &args);

    assert!(result.is_some());
}

// ---------------------------------------------------------------------------
// 以下是**源码驱动**的 G12 测试: 上面几个用例只覆盖手工构造 AST + 注册表,
// 真正让 `<print>`/`<tuple>` 整条命名空间退化成 Opaque 的是解析器对
// `Args... args` / `args...` / `sizeof...(Args)` 的支持. 这几个用例锁定
// AST 形状, 解析一旦退化 (变 Opaque / 形参丢包) 就会红.
// ---------------------------------------------------------------------------

/// 解析变参函数模板, 断言模板形参标记为 pack、函数形参类型为
/// `VariadicPack`, 且整条声明没有被顶层容错降级成 `Opaque`.
#[test]
fn test_parse_variadic_fn_template_param_is_pack() {
    let src = b"template <typename... Args> int sum(char* out, const char* fmt, Args... args);";
    let mut p = CppParser::new(src, "t.cpp");
    let ds = p
        .parse_top_level()
        .expect("variadic fn template must parse");
    assert_eq!(ds.len(), 1, "exactly one decl expected, got {ds:?}");
    let tmpl = match &ds[0] {
        CppDecl::Template {
            params,
            decl,
            requires,
            ..
        } => {
            assert_eq!(params.params.len(), 1);
            assert!(params.params[0].is_pack, "`typename... Args` must set TemplateParam::is_pack");
            assert_eq!(params.params[0].name, "Args");
            (decl, requires)
        },
        other => panic!("expected CppDecl::Template, got {other:?}"),
    };
    let (name, fn_params) = match &**tmpl.0 {
        CppDecl::FreeFn { name, params, .. } => (name.clone(), params.clone()),
        other => panic!("expected FreeFn inside template, got {other:?}"),
    };
    assert_eq!(name, "sum");
    assert_eq!(fn_params.len(), 3, "out/fmt/args, got {fn_params:?}");
    assert!(
        matches!(&fn_params[2].1, MangleType::VariadicPack(inner)
        if matches!(&inner[..], [MangleType::Class(c)] if c == "Args")),
        "`Args... args` must lower to a VariadicPack-typed parameter, got {:?}",
        fn_params[2]
    );
}

/// `args...` 实参与 `sizeof...(Args)` 的 AST 形状.
///
/// 结构上摊平发生在单态化阶段 (`template::substitute_*`), 这里只锁定
/// 解析产物: `PackExpansion(Ident("args"))` 与 `SizeofArg::Pack("Args")`.
/// 二者都嵌在函数体语句树里, 用 Debug 文本断言避免重复实现一遍遍历
/// (遍历逻辑本身由后端 match 穷尽性保证).
#[test]
fn test_parse_pack_expansion_and_sizeof_pack_ast_shape() {
    let src = b"template <typename... Args> int vprintf2(const char* fmt, Args... args) { \
                 if (sizeof...(Args) > 0) return call(fmt, args...); \
                 return 0; }";
    let mut p = CppParser::new(src, "t.cpp");
    let ds = p.parse_top_level().expect("pack expansion body must parse");
    let dbg = format!("{:?}", ds);
    assert!(
        dbg.contains("PackExpansion"),
        "`args...` must become CppExpr::PackExpansion, got {dbg}"
    );
    assert!(
        dbg.contains(r#"Pack("Args")"#),
        "`sizeof...(Args)` must become SizeofArg::Pack, got {dbg}"
    );
    assert!(
        !dbg.contains("Opaque"),
        "variadic declaration silently degraded to Opaque: {dbg}"
    );
}

/// G22: 类模板体的 `member_access` 必须逐成员记录访问说明符 ——
/// 变参类模板 (`class Tuple { T... }`) 的私有实现成员如果被误判为
/// public, sema 的访问检查就整体失效.
#[test]
fn test_parse_class_template_records_member_access() {
    let src = b"template <typename... T> class Tuple { int size_; public: int size() { \
                 return 0; } };";
    let mut p = CppParser::new(src, "t.cpp");
    let ds = p
        .parse_top_level()
        .expect("variadic class template must parse");
    let class_decl = match &ds[0] {
        CppDecl::Template { decl, .. } => match &**decl {
            CppDecl::Class(c) => c.clone(),
            other => panic!("expected class template body, got {other:?}"),
        },
        other => panic!("expected template, got {other:?}"),
    };
    assert!(!class_decl.is_struct, "`class` keyword -> is_struct = false");
    assert_eq!(class_decl.members.len(), 2, "size_ + size()");
    // 首成员继承 class 默认私有; 显式 `public:` 记录在 member_access 里,
    // 由 sema 的 member_decl_access 反查, 不得整体丢失.
    let access_of = |name: &str| {
        class_decl
            .member_access
            .iter()
            .find(|(m, _)| m == name)
            .map(|(_, a)| *a)
    };
    assert_eq!(
        access_of("size"),
        Some(Access::Public),
        "`public:` member must be recorded, got {:?}",
        class_decl.member_access
    );
}
