// src/main.rs - DCC 入口
// 第一性原理:编译器入口只做一件事:把参数翻译成配置,然后驱动 driver。
// 不在此处做任何"工作"——所有工作放在 driver::run。

use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    // 使用大栈线程运行编译器,避免深度嵌套声明 (如 limits-declparen.c 的
    // 10000 层括号) 导致栈溢出. 256MB 足以处理极端嵌套场景.
    let stack_size = 256 * 1024 * 1024; // 256 MB
    let builder = std::thread::Builder::new().stack_size(stack_size);
    let handler = builder.spawn(main_inner).unwrap();
    match handler.join() {
        Ok(code) => code,
        Err(_) => ExitCode::from(1),
    }
}

fn main_inner() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let cfg = match dcc::cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            e.print();
            return ExitCode::from(2);
        },
    };
    // 拿到 cfg 之后再构造诊断配置,这样 driver 路径与 main 路径
    // (cli 错误 / driver 错误) 都尊重 -fdiagnostics-* 选项
    let dcfg = dcc::diagnostics::DiagnosticsConfig {
        color: cfg.diag_color,
        format: cfg.diag_format,
        show_template_tree: cfg.diag_show_template_tree,
    };
    match dcc::driver::run(cfg) {
        Ok(()) => ExitCode::SUCCESS,
        Err(es) => {
            // 统一由 main.rs 兜底打印所有错误诊断;
            // 正常路径下 driver 不再 emit,避免重复输出.
            dcc::diagnostics::print_all(&es, &dcfg);
            ExitCode::from(1)
        },
    }
}
