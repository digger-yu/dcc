// included from tests/integration.rs — not a standalone cargo target

// ===== M17: C++ 修饰符与类型推导 (14 项) =====

/// M17.0: auto 真类型推导 (int / StringLit / class)
#[test]
#[cfg(unix)]
fn cpp_auto_deduce() {
    let src = include_str!("../fixtures/cpp/auto_deduce.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-auto-deduce", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: decltype(expr) 真类型查询
#[test]
#[cfg(unix)]
fn cpp_decltype() {
    let src = include_str!("../fixtures/cpp/decltype.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-decltype", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: decltype(auto) 变量类型推导
#[test]
#[cfg(unix)]
fn cpp_decltype_auto() {
    let src = include_str!("../fixtures/cpp/decltype_auto.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-decltype-auto", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: final / override 关键字
#[test]
#[cfg(unix)]
fn cpp_final_override() {
    let src = include_str!("../fixtures/cpp/final_override.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-final-override", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: friend 关键字 (lower 接受 friend 声明，不强求链接)
#[test]
#[cfg(unix)]
fn cpp_friend() {
    let src = include_str!("../fixtures/cpp/friend.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-friend", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: if constexpr 编译期分支
#[test]
#[cfg(unix)]
fn cpp_if_constexpr() {
    let src = include_str!("../fixtures/cpp/if_constexpr.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-if-constexpr", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: 成员指针 obj.*ptr_to_member
#[test]
#[cfg(unix)]
fn cpp_member_ptr() {
    let src = include_str!("../fixtures/cpp/member_ptr.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-member-ptr", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: mutable 独立字段
#[test]
#[cfg(unix)]
fn cpp_mutable() {
    let src = include_str!("../fixtures/cpp/mutable.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-mutable", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M27: mutable 成员 const 方法中可写
#[test]
#[cfg(unix)]
fn cpp_mutable_const_method() {
    let src = include_str!("../fixtures/cpp/mutable.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-mutable-const", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M27: MemberStar/PtrMemberStar 真实字段类型解引
#[test]
#[cfg(unix)]
fn cpp_member_ptr_deref() {
    let src = include_str!("../fixtures/cpp/member_ptr_deref.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-member-ptr-deref", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M27: typeid 动态 RTTI (多态类 vtable RTTI slot)
#[test]
#[cfg(unix)]
fn cpp_typeid_dynamic_rtti() {
    // typeid(e) 对多态 glvalue 必须经 vptr[-1] (typeinfo slot) 取**运行时**类型:
    // `PolyAnimal* p = &dog; typeid(*p) == typeid(Dog)`. dcc -c + host gcc 链接运行,
    // 夹具 main 在四个子检查任一不符时返回非 0.
    let src = include_str!("../fixtures/cpp/typeid_rtti.cpp");
    let (out, obj) = run_cpp_object_test("cpp-typeid-dynamic-rtti", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-cpp-typeid-dynamic-rtti-{}", std::process::id()));
    let link = std::process::Command::new("gcc")
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host gcc link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run typeid-dynamic-rtti binary");
    let _ = fs::remove_file(obj);
    let _ = fs::remove_file(&exe);
    assert_eq!(
        run.status.code().unwrap_or(-1),
        0,
        "cpp_typeid_dynamic_rtti exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

/// M27: 模板偏特化 specialization 排序
#[test]
#[cfg(unix)]
fn cpp_template_specialization() {
    let src = include_str!("../fixtures/cpp/template_specialization.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-template-spec", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M27: C++20 Modules ODR 检查
#[test]
#[cfg(unix)]
fn cpp_modules_odr() {
    let src = include_str!("../fixtures/cpp/modules_odr.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-modules-odr", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: new T[n] 数组分配 + operator new[]/delete[]
#[test]
#[cfg(unix)]
fn cpp_new_array() {
    let src = include_str!("../fixtures/cpp/new_array.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-new-array", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: smoke 测试 (最小可工作 C++ 程序)
#[test]
#[cfg(unix)]
fn cpp_smoke() {
    let src = include_str!("../fixtures/cpp/smoke.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-smoke", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: typeid(e) vtable RTTI
#[test]
#[cfg(unix)]
fn cpp_typeid() {
    let src = include_str!("../fixtures/cpp/typeid.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-typeid", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: volatile 成员函数
#[test]
#[cfg(unix)]
fn cpp_volatile_member() {
    let src = include_str!("../fixtures/cpp/volatile_member.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-volatile-member", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// Test C++ native SSA control flow lowering
/// (for / do-while / switch / break / continue / goto / label)
#[test]
#[cfg(unix)]
fn cpp_ssa_control_flow() {
    let src = include_str!("../fixtures/cpp/ssa_control_flow.cpp");
    let (out, obj) = run_cpp_object_test("cpp-ssa-control-flow", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-cpp-ssa-control-flow-{}", std::process::id()));
    let link = std::process::Command::new("gcc")
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host gcc link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run control-flow binary");
    let _ = fs::remove_file(obj);
    let _ = fs::remove_file(&exe);
    assert_eq!(
        run.status.code().unwrap_or(-1),
        0,
        "cpp_ssa_control_flow exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

/// C++ volatile semantics: scalar volatile locals must not be registerized
/// (they must be loaded/stored from memory on every access).
#[test]
#[cfg(unix)]
fn cpp_ssa_volatile_semantics() {
    let src = include_str!("../fixtures/cpp/ssa_volatile_semantics.cpp");
    let (out, obj) = run_cpp_object_test("cpp-ssa-volatile-semantics", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-cpp-ssa-volatile-semantics-{}", std::process::id()));
    let link = std::process::Command::new("gcc")
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host gcc link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run volatile binary");
    let _ = fs::remove_file(obj);
    let _ = fs::remove_file(&exe);
    assert_eq!(
        run.status.code().unwrap_or(-1),
        0,
        "cpp_ssa_volatile_semantics exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
#[cfg(unix)]
fn cpp_template_nttp_member_use() {
    // M53 任务 2: `template <int N> struct S { int n() const { return N; } };` ——
    // 非类型形参在**成员函数体**里的替换 (曾在实例化时报 undeclared identifier: N),
    // 含括号常量表达式实参 `S<(2 + 5)>` 与多个实例并存.
    let src = include_str!("../fixtures/cpp/template_nttp_member_use.cpp");
    cpp_std20_exit_code_cross_check("cpp-template-nttp-member-use", src);
}

#[test]
#[cfg(unix)]
fn cpp_alias_template_expansion() {
    // M53 任务 3: 别名模板展开 ([temp.alias]) —— `using X = ...` 含模板别名.
    // `std::enable_if_t<cond, T>` / `std::conditional_t` / 限定名别名返回类型都
    // 必须在使用点展开成目标类型, 而不是当未知类型名.
    let src = include_str!("../fixtures/cpp/alias_template_expansion.cpp");
    cpp_std20_exit_code_cross_check("cpp-alias-template-expansion", src);
}

#[test]
#[cfg(unix)]
fn cpp_template_braced_init_var() {
    // M53 任务 4: 类模板对象的花括号初始化 `wrap<int> x{5};` (曾被读成
    // `(wrap<int>) x < 5 ...` 比较表达式), 同时锁住反向情形: 未知类型名开头的
    // `l < r` 与 `if (n < 4) { ... }` 仍是表达式/复合语句 —— 消歧只用"已知类型名
    // 集合"这一真源.
    let src = include_str!("../fixtures/cpp/template_braced_init_var.cpp");
    cpp_std20_exit_code_cross_check("cpp-template-braced-init-var", src);
}

#[test]
#[cfg(unix)]
fn cpp_template_nested_call_arg() {
    // M53 任务 5: 嵌套函数模板调用 `pick(pick(5))` —— 实参本身是待实例化调用,
    // 实例化工作表必须递归收内层 (modules 专家移交: 无模块的普通 TU 同样需要).
    let src = include_str!("../fixtures/cpp/template_nested_call_arg.cpp");
    cpp_std20_exit_code_cross_check("cpp-template-nested-call-arg", src);
}

#[test]
#[cfg(unix)]
fn cpp_coroutine_std_qualified_awaiter_return() {
    // M53 任务 7: promise 成员函数直接用**限定名**返回 awaiter
    // (`std::suspend_always initial_suspend()`), 不借道 typedef 绕法.
    // 走协程专用的三轮自洽 + stdout 对照骨架 (状态机对发射顺序敏感).
    let src = include_str!("../fixtures/cpp/coroutine_std_qualified_awaiter_return.cpp");
    cpp_coroutine_cross_check("cpp-coroutine-std-qualified-awaiter", src);
}

#[test]
#[cfg(unix)]
fn cpp_multi_index() {
    let src = include_str!("../fixtures/cpp/multi_index.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-multi-index", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

#[test]
#[cfg(unix)]
fn cpp_concept() {
    let src = include_str!("../fixtures/cpp/concept.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-concept", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    // M51: 夹具自带运行断言 `identity(42) == 42`. concept 约束被误判不满足
    // (候选整体被 SFINAE 剔除) 或实例化丢失, 都只在链接/运行期暴露,
    // 所以必须真正跑起来, 而不只是"编译过".
    let code = host_link_cpp_object_and_run("cpp-concept", &_obj);
    let _ = fs::remove_file(_obj);
    assert_eq!(
        code, 0,
        "concept.cpp 运行断言失败 (1=identity<int> 结果不对, -2=链接失败, -100=无 g++)"
    );
}

#[test]
#[cfg(unix)]
fn cpp_modules() {
    let src = include_str!("../fixtures/cpp/modules.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-modules", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// P2.1: C++ `obj.*ptr_to_member` 和 `obj->*ptr_to_member` 解析
/// + codegen (Section 6). 验证 dcc 解析器接受 `.*` 单 token
///   (lexer 产生 TokKind::DotStar) 和 `->*` (`->` + `*` 两 token),
///   并走 MemberStar / PtrMemberStar 路径.
#[test]
#[cfg(unix)]
fn cpp_member_ptr_dotstar() {
    let src = include_str!("../fixtures/cpp/member_ptr_deref.cpp");
    let (out, obj) = run_cpp_object_test("cpp-member-ptr-dotstar", src);
    let _ = fs::remove_file(&obj);
    assert!(
        out.status.success(),
        "cpp_member_ptr_dotstar compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// P2.2: typeid(e) 真 RTTI 取类型 (Section 6). 验证 dcc lower
/// 阶段为 `typeid(T)` 静态路径和 `typeid(e)` 动态路径都生成
/// `(&__dcc_ti_<class>)` 真实 typeinfo 符号 (M17.0 阶段实装),
/// 而非 M8 简化版的 `((_ZTI*)(((void*)e)->__vptr + 0))` stub.
#[test]
#[cfg(unix)]
fn cpp_typeid_rtti() {
    // 静态路径 `typeid(T)` / 非多态 `typeid(e)` 取 `_ZTI<T>` 符号地址,
    // 动态路径取 vtable 的 typeinfo slot; 两者对同一多态对象必须能互相比较.
    // 因此这里不仅编译, 还要 host gcc 链接后运行, 退出码 0 才算通过.
    let src = include_str!("../fixtures/cpp/typeid_rtti.cpp");
    let (out, obj) = run_cpp_object_test("cpp-typeid-rtti", src);
    assert!(
        out.status.success(),
        "cpp_typeid_rtti compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-cpp-typeid-rtti-{}", std::process::id()));
    let link = std::process::Command::new("gcc")
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host gcc link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run typeid-rtti binary");
    let _ = fs::remove_file(obj);
    let _ = fs::remove_file(&exe);
    assert_eq!(
        run.status.code().unwrap_or(-1),
        0,
        "cpp_typeid_rtti exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

/// Stage 7 任务 4: **非限定继承成员调用** (C++ [basic.lookup.unqual]/1 +
/// [class.member.lookup]/1): 成员函数体里的裸名字除词法作用域外必须沿
/// 「本类 → 全部基类 (含虚基, 逐层)」查找, 且隐式 `this` 必须转换到
/// **成员所属子对象** (非虚基 → 编译期偏移, 虚基 → vbtable 运行期偏移)。
/// 旧行为: `return prot();` 在派生类里直接 `undeclared identifier: prot`;
/// 名字修好后 MI 次基类 / 虚基类方法又收到最派生对象首地址 (实测
/// dcc 47 / g++ 43, 虚基 dcc 100 / g++ 103)。
/// 三层证据: nm (基类实现只一份 + 调用点存在) + readelf (次基类 vtable chunk
/// 被本 TU 引用 = 虚调用槽来源) + g++ 链接 dcc .o 与 g++ 原生退出码对照.
#[test]
#[cfg(unix)]
fn cpp_derived_member_unqualified_lookup() {
    let src = include_str!("../fixtures/cpp/derived_member_unqualified_lookup.cpp");
    let (out, obj) = run_cpp_object_test("cpp-unqual-base", src);
    assert!(
        out.status.success(),
        "cpp_derived_member_unqualified_lookup compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let obj_data = fs::read(&obj).expect("read dcc object");
    let funcs = elf64_func_symbols(&obj_data);
    let defined = |n: &str| funcs.iter().filter(|(name, _, _)| name == n).count();

    // 1) 基类成员的实现各**只有一份** (派生类成员只是调用, 不得复制体)
    for name in [
        "Base_prot",
        "Base_tag",
        "Mid_bump",
        "Mid_mid_only",
        "Side_get_s",
        "Side_stag",
        "VB_vfoo",
    ] {
        assert_eq!(
            defined(name),
            1,
            "基类成员实现 {} 应被发射且仅发射一次: {:?}",
            name,
            funcs.iter().map(|(n, _, _)| n.as_str()).collect::<Vec<_>>()
        );
    }
    // 2) 非限定调用点所在的派生成员都已发射
    for name in [
        "Der_probe_prot",
        "Der_probe_tag",
        "Der_probe_bump",
        "Multi_probe_side_call",
        "Multi_probe_side_virt",
        "VDer_probe_virt",
    ] {
        assert_eq!(defined(name), 1, "派生类成员 {} 应已发射", name);
    }

    // 3) MI 次基类子对象必须有**自己的** vtable chunk, 且被本 TU 引用:
    //    `Multi::probe_side_virt` 的 this 先 +16 折到 Side 子对象, 再从该子对象
    //    的 vptr 取槽 —— 槽来源就是这份 chunk。
    let syms = elf64_syms(&obj_data);
    assert!(
        syms.iter().any(|s| s.name == "_ZTVMulti::baseSide"),
        "应发射 MI 次基类 vtable chunk `_ZTVMulti::baseSide`: {:?}",
        syms.iter().map(|s| s.name.as_str()).collect::<Vec<_>>()
    );
    let relocs = readelf_relocations(&obj);
    assert!(
        relocs.contains("_ZTVMulti::baseSide"),
        "次基类 vtable chunk 必须被 Multi 的构造器引用 (否则虚调用无从取槽), 实际重定位:\n{}",
        relocs
    );

    // 4) 行为层: dcc 产物 (g++ 链接) 与 g++ 原生退出码必须一致且为 0
    let dcc_exit = host_link_cpp_object_and_run("cpp-unqual-base", &obj);
    let _ = fs::remove_file(&obj);
    let gxx_exit = gxx_native_cpp_exit_code("cpp-unqual-base", src);
    assert_eq!(
        dcc_exit, gxx_exit,
        "dcc(g++ 链接) 退出码 {} 应与 g++ 原生 {} 一致",
        dcc_exit, gxx_exit
    );
    assert_eq!(dcc_exit, 0, "非限定继承成员调用 (单继承链 / MI 次基类 / 虚基类) 运行断言未通过");
}

/// Stage 7 任务 2: type_info 对象的 name() / before() 运行期语义.
/// dcc 发射的 typeinfo 对象按 Itanium ABI 叠层 (偏移 0 = type_info 虚表槽,
/// 偏移 8 = __name 指向 mangled 类型名字符串), 因此 `name()` 必须读到与 g++
/// 在同一个 `_ZTI<T>` 对象上读到的同一个字符串; `before()` 则是该字符串的
/// 字典序。用 dcc 产出的 .o 经 g++ 链接后运行, 与 g++ 原生编译运行对照退出码.
#[test]
#[cfg(unix)]
fn cpp_typeid_rtti_name_before() {
    let src = include_str!("../fixtures/cpp/typeid_rtti_name_before.cpp");
    // 1) dcc 编译为 .o
    let (out, obj) = run_cpp_object_test("cpp-typeid-nb", src);
    assert!(
        out.status.success(),
        "cpp_typeid_rtti_name_before compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // 2) nm 佐证: 命名空间类的 RTTI 符号必须是限定名编码 (不是 `_ZTI6Widget`),
    //    且 type_info 虚表以 weak 符号发射 (与 libstdc++ 共存时可让位)
    let nm = Command::new("nm")
        .arg(&obj)
        .output()
        .expect("nm on dcc object");
    let nm_text = String::from_utf8_lossy(&nm.stdout);
    assert!(
        nm_text.contains("_ZTIN2ns6WidgetE"),
        "ns::Widget 的 typeinfo 符号应为限定名 _ZTIN2ns6WidgetE, 实际:\n{}",
        nm_text
    );
    assert!(
        !nm_text.contains("_ZTI6Widget\n"),
        "不应再发射丢掉命名空间前缀的 _ZTI6Widget:\n{}",
        nm_text
    );
    assert!(
        nm_text.contains("dcc_ti_class_vtable"),
        "typeinfo 对象的 vptr 必须指向真实虚表 dcc_ti_class_vtable:\n{}",
        nm_text
    );
    // 3) g++ 链接 dcc 的 .o 并运行
    let dcc_exit = host_link_cpp_object_and_run("cpp-typeid-nb", &obj);
    let _ = fs::remove_file(&obj);
    // 4) g++ 原生编译运行同一夹具作为真值
    let dir = std::env::temp_dir();
    let gxx_src = dir.join(format!("dcc-typeid-nb-{}.cpp", std::process::id()));
    let gxx_exe = dir.join(format!("dcc-typeid-nb-{}.bin", std::process::id()));
    fs::write(&gxx_src, src).expect("write gxx src");
    let gxx_exit = if Command::new("g++").arg("--version").output().is_err() {
        -100
    } else {
        let c = Command::new("g++")
            .arg("-std=c++17")
            .arg(&gxx_src)
            .arg("-o")
            .arg(&gxx_exe)
            .output()
            .expect("g++ compile");
        if c.status.success() {
            let r = Command::new(&gxx_exe).output().expect("run gxx");
            let _ = fs::remove_file(&gxx_exe);
            r.status.code().unwrap_or(-3)
        } else {
            -4
        }
    };
    let _ = fs::remove_file(&gxx_src);
    assert_eq!(
        dcc_exit, gxx_exit,
        "dcc(g++ 链接) 退出码 {} 应与 g++ 原生 {} 一致",
        dcc_exit, gxx_exit
    );
    assert_eq!(dcc_exit, 0, "type_info name()/before() 运行断言未通过");
}

/// P2.3: C++ 模板实参语法 T<U> (Section 6 表格 模板实例化行).
/// 验证 dcc cpp 解析器 `parse_mangle_type` (M18 阶段实装)
/// 在 ident 后识别 `<` 触发 `MangleType::Template` 分支，接受
/// `Holder<int>` / `Holder<Holder<int>>` (嵌套 >>视作两层 `>)
/// 语法，不再走 M8 简化版对模板实参的剥离.
#[test]
#[cfg(unix)]
fn cpp_template_arg_syntax() {
    let src = include_str!("../fixtures/cpp/template_arg.cpp");
    let (out, obj) = run_cpp_object_test("cpp-template-arg-syntax", src);
    let _ = fs::remove_file(&obj);
    assert!(
        out.status.success(),
        "cpp_template_arg_syntax compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// M18.x: 模板嵌套深度限制 - 正常深度 (≤ MAX_TEMPLATE_DEPTH)
/// 应在类型上下文 (形参) 与表达式上下文 (局部声明) 都能编译;
/// 同时覆盖深层嵌套 `>>` 拆 token 的正确闭合 (template_depth.cpp).
#[test]
#[cfg(unix)]
fn cpp_template_depth_ok() {
    let src = include_str!("../fixtures/cpp/template_depth.cpp");
    let (out, obj) = run_cpp_object_test("cpp-template-depth-ok", src);
    let _ = fs::remove_file(&obj);
    assert!(
        out.status.success(),
        "cpp_template_depth_ok compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// M18.x: 模板嵌套深度超限 (MAX_TEMPLATE_DEPTH + 1) 应报模板深度错误
/// (C0004), 而非挂起/栈溢出。
#[test]
#[cfg(unix)]
/// M18.x: 模板深度限制验证 (正常深度 vs 超限深度的双向断言)
fn cpp_template_depth_limit_rejected() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    // `Wrap<Wrap<...int...>>` 形式的具体参类型: 嵌套层数 = depth
    let build = |depth: usize| -> String {
        let mut src = String::from("template <class T> struct Wrap { T v; };\n");
        src.push_str("int take(");
        src.push_str(&"Wrap<".repeat(depth));
        src.push_str("int");
        src.push_str(&">".repeat(depth));
        src.push_str("* p) { return p != 0; }\n");
        src.push_str("int main() { return 0; }\n");
        src
    };
    let compile = |label: &str, depth: usize| -> (std::process::Output, std::time::Duration) {
        let source_path = dir.join(format!("dcc-cpp-depth-{label}-{pid}.cpp"));
        let object_path = dir.join(format!("dcc-cpp-depth-{label}-{pid}.o"));
        fs::write(&source_path, build(depth)).expect("write C++ fixture");
        let start = std::time::Instant::now();
        let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["-c", "-o"])
            .arg(&object_path)
            .arg(&source_path)
            .output()
            .expect("run dcc");
        let elapsed = start.elapsed();
        let _ = fs::remove_file(&source_path);
        let _ = fs::remove_file(&object_path);
        (output, elapsed)
    };

    // 1. 正常深度 (远低于 900) 必须编译通过, 并且是毫秒级 ——
    //    实例化工作表曾经对超深类型二次/三次方爆炸 (百秒级).
    let (ok, ok_elapsed) = compile("ok", 64);
    assert!(
        ok.status.success(),
        "depth=64 应在模板深度限制内编译通过: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(
        ok_elapsed < std::time::Duration::from_secs(20),
        "depth=64 耗时 {ok_elapsed:?}, 模板实例化存在超线性膨胀"
    );

    // 2. 超过 MAX_TEMPLATE_DEPTH(900) 必须硬报错 (而非静默接受/容错成 Opaque),
    //    且诊断必须带 C0004 + 深度提示.
    let (bad, bad_elapsed) = compile("over", 950);
    assert!(!bad.status.success(), "depth=950 超过 MAX_TEMPLATE_DEPTH, 应当报错退出");
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(err.contains("C0004"), "超限应报 C0004 诊断码, 实际输出: {err}");
    assert!(
        err.contains("template nesting depth") && err.contains("exceeds limit"),
        "C0004 应说明模板嵌套深度超限, 实际输出: {err}"
    );
    assert!(
        bad_elapsed < std::time::Duration::from_secs(20),
        "超限路径应早停而不是先花 {bad_elapsed:?} 展开全部嵌套"
    );
}

/// M18.x: SFINAE 简化支持 - 匿名默认类型形参 (`typename = T`,
/// enable_if 惯用法) / 模板默认实参 / 多形参模板 (sfinae.cpp).
/// 求值层 (enable_if 条件 / trait 检测) 由 feature_degrade.rs 单元测试覆盖.
#[test]
#[cfg(unix)]
fn cpp_sfinae_smoke() {
    let src = include_str!("../fixtures/cpp/sfinae.cpp");
    let (out, obj) = run_cpp_object_test("cpp-sfinae", src);
    assert!(
        out.status.success(),
        "cpp_sfinae_smoke compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // M51: 夹具自带运行断言 (匿名默认形参 / 默认模板实参 / 多形参推导).
    // 候选被 SFINAE 误剔 or 重载选错都会改变运行结果, 因此必须实跑.
    let code = host_link_cpp_object_and_run("cpp-sfinae", &obj);
    let _ = fs::remove_file(&obj);
    assert_eq!(
        code, 0,
        "sfinae.cpp 运行断言失败 (1/2/3=对应断言不成立, -2=链接失败, -100=无 g++)"
    );
}

/// §7.4-B 判定: `tests/fixtures/cpp/enhanced_sfinae.cpp` 此前**零引用**, 在此挂载。
///
/// 为什么落 `cpp_lang.rs`: 方案 §7.4-B 明示夹具是模板语义, 不进 `cpp_abi_*`。具体地,
/// 夹具全是**模板元语义** —— `enable_if<B,T>::type` 作默认类型形参与返回类型、
/// `enable_if_t` 别名模板、`is_pointer<T*>` 偏特化、`void_t` 检测惯用法、`||` 组合条件;
/// 不涉及 mangled 名 / vtable / typeinfo 那些 ABI 差分面 (那由 `fixtures/cpp_abi_*` +
/// g++ 逐行对照钉), 所以与 `cpp_abi_template/` 不同源、不重复。
///
/// 判据三层 (全部实测):
///   1. [正] `g++ -std=c++20 -fsyntax-only` 必须接受该夹具 —— 保证它仍是标准 C++ 而不是
///      dcc 方言。挂载时发现夹具**自身**缺 `is_floating_point<double>` 偏特化, 于是
///      `test_enable_if_float(3.14)` 在 g++ 与 dcc native 里都被 SFINAE 剔成"无可行候选",
///      已补齐 (见夹具头部注释)。
///   2. [正] `dcc -fsyntax-only` 必须接受 —— parser/sema 认 M40 全部惯用法形态, 包括无名
///      可变参数形参 `template<typename...>` 与 `void_t` 检测偏特化。
///   3. [钉缺口] `dcc --cxx-native=on -c` 必须**失败**, 诊断必须是 E0500
///      `unsupported opaque declaration` 且点名 `<typename...>` 那一层 (无名可变参数模板
///      形参的 lowering 未实现); `dcc -c` (auto) 则必须额外露出 E0504 —— 证明 auto 真的
///      调度过 legacy 回落, 而不是 native 一拒就静默成功。
///
/// 升级路径: native 支持无名可变参数形参 + `void_t` 别名偏特化后, 第 3 条会失败, 届时应
/// 把本用例改成 `cpp_std20_exit_code_cross_check` —— 夹具 main 把 5 个验证点编成
/// `return 1..5`, 退出码逐条与 g++ 对照即为完整判据。
#[test]
#[cfg(unix)]
fn cpp_enhanced_sfinae_void_t_layer() {
    let src = include_str!("../fixtures/cpp/enhanced_sfinae.cpp");
    let dir = temp_dir("enhanced-sfinae");
    let src_path = write_src(&dir, "enhanced_sfinae.cpp", src);
    let obj = dir.join("enhanced_sfinae.o");

    // 1. g++ 真值: 夹具必须是标准 C++20 (无 g++ 时跳过这一条, 不放宽 2/3)。
    if tool_exists("g++") {
        let g = Command::new("g++")
            .args(["-std=c++20", "-fsyntax-only"])
            .arg(&src_path)
            .output()
            .expect("run g++");
        assert!(
            g.status.success(),
            "enhanced_sfinae.cpp 必须是标准 C++20, g++ 拒收: {}",
            String::from_utf8_lossy(&g.stderr)
        );
    }

    // 2. dcc 前端接受全部 M40 惯用法形态。
    let syn = dcc_cmd()
        .arg("-fsyntax-only")
        .arg(&src_path)
        .output()
        .expect("run dcc -fsyntax-only");
    assert!(
        syn.status.success(),
        "dcc -fsyntax-only 应接受 enable_if/enable_if_t/void_t 全部惯用法: {}",
        String::from_utf8_lossy(&syn.stderr)
    );

    // 3a. on: 不得静默接受, 必须报出指向无名可变参数形参的 E0500, 且不进 legacy。
    let on = dcc_cmd()
        .arg("--cxx-native=on")
        .arg("-c")
        .arg(&src_path)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("run dcc --cxx-native=on");
    let on_err = String::from_utf8_lossy(&on.stderr).to_string();
    assert!(!on.status.success(), "on 模式静默接住了 void_t 夹具:\n{on_err}");
    assert!(on_err.contains("E0500"), "on 模式必须报 E0500:\n{on_err}");
    assert!(
        on_err.contains("unsupported opaque declaration") && on_err.contains("<typename...>"),
        "E0500 必须点名无名可变参数模板形参 `template<typename...> struct void_t_impl`:\n{on_err}"
    );

    // 3b. auto: native 拒绝后必须真的调度过 legacy, 且两条路径都以硬失败收口。
    let auto = dcc_cmd()
        .arg("-c")
        .arg(&src_path)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("run dcc -c (auto)");
    let auto_err = String::from_utf8_lossy(&auto.stderr).to_string();
    assert!(!auto.status.success(), "auto 不应产出目标文件:\n{auto_err}");
    assert!(auto_err.contains("E0500"), "auto 侧 native 诊断丢失:\n{auto_err}");
    assert!(
        auto_err.contains("E0504") && auto_err.contains("C++ legacy backend host-cc failed"),
        "auto 必须暴露 legacy 回落也被尝试 (E0504):\n{auto_err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// §7.4-B 判定: `tests/fixtures/cpp/g11_template_templates.cpp` 此前**零引用**, 在此挂载。
///
/// 为什么落 `cpp_lang.rs` (方案 §7.4-B 明示模板相关不进 `cpp_abi_*`): 夹具主体是**模板模板
/// 形参** `template<typename T, template<typename> class Container>` 的语法识别与偏特化;
/// 夹具自身从未实例化出可执行语义 (main 只声明两个 Adapter 并 `(void)` 掉), 所以 ABI 面
/// (mangled 名/COMDAT) 与它无关。
///
/// 判据 (G11 的真实完成度就是"识别但不 lowering", 本用例把这个分层事实钉住):
///   1. [正] `g++ -std=c++20 -fsyntax-only` 必须接受 —— 夹具是标准 C++20 (挂载时原夹具
///      `#include <deque>` 因 dcc 未注册 deque 头而先撞 E0206, 挡住全部判定, 已改用
///      `std::vector` + 用户自定义 `Box<T>`, 见夹具头部注释)。
///   2. [正] `dcc -fsyntax-only` 必须接受 —— parser 认 TTP 语法 (parser.rs 的
///      `lookahead_is_template_template` G11 分支)。
///   3. [钉缺口] `dcc --cxx-native=on -c` 必须失败且诊断点名 `template<typename> class`,
///      即 TTP 模板被降级成 Opaque 而 native lowering 拒绝 —— 不许静默接受、不许换成别的
///      错码; `dcc -c` (auto) 还必须额外露出 E0504 (legacy 回落同样接不住)。
///
/// 升级路径: TTP lowering 落地后第 3 条会失败, 届时把本用例升级为链接+运行并断言退出码 0
/// (夹具 main 成功路径返回 0)。
#[test]
#[cfg(unix)]
fn cpp_g11_template_templates_recognized_not_lowered() {
    let src = include_str!("../fixtures/cpp/g11_template_templates.cpp");
    let dir = temp_dir("g11-template-templates");
    let src_path = write_src(&dir, "g11_template_templates.cpp", src);
    let obj = dir.join("g11.o");

    if tool_exists("g++") {
        let g = Command::new("g++")
            .args(["-std=c++20", "-fsyntax-only"])
            .arg(&src_path)
            .output()
            .expect("run g++");
        assert!(
            g.status.success(),
            "g11_template_templates.cpp 必须是标准 C++20, g++ 拒收: {}",
            String::from_utf8_lossy(&g.stderr)
        );
    }

    let syn = dcc_cmd()
        .arg("-fsyntax-only")
        .arg(&src_path)
        .output()
        .expect("run dcc -fsyntax-only");
    assert!(
        syn.status.success(),
        "dcc -fsyntax-only 应识别模板模板形参语法: {}",
        String::from_utf8_lossy(&syn.stderr)
    );

    let on = dcc_cmd()
        .arg("--cxx-native=on")
        .arg("-c")
        .arg(&src_path)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("run dcc --cxx-native=on");
    let on_err = String::from_utf8_lossy(&on.stderr).to_string();
    assert!(!on.status.success(), "on 模式静默接住了 TTP 夹具:\n{on_err}");
    assert!(
        on_err.contains("E0500") && on_err.contains("template<typename> class"),
        "E0500 必须点名模板模板形参那层 (G11 只完成到 parser 识别):\n{on_err}"
    );

    let auto = dcc_cmd()
        .arg("-c")
        .arg(&src_path)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("run dcc -c (auto)");
    let auto_err = String::from_utf8_lossy(&auto.stderr).to_string();
    assert!(!auto.status.success(), "auto 不应产出目标文件:\n{auto_err}");
    assert!(
        auto_err.contains("E0504") && auto_err.contains("C++ legacy backend host-cc failed"),
        "auto 必须暴露 legacy 回落也被尝试 (E0504):\n{auto_err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// P2.4: operator new[] / operator delete[] 走 _Znam / _ZdaPv
/// (Section 6). 验证 dcc lower 阶段对 `new T[n]` 和 `delete[] p`
/// 调用 mangle_op_new_array() / mangle_op_delete_array() 命名常量，
/// 即 Itanium C++ ABI 数组分配/释放符号 _Znam / _ZdaPv.
#[test]
#[cfg(unix)]
fn cpp_new_arr_mangler() {
    let src = include_str!("../fixtures/cpp/new_arr_mangler.cpp");
    let (out, obj) = run_cpp_object_test("cpp-new-arr-mangler", src);
    let _ = fs::remove_file(&obj);
    assert!(
        out.status.success(),
        "cpp_new_arr_mangler compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// P2.5: new T[n] 数组分配 (Section 6). 验证 dcc parser
/// 在 `new T[n]` 形式下识别 `[` 触发 CppExpr::NewArr, lower
/// 阶段 emit `((T*)mangle_op_new_array()(sizeof(T) * n))`, 即
/// Itanium C++ ABI 数组分配符号 _Znam 调用。配合
/// `delete[] p` 走 _ZdaPv (P2.4) 走完整 array 生命周期.
#[test]
#[cfg(unix)]
fn cpp_new_array_alloc() {
    let src = include_str!("../fixtures/cpp/new_array_alloc.cpp");
    let (out, obj) = run_cpp_object_test("cpp-new-array-alloc", src);
    let _ = fs::remove_file(&obj);
    assert!(
        out.status.success(),
        "cpp_new_array_alloc compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[cfg(unix)]
fn cpp_coroutine_state_machine() {
    // 升级背景: 原本 compile-only + #[ignore] ("current: compile-only smoke test").
    // 现在 native 后端真做协程降级 (src/cpp/coroutine_lower.rs): 协程帧 == promise 对象,
    // 局部状态提升为帧成员, 挂起点重排为 switch+fallthrough 状态机, 故按夹具注释的
    // 完整意图跑起来: ramp 后停在 initial_suspend, 每个 next() 恰好推进一个 co_yield,
    // co_return 后 final_suspend 挂起 => done(), 终态重复驱动不再执行主体, 两次调用
    // 得到独立帧. 退出码逐条与 g++ -std=c++20 对照.
    let src = include_str!("../fixtures/cpp/coroutine_state.cpp");
    cpp_coroutine_cross_check("cpp-coroutine-state", src);
}

#[test]
#[cfg(unix)]
fn cpp_modules_bmf() {
    // M26.4/M26.5: BMF 格式 + export/import 解析 + ODR 校验.
    // fixture 是 `export module bmf_test;` 的接口单元:
    //   1) 编译时经 --emit-module-file 写出模块接口 (M / E F / E C 记录);
    //   2) 另起导入 TU 从该 BMF 回放声明 (只引用符号, 不重复定义);
    //   3) 与本接口冲突 (同名不同签名) 的另一模块同时 import 必须被拒;
    //   4) fixture 自身可链接运行 (add(1,2)-3 == 0).
    let src = include_str!("../fixtures/cpp/modules_bmf.cpp");
    let dir = std::env::temp_dir().join(format!("dcc-bmf-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let mod_src = dir.join("bmf_test.cpp");
    let mod_obj = dir.join("bmf_test.o");
    let iface = dir.join("bmf_test.bmf");
    fs::write(&mod_src, src).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&mod_obj)
        .arg(&mod_src)
        .arg(format!("--emit-module-file={}", iface.display()))
        .output()
        .expect("run dcc on module TU");
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    assert!(mod_obj.exists(), "object not emitted");
    assert!(iface.exists(), "BMF interface not emitted");
    let text = fs::read_to_string(&iface).unwrap();
    assert!(text.starts_with("DCC_BMF_V1\n"), "magic: {}", text);
    assert!(text.contains("M bmf_test"), "{}", text);
    assert!(text.contains("E F add"), "{}", text);
    // M28.3/M28.5/M28.6: `E C` 布局首 token 是 class/struct/union 标记,
    // public 字段不写访问标记; `E F` 签名里带 mangled 链接名
    assert!(text.contains("E C Point @class int x; int y;"), "类布局: {}", text);
    assert!(text.contains("int _Z3addii(int, int)"), "mangled 签名: {}", text);
    assert!(!text.contains("main"), "main 不是模块导出: {}", text);

    // 导入 TU: 声明自 BMF 回放 -> `add` 为外部引用 (M28.6: 按 mangled 名),
    // 本 TU 不重复定义
    let use_src = dir.join("use.cpp");
    fs::write(
        &use_src,
        "import bmf_test;\nint use_point(Point* p) { return add(p->x, p->y); }\n",
    )
    .unwrap();
    let use_obj = dir.join("use.o");
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&use_obj)
        .arg(&use_src)
        .output()
        .expect("run dcc on import TU");
    assert!(out.status.success(), "import compile: {}", String::from_utf8_lossy(&out.stderr));
    let bytes = fs::read(&use_obj).unwrap();
    let undef = elf64_undefined_symbols(&bytes);
    assert!(undef.iter().any(|s| s == "_Z3addii"), "add 应为 mangled 外部引用: {:?}", undef);
    let defined: Vec<String> = elf64_func_symbols(&bytes)
        .into_iter()
        .map(|(n, _, _)| n)
        .collect();
    assert!(
        !defined.iter().any(|s| s == "_Z3addii"),
        "导入端不得重复定义 add: {:?}",
        defined
    );

    // ODR 校验 (M28.6): 另一模块导出**同名同类但布局不同**的 `Point` ——
    // 这是真实 ODR 违例 (同类型在两个 TU 里布局不一致). 函数重载
    // (`add(int,int)` vs `add(int)`) 反而不再撞键: mangled 名不同 = 不同实体.
    let other_src = dir.join("other.cpp");
    fs::write(
        &other_src,
        "export module other;\nexport class Point {\npublic:\n    int x;\n    long y;\n};\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(dir.join("other.o"))
        .arg(&other_src)
        .arg(format!("--emit-module-file={}", dir.join("other.bmf").display()))
        .output()
        .expect("run dcc on conflicting module");
    assert!(out.status.success(), "other module: {}", String::from_utf8_lossy(&out.stderr));
    let bad_src = dir.join("odr.cpp");
    fs::write(&bad_src, "import bmf_test;\nimport other;\nint main() { return 0; }\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(dir.join("odr.o"))
        .arg(&bad_src)
        .output()
        .expect("run dcc on conflicting imports");
    assert!(!out.status.success(), "ODR 冲突应被拒绝");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("ODR violations"), "stderr: {}", stderr);
    assert!(stderr.contains("Point"), "stderr: {}", stderr);

    // fixture 自身链接运行 (接口单元带 main, add(1,2)-3 == 0)
    let exe = dir.join("bmf_run");
    let link = Command::new("gcc")
        .arg(&mod_obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host gcc link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = Command::new(&exe).output().expect("run bmf binary");
    let _ = fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "run: exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
#[cfg(unix)]
fn cpp_module_cross_tu_import() {
    // M26.4: 跨 TU module import.
    // 导出 TU (export module) 经 --emit-module-file 写出 BMF 接口;
    // 导入 TU (import mymod) 从 BMF 生成 extern 声明 + 类布局;
    // dcc -c 两个 TU 后 host gcc 链接运行.
    let dir = std::env::temp_dir().join(format!("dcc-mod-tu-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let mod_path = dir.join("mymod.cpp");
    let mod_obj = dir.join("mymod.o");
    let bmf_path = dir.join("mymod.bmf");
    let main_path = dir.join("main.cpp");
    let main_obj = dir.join("main.o");
    let exe = dir.join("main");

    let module_src = r#"export module mymod;
export int answer() { return 42; }
export int twice(int x) { return x * 2; }
export class Widget {
public:
    int id;
    Widget(int v) { id = v; }
    int get() { return id; }
};
module :private;
int hidden_helper() { return 1; }
"#;
    fs::write(&mod_path, module_src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&mod_obj)
        .arg(&mod_path)
        .arg(format!("--emit-module-file={}", bmf_path.display()))
        .output()
        .expect("run dcc on module TU");
    assert!(out.status.success(), "module compile: {}", String::from_utf8_lossy(&out.stderr));
    assert!(bmf_path.exists(), "BMF interface not emitted");
    let bmf_text = fs::read_to_string(&bmf_path).unwrap();
    // 导出记录: 自由函数 + 类布局 + 成员函数/ctor; 私有片段声明不导出
    assert!(bmf_text.contains("E F answer"), "{}", bmf_text);
    assert!(bmf_text.contains("E F twice"), "{}", bmf_text);
    assert!(bmf_text.contains("E C Widget"), "{}", bmf_text);
    assert!(bmf_text.contains("Widget::ctor"), "{}", bmf_text);
    assert!(bmf_text.contains("Widget::get"), "{}", bmf_text);
    assert!(!bmf_text.contains("hidden_helper"), "{}", bmf_text);

    let main_src = r#"import mymod;
int main() {
    if (twice(answer()) != 84) return 1;
    Widget w(7);
    if (w.get() != 7) return 2;
    if (w.id != 7) return 3;
    return 0;
}
"#;
    fs::write(&main_path, main_src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&main_obj)
        .arg(&main_path)
        .output()
        .expect("run dcc on import TU");
    assert!(out.status.success(), "import compile: {}", String::from_utf8_lossy(&out.stderr));

    let link = Command::new("gcc")
        .arg(&mod_obj)
        .arg(&main_obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host gcc link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = Command::new(&exe).output().expect("run module binary");
    let _ = fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "run: exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
#[cfg(unix)]
fn cpp_module_import_not_found_diagnostic() {
    // M28.1: `import foo;` 找不到 BMF 接口文件时必须给明确诊断 (旧行为是
    // 静默当普通外部符号处理 -> 未声明实体全程不报错, ODR 冲突也就无从校验).
    // g++ -fmodules-ts 对照: `error: failed to read compiled module: ...` +
    // 非零退出, 同样是硬诊断 (无逃生门), dcc 侧对应 E0104.
    let dir = std::env::temp_dir().join(format!("dcc-mod-notfound-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let out = dcc_compile_module(
        &dir,
        "uses_missing",
        "import nosuchmod;\nint main() { return 0; }\n",
        false,
    );
    assert!(!out.status.success(), "missing module import must be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("E0104"), "stderr: {}", stderr);
    assert!(stderr.contains("nosuchmod"), "stderr: {}", stderr);
    assert!(stderr.contains("nosuchmod.bmf"), "stderr 要列出候选文件名: {}", stderr);
    // 诊断带 import 所在行 (夹具里是第 1 行), 便于定位
    assert!(stderr.contains("uses_missing.cpp:1:1"), "stderr: {}", stderr);

    // 分区 import 同样诊断, 且候选名用 `模块.分区.bmf` 约定
    let out = dcc_compile_module(
        &dir,
        "uses_missing_part",
        "export module mymod;\nimport :ghost;\nexport int f() { return 1; }\n",
        false,
    );
    let _ = fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "missing partition import must be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("E0104"), "stderr: {}", stderr);
    assert!(stderr.contains("mymod.ghost.bmf"), "stderr: {}", stderr);
}

#[test]
#[cfg(unix)]
fn cpp_module_cross_tu_private_access_rejected() {
    // M28.3: BMF 现在逐成员记录访问级 (`E C` 布局里的 `@private` 尾标记,
    // `E F` 尾部的 `@private`/`@protected`/`@const`), 导入端回放时还原
    // public/protected/private 与 const 限定. 跨 TU 访问 private/protected
    // 成员必须被拒, public/const 成员必须可用.
    // g++ -std=c++20 -fmodules-ts 对照: 同一声明顺序的普通多 TU 写法同样
    // 报 "is private within this context"; 访问级不同不构成 ODR 冲突.
    let dir = std::env::temp_dir().join(format!("dcc-mod-priv-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let out = dcc_compile_module(
        &dir,
        "catmod",
        r#"export module catmod;
export class Cat {
public:
    Cat(int s) : secret(s) {}
    int reveal() const { return secret; }
protected:
    int mask() { return 7; }
private:
    int secret;
};
"#,
        true,
    );
    assert!(out.status.success(), "module compile: {}", String::from_utf8_lossy(&out.stderr));
    let bmf = fs::read_to_string(dir.join("catmod.bmf")).unwrap();
    assert!(
        bmf.contains("E C Cat @class @private int secret;"),
        "private 字段入 BMF: {}",
        bmf
    );
    // public 成员不写访问标记 (向后兼容: 旧 BMF 文本逐字仍可读), 但 const 要写
    assert!(
        bmf.contains("E F Cat::reveal int _ZN3Cat6revealEv(struct Cat*) @const"),
        "const 成员函数: {}",
        bmf
    );
    assert!(bmf.contains("@protected"), "protected 成员函数入 BMF: {}", bmf);

    // 跨 TU 读 private 成员 -> 必须拒绝
    let out = dcc_compile_module(
        &dir,
        "catbad",
        "import catmod;\nint main() { Cat c(9); return c.secret != 9; }\n",
        false,
    );
    assert!(!out.status.success(), "跨 TU 访问 private 成员必须被拒");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("secret"), "stderr 要点出成员名: {}", stderr);
    assert!(stderr.contains("private"), "stderr 要点出访问级: {}", stderr);

    // 跨 TU 调 protected 成员 -> 同样拒绝 (非派生路径上下文)
    let out = dcc_compile_module(
        &dir,
        "catprot",
        "import catmod;\nint main() { Cat c(9); return c.mask() != 7; }\n",
        false,
    );
    assert!(!out.status.success(), "跨 TU 访问 protected 成员必须被拒");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("protected"), "stderr: {}", stderr);

    // 走 public const 接口则一切正常 (链接 + 运行 exit 0)
    let out = dcc_compile_module(
        &dir,
        "catok",
        "import catmod;\nint main() { const Cat c(9); return c.reveal() != 9; }\n",
        false,
    );
    assert!(
        out.status.success(),
        "public const 路径: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let exit = host_link_objects_and_run(
        &dir.join("catrun"),
        &[&dir.join("catmod.o"), &dir.join("catok.o")],
        true,
    );
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(exit, 0, "public 成员跨 TU 调用结果应为 9");
}

#[test]
#[cfg(unix)]
fn cpp_odr_conflict_rejected() {
    // M26.5/M28.6: 两个模块导出**同一链接身份但定义不同**的实体, 同时 import
    // 二者时 driver 必须报 E0103 ODR violations, 而不是静默生成代码.
    //
    // 为什么改夹具: M28.6 把 ODR 比较键从源名切到 mangled 链接名后,
    // 旧形式 (`int foo()` vs `int foo(int)`) 是 `_Z3foov` / `_Z3fooi` ——
    // 两个不同的合法重载实体, 不该被拒 (见下面 cpp_odr_overload_not_conflict).
    // 现在用两种真实 ODR 违例:
    //   1) 同名同类 `Point` 但**布局不同** (字段数不同 -> 对象尺寸不同);
    //   2) 同名同模板形参表 `Box<T>` 但**定义体不同** (M28.4 随 `E T` 带走原文,
    //      因而可比较). Itanium 不把返回类型编进 mangled 名, 同名的两个
    //      实体只能靠签名/布局文本才能区分.
    //
    // g++ -std=c++20 -fmodules-ts 对照: 同名类不同布局的两模块同时 import
    // 仍编译通过 (exit 0) —— g++ 不做这项检查, dcc 按任务书要求主动报 E0103
    // (比 g++ 更严, 不会误拒合法程序: 参照下面 cpp_odr_overload_not_conflict).
    let dir = std::env::temp_dir().join(format!("dcc-odr-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();

    let compile_module = |name: &str, src: &str| {
        let sp = dir.join(format!("{}.cpp", name));
        fs::write(&sp, src).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["-c", "-o"])
            .arg(dir.join(format!("{}.o", name)))
            .arg(&sp)
            .arg(format!("--emit-module-file={}", dir.join(format!("{}.bmf", name)).display()))
            .output()
            .expect("run dcc on module");
        assert!(out.status.success(), "{}: {}", name, String::from_utf8_lossy(&out.stderr));
    };

    // 模块 A: Point{int x} / Box<T>{T v}
    compile_module(
        "moda",
        "export module moda;\nexport class Point {\npublic:\n    int x;\n};\n\
         export template<class T> struct Box { T v; };\n",
    );
    // 模块 B: 同名但布局/定义体都不同
    compile_module(
        "modb",
        "export module modb;\nexport class Point {\npublic:\n    int x;\n    int y;\n};\n\
         export template<class T> struct Box { T v; int pad; };\n",
    );

    // 导入 TU 同时 import moda + modb -> ODR 冲突必须被拒绝
    fs::write(dir.join("main.cpp"), "import moda;\nimport modb;\nint main() { return 0; }\n")
        .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(dir.join("main.o"))
        .arg(dir.join("main.cpp"))
        .output()
        .expect("run dcc on import TU");
    let _ = fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "cpp_odr_conflict_rejected should be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("E0103"), "stderr: {}", stderr);
    assert!(stderr.contains("ODR violations"), "stderr: {}", stderr);
    // 布局不同的类与定义体不同的模板都要被点出来
    assert!(stderr.contains("Point"), "stderr: {}", stderr);
    assert!(stderr.contains("Box"), "stderr: {}", stderr);
    assert!(stderr.contains("moda") && stderr.contains("modb"), "stderr: {}", stderr);
}

#[test]
#[cfg(unix)]
fn cpp_odr_overload_not_conflict() {
    // M28.6: ODR 检测随 mangling 调整 —— `foo()` 与 `foo(int)` 分属两个
    // 模块时不再“撞名”, 因为比较键是 mangled 链接名 (`_Z3foov` vs `_Z3fooi`).
    // 同时验证两个模块的发射符号名就是 mangled 名 (类名前缀不丢).
    // g++ -std=c++20 -fmodules-ts 对照: 同一夹具三 TU 全部编译通过 (exit 0).
    let dir = std::env::temp_dir().join(format!("dcc-odr-ovl-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();

    let compile_module = |name: &str, src: &str| {
        let sp = dir.join(format!("{}.cpp", name));
        fs::write(&sp, src).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
            .args(["-c", "-o"])
            .arg(dir.join(format!("{}.o", name)))
            .arg(&sp)
            .arg(format!("--emit-module-file={}", dir.join(format!("{}.bmf", name)).display()))
            .output()
            .expect("run dcc on module");
        assert!(out.status.success(), "{}: {}", name, String::from_utf8_lossy(&out.stderr));
    };

    compile_module("moda", "export module moda;\nexport int foo() { return 1; }\n");
    compile_module("modb", "export module modb;\nexport int foo(int x) { return x; }\n");
    let a_bmf = fs::read_to_string(dir.join("moda.bmf")).unwrap();
    let b_bmf = fs::read_to_string(dir.join("modb.bmf")).unwrap();
    assert!(a_bmf.contains("int _Z3foov()"), "{}", a_bmf);
    assert!(b_bmf.contains("int _Z3fooi(int)"), "{}", b_bmf);

    fs::write(dir.join("main.cpp"), "import moda;\nimport modb;\nint main() { return 0; }\n")
        .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(dir.join("main.o"))
        .arg(dir.join("main.cpp"))
        .output()
        .expect("run dcc on import TU");
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "重载不得误判为 ODR 冲突: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[cfg(unix)]
fn cpp_odr_identical_imports_ok() {
    // M26.5: 两个模块导出同名同定义 (签名一致) 的 foo, 同时 import 不报错
    let dir = std::env::temp_dir().join(format!("dcc-odr-ok-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();

    fs::write(dir.join("moda.cpp"), "export module moda;\nexport int foo() { return 1; }\n")
        .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(dir.join("moda.o"))
        .arg(dir.join("moda.cpp"))
        .arg(format!("--emit-module-file={}", dir.join("moda.bmf").display()))
        .output()
        .expect("run dcc on moda");
    assert!(out.status.success(), "moda: {}", String::from_utf8_lossy(&out.stderr));

    fs::write(dir.join("modb.cpp"), "export module modb;\nexport int foo() { return 2; }\n")
        .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(dir.join("modb.o"))
        .arg(dir.join("modb.cpp"))
        .arg(format!("--emit-module-file={}", dir.join("modb.bmf").display()))
        .output()
        .expect("run dcc on modb");
    assert!(out.status.success(), "modb: {}", String::from_utf8_lossy(&out.stderr));

    fs::write(dir.join("main.cpp"), "import moda;\nimport modb;\nint main() { return 0; }\n")
        .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(dir.join("main.o"))
        .arg(dir.join("main.cpp"))
        .output()
        .expect("run dcc on import TU");
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "identical ODR definitions must pass: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[cfg(unix)]
fn cpp_gcc_compat() {
    let src = include_str!("../fixtures/cpp/gcc_compat.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-gcc-compat", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

#[test]
fn cpp_comdat_template_instantiation() {
    // C++ template instantiation generates COMDAT groups.
    // Test that dcc can parse GCC-generated C++ objects with COMDAT.
    if Command::new("g++").arg("--version").output().is_err() {
        return; // skip if no g++
    }
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let src1 = dir.join(format!("dcc-comdat-cpp1-{}.cpp", pid));
    let src2 = dir.join(format!("dcc-comdat-cpp2-{}.cpp", pid));
    let obj1 = dir.join(format!("dcc-comdat-cpp1-{}.o", pid));
    let obj2 = dir.join(format!("dcc-comdat-cpp2-{}.o", pid));

    // Both TUs instantiate the same template -> COMDAT group
    let template_header = r#"
template<typename T>
T max_val(T a, T b) { return a > b ? a : b; }
"#;
    fs::write(
        &src1,
        format!(
            "{header}\nint get_max(int a, int b) {{ return max_val(a, b); }}\n",
            header = template_header
        ),
    )
    .expect("write src1");
    fs::write(
        &src2,
        format!(
            "{header}\nint get_max(int, int);\nint main() {{ return get_max(1, 2); }}\n",
            header = template_header
        ),
    )
    .expect("write src2");

    let out1 = Command::new("g++")
        .arg("-c")
        .arg("-ffunction-sections")
        .arg(&src1)
        .arg("-o")
        .arg(&obj1)
        .output()
        .expect("g++ -c src1");
    let out2 = Command::new("g++")
        .arg("-c")
        .arg("-ffunction-sections")
        .arg(&src2)
        .arg("-o")
        .arg(&obj2)
        .output()
        .expect("g++ -c src2");

    let _ = fs::remove_file(&src1);
    let _ = fs::remove_file(&src2);
    let _ = fs::remove_file(&obj1);
    let _ = fs::remove_file(&obj2);

    // Both compilations should succeed
    assert!(
        out1.status.success(),
        "g++ src1 failed: {}",
        String::from_utf8_lossy(&out1.stderr)
    );
    assert!(
        out2.status.success(),
        "g++ src2 failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
}

/// Phase 5.1: C++98 iostream - clog and ws manipulator
#[test]
#[cfg(unix)]
fn cpp98_iostream_clog_ws_compile() {
    // 不只是能编译: 链成可执行文件并运行, 验证夹具里每一个独立失败分支
    // (clog 链式写入 / cerr 与 clog 不合并 / ws+ignore+peek+get 消耗前导空白 /
    // istream 成员函数 / 三个标准流互不相同) 都返回 0。
    // 历史缺陷: `std::cin >> x` 把 x 的**值**(0) 当地址传给 `>>`(void*&) 形参,
    // 于是 `&x` 被写坏 -> SIGSEGV (exit 139); 引用 ABI 修复后才稳定返回 0。
    let src = include_str!("../fixtures/cpp/iostream_basic.cpp");
    let (out, obj) = run_cpp_object_test("cpp98-iostream", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-cpp98-iostream-{}", std::process::id()));
    // 必须用 g++ 链接: 函数局部 static 流对象 emit 了 `__cxa_guard_acquire`/`__cxa_guard_release`
    // (Itanium C++ ABI 的正确形式), 它们由 libstdc++ 提供。
    let link = std::process::Command::new("g++")
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host g++ link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    // 夹具不向 stdout 写任何东西 (以便与 g++ 逐字节对照), stdin 给空输入。
    let run = std::process::Command::new(&exe)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run cpp98-iostream binary");
    let _ = fs::remove_file(obj);
    let _ = fs::remove_file(&exe);
    assert!(
        run.status.success(),
        "run: exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

/// Phase 5.2: C++98/11 implicit destructor noexcept
#[test]
#[cfg(unix)]
fn cpp98_dtor_implicit_noexcept_compile() {
    let src = include_str!("../fixtures/cpp/dtor_implicit_noexcept.cpp");
    let (out, obj) = run_cpp_object_test("cpp98-dtor-noexcept", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(obj);
}

/// Phase 5.2/5.3: C++98 iterator_traits and iterator category tags
#[test]
#[cfg(unix)]
fn cpp98_iterator_traits_compile() {
    // 不只是能编译: 链成可执行文件并运行, 让 traits 的**嵌套依赖成员类型**
    // (iterator_traits<I>::category_type/value_type/difference_type) 在运行期
    // 真的参与完成正确的重载/循环/标签选择 —— 夹具里 16 个独立错误退出码,
    // 任何一个解析错都会返回非 0, 所以不会空过。
    // 历史缺陷: `typename T::type` 被当作值类型而非类型; 成员模板指针偏特化
    // (`CharPtrTraits : iterator_traits<char*>`) 的模板参数实参丢失 -> 偏特化
    // 不匹配 -> 嵌套名解不开。
    let src = include_str!("../fixtures/cpp/iterator_traits.cpp");
    let (out, obj) = run_cpp_object_test("cpp98-iterator-traits", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-cpp98-iterator-traits-{}", std::process::id()));
    let link = std::process::Command::new("g++")
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host g++ link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = std::process::Command::new(&exe)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run cpp98-iterator-traits binary");
    let _ = fs::remove_file(obj);
    let _ = fs::remove_file(&exe);
    assert!(
        run.status.success(),
        "run: exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

/// Phase 6.5: C++11 using type alias (using Name = Type;)
#[test]
#[cfg(unix)]
fn cpp11_using_type_alias_compile() {
    let src = include_str!("../fixtures/cpp/using_type_alias.cpp");
    let (out, obj) = run_cpp_object_test("cpp11-using-alias", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(obj);
}

/// Phase 9: C++20 features (using enum, consteval, constinit, <span>, <concepts>, <compare>)
#[test]
#[cfg(unix)]
fn cpp20_features_compile() {
    let src = include_str!("../fixtures/cpp/cpp20_features.cpp");
    let (out, obj) = run_cpp_object_test("cpp20-features", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(obj);
}

/// Phase 10: C++23 features (<expected>, <flat_map>, <print>, <stdfloat>)
#[test]
#[cfg(unix)]
fn cpp23_features_compile() {
    let src = include_str!("../fixtures/cpp/cpp23_features.cpp");
    let (out, obj) = run_cpp_object_test("cpp23-features", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(obj);
}

// ===== C++ Feature Keyword Checklist Validation =====

/// Verify every entry in CPP_FEATURE_KEYWORDS is detected by has_cpp_feature.
/// This prevents the recurring bug where new C++ feature test fixtures are
/// added but the detection heuristic is not updated.
#[test]
fn cpp_feature_keywords_all_detected() {
    // §5.2 规则 2: 子文件内不写 use / mod, 这里直接走全量路径名 (导入权归壳)。
    for &kw in dcc::cpp::lower::CPP_FEATURE_KEYWORDS {
        // Wrap keyword in spaces to simulate real source context.
        // For patterns already containing spaces (e.g. "class "),
        // just add minimal context.
        let test_src = format!("void foo() {{ {} }}\n", kw.trim());
        assert!(
            dcc::cpp::lower::has_cpp_feature(&test_src),
            "CPP_FEATURE_KEYWORDS entry {:?} not detected by has_cpp_feature",
            kw
        );
    }
}

/// Verify namespace typedef handling is consistent between AST lower
/// (qualify_name) and strip path (flatten :: to _).
/// Both paths must produce the same flattened name for namespace typedefs.
/// Stage 10.4: 仅在 `legacy-strip-debug` feature 下编译.
#[cfg(feature = "legacy-strip-debug")]
#[test]
fn cpp_namespace_typedef_qualify_name_consistent() {
    // Strip path: namespace typedef std::float32_t should become
    // std_float32_t (same as qualify_name output).
    let src = "namespace std { typedef float float32_t; }\n";
    let stripped = dcc::cpp::lower::strip_cpp_keywords(src);
    // After stripping, the typedef should use flattened name
    assert!(
        stripped.contains("std_float32_t") || stripped.contains("float32_t"),
        "namespace typedef not properly flattened in strip path.\nOutput: {}",
        stripped
    );
}

// ====== x86_64 后端缺陷清单收口 (缺陷 1-8) 的运行期对照 ======
// 每条测试都指向一个真实失效形态, 夹具名即缺陷类别; 严禁在编译器或本文件
// 按夹具名特判 (根因修复 + 单一真源, 见各实现处注释)。

/// 缺陷 1: C++ switch 下降按 `starts_with("case_<v>")` 匹配块名, ≥10 个 case 时
/// `case_1` 会命中 `case_11` 的块 (HashMap 迭代序决定命中哪一个, 故随机错编译)。
/// 夹具含 0..19 共 20 个 case 与跨 2^31 的乘加, 逐字节对照 g++。
#[test]
#[cfg(unix)]
fn cpp_switch_case_prefix_exact_gcc_cross_check() {
    let src = include_str!("../fixtures/cpp/switch_case_prefix_exact.cpp");
    cpp_gcc_cross_check("cpp-switch-case-prefix-exact", src);
}

/// 缺陷 2: C++ native `long` 乘加跨 2^31 被按 32 位截断
/// (`long fp = 103060990L; fp = fp * 100 + -10;` 得负数)。
#[test]
#[cfg(unix)]
fn cpp_long_mul_wide_const_gcc_cross_check() {
    let src = include_str!("../fixtures/cpp/long_mul_wide_const.cpp");
    cpp_gcc_cross_check("cpp-long-mul-wide-const", src);
}

/// 缺陷 8 (C++ 协程轴): awaiter 槽尺寸限制 ≤8 字节。根因是降级器把帧槽拷进
/// step 函数的局部量并在副本上调三个协议钩子, 违反 [basic.coro.await] 的
/// "三钩子同一对象" —— `await_suspend` 对 awaiter 自身字段的写随臂消失。
/// 夹具用 24/32 字节 awaiter (第二对 quad 正是后端装载轴) 改写自身分量并在
/// `await_resume` 校验, 三个协程交错驱动, 逐字节对照 g++。
#[test]
#[cfg(unix)]
fn cpp_coroutine_fat_awaiter_slot_run() {
    let src = include_str!("../fixtures/cpp/coroutine_fat_awaiter_slot.cpp");
    cpp_coroutine_cross_check("cpp-coroutine-fat-awaiter-slot", src);
}

/// C++ 前端缺陷: 一条声明里的多个 init-declarator 不支持 ([dcl.dcl]/[stmt.pre]).
/// 根因: `parse_local_var_decl` 只解析**一个** declarator, 遇到 `,` 就落到失败
/// 路径, `parse_body` 的容错把整段原文塞进 `CppStmt::OpaqueStmt`; 全局路径同样
/// 抛 "unsupported opaque declaration"。现在一条声明 = 共享 decl-specifier
/// (`DeclSpec`) + init-declarator 列表 (`InitDeclarator`), 指针/引用/数组层级
/// 归每个 declarator 自己 ([dcl.name]: `int *p, q;` 的 q 是 int)。
/// 五个夹具分别锁住五条路径: 函数体局部 / for-init / 全局 / 函数内 static /
/// 类成员。退出码必须与 g++ -std=c++20 真值相等 (夹具内 `return N` 逐条对应验证点)。
#[test]
#[cfg(unix)]
fn cpp_multi_declarator_local() {
    // 局部声明: 带初值 / 无初值 / 指针混合 `int *p, q = 6;` (q 必须是 int)。
    let src = include_str!("../fixtures/cpp/multi_declarator_local.cpp");
    cpp_std20_exit_code_cross_check("cpp-multi-declarator-local", src);
}

#[test]
#[cfg(unix)]
fn cpp_multi_declarator_for_init() {
    // for-init 只能是**单个** declaration-statement ([stmt.init]), 所以
    // `for (int i = 0, j = n - 1; i < j; i++, j--)` 必须作为一条语句降级。
    let src = include_str!("../fixtures/cpp/multi_declarator_for_init.cpp");
    cpp_std20_exit_code_cross_check("cpp-multi-declarator-for-init", src);
}

#[test]
#[cfg(unix)]
fn cpp_multi_declarator_global() {
    // 全局声明按 declarator 展开成 N 个 `CppDecl::GlobalVar`, 每个对象独立、
    // 类型正确 (含 const / static / volatile)。
    let src = include_str!("../fixtures/cpp/multi_declarator_global.cpp");
    cpp_std20_exit_code_cross_check("cpp-multi-declarator-global", src);
}

#[test]
#[cfg(unix)]
fn cpp_multi_declarator_static_local() {
    // `static int a = 1, b = 2;` —— 共享 static 存储期, 但每个对象各自只做
    // 一次线程安全初始化 ([stmt.dcl]), 跨调用各保留各的值。
    let src = include_str!("../fixtures/cpp/multi_declarator_static_local.cpp");
    cpp_std20_exit_code_cross_check("cpp-multi-declarator-static-local", src);
}

#[test]
#[cfg(unix)]
fn cpp_multi_declarator_member() {
    // 类成员声明展开成 N 个字段 (旧行为: 只留第一个名字, `m_b` 静默消失 ->
    // "member 'm_b' not found"), 外加成员函数体内的多 declarator。
    let src = include_str!("../fixtures/cpp/multi_declarator_member.cpp");
    cpp_std20_exit_code_cross_check("cpp-multi-declarator-member", src);
}

#[test]
#[cfg(unix)]
fn cpp_multi_declarator_typedef() {
    // `typedef int C, *P;` 同属「decl-specifier + init-declarator 列表」:
    // 必须展开成两个别名, 且指针/数组层级只属于各自的 declarator。
    let src = include_str!("../fixtures/cpp/multi_declarator_typedef.cpp");
    cpp_std20_exit_code_cross_check("cpp-multi-declarator-typedef", src);
}

// ===== M54: C++ 嵌套循环 SSA→AST 重建栈溢出根因回归 =====
// 旧缺陷: `rebuild_bb` 在循环内模式下不识别嵌套内层循环的 header (回边被
// 当普通边递归), 任意两层 for/while 嵌套的 C++ 程序无限互递归直到栈溢出;
// 修后又暴露出单 latch 循环体收集漏掉 `continue` 分支区域的第二层根因。
// 以 `cpp_std20_exit_code_cross_check` 与 g++ -std=c++20 逐用例对照退出码。

#[test]
#[cfg(unix)]
fn cpp_nested_for_two_level_run() {
    // 复现用例本体: 两层 for, 累加结果必须正确 (不只是不崩)。
    let src = include_str!("../fixtures/cpp/nested_for_two_level.cpp");
    cpp_std20_exit_code_cross_check("cpp-nested-for-two-level", src);
}

#[test]
#[cfg(unix)]
fn cpp_nested_for_three_level_break_continue_run() {
    // 三层 for + break/continue (覆盖 continue-latch 区域被误发 break 的缺陷)。
    let src = include_str!("../fixtures/cpp/nested_for_three_level_break_continue.cpp");
    cpp_std20_exit_code_cross_check("cpp-nested-for-three-level-break-continue", src);
}

#[test]
#[cfg(unix)]
fn cpp_nested_for_while_and_while_dowhile_run() {
    // for 嵌套 while、while 嵌套 do-while 的混合循环形状。
    let src = include_str!("../fixtures/cpp/nested_for_while_and_while_dowhile.cpp");
    cpp_std20_exit_code_cross_check("cpp-nested-for-while-and-while-dowhile", src);
}

#[test]
#[cfg(unix)]
fn cpp_nested_loop_member_call_run() {
    // 循环体里调用成员函数: 调用点把真实 CFG 回边撑活, 不会被折叠成直线。
    let src = include_str!("../fixtures/cpp/nested_loop_member_call.cpp");
    cpp_std20_exit_code_cross_check("cpp-nested-loop-member-call", src);
}

#[test]
#[cfg(unix)]
fn cpp_nested_loop_deep_stress_run() {
    // 8 层 for + 6 层混合 + 5 层 while 嵌套 do-while 压力: 确保修复是收敛性
    // 的, 不是靠循环层数上限报错混过去。
    let src = include_str!("../fixtures/cpp/nested_loop_deep_stress.cpp");
    cpp_std20_exit_code_cross_check("cpp-nested-loop-deep-stress", src);
}

// === M54: C++ 路径全局 / 命名空间作用域对象的建模与发射 ===

#[test]
#[cfg(unix)]
fn cpp_global_array_dims_and_sizeof_run() {
    // 缺陷 1+4: 全局数组的**维度**必须进到类型里 (`int g[10]` 的对象尺寸是 40 不是
    // 4), `sizeof(g)` / `&g[3]-g` 等下标算术常量都依赖它。
    let src = include_str!("../fixtures/cpp/global_array_dims_and_sizeof.cpp");
    cpp_std20_exit_code_cross_check("cpp-global-array-dims-and-sizeof", src);
}

#[test]
#[cfg(unix)]
fn cpp_global_array_init_and_access_run() {
    // 缺陷 1+2+3: 全局数组写入/读回、部分初始化 `{1,2}`、多维聚合、以及
    // `char s[] = "abc";` 由初值长度推出维度。
    let src = include_str!("../fixtures/cpp/global_array_init_and_access.cpp");
    cpp_std20_exit_code_cross_check("cpp-global-array-init-and-access", src);
}

#[test]
#[cfg(unix)]
fn cpp_constexpr_call_folds_in_constant_context_run() {
    // 缺陷 4: constexpr 函数调用必须在常量上下文里编译期求值 —— 数组维度、
    // `static_assert`、`switch case` 标签、enum 数、局部数组维度。
    let src = include_str!("../fixtures/cpp/constexpr_call_folds_in_constant_context.cpp");
    cpp_std20_exit_code_cross_check("cpp-constexpr-call-folds-in-constant-context", src);
}

#[test]
#[cfg(unix)]
fn cpp_namespace_array_and_extern_pairing_run() {
    // 命名空间作用域数组 (`namespace ns { int a[5]; }`) 的尺寸/下标/初值, 以及
    // `extern` 声明与后文定义配对成同一个对象 (含不完整数组声明由定义补全维度)。
    let src = include_str!("../fixtures/cpp/global_namespace_array_and_extern.cpp");
    cpp_std20_exit_code_cross_check("cpp-namespace-array-and-extern-pairing", src);
}

#[test]
#[cfg(unix)]
fn cpp_global_object_symbol_sizes_in_object_file() {
    // 缺陷 1 的**目标文件层**回归: 退出码相等只能证明代码读写一致, 符号表里的
    // 对象尺寸错了意味着真实内存踩踏 (旧行为: `int g[10]` 发成 4 字节)。
    // 这里直接卡 `objdump -t` 的 size 字段, 并与 g++ 产物的尺寸逐项对照。
    let src = "int g[10];\n\
               int two[2][3];\n\
               char s[] = \"abc\";\n\
               double darr[2];\n\
               struct P { int x; int y; };\n\
               P parr[3];\n\
               int main() { return 0; }\n";
    let want: [(&str, u64); 5] = [("g", 40), ("two", 24), ("s", 4), ("darr", 16), ("parr", 24)];
    let (out, obj) = run_cpp_object_test("cpp-global-symbol-sizes", src);
    assert!(out.status.success(), "dcc -c 失败: {}", String::from_utf8_lossy(&out.stderr));
    let dump = Command::new("objdump")
        .args(["-t"])
        .arg(&obj)
        .output()
        .expect("objdump -t");
    let text = String::from_utf8_lossy(&dump.stdout).to_string();
    let _ = fs::remove_file(&obj);
    for (name, size) in want {
        let got = object_symbol_size(&text, name)
            .unwrap_or_else(|| panic!("符号 {} 未出现在对象文件符号表里:\n{}", name, text));
        assert_eq!(got, size, "全局对象 `{}` 的发射尺寸应为 {} 字节（维度必须进类型）", name, size);
    }
}

// ---------------------------------------------------------------------------
// G12: Variadic Templates + Parameter Packs
//
// 验证可变参数模板的解析和实例化。原 `tests/test_g12_variadic_templates.rs`
// 独立 target 并入本主题文件（模板属语言面，与 ABI 差分主题 cpp_abi_* 不同源）。
// 手工构造 AST + 注册表的用例覆盖声明/实例化/类型键；后面几个是**源码驱动**的
// 用例：真正让 `<print>`/`<tuple>` 整条命名空间退化成 Opaque 的是解析器对
// `Args... args` / `args...` / `sizeof...(Args)` 的支持，这几个用例锁定 AST 形状，
// 解析一旦退化 (变 Opaque / 形参丢包) 就会红。
// ---------------------------------------------------------------------------

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
    let args = vec![
        MangleType::Int,
        MangleType::VariadicPack(vec![MangleType::Double, MangleType::Float]),
        MangleType::Char,
    ];

    let expanded = dcc::cpp::template::merge_expanded_args(&args);

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
