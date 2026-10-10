// included from tests/integration.rs — not a standalone cargo target

/// x86_64 va_arg GP 寄存器路径 (1-5 变参全部在 GP 寄存器).
/// 修复前 overflow 偏移多 8 字节, 5+ 变参会读到错误参数.
#[test]
fn va_arg_x86_64_gp_path() {
    let src = VA_LIST_TYPEDEF.to_string()
        + "int sum_int5(int n, ...) {\n\
           va_list ap;\n\
           __builtin_va_start(ap, n);\n\
           int s = 0;\n\
           for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, int);\n\
           __builtin_va_end(ap);\n\
           return s;\n\
           }\n\
           int main(void) { return sum_int5(2, 10, 20) - 30; }\n";
    let (output, exe) = run_compile_and_run("va-arg-gp", &src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "expected sum_int5(2, 10, 20) == 30");
}

/// x86_64 va_arg 溢出栈路径 (6+ 变参走 overflow_arg_area).
/// 修复前 overflow_off = frame_size + 16 错为 16, 现已对齐到
/// SysV ABI: r13 + 16 = caller 栈上第 1 个变参.
#[test]
fn va_arg_x86_64_overflow_path() {
    let src = VA_LIST_TYPEDEF.to_string()
        + "int sum_int13(int n, ...) {\n\
           va_list ap;\n\
           __builtin_va_start(ap, n);\n\
           int s = 0;\n\
           for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, int);\n\
           __builtin_va_end(ap);\n\
           return s;\n\
           }\n\
           int main(void) { return sum_int13(13, 1,2,3,4,5,6,7,8,9,10,11,12,13) - 91; }\n";
    let (output, exe) = run_compile_and_run("va-arg-overflow", &src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "expected sum_int13(13, 1..13) == 91");
}

/// x86_64 va_arg 浮点路径 (XMM reg + 溢出栈 8 字节步进).
/// 修复前 overflow 步进用 16 (XMM 槽位) 而非 8 (栈上 double).
#[test]
fn va_arg_x86_64_double_path() {
    let src = VA_LIST_TYPEDEF.to_string()
        + "double sum_double5(int n, ...) {\n\
           va_list ap;\n\
           __builtin_va_start(ap, n);\n\
           double s = 0.0;\n\
           for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, double);\n\
           __builtin_va_end(ap);\n\
           return s;\n\
           }\n\
           int main(void) {\n\
               double r = sum_double5(3, 1.0, 2.0, 3.0);\n\
               return ((int)r) - 6;\n\
           }\n";
    let (output, exe) = run_compile_and_run("va-arg-double", &src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "expected sum_double5(3, 1.0,2.0,3.0) == 6.0");
}

/// x86_64 va_arg 浮点溢出栈 (4 个 XMM 用完后, 第 5+ 走栈).
/// overflow 步进 8 字节, 验证 sum_double8 = 36.0.
#[test]
fn va_arg_x86_64_double_overflow() {
    let src = VA_LIST_TYPEDEF.to_string()
        + "double sum_double8(int n, ...) {\n\
           va_list ap;\n\
           __builtin_va_start(ap, n);\n\
           double s = 0.0;\n\
           for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, double);\n\
           __builtin_va_end(ap);\n\
           return s;\n\
           }\n\
           int main(void) {\n\
               double r = sum_double8(8, 1.0,2.0,3.0,4.0,5.0,6.0,7.0,8.0);\n\
               return ((int)r) - 36;\n\
           }\n";
    let (output, exe) = run_compile_and_run("va-arg-double-overflow", &src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "expected sum_double8(8, 1..8) == 36.0");
}

/// x86_64 va_arg 无外部符号 — overflow 路径直接读栈, 无需 libc.
#[test]
fn va_arg_x86_64_no_external_symbols() {
    let source = VA_LIST_TYPEDEF.to_string()
        + "int sum_int5(int n, ...) {\n\
           va_list ap;\n\
           __builtin_va_start(ap, n);\n\
           int s = 0;\n\
           for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, int);\n\
           __builtin_va_end(ap);\n\
           return s;\n\
           }\n";
    let (output, object_path) = run_object_test("va-arg-noext", &source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj = fs::read(&object_path).unwrap_or_default();
    let _ = fs::remove_file(&object_path);
    for forbidden in &["__va_", "__builtin_va_", "va_start", "va_arg", "va_end"] {
        let needle = forbidden.as_bytes();
        assert!(
            !obj.windows(needle.len()).any(|w| w == needle),
            "object should not reference libc/internal va symbols ({forbidden})"
        );
    }
}

/// x86_64 va_arg fixture 包含 sum_int5/sum_int13/sum_long5/sum_double5/sum_double8.
#[test]
fn va_arg_x86_64_fixture_compiles() {
    let src = include_str!("../fixtures/c/va_arg.c");
    let (output, object_path) = run_object_test("va-arg-fixture", src);
    assert!(
        output.status.success(),
        "fixture compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let obj = fs::read(&object_path).unwrap_or_default();
    let _ = fs::remove_file(&object_path);
    for fname in &[
        "sum_int5",
        "sum_int13",
        "sum_long5",
        "sum_double5",
        "sum_double8",
        "sum_ld3",
    ] {
        let needle = fname.as_bytes();
        assert!(obj.windows(needle.len()).any(|w| w == needle), "fixture should define {fname}");
    }
}

/// x86_64 va_arg long double: 80-bit x87 扩展精度,
/// SysV ABI 要求 16 字节对齐 + fldt/fstpt 读写。
/// 验证调用方以 80-bit 格式传递, 被调方以 fldt 读取。
#[test]
fn va_arg_x86_64_long_double() {
    let src = VA_LIST_TYPEDEF.to_string()
        + "long double sum_ld3(int n, ...) {\n\
           va_list ap;\n\
           __builtin_va_start(ap, n);\n\
           long double s = 0.0L;\n\
           for (int i = 0; i < n; i++)\n\
               s += __builtin_va_arg(ap, long double);\n\
           __builtin_va_end(ap);\n\
           return s;\n\
           }\n\
           int main(void) {\n\
           long double r = sum_ld3(3, 1.0L, 2.0L, 3.0L);\n\
           long long v = (long long)(r + 0.5L);\n\
           return (int)(v - 6);\n\
           }\n";
    let (output, exe) = run_compile_and_run("va-arg-long-double", &src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "expected sum_ld3(3, 1.0L, 2.0L, 3.0L) == 6.0");
}

/// va_list 作为函数参数传递 (指针退化): 辅助函数通过
/// va_list 形参读取可变参数.
#[test]
fn va_list_as_function_param() {
    let src = VA_LIST_TYPEDEF.to_string()
        + "int sum_via_ap(int n, va_list ap) {\n\
           int s = 0;\n\
           for (int i = 0; i < n; i++)\n\
               s += __builtin_va_arg(ap, int);\n\
           return s;\n\
           }\n\
           int caller(int n, ...) {\n\
           va_list ap;\n\
           __builtin_va_start(ap, n);\n\
           int r = sum_via_ap(n, ap);\n\
           __builtin_va_end(ap);\n\
           return r;\n\
           }\n\
           int main(void) {\n\
           return caller(4, 1, 2, 3, 4) - 10;\n\
           }\n";
    let (output, exe) = run_compile_and_run("va-list-param", &src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "va_list as function param failed");
}

/// va_list 指针解引用 (*pap): 通过指向 va_list 的指针访问.
#[test]
fn va_list_pointer_deref() {
    let src = VA_LIST_TYPEDEF.to_string()
        + "int caller(int n, ...) {\n\
           va_list ap;\n\
           __builtin_va_start(ap, n);\n\
           va_list *pap = &ap;\n\
           int s = 0;\n\
           for (int i = 0; i < n; i++)\n\
               s += __builtin_va_arg(*pap, int);\n\
           __builtin_va_end(ap);\n\
           return s;\n\
           }\n\
           int main(void) {\n\
           return caller(3, 1, 2, 3) - 6;\n\
           }\n";
    let (output, exe) = run_compile_and_run("va-list-ptr-deref", &src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "va_list pointer deref failed");
}

/// va_list 结构体成员 (obj.ap): va_list 作为结构体字段.
#[test]
fn va_list_struct_member() {
    let src = VA_LIST_TYPEDEF.to_string()
        + "struct S { int x; va_list ap; };\n\
           int caller(int n, ...) {\n\
           struct S obj;\n\
           obj.x = 0;\n\
           __builtin_va_start(obj.ap, n);\n\
           int s = 0;\n\
           for (int i = 0; i < n; i++)\n\
               s += __builtin_va_arg(obj.ap, int);\n\
           __builtin_va_end(obj.ap);\n\
           return s;\n\
           }\n\
           int main(void) {\n\
           return caller(3, 1, 2, 3) - 6;\n\
           }\n";
    let (output, exe) = run_compile_and_run("va-list-member", &src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "va_list struct member failed");
}

/// va_copy 独立性: 复制后两个 va_list 应独立推进.
#[test]
fn va_arg_va_copy_independence() {
    let src = include_str!("../fixtures/c/va_arg/va_copy.c");
    let (output, exe) = run_compile_and_run("va-copy-indep", src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "va_copy independence failed");
}

/// 含 int+double 的结构体 va_arg.
#[test]
fn va_arg_struct_mixed() {
    let src = include_str!("../fixtures/c/va_arg/struct_mixed.c");
    let (output, exe) = run_compile_and_run("va-struct-mixed", src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "struct mixed va_arg failed");
}

/// long double va_arg + 含 long double 的结构体.
#[test]
fn va_arg_long_double_struct() {
    let src = include_str!("../fixtures/c/va_arg/long_double.c");
    let (output, exe) = run_compile_and_run("va-long-double-struct", src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "long double struct va_arg failed");
}

/// va_list 作为函数参数传递.
#[test]
fn va_arg_list_param_fixture() {
    let src = include_str!("../fixtures/c/va_arg/va_list_param.c");
    let (output, exe) = run_compile_and_run("va-list-param-fix", src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "va_list param fixture failed");
}

/// va_arg(ap, struct S) 直接聚合读取
#[test]
fn va_arg_aggregate_direct() {
    let src = include_str!("../fixtures/c/va_arg/aggregate_va_arg.c");
    let (output, exe) = run_compile_and_run("va-agg-direct", src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "aggregate va_arg fixture failed");
}

/// D3: `fp_offset` 的上限与推进步长必须按**实际占用的 XMM 寄存器个数**算.
/// 按 SSE 家族 eightbyte 个数算会把 `[Sse, SseUp]` 当作 2 个寄存器, 上限从 160
/// 缩到 144, 于是第 8 个向量参数被误判溢出而去 overflow_arg_area 取.
#[test]
fn va_arg_xmm_reg_count_not_eightbyte_count() {
    let src = include_str!("../fixtures/c/va_arg/xmm_reg_count_not_eightbyte_count.c");
    let code = run_gcc_cross_check("va-xmm-reg-count", src);
    assert_eq!(code, 0, "XMM 寄存器计数判据 fixture failed (返回值为首个读错的序号)");
}

/// 对齐 **大于 16 字节** 的对象（`vector_size(32)` 的 v4df/v8sf、
/// `aligned(32)` 的结构）必须整体走内存（SysV ABI 3.2.3p2 + GCC 4.6 的 ABI
/// 变更: `classify_argument` 里 `if (align > 128) return CLASS_MEMORY`）。
/// 分类错会把高 16 字节永久丢失。
#[test]
fn va_arg_align32_vector_memory() {
    let src = include_str!("../fixtures/c/va_arg/align32_vector_mem.c");
    let code = run_gcc_cross_check("va-align32-mem", src);
    assert_eq!(code, 0, ">16 字节对齐参数整体走内存 fixture failed");
}

/// D1 + D4: 通过已退化为指针的 `va_list` 形参读聚合变参, 并按值返回.
/// D1 = 指针变体把 reg_save_area 装进 r9 却让共用的装载体用 rcx 当基址
/// (缺 REX.B), 读到 va_list 结构体自身的栈垃圾; D4 = `return va_arg(ap, struct S)`
/// 走寄存器返回时 rax 只是缓冲区地址, 不装载内容则调用方把地址当数据.
#[test]
fn va_arg_agg_via_va_list_param() {
    let src = include_str!("../fixtures/c/va_arg/agg_via_va_list_param.c");
    let (output, exe) = run_compile_and_run("va-agg-via-param", src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "指针型 va_list 参数读聚合 fixture failed");
}

/// D5: `va_arg(ap, __int128)` 在 `gp_offset == 40`（只剩 8 字节）时不得被拆分，
/// `gp_offset` 也不得推进——GCC 真值是整体上溢出区。旧实现多一条 GCC 里不存在
/// 的 hybrid 路径且无条件 `+= 16`.
#[test]
fn va_arg_int128_gp_boundary_no_split() {
    let src = include_str!("../fixtures/c/va_arg/int128_gp_boundary_no_split.c");
    let (output, exe) = run_compile_and_run("va-i128-boundary", src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "__int128 GP 边界不拆分 fixture failed");
}

/// D6: `gen_va_arg_ptr` 曾完全没有 `__int128` 分支，16 字节标量落回单 8 字节
/// 通用路径（只取到也只推进低半）.
#[test]
fn va_arg_int128_via_va_list_param() {
    let src = include_str!("../fixtures/c/va_arg/int128_via_va_list_param.c");
    let (output, exe) = run_compile_and_run("va-i128-via-param", src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "指针型 va_list 上 __int128 fixture failed");
}

/// D3 落点 + D5 边界：全局 `va_list` 跨函数取 `__int128`。
/// `va_start` 写真全局内存而 `va_arg` 读函数私有懒分配栈槽时，四个参数全错.
#[test]
fn va_arg_int128_global_valist() {
    let src = include_str!("../fixtures/c/va_arg/int128_global_valist.c");
    let (output, exe) = run_compile_and_run("va-i128-global", src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "全局 va_list + __int128 fixture failed");
}

/// D7: 循环体内的 `__int128` 局部量必须整体 16 字节落槽.
/// `mov_to_stack` 对 sz >= 8 只发一个 8 字节 store, 高半(rdx)从未写入变量槽,
/// 于是 `v >> 64` 读到栈残值——实测三次迭代全部返回最后一次的高半.
#[test]
fn va_arg_int128_loop_local_full_width() {
    let src = include_str!("../fixtures/c/va_arg/int128_loop_local_full_width.c");
    let (output, exe) = run_compile_and_run("va-i128-fullwidth", src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(
        code, 0,
        "__int128 循局部量全宽落槽 fixture failed (每 2 位 = 一组首个读错的下标+1)"
    );
}

/// va_copy 高级形式: 结构体成员 + 指针解引用
#[test]
fn va_copy_advanced_forms() {
    let src = include_str!("../fixtures/c/va_arg/va_copy_advanced.c");
    let (output, exe) = run_compile_and_run("va-copy-adv", src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "va_copy advanced forms fixture failed");
}

/// double 精度验证: va_arg(ap, double) 不被截断为 float
#[test]
fn va_arg_double_precision() {
    let source = r#"
typedef __builtin_va_list va_list;
double sum_d(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    double s = 0.0;
    for (int i = 0; i < n; i++)
        s += __builtin_va_arg(ap, double);
    __builtin_va_end(ap);
    return s;
}
int main(void) {
    /* 验证精度: 1.1 + 2.2 + 3.3 = 6.6 */
    double r = sum_d(3, 1.1, 2.2, 3.3);
    int err = (int)(r * 1000.0) - 6600;
    if (err < 0) err = -err;
    return err > 1 ? 1 : 0;
}
"#;
    let (output, exe) = run_compile_and_run("va-dbl-prec", source);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rc = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(rc, 0, "double precision should return 0, got {rc}");
}

/// QEMU runtime: va_arg — variadic function with integer args
#[test]
fn non_x86_qemu_runtime_va_arg() {
    let source = r#"
typedef __builtin_va_list va_list;
#define va_start(ap, last) __builtin_va_start(ap, last)
#define va_arg(ap, type)   __builtin_va_arg(ap, type)
#define va_end(ap)         __builtin_va_end(ap)

int sum_n(int n, ...) {
    va_list ap;
    va_start(ap, n);
    int total = 0;
    for (int i = 0; i < n; i++) {
        total += va_arg(ap, int);
    }
    va_end(ap);
    return total;
}
int main(void) {
    /* 3+7+11+13+19 = 53, return 53 - 11 = 42 */
    int r = sum_n(5, 3, 7, 11, 13, 19);
    return r - 11;
}
"#;
    let targets = [
        ("aarch64-linux-gnu", "qemu-aarch64"),
        ("riscv64-linux-gnu", "qemu-riscv64"),
        ("mips-linux-gnu", "qemu-mips"),
        ("mips64-linux-gnuabi64", "qemu-mips64"),
        ("arm-linux-gnueabihf", "qemu-arm"),
    ];
    let mut runnable = 0;
    for (target, qemu) in targets {
        if qemu_runtime_target("qemu-vaarg", target, qemu, 42, source).is_some() {
            runnable += 1;
        }
    }
    if runnable == 0 {
        eprintln!("skip QEMU va_arg matrix: no qemu-user binaries found");
    }
}

/// SysV va_list GP + SSE 浮点路径编译通过。
#[test]
fn va_list_sse_compiles() {
    let source = include_str!("../../tests/fixtures/smoke/va_list.c");
    let (output, obj) = run_object_test("va-list-sse", source);
    let _ = fs::remove_file(obj);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
}

/// D2: `va_arg(ap, __m128d)` 的两个 eightbyte ([Sse, SseUp]) 必须共享同一个
/// 16 字节 XMM 槽, 且装载结果必须被消费侧按「rax = 聚合地址」约定原样接收.
/// 消费侧曾从 `vector_temp_base(0)` 取源 (与装载器写的 `alloc_sret_slot` 槽
/// 差一个 +600 常量偏移), 把已填好的值用错误源覆盖, 两个向量全读错.
#[test]
fn va_arg_sseup_shares_one_slot() {
    let src = include_str!("../fixtures/c/va_arg/sseup_shares_one_slot.c");
    let (output, exe) = run_compile_and_run("va-sseup-slot", src);
    assert!(
        output.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let code = run_exe(&exe);
    let _ = fs::remove_file(exe);
    assert_eq!(code, 0, "[Sse, SseUp] 共享单槽 fixture failed");
}

/// D3 第二半: `[Sse, Sse]`（`struct {double;double;}`, GCC 实测用 xmm0+xmm1）与
/// `[Sse, SseUp]`（同一槽的低/高半）槽布局不同, 不能按奇偶统一近似.
#[test]
fn va_arg_sse_vs_sseup_slot_layout() {
    let src = include_str!("../fixtures/c/va_arg/sse_vs_sseup_slot_layout.c");
    let code = run_gcc_cross_check("va-sse-slot-layout", src);
    assert_eq!(
        code, 0,
        "[Sse,Sse] vs [Sse,SseUp] 槽布局 fixture failed (返回值为首个读错的序号)"
    );
}

/// D3 第三半（新增边界用例）: 命名 `va_list` 上把 double / `struct{double,double}`
/// / `__m128d` / `v4sf` / `struct{int,double}` / `struct{double,double,double}`
/// / `long double` / `int` 混成一条长流, 逐值与 gcc 对照。
///
/// 单一类型的夹具只能证明"那种类型自洽"; 游标错位必须在混合流里才累积暴露。
#[test]
fn va_arg_mixed_sse_pipeline_named() {
    let src = include_str!("../fixtures/c/va_arg/mixed_sse_pipeline_named.c");
    let code = run_gcc_cross_check("va-mixed-sse-named", src);
    assert_eq!(code, 0, "命名 va_list 混合 SSE 流水线 fixture failed");
}

/// 同一批混合 SSE 类对象改经**已退化为指针**的 `va_list` 形参读取。
/// 三种 `va_list` 落点必须共用同一份 ABI 判定（历史上它们各持一份副本, 单点
/// 修复会在另一份里静默失效）。
#[test]
fn va_arg_mixed_sse_via_valist_param() {
    let src = include_str!("../fixtures/c/va_arg/mixed_sse_via_valist_param.c");
    let code = run_gcc_cross_check("va-mixed-sse-ptr", src);
    assert_eq!(code, 0, "指针落点 va_list 混合 SSE 聚合 fixture failed");
}

/// 同一批混合 SSE 类对象改经**全局** `va_list` 在另一个函数里读取, 并跨过 XMM
/// 保存区末端（>8 槽后转入 overflow_arg_area）。
#[test]
fn va_arg_global_valist_sse_agg() {
    let src = include_str!("../fixtures/c/va_arg/global_valist_sse_agg.c");
    let code = run_gcc_cross_check("va-global-sse", src);
    assert_eq!(code, 0, "全局 va_list + SSE 聚合跨函数读取 fixture failed");
}

/// `va_copy` 后两份游标各自独立推进, 副本与原件对同一串 SSE 类聚合的读出必须
/// 逐值相同（且都等于 gcc 的输出）。
#[test]
fn va_arg_vacopy_sse_agg() {
    let src = include_str!("../fixtures/c/va_arg/vacopy_sse_agg.c");
    let code = run_gcc_cross_check("va-vacopy-sse", src);
    assert_eq!(code, 0, "va_copy + SSE 聚合双游标 fixture failed");
}

/// `__int128`（2 个 GP eightbyte）与 SSE 类聚合交替读取: GP 与 XMM 游标必须
/// 互相独立, 且 `__int128` 装不下时"整体上栈、gp_offset 不推进"不得因中间插了
/// SSE 读取而被破坏。
#[test]
fn va_arg_int128_sse_interleave() {
    let src = include_str!("../fixtures/c/va_arg/int128_sse_interleave.c");
    let code = run_gcc_cross_check("va-i128-sse-mix", src);
    assert_eq!(code, 0, "__int128 与 SSE 聚合交替 fixture failed");
}

// ---------- 跨 TU（callee.c + caller.c）混合链接差分 ----------
//
// 下面 4 组夹具来自 `tests/fixtures/c/va_arg_mix/`，每组必须由 gcc 与 dcc
// **各自单独编译**两个 TU 后混合链接才能判定（`run_gcc_two_tu_cross_check`
// 的四种组合）。之所以不能退化成单文件 `run_gcc_cross_check`：变参调用约定
// 的失效形态常常是「caller 与 callee 对称地错」，同一编译器产出的两侧内部
// 自洽、退出码依然 0，只有把一侧的对象表示交给另一个实现才照得出来
// （D9 `ld80_vararg` 就是反例教材，见该目录 README 第 4 条）。

/// `va_arg(ap, __m128d)` 的两个 eightbyte 必须共享同一个 16 字节 XMM 槽。
/// 历史上出现过「低半进寄存器 + 高半进栈」的拆分；混合链接下它只能来自
/// 一侧的读/写约定，因此 four-combo 对照是唯一真判据。
#[test]
#[cfg(unix)]
fn va_arg_mix_m128_shared_slot_cross_tu_gcc_interop() {
    let callee = include_str!("../fixtures/c/va_arg_mix/m128_shared_slot_callee.c");
    let caller = include_str!("../fixtures/c/va_arg_mix/m128_shared_slot_caller.c");
    let rc = run_gcc_two_tu_cross_check("va-mix-m128-shared", callee, caller);
    assert_eq!(rc, 0, "m128_shared_slot: callee 用位掩码返回读错项，0 表示全对");
}

/// 11 个命名参数占满 6 个 GP 寄存器（`gp_offset == 48`）后，
/// `va_arg(ap, __int128)` 全部落在 `overflow_arg_area`；曾出现「低半正确、
/// 高半恒为同一个栈残值」。callee 把 7 个 `__int128` 的高低半写进全局数组，
/// caller 逐项打印并与 gcc 参照逐字节比对（只比退出码会把错判成 MATCH）。
#[test]
#[cfg(unix)]
fn va_arg_mix_int128_11named_cross_tu_gcc_interop() {
    let callee = include_str!("../fixtures/c/va_arg_mix/int128_11named_callee.c");
    let caller = include_str!("../fixtures/c/va_arg_mix/int128_11named_caller.c");
    let rc = run_gcc_two_tu_cross_check("va-mix-int128-11named", callee, caller);
    assert_eq!(rc, 0, "int128_11named: caller 恒返回 0，判定全在 stdout 逐字节对照里");
}

/// D9：变参 `long double` 属 X87 类，永远放在 16 字节 `overflow_arg_area`
/// 且内容是**真 80-bit 编码**。dcc 曾发 `movsd` 只取低 8 字节当成 f64 位型
/// （读到的是 64 位尾数，`(long long)` 饱和成 LLONG_MIN）。dcc caller 与 dcc
/// callee 共用私有约定时是自洽的错，单 TU 夹具查不出来。
#[test]
#[cfg(unix)]
fn va_arg_mix_ld80_vararg_cross_tu_gcc_interop() {
    let callee = include_str!("../fixtures/c/va_arg_mix/ld80_vararg_callee.c");
    let caller = include_str!("../fixtures/c/va_arg_mix/ld80_vararg_caller.c");
    let rc = run_gcc_two_tu_cross_check("va-mix-ld80-vararg", callee, caller);
    assert_eq!(rc, 0, "ld80_vararg: caller 逐判据返回 1/2/3，0 表示 80-bit 槽读对");
}

/// D10：具名（非变参）`long double` 形参与返回值的 SysV 约定。三处缺陷
/// （caller 槽内 80-bit 编码、callee 入口降级、X87 标量返回值应在 st(0)
/// 却被按 sret 处理）均已修复：caller 同时校验两个形参的全局回写**与**
/// 返回值的整数/小数部分，缺任何一项判据都会出现假绿。
#[test]
#[cfg(unix)]
fn va_arg_mix_ld80_namedparam_cross_tu_gcc_interop() {
    let callee = include_str!("../fixtures/c/va_arg_mix/ld80_namedparam_callee.c");
    let caller = include_str!("../fixtures/c/va_arg_mix/ld80_namedparam_caller.c");
    let rc = run_gcc_two_tu_cross_check("va-mix-ld80-namedparam", callee, caller);
    assert_eq!(rc, 0, "ld80_namedparam: caller 逐判据返回 1..4，0 表示形参与 st(0) 返回均对");
}

/// D8（**已修并全档转正**）：`va_arg` 取 VLA（运行时尺寸）聚合后，`d.data[i]` 的
/// 基址必须从 VLA **元数据槽里加载**（`mov -0x78(%r13),%rax`），不能 `lea` 那个槽本身；
/// 而调用侧把整块 VLA 聚合作为变参实参传出去时，也必须拿到同一个数据区。
///
/// 两处修点：`semantics::address_from_storage`（VLA/VM 聚合归 `StackSlotLoad`）与
/// `mem/object_resolver.rs::resolve_identity`（`vla_offsets` 排在 `var_map` 前）。
/// -O2/-O3 那一半原先仍坑：函数体里有运行时尺寸对象时 SSA 往返会丢它的身份（基址
/// 落回描述符区、`sizeof` 读回 0），现由 `opt.rs::block_has_unsafe_ssa_pattern` 让
/// 整函数退回 AST 路径 —— 与同列的 `static`/`cleanup`/嵌套函数同一口径。
///
/// 旧形状（修前）：四档全 SIGSEGV(139)，本件因此住在 `c/va_arg_xfail/`；现在它已
/// 移入 `c/va_arg/`（该目录由 `scripts/dcc_gcc_diff.sh` 全量扫，必须全绿）。
#[test]
#[cfg(unix)]
fn va_arg_vla_agg_via_va_arg_matches_gcc_at_all_levels() {
    let src = include_str!("../fixtures/c/va_arg/vla_agg_via_va_arg.c");
    for flag in ["-O0", "-O1", "-O2", "-O3"] {
        assert_eq!(
            run_gcc_cross_check_opt("va-vla-agg", src, &[flag]),
            0,
            "{flag}: VLA 聚合经 va_arg 赋值后的成员基址/运行时尺寸与 gcc 不一致"
        );
    }
}
