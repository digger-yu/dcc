// RISC-V 64 backend - mechanically split from the original monolithic
// implementation. Pure move: semantics are preserved; only visibility and
// import paths were adjusted for the modular layout.
use super::codegen::{
    BranchKind, Riscv64Codegen, RvvConfig, expr_has_call, find_func,
    classify_rv_hfa, HfaInfo, trunc_i32, is_float_ty, is_vector_ty,
    is_complex_ty, is_aggregate, aggregate_size, value_type_size,
    align_rodata, align_up_u64, stmt_has_call, var_is_static,
};
use crate::ast::{
    self, find_field, BinOp, BlockItem, Decl, Expr, Field, FuncDecl, Program,
    Stmt, Type, UnOp,
};
use crate::codegen::shared::asm_constraints::{
    self, resolve_asm_operands, resolve_asm_template_enhanced, AsmArch,
};
use crate::codegen::shared::backend::{CommonImage, CompileInput};
use crate::codegen::shared::elf::{
    self, ComdatGroup, ElfRela, ElfSection, ElfSym, R_RISCV_JAL, STB_GLOBAL,
    STB_LOCAL, STB_WEAK, STT_FUNC, STT_OBJECT, STT_TLS,
};
use crate::ir::{
    self, ExprKind, ExprLiteral, Function, Global, Module, TypedExpr,
    TypedStmt, ValueType,
};
use crate::target::Target;
use std::collections::HashMap;
use std::sync::Arc;

impl Riscv64Codegen {
    pub(crate) fn build_asm_output(&self, module: &Module) -> String {
        let prog = module.source();
        let mut out = String::new();
        let name = if self.source_name.is_empty() {
            "input.c".to_string()
        } else {
            self.source_name.clone()
        };
        out.push_str(&format!("\t.file \"{name}\"\n"));
        if !self.rodata.is_empty() {
            out.push_str("\t.section .rodata\n");
            for g in &module.globals {
                if self.global_rodata.contains_key(&g.name) {
                    out.push_str(&format!("\t.globl {}\n", g.name));
                    out.push_str(&format!("{}:\n", g.name));
                }
            }
        }
        out.push_str("\t.text\n");
        // 发射文件级 asm 原始文本
        for line in &self.raw_global_asm_lines {
            out.push_str(&format!("\t{line}\n"));
        }
        for func in &module.functions {
            let is_weak = find_func(prog, &func.name)
                .map(|f| f.is_weak)
                .unwrap_or(false);
            if is_weak {
                out.push_str(&format!("\t.weak {}\n", func.name));
            } else {
                out.push_str(&format!("\t.globl {}\n", func.name));
            }
            out.push_str(&format!("{}:\n", func.name));
            // 发射函数级内联 asm 原始文本
            for (fname, lines) in &self.raw_func_asm_lines {
                if fname == &func.name || fname.starts_with("__inline_") {
                    for line in lines {
                        out.push_str(&format!("\t{line}\n"));
                    }
                }
            }
            if let Some(size) = self.func_sizes.get(&func.name) {
                out.push_str(&format!("\t// IR lowering, {} bytes\n", size));
            }
            out.push_str("\tret\n");
        }
        out
    }

    /// 增强版 asm 模板占位符替换: 使用共享约束解析器分配寄存器并替换为实际寄存器名
}
