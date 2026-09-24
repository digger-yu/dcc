// src/codegen/x86_64/instructions.rs
// Low-level x86_64 machine code emission methods for Codegen
// Core instruction encoding: mov, arithmetic, logic, stack, jumps, labels

use crate::ast::{Expr, Type};
use crate::codegen::x86_64::codegen::JumpKind;
use crate::codegen::x86_64::codegen::PendingJump;
use crate::codegen::x86_64::Codegen;
use crate::semantics::ObjectResolver;

// REX prefix encoding helpers
#[inline]
fn rex_w() -> u8 {
    0x48
}
#[inline]
fn rex_r() -> u8 {
    0x4C
}
#[inline]
fn rex_b() -> u8 {
    0x41
}
#[inline]
fn rex_rb() -> u8 {
    0x4D
}
#[inline]
fn rex_xb() -> u8 {
    0x45
}
#[inline]
fn rex_wb() -> u8 {
    0x49
}
#[inline]
fn rex_wr() -> u8 {
    0x4C
}

// Encode full REX prefix
fn encode_rex_full(w: bool, r: u8, x: u8, b: u8) -> Option<u8> {
    let mut rex = 0x40u8;
    let mut needed = false;
    if w {
        rex |= 0x08;
        needed = true;
    }
    if r >= 8 {
        rex |= 0x04;
        needed = true;
    }
    if x >= 8 {
        rex |= 0x02;
        needed = true;
    }
    if b >= 8 {
        rex |= 0x01;
        needed = true;
    }
    if needed {
        Some(rex)
    } else {
        None
    }
}

impl Codegen {
    // ========================================================================
    // Core byte emission
    // ========================================================================

    pub fn emit(&mut self, b: &[u8]) {
        self.code.extend_from_slice(b);
    }

    // ========================================================================
    // Label management
    // ========================================================================

    pub fn fresh_label(&mut self) -> String {
        let n = self.next_label;
        self.next_label += 1;
        format!(".L{}", n)
    }

    pub fn label(&mut self, name: &str) {
        self.labels.insert(name.to_string(), self.pos());
    }

    // ========================================================================
    // Jump / branch instructions
    // ========================================================================

    pub fn jmp_rel32(&mut self, target: &str, kind: JumpKind) {
        let at = self.pos();
        if kind == JumpKind::Jmp {
            self.emit(&[0xE9, 0, 0, 0, 0]);
        } else {
            let cc = match kind {
                JumpKind::Je => 0x84,
                JumpKind::Jne => 0x85,
                JumpKind::Jl => 0x8C,
                JumpKind::Jle => 0x8E,
                JumpKind::Jg => 0x8F,
                JumpKind::Jge => 0x8D,
                JumpKind::Jb => 0x82,
                JumpKind::Ja => 0x87,
                JumpKind::Jns => 0x89,
                JumpKind::Jo => 0x80,
                _ => unreachable!(),
            };
            self.emit(&[0x0F, cc, 0, 0, 0, 0]);
        }
        self.pending.push(PendingJump {
            at,
            target: target.into(),
            kind,
            disp_offset: if kind == JumpKind::Jmp { at + 1 } else { at + 2 },
            next_ip: if kind == JumpKind::Jmp { at + 5 } else { at + 6 },
            is_push_imm: false,
        });
    }

    // ========================================================================
    // MOV instructions
    // ========================================================================

    pub fn mov(&mut self, dst: u8, src: u8) {
        // mov dst, src: 使用 0x8B (mov r64, r/m64) 使 dst 在 reg 字段, src 在 r/m 字段
        // REX.R (dst) 与 REX.B (src) 必须独立判断: 二者都 >= 8 时需同时置位,
        // 否则高位源寄存器 (r8-r15) 会被截断为 reg&7 (r8→rax, r12→rsp)
        let mut rex = 0x48u8;
        if dst >= 8 {
            rex |= 0x04;
        }
        if src >= 8 {
            rex |= 0x01;
        }
        self.emit(&[rex, 0x8B, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
    }

    pub fn mov_imm(&mut self, reg: u8, v: i128) {
        if v >= i32::MIN as i128 && v <= i32::MAX as i128 {
            // 使用 MOV r/m64, imm32 (REX.W + 0xC7 /0) 符号扩展 32 位立即数到 64 位
            // 避免 MOV r32, imm32 的零扩展导致负数比较错误
            let rex = 0x48 + if reg >= 8 { 0x01 } else { 0 };
            self.emit(&[rex, 0xC7, 0xC0 + (reg & 7)]);
            self.emit(&(v as i32).to_le_bytes());
        } else {
            self.emit(&[0x48 + if reg >= 8 { 0x01 } else { 0 }]);
            self.emit(&[0xb8 + (reg & 7)]);
            self.emit(&(v as i64).to_le_bytes());
        }
    }

    // ========================================================================
    // Stack instructions
    // ========================================================================

    pub fn push(&mut self, reg: u8) {
        // 硬件 push: 用 rsp, 变量用 r13, 互不干扰
        if reg == 0 {
            self.emit(&[0x50]);
        }
        // push rax
        else if reg == 1 {
            self.emit(&[0x51]);
        }
        // push rcx
        else if reg == 2 {
            self.emit(&[0x52]);
        }
        // push rdx
        else if reg == 3 {
            self.emit(&[0x53]);
        }
        // push rbx
        else if reg == 7 {
            self.emit(&[0x57]);
        }
        // push rdi
        else if reg == 6 {
            self.emit(&[0x56]);
        }
        // push rsi
        else if reg == 8 {
            self.emit(&[0x41, 0x50]);
        }
        // push r8
        else if reg == 9 {
            self.emit(&[0x41, 0x51]);
        }
        // push r9
        else if reg == 10 {
            self.emit(&[0x41, 0x52]);
        }
        // push r10
        else if reg == 11 {
            self.emit(&[0x41, 0x53]);
        }
        // push r11
        else if reg == 12 {
            self.emit(&[0x41, 0x54]);
        }
        // push r12
        else if reg == 13 {
            self.emit(&[0x41, 0x55]);
        }
        // push r13
        else if reg == 14 {
            self.emit(&[0x41, 0x56]);
        }
        // push r14
        else if reg == 15 {
            self.emit(&[0x41, 0x57]);
        }
        // push r15
        else {
            panic!("push: unsupported reg {}", reg);
        }
    }

    pub fn pop(&mut self, reg: u8) {
        if reg == 0 {
            self.emit(&[0x58]);
        }
        // pop rax
        else if reg == 1 {
            self.emit(&[0x59]);
        }
        // pop rcx
        else if reg == 2 {
            self.emit(&[0x5a]);
        }
        // pop rdx
        else if reg == 3 {
            self.emit(&[0x5b]);
        }
        // pop rbx
        else if reg == 7 {
            self.emit(&[0x5f]);
        }
        // pop rdi
        else if reg == 6 {
            self.emit(&[0x5e]);
        }
        // pop rsi
        else if reg == 8 {
            self.emit(&[0x41, 0x58]);
        }
        // pop r8
        else if reg == 9 {
            self.emit(&[0x41, 0x59]);
        }
        // pop r9
        else if reg == 10 {
            self.emit(&[0x41, 0x5a]);
        }
        // pop r10
        else if reg == 11 {
            self.emit(&[0x41, 0x5b]);
        }
        // pop r11
        else if reg == 12 {
            self.emit(&[0x41, 0x5c]);
        }
        // pop r12
        else if reg == 13 {
            self.emit(&[0x41, 0x5d]);
        }
        // pop r13
        else if reg == 14 {
            self.emit(&[0x41, 0x5e]);
        }
        // pop r14
        else if reg == 15 {
            self.emit(&[0x41, 0x5f]);
        }
        // pop r15
        else {
            panic!("pop: unsupported reg {}", reg);
        }
    }

    pub fn store_rsp_qword(&mut self, reg: u8, off: usize) {
        let rex = 0x48 | if reg >= 8 { 0x04 } else { 0 };
        self.emit(&[rex, 0x89]);
        if off < 128 {
            self.emit(&[0x44 | ((reg & 7) << 3), 0x24, off as u8]);
        } else {
            self.emit(&[0x84 | ((reg & 7) << 3), 0x24]);
            self.emit(&(off as u32).to_le_bytes());
        }
    }

    pub fn load_rsp_qword(&mut self, reg: u8, off: usize) {
        let rex = 0x48 | if reg >= 8 { 0x04 } else { 0 };
        self.emit(&[rex, 0x8B]);
        if off < 128 {
            self.emit(&[0x44 | ((reg & 7) << 3), 0x24, off as u8]);
        } else {
            self.emit(&[0x84 | ((reg & 7) << 3), 0x24]);
            self.emit(&(off as u32).to_le_bytes());
        }
    }

    // ========================================================================
    // R13-relative addressing (frame pointer alternative)
    // ========================================================================

    pub fn emit_lea_r13(&mut self, reg: u8, off: i64) {
        let rex_r = if reg >= 8 { 0x04 } else { 0 };
        if Self::fits_disp32(off) {
            self.emit(&[0x49 | rex_r, 0x8D]);
            self.emit_r13_modrm_disp(reg, off);
        } else {
            self.mov_imm(11, off as i128);
            self.emit(&[0x4D, 0x8D, 0x5C, 0x1D, 0x00]); // lea r11, [r13+r11]
            let rex = 0x49 | rex_r | 0x01;
            self.emit(&[rex, 0x8D]);
            self.emit_reg_indirect_modrm(reg, 11);
        }
    }

    pub fn emit_mov_mem_r13(&mut self, reg: u8, off: i64) {
        self.load_stack(reg, off);
    }

    pub fn emit_r13_modrm_disp(&mut self, reg_field: u8, off: i64) -> bool {
        if Self::fits_disp8(off) {
            self.emit(&[0x45 | ((reg_field & 7) << 3), off as i8 as u8]);
            true
        } else if Self::fits_disp32(off) {
            self.emit(&[0x85 | ((reg_field & 7) << 3)]);
            self.emit(&(off as i32).to_le_bytes());
            true
        } else {
            false
        }
    }

    pub fn emit_reg_indirect_modrm(&mut self, reg_field: u8, base: u8) {
        let rm = base & 7;
        if rm == 4 {
            self.emit(&[0x04 | ((reg_field & 7) << 3), 0x24]);
        } else if rm == 5 {
            self.emit(&[0x45 | ((reg_field & 7) << 3), 0x00]);
        } else {
            self.emit(&[(reg_field & 7) << 3 | rm]);
        }
    }

    // ========================================================================
    // Stack frame variable management
    // ========================================================================

    pub fn var_offset(&mut self, name: &str) -> i64 {
        if let Some(&(off, _)) = self.var_map.get(name) {
            return off;
        }
        // 懒分配路径: 委托给 register_var 以保证偏移计算一致
        self.register_var(name, &Type::Int)
    }

    pub fn load_stack(&mut self, reg: u8, off: i64) {
        self.load_stack_sized(reg, off, 8);
    }

    pub fn store_stack(&mut self, reg: u8, off: i64) {
        if Self::fits_disp32(off) {
            let mut rex = 0x49u8;
            if reg >= 8 {
                rex |= 0x04;
            }
            self.emit(&[rex, 0x89]);
            self.emit_r13_modrm_disp(reg, off);
        } else {
            self.emit_lea_r13(11, off);
            let mut rex = 0x49u8;
            if reg >= 8 {
                rex |= 0x04;
            }
            self.emit(&[rex, 0x89]);
            self.emit_reg_indirect_modrm(reg, 11);
        }
    }

    pub fn load_stack_sized(&mut self, reg: u8, off: i64, size: usize) {
        if Self::fits_disp32(off) {
            let rex_r = if reg >= 8 { 0x04 } else { 0 };
            match size {
                1 => self.emit(&[0x41 | rex_r, 0x0F, 0xB6]),
                2 => self.emit(&[0x41 | rex_r, 0x0F, 0xB7]),
                4 => self.emit(&[0x41 | rex_r, 0x8B]),
                _ => self.emit(&[0x49 | rex_r, 0x8B]),
            }
            self.emit_r13_modrm_disp(reg, off);
        } else {
            self.emit_lea_r13(11, off);
            let rex = match size {
                1 | 2 | 4 => 0x41 | if reg >= 8 { 0x04 } else { 0 },
                _ => 0x49 | if reg >= 8 { 0x04 } else { 0 },
            };
            match size {
                1 => self.emit(&[rex, 0x0F, 0xB6]),
                2 => self.emit(&[rex, 0x0F, 0xB7]),
                4 => self.emit(&[rex, 0x8B]),
                _ => self.emit(&[rex, 0x8B]),
            }
            self.emit_reg_indirect_modrm(reg, 11);
        }
    }

    pub fn emit_store(&mut self, src_reg: u8, offset: i64, size: usize) {
        let emit_direct = |this: &mut Self, src_reg: u8, offset: i64, size: usize| match size {
            1 => {
                let rex = 0x41u8 | if src_reg >= 8 { 0x04 } else { 0 };
                this.emit(&[rex, 0x88]);
                this.emit_r13_modrm_disp(src_reg, offset);
            },
            2 => {
                let rex = 0x41u8 | if src_reg >= 8 { 0x04 } else { 0 };
                this.emit(&[0x66, rex, 0x89]);
                this.emit_r13_modrm_disp(src_reg, offset);
            },
            4 => {
                let rex = 0x41u8 | if src_reg >= 8 { 0x04 } else { 0 };
                this.emit(&[rex, 0x89]);
                this.emit_r13_modrm_disp(src_reg, offset);
            },
            8 => {
                let rex = 0x49u8 | if src_reg >= 8 { 0x04 } else { 0 };
                this.emit(&[rex, 0x89]);
                this.emit_r13_modrm_disp(src_reg, offset);
            },
            _ => {},
        };

        if Self::fits_disp32(offset) {
            emit_direct(self, src_reg, offset, size);
            return;
        }

        self.emit_lea_r13(11, offset);
        match size {
            1 => {
                let rex = 0x41u8 | if src_reg >= 8 { 0x04 } else { 0 };
                self.emit(&[rex, 0x88]);
                self.emit_reg_indirect_modrm(src_reg, 11);
            },
            2 => {
                let rex = 0x41u8 | if src_reg >= 8 { 0x04 } else { 0 };
                self.emit(&[0x66, rex, 0x89]);
                self.emit_reg_indirect_modrm(src_reg, 11);
            },
            4 => {
                let rex = 0x41u8 | if src_reg >= 8 { 0x04 } else { 0 };
                self.emit(&[rex, 0x89]);
                self.emit_reg_indirect_modrm(src_reg, 11);
            },
            8 => {
                let rex = 0x49u8 | if src_reg >= 8 { 0x04 } else { 0 };
                self.emit(&[rex, 0x89]);
                self.emit_reg_indirect_modrm(src_reg, 11);
            },
            _ => {},
        }
    }

    pub fn emit_load_mem_typed(&mut self, size: usize, ty: &Type) {
        match ty {
            Type::Float => self.emit(&[0xF3, 0x0F, 0x10, 0x00]), // movss xmm0, [rax]
            Type::Double => self.emit(&[0xF2, 0x0F, 0x10, 0x00]), // movsd xmm0, [rax]
            _ => {
                if size < 8 && Self::is_signed_int(ty) {
                    match size {
                        1 => self.emit(&[0x48, 0x0F, 0xBE, 0x00]), // movsbq rax, [rax]
                        2 => self.emit(&[0x48, 0x0F, 0xBF, 0x00]), // movswq rax, [rax]
                        _ => self.emit(&[0x48, 0x63, 0x00]),       // movsxd rax, [rax]
                    }
                } else {
                    self.emit_load_mem(size);
                }
            },
        }
    }

    // ========================================================================
    // RSP manipulation
    // ========================================================================

    pub fn emit_sub_rsp(&mut self, bytes: i64) {
        if bytes <= 0 {
            return;
        }
        if bytes <= i32::MAX as i64 {
            self.emit(&[0x48, 0x81, 0xEC]);
            self.emit(&(bytes as i32).to_le_bytes());
        } else {
            self.mov_imm(11, bytes as i128);
            self.emit(&[0x4C, 0x29, 0xDC]); // sub rsp, r11
        }
    }

    pub fn emit_add_rsp(&mut self, bytes: i64) {
        if bytes <= 0 {
            return;
        }
        if bytes <= i32::MAX as i64 {
            self.emit(&[0x48, 0x81, 0xC4]);
            self.emit(&(bytes as i32).to_le_bytes());
        } else {
            self.mov_imm(11, bytes as i128);
            self.emit(&[0x4C, 0x01, 0xDC]); // add rsp, r11
        }
    }

    // ========================================================================
    // Arithmetic instructions
    // ========================================================================

    pub fn imul_reg(&mut self, dst: u8, src: u8) {
        let mut rex = 0x48u8;
        if dst >= 8 {
            rex |= 0x04;
        }
        if src >= 8 {
            rex |= 0x01;
        }
        self.emit(&[rex, 0x0F, 0xAF, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
    }

    pub fn imul_imm(&mut self, reg: u8, imm: i128) {
        let rex = if reg >= 8 { 0x4D } else { 0x48 };
        let modrm = 0xC0 | ((reg & 7) << 3) | (reg & 7);
        self.emit(&[rex, 0x69, modrm]);
        self.emit(&(imm as i32).to_le_bytes());
    }

    /// Increment register by 1.
    pub fn increment(&mut self, reg: u8, sz: usize) {
        if sz == 1 {
            let rex = if reg >= 8 { 0x41 } else { 0x00 };
            self.emit(&[rex, 0xFE, 0xC0 | (reg & 7)]);
        } else if sz == 2 {
            let _rex = if reg >= 8 { 0x66 | 0x41 << 8 } else { 0x66 };
            // 66 prefix for 16-bit
            if reg >= 8 {
                self.emit(&[0x66, 0x41, 0xFF, 0xC0 | (reg & 7)]);
            } else {
                self.emit(&[0x66, 0xFF, 0xC0 | (reg & 7)]);
            }
        } else if sz == 4 {
            if reg >= 8 {
                self.emit(&[0x41, 0xFF, 0xC0 | (reg & 7)]);
            } else {
                self.emit(&[0xFF, 0xC0 | (reg & 7)]);
            }
        } else {
            // 64-bit
            let rex = if reg >= 8 { 0x49 } else { 0x48 };
            self.emit(&[rex, 0xFF, 0xC0 | (reg & 7)]);
        }
    }

    // ========================================================================
    // Call / Ret
    // ========================================================================

    pub fn call_label(&mut self, l: &str) {
        let at = self.pos();
        self.emit(&[0xe8, 0, 0, 0, 0]);
        self.disasm_calls.insert(at + 1, l.to_string());
        if self.relocatable {
            // 延迟到 Phase 3 符号表构建后再解析
            self.pending_func_rels.push((at + 1, l.to_string()));
        } else {
            self.pending.push(PendingJump {
                at,
                target: l.into(),
                kind: JumpKind::Jmp,
                disp_offset: at + 1,
                next_ip: at + 5,
                is_push_imm: false,
            });
        }
    }

    pub fn emit_call_extern(&mut self, name: &str) {
        let _at = self.pos();
        self.emit(&[0xe8, 0, 0, 0, 0]); // call rel32 placeholder
        let call_at = self.code.len() as u64 - 4;
        self.disasm_calls.insert(call_at, name.to_string());
        self.pending_func_rels.push((call_at, name.to_string()));
    }

    // ========================================================================
    // Global variable support
    // ========================================================================

    pub fn is_global_not_local(&self, name: &str) -> bool {
        !self.is_local_name(name) && self.is_global_var(name)
    }

    pub fn is_global_var(&self, name: &str) -> bool {
        self.is_global_object(name)
    }

    pub fn emit_global_addr(&mut self, reg: u8, name: &str) {
        let resolved = self.resolve_global_key(name).to_string();
        let disp32_pos = self.pos() + 3;
        let rex = if reg >= 8 { 0x4C } else { 0x48 };
        self.emit(&[rex, 0x8D, 0x05 | ((reg & 7) << 3), 0, 0, 0, 0]);
        self.pending_data_rels.push((disp32_pos, resolved));
    }

    pub fn emit_global_load(&mut self, reg: u8, name: &str) {
        let resolved = self.resolve_global_key(name).to_string();
        let (sz, is_tls, _off, is_signed) = self
            .globals_raw
            .get(&resolved)
            .map(|g| (g.size, g.is_tls, g.offset, Self::is_signed_int(&g.ty)))
            .unwrap_or((8, false, 0, false));
        let tls_exec = is_tls && !self.relocatable;
        let pref = if reg >= 8 { 0x41 } else { 0x00 }; // REX.R for high reg
                                                       // 注: ET_EXEC TLS 走 pending_tls_rels (由 resolve() 修补).
                                                       // ET_REL TLS 走 pending_data_rels + R_X86_64_PC32 (host linker 处理).
                                                       // FS 段前缀与 RIP-relative 寻址不兼容, 故不使用.
        match sz {
            1 => {
                let pos = self.pos() + 3 + if pref != 0 { 1 } else { 0 };
                let modrm = 0x05 | ((reg & 7) << 3);
                let mut b = if pref != 0 { vec![pref] } else { vec![] };
                // 有符号: MOVSX (0F BE); 无符号: MOVZX (0F B6)
                b.extend_from_slice(&[
                    0x0F,
                    if is_signed { 0xBE } else { 0xB6 },
                    modrm,
                    0,
                    0,
                    0,
                    0,
                ]);
                self.emit(&b);
                if tls_exec {
                    self.pending_tls_rels.push((pos, resolved.clone()));
                } else {
                    self.pending_data_rels.push((pos, resolved.clone()));
                }
            },
            2 => {
                let pos = self.pos() + 3 + if pref != 0 { 1 } else { 0 };
                let modrm = 0x05 | ((reg & 7) << 3);
                let mut b = if pref != 0 { vec![pref] } else { vec![] };
                // 有符号: MOVSX (0F BF); 无符号: MOVZX (0F B7)
                b.extend_from_slice(&[
                    0x0F,
                    if is_signed { 0xBF } else { 0xB7 },
                    modrm,
                    0,
                    0,
                    0,
                    0,
                ]);
                self.emit(&b);
                if tls_exec {
                    self.pending_tls_rels.push((pos, resolved.clone()));
                } else {
                    self.pending_data_rels.push((pos, resolved.clone()));
                }
            },
            4 => {
                let modrm = 0x05 | ((reg & 7) << 3);
                if is_signed {
                    // 有符号: MOVSXD r64, r/m32 (REX.W + 63)
                    let pos = self.pos() + 3;
                    let rex = if reg >= 8 { 0x4C } else { 0x48 };
                    self.emit(&[rex, 0x63, modrm, 0, 0, 0, 0]);
                    if tls_exec {
                        self.pending_tls_rels.push((pos, resolved.clone()));
                    } else {
                        self.pending_data_rels.push((pos, resolved.clone()));
                    }
                } else {
                    // 无符号: MOV r32, r/m32 (8B)
                    let pos = self.pos() + 2 + if pref != 0 { 1 } else { 0 };
                    let mut b = if pref != 0 { vec![pref] } else { vec![] };
                    b.extend_from_slice(&[0x8B, modrm, 0, 0, 0, 0]);
                    self.emit(&b);
                    if tls_exec {
                        self.pending_tls_rels.push((pos, resolved.clone()));
                    } else {
                        self.pending_data_rels.push((pos, resolved.clone()));
                    }
                }
            },
            _ => {
                let pos = self.pos() + 3;
                let rex = if reg >= 8 { 0x4C } else { 0x48 };
                let modrm = 0x05 | ((reg & 7) << 3);
                self.emit(&[rex, 0x8B, modrm, 0, 0, 0, 0]);
                if tls_exec {
                    self.pending_tls_rels.push((pos, resolved.clone()));
                } else {
                    self.pending_data_rels.push((pos, resolved.clone()));
                }
            },
        }
    }

    pub fn emit_global_store(&mut self, reg: u8, name: &str) {
        let resolved = self.resolve_global_key(name).to_string();
        let (sz, is_tls, _off) = self
            .globals_raw
            .get(&resolved)
            .map(|g| (g.size, g.is_tls, g.offset))
            .unwrap_or((8, false, 0));
        let tls_exec = is_tls && !self.relocatable;
        let modrm = 0x05 | ((reg & 7) << 3);
        match sz {
            1 => {
                let pos = self.pos() + 2 + if reg >= 4 { 1 } else { 0 };
                if reg >= 4 {
                    self.emit(&[0x40, 0x88, modrm, 0, 0, 0, 0]);
                } else {
                    self.emit(&[0x88, modrm, 0, 0, 0, 0]);
                }
                if tls_exec {
                    self.pending_tls_rels.push((pos, resolved.clone()));
                } else {
                    self.pending_data_rels.push((pos, resolved.clone()));
                }
            },
            2 => {
                let pos = self.pos() + 3;
                self.emit(&[0x66, 0x89, modrm, 0, 0, 0, 0]);
                if tls_exec {
                    self.pending_tls_rels.push((pos, resolved.clone()));
                } else {
                    self.pending_data_rels.push((pos, resolved.clone()));
                }
            },
            4 => {
                let pos = self.pos() + 2;
                self.emit(&[0x89, modrm, 0, 0, 0, 0]);
                if tls_exec {
                    self.pending_tls_rels.push((pos, resolved.clone()));
                } else {
                    self.pending_data_rels.push((pos, resolved.clone()));
                }
            },
            _ => {
                let pos = self.pos() + 3;
                let rex = if reg >= 8 { 0x4C } else { 0x48 };
                self.emit(&[rex, 0x89, modrm, 0, 0, 0, 0]);
                if tls_exec {
                    self.pending_tls_rels.push((pos, resolved.clone()));
                } else {
                    self.pending_data_rels.push((pos, resolved.clone()));
                }
            },
        }
    }

    // ========================================================================
    // Local variable registration / slot allocation
    // ========================================================================

    pub fn register_var(&mut self, name: &str, ty: &Type) -> i64 {
        if let Some(entry) = self.var_map.get(name) {
            // 变量已存在 (外层作用域同名变量或 pre_register_vars 预分配).
            // 始终更新类型: 内层作用域可能用不同类型遮蔽外层变量
            // (如外层 unsigned long xx, 内层 signed int xx).
            // 更新类型确保 mov_to_stack/mov_from_stack 按正确宽度
            // 存取栈槽 (如 signed int 用 4 字节 store, movsxd load).
            let (off, ref existing_ty) = *entry;
            if existing_ty != ty {
                self.var_map.insert(name.to_string(), (off, ty.clone()));
            }
            return off;
        }
        // P1-5: register asm 变量不分配栈槽, 直接绑定到指定寄存器
        if self.reg_bindings.contains_key(name) {
            return 0;
        }
        let is_vla = Self::type_has_vla(ty);
        let alloc_sz = if is_vla {
            ty.size().saturating_add(16)
        } else {
            ty.size()
        };
        let slots = ((alloc_sz.saturating_add(7) / 8).max(1)) as i64;
        let used = self.var_map_total_slots.saturating_mul(8);
        let hdr = 48i64 + self.temp_slots as i64 * 8;
        let off = -(used + hdr) - (slots.saturating_sub(1) * 8);
        self.var_map.insert(name.to_string(), (off, ty.clone()));
        self.var_map_total_slots = self.var_map_total_slots.saturating_add(slots);
        off
    }

    pub fn alloc_sret_slot(&mut self, size: usize) -> i64 {
        self.sret_depth += 1;
        let padded = (size as i64 + 15) & !15;
        // Allocate sret slots inside the frame, starting after vec_reserve(512)
        // + bf_rmw_reserve(32) + padding(56). This lands in the trampoline
        // reserve area, well below parameter/local save slots near r13.
        // Previously used -8 - expr_depth*8 - sret_depth*padded, which placed
        // sret slots directly on top of parameter save slots (e.g. rdi at -72
        // and sret slot 2 at -104..-56 overlap in pr51877).
        let base = self.vec_temp_off + 600;
        base - (self.sret_depth as i64 - 1) * padded
    }

    /// Allocate a local variable slot (generic name).
    pub fn alloc_local(&mut self, sz: usize) -> i64 {
        self.alloc_sret_slot(sz)
    }

    // ========================================================================
    // XMM / SSE helpers
    // ========================================================================

    pub fn emit_xmm0_r13_disp(&mut self, opcode: &[u8], off: i64) {
        if Self::fits_disp32(off) {
            self.emit(opcode);
            if Self::fits_disp8(off) {
                self.emit(&[0x45, off as i8 as u8]);
            } else {
                self.emit(&[0x85]);
                self.emit(&(off as i32).to_le_bytes());
            }
        } else {
            self.emit_lea_r13(11, off);
            let mut rewritten = opcode.to_vec();
            if let Some(rex) = rewritten.iter_mut().rev().find(|b| (**b & 0xF0) == 0x40) {
                *rex = (*rex & !0x01) | 0x01;
            }
            self.emit(&rewritten);
            self.emit(&[0x03]);
        }
    }

    pub fn emit_xmm_store_rsp(&mut self, pfx: u8, xmm: u8, off: usize) {
        self.emit(&[pfx, 0x0F, 0x11]);
        if off < 128 {
            self.emit(&[0x44 | ((xmm & 7) << 3), 0x24, off as u8]);
        } else {
            self.emit(&[0x84 | ((xmm & 7) << 3), 0x24]);
            self.emit(&(off as u32).to_le_bytes());
        }
    }

    pub fn emit_xmm_load_rsp(&mut self, pfx: u8, xmm: u8, off: usize) {
        self.emit(&[pfx, 0x0F, 0x10]);
        if off < 128 {
            self.emit(&[0x44 | ((xmm & 7) << 3), 0x24, off as u8]);
        } else {
            self.emit(&[0x84 | ((xmm & 7) << 3), 0x24]);
            self.emit(&(off as u32).to_le_bytes());
        }
    }

    pub fn load_stack_xmm(&mut self, xmm: u8, off: i64, elem_size: usize, is_float: bool) {
        if is_float || elem_size >= 8 {
            let pfx = if is_float { 0xF3 } else { 0xF2 };
            self.emit_xmm_r13_disp(pfx, 0x10, xmm, off);
            return;
        }
        self.load_stack_sized(0, off, elem_size);
        self.emit_movd_gp_to_xmm(xmm, 0);
    }

    pub fn store_xmm128_stack(&mut self, xmm: u8, off: i64) {
        self.emit_xmm_r13_disp(0x00, 0x11, xmm, off);
    }

    pub fn store_xmm64_stack(&mut self, xmm: u8, off: i64) {
        self.emit_xmm_r13_disp(0xF2, 0x11, xmm, off);
    }

    pub fn emit_movq_gp_to_xmm(&mut self, xmm: u8, gp: u8) {
        let mut rex = 0x48;
        if xmm >= 8 {
            rex |= 0x04;
        }
        if gp >= 8 {
            rex |= 0x01;
        }
        self.emit(&[0x66, rex, 0x0F, 0x6E, 0xC0 | ((xmm & 7) << 3) | (gp & 7)]);
    }

    pub fn flip_sign_xmm(&mut self, xmm: u8, ty: &Type) {
        let mask: u64 = if matches!(ty, Type::Float) {
            0x8000_0000
        } else {
            0x8000_0000_0000_0000
        };
        self.emit(&[0x48, 0xB8]);
        self.emit(&mask.to_le_bytes()); // movabs rax, mask
        self.emit(&[0x66, 0x48, 0x0F, 0x6E, 0xC8]); // movq xmm1, rax
        if xmm == 1 {
            // xorps/xorpd xmm0, xmm1 → 目标为 xmm1 时借用 xmm2
            self.emit(&[0x66, 0x48, 0x0F, 0x6E, 0xD0]); // movq xmm2, rax
            if matches!(ty, Type::Float) {
                self.emit(&[0x0F, 0x57, 0xCA]); // xorps xmm1, xmm2
            } else {
                self.emit(&[0x66, 0x0F, 0x57, 0xCA]); // xorpd xmm1, xmm2
            }
        } else {
            if matches!(ty, Type::Float) {
                self.emit(&[0x0F, 0x57, 0xC1]); // xorps xmm0, xmm1
            } else {
                self.emit(&[0x66, 0x0F, 0x57, 0xC1]); // xorpd xmm0, xmm1
            }
        }
    }

    pub fn emit_scalar_to_xmm0_if_needed(&mut self, expr: &Expr, elem_ty: &Type) {
        if !matches!(elem_ty, Type::Float | Type::Double | Type::LongDouble) {
            return;
        }
        if matches!(
            expr.ty(),
            Type::Float | Type::Double | Type::LongDouble | Type::Complex(_) | Type::Imaginary(_)
        ) {
            // 浮点表达式结果已在 xmm0
            // 但如果表达式类型与目标类型不匹配，需要转换
            // 例如: double 表达式赋值给 float 变量，需要 cvtsd2ss
            if matches!(elem_ty, Type::Float) && matches!(expr.ty(), Type::Double) {
                // double -> float: cvtsd2ss xmm0, xmm0
                self.emit(&[0xF2, 0x0F, 0x5A, 0xC0]);
            } else if matches!(elem_ty, Type::Double) && matches!(expr.ty(), Type::Float) {
                // float -> double: cvtss2sd xmm0, xmm0
                self.emit(&[0xF3, 0x0F, 0x5A, 0xC0]);
            }
            return;
        }
        // 整数→浮点转换: 根据整数类型宽度选择 32 位或 64 位转换
        let expr_ty = expr.ty();
        let is_64bit =
            matches!(expr_ty, Type::Long | Type::ULong | Type::LongLong | Type::ULongLong);
        // 无符号 <=32 位类型: 值在寄存器中已零扩展到 64 位,
        // 但 cvtsi2ss/sd 32 位形式会将有符号 32 位解释转换,
        // 对 >INT32_MAX 的值 (如 0x80000081) 产生错误负数结果.
        // 修复: 先零扩展到 64 位 (mov eax,eax), 再用 REX.W 64 位转换.
        let is_unsigned_small =
            matches!(expr_ty, Type::Bool | Type::UChar | Type::UShort | Type::UInt);
        if is_unsigned_small {
            // 零扩展: mov dst32, dst32 (32 位写自动清零高 32 位)
            let dst = 0; // 整数结果约定在 rax
            let rex = 0x40 | if dst >= 8 { 0x01 } else { 0 };
            self.emit(&[rex, 0x89, 0xC0 | ((dst & 7) << 3) | (dst & 7)]);
        }
        // 无符号 64 位类型: cvtsi2ssq/cvtsi2sdq 将有符号 64 位
        // 解释转换, 对 >INT63 的值产生错误结果.
        // 使用 emit_ulong_to_float: 检查符号位, 正数直接转换,
        // bit63=1 则 shr 1 + 转换 + 乘 2.0.
        let is_unsigned_64 = matches!(expr_ty, Type::ULong | Type::ULongLong);
        if is_unsigned_64 {
            let is_double = !matches!(elem_ty, Type::Float);
            self.emit_ulong_to_float(0, is_double);
        } else if matches!(elem_ty, Type::Float) {
            if is_64bit || is_unsigned_small {
                // cvtsi2ssq xmm0, rax (REX.W + F3 0F 2A C0)
                self.emit(&[0xF3, 0x48, 0x0F, 0x2A, 0xC0]);
            } else {
                self.emit(&[0xF3, 0x0F, 0x2A, 0xC0]); // cvtsi2ss xmm0, eax
            }
        } else {
            if is_64bit || is_unsigned_small {
                // cvtsi2sdq xmm0, rax (REX.W + F2 0F 2A C0)
                self.emit(&[0xF2, 0x48, 0x0F, 0x2A, 0xC0]);
            } else {
                self.emit(&[0xF2, 0x0F, 0x2A, 0xC0]); // cvtsi2sd xmm0, eax
            }
        }
    }

    // ========================================================================
    // Alignment
    // ========================================================================

    /// Align code to the given boundary by emitting NOPs.
    pub fn align(&mut self, alignment: usize) {
        if alignment <= 1 {
            return;
        }
        let pos = self.code.len() % alignment;
        if pos != 0 {
            let nops = alignment - pos;
            // Use multi-byte NOP sequences
            for _ in 0..nops {
                self.emit(&[0x90]);
            }
        }
    }

    /// Emit a single NOP.
    pub fn nop(&mut self) {
        self.emit(&[0x90]);
    }

    pub fn maybe_align_jump(&mut self) {
        if self.target.align_jumps > 1 {
            self.emit_align_padding(self.target.align_jumps);
        }
    }

    pub fn maybe_align_loop(&mut self) {
        if self.target.align_loops > 1 {
            self.emit_align_padding(self.target.align_loops);
        }
    }

    // ========================================================================
    // Sign / zero extend
    // ========================================================================

    /// Sign-extend a value in a register to full width.
    pub fn sign_extend_to_reg(&mut self, dst: u8, src: u8, from_bits: u8) {
        match from_bits {
            8 => {
                // movsx dst, byte src
                let rex = encode_rex_full(true, dst, 0, src);
                if let Some(r) = rex {
                    self.emit(&[r, 0x0F, 0xBE, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
                } else {
                    self.emit(&[0x48, 0x0F, 0xBE, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
                }
            },
            16 => {
                let rex = encode_rex_full(true, dst, 0, src);
                if let Some(r) = rex {
                    self.emit(&[r, 0x0F, 0xBF, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
                } else {
                    self.emit(&[0x48, 0x0F, 0xBF, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
                }
            },
            32 => {
                let rex = encode_rex_full(true, dst, 0, src);
                if let Some(r) = rex {
                    self.emit(&[r, 0x63, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
                } else {
                    self.emit(&[0x48, 0x63, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
                }
            },
            _ => {
                if dst != src {
                    self.mov(dst, src);
                }
            },
        }
    }

    // ========================================================================
    // Resolvers and helpers
    // ========================================================================

    pub fn resolve_global_key<'a>(&'a self, name: &'a str) -> &'a str {
        self.static_local_map
            .resolve(&self.current_func_name, name)
            .unwrap_or(name)
    }

    pub fn resolve_asm_label<'a>(&'a self, c_name: &'a str) -> &'a str {
        self.asm_label_map
            .get(c_name)
            .map(|s| s.as_str())
            .unwrap_or(c_name)
    }

    pub fn rsp(&self) -> u64 {
        4
    }

    pub fn vector_temp_base(&self, slot: usize, vec_size: usize) -> i64 {
        let slot_bytes = vec_size.div_ceil(8) as i64 * 8;
        self.vec_temp_off + (slot as i64 * slot_bytes)
    }

    pub fn alloc_callee_saved(&mut self) -> Option<u8> {
        for i in 0..4 {
            if self.reg_allocated & (1 << i) == 0 {
                self.reg_allocated |= 1 << i;
                self.callee_saved_used |= 1 << i;
                return Some(self.callee_saved_pool[i]);
            }
        }
        None // 池已耗尽
    }

    pub fn free_callee_saved(&mut self, reg: u8) {
        for i in 0..4 {
            if self.callee_saved_pool[i] == reg {
                self.reg_allocated &= !(1 << i);
                return;
            }
        }
    }

    // ---- 以下为自 monolith（28a9063）恢复的底层指令编码原语 ----

    pub(crate) fn emit_add_r10(&mut self, inner_sz: i64) {
        // add rax/eax, r10: reg=r10 → REX.R+W (0x4C)
        let rex = if inner_sz == 8 { 0x4C } else { 0x44 };
        self.emit(&[rex, 0x01, 0xD0]);
    }
    pub(crate) fn emit_add_r11(&mut self, inner_sz: i64) {
        let rex = if inner_sz == 8 { 0x4C } else { 0x44 };
        self.emit(&[rex, 0x01, 0xD8]);
    }
    pub(crate) fn emit_add_rm(&mut self, inner_sz: i64, off: i64) {
        let rex = if inner_sz == 8 { 0x48 } else { 0x00 };
        if rex != 0 {
            self.emit(&[rex]);
        }
        self.emit(&[0x03, 0x44, 0x24, off as u8]); // add rax/eax, [rsp+off]
    }
    pub(crate) fn emit_align_padding(&mut self, alignment: u32) {
        if alignment <= 1 {
            return;
        }
        let pos = self.code.len() as u64;
        let aligned = (pos + alignment as u64 - 1) & !(alignment as u64 - 1);
        let pad = (aligned - pos) as usize;
        for _ in 0..pad {
            self.emit(&[0x90]); // NOP
        }
    }
    pub(crate) fn emit_and_rdx_rsi(&mut self, sz: usize) {
        match sz {
            1 => self.emit(&[0x40, 0x20, 0xF2]), // and dl, sil
            2 => self.emit(&[0x66, 0x21, 0xF2]), // and dx, si
            4 => self.emit(&[0x21, 0xF2]),       // and edx, esi
            _ => self.emit(&[0x48, 0x21, 0xF2]), // and rdx, rsi
        }
    }
    pub(crate) fn emit_cas_loop<F: Fn(&mut Self, usize)>(&mut self, sz: usize, compute_new: F) {
        match sz {
            1 => {
                // 8-bit: al/al, sil/dl/ah 8 位子寄存器参与
                self.emit(&[0x0F, 0xB6, 0x07]); // movzx eax, byte [rdi]
                let l_retry = self.fresh_label();
                self.label(&l_retry);
                self.emit(&[0x89, 0xC1]); // mov ecx, eax
                self.emit(&[0x89, 0xC2]); // mov edx, eax
                compute_new(self, sz);
                self.emit_lock_cmpxchg_rdi_rdx(sz);
                let l_done = self.fresh_label();
                self.jmp_rel32(&l_done, JumpKind::Je);
                self.jmp_rel32(&l_retry, JumpKind::Jmp);
                self.label(&l_done);
            },
            2 => {
                // 16-bit: ax/si/dx 子寄存器
                self.emit(&[0x0F, 0xB7, 0x07]); // movzx eax, word [rdi]
                let l_retry = self.fresh_label();
                self.label(&l_retry);
                self.emit(&[0x89, 0xC1]); // mov ecx, eax
                self.emit(&[0x89, 0xC2]); // mov edx, eax
                compute_new(self, sz);
                self.emit_lock_cmpxchg_rdi_rdx(sz);
                let l_done = self.fresh_label();
                self.jmp_rel32(&l_done, JumpKind::Je);
                self.jmp_rel32(&l_retry, JumpKind::Jmp);
                self.label(&l_done);
            },
            4 => {
                // 32-bit (zero-extends to 64-bit)
                self.emit(&[0x8B, 0x07]); // mov eax, [rdi]
                let l_retry = self.fresh_label();
                self.label(&l_retry);
                self.emit(&[0x89, 0xC1]); // mov ecx, eax
                self.emit(&[0x89, 0xC2]); // mov edx, eax
                compute_new(self, sz);
                self.emit_lock_cmpxchg_rdi_rdx(sz);
                let l_done = self.fresh_label();
                self.jmp_rel32(&l_done, JumpKind::Je);
                self.jmp_rel32(&l_retry, JumpKind::Jmp);
                self.label(&l_done);
            },
            _ => {
                // 64-bit
                self.emit(&[0x48, 0x8B, 0x07]); // mov rax, [rdi]
                let l_retry = self.fresh_label();
                self.label(&l_retry);
                self.emit(&[0x48, 0x89, 0xC1]); // mov rcx, rax
                self.emit(&[0x48, 0x89, 0xC2]); // mov rdx, rax
                compute_new(self, sz);
                self.emit_lock_cmpxchg_rdi_rdx(sz);
                let l_done = self.fresh_label();
                self.jmp_rel32(&l_done, JumpKind::Je);
                self.jmp_rel32(&l_retry, JumpKind::Jmp);
                self.label(&l_done);
            },
        }
    }
    pub(crate) fn emit_c_ld32(&mut self, inner_sz: i64, off1: i64, off2: i64, signed: bool) {
        if inner_sz == 4 {
            self.emit(&[0x8B, 0x44, 0x24, off1 as u8]); // mov eax, [rsp+off1]
            self.emit(&[0x8B, 0x4C, 0x24, off2 as u8]); // mov ecx, [rsp+off2]
        } else {
            let ld = match (inner_sz, signed) {
                (2, true) => 0xBF,
                (2, false) => 0xB7,
                (_, true) => 0xBE,
                (_, false) => 0xB6,
            };
            self.emit(&[0x0F, ld, 0x44, 0x24, off1 as u8]); // movsx/movzx eax
            self.emit(&[0x0F, ld, 0x4C, 0x24, off2 as u8]); // movsx/movzx ecx
        }
    }
    pub(crate) fn emit_cqo(&mut self, inner_sz: i64) {
        if inner_sz == 8 {
            self.emit(&[0x48]);
        }
        self.emit(&[0x99]);
    }
    pub(crate) fn emit_div_rm(&mut self, inner_sz: i64, is_signed: bool, rm32: u8) {
        let op = if is_signed { rm32 + 8 } else { rm32 };
        if inner_sz == 8 {
            self.emit(&[0x48, 0xF7, op]);
        } else {
            self.emit(&[0xF7, op]);
        }
    }
    pub(crate) fn emit_evex_prefix(
        &mut self,
        r: u8,
        b: u8,
        map: u8,
        w: u8,
        v: u8,
        l: u8,
        pp: u8,
        mask: u8,
    ) {
        let r_inv = if r >= 8 { 0u8 } else { 1u8 };
        let r2 = if r >= 16 { 0u8 } else { 1u8 };
        let b_inv = if b >= 8 { 0u8 } else { 1u8 };
        let v_inv = (!v) & 0x0F;
        // V' 位 (P2 bit3) = ~v[4]: 高位移位, 缺失会导致 src1 解码到 zmm16-31
        let v4 = if v >= 16 { 0u8 } else { 1u8 };
        self.emit(&[
            0x62,
            (r_inv << 7) | (1u8 << 6) | (b_inv << 5) | (r2 << 4) | (map & 0x0F),
            (w << 7) | (v_inv << 3) | (1u8 << 2) | (pp & 0x03),
            (l << 5) | (v4 << 3) | (mask & 0x07),
        ]);
    }
    pub(crate) fn emit_imul_r10(&mut self, inner_sz: i64) {
        // imul rax/eax, r10: rm=r10 → REX.B (0x41/0x49)
        let rex = if inner_sz == 8 { 0x49 } else { 0x41 };
        self.emit(&[rex, 0x0F, 0xAF, 0xC2]);
    }
    pub(crate) fn emit_imul_r11(&mut self, inner_sz: i64) {
        let rex = if inner_sz == 8 { 0x49 } else { 0x41 };
        self.emit(&[rex, 0x0F, 0xAF, 0xC3]);
    }
    pub(crate) fn emit_imul_rm(&mut self, inner_sz: i64, off: i64) {
        let rex = if inner_sz == 8 { 0x48 } else { 0x00 };
        if rex != 0 {
            self.emit(&[rex]);
        }
        self.emit(&[0x0F, 0xAF, 0x44, 0x24, off as u8]); // imul rax/eax, [rsp+off]
    }
    pub(crate) fn emit_load_mem(&mut self, size: usize) {
        match size {
            1 => self.emit(&[0x48, 0x0f, 0xb6, 0x00]), // movzbl (%rax), %rax
            2 => self.emit(&[0x48, 0x0f, 0xb7, 0x00]), // movzwl (%rax), %rax
            4 => self.emit(&[0x8b, 0x00]),             // mov (%rax), %eax (零扩展到64位)
            _ => self.emit(&[0x48, 0x8b, 0x00]),       // mov (%rax), %rax
        }
    }
    fn emit_lock_cmpxchg_rdi_rdx(&mut self, sz: usize) {
        match sz {
            1 => self.emit(&[0xF0, 0x40, 0x0F, 0xB0, 0x17]), // lock cmpxchg [rdi], dl
            2 => self.emit(&[0x66, 0xF0, 0x0F, 0xB1, 0x17]), // lock cmpxchg [rdi], dx
            4 => self.emit(&[0xF0, 0x0F, 0xB1, 0x17]),       // lock cmpxchg [rdi], edx
            _ => self.emit(&[0xF0, 0x48, 0x0F, 0xB1, 0x17]), // lock cmpxchg [rdi], rdx
        }
    }
    pub(crate) fn emit_lock_cmpxchg_rdi_rsi(&mut self, sz: usize) {
        match sz {
            1 => self.emit(&[0xF0, 0x40, 0x0F, 0xB0, 0x37]), // lock cmpxchg [rdi], sil
            2 => self.emit(&[0x66, 0xF0, 0x0F, 0xB1, 0x37]), // lock cmpxchg [rdi], si
            4 => self.emit(&[0xF0, 0x0F, 0xB1, 0x37]),       // lock cmpxchg [rdi], esi
            _ => self.emit(&[0xF0, 0x48, 0x0F, 0xB1, 0x37]), // lock cmpxchg [rdi], rsi
        }
    }
    pub(crate) fn emit_lock_xchg_rdi_rsi(&mut self, sz: usize) {
        match sz {
            1 => self.emit(&[0xF0, 0x40, 0x86, 0x37]), // lock xchg [rdi], sil
            2 => self.emit(&[0x66, 0xF0, 0x87, 0x37]), // lock xchg [rdi], si
            4 => self.emit(&[0xF0, 0x87, 0x37]),       // lock xchg [rdi], esi
            _ => self.emit(&[0xF0, 0x48, 0x87, 0x37]), // lock xchg [rdi], rsi
        }
        match sz {
            1 => self.emit(&[0x40, 0x0F, 0xB6, 0xF6]), // movzbl sil, esi
            2 => self.emit(&[0x0F, 0xB7, 0xF6]),       // movzwl si, esi
            _ => {},
        }
    }
    pub(crate) fn emit_movd_gp_to_xmm(&mut self, xmm: u8, gp: u8) {
        let mut rex = 0x40;
        if xmm >= 8 {
            rex |= 0x04;
        }
        if gp >= 8 {
            rex |= 0x01;
        }
        self.emit(&[0x66]);
        if rex != 0x40 {
            self.emit(&[rex]);
        }
        self.emit(&[0x0F, 0x6E, 0xC0 | ((xmm & 7) << 3) | (gp & 7)]);
    }
    pub(crate) fn emit_movdqu_r13_to_xmm(&mut self, xmm_reg: u8, off: i64) {
        if Self::fits_disp32(off) {
            let rex = 0x41 | if xmm_reg >= 8 { 0x04 } else { 0 };
            self.emit(&[0xF3, rex, 0x0F, 0x6F]);
            self.emit_r13_modrm_disp(xmm_reg, off);
        } else {
            self.emit_lea_r13(11, off);
            let rex = 0x41 | if xmm_reg >= 8 { 0x04 } else { 0 } | 0x01;
            self.emit(&[0xF3, rex, 0x0F, 0x6F]);
            self.emit_reg_indirect_modrm(xmm_reg, 11);
        }
    }
    pub(crate) fn emit_movdqu_xmm_to_r13(&mut self, xmm_reg: u8, off: i64) {
        if Self::fits_disp32(off) {
            let rex = 0x41 | if xmm_reg >= 8 { 0x04 } else { 0 };
            self.emit(&[0xF3, rex, 0x0F, 0x7F]);
            self.emit_r13_modrm_disp(xmm_reg, off);
        } else {
            self.emit_lea_r13(11, off);
            let rex = 0x41 | if xmm_reg >= 8 { 0x04 } else { 0 } | 0x01;
            self.emit(&[0xF3, rex, 0x0F, 0x7F]);
            self.emit_reg_indirect_modrm(xmm_reg, 11);
        }
    }
    pub(crate) fn emit_mov_from_mem(&mut self, sz: usize, dst: u8) {
        match sz {
            1 => self.emit(&[0x0F, 0xB6, 0x07]), // movzx eax, byte [rdi]
            2 => self.emit(&[0x0F, 0xB7, 0x07]), // movzx eax, word [rdi]
            4 => self.emit(&[0x8B, 0x07]),       // mov eax, [rdi] (zero-extends)
            _ => self.emit(&[0x48, 0x8B, 0x07]), // mov rax, [rdi]
        }
        if dst != 0 {
            self.mov(dst, 0);
        }
    }
    pub(crate) fn emit_mov_rcx_r10(&mut self, inner_sz: i64) {
        let rex = if inner_sz == 8 { 0x4C } else { 0x44 };
        self.emit(&[rex, 0x89, 0xD1]);
    }
    pub(crate) fn emit_mov_rcx_r11(&mut self, inner_sz: i64) {
        let rex = if inner_sz == 8 { 0x4C } else { 0x44 };
        self.emit(&[rex, 0x89, 0xD9]);
    }
    pub(crate) fn emit_mov_to_mem(&mut self, sz: usize) {
        match sz {
            1 => self.emit(&[0x40, 0x88, 0x37]), // mov [rdi], sil
            2 => self.emit(&[0x66, 0x89, 0x37]), // mov [rdi], si
            4 => self.emit(&[0x89, 0x37]),       // mov [rdi], esi
            _ => self.emit(&[0x48, 0x89, 0x37]), // mov [rdi], rsi
        }
    }
    pub(crate) fn emit_neg(&mut self, sz: usize) {
        match sz {
            1 => self.emit(&[0x40, 0xF6, 0xDE]), // neg sil
            2 => self.emit(&[0x66, 0xF7, 0xDE]), // neg si
            4 => self.emit(&[0xF7, 0xDE]),       // neg esi
            _ => self.emit(&[0x48, 0xF7, 0xDE]), // neg rsi
        }
    }
    pub(crate) fn emit_or_rdx_rsi(&mut self, sz: usize) {
        match sz {
            1 => self.emit(&[0x40, 0x08, 0xF2]), // or dl, sil
            2 => self.emit(&[0x66, 0x09, 0xF2]), // or dx, si
            4 => self.emit(&[0x09, 0xF2]),       // or edx, esi
            _ => self.emit(&[0x48, 0x09, 0xF2]), // or rdx, rsi
        }
    }
    pub(crate) fn emit_store_imm32_mem(&mut self, base: u8, disp: i32, imm: i32) {
        let mut rex: u8 = 0x40;
        if base >= 8 {
            rex |= 0x01; // REX.B
        }
        let rm = base & 7;
        if disp.abs() < 128 {
            self.emit(&[rex, 0xC7, 0x40 | rm]);
            self.emit(&(disp as i8).to_le_bytes());
        } else {
            self.emit(&[rex, 0xC7, 0x80 | rm]);
            self.emit(&disp.to_le_bytes());
        }
        self.emit(&imm.to_le_bytes());
    }
    pub(crate) fn emit_store_reg_mem(&mut self, base: u8, disp: i32, src: u8) {
        let mut rex: u8 = 0x48;
        if src >= 8 {
            rex |= 0x04; // REX.R
        }
        if base >= 8 {
            rex |= 0x01; // REX.B
        }
        let rm = base & 7;
        if disp == 0 && rm != 5 {
            self.emit(&[rex, 0x89, ((src & 7) << 3) | rm]);
        } else if disp.abs() < 128 {
            self.emit(&[rex, 0x89, 0x40 | ((src & 7) << 3) | rm]);
            self.emit(&(disp as i8).to_le_bytes());
        } else {
            self.emit(&[rex, 0x89, 0x80 | ((src & 7) << 3) | rm]);
            self.emit(&disp.to_le_bytes());
        }
    }
    pub(crate) fn emit_string_ptr_slot(
        &mut self,
        rel: Option<(u64, Option<usize>)>,
        data: &[u8],
        rodata_slots: bool,
    ) -> Vec<u8> {
        let data = data.to_vec();
        if !self.string_pool.contains_key(&data) {
            let o = self.rodata.len() as u64;
            self.rodata.extend_from_slice(&data);
            self.string_pool.insert(data.clone(), o);
        }
        let str_off = *self.string_pool.get(&data).unwrap();
        let mut slot = vec![0u8; 8];
        if let Some((off, sec)) = rel {
            if self.relocatable {
                self.pending_data_rodata_rels.push((off, str_off, sec));
            } else if rodata_slots {
                slot.copy_from_slice(&str_off.to_le_bytes());
                self.rodata_string_slots.push(off);
            }
        }
        slot
    }
    pub(crate) fn emit_sub_rax_rcx(&mut self, inner_sz: i64) {
        let rex = if inner_sz == 8 { 0x48 } else { 0x00 };
        if rex != 0 {
            self.emit(&[rex]);
        }
        self.emit(&[0x29, 0xC8]); // sub rax/eax, rcx
    }
    pub(crate) fn emit_sub_rcx_rax(&mut self, inner_sz: i64) {
        let rex = if inner_sz == 8 { 0x48 } else { 0x00 };
        if rex != 0 {
            self.emit(&[rex]);
        }
        self.emit(&[0x29, 0xC1]); // sub rcx/eax? rcx, rax
    }
    pub(crate) fn emit_vex3_prefix(&mut self, r: u8, v: u8, l: u8, pp: u8) {
        let r_inv = if r >= 8 { 0u8 } else { 1u8 };
        let v_inv = (!v) & 0x0F;
        self.emit(&[0xC5, (r_inv << 7) | (v_inv << 3) | (l << 2) | pp]);
    }
    pub(crate) fn emit_vex4_prefix(&mut self, r: u8, b: u8, map: u8, w: u8, v: u8, l: u8, pp: u8) {
        let r_inv = if r >= 8 { 0u8 } else { 1u8 };
        let b_inv = if b >= 8 { 0u8 } else { 1u8 };
        let v_inv = (!v) & 0x0F;
        self.emit(&[
            0xC4,
            (r_inv << 7) | (1u8 << 6) | (b_inv << 5) | (map & 0x1F),
            (w << 7) | (v_inv << 3) | (l << 2) | (pp & 0x03),
        ]);
    }
    pub(crate) fn emit_xor_edx(&mut self, inner_sz: i64) {
        if inner_sz == 8 {
            self.emit(&[0x48, 0x31, 0xD2]);
        } else {
            self.emit(&[0x31, 0xD2]);
        }
    }
    pub(crate) fn emit_xor_rdx_rsi(&mut self, sz: usize) {
        match sz {
            1 => self.emit(&[0x40, 0x30, 0xF2]), // xor dl, sil
            2 => self.emit(&[0x66, 0x31, 0xF2]), // xor dx, si
            4 => self.emit(&[0x31, 0xF2]),       // xor edx, esi
            _ => self.emit(&[0x48, 0x31, 0xF2]), // xor rdx, rsi
        }
    }
}
