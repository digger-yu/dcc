// src/main.rs - DCC 入口
// 第一性原理:编译器入口只做一件事:把参数翻译成配置,然后驱动 driver。
// 不在此处做任何"工作"——所有工作放在 driver::run。
#![allow(dead_code, unreachable_patterns, dropping_references)]

use std::env;
use std::process::ExitCode;

mod ast;
mod bootstrap;
mod cli;
mod codegen;
mod const_eval;
mod cpp;
mod debug;
mod diagnostics;
mod driver;
mod format_check;
mod ir;
mod link;
mod loop_opt;
mod lto;
mod mmap_io;
mod opt;
mod parser;
mod preproc;
mod profile;
mod sanitizer;
mod ssa;
mod ssa_gvn;
mod ssa_passes;
mod target;
mod tokens;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let cfg = match cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            e.print();
            return ExitCode::from(2);
        }
    };
    // 拿到 cfg 之后再构造诊断配置,这样 driver 路径与 main 路径
    // (cli 错误 / driver 错误) 都尊重 -fdiagnostics-* 选项
    let dcfg = crate::diagnostics::DiagnosticsConfig {
        color: cfg.diag_color,
        format: cfg.diag_format,
        show_template_tree: cfg.diag_show_template_tree,
    };
    match driver::run(cfg) {
        Ok(()) => ExitCode::SUCCESS,
        Err(es) => {
            // 统一由 main.rs 兜底打印所有错误诊断;
            // 正常路径下 driver 不再 emit,避免重复输出。
            crate::diagnostics::print_all(&es, &dcfg);
            ExitCode::from(1)
        }
    }
}
