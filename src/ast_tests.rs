use std::sync::Arc;

use crate::ast::{Field, QualType, QualifierFlags, Type};

#[test]
fn struct_size_rounds_up_to_alignment() {
    let fields = vec![
        Field {
            name: "c".into(),
            ty: Type::Char,
            offset: 0,
            bit_width: None,
            bit_offset: 0,
            quals: QualifierFlags::NONE,
            ptr_quals: vec![],
            field_align: None,
            field_packed: false,
        },
        Field {
            name: "p".into(),
            ty: Type::Ptr(Box::new(Type::Void.into())),
            offset: 8,
            bit_width: None,
            bit_offset: 0,
            quals: QualifierFlags::NONE,
            ptr_quals: vec![],
            field_align: None,
            field_packed: false,
        },
        Field {
            name: "tail".into(),
            ty: Type::Char,
            offset: 16,
            bit_width: None,
            bit_offset: 0,
            quals: QualifierFlags::NONE,
            ptr_quals: vec![],
            field_align: None,
            field_packed: false,
        },
    ];
    assert_eq!(Type::Struct("s".into(), Arc::new(fields), 8, false).size(), 24);
}

#[test]
fn union_size_rounds_up_to_alignment() {
    let fields = vec![
        Field {
            name: "wide".into(),
            ty: Type::Ptr(Box::new(Type::Void.into())),
            offset: 0,
            bit_width: None,
            bit_offset: 0,
            quals: QualifierFlags::NONE,
            ptr_quals: vec![],
            field_align: None,
            field_packed: false,
        },
        Field {
            name: "bytes".into(),
            ty: Type::Array(Box::new(Type::Char.into()), crate::ast::ArraySize::Const(9)),
            offset: 0,
            bit_width: None,
            bit_offset: 0,
            quals: QualifierFlags::NONE,
            ptr_quals: vec![],
            field_align: None,
            field_packed: false,
        },
    ];
    assert_eq!(Type::Union("u".into(), Arc::new(fields), 8, false).size(), 16);
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
            quals: QualifierFlags::NONE,
            ptr_quals: vec![],
            field_align: None,
            field_packed: false,
        },
        Field {
            name: "i".into(),
            ty: Type::Int,
            offset: 1,
            bit_width: None,
            bit_offset: 0,
            quals: QualifierFlags::NONE,
            ptr_quals: vec![],
            field_align: None,
            field_packed: false,
        },
    ];
    let ty = Type::Struct("packed".into(), Arc::new(fields), 1, false);
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
        Type::Ptr(Box::new(Type::Void.into())),
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

    let ptr_qt = QualType::new(Type::Ptr(Box::new(Type::Int.into())));
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
    let qt3 = QualType::with_quals(Type::Ptr(Box::new(Type::Int.into())), QualifierFlags::CONST);
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

fn tfield(name: &str, ty: Type, offset: usize) -> Field {
    Field {
        name: name.into(),
        ty,
        offset,
        bit_width: None,
        bit_offset: 0,
        quals: QualifierFlags::NONE,
        ptr_quals: vec![],
        field_align: None,
        field_packed: false,
    }
}

fn tstruct(name: &str, fields: Vec<Field>) -> Type {
    Type::Struct(name.into(), Arc::new(fields), 8, false)
}

/// 具名聚合相等 = 名义同一律：同一个 tag「先前向声明、后补全」的两份副本必须是同一
/// 类型。内核 `EXPORT_SYMBOL_GPL` 的 `extern typeof(f) f;` 正是拿补全后的那一份去和
/// 补全前记下的那一份比较，按字段深度比较会判成不同类型（E0007 误报）。
#[test]
fn named_struct_equality_is_nominal_across_completion() {
    let incomplete = tstruct("irq_desc", vec![]);
    let complete =
        tstruct("irq_desc", vec![tfield("x", Type::Int, 0), tfield("lock", Type::Long, 8)]);
    assert_eq!(incomplete, complete, "同一个 tag 就是同一个类型");
    assert_eq!(complete, incomplete, "比较必须对称");

    let other = tstruct("irq_data", vec![]);
    assert_ne!(incomplete, other, "不同 tag 名不得相等");
    let union_of_same_name = Type::Union("irq_desc".into(), Arc::new(vec![]), 8, false);
    assert_ne!(incomplete, union_of_same_name, "struct 与 union 是不同类型");
}

/// 匿名聚合没有名字可依，必须继续按结构比较（同构才等）。
#[test]
fn anonymous_struct_equality_is_structural() {
    let a = tstruct("", vec![tfield("x", Type::Int, 0)]);
    let b = tstruct("", vec![tfield("x", Type::Int, 0)]);
    let c = tstruct("", vec![tfield("x", Type::Long, 0)]);
    let d = tstruct("", vec![tfield("y", Type::Int, 0)]);
    assert_eq!(a, b, "字段逐项相同的匿名 struct 是同一类型");
    assert_ne!(a, c, "字段类型不同 ⇒ 匿名 struct 不同");
    assert_ne!(a, d, "字段名不同 ⇒ 匿名 struct 不同");
}

/// 类型标签必须**有界**：派生 `Debug` 会把聚合字段树整棵展开，内核里 `struct page`
/// 这类共享 DAG 类型按树展开每层翻倍，实测单个串申请 3.4 GB 直接把编译打死。
#[test]
fn identity_tag_is_bounded_for_aggregate_types() {
    // 一个「大」聚合：512 个字段（字段本身又是同名的深类型），串仍应短小。
    let fields: Vec<Field> = (0..512)
        .map(|i| tfield(&format!("f{i}"), Type::Ptr(Box::new(Type::Int.into())), i * 8))
        .collect();
    let big = tstruct("page_like", fields);
    let tag = big.identity_tag().expect("具名聚合的身份串必须存在");
    assert!(tag.len() < 96, "具名聚合按名义编码，身份串不该随字段数增长，实际 {tag:?}");
    assert!(tag.contains("struct page_like"), "身份串要带 tag 名: {tag:?}");

    // 指针/数组/函数包装只按其自身结构展开，具名聚合命中即返回。
    let wrapped = Type::Func(
        Box::new(Type::Ptr(Box::new(big.clone().into())).into()),
        vec![Type::Array(Box::new(big.into()), crate::ast::ArraySize::Const(4)).into()],
    );
    let tag2 = wrapped.identity_tag().expect("包装类型的身份串必须存在");
    assert!(tag2.len() < 160, "包装类型身份串应有界, 实际 {tag2:?}");
    assert_eq!(wrapped.identity_tag(), tag2.into(), "同一类型必须给出同一串");
}

/// 匿名聚合的展开受**串长度上限**约束：超出 `TYPE_TAG_MAX_BYTES` 的匿名嵌套返回
/// `None`（= 放弃这次值编号），而不是给出一个可能撞名的截断串，也不许把内存吃穿。
#[test]
fn identity_tag_bails_out_on_oversized_anonymous_nesting() {
    // 宽 6 × 深 6 ⇒ 6^6 ≈ 4.6 万个节点，远超 4 KiB 上限；浅的同样例必须仍给串。
    fn nest(depth: u32) -> Type {
        if depth == 0 {
            return Type::Int;
        }
        tstruct(
            "",
            (0..6)
                .map(|i| tfield(&format!("f{i}"), nest(depth - 1), i * 8))
                .collect(),
        )
    }
    let shallow = nest(2).identity_tag();
    assert!(shallow.is_some(), "浅的匿名聚合应照常给串: {shallow:?}");
    assert_eq!(
        nest(6).identity_tag(),
        None,
        "超过长度上限的匿名聚合必须放弃，而不是硬给一个截断串"
    );
}
