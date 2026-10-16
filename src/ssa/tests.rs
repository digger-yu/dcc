use super::*;
use crate::diagnostics::Span;

use crate::ast::{BinOp, Block, BlockItem, Decl, Expr, FuncDecl, QualType, Stmt, Type};
use std::collections::HashSet;

use crate::ssa::cfg::{
    compute_dominators, compute_ipdoms, find_loop_body_multi, is_back_edge, term_succs,
};
use crate::ssa::expr_rebuild::infer_binop_type;
use crate::ssa::loop_exit::{
    is_eh_pad, loop_exit_owner, loop_exit_plan, loop_shape, resolve_loop_exit_stub, LoopExitOwner,
    LoopExitPlan,
};
use crate::ssa::rebuild::{
    branch_join, is_loop_exit_pure_return, is_lower_ir_temp_base, is_pass_hint_inst,
};

/// 手搭一个「二路分支的其中一条路径本身又是二路分支」的 CFG —— 嵌套三元
/// (`a ? b : c ? d : e`)、三元分支里的 `&&`/`||`、以及语句级
/// `if (x) {...} else { if (y) {...} }` 降级后都是这个形状。
///
/// ```text
/// entry: if c1 -> A, B
/// A:     goto M
/// B:     if c2 -> B1, B2      <- 汇聚点不在「唯一前驱+无条件跳转」链上
/// B1:    goto M2
/// B2:    goto M2
/// M2:    goto M
/// M:     goto E
/// E:     return
/// ```
fn nested_branch_cfg() -> Function {
    let span = Span::point("t.c", 1, 1);
    let mut f = Function::new("f".into(), Type::Int, span);
    for b in ["A", "B", "B1", "B2", "M", "M2", "E"] {
        f.add_block(b.to_string());
    }
    let c = Operand::Int(1);
    f.blocks.get_mut("entry").unwrap().term = Terminator::Branch {
        cond: c.clone(),
        t: "A".into(),
        f: "B".into(),
    };
    f.blocks.get_mut("A").unwrap().term = Terminator::Jump("M".into());
    f.blocks.get_mut("B").unwrap().term = Terminator::Branch {
        cond: c,
        t: "B1".into(),
        f: "B2".into(),
    };
    f.blocks.get_mut("B1").unwrap().term = Terminator::Jump("M2".into());
    f.blocks.get_mut("B2").unwrap().term = Terminator::Jump("M2".into());
    f.blocks.get_mut("M2").unwrap().term = Terminator::Jump("M".into());
    f.blocks.get_mut("M").unwrap().term = Terminator::Jump("E".into());
    f.blocks.get_mut("E").unwrap().term = Terminator::Ret {
        val: Some(Operand::Int(0)),
    };
    f
}

/// 后支配者必须把嵌套分支的凝聚点判对: `entry` 的凝聚点是 `M` (而不是内层
/// 的 `M2`), `B` 的是 `M2`。`branch_join` 的旧启发式从 `B` 出发直接放弃
/// (它是个二分支), 因此 `entry` 这一层只能靠后支配者兜底 —— 判错或直接
/// 判空都会让 `M` 被内联进一条路径, `If` 之后的代码在另一条路径上丢失。
#[test]
fn ipdom_and_branch_join_find_nested_joins() {
    let f = nested_branch_cfg();
    let ip = compute_ipdoms(&f);
    assert_eq!(ip.get("entry").map(|s| s.as_str()), Some("M"), "entry 的后支配者应为 M");
    assert_eq!(ip.get("B").map(|s| s.as_str()), Some("M2"), "B 的后支配者应为 M2");
    assert_eq!(branch_join("entry", "A", "B", &f).as_deref(), Some("M"));
    assert_eq!(branch_join("B", "B1", "B2", &f).as_deref(), Some("M2"));
}

/// **非对称汇聚** (SQLite `mallocWithAlarm` 段错误根因): 一条臂沿 Jump 直链
/// 走到多入边汇聚块, 另一条臂则是一个「可经内层 `Ret` 出函数」的嵌套分支 ——
/// 它的 `walk_to_join` 因遇到非 Jump 终结符 (内层 Branch) 而返回 `None`, 但其
/// 非返回路径仍直达同一个汇聚块。
///
/// CFG 对应:
/// ```text
/// entry --Branch(then, els)
/// then  --Branch(ret, fall)
/// ret   --Ret
/// fall  --Jump tail
/// els   --Jump tail
/// tail  (preds=fall,els) --Ret
/// ```
/// 旧 `branch_join` 只在两臂都给出 Some 时认汇聚, 这里漏判后 ipdom 又只剩虚拟
/// EXIT 占位名 (一条路径直接出函数), 于是返回错误的 `None`, 重建把 `tail` 内联进
/// 第一个到达它的内层臂, 外层假分支路径上 If 之后的尾部语句整体丢失。
#[test]
fn branch_join_finds_asymmetric_join_when_one_arm_can_return() {
    let span = Span::point("t.c", 1, 1);
    let mut f = Function::new("f".into(), Type::Int, span);
    for b in ["then", "els", "ret", "fall", "tail"] {
        f.add_block(b.to_string());
    }
    let c = Operand::Int(1);
    f.blocks.get_mut("entry").unwrap().term = Terminator::Branch {
        cond: c.clone(),
        t: "then".into(),
        f: "els".into(),
    };
    f.blocks.get_mut("then").unwrap().term = Terminator::Branch {
        cond: c,
        t: "ret".into(),
        f: "fall".into(),
    };
    f.blocks.get_mut("ret").unwrap().term = Terminator::Ret {
        val: Some(Operand::Int(0)),
    };
    f.blocks.get_mut("fall").unwrap().term = Terminator::Jump("tail".into());
    f.blocks.get_mut("els").unwrap().term = Terminator::Jump("tail".into());
    f.blocks.get_mut("tail").unwrap().term = Terminator::Ret {
        val: Some(Operand::Int(0)),
    };

    // 外层分支的公共汇聚块必须是 tail, 不能因 then 臂可 return 就判空。
    assert_eq!(branch_join("entry", "then", "els", &f).as_deref(), Some("tail"));
}

/// **两臂各自又是一层分支** (SQLite `sqlite3SrcListAppend` 段错误根因): 快路径的
/// `walk_to_join` 在臂自己的 `Branch` 上返回 `None`, 非对称可达那步没有候选块可验,
/// 体内又有早退 `Ret` ⇒ ipdom 只剩虚拟 EXIT 占位名, 三条判据全落空。旧实现在这里给出
/// `None`, 于是重建把公共尾部内联进第一条臂, 兄弟臂因 `visited` 去重拿到空 ——
/// `pItem=&pList->a[…]` 与 `return pList` 一起消失, `%rax` 留着 `nSrc` 当指针返回,
/// `selectExpander` 解引用空 SrcList ⇒ 任何两表 JOIN 段错误。
///
/// 修复后的判据: `arm_join` 穿过臂自己的嵌套分支 ——
/// 死侧 (`Ret` 臂) 只会出函数时, 另一侧给出的多入边块就是汇聚块。
#[test]
fn branch_join_finds_join_when_both_arms_are_branches() {
    let span = Span::point("t.c", 1, 1);
    let mut f = Function::new("f".into(), Type::Int, span);
    for b in [
        "then", "t_alloc", "t_set", "t_mid", "els", "t_full", "e_set", "e_mid",
    ] {
        f.add_block(b.to_string());
    }
    f.add_block("tail".to_string());
    let c = Operand::Int(1);
    f.blocks.get_mut("entry").unwrap().term = Terminator::Branch {
        cond: c.clone(),
        t: "then".into(),
        f: "els".into(),
    };
    // then 臂: `if(!pNew) return 0;` + `p->nSrc = 1;`
    f.blocks.get_mut("then").unwrap().term = Terminator::Branch {
        cond: c.clone(),
        t: "t_alloc".into(),
        f: "t_set".into(),
    };
    f.blocks.get_mut("t_alloc").unwrap().term = Terminator::Ret {
        val: Some(Operand::Int(0)),
    };
    f.blocks.get_mut("t_set").unwrap().term = Terminator::Jump("t_mid".into());
    f.blocks.get_mut("t_mid").unwrap().term = Terminator::Jump("tail".into());
    // els 臂: `if(p->nSrc >= 4) return p;` + `p->nSrc += 1;`
    f.blocks.get_mut("els").unwrap().term = Terminator::Branch {
        cond: c.clone(),
        t: "t_full".into(),
        f: "e_set".into(),
    };
    f.blocks.get_mut("t_full").unwrap().term = Terminator::Ret {
        val: Some(Operand::Int(0)),
    };
    f.blocks.get_mut("e_set").unwrap().term = Terminator::Jump("e_mid".into());
    f.blocks.get_mut("e_mid").unwrap().term = Terminator::Jump("tail".into());
    f.blocks.get_mut("tail").unwrap().term = Terminator::Ret {
        val: Some(Operand::Int(0)),
    };

    assert_eq!(
        branch_join("entry", "then", "els", &f).as_deref(),
        Some("tail"),
        "两臂各自是分支时也必须找到公共尾部, 否则尾部只活在第一条臂里"
    );
}

/// 判据的**反向**约束: 两条臂真的分岔 (各自汇聚到不同的多入边块, 且互不可达) 时不得
/// 给出真实汇聚块 —— 拿它当 `stop_at` 会把一条臂的续体搬到 If 之后, 语义照样错。
/// 此时 `branch_join` 只允许回落到虚拟 EXIT 占位名 (调用方按「不是真实块」过滤成 `None`)。
#[test]
fn branch_join_keeps_divergent_arms_apart() {
    let span = Span::point("t.c", 1, 1);
    let mut f = Function::new("f".into(), Type::Int, span);
    for b in ["A1", "A2", "MA", "B1", "B2", "MB"] {
        f.add_block(b.to_string());
    }
    let c = Operand::Int(1);
    f.blocks.get_mut("entry").unwrap().term = Terminator::Branch {
        cond: c.clone(),
        t: "A".into(),
        f: "B".into(),
    };
    f.add_block("A".to_string());
    f.add_block("B".to_string());
    f.blocks.get_mut("A").unwrap().term = Terminator::Branch {
        cond: c.clone(),
        t: "A1".into(),
        f: "A2".into(),
    };
    f.blocks.get_mut("A1").unwrap().term = Terminator::Jump("MA".into());
    f.blocks.get_mut("A2").unwrap().term = Terminator::Jump("MA".into());
    f.blocks.get_mut("MA").unwrap().term = Terminator::Ret {
        val: Some(Operand::Int(0)),
    };
    f.blocks.get_mut("B").unwrap().term = Terminator::Branch {
        cond: c.clone(),
        t: "B1".into(),
        f: "B2".into(),
    };
    f.blocks.get_mut("B1").unwrap().term = Terminator::Jump("MB".into());
    f.blocks.get_mut("B2").unwrap().term = Terminator::Jump("MB".into());
    f.blocks.get_mut("MB").unwrap().term = Terminator::Ret {
        val: Some(Operand::Int(0)),
    };

    let j = branch_join("entry", "A", "B", &f);
    assert!(
        !j.as_deref().is_some_and(|n| f.blocks.contains_key(n)),
        "分岔的两臂没有公共汇聚块, 实得 {j:?}"
    );
}

/// `collect_all_preds` 给出的前驱表必须是**确定序**（本仓不变式：HashMap
/// 迭代序一律不得外泄）。它是 `bb.preds` 缓存与 `DCC_SSA_IR_DUMP` 读数的唯一
/// 来源：曾经这里直接 `for (name, bb) in &func.blocks` 迭代 HashMap，同一
/// 二进制连跑 8 次会给出两种 preds 序 —— 集合内容不变，但拿诊断文本做
/// 差分对拍时会把「同一个二进制」误读成「两个行为」。
///
/// 判据强度：造一扇入 7 个前驱到同一个 `join` 的 CFG。若退化回 HashMap 迭代，
/// 随机序恰好用不上等于规范序的概率是 1/7! = 1/5040 ⇒ **单次运行就有足够
/// 判定力**，不需要多次采样碰运气（同 `check_fixtures_selftest.sh` 的思路）。
#[test]
fn collect_all_preds_yields_canonical_deterministic_order() {
    let span = Span::point("t.c", 1, 1);
    let mut f = Function::new("f".into(), Type::Int, span);
    // canonical 块序 = block_order 的插入序: entry, in0..in5, join
    for i in 0..6 {
        let b = format!("in{i}");
        f.add_block(b.clone());
        f.blocks.get_mut(&b).unwrap().term = Terminator::Jump("join".into());
    }
    f.add_block("join".into());
    f.blocks.get_mut("join").unwrap().term = Terminator::Ret {
        val: Some(Operand::Int(0)),
    };
    f.blocks.get_mut("entry").unwrap().term = Terminator::Jump("join".into());

    let order = ordered_block_names(&f);
    let idx = |n: &str| {
        order
            .iter()
            .position(|b| b == n)
            .unwrap_or_else(|| panic!("`{n}` 不在规范块序里: {order:?}"))
    };
    let preds = collect_all_preds(&f);

    // 1) 结构性质：每个目标块的前驱表沿规范块序单调递增。
    for (target, ps) in &preds {
        for w in ps.windows(2) {
            assert!(
                idx(&w[0]) < idx(&w[1]),
                "`{target}` 的 preds 未走规范块序: {ps:?} (canonical {order:?})"
            );
        }
    }
    // 2) 具体读数钉死：1/5040 的偶合概率在这条断言上。
    assert_eq!(
        preds["join"],
        vec!["entry", "in0", "in1", "in2", "in3", "in4", "in5"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
        "7 路扇入的 preds 必须按规范块序给出 (canonical {order:?})"
    );

    // 3) 这个序会随 `refresh_cfg` 落进缓存并进 dump 读数，一起钉住。
    refresh_cfg(&mut f);
    assert_eq!(f.blocks["join"].preds, preds["join"], "bb.preds 缓存必须与派生结果同序");
    let d1 = dump_function(&f);
    let d2 = dump_function(&f);
    assert_eq!(d1, d2, "同一 IR 两次读数必须逐字一致");
    assert!(
        d1.contains(
            "join: preds=[\"entry\", \"in0\", \"in1\", \"in2\", \"in3\", \"in4\", \"in5\"]"
        ),
        "dump 里的 preds 行未按规范序输出:\n{d1}"
    );

    // 4) 菱形图上的集合内容（只排序不能掩盖多划/漏划前驱边）。
    let g = nested_branch_cfg();
    let gp = collect_all_preds(&g);
    let mut m = gp["M"].clone();
    let mut m2 = gp["M2"].clone();
    m.sort();
    m2.sort();
    assert_eq!(m, vec!["A".to_string(), "M2".to_string()]);
    assert_eq!(m2, vec!["B1".to_string(), "B2".to_string()]);
}

#[test]
fn simple_function_builds_ssa() {
    let span = Span::point("t.c", 1, 1);
    let f = FuncDecl {
        ret_ty: Type::Int,
        name: crate::ast::Ident {
            name: "f".into(),
            span: span.clone(),
        },
        params: vec![],
        body: Some(Block {
            items: vec![
                BlockItem::Decl(Decl::Var(crate::ast::VarDecl {
                    ty: Type::Int,
                    name: crate::ast::Ident {
                        name: "x".into(),
                        span: span.clone(),
                    },
                    init: Some(Expr::Int {
                        v: 1,
                        span: span.clone(),
                        ty: Type::Int,
                    }),
                    span: span.clone(),
                    is_extern: false,
                    is_static: false,
                    section: None,
                    alignment: None,
                    is_weak: false,
                    visibility: None,
                    is_unused: false,
                    register_name: None,
                    home: None,
                    cleanup_fn: None,
                    is_thread_local: false,
                    no_sanitize: vec![],
                    alias_name: None,
                    copy_fn: None,
                    is_retain: false,
                    is_used_attr: false,
                    quals: crate::ast::QualifierFlags::NONE,
                    addr_space: crate::ast::AddrSpace::None,
                    tls_model: None,
                    asm_label: None,
                })),
                BlockItem::Stmt(Stmt::Return {
                    val: Some(Expr::Ident {
                        name: crate::ast::Ident {
                            name: "x".into(),
                            span: span.clone(),
                        },
                        span: span.clone(),
                        ty: Type::Int,
                    }),
                    span: span.clone(),
                }),
            ],
            span: span.clone(),
        }),
        span: span.clone(),
        is_extern: false,
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
        upvars: vec![],
        is_old_style: false,
        format_kind: None,
        format_string_idx: None,
        format_first_arg: None,
        no_sanitize: vec![],
        target_clones: vec![],
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
        is_global: false,
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
    };
    let func = build_from_func(&f).expect("structured");
    assert!(func.blocks.contains_key("entry"));
    assert_eq!(func.params.len(), 0);
}

#[test]
fn if_else_structured() {
    let span = Span::point("t.c", 1, 1);
    let f = FuncDecl {
        ret_ty: Type::Int,
        name: crate::ast::Ident {
            name: "f".into(),
            span: span.clone(),
        },
        params: vec![],
        body: Some(Block {
            items: vec![BlockItem::Stmt(Stmt::If {
                cond: Expr::Int {
                    v: 1,
                    span: span.clone(),
                    ty: Type::Int,
                },
                then_: Box::new(Stmt::Return {
                    val: Some(Expr::Int {
                        v: 1,
                        span: span.clone(),
                        ty: Type::Int,
                    }),
                    span: span.clone(),
                }),
                else_: Some(Box::new(Stmt::Return {
                    val: Some(Expr::Int {
                        v: 0,
                        span: span.clone(),
                        ty: Type::Int,
                    }),
                    span: span.clone(),
                })),
                span: span.clone(),
            })],
            span: span.clone(),
        }),
        span: span.clone(),
        is_extern: false,
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
        upvars: vec![],
        is_old_style: false,
        format_kind: None,
        format_string_idx: None,
        format_first_arg: None,
        no_sanitize: vec![],
        target_clones: vec![],
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
        is_global: false,
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
    };
    let func = build_from_func(&f).expect("structured");
    assert!(func.blocks.contains_key("if.then"));
    assert!(func.blocks.contains_key("if.else"));
    assert!(func.blocks.contains_key("if.end"));
}

#[test]
fn goto_bails_out() {
    let span = Span::point("t.c", 1, 1);
    let f = FuncDecl {
        ret_ty: Type::Int,
        name: crate::ast::Ident {
            name: "f".into(),
            span: span.clone(),
        },
        params: vec![],
        body: Some(Block {
            items: vec![BlockItem::Stmt(Stmt::Goto {
                target: crate::ast::Ident {
                    name: "L".into(),
                    span: span.clone(),
                },
                span: span.clone(),
            })],
            span: span.clone(),
        }),
        span: span.clone(),
        is_extern: false,
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
        upvars: vec![],
        is_old_style: false,
        format_kind: None,
        format_string_idx: None,
        format_first_arg: None,
        no_sanitize: vec![],
        target_clones: vec![],
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
        is_global: false,
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
    };
    assert!(build_from_func(&f).is_none());
}

/// SSA→AST 重建的「单次使用即内联」命名契约。
///
/// 这条规则直接决定循环条件/短路结果能不能正确工作:
/// 内联意味着值不在每次读时重算, 所以**只有**编译器合成、全局唯一、
/// 从不被用户代码引用的 base 才能享受它。`lower_ir::fresh_logical_base`
/// 产的短路结果变量 (`dcc_land<N>` / `dcc_lor<N>`) 故意**不**符合本形状:
/// 它们在 merge 块被读一次, 但必须在两条分支里各写一次, 一旦被内联进
/// 使用点就会丢掉赋值 (历史缺陷: `a==2 || a==3` 恒为假)。
#[test]
fn lower_ir_temp_base_recognizes_only_fresh_syn_names() {
    // `fresh_syn` 形状: `dcc_t<序号>_<后缀>`
    assert!(is_lower_ir_temp_base("dcc_t1_field"));
    assert!(is_lower_ir_temp_base("dcc_t42_sym_addr"));
    assert!(is_lower_ir_temp_base("dcc_t7_call_result"));
    // 序号位必须是纯数字, 且必须带 `_后缀` 分隔符:
    assert!(!is_lower_ir_temp_base("dcc_ttmp"));
    assert!(!is_lower_ir_temp_base("dcc_t1"));
    assert!(!is_lower_ir_temp_base("dcc_table_x"));
    // C 前端 (`lower.rs`) 的 `_t<N>_<base>` 是另一套命名, 不由本函数识别:
    assert!(!is_lower_ir_temp_base("_t3_add"));
    // 短路结果变量是**具名局部**, 必须走真实的声明+赋值路径:
    assert!(!is_lower_ir_temp_base("dcc_land1"));
    assert!(!is_lower_ir_temp_base("dcc_lor2"));
    // 用户/符号名一律不匹配:
    assert!(!is_lower_ir_temp_base(""));
    assert!(!is_lower_ir_temp_base("i"));
    assert!(!is_lower_ir_temp_base("__dcc_static_0"));
}

/// 取第一个函数并试 SSA 化；`None` = 该函数不进管线（留在 AST 通道上）。
fn try_build(src: &str, name: &str) -> Option<Function> {
    let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
    for decl in prog.decls.iter() {
        if let crate::ast::Decl::Func(f) = decl {
            if f.name.name == name {
                return build_from_func(f);
            }
        }
    }
    panic!("找不到函数 {name}");
}

/// EH 落地垫块永远不属于循环结构：既不能当 latch / 体块，也不能当续体。
///
/// 实测形状（C++ `for` + 循环作用域内需要析构的对象）里，垫块会 `goto` 回入循环头，
/// 那条边在 CFG 上就是一条**回边**：于是垫块被算成 latch、被圈进 body，它的去向
/// `eh_resume.8` 又被数成「共享出口」选成续体 —— 循环正常走完就落进
/// `_Unwind_Resume`（`eh_loop_and_scope_exit.cpp`：g++ 退出码 0 / dcc -3，而只跑 C
/// 语料的 gcc 差分完全看不到这一条）。本例用真 IR 手工把体块标成垫块，钉两面：
///   * 标为垫块后它不能再是 latch，也不能再在 body 里；
///   * 反向对照：不标垫块时它**仍然**是 latch（否则「把谁都挡在循环外」也能做绿）。
#[test]
fn eh_pads_are_never_loop_members_or_continuations() {
    let src = "int s(int n){int acc=0;for(int i=0;i<n;i++){acc+=i;}return acc+n;}\n";
    let func = try_build(src, "s").expect("普通 for 应能建 IR");
    let doms = compute_dominators(&func);

    // 先定到那个循环的头与 latch。
    let mut headers: Vec<&String> = func.blocks.keys().collect();
    headers.sort();
    let (header, latches, body) = headers
        .iter()
        .filter_map(|h| loop_shape(&func, h, &doms).map(|(l, b)| (h.to_string(), l, b)))
        .next()
        .expect("应找到一个循环");
    assert!(!latches.is_empty(), "反向对照：该循环应有 latch");
    let latch = latches[0].clone();
    assert!(body.contains(&latch), "latch {latch} 本应在 body 里");

    // 把 latch 标成落地垫块：它不得再被算进循环结构（副本上做，走显式 `fork()`）。
    let mut marked = func.fork();
    marked.eh_unwind_blocks = vec![latch.clone()];
    if let Some((l2, b2)) = loop_shape(&marked, &header, &doms) {
        assert!(
            !l2.contains(&latch),
            "被标成 EH 落地垫的块不能再当 latch（它那条回入边不是循环回边）"
        );
        assert!(
            !b2.contains(&latch),
            "被标成 EH 落地垫的块不能再在 body 里（否则它的去向会被数成出口）"
        );
    }
    // 垫块也不能当续体：就算还能找到循环，`loop_exit_plan.cont` 必须不是垫块。
    for h in headers.iter() {
        if let Some((ls, bs)) = loop_shape(&marked, h, &doms) {
            let plan = loop_exit_plan(&marked, h, &bs, &ls, &doms);
            if let Some(c) = plan.cont.as_deref() {
                assert!(
                    !is_eh_pad(c, &marked),
                    "循环续体落在了 EH 落地垫 {c} 上：循环正常结束会走进 _Unwind_Resume"
                );
            }
        }
    }
}

/// 子对象地址现在**有 IR 表示**（`InstKind::MemberAddr`）：`p->fld[i]`（fld 是零长/
/// 柔性数组）的基址 = 对象地址 + 字段偏移。以前 IR 表达不了，只能整函数退 SSA
/// （§7.29），本节把「退路」的断言换成「表示存在」的断言。
///
/// 为什么不只断言「能进 SSA」：退路删掉后，发一个看起来能跑的错形状也能做绿。
/// 所以这里钉「IR 里确实出现了 `MemberAddr`」，而**值对不对**交给
/// `vla_and_flexible_array_match_gcc_at_all_levels` 拿 gcc 实测地面真值逐档对照。
/// 读与写两侧都钉（历史上「只挡一边」重复错过两次）。
#[test]
fn subobject_array_decay_is_represented_as_member_addr() {
    fn n_member_addr(src: &str, fname: &str) -> usize {
        let f = try_build(src, fname).expect("子对象下标应能进 SSA");
        f.blocks
            .values()
            .flat_map(|b| b.insts.iter())
            .filter(|i| matches!(i.kind, InstKind::MemberAddr { .. }))
            .count()
    }

    let zero_len = "struct Z { int count; int data[0]; };\n\
                    int t(struct Z* z){ return z->data[0] + z->data[2]; }\n";
    assert!(n_member_addr(zero_len, "t") >= 1, "零长数组成员的下标基址必须走 MemberAddr");
    // 条数不钉死：构建期不做 CSE，`data[0] + data[2]` 现在发两次衰减（实测 2 条），
    // 以后的 pass 把它合成一条不是错。只钉「有没有」与「不该有的地方没有」。
    let flex = "struct F { int count; int data[]; };\n\
                int t(struct F* f){ f->data[1] = 7; return f->data[1]; }\n";
    assert!(
        n_member_addr(flex, "t") >= 1,
        "柔性数组成员的**写**同样要拿到子对象地址（与读同一机制，不能只做一边）"
    );
    // `&成员` 取址（不是数组字段）也必须能表达：这是 §7.29 同一缺口的另一侧。
    let addr_of_scalar_member = "struct P { int a; int b; };\n\
                                 int* t(struct P* p){ return &p->b; }\n";
    assert!(
        n_member_addr(addr_of_scalar_member, "t") >= 1,
        "&p->b 要发 MemberAddr，而不是把字段当值读"
    );

    let fixed_2d = "int t(void){ int a[3][4]; a[2][1] = 5; return a[2][1]; }\n";
    assert!(try_build(fixed_2d, "t").is_some(), "固定大小二维数组仍应走 SSA 往返");
    let ptr_elem = "int t(int* p){ p[2] = 9; return p[2]; }\n";
    assert!(try_build(ptr_elem, "t").is_some(), "指针下标（无需衰减）不得被误挡");
    assert_eq!(
        n_member_addr(ptr_elem, "t"),
        0,
        "指针下标不应凭空多造一条 MemberAddr（反向对照）"
    );
}

/// 多出口循环退路钉的是**出口所有权契约**，而不是「两个循环外后继就得挡」。
///
/// 旧版本这里钉的是旧计数口径（「体内 return + 循环落出 = 两个出口 ⇒ 必须判为多
/// 出口」），那个口径已被逐形实测推翻：同一个读数下 `for(...){if(i==2)return 7;}` 的
/// 侧出口是 `Ret` 块（不占续体名额），在到达边就地发 `return` 就对了（四档与 gcc 一致，
/// 见 `tests/fixtures/opt/loop_exit_ownership.c`）。现在钉的是：哪个出口是自然落点
/// (`plan.cont`)、其余出口走哪种落地方式 (`LoopExitOwner`)，以及仍不可表达的那一族确实被挡。
///
/// 两面仍然都要钉（历史先例：「续体被外层共享」那条判据实测会挡掉**所有** for 循环）：
/// 上半是「不该挡的别挡」（捕手，与 `plain_loops_still_go_through_ssa` / `adv` 夹具同一
/// 方向），下半是「该挡的必须挡」。
#[test]
fn multi_exit_cycle_predicate_is_narrow_and_sound() {
    // ---- 一面：这些形状都必须**不挡**（逐个都有四档 gcc 实测支撑）----
    let unblocked: &[(&str, &str, &str)] = &[
        (
            "普通 for（P12-B 验收夹具就是这个形状）",
            "s",
            "long s(long n){long acc=0;for(long i=0;i<n;i++)acc+=i*3;return acc+n;}\n",
        ),
        (
            "只有 continue",
            "s",
            "long s(long n){long acc=0;for(long i=0;i<n;i++){if(i%2==0)continue;acc+=i;}return acc;}\n",
        ),
        (
            "一句普通 break（桩块与合并块不得数成两个出口）",
            "s",
            "int s(int n){int acc=0;for(int i=0;i<n;i++){if(i==3)break;acc+=i;}return acc;}\n",
        ),
        (
            "两个 break 合并到同一个落点",
            "s",
            "int s(int n){int acc=0;for(int i=0;i<n;i++){if(i==1)break;if(i==2)break;acc+=i;}return acc;}\n",
        ),
        (
            "体内 return + 循环落出（旧口径错挡的那一条）",
            "a",
            "int a(int n){for(int i=0;i<n;i++){if(i==2)return 7;}return 1;}\n",
        ),
        (
            "return 与 break 同现",
            "a",
            "int a(int n){int s=0;for(int i=0;i<n;i++){if(i==1)return 9;if(i==3)break;s+=i;}return s;}\n",
        ),
        (
            "两个不同的纯 return 块",
            "a",
            "int a(int n){int s=0;for(int i=0;i<n;i++){if(i==1)return 9;if(i==2)return 8;s+=i;}return s;}\n",
        ),
        (
            "侧出口自带语句再跳出（InlineOnEdge）",
            "a",
            "int g;int a(int n){int s=0;for(int i=0;i<n;i++){if(i==1){g=101;break;}s+=i;}return s+g;}\n",
        ),
        (
            "旋转 do-while + 体内 return（节点头即分支点）",
            "a",
            "int a(int n){int i=0;do{i++;if(i==3)return 42;}while(i<n);return -1;}\n",
        ),
        (
            "无限循环 + 节点头 return/break 并存",
            "a",
            "int a(int n){int i=0;for(;;){if(i>=n)return 5;if(i==2)break;i++;}return 6;}\n",
        ),
        (
            "break 落到桩块且循环后还有代码",
            "a",
            "int a(int n){int s=0;for(int i=0;i<n;i++){if(i==2)break;s+=i;}if(s>3)return 1;return 2;}\n",
        ),
        // 收窄的**代价上限**也要钉住：`header_break_with_named_write` 只能挡「头块上真
        // 有一句 break、而续体由另一个 latch 提供」那一族。少了 `!tail_exits.is_empty()`
        // 限定就会把 `while (n--)` 这一整族（头块自己就是唯一落出的条件块）一并踢出 SSA
        // ——那是相对 HEAD 的覆盖面倒退，不优化但也不报错，**任何 gcc 差分测试都拦不住它**。
        (
            "while(k-- > 0) 无侧出口（C 里最常见，不得被收窄误挡）",
            "a",
            "int a(int n){int k=n,acc=0;while(k-- > 0){acc+=k;}return acc;}\n",
        ),
        (
            "while(k-- > 0) + 体内 break（头块那条边是循环条件本身）",
            "a",
            "int a(int n){int k=n,s=0;while(k-- > 0){if(k==2)break;s+=k;}return s;}\n",
        ),
        (
            "while(k-- > 0) + return 与 break 同现",
            "a",
            "int a(int n){int k=n,s=0;while(k-- > 0){if(k==3)return 91;if(k==1)break;s+=k;}return s+1;}\n",
        ),
    ];
    for (why, fname, src) in unblocked {
        let func = try_build(src, fname).unwrap_or_else(|| panic!("{why}: 应能进 SSA 建 IR"));
        assert!(!has_multi_exit_cycle(&func), "{why}: 不该被判为不可表达");
    }

    // ---- 二面：钉住「谁当自然落点、其余边走哪种落地方式」----
    // `for(int i=0;i<n;i++){if(i==2)return 7;} return 1;`
    //   自然落点 = for.end（循环外也有前驱进入，只能发一次）
    //   侧出口 if.then = 体内纯 return ⇒ 在到达边就地发 `return`
    let early = "int a(int n){for(int i=0;i<n;i++){if(i==2)return 7;}return 1;}\n";
    let (header, plan, owners) = shape_owners(early, "a");
    assert_eq!(header, "for.body", "早退循环的循环头应是体内块");
    assert_eq!(plan.cont.as_deref(), Some("for.end"), "自然落点应是 for.end");
    assert!(plan.expressible, "该形状应可表达: {}", plan.reason);
    assert_eq!(
        owner_of_edge(&owners, "if.then"),
        LoopExitOwner::PureReturn,
        "体内 return 的落点必须在到达边发 `return`（不占续体名额）"
    );
    assert_eq!(
        owner_of_edge(&owners, "for.end"),
        LoopExitOwner::Continuation,
        "循环落出的那条边只能发 `break`，续体在循环之后发一次"
    );
    // 提升许可：本形状的循环头是**体内块** for.body，它的出口边 `if.then` 不是续体，
    // 提升就会把那条 `return` 随丢弃的头块分支一起丢掉（§7.32 那 14 条错值的根因），
    // 所以计划必须说「不许提升」，发射器退回 `while(1){ if(i==2) return 7; … }`。
    assert!(
        !plan.allow_header_lift,
        "节点头的出口边不是续体 ⇒ 不得把它的 Branch 提成 while 条件"
    );

    // `for(...){if(i==1){g=101;break;}s+=i;}` 的侧出口块自带语句且前驱全在循环内
    // ⇒ 只能在到达边就地展开，绝不能当续体（否则续体双份执行）。
    let inline_src =
        "int g;int a(int n){int s=0;for(int i=0;i<n;i++){if(i==1){g=101;break;}s+=i;}return s;}\n";
    let (header2, plan2, owners2) = shape_owners(inline_src, "a");
    assert_eq!(header2, "for.body");
    assert_eq!(plan2.cont.as_deref(), Some("for.end"), "续体仍是循环落出块");
    assert!(
        owners2
            .iter()
            .any(|(_, _, o)| *o == LoopExitOwner::InlineOnEdge),
        "带语句且前驱全在循环内的侧出口必须在到达边展开"
    );
    // 头块的出口边（`if.then`，带语句、前驱全在循环内）不是续体 ⇒ 提升同样被否掉，
    // 那条边回到 `if` 语句上由出口边守卫安排。
    assert!(
        !plan2.allow_header_lift && plan2.expressible,
        "侧出口展开与提升互斥：应可表达且不许提升 ({})",
        plan2.reason
    );

    // ---- 三面：仍不可表达的那一族必须挡（退回 AST，不优化但正确）----
    // 节点头自己就是循环体块（旋转 do-while）、带对具名存储的写、而它那条出口边是
    // `break`（折到续体）。实测：提升会丢头块的写（`do_break(3)` 得 9 而 gcc 得 6），
    // 退回 `while(1){if(c)…}` 同样错 ⇒ 只能整函数退出 SSA 往返。
    let blocked_srcs: &[(&str, &str)] = &[
        (
            "旋转 do-while：头块带具名写 + break 到续体",
            "int a(int n){int i=0,s=0;do{s+=i;i++;if(i==3)break;}while(i<n);return s+i;}\n",
        ),
        (
            "无限循环：头块带累加写 + break 到续体",
            "int a(int n){int i=0,s=0;for(;;){s+=i;i++;if(i>n)break;}return s+i;}\n",
        ),
    ];
    for (why, src) in blocked_srcs {
        let func = try_build(src, "a").unwrap_or_else(|| panic!("{why}: 应能建 IR"));
        assert!(has_multi_exit_cycle(&func), "{why}: 必须挡（两种发法实测都错）");
    }
    // 同形状但侧出口是 `return`（而不是 break）⇒ 必须仍然放行：这一对是上面那两条
    // 反向对照，证明挡的是「头块 break 到续体」而不是「所有带写的节点头」。
    let near_miss =
        "int a(int n){int i=0,s=0;do{s+=i;i++;if(i==3)return 42;}while(i<n);return s+i;}\n";
    let nm = try_build(near_miss, "a").expect("near miss 应能建 IR");
    assert!(
        !has_multi_exit_cycle(&nm),
        "同形状但侧出口是 return：已实测四档与 gcc 一致，不得误挡"
    );
}

/// 取证辅助：取函数里**第一个**自然循环的节点头、所有权计划，以及每条出口边的
/// `(原始目标, 折叠后代表块, 所有权)`。判据与发射器走同一套 API，这里也只是读它们。
fn shape_owners(
    src: &str,
    fname: &str,
) -> (String, LoopExitPlan, Vec<(String, String, LoopExitOwner)>) {
    let func = try_build(src, fname).expect("应能建 IR");
    let doms = compute_dominators(&func);
    let mut headers: Vec<&String> = func.blocks.keys().collect();
    headers.sort();
    for header in headers {
        let Some((latches, body_incl)) = loop_shape(&func, header, &doms) else {
            continue;
        };
        let body: HashSet<String> = body_incl
            .iter()
            .filter(|b| b.as_str() != header.as_str())
            .cloned()
            .collect();
        let plan = loop_exit_plan(&func, header, &body, &latches, &doms);
        let mut names: Vec<&String> = body_incl.iter().collect();
        names.sort();
        let mut owners = Vec::new();
        for name in names {
            let Some(b) = func.blocks.get(name) else {
                continue;
            };
            for s in term_succs(&b.term) {
                if s == header || body.contains(s) || is_back_edge(name, s, &doms) {
                    continue;
                }
                let o = loop_exit_owner(s, &func, header, &body, &doms);
                let rep = resolve_loop_exit_stub(s, &func, header, &body);
                if !owners.iter().any(|(a, _, _)| a == s) {
                    owners.push((s.to_string(), rep, o));
                }
            }
        }
        return (header.clone(), plan, owners);
    }
    panic!("没找到自然循环");
}

fn owner_of_edge(owners: &[(String, String, LoopExitOwner)], target: &str) -> LoopExitOwner {
    for (_, rep, o) in owners {
        if rep == target {
            return *o;
        }
    }
    panic!("出口边里没有 {target}");
}

/// **取证窗口**（`#[ignore]`，不进任何判据）：把多出口循环各形状的**真实 CFG** 打出来。
///
/// 运行：`cargo test --lib --release -- --ignored --nocapture multi_exit_shape_probe`
///
/// 为什么需要它：`has_multi_exit_cycle` 挡的是「循环外后继 > 1」，而历史两次被 gcc 差分
/// 证伪都源于**拿猜测的形状当边界**（§7.28 的三条改法、§7.32 的放松）。这张表逐条打印
/// 每个出口边的客观读数（terminator / phi / 语句数 / 前驱集合 / 是否头块直出 / 是否
/// latch 落出 / 桩块折叠后的目标），出口所有权模型的每一层放行都必须对着它说话。
#[test]
#[ignore]
fn multi_exit_shape_probe() {
    let shapes: &[(&str, &str)] = &[
        (
            "S1_plain_for",
            "long a(long n){long acc=0;for(long i=0;i<n;i++)acc+=i*3;return acc+n;}\n",
        ),
        (
            "S2_continue_only",
            "long a(long n){long acc=0;for(long i=0;i<n;i++){if(i%2==0)continue;acc+=i;}return acc;}\n",
        ),
        (
            "S3_early_return",
            "int a(int n){for(int i=0;i<n;i++){if(i==2)return 7;}return 1;}\n",
        ),
        (
            "S4_ret_and_break",
            "int a(int n){int s=0;for(int i=0;i<n;i++){if(i==1)return 9;if(i==3)break;s+=i;}return s;}\n",
        ),
        (
            "S5_break_only",
            "int a(int n){int s=0;for(int i=0;i<n;i++){if(i==3)break;s+=i;}return s;}\n",
        ),
        (
            "S6_side_stmt_break",
            "int g;int a(int n){int s=0;for(int i=0;i<n;i++){if(i==1){g=101;break;}s+=i;}return s;}\n",
        ),
        (
            "S7_two_breaks_merge",
            "int a(int n){int s=0;for(int i=0;i<n;i++){if(i==1)break;if(i==2)break;s+=i;}return s;}\n",
        ),
        (
            "S8_do_while_rotated",
            "int a(int n){int i=0;do{i++;if(i==3)return 42;}while(i<n);return -1;}\n",
        ),
        (
            "S9_infinite_loop_head_branch",
            "int a(int n){int i=0;for(;;){if(i>=n)return 5;if(i==2)break;i++;}return 6;}\n",
        ),
        (
            "S10_ret_twopaths_same_block",
            "int a(int n){int s=0;for(int i=0;i<n;i++){if(i==1)return 9;if(i==2)return 9;s+=i;}return s;}\n",
        ),
        (
            "S11_ret_depends_on_loop_var",
            "int a(int n){int s=0;for(int i=0;i<n;i++){if(i==2)return s;s+=i;}return s;}\n",
        ),
        (
            "S12_nested_inner_early_return",
            "int a(int n){int acc=0;for(int i=0;i<n;i++){for(int j=0;j<i;j++){if(j==1)return 77;}}return acc;}\n",
        ),
        (
            "S13_break_then_outer_code",
            "int a(int n){int s=0;for(int i=0;i<n;i++){if(i==2)break;s+=i;}if(s>3)return 1;return 2;}\n",
        ),
        (
            "S14_while_carried_header_inst",
            "int a(int n){int k=n,s=0;while(k-- > 0){if(k==2)return 7;s+=k;}return s;}\n",
        ),
        (
            "S15_pure_infinite_no_fallthrough",
            "int a(int n){for(;;){if(n<=0)return 3;n--;}}\n",
        ),
        // ---- 对抗样本 (本轮新加的「应当仍被挡」形状; 拿不到它们就证明不了退路变窄而不是没了) ----
        (
            "T1_multi_pred_ret_plus_merge",
            "int a(int n){int s=0;for(int i=0;i<n;i++){if(i==1||i==2)return 5;s+=i;}return s;}\n",
        ),
        (
            "T2_multi_pred_ret_only",
            "int a(int n){for(int i=0;i<n;i++){if(i==1||i==2)return 5;}}\n",
        ),
        (
            "T3_break_and_return_and_continue",
            "int a(int n){int s=0;for(int i=0;i<n;i++){if(i==1)continue;if(i==3)return 4;if(i==5)break;s+=i;}return s;}\n",
        ),
        (
            "T4_nested_two_breaks",
            "int a(int n){int s=0;for(int i=0;i<n;i++){for(int j=0;j<n;j++){if(j==1)break;}if(i==2)break;s+=i;}return s;}\n",
        ),
        (
            "T5_do_while_break_tail",
            "int a(int n){int i=0;do{if(i==2)break;i++;}while(i<n);return i;}\n",
        ),
        (
            "T6_two_shared_exits",
            "int a(int n){int s=0;for(int i=0;i<n;i++){if(i==1){s=9;return s;}if(i==3)return 4;s+=i;}return s;}\n",
        ),
    ];
    for (tag, src) in shapes {
        let Some(func) = try_build(src, "a") else {
            println!("==== {tag}: BUILD-FAIL (函数根本没进 SSA) ====");
            continue;
        };
        println!("\n==== SHAPE {tag} ====");
        println!(
            "blocked_before(unbounded outsides) = {} / blocked_after(plan inexpressible) = {}",
            has_multi_exit_cycle_before_shape_gate(&func),
            has_multi_exit_cycle(&func)
        );
        print!("{}", shape_probe_report(&func));
    }
}

/// **旧退路的计数规则**（仅供取证对比，不参与任何语义）：「循环外后继个数 > 1」。
///
/// 把它留在测试模块而不是删掉，是因为本轮的「拆了多少退路」必须能被直接读到:
/// 同一个形状用旧规则与新计划各判一次, 差集就是本轮真正释放的覆盖面。
fn has_multi_exit_cycle_before_shape_gate(func: &Function) -> bool {
    let doms = compute_dominators(func);
    let mut headers: Vec<&String> = func.blocks.keys().collect();
    headers.sort();
    for header in headers {
        let Some(hb) = func.blocks.get(header) else {
            continue;
        };
        let latches: Vec<String> = hb
            .preds
            .iter()
            .filter(|p| is_back_edge(p, header, &doms))
            .cloned()
            .collect();
        if latches.is_empty() {
            continue;
        }
        let body: HashSet<String> = find_loop_body_multi(func, header, &latches)
            .into_iter()
            .collect();
        let mut outside: Vec<String> = Vec::new();
        let mut names: Vec<&String> = body.iter().chain(std::iter::once(header)).collect();
        names.sort();
        for name in names {
            let Some(b) = func.blocks.get(name) else {
                continue;
            };
            for s in term_succs(&b.term) {
                if s == header || body.contains(s) || is_back_edge(name, s, &doms) {
                    continue;
                }
                if !outside.iter().any(|e| e == s) {
                    outside.push(s.to_string());
                }
            }
        }
        if outside.len() > 1 {
            return true;
        }
    }
    false
}

/// [`multi_exit_shape_probe`] 的读数器：块清单 + 每个自然循环的出口边分类表。
fn shape_probe_report(func: &Function) -> String {
    let doms = compute_dominators(func);
    let mut out = String::new();
    out.push_str("  -- blocks --\n");
    for name in &func.block_order {
        let Some(bb) = func.blocks.get(name) else {
            continue;
        };
        let stmts = bb
            .insts
            .iter()
            .filter(|i| !is_pass_hint_inst(&i.kind))
            .count();
        out.push_str(&format!(
            "    {:<16} stmts={} phis={} preds={:?} term={:?}\n",
            bb.name,
            stmts,
            bb.phis.len(),
            bb.preds,
            bb.term
        ));
    }
    let mut headers: Vec<&String> = func.blocks.keys().collect();
    headers.sort();
    for header in headers {
        let hb = &func.blocks[header];
        let latches: Vec<String> = hb
            .preds
            .iter()
            .filter(|p| is_back_edge(p, header, &doms))
            .cloned()
            .collect();
        if latches.is_empty() {
            continue;
        }
        let body: HashSet<String> = find_loop_body_multi(func, header, &latches)
            .into_iter()
            .collect();
        out.push_str(&format!(
            "  -- loop header={} body={:?} latches={:?} --\n",
            header,
            {
                let mut v: Vec<&str> = body.iter().map(|s| s.as_str()).collect();
                v.sort();
                v
            },
            latches
        ));
        // 头块条件边的两侧与 latch 的非回边后继 —— 续体唯一化的两个候选真源
        if let Some(hb) = func.blocks.get(header) {
            out.push_str(&format!(
                "    header_succs={:?} (cond-exit 候选: 其中不在 body 的边)\n",
                term_succs(&hb.term)
            ));
        }
        let mut names: Vec<&String> = body.iter().chain(std::iter::once(header)).collect();
        names.sort();
        let mut distinct: Vec<String> = Vec::new();
        for name in names {
            let Some(b) = func.blocks.get(name) else {
                continue;
            };
            for s in term_succs(&b.term) {
                if s == header || body.contains(s) || is_back_edge(name, s, &doms) {
                    continue;
                }
                let Some(tb) = func.blocks.get(s) else {
                    continue;
                };
                let pure_ret = is_loop_exit_pure_return(s, func);
                let stmts = tb
                    .insts
                    .iter()
                    .filter(|i| !is_pass_hint_inst(&i.kind))
                    .count();
                let stub =
                    stmts == 0 && tb.phis.is_empty() && matches!(tb.term, Terminator::Jump(_));
                let all_in = tb
                    .preds
                    .iter()
                    .all(|p| p.as_str() == header || body.contains(p.as_str()));
                let head_direct = term_succs(&hb.term).iter().any(|x| *x == s);
                let latch_exit = body.iter().any(|m| {
                    m != header
                        && func
                            .blocks
                            .get(m)
                            .map(|b| term_succs(&b.term).iter().any(|x| *x == s))
                            .unwrap_or(false)
                });
                let folded = probe_fold_stub(s, func, &body);
                out.push_str(&format!(
                    "    EXIT {name}->{s}: term={:?} phis={} stmts={stmts} preds={:?} pure_return={pure_ret} \
                     zero_stmt_stub={stub} all_preds_in_loop={all_in} header_direct={head_direct} \
                     latch_exit={latch_exit} fold=>{folded}\n",
                    tb.term,
                    tb.phis.len(),
                    tb.preds,
                ));
                if !distinct.iter().any(|e| e == s) {
                    distinct.push(s.to_string());
                }
            }
        }
        let folded_distinct: Vec<String> = distinct
            .iter()
            .map(|d| probe_fold_stub(d, func, &body))
            .filter(|d| !d.is_empty())
            .collect::<Vec<_>>();
        let mut fd = folded_distinct.clone();
        fd.sort();
        fd.dedup();
        out.push_str(&format!(
            "    outsides(raw)={:?} outsides(folded)={:?} blocked={}\n",
            distinct,
            fd,
            distinct.len() > 1
        ));
        // ---- 本轮的所有权计划读数 (设计段每一条放行都必须对着这里说话) ----
        if let Some((pl_latches, pl_body)) = loop_shape(func, header, &doms) {
            let plan = loop_exit_plan(func, header, &pl_body, &pl_latches, &doms);
            out.push_str(&format!(
                "    PLAN cont={:?} expressible={} allow_header_lift={} reason={}\n",
                plan.cont, plan.expressible, plan.allow_header_lift, plan.reason
            ));
            let mut enames: Vec<&String> = pl_body.iter().collect();
            enames.sort();
            for name in enames {
                let Some(b) = func.blocks.get(name) else {
                    continue;
                };
                for s in term_succs(&b.term) {
                    if s == header || pl_body.contains(s) || is_back_edge(name, s, &doms) {
                        continue;
                    }
                    let owner = loop_exit_owner(s, func, header, &pl_body, &doms);
                    out.push_str(&format!("      EDGE {name}->{s}: owner={owner:?}\n"));
                }
            }
        }
    }
    out
}

/// 沿「无语句、无 phi、仍在循环外」的 `Jump` 链折叠桩块（break 的空壳落点）。
fn probe_fold_stub(target: &str, func: &Function, body: &HashSet<String>) -> String {
    let mut cur = target.to_string();
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        if !seen.insert(cur.clone()) {
            return cur;
        }
        let Some(bb) = func.blocks.get(&cur) else {
            return cur;
        };
        let stmts = bb
            .insts
            .iter()
            .filter(|i| !is_pass_hint_inst(&i.kind))
            .count();
        let is_stub = stmts == 0 && bb.phis.is_empty();
        match &bb.term {
            Terminator::Jump(n) if is_stub && !body.contains(n) => cur = n.clone(),
            _ => return cur,
        }
    }
}

/// 无法在 SSA 里忠实表达的表达式必须让**整个函数**放弃 SSA 化，而不是静默
/// 返 `Operand::Int(0)`（那条路径曾在 -O2 产生一批错值，见 `Builder::unsupported`）。
///
/// 反向对照同样重要：不带该形态的同一函数必须能进管线，否则「所有函数全部
/// 放弃 SSA」也能把测试做绿（那其实就是「SSA 整层空转」那种假绿）。
#[test]
fn unlowerable_expr_bails_out_of_ssa_instead_of_becoming_zero() {
    assert!(
        try_build("int a(void){ return (int[]){1,2,3}[1]; }\n", "a").is_none(),
        "复合字面量不可降级"
    );
    // `&x[2]` 曾经也只能退 SSA（IR 里没有「下标的左值地址」形态），现已由
    // `InstKind::IndexAddr` 表达。这里不只断言「能进管线」，而是钉住 IR 里
    // 确实出现了那条指令：只查 is_some 的话，将来删了它又换回退路也能做绿。
    {
        let f = try_build("int b(void){ int x[4]; int *p = &x[2]; return (int)(long)p; }\n", "b")
            .expect("&下标 现在应能进 SSA（IndexAddr）");
        let n = f
            .blocks
            .values()
            .flat_map(|b| b.insts.iter())
            .filter(|i| matches!(i.kind, InstKind::IndexAddr { .. }))
            .count();
        assert!(n >= 1, "`&x[2]` 必须发 IndexAddr，实得 {n} 条");
    }
    assert!(
        try_build("int c(void){ int x[4]; x[2] = 9; return x[2]; }\n", "c").is_some(),
        "对照组必须仍进 SSA 管线"
    );
}

/// 一般的「左值地址」形态：`&m[i][j]`（下标基址本身是计算出来的数组行）过去只能
/// 整函数退 SSA，现在逐层发 `IndexAddr`。只钉「进管线 + 发了多条 IndexAddr」这两件
/// SSA 结构层能直接断言的事：行步长是否算对、地址临时值是否被误内联成嵌套取址（历史
/// 段错误），由集成夹具 `lvalue_addr_rows_match_gcc_at_all_levels` 逐档与 gcc 对照钉住
/// —— 若守卫失效，-O2/-O3 会段错误或算错指针差而当场变红。
#[test]
fn nested_lvalue_addr_goes_through_ssa_as_index_addr_chain() {
    let count_idx_addr = |src: &str, name: &str| -> usize {
        try_build(src, name)
            .expect("&m[i][j] 现在应能进 SSA")
            .blocks
            .values()
            .flat_map(|b| b.insts.iter())
            .filter(|i| matches!(i.kind, InstKind::IndexAddr { .. }))
            .count()
    };
    // 二维：一行地址 + 一个元素地址 = 至少 2 条 IndexAddr。
    let d2 = "int t(int i,int j){ int m[3][5]; int *p = &m[i][j]; return (int)(long)p; }\n";
    assert!(
        count_idx_addr(d2, "t") >= 2,
        "`&m[i][j]` 应逐层发 >=2 条 IndexAddr（行 + 元素）"
    );
    // 三维：至少 3 条（两层行 + 元素）。
    let d3 =
        "int t(int i,int j,int k){ int m[2][3][4]; int *p = &m[i][j][k]; return (int)(long)p; }\n";
    assert!(count_idx_addr(d3, "t") >= 3, "`&m[i][j][k]` 应逐层发 >=3 条 IndexAddr");
    // 反向对照：不经计算行的 `&m[0][j]`（基址是具名数组，不需内层行地址）仍只需 1 层？
    // 不——`m[0]` 仍是 Index，会递归发一条行地址；故也 >=2。真正只需 1 条的是一维。
    let d1 = "int t(int i){ int a[4]; int *p = &a[i]; return (int)(long)p; }\n";
    assert_eq!(
        count_idx_addr(d1, "t"),
        1,
        "一维 `&a[i]` 只应发 1 条 IndexAddr（基址是具名数组，无计算行）"
    );
}

/// 内存左值的**读—改—写**必须在 IR 里同时出现一次读与一次写回。
///
/// 这条钉的是一个 P0 静默误编译：`Expr::PostInc/PreInc` 那条 arm 只认 `Expr::Ident`
/// 操作数，其余形态直接 `return Operand::Int(one)` —— 既不读也不写、连 `unsupported`
/// 都不标，函数照样进 SSA。实测 -O2 下 `int r = ++*p;` 得到常量 1 且 `*p` 没被写。
///
/// 为什么不只靠执行级夹具 `ssa_mem_rmw_matches_gcc_at_all_levels`：那条在形态
/// 退化成「整函数退 SSA」时**仍然会绿**（产物正确，只是不优化）。这里钉的是
/// 「IR 里确实做了这件事」，与 `MemberAddr`/`IndexAddr` 同一口径。
#[test]
fn mem_lvalue_rmw_emits_load_and_store_in_ir() {
    let cases = [
        ("int f(int *p){ return ++*p; }\n", "f", "`++*p`"),
        ("int f(int *p){ return (*p)++; }\n", "f", "`(*p)++`"),
        ("int f(int a[4], int i){ return a[i]++; }\n", "f", "`a[i]++`"),
        ("struct S{int f;}; int f(struct S *q){ return ++q->f; }\n", "f", "`++q->f`"),
    ];
    for (src, name, what) in cases {
        let func = try_build(src, name).unwrap_or_else(|| panic!("{what} 现在应能进 SSA"));
        let (mut reads, mut writes) = (0usize, 0usize);
        for b in func.blocks.values() {
            for i in &b.insts {
                match i.kind {
                    InstKind::Load { .. }
                    | InstKind::IndexLoad { .. }
                    | InstKind::MemberLoad { .. } => reads += 1,
                    InstKind::Store { .. }
                    | InstKind::IndexStore { .. }
                    | InstKind::MemberStore { .. } => writes += 1,
                    _ => {},
                }
            }
        }
        assert!(reads >= 1, "{what}: IR 里必须有一次读，实得 {reads}");
        assert!(writes >= 1, "{what}: IR 里必须有一次写回，实得 {writes}");
    }

    // 复合赋值：读与写必须共用**同一个**地址临时值 —— 这就是「左值只求值一次」
    // 在 IR 里的形状。旧降级把 parser 展开出来的左值拷贝再降一遍，两个地址是
    // 两个临时值；下标带副作用时（`a[i++] += 5`）它们不是同一个格子。
    let func = try_build("int f(int a[4], int i){ return a[i] += 7; }\n", "f")
        .expect("`a[i] += 7` 现在应能进 SSA");
    let mut load_addrs: Vec<String> = Vec::new();
    let mut store_addrs: Vec<String> = Vec::new();
    for b in func.blocks.values() {
        for i in &b.insts {
            match &i.kind {
                InstKind::Load { e, .. } | InstKind::Store { e, .. } => {
                    if let Operand::Var(v) = e {
                        let n = v.name();
                        if matches!(i.kind, InstKind::Load { .. }) {
                            load_addrs.push(n);
                        } else {
                            store_addrs.push(n);
                        }
                    }
                },
                _ => {},
            }
        }
    }
    assert!(
        load_addrs.iter().any(|a| store_addrs.contains(a)),
        "读与写必须共用同一个地址临时值（左值只求值一次），实得 load={load_addrs:?} store={store_addrs:?}"
    );
}

/// 逗号表达式的左操作数只为副作用存在，但必须照常降级：旧实现直接丢掉 `l`,
/// 于是 `x = (g(), 1)` 里的调用在产物里消失。这条测试钉住两个面：
/// 函数仍能进 SSA（逗号不是放弃理由）且 IR 里真有那一次调用。
#[test]
fn comma_keeps_the_left_operand_side_effect() {
    let f = try_build("int g(void); int f(void){ int x = (g(), 1); return x; }\n", "f")
        .expect("逗号表达式不应成为放弃 SSA 的理由");
    let calls = f
        .blocks
        .values()
        .flat_map(|bb| bb.insts.iter())
        .filter(|i| matches!(&i.kind, InstKind::Call { callee, .. } if matches!(callee, Operand::SymAddr(n, _) if n == "g")))
        .count();
    assert_eq!(calls, 1, "必须保留逗号左侧的那一次调用");
}

/// 三元分支带副作用/可陷获时不得降级成 `Select`（它两侧都执行），而要让整个
/// 函数放弃 SSA 化。反向对照：纯分支必须仍走 Select，否则「一律放弃」也能绿。
#[test]
fn ternary_with_side_effect_branches_bails_out() {
    assert!(
        try_build("int t1(void){ int x = 3, y = 2; return (x < y ? x++ : y++); }\n", "t1")
            .is_none(),
        "两侧自增不得被无条件执行"
    );
    assert!(
        try_build("int t3(int c, int *p){ return c ? *p : 0; }\n", "t3").is_none(),
        "分支里的读内存不得被提前执行"
    );
    let f = try_build("int t2(int c,int a,int b){ return c ? a + 1 : b + 2; }\n", "t2")
        .expect("纯三元表达式仍应进 SSA 管线");
    assert!(
        f.blocks.values().any(|bb| bb
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::Select { .. }))),
        "纯分支仍降级成 Select（否则本判据只是在关掉整层优化）"
    );
}

/// 不被认识的赋值左值必须让整函数放弃 SSA 化，而不是“只做不写”。
///
/// `__real__ z = r` 这类左值不在 `lower_lvalue` 的形态表里，旧实现于是
/// 什么都不发、只把 RHS 当表达式值返回 —— 两条写指令在 IR 里根本不存在，
/// `double _Complex z; __real__ z=r; __imag__ z=i; return z;` 在 -O2 返回未初始化数据。
/// 反向对照：普通局部赋值与 `p->a = v` 这类受支持的左值仍须进管线。
#[test]
fn unrecognised_assign_lvalue_bails_instead_of_dropping_the_store() {
    assert!(
        try_build(
            "double _Complex f(void){ double _Complex z; __real__ z = 1; __imag__ z = 2; return z; }\n",
            "f"
        )
        .is_none(),
        "复数字段赋值不能被静默丢弃"
    );
    let ok =
        try_build("struct S{double a;}; double g(struct S *p){ p->a = 1; return p->a; }\n", "g")
            .expect("受支持的左值仍应进 SSA 管线");
    // 本判据要的是「写确实进了 IR」，不是某个具体指令形态。标量成员写现在走
    // `&p->a` 取址 + `Store`（聚合基址按右值降级会把写落进 `rep movsq` 的帧副本，
    // 见 `lower_lvalue` 的注释），因此 `Store` 与 `MemberStore` 都算数；
    // 一条都不发才是本测试要拦的回归。
    assert!(
        ok.blocks.values().any(|bb| bb
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::MemberStore { .. } | InstKind::Store { .. }))),
        "成员赋值必须真的发射一条写指令（Store 或 MemberStore）"
    );
}

/// 聚合对象的整体赋值在 IR 里没有表示，必须让整个函数退出 SSA，而不是只搬
/// 一半字节。实测 -O2 旧行为：`struct P2 { long a, b; } g; g = p;` 之后
/// `g.b` 读到 0 —— 那条 `Store` 重建后是 `*(&g) = p`，后端按标量搬 8 字节。
/// 同一份代码里 `struct P2 t = p;` 是对的（它走 `lower_expr_into`，不走 Store），
/// 所以坏的不是「结构体」而是**给具名对象整体赋值**这条形态。
/// 反向对照：成员赋值 `g.b = v` 必须仍进管线（不能一刀切退出）。
#[test]
fn aggregate_assignment_to_named_object_bails() {
    assert!(
        try_build("struct P { long a; long b; }; struct P g; void f(struct P p){ g = p; }\n", "f")
            .is_none(),
        "聚合整体赋值不得降级成一次标量 Store"
    );
    assert!(
        try_build("struct P { long a; long b; }; struct P g; void f(long v){ g.b = v; }\n", "f")
            .is_some(),
        "成员赋值是受支持的形态，不得一起退出 SSA 管线"
    );
}

/// 复数算术的重建类型必须是复数（关系运算仍是 int）。
///
/// codegen 靠类型选 `gen_complex_binop`；`infer_binop_type` 少了复数分支时，
/// `x + y`（两侧都是 `double _Complex`）会掉到 Int 回退，复数加法退化成
/// 标量运算（实测 -O2 得 (1.0e9, 0)，gcc 与 -O0 都是 (6, 8)）。
#[test]
fn infer_binop_type_keeps_complex_results() {
    let sp = Span::point("t.c", 1, 1);
    let cx = || Expr::Ident {
        name: crate::ast::Ident {
            name: "x".into(),
            span: sp.clone(),
        },
        span: sp.clone(),
        ty: Type::Complex(Box::new(QualType::new(Type::Double))),
    };
    let dbl = || Expr::Ident {
        name: crate::ast::Ident {
            name: "y".into(),
            span: sp.clone(),
        },
        span: sp.clone(),
        ty: Type::Double,
    };
    for op in [BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div] {
        let t = infer_binop_type(&op, &cx(), &cx());
        assert!(
            matches!(&t, Type::Complex(q) if matches!(q.base, Type::Double)),
            "{:?} 的复数运算结果必须是 _Complex double，实得 {:?}",
            op,
            t
        );
        // 复数与实数混合运算得同宽度的复数 (C11 6.3.1.8)。
        assert!(
            matches!(infer_binop_type(&op, &cx(), &dbl()), Type::Complex(_)),
            "{:?} 复数+实数应仍为复数",
            op
        );
    }
    // 关系/相等运算的结果仍是 int（上面必须早于复数分支）。
    assert!(
        matches!(infer_binop_type(&BinOp::EqEq, &cx(), &cx()), Type::Int),
        "复数相等运算结果必须是 int"
    );
}
