use std::sync::Arc;

use crate::ast::{Field, QualifierFlags, QualType, Type};

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

// ===== QualifierFlags 测试 =====

#[test]
fn qualflags_default_is_empty() {
    let q = QualifierFlags::default();
    assert!(q.is_empty());
    assert!(!q.is_const());
    assert!(!q.is_volatile());
    assert!(!q.is_restrict());
    assert!(!q.is_atomic());
    assert_eq!(q.bits(), 0);
}

#[test]
fn qualflags_individual_predicates() {
    assert!(QualifierFlags::CONST.is_const());
    assert!(!QualifierFlags::CONST.is_volatile());
    assert!(QualifierFlags::VOLATILE.is_volatile());
    assert!(QualifierFlags::RESTRICT.is_restrict());
    assert!(QualifierFlags::ATOMIC.is_atomic());
    assert!(QualifierFlags::NODEREF.contains(QualifierFlags::NODEREF));
}

#[test]
fn qualflags_combine_via_bitor() {
    let q = QualifierFlags::CONST | QualifierFlags::VOLATILE;
    assert!(q.is_const());
    assert!(q.is_volatile());
    assert!(!q.is_restrict());
    assert_eq!(q.bits(), 0x03);
    let mut acc = QualifierFlags::empty();
    acc |= QualifierFlags::RESTRICT;
    acc |= QualifierFlags::ATOMIC;
    assert!(acc.is_restrict());
    assert!(acc.is_atomic());
    assert!(!acc.is_const());
    assert_eq!(acc.bits(), 0x0C);
}

#[test]
fn qualflags_contains_subset() {
    let q = QualifierFlags::CONST | QualifierFlags::VOLATILE | QualifierFlags::RESTRICT;
    assert!(q.contains(QualifierFlags::CONST));
    assert!(q.contains(QualifierFlags::VOLATILE | QualifierFlags::RESTRICT));
    assert!(!q.contains(QualifierFlags::ATOMIC));
    assert!(!q.contains(QualifierFlags::CONST | QualifierFlags::ATOMIC));
}

#[test]
fn qualflags_iso_c_strips_atomic_and_nodederef() {
    let q = QualifierFlags::CONST
        | QualifierFlags::VOLATILE
        | QualifierFlags::RESTRICT
        | QualifierFlags::ATOMIC
        | QualifierFlags::NODEREF;
    let iso = q.iso_c();
    assert_eq!(iso, QualifierFlags::CONST | QualifierFlags::VOLATILE | QualifierFlags::RESTRICT);
    assert!(!iso.is_atomic());
    assert_eq!(iso.bits(), 0x07);
}

#[test]
fn qualflags_display_empty_is_blank() {
    assert_eq!(format!("{}", QualifierFlags::empty()), "");
    assert_eq!(format!("{}", QualifierFlags::CONST), "const ");
    assert_eq!(
        format!("{}", QualifierFlags::CONST | QualifierFlags::VOLATILE),
        "const volatile "
    );
}

// ===== QualType 测试 =====

#[test]
fn qualtype_new_wraps_type_with_no_quals() {
    let qt = QualType::new(Type::Int);
    assert!(qt.is_unqualified());
    assert_eq!(qt.base, Type::Int);
    assert_eq!(qt.quals, QualifierFlags::NONE);
}

#[test]
fn qualtype_with_quals_preserves_both() {
    let qt = QualType::with_quals(Type::Char, QualifierFlags::CONST);
    assert!(qt.is_const());
    assert!(!qt.is_volatile());
    assert_eq!(qt.base, Type::Char);
}

#[test]
fn qualtype_from_type_is_unqualified() {
    let qt: QualType = Type::Int.into();
    assert!(qt.is_unqualified());
    assert_eq!(qt.base, Type::Int);
    let from_ref: QualType = (&Type::Double).into();
    assert_eq!(from_ref.base, Type::Double);
    assert!(from_ref.is_unqualified());
}

#[test]
fn qualtype_default_is_unqualified_int() {
    let qt = QualType::default();
    assert_eq!(qt.base, Type::Int);
    assert!(qt.is_unqualified());
}

#[test]
fn qualtype_size_align_forward_to_base() {
    let qt = QualType::with_quals(Type::Long, QualifierFlags::CONST);
    assert_eq!(qt.size(), 8);
    assert_eq!(qt.align(), 8);
    let pqt = QualType::with_quals(
        Type::Ptr(Box::new(Type::Void)),
        QualifierFlags::CONST | QualifierFlags::VOLATILE,
    );
    assert_eq!(pqt.size(), 8);
    assert!(pqt.is_ptr());
}

#[test]
fn qualtype_predicates_forward_to_base() {
    let int_qt = QualType::new(Type::Int);
    assert!(int_qt.is_int());
    assert!(int_qt.is_arith());
    assert!(!int_qt.is_ptr());

    let ptr_qt = QualType::new(Type::Ptr(Box::new(Type::Int)));
    assert!(ptr_qt.is_ptr());
    assert!(!ptr_qt.is_int());
    assert!(!ptr_qt.is_arith());

    let dbl_qt = QualType::new(Type::Double);
    assert!(dbl_qt.is_arith());
    assert!(!dbl_qt.is_int());
}

#[test]
fn qualtype_with_base_preserves_quals() {
    let qt = QualType::with_quals(Type::Int, QualifierFlags::CONST | QualifierFlags::VOLATILE);
    let qt2 = qt.with_base(Type::Long);
    assert_eq!(qt2.base, Type::Long);
    assert!(qt2.is_const());
    assert!(qt2.is_volatile());
}

#[test]
fn qualtype_with_quals_set_replaces() {
    let qt = QualType::with_quals(Type::Int, QualifierFlags::CONST);
    let qt2 = qt.with_quals_set(QualifierFlags::RESTRICT);
    assert!(!qt2.is_const());
    assert!(qt2.is_restrict());
    assert_eq!(qt2.base, Type::Int);
}

#[test]
fn qualtype_add_quals_ors_into_existing() {
    let qt = QualType::with_quals(Type::Int, QualifierFlags::CONST);
    let qt2 = qt.add_quals(QualifierFlags::VOLATILE);
    assert!(qt2.is_const());
    assert!(qt2.is_volatile());
    assert!(!qt2.is_restrict());
}

#[test]
fn qualtype_display_prefixes_quals() {
    let qt = QualType::with_quals(Type::Int, QualifierFlags::CONST);
    assert_eq!(format!("{}", qt), "const int");
    let qt2 = QualType::with_quals(Type::Int, QualifierFlags::CONST | QualifierFlags::VOLATILE);
    assert_eq!(format!("{}", qt2), "const volatile int");
    let qt3 = QualType::with_quals(
        Type::Ptr(Box::new(Type::Int)),
        QualifierFlags::CONST,
    );
    assert_eq!(format!("{}", qt3), "const int*");
}

#[test]
fn qualtype_partialeq_ignores_quals_when_bases_differ() {
    let a = QualType::with_quals(Type::Int, QualifierFlags::CONST);
    let b = QualType::with_quals(Type::Long, QualifierFlags::CONST);
    assert_ne!(a, b);
    let c = QualType::new(Type::Int);
    let d = QualType::with_quals(Type::Int, QualifierFlags::CONST);
    // PartialEq 比较 base + quals; 两个字段都参与比较.
    assert_ne!(c, d);
    let e = QualType::new(Type::Int);
    assert_eq!(c, e);
}

