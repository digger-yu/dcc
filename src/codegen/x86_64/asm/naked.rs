//! asm/naked.rs —— 自历史完整实现（git 28a9063 单文件 Codegen）按职责簇恢复的模块。
//! 仅移动/可见性调整，不改语义（backend_refactor_plan §3.1 原则 1）。
#![allow(clippy::all)]
#![allow(unused_imports)]
use crate::ast::*;
use crate::codegen::shared::elf as shared_elf;
use crate::codegen::shared::elf::*;
use crate::codegen::shared::*;
use crate::codegen::x86_64::asm::*;
use crate::codegen::x86_64::Codegen;
use crate::codegen::x86_64::*;
use crate::const_eval::{self, ConstValue};
use crate::debug::*;
use crate::diagnostics::{Diagnostic, Severity, Span};
use crate::ir::{self, *};
use crate::semantics::{self, *};
use crate::target::Target;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
impl Codegen {
    pub(crate) fn compile_naked_func(&mut self, f: &FuncDecl, _prog: &Program, func_start: u64) {
        self.expr_depth = 0;
        self.temp_slots = 0;
        if let Some(body) = &f.body {
            for item in &body.items {
                match item {
                    BlockItem::Stmt(Stmt::Expr { e, .. }) => {
                        self.gen_naked_asm_expr(e);
                    },
                    BlockItem::Stmt(Stmt::AsmGoto {
                        asm_str,
                        outputs,
                        output_names: _,
                        output_constraints: _,
                        inputs,
                        input_names: _,
                        input_constraints: _,
                        clobbers: _,
                        goto_labels,
                        volatile: _,
                        span,
                    }) => {
                        self.current_asm_src_line = span.line;
                        let mut resolved_asm = asm_str.clone();
                        let label_base = outputs.len() + inputs.len();
                        for (i, label) in goto_labels.iter().enumerate().rev() {
                            let prefixed = Self::c_label_to_asm(&label.name);
                            resolved_asm = Self::asm_replace_label_idx(&resolved_asm, i, &prefixed);
                            resolved_asm = Self::asm_replace_label_idx(
                                &resolved_asm,
                                label_base + i,
                                &prefixed,
                            );
                            resolved_asm =
                                resolved_asm.replace(&format!("%l[{}]", label.name), &prefixed);
                            // Also replace literal label definitions in asm text
                            resolved_asm = resolved_asm
                                .replace(&format!("{}:", label.name), &format!("{}:", prefixed));
                        }
                        self.emit_asm_text_lines(&resolved_asm, span);
                    },
                    _ => {}, // naked 函数忽略非 asm 语句
                }
            }
        }
        // 记录函数大小
        let func_end = self.pos();
        self.func_sizes
            .insert(f.name.name.clone(), func_end - func_start);
    }
    fn gen_naked_asm_expr(&mut self, e: &Expr) {
        match e {
            Expr::AsmExpr {
                asm_str,
                outputs,
                output_names,
                output_constraints,
                inputs,
                input_names,
                input_constraints,
                clobbers,
                volatile,
                span,
                ..
            } => {
                self.current_asm_src_line = span.line;
                self.emit_inline_asm(
                    asm_str,
                    outputs,
                    output_names,
                    output_constraints,
                    inputs,
                    input_names,
                    input_constraints,
                    clobbers,
                    *volatile,
                );
            },
            // 语句表达式 ({...}) 可能包含 asm: 递归处理
            Expr::StmtExpr { block, .. } => {
                for item in &block.items {
                    if let BlockItem::Stmt(Stmt::Expr { e: inner, .. }) = item {
                        self.gen_naked_asm_expr(inner);
                    }
                }
            },
            _ => {}, // 忽略非 asm 表达式
        }
    }
}
