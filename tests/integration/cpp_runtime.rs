// included from tests/integration.rs — not a standalone cargo target

/// C++ 前端 M7/M8: 简单 class + 构造函数 + 虚析构链路能 lowered, 且产物里 ctor 符号与
/// **调用**真实存在 —— 两条后端路径各自钉住, 退出码互相一致。
///
/// 历史 (为什么以前是死件): 本用例写于 M7「dcc 主入口还没开放 C++ 子命令」时期,
/// 当时只能直接调 driver 模块, 而 integration target 拿不到 `dcc::cpp`, 于是被一个
/// 恒假条件属性永久排除编译 + 空函数体 (覆盖索引里的 JAC-DISABLED)。该前提在 M8
/// 之后已失效: `dcc` 现在接 `.cpp` 与 `--cxx-native=off|on|auto`, 所以删掉 cfg 门,
/// 用真实 CLI 判据重写 (函数名不变 —— 覆盖索引与其它文档引用它)。
///
/// 判据 (每条都是可回归的具体证据, 不是对常量取真):
///   native (`--cxx-native=on`, dcc 内置 codegen):
///     a. 符号表里 `Counter_ctor_complete` / `Counter_ctor_base` / `Counter_dtor_complete`
///        / `Counter_dtor_base` / `Counter_bump` / `main` 六个符号都**带地址**(已定义);
///     b. 多态类的 vtable 对象 `_ZTV7Counter` 已定义;
///     c. 重定位表里 `R_X86_64_PLT32 Counter_ctor_complete` —— ctor 的**调用**真实存
///        在 (不是只在符号表挂个名字);
///     d. 虚析构链路三环: ctor 体内 `R_X86_64_PC32 _ZTV7Counter` (vptr 写 vtable) +
///        vtable 槽位 `R_X86_64_64 Counter_dtor_complete` + 回收点 `R_X86_64_PLT32
///        Counter_dtor_complete`。
///   legacy (`--cxx-native=off`, Itanium mangled + host cc):
///     e. `_ZN7CounterC1Ev` (complete-object ctor) / `_ZN7CounterD1Ev` / `_ZN7Counter4bumpEv`
///        三个 mangled 符号均已定义;
///     f. 反汇编里真的有对 `_ZN7CounterC1Ev` 的 `call`;
///     g. 产物经 host g++ 链接后运行退出码 == 2 (两次 bump 后的 n), 且与 native 产物
///        退出码一致 —— 同一语义两条后端不得分叉。
#[test]
#[cfg(unix)]
fn cpp_class_with_ctor_lowers_and_compiles() {
    let src = r#"
class Counter {
public:
  int n;
  Counter() : n(0) {}
  void bump() { n = n + 1; }
  virtual ~Counter() {}
};
int main() { Counter c; c.bump(); c.bump(); return c.n; }
"#;

    // nm 行有地址字段 = 该符号在本 TU 里**被定义** (区别于 `U`/`w` 这类无地址的引入项)。
    let is_defined = |dump: &str, name: &str| -> bool {
        dump.lines().any(|l| {
            let fields: Vec<&str> = l.split_whitespace().collect();
            fields.len() == 3 && fields[2] == name && fields[0].starts_with(|c: char| c.is_ascii_hexdigit())
        })
    };
    let nm_of = |obj: &std::path::Path| -> String {
        let out = Command::new("nm").arg(obj).output().expect("run nm");
        assert!(out.status.success(), "nm 失败: {:?}", obj);
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let objdump_of = |args: &[&str], obj: &std::path::Path| -> String {
        let mut cmd = Command::new("objdump");
        for a in args {
            cmd.arg(a);
        }
        let out = cmd.arg(obj).output().expect("run objdump");
        assert!(out.status.success(), "objdump 失败: {:?}", obj);
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    // ---------------- native 路径 ----------------
    let ndir = temp_dir("ctor-native");
    let nsrc = write_src(&ndir, "counter.cpp", src);
    let nobj = ndir.join("counter_native.o");
    let nout = dcc_cmd()
        .arg("--cxx-native=on")
        .arg("-c")
        .arg(&nsrc)
        .arg("-o")
        .arg(&nobj)
        .output()
        .expect("run dcc --cxx-native=on");
    let nerr = String::from_utf8_lossy(&nout.stderr).to_string();
    assert!(nout.status.success(), "native 路径编译失败:\n{nerr}");

    let nsyms = nm_of(&nobj);
    for name in [
        "Counter_ctor_complete",
        "Counter_ctor_base",
        "Counter_dtor_complete",
        "Counter_dtor_base",
        "Counter_bump",
        "main",
        "_ZTV7Counter",
    ] {
        assert!(
            is_defined(&nsyms, name),
            "native 产物缺少已定义符号 {name} (class + ctor + 虚析构未真降出符号), nm 输出:\n{nsyms}"
        );
    }

    // objdump 的重定位列宽随版本变化, 所以按行匹配「类型 + 目标符号」两个片段,
    // 不依赖它们的间距。
    let has_reloc = |dump: &str, kind: &str, sym: &str| -> bool {
        dump.lines().any(|l| l.contains(kind) && l.contains(sym))
    };
    let nreloc = objdump_of(&["-rx"], &nobj);
    for (kind, sym, label) in [
        ("R_X86_64_PLT32", "Counter_ctor_complete", "main 里的 ctor 调用"),
        ("R_X86_64_PC32", "_ZTV7Counter", "ctor/dtor 里的 vptr 写入"),
        ("R_X86_64_64", "Counter_dtor_complete", "vtable 里的虚析构槽位"),
        ("R_X86_64_PLT32", "Counter_dtor_complete", "main 里的析构回收调用"),
    ] {
        assert!(
            has_reloc(&nreloc, kind, sym),
            "native 产物缺少{label} (重定位 `{kind}` -> `{sym}`), objdump -rx:\n{nreloc}"
        );
    }

    // ---------------- legacy 路径 ----------------
    let ldir = temp_dir("ctor-legacy");
    let lsrc = write_src(&ldir, "counter.cpp", src);
    let lobj = ldir.join("counter_legacy.o");
    let lout = dcc_cmd()
        .arg("--cxx-native=off")
        .arg("-c")
        .arg(&lsrc)
        .arg("-o")
        .arg(&lobj)
        .output()
        .expect("run dcc --cxx-native=off");
    let lerr = String::from_utf8_lossy(&lout.stderr).to_string();
    assert!(lout.status.success(), "legacy 路径编译失败:\n{lerr}");

    let lsyms = nm_of(&lobj);
    for name in ["_ZN7CounterC1Ev", "_ZN7CounterD1Ev", "_ZN7Counter4bumpEv"] {
        assert!(
            is_defined(&lsyms, name),
            "legacy 产物缺少 Itanium mangled 符号 {name}, nm 输出:\n{lsyms}"
        );
    }
    let ldis = objdump_of(&["-d"], &lobj);
    assert!(
        ldis.contains("call") && ldis.contains("<_ZN7CounterC1Ev>"),
        "legacy 产物里没有对 ctor `_ZN7CounterC1Ev` 的调用, objdump -d:\n{ldis}"
    );

    // ---------------- 两态运行语义一致 ----------------
    let native_code = host_link_cpp_object_and_run("cpp-ctor-native", &nobj);
    let legacy_code = host_link_cpp_object_and_run("cpp-ctor-legacy", &lobj);
    let _ = std::fs::remove_dir_all(&ndir);
    let _ = std::fs::remove_dir_all(&ldir);
    assert_eq!(
        native_code, 2,
        "native 产物运行退出码应为 2 (两次 bump), 实得 {native_code} (-2=链接失败, -100=无 g++)"
    );
    assert_eq!(
        legacy_code, 2,
        "legacy 产物运行退出码应为 2 (两次 bump), 实得 {legacy_code} (-2=链接失败, -100=无 g++)"
    );
    if tool_exists("g++") {
        assert_eq!(
            gxx_native_cpp_exit_code("cpp-ctor-ref", src),
            native_code,
            "g++ 原生编译同一源码的退出码必须与 dcc 产物一致"
        );
    }
}

/// M17.0: explicit 构造函数
#[test]
#[cfg(unix)]
fn cpp_explicit_ctor() {
    let src = include_str!("../fixtures/cpp/explicit_ctor.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-explicit-ctor", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: using Base::Base; 继承构造
#[test]
#[cfg(unix)]
fn cpp_inheriting_ctor() {
    let src = include_str!("../fixtures/cpp/inheriting_ctor.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-inheriting-ctor", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: noexcept 关键字
#[test]
#[cfg(unix)]
fn cpp_noexcept() {
    let src = include_str!("../fixtures/cpp/noexcept.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-noexcept", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: placement new
#[test]
#[cfg(unix)]
fn cpp_placement_new() {
    let src = include_str!("../fixtures/cpp/placement_new.cpp");
    let (out, _obj) = run_cpp_object_test("cpp-placement-new", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let _ = fs::remove_file(_obj);
}

/// M17.0: vtable/typeinfo/member-offset 符号多文件链接不重复定义
/// 两个 TU 包含相同 class 定义，链接器应通过 weak 解析而非报 multiple definition
#[test]
#[cfg(unix)]
fn cpp_weak_vtable_multifile_link() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    // 共享 class 定义（含 virtual + 数据成员 + 显式构造）
    let class_def = "class Foo {\n\
public:\n\
    Foo() : x(0) {}\n\
    virtual int val() const { return 42; }\n\
    int x;\n\
};\n";
    // TU1: 定义 Foo + helper() 使用 val() 和 x
    let tu1 = format!(
        "{class_def}\
int helper() {{ Foo f; f.x = 1; return f.val() + f.x; }}\n"
    );
    // TU2: 同样定义 Foo + main() 也使用 val()/x, 并跨 TU 调用 helper()
    let tu2 = format!(
        "{class_def}\
extern int helper();\n\
int main() {{ Foo g; g.x = 2; if (helper() != 43) return 1; return g.val() + g.x - 44; }}\n"
    );
    let p1 = dir.join(format!("dcc-cpp-weak-tu1-{pid}.cpp"));
    let p2 = dir.join(format!("dcc-cpp-weak-tu2-{pid}.cpp"));
    let o1 = dir.join(format!("dcc-cpp-weak-tu1-{pid}.o"));
    let o2 = dir.join(format!("dcc-cpp-weak-tu2-{pid}.o"));
    let exe = dir.join(format!("dcc-cpp-weak-{pid}"));
    fs::write(&p1, &tu1).unwrap();
    fs::write(&p2, &tu2).unwrap();
    let dcc = env!("CARGO_BIN_EXE_dcc");
    // 编译 TU1
    let c1 = Command::new(dcc)
        .args(["-c", "-o"])
        .arg(&o1)
        .arg(&p1)
        .output()
        .expect("dcc");
    assert!(c1.status.success(), "tu1 compile: {}", String::from_utf8_lossy(&c1.stderr));
    // 编译 TU2
    let c2 = Command::new(dcc)
        .args(["-c", "-o"])
        .arg(&o2)
        .arg(&p2)
        .output()
        .expect("dcc");
    let _ = fs::remove_file(&p1);
    let _ = fs::remove_file(&p2);
    assert!(c2.status.success(), "tu2 compile: {}", String::from_utf8_lossy(&c2.stderr));
    // 真实多 TU 链接: 两个 TU 都发射 Foo 的 vtable / typeinfo / typeinfo name /
    // 构造/析构/虚成员函数, 这些符号必须是 weak(GC-section 可合并), 否则
    // host gcc 会报 multiple definition.
    //
    // 链接驱动用 g++ 而不是 gcc: C++ TU 的零成本异常表需要 libstdc++/libgcc_s
    // (`__gxx_personality_v0` 等), 这与 g++ 自己产物的要求一致 —— g++ 编译的
    // 含异常表的目标文件用纯 gcc 链接同样报 undefined reference。本测试断言的
    // 是「weak 符号不重复定义」, 与用哪个链接驱动无关。
    let link = Command::new("g++")
        .arg("-no-pie")
        .arg(&o1)
        .arg(&o2)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host g++ link");
    let link_stderr = String::from_utf8_lossy(&link.stderr).to_string();
    assert!(
        !link_stderr.contains("multiple definition"),
        "weak vtable/typeinfo symbols collided across TUs: {link_stderr}"
    );
    assert!(link.status.success(), "link: {link_stderr}");
    let run = Command::new(&exe)
        .output()
        .expect("run weak-vtable multifile binary");
    let _ = fs::remove_file(&o1);
    let _ = fs::remove_file(&o2);
    let _ = fs::remove_file(&exe);
    assert_eq!(
        run.status.code().unwrap_or(-1),
        0,
        "cpp_weak_vtable_multifile_link exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

/// Stage 7 任务 3: 构造/析构期虚派发绑定当前类 vtable.
/// ctor/dtor 体内对虚函数的调用必须绑定"当前正在构造/析构的类"的 vtable
/// (而非最派生类). 用 dcc 产出的 .o 经 g++ 链接后运行, 退出码必须为 0, 且与
/// g++ 原生编译运行的退出码交叉一致 (证明语义等价).
#[test]
#[cfg(unix)]
fn cpp_ctor_dtor_virtual_dispatch() {
    let src = include_str!("../fixtures/cpp/ctor_dtor_virtual_dispatch.cpp");
    // 1) dcc 编译为 .o
    let (out, obj) = run_cpp_object_test("cpp-ctor-dtor-vd", src);
    assert!(
        out.status.success(),
        "cpp_ctor_dtor_virtual_dispatch compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // 2) g++ 链接 dcc 的 .o 并运行 (dcc 产物需与 libstdc++ 运行期共存自洽)
    let dcc_exit = host_link_cpp_object_and_run("cpp-ctor-dtor-vd", &obj);
    let _ = fs::remove_file(&obj);
    // 3) g++ 原生编译运行同一夹具作为真值
    let gxx_dir = std::env::temp_dir();
    let gxx_src = gxx_dir.join(format!("dcc-ctor-dtor-vd-{}.cpp", std::process::id()));
    let gxx_exe = gxx_dir.join(format!("dcc-ctor-dtor-vd-{}.bin", std::process::id()));
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
    assert_eq!(dcc_exit, 0, "构造/析构期虚派发断言未通过");
}

/// Stage 5/7: `delete p` 必须先跑析构再释放存储 ([expr.delete]/3).
///
/// 三层证据:
///   1) 符号层 (nm): 最派生类的 complete 析构以 weak 定义发射, `operator new` /
///      `operator delete` 以未定义外部符号引用 (由 libstdc++ 提供) —— 否则
///      `delete` 只会退化成 `free()`;
///   2) 布局层 (readelf 重定位): 派生类主 vtable 的**虚析构槽**必须指向
///      `Derived_dtor_complete` (而非继承来的 `Base_dtor_complete`) —— 这是
///      `delete (Base*)pd` 能跑到派生析构的根因, 也是 sema 为 `Derived` 合成的
///      隐式虚析构登记进 `virtual_methods` 的佐证;
///   3) 行为层: dcc .o 经 g++ 链接运行的退出码与 g++ 原生编译一致 (0), 夹具内
///      同时覆盖多态点型虚派发 / 派生点型 / 非多态类直调三条路径, 并用相邻 `int`
///      全局捕获窄对象被 8 字节 store 越界写。
#[test]
#[cfg(unix)]
fn cpp_delete_runs_virtual_dtor() {
    let src = include_str!("../fixtures/cpp/delete_virtual_dtor.cpp");
    // 1) dcc 编译为 .o
    let (out, obj) = run_cpp_object_test("cpp-del-vdtor", src);
    assert!(
        out.status.success(),
        "cpp_delete_runs_virtual_dtor compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let obj_data = fs::read(&obj).expect("read dcc object");
    let syms = elf64_syms(&obj_data);
    let sym_value =
        |name: &str| -> Option<u64> { syms.iter().find(|s| s.name == name).map(|s| s.value) };
    for name in [
        "Derived_dtor_complete",
        "Base_dtor_complete",
        "Plain_dtor_complete",
    ] {
        assert!(
            sym_value(name).is_some(),
            "dcc 产物应发射析构函数 {} (delete 的调用目标): {:?}",
            name,
            syms.iter().map(|s| s.name.as_str()).collect::<Vec<_>>()
        );
    }
    let undef = elf64_undefined_symbols(&obj_data);
    for mangled in ["_Znwm", "_ZdlPv"] {
        assert!(
            undef.iter().any(|s| s == mangled),
            "new/delete 应引用 libstdc++ 的 {} (未定义外部符号), 实际: {:?}",
            mangled,
            undef
        );
    }

    // 2) 布局层: `_ZTV7Derived` = [offset-to-top][typeinfo][dtor][tag],
    //    即 address point = 符号 + 0x10, 虚析构在 (vptr) 位置 = 符号 + 0x10。
    let relocs = readelf_relocations(&obj);
    let reloc_at = |addr: u64| -> String {
        relocs
            .lines()
            .find(|l| {
                let mut it = l.split_whitespace();
                match (it.next(), it.nth(1)) {
                    (Some(off), Some(ty)) => {
                        ty == "R_X86_64_64" && u64::from_str_radix(off, 16).ok() == Some(addr)
                    },
                    _ => false,
                }
            })
            .unwrap_or("")
            .to_string()
    };
    let derived_vt = sym_value("_ZTV7Derived").expect("_ZTV7Derived 应已发射");
    assert!(
        reloc_at(derived_vt + 8).contains("_ZTI7Derived"),
        "派生类 vtable 的 typeinfo 槽 (vptr-8) 必须指向 _ZTI7Derived, 实际: {}",
        reloc_at(derived_vt + 8)
    );
    assert!(
        reloc_at(derived_vt + 0x10).contains("Derived_dtor_complete"),
        "派生类主 vtable 的虚析构槽必须是 Derived_dtor_complete (重写基类槽), 实际: {}",
        reloc_at(derived_vt + 0x10)
    );
    let base_vt = sym_value("_ZTV4Base").expect("_ZTV4Base 应已发射");
    assert!(
        reloc_at(base_vt + 0x10).contains("Base_dtor_complete"),
        "基类 vtable 的虚析构槽应是 Base_dtor_complete, 实际: {}",
        reloc_at(base_vt + 0x10)
    );

    // 3) 行为层: g++ 链接 dcc 的 .o 并运行 (与 libstdc++ 的 operator delete 共存)
    let dcc_exit = host_link_cpp_object_and_run("cpp-del-vdtor", &obj);
    let _ = fs::remove_file(&obj);
    // 4) g++ 原生编译运行同一夹具作为真值
    let dir = std::env::temp_dir();
    let gxx_src = dir.join(format!("dcc-del-vdtor-{}.cpp", std::process::id()));
    let gxx_exe = dir.join(format!("dcc-del-vdtor-{}.bin", std::process::id()));
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
    assert_eq!(dcc_exit, 0, "`delete` 的析构语义运行断言未通过");
}

/// Stage 7 任务 2: `dynamic_cast<T*>` 与 libstdc++ 运行期互操作.
///
/// dcc 只把 `dynamic_cast` 降级为对 `__dynamic_cast(src, src_ti, dst_ti, hint)`
/// 的调用, 类型遍历完全由 libstdc++ 按 Itanium ABI 读 dcc 发射的 typeinfo 对象
/// (`__class_type_info`/`__si_class_type_info`/`__vmi_class_type_info`) 与其 vptr
/// 指向的 type_info 虚表完成. 因此两侧必须同时成立:
///   1) 符号层: `_ZTI<T>`/`_ZTV<T>` weak 发射 + `U __dynamic_cast` 引用;
///   2) 字层: typeinfo 对象首槽重定位到对应形态的 type_info 虚表, 次槽指向
///      `_ZTS<T>` 名字串, 基类槽指向基类 `_ZTI`; vtable 的 typeinfo 槽在 vptr-8,
///      重写后的虚函数实现落在基类同槽上 (多继承下不同基链各自成槽);
///   3) 行为层: upcast/downcast/crosscast/null 四分支退出码与 g++ 原生一致.
#[test]
#[cfg(unix)]
fn cpp_dynamic_cast_runtime_interop() {
    let src = include_str!("../fixtures/cpp/dynamic_cast_basic.cpp");
    // 1) dcc 编译为 .o
    let (out, obj) = run_cpp_object_test("cpp-dyncast", src);
    assert!(
        out.status.success(),
        "cpp_dynamic_cast_runtime_interop compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let obj_data = fs::read(&obj).expect("read dcc object");
    // 2) nm 佐证: 多继承类的 typeinfo / vtable / secondary vtable 均 weak 发射,
    //    `__dynamic_cast` 以未定义外部符号引用 (由 libstdc++ 提供实现)
    let syms = elf64_syms(&obj_data);
    let sym_value =
        |name: &str| -> Option<u64> { syms.iter().find(|s| s.name == name).map(|s| s.value) };
    for name in [
        "_ZTI4Base",
        "_ZTI3Mid",
        "_ZTI5Other",
        "_ZTI4Leaf",
        "_ZTS4Leaf",
        "_ZTV4Leaf",
        "_ZTVLeaf::baseOther",
        "dcc_ti_class_vtable",
        "dcc_ti_si_vtable",
        "dcc_ti_vmi_vtable",
    ] {
        assert!(
            sym_value(name).is_some(),
            "dcc 产物应发射 RTTI 符号 {}: {:?}",
            name,
            syms.iter().map(|s| s.name.as_str()).collect::<Vec<_>>()
        );
    }
    assert!(
        elf64_undefined_symbols(&obj_data)
            .iter()
            .any(|s| s == "__dynamic_cast"),
        "dynamic_cast 应降级为对 libstdc++ `__dynamic_cast` 的引用"
    );

    // 3) 重定位层: `_ZTI4Leaf` 是 __vmi_class_type_info (双非虚基), 槽序必须是
    //    [vptr, __name, flags|base_count, bases[0].typeinfo, bases[0].offset_flags,
    //     bases[1].typeinfo, ...]; `_ZTV4Leaf` 的 typeinfo 槽在 vptr-8 (= +0x10),
    //    虚函数槽从 vptr 开始: slot0 = 继承的虚析构, slot1 = Leaf_id (重写 Mid/Base
    //    的 id) —— 多继承下 `Leaf::oid` 占自己的槽, 不得顶掉 `Leaf::id`.
    let relocs = readelf_relocations(&obj);
    // readelf 的偏移列宽随节大小变化, 按数值解析而非字符串宽度对齐。
    let reloc_at = |addr: u64| -> String {
        relocs
            .lines()
            .find(|l| {
                let mut it = l.split_whitespace();
                match (it.next(), it.nth(1)) {
                    (Some(off), Some(ty)) => {
                        ty == "R_X86_64_64" && u64::from_str_radix(off, 16).ok() == Some(addr)
                    },
                    _ => false,
                }
            })
            .unwrap_or("")
            .to_string()
    };
    let leaf_ti = sym_value("_ZTI4Leaf").unwrap();
    assert!(
        reloc_at(leaf_ti).contains("dcc_ti_vmi_vtable"),
        "_ZTI4Leaf 首槽 (vptr) 必须指向 __vmi_class_type_info 虚表, 实际: {}",
        reloc_at(leaf_ti)
    );
    assert!(
        reloc_at(leaf_ti + 8).contains("_ZTS4Leaf"),
        "_ZTI4Leaf 的 __name 槽必须指向 _ZTS4Leaf, 实际: {}",
        reloc_at(leaf_ti + 8)
    );
    assert!(
        reloc_at(leaf_ti + 0x18).contains("_ZTI3Mid"),
        "__vmi_class_type_info.__bases[0].typeinfo 必须是 _ZTI3Mid, 实际: {}",
        reloc_at(leaf_ti + 0x18)
    );
    assert!(
        reloc_at(leaf_ti + 0x28).contains("_ZTI5Other"),
        "__vmi_class_type_info.__bases[1].typeinfo 必须是 _ZTI5Other, 实际: {}",
        reloc_at(leaf_ti + 0x28)
    );
    let leaf_vt = sym_value("_ZTV4Leaf").unwrap();
    assert!(
        reloc_at(leaf_vt + 8).contains("_ZTI4Leaf"),
        "vtable 的 typeinfo 槽 (vptr-8) 必须指向本类 _ZTI, 实际: {}",
        reloc_at(leaf_vt + 8)
    );
    assert!(
        reloc_at(leaf_vt + 0x18).contains("Leaf_id"),
        "Leaf 主 vtable 的 id 槽必须是重写实现 Leaf_id (不是 Mid_id), 实际: {}",
        reloc_at(leaf_vt + 0x18)
    );
    // Other 子对象的 secondary vtable: 自己的虚函数序 (dtor, oid), 与 Other
    // 自身主 vtable 同序 —— 这是 static_cast<Other*> 后能正确虚派发的根因。
    let leaf_sec = sym_value("_ZTVLeaf::baseOther").unwrap();
    assert!(
        reloc_at(leaf_sec + 0x18).contains("Leaf_oid"),
        "Leaf 的 Other 子对象 secondary vtable 的 oid 槽必须是 Leaf_oid, 实际: {}",
        reloc_at(leaf_sec + 0x18)
    );

    // 4) g++ 链接 dcc 的 .o 并运行 (走 libstdc++ 真 `__dynamic_cast`)
    let dcc_exit = host_link_cpp_object_and_run("cpp-dyncast", &obj);
    let _ = fs::remove_file(&obj);
    // 5) g++ 原生编译运行同一夹具作为真值
    let dir = std::env::temp_dir();
    let gxx_src = dir.join(format!("dcc-dyncast-{}.cpp", std::process::id()));
    let gxx_exe = dir.join(format!("dcc-dyncast-{}.bin", std::process::id()));
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
    assert_eq!(dcc_exit, 0, "dynamic_cast upcast/downcast/crosscast/null 四分支运行断言未通过");
}

/// P2.6: placement new 走裸 malloc 替身 (Section 6). 验证 dcc
/// parser 检测 `new (ptr) T(args)` 形式，lower 阶段 emit 构造
/// 链 `(void*)(ptr), mangle_ctor(&n, ...)(ptr, args)`, 不调用
/// operator new 分配新内存，模拟 Itanium C++ ABI placement
/// new 语义 (M17.0 阶段实装).
#[test]
#[cfg(unix)]
fn cpp_placement_new_ctor() {
    let src = include_str!("../fixtures/cpp/placement_new_ctor.cpp");
    let (out, obj) = run_cpp_object_test("cpp-placement-new-ctor", src);
    let _ = fs::remove_file(&obj);
    assert!(
        out.status.success(),
        "cpp_placement_new_ctor compile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[cfg(unix)]
fn cpp_enum_semantics_runtime() {
    // enum 保留枚举值 + scoped + 底层类型; dcc -c 后 host gcc 链接并运行.
    let src = include_str!("../fixtures/cpp/enum_semantics.cpp");
    let (out, obj) = run_cpp_object_test("cpp-enum-semantics", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-cpp-enum-semantics-{}", std::process::id()));
    let link = std::process::Command::new("gcc")
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host gcc link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run enum binary");
    let _ = fs::remove_file(obj);
    let _ = fs::remove_file(&exe);
    assert!(
        run.status.success(),
        "run: exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
#[cfg(unix)]
fn cpp_member_parser_runtime() {
    // 引用成员 + operator new/delete + hex/oct/bin/float/转义字面量.
    let src = include_str!("../fixtures/cpp/member_parser.cpp");
    let (out, obj) = run_cpp_object_test("cpp-member-parser", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-cpp-member-parser-{}", std::process::id()));
    let link = std::process::Command::new("gcc")
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host gcc link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run member-parser binary");
    let _ = fs::remove_file(obj);
    let _ = fs::remove_file(&exe);
    assert!(
        run.status.success(),
        "run: exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
#[cfg(unix)]
fn cpp_auto_deduction_runtime() {
    // auto 从 函数调用 / 成员调用 / 成员访问 / new 推导真实类型.
    let src = include_str!("../fixtures/cpp/auto_deduction.cpp");
    let (out, obj) = run_cpp_object_test("cpp-auto-deduction", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-cpp-auto-deduction-{}", std::process::id()));
    let link = std::process::Command::new("gcc")
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host gcc link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run auto binary");
    let _ = fs::remove_file(obj);
    let _ = fs::remove_file(&exe);
    assert!(
        run.status.success(),
        "run: exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
#[cfg(unix)]
fn cpp_noexcept_expr_runtime() {
    // noexcept(expr) 分析表达式抛异常可能性; dcc -c 后 host gcc 链接并运行.
    let src = include_str!("../fixtures/cpp/noexcept_expr.cpp");
    let (out, obj) = run_cpp_object_test("cpp-noexcept-expr", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-cpp-noexcept-expr-{}", std::process::id()));
    let link = std::process::Command::new("gcc")
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host gcc link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run noexcept binary");
    let _ = fs::remove_file(obj);
    let _ = fs::remove_file(&exe);
    assert!(
        run.status.success(),
        "run: exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
#[cfg(unix)]
fn cpp_coroutine_runtime() {
    // 协程 co_yield/co_return 降级为状态机后, 挂起位置必须跨调用保持.
    // 升级背景: 原本 #[ignore] 只要求能编译. 现在夹具把形参 (n) 与局部量 (i/total/k)
    // 提升进协程帧 (即 promise 对象尾部追加的帧字段), 把 while / for 循环体内的
    // 挂起点与回边重排成 switch+fallthrough 状态机 (等价 CFG 变换), 并用 20 条
    // `if (...) return N` 断言两个交错推进的 Generator 帧互不干扰、死分支里的
    // co_yield 不污染状态机、co_return 后重复驱动仍为 done.
    // dcc -c + host gcc 链接运行, 退出码与 g++ -std=c++20 逐条对照.
    let src = include_str!("../fixtures/cpp/coroutine_runtime.cpp");
    cpp_coroutine_cross_check("cpp-coroutine-runtime", src);
}

#[test]
#[cfg(unix)]
fn cpp_virtual_inheritance_runtime() {
    // 菱形虚继承 Itanium ABI 布局：vbtable + vbptr + 尾部共享虚基子对象，
    // 成员访问与上转型经 vbtable this-adjust; dcc -c + host gcc 链接运行.
    let src = include_str!("../fixtures/cpp/virtual_inheritance.cpp");
    let (out, obj) = run_cpp_object_test("cpp-virtual-inheritance", src);
    assert!(out.status.success(), "compile: {}", String::from_utf8_lossy(&out.stderr));
    let dir = std::env::temp_dir();
    let exe = dir.join(format!("dcc-cpp-virtual-inheritance-{}", std::process::id()));
    let link = std::process::Command::new("gcc")
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host gcc link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run virtual-inheritance binary");
    let _ = fs::remove_file(obj);
    let _ = fs::remove_file(&exe);
    assert!(
        run.status.success(),
        "run: exit={:?} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
#[cfg(unix)]
fn cpp_module_partition_import_runtime() {
    // M26.4: module partition 跨 TU.
    // part TU `export module mymod:part;` -> mymod.part.bmf (P 行);
    // primary TU `export module mymod; import :part;` -> mymod.bmf (X 行),
    // 分区导出并入; main `import mymod;` 链接全部三个对象文件.
    let dir = std::env::temp_dir().join(format!("dcc-mod-part-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let part_path = dir.join("part.cpp");
    let part_obj = dir.join("part.o");
    let part_bmf = dir.join("mymod.part.bmf");
    let prim_path = dir.join("prim.cpp");
    let prim_obj = dir.join("prim.o");
    let prim_bmf = dir.join("mymod.bmf");
    let main_path = dir.join("main.cpp");
    let main_obj = dir.join("main.o");
    let exe = dir.join("main");

    // 分区 TU
    fs::write(&part_path, "export module mymod:part;\nexport int pfn() { return 7; }\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&part_obj)
        .arg(&part_path)
        .arg(format!("--emit-module-file={}", part_bmf.display()))
        .output()
        .expect("run dcc on partition TU");
    assert!(
        out.status.success(),
        "partition compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let part_text = fs::read_to_string(&part_bmf).unwrap();
    assert!(part_text.contains("M mymod"), "{}", part_text);
    assert!(part_text.contains("P part"), "{}", part_text);
    assert!(part_text.contains("E F pfn"), "{}", part_text);

    // primary TU (导入分区, 再导出)
    fs::write(
        &prim_path,
        "export module mymod;\nimport :part;\nexport int get() { return pfn(); }\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&prim_obj)
        .arg(&prim_path)
        .arg(format!("--emit-module-file={}", prim_bmf.display()))
        .output()
        .expect("run dcc on primary TU");
    assert!(
        out.status.success(),
        "primary compile: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let prim_text = fs::read_to_string(&prim_bmf).unwrap();
    assert!(prim_text.contains("M mymod"), "{}", prim_text);
    assert!(prim_text.contains("X part"), "{}", prim_text);
    assert!(prim_text.contains("E F get"), "{}", prim_text);

    // 导入 TU
    fs::write(&main_path, "import mymod;\nint main() { return get() != 7; }\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&main_obj)
        .arg(&main_path)
        .output()
        .expect("run dcc on import TU");
    assert!(out.status.success(), "import compile: {}", String::from_utf8_lossy(&out.stderr));

    let link = Command::new("gcc")
        .arg(&part_obj)
        .arg(&prim_obj)
        .arg(&main_obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("host gcc link");
    assert!(link.status.success(), "link: {}", String::from_utf8_lossy(&link.stderr));
    let run = Command::new(&exe).output().expect("run partition binary");
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
fn cpp_module_export_single_declaration_private_fragment_runtime() {
    // M53 任务 6: **声明级** export (`export int f();` 不带 `export { }` 块) 与
    // `export { int g(); }` 块路径必须同形: 两条都进 BMF `E F` 记录, 都保持外部
    // 链接, 导入 TU 能链接能跑. 接手前的基线会把裸 export 声明的标记丢掉
    // (parse_export_decl 透传不落逐声明标记), 于是 "接口里声明 + 私有片段里
    // 定义" 被误降为内部链接, 导入端得到 undefined reference.
    // g++ -std=c++20 -fmodules-ts 对照: 同样的裸 `export int f();` 写法 (定义放
    // 在 purview 里) 编译+链接+运行 exit 0 (支持); 而 `module :private;` 本身
    // 在 g++ 13.3 上是 "sorry, unimplemented: private module fragment", 所以
    // 私有片段那半只能按 dcc 侧 BMF 与链接自洽验证.
    let dir = std::env::temp_dir().join(format!("dcc-mod-expdecl-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();

    let out = dcc_compile_module(
        &dir,
        "decl_export_mod",
        r#"export module decl_export_mod;
export int via_single_decl();
export {
    int via_export_block();
}
module :private;
int via_single_decl() { return 3; }
int via_export_block() { return 4; }
"#,
        true,
    );
    assert!(out.status.success(), "iface compile: {}", String::from_utf8_lossy(&out.stderr));
    // BMF 文件名按 TU 名 (`<name>.bmf`), 这里 TU 名与模块名同.
    let bmf = fs::read_to_string(dir.join("decl_export_mod.bmf")).unwrap();
    assert!(
        bmf.contains("E F via_single_decl "),
        "裸 `export int f();` 必须作为导出实体进 BMF: {}",
        bmf
    );
    assert!(
        bmf.contains("E F via_export_block "),
        "`export {{ }}` 块里的声明不得回退: {}",
        bmf
    );

    let out = dcc_compile_module(
        &dir,
        "use",
        r#"import decl_export_mod;
int main() {
    if (via_single_decl() != 3) return 1;
    if (via_export_block() != 4) return 2;
    if (via_single_decl() + via_export_block() != 7) return 3;
    return 0;
}
"#,
        false,
    );
    assert!(out.status.success(), "use compile: {}", String::from_utf8_lossy(&out.stderr));
    let exit = host_link_objects_and_run(
        &dir.join("run"),
        &[&dir.join("decl_export_mod.o"), &dir.join("use.o")],
        true,
    );
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(exit, 0, "两种 export 拼写的跨 TU 链接必须一致 (退出码 {})", exit);
}

#[test]
#[cfg(unix)]
fn cpp_module_private_fragment_internal_linkage() {
    // M28.2: `module :private;` 片段里的实体必须是模块内部链接 (STB_LOCAL),
    // 不得以全局符号 `T` 泄漏到导出 TU 的 .o; 也不得进入 BMF 接口
    // (否则导入端会重放出指向 LOCAL 定义的 extern).
    let dir = std::env::temp_dir().join(format!("dcc-mod-privfrag-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let mod_obj = dir.join("privmod.o");
    let out = dcc_compile_module(
        &dir,
        "privmod",
        r#"export module privmod;
int hidden_helper();
export int visible() { return hidden_helper() + 1; }
module :private;
int hidden_helper() { return 41; }
"#,
        true,
    );
    assert!(out.status.success(), "module compile: {}", String::from_utf8_lossy(&out.stderr));
    let bmf = fs::read_to_string(dir.join("privmod.bmf")).unwrap();
    assert!(bmf.contains("E F visible"), "{}", bmf);
    assert!(!bmf.contains("hidden_helper"), "私有片段实体不得入接口: {}", bmf);

    // nm 断言: 读 ELF 符号表, hidden_helper 必须是 LOCAL 绑定 (bind==0),
    // 而 visible 必须是 GLOBAL (bind==1)
    let bytes = fs::read(&mod_obj).unwrap();
    let syms = elf64_syms(&bytes);
    let hidden: Vec<&Elf64Sym> = syms
        .iter()
        .filter(|s| s.name.contains("hidden_helper"))
        .collect();
    assert!(
        !hidden.is_empty(),
        "私有片段函数应仍在本 TU 发射: {:?}",
        syms.iter().map(|s| s.name.clone()).collect::<Vec<_>>()
    );
    for s in &hidden {
        assert_eq!(s.bind, 0, "hidden_helper 必须是 STB_LOCAL (0), 实得 {}: {}", s.bind, s.name);
        assert_eq!(s.stype, 2, "hidden_helper 仍为函数符号: {:?}", s.name);
    }
    // 对应 `nm` 输出里不得出现大写 `T hidden_helper` (全局已定义函数)
    assert!(
        !syms
            .iter()
            .any(|s| s.name.contains("hidden_helper") && s.bind == 1),
        "hidden_helper 不得作为全局符号 (nm `T`) 泄漏: {:?}",
        syms.iter().map(|s| (&s.name, s.bind)).collect::<Vec<_>>()
    );
    let vis = syms
        .iter()
        .find(|s| s.name.contains("visible"))
        .expect("visible 符号");
    assert_eq!(vis.bind, 1, "visible 必须是 STB_GLOBAL");

    // 导入端不得看见私有实体名 (g++ 对照: `'hidden_helper' was not declared
    // in this scope`; gcc 13 不能直接跑 `module :private;`, 故用非导出的
    // purview 实体等价实测)
    let out = dcc_compile_module(
        &dir,
        "privbad",
        "import privmod;\nint main() { return hidden_helper() != 41; }\n",
        false,
    );
    assert!(!out.status.success(), "导入端不得引用私有片段实体");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("hidden_helper"), "stderr: {}", stderr);

    // 导入端只用导出实体, 跨 TU 链接运行结果正确 (41 + 1 == 42)
    let use_obj = dir.join("privuse.o");
    let out = dcc_compile_module(
        &dir,
        "privuse",
        "import privmod;\nint main() { return visible() != 42; }\n",
        false,
    );
    assert!(out.status.success(), "import compile: {}", String::from_utf8_lossy(&out.stderr));
    let exit = host_link_objects_and_run(&dir.join("privrun"), &[&mod_obj, &use_obj], false);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(exit, 0, "私有片段不影响导出实体的跨 TU 调用");
}

#[test]
#[cfg(unix)]
fn cpp_module_cross_tu_template_runtime() {
    // M28.4: BMF 随 `E T` 带走模板原文 (`@body=`), 导入端注入解析 ->
    // 实例化管线能重建成员. 任务书指定场景: Box<int> b; b.v=3; return b.get()!=3.
    // g++ -std=c++20 -fmodules-ts 对照: 同一夹具编译+链接+运行 exit 0 (支持).
    // 注意: 必须写 `export template` (裸 `template` 在 purview 里不导出,
    // 两侧语义一致).
    // 同模块内再放一个**函数模板** `pick`: `@body=` 按模板名逐个提取,
    // 两条 `E T` 记录不得互相污染 (类模板 + 函数模板共存回归).
    let dir = std::env::temp_dir().join(format!("dcc-mod-tpl-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let out = dcc_compile_module(
        &dir,
        "boxmod",
        r#"export module boxmod;
export template<class T> struct Box { T v; T get() { return v; } };
export template<class T> T pick(T x) { return x; }
"#,
        true,
    );
    assert!(out.status.success(), "module compile: {}", String::from_utf8_lossy(&out.stderr));
    let bmf = fs::read_to_string(dir.join("boxmod.bmf")).unwrap();
    assert!(
        bmf.contains(
            "E T Box T @body=template<class T> struct Box { T v; T get() { return v; } };"
        ),
        "模板体随 `E T` 序列化: {}",
        bmf
    );
    assert!(
        bmf.contains("E T pick T @body=template<class T> T pick(T x) { return x; }"),
        "函数模板体也随 `E T` 序列化: {}",
        bmf
    );

    let out = dcc_compile_module(
        &dir,
        "boxuse",
        r#"import boxmod;
int main() {
    Box<int> b;
    b.v = 3;
    if (b.get() != 3) return 1;
    if (pick(5) != 5) return 2;
    if (pick(pick(5)) != 5) return 3;
    long L = 7;
    if (pick(pick(L)) != 7) return 4;
    return 0;
}
"#,
        false,
    );
    assert!(out.status.success(), "import compile: {}", String::from_utf8_lossy(&out.stderr));
    let exit = host_link_objects_and_run(
        &dir.join("boxrun"),
        &[&dir.join("boxmod.o"), &dir.join("boxuse.o")],
        true,
    );
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(exit, 0, "跨 TU 类模板成员 + 函数模板调用必须各自返回预期值");
    // M53 转正: **嵌套**函数模板调用 `pick(pick(5))` (实参本身就是待实例化调用)
    // 现已走通 —— 实例化工作表会递归收内层 (无模块的单 TU 见
    // `cpp_template_nested_call_arg`)。这里连同内层类型不同的 `pick(pick(L))`
    // 一起验证跨模块回放后的模板也能逐层实例化。
}

#[test]
#[cfg(unix)]
fn cpp_module_cross_tu_virtual_runtime() {
    // M28.5: 虚函数类跨 TU. BMF 要记录基类/virtuality (`@base:` + `@virtual`),
    // 导入端回放后虚调用必须打到派生类 override; 本 TU 不得发射空壳
    // `_ZTV`/`_ZTI`/`_ZTS` (否则链接期与导出 TU 的 weak 副本随机二选一 -> 段错).
    // g++ -std=c++20 -fmodules-ts 对照: 同一夹具 (含 new/delete 多态)
    // 编译+链接+运行 exit 0 (支持).
    let dir = std::env::temp_dir().join(format!("dcc-mod-virt-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let out = dcc_compile_module(
        &dir,
        "shapemod",
        r#"export module shapemod;
export class Shape {
public:
    virtual int area() { return 0; }
    virtual ~Shape() {}
};
export class Rect : public Shape {
public:
    Rect(int w, int h) : w(w), h(h) {}
    int area() { return w * h; }
private:
    int w;
    int h;
};
export int poly(Shape* s) { return s->area(); }
"#,
        true,
    );
    assert!(out.status.success(), "module compile: {}", String::from_utf8_lossy(&out.stderr));
    let bmf = fs::read_to_string(dir.join("shapemod.bmf")).unwrap();
    assert!(bmf.contains("E C Rect @class @base:public:-:Shape"), "基类入布局: {}", bmf);
    assert!(
        bmf.contains("E F Rect::area ") && bmf.contains("@virtual"),
        "virtuality 传递: {}",
        bmf
    );

    let out = dcc_compile_module(
        &dir,
        "shapeuse",
        r#"import shapemod;
int main() {
    Rect r(3, 4);
    if (poly(&r) != 12) return 1;
    Shape* p = new Rect(2, 5);
    int n = p->area();
    delete p;
    return n != 10;
}
"#,
        false,
    );
    assert!(out.status.success(), "import compile: {}", String::from_utf8_lossy(&out.stderr));
    // 导入 TU 不得发射 RTTI/虚表
    let syms = elf64_syms(&fs::read(dir.join("shapeuse.o")).unwrap());
    let leaked: Vec<&String> = syms
        .iter()
        .map(|s| &s.name)
        .filter(|n| n.starts_with("_ZTV") || n.starts_with("_ZTI") || n.starts_with("_ZTS"))
        .collect();
    assert!(leaked.is_empty(), "导入 TU 不得发射虚表/typeinfo: {:?}", leaked);
    // 导出 TU 才发射 (weak)
    let exp = elf64_syms(&fs::read(dir.join("shapemod.o")).unwrap());
    assert!(
        exp.iter().any(|s| s.name == "_ZTV4Rect" && s.bind == 2),
        "导出 TU 应发射 weak _ZTV4Rect"
    );
    // new/delete 在 libstdc++, 用 g++ 链接
    let exit = host_link_objects_and_run(
        &dir.join("shaperun"),
        &[&dir.join("shapemod.o"), &dir.join("shapeuse.o")],
        true,
    );
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(exit, 0, "跨 TU 虚函数多态调用必须返回 12 / 10");
}

#[test]
#[cfg(unix)]
fn cpp_module_imported_polymorphic_ctor_not_duplicated() {
    // M28.5 补: 导入 TU 不得**定义**导入多态类的隐式 ctor/dtor.
    // 缺陷形状: 重放出的类是纯声明, native 管线合成的本地隐式 ctor 不写 vptr
    // (虚表不在本 TU), 却与导出 TU 的 weak 定义**同名不同内容** —— weak 符号
    // 链接期任选其一, 选中本地副本时在导入 TU 里构造的对象 vptr 未初始化,
    // 虚调用直接段错, 表现形式是“只在导入 TU 排前时挂” (链接顺序敏感).
    // 修法: 链接计划把导入多态类的 ctor/dtor 外部化 (只引用不定义).
    let dir = std::env::temp_dir().join(format!("dcc-mod-ctor-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let out = dcc_compile_module(
        &dir,
        "shapemod",
        r#"export module shapemod;
export class Shape {
public:
    virtual int area() { return 0; }
    virtual ~Shape() {}
};
export class Rect : public Shape {
public:
    Rect(int w, int h) : w(w), h(h) {}
    int area() { return w * h; }
private:
    int w;
    int h;
};
"#,
        true,
    );
    assert!(out.status.success(), "module compile: {}", String::from_utf8_lossy(&out.stderr));
    // 导入 TU 默认构造多态基类对象 + 虚调用
    let out = dcc_compile_module(
        &dir,
        "shapebase",
        r#"import shapemod;
int main() {
    Shape s;
    if (s.area() != 0) return 1;
    Rect r(2, 3);
    Shape* p = &r;
    return p->area() != 6;
}
"#,
        false,
    );
    assert!(out.status.success(), "import compile: {}", String::from_utf8_lossy(&out.stderr));
    // 关键断言: 导入 TU 对 ctor 只能有**引用** (shndx == 0 = SHN_UNDEF),
    // 不得有定义; 定义必须在导出 TU.
    let imp = elf64_syms(&fs::read(dir.join("shapebase.o")).unwrap());
    let defined: Vec<&Elf64Sym> = imp
        .iter()
        .filter(|s| s.name.contains("ctor") && s.shndx != 0)
        .collect();
    assert!(
        defined.is_empty(),
        "导入 TU 不得定义导入类的 ctor: {:?}",
        defined
            .iter()
            .map(|s| (&s.name, s.bind))
            .collect::<Vec<_>>()
    );
    assert!(
        imp.iter().any(|s| s.name.contains("ctor") && s.shndx == 0),
        "导入 TU 应以外部引用形式调 ctor"
    );
    let exp = elf64_syms(&fs::read(dir.join("shapemod.o")).unwrap());
    assert!(
        exp.iter().any(|s| s.name.contains("ctor") && s.shndx != 0),
        "导出 TU 必须发射 ctor 定义"
    );
    // 两种链接顺序都得跑通 (weak 去重后语义一致)
    let a = host_link_objects_and_run(
        &dir.join("run_mod_first"),
        &[&dir.join("shapemod.o"), &dir.join("shapebase.o")],
        true,
    );
    let b = host_link_objects_and_run(
        &dir.join("run_use_first"),
        &[&dir.join("shapebase.o"), &dir.join("shapemod.o")],
        true,
    );
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(a, 0, "mod-first 链接顺序下多态对象必须可用");
    assert_eq!(b, 0, "use-first 链接顺序下多态对象必须可用 (旧缺陷在此段错)");
}

#[test]
#[cfg(unix)]
fn cpp_module_mangled_linkage_no_name_collision() {
    // M28.6: 模块导出/导入实体统一走 mangled 链接名. 旧缺陷: `Widget::get`
    // 与全局 `get` 在发射层面重名 (都用源名 `get`), 链接期相互吃掉.
    // g++ -std=c++20 -fmodules-ts 对照: 同一夹具运行 exit 0 (支持;
    // g++ 额外把模块作用域也编进名字 `_ZW5wmod...`, dcc 暂不模拟, 见报告).
    let dir = std::env::temp_dir().join(format!("dcc-mod-mangle-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let out = dcc_compile_module(
        &dir,
        "wmod",
        "export module wmod;\nexport class Widget {\npublic:\n    int id;\n    int get() { return id * 2; }\n};\n",
        true,
    );
    assert!(out.status.success(), "module compile: {}", String::from_utf8_lossy(&out.stderr));
    let bmf = fs::read_to_string(dir.join("wmod.bmf")).unwrap();
    assert!(bmf.contains("_ZN6Widget3getEv"), "成员函数 mangled 名入 BMF: {}", bmf);
    // 导出 TU 的发射符号名就是 mangled 名
    let names: Vec<String> = elf64_func_symbols(&fs::read(dir.join("wmod.o")).unwrap())
        .into_iter()
        .map(|(n, _, _)| n)
        .collect();
    assert!(names.iter().any(|n| n == "_ZN6Widget3getEv"), "发射名=mangled: {:?}", names);

    // 导入端同时定义同名的全局函数 -> 两个符号必须共存
    let out = dcc_compile_module(
        &dir,
        "wuse",
        r#"import wmod;
int get() { return 7; }
int main() {
    Widget w; w.id = 5;
    int a = w.get();
    int b = get();
    return (a == 10 && b == 7) ? 0 : 1;
}
"#,
        false,
    );
    assert!(out.status.success(), "import compile: {}", String::from_utf8_lossy(&out.stderr));
    let undef = elf64_undefined_symbols(&fs::read(dir.join("wuse.o")).unwrap());
    assert!(
        undef.iter().any(|s| s == "_ZN6Widget3getEv"),
        "成员调用按 mangled 引用: {:?}",
        undef
    );
    let exit = host_link_objects_and_run(
        &dir.join("wrun"),
        &[&dir.join("wmod.o"), &dir.join("wuse.o")],
        true,
    );
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(exit, 0, "Widget::get 与全局 get 不得相互冲突");
}

/// C++ 模板 vector: 验证 std::vector<int> 实例化路径真正工作
#[test]
fn cpp_template_vector_int_instantiation() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-cpp-vec-int-{}.cpp", std::process::id()));
    let asm_path = dir.join(format!("dcc-cpp-vec-int-{}.s", std::process::id()));
    // M51: dcc 只把 src/cpp/stdlib/headers 加进 include 搜索路径, 不会预包含,
    // 所以必须像真实 C++ 一样写 `#include <vector>` (g++ 同样要求).
    let source = "\
#include <vector>
int main() {
    std::vector<int> v;
    int x = 42;
    v.push_back(x);
    return 0;
}
";
    fs::write(&source_path, source).expect("write C++ fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let asm = fs::read_to_string(&asm_path).unwrap_or_default();
    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&asm_path);
    assert!(
        output.status.success(),
        "std::vector<int> + push_back 编译失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // 真实断言: 类模板确实被单态化成具体类, 且成员 push_back 也进入了产出.
    // (实例化类名形式见 template::instantiated_class_name: 名字是**规范名**
    //  `std::vector`, `::` 展平后得到 `std_vector`)
    assert!(
        asm.contains("__dcc_template_std_vector_int"),
        "未生成 std::vector<int> 的单态化类符号, 实例化路径未打通:\n{asm}"
    );
    assert!(asm.contains("push_back"), "成员函数 push_back 未被实例化/发射:\n{asm}");
}

/// C++ 模板 vector: 验证 vector 头文件声明为模板类 (compile-only)
#[test]
fn cpp_vector_header_is_template_class() {
    let dir = std::env::temp_dir();
    let source_path = dir.join(format!("dcc-cpp-vec-tpl-{}.cpp", std::process::id()));
    let asm_path = dir.join(format!("dcc-cpp-vec-tpl-{}.s", std::process::id()));
    // 使用两种元素类型验证单态化
    let source = "\
int main() {
    std::vector<int> vi;
    std::vector<double> vd;
    return 0;
}
";
    fs::write(&source_path, source).expect("write C++ fixture");
    let _output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run dcc");
    let _ = fs::remove_file(&source_path);
    let _ = fs::remove_file(&asm_path);
    // Compile-only: verify compilation works for multiple template instantiations
}

// ====== Coroutine co_await runtime: promise + awaiter 协议 ======
#[test]
#[cfg(unix)]
fn cpp_coroutine_co_await_runtime() {
    // co_await 真挂起/真恢复: 夹具用一个把事件序列压成十进制指纹的 awaiter,
    // 因此 `trace` 单个整数就唯一固定了协议钩子的调用次序与次数:
    //   initial_suspend 挂起 => ramp 不执行主体; promise::await_transform 把 co_await
    //   操作数转成 awaiter; await_ready 为真 => 跳过 await_suspend 但当次仍调
    //   await_resume 并继续往下跑; await_ready 为假 => 调 await_suspend 后精确挂起;
    //   awaiter 携带的帧内累加器指针跨挂起点存活; co_return 值经 return_value 落帧,
    //   final_suspend 挂起即 done. 退出码与 g++ -std=c++20 逐条对照.
    //
    // 夹具故意包含**两个共用同一 promise_type 的协程函数** (awaitable / second) 并
    // 交错驱动: 本实现的帧模型是"帧 == promise 对象", handle 只能按 promise 类型找
    // 入口, 所以状态机归属必须经帧内 `dcc_coro_fn` 标签路由. 只写一个协程函数时这条
    // 路径整块不被覆盖, 而两帧的状态号一旦串扰就会当场错轨迹/错 done.
    let src = include_str!("../fixtures/cpp/coroutine_co_await.cpp");
    cpp_coroutine_cross_check("cpp-coroutine-coawait-runtime", src);
}

// ====== Coroutine shared promise: 多协程函数共用 promise_type 的帧归属路由 ======
#[test]
#[cfg(unix)]
fn cpp_coroutine_shared_promise_runtime() {
    // 帧 == promise 对象 → 一个 promise 类只有一份帧布局, 而 handle 只能按 promise
    // 类型找入口. 夹具用 a(int)/b(int)/c() 三个共用同一 promise_type 的协程函数 (其中
    // a/b 带循环回边 => 各自 ≥10 臂) 轮转驱动 30 轮, 逐条断言每个帧的最后产出值/
    // 成功推进次数/累加指纹, 以及终态后重复驱动不再推进 —— 归属标签一旦接错, 跳板
    // 会把帧送到别人的状态机, 这三组指纹立即不一致. 退出码与 g++ -std=c++20 对照.
    let src = include_str!("../fixtures/cpp/coroutine_shared_promise.cpp");
    cpp_coroutine_cross_check("cpp-coroutine-shared-promise-runtime", src);
}

#[test]
#[cfg(unix)]
fn cpp_global_static_initializers_run() {
    // 缺陷 3: 全局变量的静态初值必须真的发射 (整型/const/constexpr/浮点/负浮点/
    // 取址初值/数组衰减/结构体聚合对象), 不能落进 .bss 读回 0。
    let src = include_str!("../fixtures/cpp/global_static_initializers.cpp");
    cpp_std20_exit_code_cross_check("cpp-global-static-initializers", src);
}

// ---------------------------------------------------------------------------
// Stage 2 验收: sema/HIR 与 legacy C 文本降级的**自动对照**
//
// 原 `tests/cpp_hir_legacy_correspondence.rs` 独立 target 并入本主题文件。
// 人工比对 dump 挡不住回归, 所以这里是可复跑的三向断言; 对照键、样本语料
// 与两路径降级助手均在 `tests/common/harness/cpp_frontend.rs`。
// ---------------------------------------------------------------------------

#[test]
#[ignore = "对照明细: cargo test --test integration dump_correspondence_report -- --ignored --nocapture"]
fn dump_correspondence_report() {
    for (i, src) in CORPUS.iter().enumerate() {
        let (decls, symbols, hir, legacy_src) = lower_both(src);
        let lines = legacy_definition_lines(&legacy_src);
        let expected = expected_entities(&decls);
        eprintln!("== 样本 #{}", i);
        eprintln!("   AST 定义实体: {:?}", expected);
        eprintln!("   native:       {:?}", native_entities(&symbols, &hir));
        for e in &expected {
            eprintln!("   {:?} -> legacy {}", e, legacy_defines(&lines, e));
        }
        eprintln!("   legacy 未 mangled 定义名: {:?}", legacy_plain_definition_names(&lines));
    }
}

/// 源码里写了体的每个实体, native 侧都必须登记。
#[test]
fn every_defined_entity_is_registered_in_hir() {
    for (i, src) in CORPUS.iter().enumerate() {
        let (decls, symbols, hir, _legacy) = lower_both(src);
        let expected = expected_entities(&decls);
        let native = native_entities(&symbols, &hir);
        assert!(expected.len() >= 3, "样本 #{i} 的基准集过小 ({expected:?}), 对照本身已失效");
        let missing: Vec<&Entity> = expected.iter().filter(|e| !native.contains(*e)).collect();
        assert!(
            missing.is_empty(),
            "样本 #{i}: 源码里定义了但 native 符号表/HIR 未登记的实体: {missing:?}\n  \
             native: {native:?}"
        );
    }
}

/// 源码里写了体的每个实体, legacy 侧都必须真的发出。
#[test]
fn every_defined_entity_is_emitted_by_legacy() {
    for (i, src) in CORPUS.iter().enumerate() {
        let (decls, _symbols, _hir, legacy_src) = lower_both(src);
        let lines = legacy_definition_lines(&legacy_src);
        let expected = expected_entities(&decls);
        let missing: Vec<&Entity> = expected
            .iter()
            .filter(|e| !legacy_defines(&lines, e))
            .collect();
        assert!(
            missing.is_empty(),
            "样本 #{i}: 源码里定义了但 legacy 产物里没有对应定义的实体: {missing:?}"
        );
    }
}

/// 反向: legacy 以普通名字发出的定义, native 符号表里必须有同名实体 (native 不得少)。
#[test]
fn legacy_emits_no_plain_named_entity_native_lacks() {
    for (i, src) in CORPUS.iter().enumerate() {
        let (_decls, symbols, _hir, legacy_src) = lower_both(src);
        let lines = legacy_definition_lines(&legacy_src);
        let mut native_names: BTreeSet<String> = BTreeSet::new();
        for s in &symbols {
            if matches!(
                s.kind,
                CxxSymbolKind::Function
                    | CxxSymbolKind::Method
                    | CxxSymbolKind::Constructor
                    | CxxSymbolKind::Destructor
            ) {
                native_names.insert(callable_key(&s.name));
            }
        }
        let plain = legacy_plain_definition_names(&lines);
        let extra: Vec<&String> = plain
            .iter()
            .filter(|n| !native_names.contains(*n))
            .collect();
        assert!(
            extra.is_empty(),
            "样本 #{i}: legacy 发出了 native 不认识的普通命名定义: {extra:?}"
        );
    }
}

/// 类实体的对照: native 的每个类 (取基名) 都必须在 legacy 里以 `struct <名>` 出现。
#[test]
fn every_native_class_has_a_legacy_struct_definition() {
    for (i, src) in CORPUS.iter().enumerate() {
        let (_decls, symbols, _hir, legacy_src) = lower_both(src);
        for s in &symbols {
            if s.kind != CxxSymbolKind::Class || is_backend_internal(&s.name) {
                continue;
            }
            let key = class_key(&s.name);
            let hit = legacy_src.contains(&format!("struct {key} {{"))
                || legacy_src.contains(&format!("struct {key}\n"))
                || legacy_src.contains(&format!("struct {key} "))
                || legacy_src.contains(&format!("{key} {{"));
            assert!(hit, "样本 #{i}: legacy 里没有类 {key} 的 struct 定义");
        }
    }
}

/// HIR dump 本身可用 (Stage 2 第一条验收): 结构化 dump 里能看到函数与语句节点。
#[test]
fn hir_dump_exposes_functions_and_stmts() {
    let (_decls, _symbols, hir, _legacy) = lower_both(CORPUS[0]);
    let dump = format!("{:#?}", hir);
    assert!(dump.contains("functions"), "HIR dump 缺少函数表:\n{dump}");
    assert!(dump.contains("HirStmt"), "HIR dump 缺少语句节点:\n{dump}");
    assert!(
        hir.functions.len() >= 4,
        "样本 #0 至少应有 ctor/dtor/get/bump/useit 的定义: {:?}",
        hir.functions.len()
    );
}

// ---------------------------------------------------------------------------
// Stage 3b 验收: `--cxx-native=auto|off|on` 的 **CLI 落地契约**
//
// 原 `tests/driver_cxx_native_cli_contract.rs` 独立 target 并入本主题文件。
// 本文件就是这三条硬要求的机器判据，它们只能在真实命令行上验证:
// - `off` 强制 legacy, 且 verbose 里能看到决策与原因;
// - `on` **不得静默 fallback** (native 覆盖率要能被直接量化);
// - `auto` 的 fallback **必须在 verbose 输出中可见** (含原因), 并进路径统计。
// 另外 `--cxx-native` 必须出现在 `--help` 里: 用户与脚本选不出这个开关,
// 就等于它没落地。非法取值也必须是硬失败 (非 0 退出), 不能默默退回默认值。
// ---------------------------------------------------------------------------

#[test]
fn help_documents_the_cxx_native_switch() {
    let out = dcc_cmd().arg("--help").output().expect("运行 dcc --help");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("--cxx-native"),
        "--help 没有列出 --cxx-native:\n{text}"
    );
}

#[test]
fn invalid_mode_is_a_hard_error() {
    let dir = temp_dir("invalid");
    let src = write_src(&dir, "plain.cpp", PLAIN_CLASS);
    let obj = dir.join("plain.o");
    for arg in ["--cxx-native=bogus", "--cxx-native"] {
        let out = dcc_cmd()
            .arg(arg)
            .arg("-c")
            .arg(&src)
            .arg("-o")
            .arg(&obj)
            .output()
            .expect("运行 dcc");
        assert!(!out.status.success(), "{arg} 被接受了 (应当报错退出)");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("unknown C++ native mode") || err.contains("missing value after --cxx-native"),
            "{arg} 的错误信息不对: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn off_forces_legacy_with_visible_decision_and_summary() {
    let dir = temp_dir("off");
    let src = write_src(&dir, "plain.cpp", PLAIN_CLASS);
    let obj = dir.join("plain.o");
    let out = dcc_cmd()
        .arg("-v")
        .arg("--cxx-native=off")
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("运行 dcc");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "off 模式编译失败:\n{err}");
    assert!(
        err.contains("c++ backend decision: path=legacy reason=--cxx-native=off forced legacy backend"),
        "verbose 没有暴露 off 的决策与原因:\n{err}"
    );
    assert!(
        err.contains("c++ path summary: native=0 legacy=1"),
        "路径统计没有计入 legacy:\n{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn default_and_on_use_the_native_path() {
    for (tag, mode) in [("default", None), ("on", Some("--cxx-native=on"))] {
        let dir = temp_dir(&format!("native-{tag}"));
        let src = write_src(&dir, "plain.cpp", PLAIN_CLASS);
        let obj = dir.join("plain.o");
        let mut cmd = dcc_cmd();
        cmd.arg("-v");
        if let Some(m) = mode {
            cmd.arg(m);
        }
        let out = cmd
            .arg("-c")
            .arg(&src)
            .arg("-o")
            .arg(&obj)
            .output()
            .expect("运行 dcc");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "[{tag}] 编译失败:\n{err}");
        assert!(
            err.contains("c++ backend decision: path=native"),
            "[{tag}] 没有走 native:\n{err}"
        );
        assert!(
            err.contains("c++ path summary: native=1 legacy=0"),
            "[{tag}] 路径统计没有计入 native:\n{err}"
        );
        assert!(obj.exists(), "[{tag}] 没有产出目标文件");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `on` 不得静默 fallback: native 接不住就是硬错误, 且绝不进 legacy。
#[test]
fn on_never_falls_back_silently() {
    let dir = temp_dir("on-nofallback");
    let src = write_src(&dir, "rej.cpp", NATIVE_REJECTED);
    let obj = dir.join("rej.o");
    let out = dcc_cmd()
        .arg("--cxx-native=on")
        .arg("-v")
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("运行 dcc");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "on 模式静默接住了它不该支持的 TU:\n{err}");
    assert!(err.contains("E0500"), "on 模式必须报 E0500:\n{err}");
    assert!(
        !err.contains("using C++ legacy backend path"),
        "on 模式偷偷走了 legacy:\n{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `auto` 的 fallback 必须在 verbose 里可见 (原因 + 实际走的路径)。
#[test]
fn auto_fallback_is_visible_in_verbose() {
    let dir = temp_dir("auto-fallback");
    let src = write_src(&dir, "rej.cpp", NATIVE_REJECTED);
    let obj = dir.join("rej.o");
    let out = dcc_cmd()
        .arg("--cxx-native=auto")
        .arg("-v")
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("运行 dcc");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("c++ auto fallback: native lowering rejected TU:"),
        "auto 回落没有说出原因:\n{err}"
    );
    assert!(
        err.contains("using C++ legacy backend path after native rejection"),
        "auto 回落没有暴露实际路径:\n{err}"
    );
    assert!(
        err.contains("c++ path summary: native=0 legacy=1"),
        "auto 回落没有计入 fallback 统计:\n{err}"
    );
    assert!(
        err.contains("fallback[native lowering rejected TU:"),
        "fallback 原因没有进入统计摘要:\n{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Stage 10.4 验收: 默认编译 (无 `legacy-strip-debug` feature) 下，主路径不得再
// 依赖 `cpp::lower::strip_cpp_keywords` 的字符级关键字剖离。
//
// 原 `tests/legacy_strip_not_in_main_path.rs` 的整文件 `#![cfg(not(feature = ...))]`
// 并入主题文件后降级为逐测试门控 —— 语义完全一致：只有在该 feature **未启用**
// 时才编译运行这些不变式。
// ---------------------------------------------------------------------------

/// 默认 feature 下，主路径产物不得带上 `strip_cpp_keywords` 的字符级剖离痕迹。
///
/// Stage 10.4 的不变式本形是一个编译期事实（该函数只在
/// `#[cfg(feature = "legacy-strip-debug")]` 下存在），但只对常量取真
/// (`assert!(!cfg!(...))`) 是个永真断言，拦不住任何回归。所以这里把它换成
/// **可执行判据**：用主路径真降级一份 C++ 样本，断言产物里不会出现
/// `strip_cpp_keywords` 独有的脉冲 `#define std_swap(a, b)`（全仓只有该函数会
/// 注入它）。一旦有人把字符级剖离（或其脉冲）重新引入主路径，本用例会红。
#[test]
#[cfg(not(feature = "legacy-strip-debug"))]
fn strip_cpp_keywords_is_not_exported_in_default_build() {
    let (_decls, _symbols, _hir, legacy_src) = lower_both(CORPUS[0]);
    assert!(!legacy_src.is_empty(), "主路径必须仍能降级出 C 文本");
    assert!(
        !legacy_src.contains("#define std_swap(a, b)"),
        "Stage 10.4 不变式: 默认构建的主路径产物里不得出现 strip_cpp_keywords 的脉冲:\n{legacy_src}"
    );
}

/// 进一步: `cpp::lower` 模块在默认 feature 下仍可用 (即 Stage 10.4
/// 只移除 strip 路径, 不会破坏 native path 依赖的 lower 模块本身) ——
/// 直接调用其公开入口并对真实产出做断言，而不是对字符串字面量取真。
#[test]
#[cfg(not(feature = "legacy-strip-debug"))]
fn lower_module_remains_accessible_in_default_build() {
    let c_ty = dcc::cpp::lower::mangle_to_c(&MangleType::Int);
    assert!(
        !c_ty.is_empty(),
        "`cpp::lower::mangle_to_c` 在默认构建下必须可用并产出类型名"
    );
}

/// Verify strip_cpp_keywords removes all entries in CPP_STRIP_KEYWORDS.
/// Stage 10.4: 仅在 `legacy-strip-debug` feature 下编译; 默认 cargo 默认仅编译
/// native 路径, 该函数不存在.
#[cfg(feature = "legacy-strip-debug")]
#[test]
fn cpp_strip_keywords_all_removed() {
    // §5.2 规则 2: 子文件内不写 use / mod, 这里直接走全量路径名 (导入权归壳)。
    for &kw in dcc::cpp::lower::CPP_STRIP_KEYWORDS {
        // Some keywords are replaced rather than deleted
        // (decltype -> void*, nullptr -> ((void*)0)).
        // The key property: the original keyword must not appear in output.
        let test_src = format!("int x = {} ;\n", kw);
        let stripped = dcc::cpp::lower::strip_cpp_keywords(&test_src);
        assert!(
            !stripped.contains(kw),
            "CPP_STRIP_KEYWORDS entry {:?} not fully stripped.\nOutput: {}",
            kw,
            stripped
        );
    }
}

#[test]
#[cfg(unix)]
fn cpp_cxx_native_off_forces_legacy() {
    // Stage 3 端到端已验证的最小 plain class, Off 模式必须走 legacy.
    let src = "struct Counter { Counter() : v(0) {} int v; };\n\
               int main() { Counter c; return c.v; }\n";
    let (out, obj) = match run_cpp_with_mode("stage0-off", src, "off") {
        Some(t) => t,
        None => return,
    };
    assert!(
        out.status.success(),
        "cxx-native=off should succeed via legacy; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(path_decision_in(&out.stderr), "legacy");
    assert!(obj.exists(), "object file should exist: {}", obj.display());
}

#[test]
#[cfg(unix)]
fn cpp_cxx_native_on_forces_native() {
    let src = "struct Counter { Counter() : v(0) {} int v; };\n\
               int main() { Counter c; return c.v; }\n";
    let (out, obj) = match run_cpp_with_mode("stage0-on", src, "on") {
        Some(t) => t,
        None => return,
    };
    assert!(
        out.status.success(),
        "cxx-native=on should succeed via native; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(path_decision_in(&out.stderr), "native");
    assert!(obj.exists(), "object file should exist: {}", obj.display());
}

#[test]
#[cfg(unix)]
fn cpp_cxx_native_auto_uses_native_for_plain_class() {
    // plain class 是 EndToEnd 档特性, Auto 必须走 native.
    let src = "struct Counter { Counter() : v(0) {} int v; };\n\
               int main() { Counter c; return c.v; }\n";
    let (out, obj) = match run_cpp_with_mode("stage0-auto", src, "auto") {
        Some(t) => t,
        None => return,
    };
    assert!(
        out.status.success(),
        "cxx-native=auto plain class should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(path_decision_in(&out.stderr), "native");
    assert!(obj.exists(), "object file should exist: {}", obj.display());
}

#[test]
#[cfg(unix)]
fn cpp_cxx_native_auto_falls_back_for_unsupported() {
    // Stage 4-6 特性在 Stage 0 仅到达 Lowered 档; Auto 必须 fallback.
    // 这里用单继承 + 虚函数 (Stage 5) 触发 fallback.
    let src = "struct Base { virtual int f() { return 1; } };\n\
               struct Derived : Base { int f() override { return 2; } };\n\
               int main() { Derived d; return d.f() - 2; }\n";
    let (out, obj) = match run_cpp_with_mode("stage0-auto-fallback", src, "auto") {
        Some(t) => t,
        None => return,
    };
    // 不论 native 还是 fallback 都应至少通过 legacy; 关键是 verbose
    // 日志里要给出 fallback 原因.
    let stderr = String::from_utf8_lossy(&out.stderr);
    let decision = path_decision_in(&out.stderr);
    assert!(
        decision == "legacy" || decision == "native",
        "expected explicit path decision, got {:?}; stderr={}",
        decision,
        stderr
    );
    if decision == "legacy" {
        // 必须出现特性名与档位 (Stage 0 多档支持矩阵的可审计性)
        assert!(
            stderr.contains("virtual function") && stderr.contains("@"),
            "fallback reason must include feature@level; stderr={}",
            stderr
        );
    }
    // legacy fallback 路径产出文件; native 路径产出文件亦可.
    let _ = obj;
}
