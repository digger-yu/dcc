// ARM backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::shared::asm_constraints::{self, resolve_asm_operands};

use crate::codegen::arm::codegen::ArmCodegen;

impl ArmCodegen {
    // 与所有后端的 `emit_inline_asm` 签名保持一致, 打包成结构体收益有限。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_inline_asm(
        &mut self,
        asm_str: &str,
        outputs: &[crate::ast::Expr],
        output_names: &[Option<String>],
        output_constraints: &[String],
        inputs: &[crate::ast::Expr],
        input_names: &[Option<String>],
        input_constraints: &[String],
        clobbers: &[String],
        span_file: &str,
        span_line: u32,
        volatile: bool,
    ) {
        let pool = asm_constraints::arm_reg_pool();
        let resolved_operands = resolve_asm_operands(
            output_constraints,
            input_constraints,
            output_names,
            input_names,
            &pool,
        );

        // --- volatile asm: 前置屏障 ---
        if volatile {
            self.emit_dmb();
        }

        // --- 加载 inout 输入值到分配的寄存器 ---
        for (i, op) in resolved_operands.iter().enumerate() {
            if op.is_input && op.is_output {
                if let Some(ref alloc) = op.reg {
                    if i < outputs.len() {
                        if let crate::ast::Expr::Ident { name, .. } = &outputs[i] {
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
                        if let crate::ast::Expr::Ident { name, .. } = &inputs[input_idx] {
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
        // 文本与机器码出自同一次解析: 不再按目标模式二选一。
        self.encode_and_emit_asm(&resolved, span_file, span_line);

        // --- 写回输出寄存器到 C 变量 ---
        let mut first_output_reg: Option<u8> = None;
        for (i, op) in resolved_operands.iter().enumerate() {
            if op.is_output && i < outputs.len() {
                if let Some(ref alloc) = op.reg {
                    if first_output_reg.is_none() {
                        first_output_reg = Some(alloc.idx);
                    }
                    if let crate::ast::Expr::Ident { name, .. } = &outputs[i] {
                        self.asm_store_reg_to_var(&name.name, alloc.idx);
                    }
                }
            }
        }

        // --- memory clobber 或 volatile: DMB 编译器屏障 ---
        if clobbers.iter().any(|c| c == "memory") || volatile {
            self.emit_dmb();
        }

        // --- 表达式结果: 第一个输出寄存器 → r0 ---
        if let Some(reg) = first_output_reg {
            if reg != 0 {
                self.emit_mov_rd_rs(0, reg);
            }
        } else {
            self.emit_load_imm(0, 0);
        }
    }

    /// DMB ISH — 内部共享内存屏障 (memory clobber 编译器屏障)
    pub(crate) fn emit_dmb(&mut self) {
        // DMB ISH: 0xF57FF05B (ARMv7+)
        self.code.extend_from_slice(&[0x5B, 0xF0, 0x7F, 0xF5]);
    }
}
