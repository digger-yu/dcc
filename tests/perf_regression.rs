// 性能回归测试 — 防止 O(n²) 编译复杂度回归。
//
// 背景: 大规模声明/参数/大帧用例曾因线性扫描与逐槽 placeholder
// 退化为 O(n²)。这些测试用"缩放比"断言 (100k 耗时 / 10k 耗时)
// 检测超线性回归, 该比值与机器速度基本无关:
//   - 线性实现: 比值 ≈ 10
//   - O(n²) 实现: 比值 ≈ 100
// 阈值取在两者之间, 留出调度抖动余量。
//
// 同时附带宽松的绝对时间上限兜底 (防挂起, 而非精确性能断言)。
use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("dcc-perf-{}-{}", name, std::process::id()))
}

fn compile_timed(name: &str, source: &str, limit: Duration) -> Duration {
    let src_path = temp_path(&format!("{name}.c"));
    let obj_path = temp_path(&format!("{name}.o"));
    fs::write(&src_path, source).expect("write perf fixture");
    let start = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_dcc"))
        .args(["-c", "-o"])
        .arg(&obj_path)
        .arg(&src_path)
        .stderr(Stdio::piped())
        .output()
        .expect("run dcc");
    let elapsed = start.elapsed();
    let _ = fs::remove_file(&src_path);
    let obj_ok = fs::metadata(&obj_path)
        .map(|m| m.len() > 0)
        .unwrap_or(false);
    let _ = fs::remove_file(&obj_path);
    assert!(
        output.status.success(),
        "{name}: dcc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(obj_ok, "{name}: object file missing or empty");
    assert!(elapsed < limit, "{name}: compile took {elapsed:?} (limit {limit:?})");
    elapsed
}

/// 100k extern 声明不得超时, 且复杂度不得退回 O(n²)。
#[test]
fn perf_extern_decl_scaling() {
    let gen = |n: u32| {
        let mut s = String::with_capacity(n as usize * 20);
        for i in 0..n {
            s.push_str(&format!("extern int g{i};\n"));
        }
        s.push_str("int main(void) { return g0 + g");
        s.push_str(&(n - 1).to_string());
        s.push_str("; }\n");
        s
    };
    let t10k = compile_timed("extern-10k", &gen(10_000), Duration::from_secs(30));
    let t100k = compile_timed("extern-100k", &gen(100_000), Duration::from_secs(60));
    let ratio = t100k.as_secs_f64() / t10k.as_secs_f64().max(0.001);
    assert!(
        ratio < 70.0,
        "extern decl scaling ratio {ratio:.1} suggests quadratic regression \
         (linear ~10, quadratic ~100)"
    );
}

/// 100k 函数参数不得超时, 且参数注册复杂度不得退回 O(n²)。
#[test]
fn perf_function_param_scaling() {
    let gen = |n: u32| {
        let mut s = String::with_capacity(n as usize * 16);
        s.push_str("int f(");
        for i in 0..n {
            if i > 0 {
                s.push_str(", ");
            }
            // 每个参数独立声明类型 (dcc 不支持 K&R 式类型共享)
            s.push_str(&format!("int a{i}"));
        }
        s.push_str(") { return a0 + a");
        s.push_str(&(n - 1).to_string());
        s.push_str("; }\n");
        s
    };
    let t10k = compile_timed("param-10k", &gen(10_000), Duration::from_secs(30));
    let t100k = compile_timed("param-100k", &gen(100_000), Duration::from_secs(60));
    let ratio = t100k.as_secs_f64() / t10k.as_secs_f64().max(0.001);
    assert!(
        ratio < 40.0,
        "param scaling ratio {ratio:.1} suggests quadratic regression \
         (linear ~10, quadratic ~100)"
    );
}

/// 超大栈帧 (4GB 数组) 编译不挂起、不溢出、不生成超长代码。
/// 布局内部用 64 位计算, 生成代码大小必须与帧大小无关。
#[test]
fn perf_large_frame_compiles() {
    let source = "\
extern void use(char *);
int main(void) {
    volatile char big[0x100000000UL];
    big[0] = 1;
    use((char *)big);
    return 0;
}
";
    let elapsed = compile_timed("large-frame", source, Duration::from_secs(30));
    // 生成代码不得随帧大小线性膨胀: 编译应在远小于上限的时间内完成。
    assert!(
        elapsed < Duration::from_secs(10),
        "large frame compile unexpectedly slow: {elapsed:?}"
    );
}

/// 10,000 层嵌套指针声明不得栈溢出 (迭代式解析)。
/// 形式同 torture limits-declparen.c: int (*(*...(*v)...)...) = 0;
#[test]
fn perf_deep_declaration_no_stack_overflow() {
    let n = 10_000;
    let mut s = String::with_capacity(n * 4 * 2 + 64);
    s.push_str("int ");
    for _ in 0..n {
        s.push_str("(*");
    }
    s.push_str("fp");
    for _ in 0..n {
        s.push(')');
    }
    s.push_str(" = 0;\nint main(void) { return 0; }\n");
    compile_timed("deep-declparen", &s, Duration::from_secs(30));
}
