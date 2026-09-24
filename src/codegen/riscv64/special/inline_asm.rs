// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::ast::Expr;
use crate::codegen::riscv64::codegen::Riscv64Codegen;
use crate::codegen::shared::asm_constraints::{self, resolve_asm_operands};

impl Riscv64Codegen {
    /// 发射内联汇编: 加载输入 → 发射 asm → 写回输出 → 设置表达式结果
    // 参数即内联 asm 的完整上下文 (asm 文本/输出/输入/修饰符/clobber/标签/行号等),
    // 与所有后端的 `emit_inline_asm` 签名保持一致, 打包成结构体收益有限。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_inline_asm(
        &mut self,
        asm_str: &str,
        outputs: &[Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        clobbers: &[String],
        span_file: &str,
        span_line: u32,
        volatile: bool,
    ) {
        let pool = asm_constraints::riscv64_reg_pool();
        let resolved_operands = resolve_asm_operands(
            output_constraints,
            input_constraints,
            output_names,
            input_names,
            &pool,
        );

        // --- volatile asm: 前置屏障 ---
        if volatile {
            self.emit_fence();
        }

        // --- 加载 inout 输入值到分配的寄存器 ---
        for (i, op) in resolved_operands.iter().enumerate() {
            if op.is_input && op.is_output {
                if let Some(ref alloc) = op.reg {
                    if i < outputs.len() {
                        if let Expr::Ident { name, .. } = &outputs[i] {
                            self.asm_load_var_to_reg(&name.name, alloc.idx);
                        }
                    }
                }
            }
        }
        // --- 加载纯输入值到分配的寄存器 ---
        for (j, op) in resolved_operands.iter().enumerate() {
            if op.is_input && !op.is_output {
                if let Some(ref alloc) = op.reg {
                    let input_idx = j - output_constraints.len();
                    if input_idx < inputs.len() {
                        if let Expr::Ident { name, .. } = &inputs[input_idx] {
                            self.asm_load_var_to_reg(&name.name, alloc.idx);
                        }
                    }
                }
            }
        }

        // --- 解析模板并发射 asm ---
        let resolved = Self::resolve_asm_template_enhanced(
            asm_str,
            output_constraints,
            input_constraints,
            output_names,
            input_names,
            &[],
        );
        if self.asm_text_mode {
            let lines: Vec<String> = resolved
                .split('\n')
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            self.raw_func_asm_lines
                .push((format!("__inline_{}", self.label_serial), lines));
            self.label_serial += 1;
        } else {
            self.encode_and_emit_asm(&resolved, span_file, span_line);
        }

        // --- 写回输出寄存器到 C 变量 ---
        let mut first_output_reg: Option<u8> = None;
        for (i, op) in resolved_operands.iter().enumerate() {
            if op.is_output && i < outputs.len() {
                if let Some(ref alloc) = op.reg {
                    if first_output_reg.is_none() {
                        first_output_reg = Some(alloc.idx);
                    }
                    if let Expr::Ident { name, .. } = &outputs[i] {
                        self.asm_store_reg_to_var(&name.name, alloc.idx);
                    }
                }
            }
        }

        // --- memory clobber 或 volatile: FENCE 编译器屏障 ---
        if clobbers.iter().any(|c| c == "memory") || volatile {
            self.emit_fence();
        }

        // --- 表达式结果: 第一个输出寄存器 → a0 (x10) ---
        if let Some(reg) = first_output_reg {
            if reg != 10 {
                self.emit_addi(10, reg, 0); // mv a0, reg
            }
        } else {
            self.li_a0(0);
        }
    }
}
