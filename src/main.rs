// src/main.rs - DCC 入口
// 第一性原理:编译器入口只做一件事:把参数翻译成配置,然后驱动 driver。
// 不在此处做任何"工作"——所有工作放在 driver::run。

use std::env;
use std::process::ExitCode;

mod cli;
mod driver;
mod diagnostics;
mod target;
mod tokens;
mod ast;
mod parser;
mod codegen;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let cfg = match cli::parse(&args) {
        Ok(c)  => c,
        Err(e) => { e.print(); return ExitCode::from(2); }
    };
    match driver::run(cfg) {
        Ok(())  => ExitCode::SUCCESS,
        Err(es) => {
            for e in es { e.print(); }
            ExitCode::from(1)
        }
    }
}
