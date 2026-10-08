// included from tests/integration.rs — not a standalone cargo target

// ============================================================================
// Phase 6.2（收尾）：gdb 在 dcc `-g` 的真产物里**读得到**局部变量
//
// 这一组用例是 §11.30/§11.32 那条「位置对拍只在 readelf 层、没有调试器层实读证据」欠账的
// 还账。它盖的是两条完全不同的 DWARF 路线，而且两条都用**反向对照**钉住「不是空转」：
//
//   * `DW_OP_regN`：`register int v asm("rbx")` 的住在活寄存器里。判据不是「读到 42」就算完
//     ——还要单独验一次「把 %rbx 改掉，print v 立刻跟着变」。只会读内存副本的实现也能读到 42，
//     但那条对照会红，因为它读的不是寄存器。
//   * `DW_OP_fbreg`：普通局部 `int t` 住在 `%r13-72`，而 `DW_OP_fbreg: -88` 是**相对 CFA**
//     的。整体偏 8 字节从 readelf 上看不出来（§11.32 写的那条 `cfa_frame_offset` 单源就是为
//     这个），所以判据是「在 gdb 眼里的那个地址上装一个哨兵，再让**程序自己**往同一处写
//     sink 的返回值」：观察点必须报 `Old value = 哨兵 / New value = 1`。这一条同时钉死了
//     调试侧的 CFA 与发射器实际用的栈槽是同一个事实。
//
// 以及 §11.32「仍未做的」里那句「先验一次调试器确实读得到」——验的就是上面两条。
// ============================================================================

/// gdb 那句内部错误的特征串（修复前后各实测过一次：修复前一次 `-batch` 会话刷 86 条）。
const GDB_CU_SYMTAB_ERROR: &str = "in read in CU, but not in symtab";
#[test]
#[cfg(unix)]
fn gdb_reads_dcc_debug_info_locals_through_reg_and_fbreg() {
    if let Some(why) = dwarf_gdb_unavailable() {
        eprintln!("skip gdb_reads_dcc_debug_info_locals_through_reg_and_fbreg: {why}");
        return;
    }
    let (_sf, exe) = dwarf_gdb_build("dwarfgdb", &["-g"], true);

    // ── 主证据：一条会话里同时读到两条路线，且栈槽那条经过「程序自己写」的验证 ──
    let main = gdb_batch(
        &exe,
        &[
            "break sink",
            "run",
            // 回到 f、停在 call 之后：t 的槽还没被写，先由 gdb 灌哨兵（写路径同样是 fbreg）
            "finish",
            "set var t = 23130",
            "printf \"TSET=[%d]\\n\", t",
            "x/1wd &t",
            "watch -l *&t",
            // 程序自己执行 mov %eax,-0x48(%r13) —— 观察点必须在这一条上触发
            "continue",
            "printf \"T=[%d]\\n\", t",
            "printf \"V=[%d]\\n\", v",
            "info locals",
        ],
    );
    assert!(
        !main.contains(GDB_CU_SYMTAB_ERROR),
        "gdb 仍在 CU/symtab 上内部出错（`.debug_info` 的 CU 区间与 `.debug_line` 的地址序 \
         必须共用 `normalize_cu` 那一份口径）：\n{main}"
    );
    assert!(
        main.contains("TSET=[23130]"),
        "gdb 通过 DW_OP_fbreg 写进去的哨兵没能读回来，说明 fbreg 那侧根本不通：\n{main}"
    );
    assert!(
        main.contains("Old value = 23130") && main.contains("New value = 1"),
        "哨兵没被**程序自己的那次 store** 改掉：gdb 眼里的 t（CFA-88）与 dcc 发射的 \
         `-0x48(%r13)` 不是同一个槽（CFA 偏移整体跳 8 字节在 readelf 上看不出来，就是靠这条抓）：\n{main}"
    );
    assert!(
        main.contains("T=[1]"),
        "DW_OP_fbreg 路线没读到 sink 的返回值 1：\n{main}"
    );
    assert!(
        main.contains("V=[42]"),
        "DW_OP_reg 路线没读到 register int v asm(\"rbx\") 的 42：\n{main}"
    );
    assert!(
        main.contains("v = 42") && main.contains("t = 1"),
        "info locals 必须把两个局部都列出来（变量 DIE 的可见范围与位置都得能用）：\n{main}"
    );

    // ── 反向对照 1：`DW_OP_reg` 必须是**活寄存器**，不是内存副本 ──
    // 只断言 `V=[42]` 的话，一个「把寄存器家也发成 fbreg 槽」的实现照样能绿（42 也在槽里）。
    // 改掉 %rbx 后 print v 必须跟着变，才证明位置表达式真是 DW_OP_reg3。
    let reglive = gdb_batch(
        &exe,
        &[
            "break sink",
            "run",
            "finish",
            "printf \"V0=[%d]\\n\", v",
            "set $rbx = 4660",
            "printf \"V1=[%d]\\n\", v",
        ],
    );
    assert!(
        reglive.contains("V0=[42]") && reglive.contains("V1=[4660]"),
        "改了 %rbx 而 print v 不跟着变，说明 v 的位置表达式不是 DW_OP_regN（寄存器家那条路线\
         没接线，退回了内存副本）：\n{reglive}"
    );

    // ── 反向对照 2（还没接线的形状）：形参没有 DW_AT_location，必须**读不到** ──
    // 这条既防空转（如果整个单元都没有调试信息，gdb 会说 "No symbol"，主证据也早该红），
    // 也把当前缺口钉成显式判据：哪天 `param_locations` 接上了，本串会消失，用例逼着来翻。
    let param = gdb_batch(&exe, &["break sink", "run", "print x"]);
    assert!(
        param.contains("$1 = <optimized out>"),
        "形参 x 今天的判据是「读不到」（`$1 = <optimized out>`）。它没出现要么是本用例\
         空转（gdb 压根没拿到 DWARF），要么是形参位置已经接线了 —— 后者请把本判据翻成\
         「读到 0」并同步 §11.32：\n{param}"
    );
    assert!(
        !param.contains("$1 = 0"),
        "形参未接线的形状读出了具体值，说明上面的 optimized out 判据已失效：\n{param}"
    );

    // ── 反向对照 3：出了 f 的作用域就必须读不到（主证据不是「到处都能蒙对」）──
    let outside = gdb_batch(&exe, &["break main", "run", "print v", "print t"]);
    assert!(
        outside.contains("No symbol \"v\" in current context")
            && outside.contains("No symbol \"t\" in current context"),
        "v/t 是 f 的局部，在 main 的作用域里必须不可见；读得到就说明 DIE 的嵌套范围是假的：\n{outside}"
    );

    let _ = fs::remove_file(&exe);
    let _ = fs::remove_file(&_sf);
}

#[test]
#[cfg(unix)]
fn gdb_line_breakpoint_binds_on_line_that_has_a_dwarf_row() {
    if let Some(why) = dwarf_gdb_unavailable() {
        eprintln!("skip gdb_line_breakpoint_binds_on_line_that_has_a_dwarf_row: {why}");
        return;
    }
    let (sf, exe) = dwarf_gdb_build("dwarfgdbline", &["-g"], true);
    let src = sf.to_string_lossy().to_string();
    // 第 2 行是 `int f(void) {`（函数入口行，行号取自 `DW_AT_decl_line`）
    let entry = gdb_batch(
        &exe,
        &[&format!("break {src}:2"), "run", "printf \"HERE=%d\\n\", 7"],
    );
    assert!(
        !entry.contains(GDB_CU_SYMTAB_ERROR),
        "行断点会话里不得出现 CU/symtab 内部错误：\n{entry}"
    );
    assert!(
        entry.contains("Breakpoint 1 at") && entry.contains("f () at") && entry.contains("HERE=7"),
        "行断点 `break <file>:2`（函数入口行）必须绑上、命中 f、并跑到下一条命令：\n{entry}"
    );

    // 语句级行断点（本次接线的主体）。两行的出处不同，必须分别钉住：
    //   * 第 4 行 `int t = sink(0);` 是带初始化的**局部声明**，函数体顶层的
    //     `BlockItem::Decl(Decl::Var)` 在 `emit/functions.rs` 里就地展开、不经过 `gen_stmt`，
    //     只在 `gen_stmt` 里记行号是不够的；
    //   * 第 5 行 `return v + t;` 是普通语句，走 `gen_stmt`。
    // 行表地址若与代码地址不在同一个坐标系（`DW_LNE_set_address` 漏挂 reloc），gdb 会照直
    // 报 `No line N in file`，所以这里同时是那条 reloc 的实读证据。
    let hit4 = gdb_batch(
        &exe,
        &[&format!("break {src}:4"), "run", "printf \"STOP4=%d\\n\", 4"],
    );
    let hit5 = gdb_batch(
        &exe,
        &[&format!("break {src}:5"), "run", "printf \"STOP5=%d\\n\", 5"],
    );
    for (line, tag, out) in
        [(4usize, "局部声明行", &hit4), (5, "普通语句行", &hit5)]
    {
        assert!(
            !out.contains(GDB_CU_SYMTAB_ERROR),
            "第 {line} 行（{tag}）的断点会话里不得出现 CU/symtab 内部错误：\n{out}"
        );
        assert!(
            !out.contains("No line"),
            "第 {line} 行（{tag}）的行断点没绑上：`No line {line} in file` 要么是行表里没有\n\
             这一行（后端没采集 pc↔行号），要么是行表的地址与代码地址不同坐标系\n\
             （`.debug_line` 的 `DW_LNE_set_address` 没挂 reloc）：\n{out}"
        );
        assert!(
            out.contains("f () at") && out.contains(&format!("STOP{line}={line}")),
            "第 {line} 行（{tag}）的断点必须命在 f 里并跑到下一条命令：\n{out}"
        );
    }

    // 「按语句发条目」而不是「整个函数铺一行」的分水岭：三行必须绑到**三个不同**的地址。
    // 少了这条，把函数所有行平铺到同一个 pc 上的假接线也能让上面几条全绿。
    let a2 = gdb_bp_addr(&entry, "入口行");
    let a4 = gdb_bp_addr(&hit4, "第 4 行");
    let a5 = gdb_bp_addr(&hit5, "第 5 行");
    assert!(
        a2 != a4 && a2 != a5 && a4 != a5,
        "行表必须按语句给地址：`break :2`/`:4`/`:5` 绑到了同一批地址（{a2} / {a4} / {a5}），说明条目是平铺的而不是逐语句的：三份会话输出 |{entry}| |{hit4}| |{hit5}|"
    );

    // 反向对照：同一份源**不带 -g** 编出来的产物里，行断点与局部读取都不成立。
    // 这条钉的是「上面的绿来自 dcc 发的 DWARF」而不是 gdb 从 ELF 符号表里凑出来的巧合。
    let (_sf2, exe2) = dwarf_gdb_build("dwarfgdbnolg", &["-O0"], true);
    let nolg = gdb_batch(
        &exe2,
        &[
            &format!("break {src}:5"),
            "run",
            "break f",
            "run",
            "printf \"V=[%d]\\n\", v",
        ],
    );
    assert!(
        nolg.contains("No symbol table is loaded") || nolg.contains("No line 5 in file"),
        "不带 -g 的产物里行断点应当绑不上（否则上面的正判据没有对照）：\n{nolg}"
    );
    assert!(
        nolg.contains("No symbol \"v\" in current context"),
        "不带 -g 的产物里 `v` 这个局部应当不可见：\n{nolg}"
    );
    assert!(
        !nolg.contains("V=[42]"),
        "不带 -g 竟然读出了 v=42 —— 说明用例依赖的不是本包发出的 DWARF：\n{nolg}"
    );

    let _ = fs::remove_file(&exe2);
    let _ = fs::remove_file(&exe);
    let _ = fs::remove_file(&sf);
}

// ============================================================================
// 字节层的那三条不变量（CU 区间 = 全部函数的 min/max、一个函数一条 DIE、区间不得零长）
// 直接在 `-g` 产物上验，不看发射器的内部状态。
// ============================================================================

#[test]
#[cfg(unix)]
fn debug_info_cu_range_spans_all_functions_with_one_die_per_function() {
    if !tool_exists("readelf") {
        eprintln!("skip debug_info_cu_range_spans_all_functions_with_one_die_per_function: readelf not found");
        return;
    }
    let (sf, obj) = dwarf_gdb_build("dwarfstruct", &["-g"], false);
    let dump = readelf_debug_info_dump(&obj);
    let rows = parse_debug_info(&dump);
    let cu = rows
        .iter()
        .find(|r| r.tag == "DW_TAG_compile_unit")
        .expect("没有 DW_TAG_compile_unit");
    let subs: Vec<&DwarfDieRow> = rows
        .iter()
        .filter(|r| r.tag == "DW_TAG_subprogram" && r.low.is_some())
        .collect();

    // 输入形状本身要先成立：`sink` 同时有第 1 行的原型与第 7 行的定义 —— 修复前这里就是
    // 两条同名同址的 DIE（上游 `func_pcs` 按名字取 label，原型与定义拿到同一个 PC）。
    assert!(
        DWARF_GDB_SRC.lines().next().unwrap_or("").starts_with("int sink(int);"),
        "夹具的「原型先于定义」这一形状没了，下面的去重判据就成了空转"
    );
    // 一个函数只能有一条 `DW_TAG_subprogram`。这里不能用「集合恰好等于这三个名字」：
    // 去重失效时多出来的那条**同名** DIE 会被集合吸掉，判据就永远绿。
    let mut names: Vec<&str> = subs.iter().map(|s| s.name.as_str()).collect();
    names.sort();
    assert_eq!(
        names,
        vec!["f", "main", "sink"],
        "一个函数只能有一条 DW_TAG_subprogram（多出来的是原型/定义重复，少掉的是被吞了）"
    );

    // CU 区间必须**覆盖**每一条函数区间（gdb 拿它做「按地址找 CU」的第一道门）
    let cu_lo = cu.low.expect("CU 没有 DW_AT_low_pc");
    let cu_hi = cu.high.expect("CU 没有 DW_AT_high_pc");
    for s in subs.iter() {
        let lo = s.low.unwrap();
        let hi = s.high.unwrap_or(0);
        assert!(hi > 0, "{} 的 DW_AT_high_pc 是零长区间", s.name);
        assert!(
            lo >= cu_lo && lo + hi <= cu_lo + cu_hi,
            "函数 {} 的区间 [{:#x}, {:#x}) 落在 CU 区间 [{:#x}, {:#x}) 之外 —— 这正是 \
             gdb 那句 `Internal error: pc ... in read in CU, but not in symtab.` 的形状",
            s.name, lo, lo + hi, cu_lo, cu_lo + cu_hi
        );
    }

    // 口径必须是 min/max，不是「源码序首/尾」
    let min_lo = subs.iter().map(|s| s.low.unwrap()).min().unwrap();
    let max_hi = subs.iter().map(|s| s.low.unwrap() + s.high.unwrap()).max().unwrap();
    assert_eq!(cu_lo, min_lo, "DW_AT_low_pc 得是所有函数的最小地址");
    assert_eq!(cu_lo + cu_hi, max_hi, "DW_AT_high_pc（常量偏移）的末端得是所有函数的最大地址");

    // 反向对照：本用例必须能区分 min/max 与旧的 first/last。判据 = 「源码序首个函数」
    // （`DW_AT_decl_line` 最小者，这里是只有原型的 sink，行号 1）**不是**地址最小的那个。
    // 一旦夹具的形状变成「源码序 == 地址序」，旧口径也会给出同样的数，本用例就再也抓不到它。
    let first_in_source = subs.iter().min_by_key(|s| s.decl_line.unwrap_or(u64::MAX)).unwrap();
    let first_in_addresses = subs.iter().min_by_key(|s| s.low.unwrap()).unwrap();
    assert_ne!(
        first_in_source.name, first_in_addresses.name,
        "夹具失效：源码序首个函数与地址序首个函数成了同一个，min/max 与旧的 first/last \
         两种口径再也分不开（旧的会给出 {:#x}）",
        first_in_source.low.unwrap()
    );

    // 每条函数都得有 DW_AT_frame_base：`DW_OP_fbreg` 与 `DW_OP_call_frame_cfa` 全靠它落地，
    // 少一条就是 gdb 报 "value requires the DW_TAG_subprogram's frame base" 那一类读不到。
    for s in subs.iter() {
        assert!(s.frame_base, "{} 没有 DW_AT_frame_base", s.name);
    }

    let _ = fs::remove_file(&obj);
    let _ = fs::remove_file(&sf);
}

// ============================================================================
// `-g -O2`：函数级并行发射与「放开寄存器家」在同一档上交汇
//
// 两件必须同时成立的事：
//   * 语句行条目是 **worker 侧**的 Codegen 采集的，而 `.debug_line` 只由主 Codegen 拼装。
//     合并期不带回那三张表，`-g -O0`/`-O1` 有语句行、`-g -O2`（`cli.rs` 的 `opt >= 2`
//     默认走并行）整张表退化成每函数一行 —— `break file.c:N` 全部报 `No line N in file`。
//     与 N1/N12/debug_var_facts 同一族：函数级事实只存在于 worker，没有回程。
//   * Phase 6.2 取消 `-g` 禁寄存器家后，`-g -O2` 里连 `register asm` 都没写的普通局部
//     （本夹具的 `acc`/`i`）也会拿到 `DW_OP_regN`。这里同时验「它是活寄存器」：改掉
//     那个号，`print acc` 必须跟着变 —— 只会发栈槽位置表达式也能让 `print` 读到旧值，
//     但那条对照会红。
// ============================================================================

/// 与 `DWARF_GDB_SRC` 的分工：那份钉**显式** `register asm`（号由源码指定，与档位无关），
/// 这份钉**自动**绑定（只有准入判据 + 档位到 `-O2` 才有），而且全文不含 `asm(` —— 否则
/// `DW_OP_reg` 的出处就成了源码形状而不是分配器。热循环让准入判据成立，三个函数让
/// 并行那条路真的被走到（门槛是 `top_funcs.len() >= 2`）。
const DWARF_GDB_O2_SRC: &str = r#"int sink(int);
long s(long n) {
    long acc = 0;
    for (long i = 0; i < n; i++) acc += i;
    return acc;
}
int sink(int x) { return x + 1; }
int main(void) { return (int)(s(5) % 7); }
"#;

#[test]
#[cfg(unix)]
fn gdb_parallel_o2_keeps_statement_rows_and_register_home() {
    if let Some(why) = dwarf_gdb_unavailable() {
        eprintln!("skip gdb_parallel_o2_keeps_statement_rows_and_register_home: {why}");
        return;
    }
    // 前置形状：这两条不成立时，下面的判据全部会变成「捞不到东西」而不是「红」。
    assert!(
        !DWARF_GDB_O2_SRC.contains("asm("),
        "夹具里出现了显式 register asm，DW_OP_reg 的出处就分不开是谁给的了"
    );

    let (sf, exe) = dwarf_gdb_build_env("dwarfgdbo2", DWARF_GDB_O2_SRC, &["-g", "-O2"], true, &[]);
    let src = sf.to_string_lossy().to_string();
    let dump = readelf_debug_info_dump(&exe);
    let rows = parse_debug_info(&dump);
    let subs: Vec<&DwarfDieRow> = rows
        .iter()
        .filter(|r| r.tag == "DW_TAG_subprogram" && r.low.is_some())
        .collect();
    assert!(
        subs.len() >= 2,
        "并行发射的门槛是「至少两个函数」，本产物只有 {} 条函数 DIE ⇒ 并行那条路压根没被走到，本用例就捞不到东西：{dump}",
        subs.len()
    );

    // ── 正判据 1：并行 `-O2` 产物里第 4 行（循环那条语句）必须有行、必须绑上、命在 s，
    // 而且 gdb 打出来的源码必须就是那一行的原文（行表地址与代码地址必须在同一坐标系）。
    // 修复前这里直接是 `No line 4 in file`：并行发射把 worker 采的行号表丢了。
    let par = gdb_batch(
        &exe,
        &[&format!("break {src}:4"), "run", r#"printf "PAR=1\n""#],
    );
    assert!(
        !par.contains(GDB_CU_SYMTAB_ERROR),
        "并行 `-g -O2` 产物里行断点会话又刷出了 CU/symtab 内部错误：{par}"
    );
    assert!(
        !par.contains("No line"),
        "并行 `-g -O2` 产物里 `break :4` 绑不上：worker 采集的行号条目没在合并期带回主 \nCodegen（`.debug_line` 只剩每函数一行）：{par}"
    );
    assert!(
        par.contains("s (")
            && par.contains("for (long i = 0; i < n; i++) acc += i;")
            && par.contains("PAR=1"),
        "第 4 行的断点必须命在 s 里、打出该行原文、并跑到下一条命令：{par}"
    );

    // 绑上的地址必须在 s 之内、而且**不是入口**：兜底表只有「每函数一行」，那一行正好落在
    // low_pc 上，光看「绑上了」会被它蒙过去。
    let s_low = subs
        .iter()
        .find(|r| r.name == "s")
        .and_then(|r| r.low)
        .expect("产物里没有 s 的 DW_AT_low_pc");
    let a4 = u64::from_str_radix(&gdb_bp_addr(&par, "-O2 并行第 4 行"), 16)
        .expect("并行那次的断点地址不是十六进制");
    assert!(
        a4 > s_low,
        "`break :4` 停在函数入口 {s_low:#x} 上 —— 拿到的仍是每函数一行的兜底表，\n不是语句行：{par}"
    );

    // ── 对照：同一行在**串行** `-O2` 里必须给出同一个函数内偏移（并行与串行只能有
    // 同一个事实的两种实现）。少了它，「按 chunk 落位基址平移 PC」写成不平移、
    // 或者平移到错的基址，上面几条仍然会绿。
    let (sf_ser, ser) = dwarf_gdb_build_env(
        "dwarfgdbo2ser",
        DWARF_GDB_O2_SRC,
        &["-g", "-O2"],
        true,
        &[("DCC_NO_PARALLEL", "1")],
    );
    // 断点要用**串行那份自己的**源文件名：每份产物的 `DW_AT_name` 就是它自己的那个路径，
    // 拿并行那份的名字去约串行产物，gdb 会说 `No source file named ...`。
    let src_ser = sf_ser.to_string_lossy().to_string();
    let srows = parse_debug_info(&readelf_debug_info_dump(&ser));
    let ser_low = srows
        .iter()
        .find(|r| r.tag == "DW_TAG_subprogram" && r.name == "s")
        .and_then(|r| r.low)
        .expect("串行产物里没有 s 的 low_pc");
    let sed = gdb_batch(
        &ser,
        &[&format!("break {src_ser}:4"), "run", r#"printf "SER=1\n""#],
    );
    assert!(
        sed.contains("for (long i = 0; i < n; i++) acc += i;") && sed.contains("SER=1"),
        "串行 `-g -O2` 里同一行必须也绑得上并命中（并行与串行不一致就是两套事实）：{sed}"
    );
    let a4s =
        u64::from_str_radix(&gdb_bp_addr(&sed, "-O2 串行第 4 行"), 16).expect("串行断点地址");
    assert_eq!(
        a4 - s_low,
        a4s - ser_low,
        "并行与串行给出的语句行不是同一个函数内偏移：并行 {a4:#x} 入口 {s_low:#x}，\n串行 {a4s:#x} 入口 {ser_low:#x}"
    );

    // ── 正判据 2：`-g -O2` 下**没写 register asm** 的普通局部也必须拿到 `DW_OP_regN`。
    // 这就是取消 `-g` 禁寄存器家（旧拒因 `debug-info-forbids-register-home`）的产物证据；
    // 本夹具全文不含 `asm(`，所以这里的 `DW_OP_reg` 只能来自分配器。
    let acc = var_location(&rows, "acc").expect("并行 `-g -O2` 产物里 acc 没有 DW_AT_location");
    let reg = dwarf_reg_name(&acc)
        .unwrap_or_else(|| panic!("acc 的位置表达式不是 DW_OP_regN（寄存器家没被 DWARF 跟上）：{acc}"));

    // 反向对照（档位对照）：同一份源在 `-O1` 下不跑这条分配 ⇒ acc 必须是 `DW_OP_fbreg`。
    // 少了它，`DW_OP_reg` 就成了「永远成立」的字符串，本判据抓不到「禁制被取消」这件事。
    let (_sf3, o1) = dwarf_gdb_build_env("dwarfgdbo1", DWARF_GDB_O2_SRC, &["-g", "-O1"], true, &[]);
    let o1rows = parse_debug_info(&readelf_debug_info_dump(&o1));
    let acc1 = var_location(&o1rows, "acc").expect("`-O1` 产物里 acc 没有 DW_AT_location");
    assert!(
        acc1.contains("DW_OP_fbreg") && !acc1.contains("DW_OP_reg"),
        "`-O1` 下 acc 已经是寄存器家了 ⇒ 上面那条 `DW_OP_reg` 的对照没了（本用例再也\n分不开「因为取消了禁制」与「本来人人都绑」）：{acc1}"
    );

    // ── 正判据 3：那个号是**活寄存器**，不是内存副本。
    // 只断言 `print acc` 得到一个数不够：一个「把寄存器家也发成 fbreg 槽」的实现照样能
    // 读到旧值。改掉寄存器本身后 print 必须跟着变，才证明位置表达式真是 `DW_OP_regN`。
    let live = gdb_batch(
        &exe,
        &[
            &format!("break {src}:4"),
            "run",
            &format!("set ${reg} = 4660"),
            &format!(r#"printf "ACC=[%d]\n", acc"#),
        ],
    );
    assert!(
        live.contains("ACC=[4660]"),
        "改了 {reg} 而 `print acc` 不跟着变 ⇒ acc 的位置表达式不是 `DW_OP_regN`（退回了\n内存副本）：{live}"
    );

    // ── 反向对照（还没接线的形状）：`-O2` 下形参仍然没有 `DW_AT_location`，必须读不到。
    // 这条既防空转（整个单元没有调试信息时 gdb 会说 No symbol，正判据早该红），
    // 也把当前缺口钉成显式判据：哪天形参位置接上了，本串会消失，用例逼着来翻。
    let param = gdb_batch(&exe, &[&format!("break {src}:4"), "run", "print n"]);
    assert!(
        param.contains("$1 = <optimized out>"),
        "形参 n 今天的判据是「读不到」（`$1 = <optimized out>`）。它没出现要么是本用例\n空转，要么是形参位置已接线 —— 后者请把本判据翻成「读到 5」并同步 §11.32：{param}"
    );
    assert!(
        !param.contains("$1 = 5"),
        "形参未接线的形状读出了具体值，说明上面的 optimized out 判据已失效：{param}"
    );

    for p in [&sf_ser, &o1, &ser, &exe, &sf] {
        let _ = fs::remove_file(p);
    }
}
