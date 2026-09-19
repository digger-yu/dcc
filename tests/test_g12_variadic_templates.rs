//! G12: Variadic Templates + Parameter Packs 测试
//! 
//! 验证可变参数模板的解析和实例化

use dcc::cpp::template::TemplateRegistry;
use dcc::cpp::mangle::MangleType;
use dcc::cpp::parser::{CppDecl, TemplateParam, ClassDecl, TemplateArgs};

/// 测试基础 variadic template 解析
#[test]
fn test_variadic_template_declaration() {
    // 模拟解析 `template<typename... T> struct Tuple`
    let mut registry = TemplateRegistry::default();
    
    // 创建 varaiadic param: typename... T (is_pack = true)
    let params = vec![
        TemplateParam {
            name: "T".to_string(),
            default: None,
            non_type: None,
            is_pack: true,
            template_params: None,
        }
    ];
    
    // 创建一个简单 class 作为模板体
    let class_decl = CppDecl::Class(ClassDecl {
        name: "Tuple".to_string(),
        is_struct: true,
        is_union: false,
        bases: vec![],
        members: vec![],
    });
    
    let targs = TemplateArgs { params };
    registry.register(&targs, &None, &class_decl);
}

/// 测试实例化 variadic template: `Tuple<int, double>`
#[test]
fn test_instantiate_variadic_tuple() {
    let mut registry = TemplateRegistry::default();
    
    // 注册 variadic template
    let params = vec![
        TemplateParam {
            name: "T".to_string(),
            default: None,
            non_type: None,
            is_pack: true,
            template_params: None,
        }
    ];
    
    let class_decl = CppDecl::Class(ClassDecl {
        name: "Tuple".to_string(),
        is_struct: true,
        is_union: false,
        bases: vec![],
        members: vec![],
    });
    
    let targs = TemplateArgs { params };
    registry.register(&targs, &None, &class_decl);
    
    // 实例化：Tuple<int, double, char>
    let args = vec![
        MangleType::Int,
        MangleType::Double,
        MangleType::Char,
    ];
    
    let result = registry.instantiate_class("Tuple", &args);
    assert!(result.is_some());
    
    // Verify instantiation worked
    let inst = result.unwrap();
    match inst {
        CppDecl::Class(c) => {
            // Instantiate returns a class with actual name, no prefix added
            assert!(!c.name.is_empty());
        }
        _ => panic!("Expected Class decl"),
    }
}

/// 测试类型 key 编码包含 pack
#[test]
fn test_type_key_with_pack() {
    let pack = MangleType::VariadicPack(vec![
        MangleType::Int,
        MangleType::Double,
        MangleType::Char,
    ]);
    
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
        MangleType::VariadicPack(vec![
            MangleType::Double,
            MangleType::Float,
        ]),
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
    let params = vec![
        TemplateParam {
            name: "T".to_string(),
            default: None,
            non_type: None,
            is_pack: false,
            template_params: None,
        }
    ];
    
    let class_decl = CppDecl::Class(ClassDecl {
        name: "Box".to_string(),
        is_struct: true,
        is_union: false,
        bases: vec![],
        members: vec![],
    });
    
    let targs = TemplateArgs { params };
    registry.register(&targs, &None, &class_decl);
    
    // Instantiate with single type
    let args = vec![MangleType::Int];
    let result = registry.instantiate_class("Box", &args);
    
    assert!(result.is_some());
}
