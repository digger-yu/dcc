use std::sync::Arc;

use crate::ast::{Field, Type};

#[test]
fn struct_size_rounds_up_to_alignment() {
    let fields = vec![
        Field {
            name: "c".into(),
            ty: Type::Char,
            offset: 0,
            bit_width: None,
            bit_offset: 0,
        },
        Field {
            name: "p".into(),
            ty: Type::Ptr(Box::new(Type::Void)),
            offset: 8,
            bit_width: None,
            bit_offset: 0,
        },
        Field {
            name: "tail".into(),
            ty: Type::Char,
            offset: 16,
            bit_width: None,
            bit_offset: 0,
        },
    ];
    assert_eq!(Type::Struct("s".into(), Arc::new(fields), 8).size(), 24);
}

#[test]
fn union_size_rounds_up_to_alignment() {
    let fields = vec![
        Field {
            name: "wide".into(),
            ty: Type::Ptr(Box::new(Type::Void)),
            offset: 0,
            bit_width: None,
            bit_offset: 0,
        },
        Field {
            name: "bytes".into(),
            ty: Type::Array(Box::new(Type::Char), crate::ast::ArraySize::Const(9)),
            offset: 0,
            bit_width: None,
            bit_offset: 0,
        },
    ];
    assert_eq!(Type::Union("u".into(), Arc::new(fields), 8).size(), 16);
}

#[test]
fn packed_struct_size_uses_observed_alignment() {
    let fields = vec![
        Field {
            name: "c".into(),
            ty: Type::Char,
            offset: 0,
            bit_width: None,
            bit_offset: 0,
        },
        Field {
            name: "i".into(),
            ty: Type::Int,
            offset: 1,
            bit_width: None,
            bit_offset: 0,
        },
    ];
    let ty = Type::Struct("packed".into(), Arc::new(fields), 1);
    assert_eq!(ty.align(), 1);
    assert_eq!(ty.size(), 5);
}
