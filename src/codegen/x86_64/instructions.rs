// src/codegen/x86_64/instructions.rs
// Low-level x86_64 machine code emission methods for Codegen
// Core instruction encoding: mov, arithmetic, logic, stack, jumps, labels

use crate::ast::Type;
use crate::codegen::x86_64::codegen::JumpKind;
use crate::codegen::x86_64::Codegen;

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

    /// Emit raw bytes into the code buffer.
    pub fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    // ========================================================================
    // Label management
    // ========================================================================

    /// Generate a fresh unique label name.
    pub fn fresh_label(&mut self) -> String {
        let n = self.next_label;
        self.next_label += 1;
        format!(".L{}", n)
    }

    /// Define a label at the current code position.
    pub fn label(&mut self, name: &str) {
        self.labels.insert(name.to_string(), self.code.len() as u64);
    }

    // ========================================================================
    // Jump / branch instructions
    // ========================================================================

    /// Emit a 32-bit relative jump with pending fixup.
    pub fn jmp_rel32(&mut self, target: &str, kind: JumpKind) {
        let at = self.code.len() as u64;
        let (opcode, has_0f) = match kind {
            JumpKind::Jmp => (0xE9, false),
            JumpKind::Je => (0x84, true),
            JumpKind::Jne => (0x85, true),
            JumpKind::Jl => (0x8C, true),
            JumpKind::Jle => (0x8E, true),
            JumpKind::Jg => (0x8F, true),
            JumpKind::Jge => (0x8D, true),
            JumpKind::Jb => (0x82, true),
            JumpKind::Ja => (0x87, true),
            JumpKind::Jns => (0x89, true),
            JumpKind::Jo => (0x80, true),
        };
        if has_0f {
            self.emit(&[0x0F, opcode]);
        } else {
            self.emit(&[opcode]);
        }
        let disp_offset = self.code.len() as u64;
        self.emit(&[0x00, 0x00, 0x00, 0x00]); // placeholder
        let next_ip = self.code.len() as u64;
        self.pending
            .push(crate::codegen::x86_64::codegen::PendingJump {
                at,
                target: target.to_string(),
                kind,
                disp_offset,
                next_ip,
                is_push_imm: false,
            });
    }

    // ========================================================================
    // MOV instructions
    // ========================================================================

    /// mov reg, reg (64-bit)
    pub fn mov(&mut self, dst: u8, src: u8) {
        let rex = encode_rex_full(true, dst, 0, src);
        if let Some(r) = rex {
            self.emit(&[r, 0x8B, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
        } else {
            self.emit(&[0x48, 0x8B, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
        }
    }

    /// mov reg, imm (sign-extended to 64-bit)
    pub fn mov_imm(&mut self, reg: u8, imm: i128) {
        // Check if fits in 32-bit sign-extended (use mov reg, imm32 form)
        let imm64 = imm as i64;
        if imm64 >= i32::MIN as i64 && imm64 <= i32::MAX as i64 {
            let rex = if reg >= 8 { 0x48 | 0x01 } else { 0x48 };
            self.emit(&[rex, 0xC7, 0xC0 | (reg & 7)]);
            self.emit(&(imm64 as i32).to_le_bytes());
        } else {
            // mov r64, imm64 (10-byte form)
            let rex = if reg >= 8 { 0x48 | 0x01 } else { 0x48 };
            self.emit(&[rex, 0xB8 | (reg & 7)]);
            self.emit(&imm64.to_le_bytes());
        }
    }

    // ========================================================================
    // Stack instructions
    // ========================================================================

    /// push reg (64-bit)
    pub fn push(&mut self, reg: u8) {
        if reg >= 8 {
            self.emit(&[0x41, 0x50 | (reg - 8)]);
        } else {
            self.emit(&[0x50 | reg]);
        }
    }

    /// pop reg (64-bit)
    pub fn pop(&mut self, reg: u8) {
        if reg >= 8 {
            self.emit(&[0x41, 0x58 | (reg - 8)]);
        } else {
            self.emit(&[0x58 | reg]);
        }
    }

    /// Store register to RSP-relative stack slot (qword).
    pub fn store_rsp_qword(&mut self, reg: u8, off: usize) {
        let rex_w = if reg >= 8 { 0x4C } else { 0x48 };
        if off == 0 {
            // mov [rsp], reg
            self.emit(&[rex_w, 0x89, 0x04 | ((reg & 7) << 3), 0x24]);
        } else if off < 128 {
            self.emit(&[rex_w, 0x89, 0x44 | ((reg & 7) << 3), 0x24, off as u8]);
        } else {
            self.emit(&[rex_w, 0x89, 0x84 | ((reg & 7) << 3), 0x24]);
            self.emit(&(off as i32).to_le_bytes());
        }
    }

    /// Load from RSP-relative stack slot to register (qword).
    pub fn load_rsp_qword(&mut self, reg: u8, off: usize) {
        let rex_w = if reg >= 8 { 0x4C } else { 0x48 };
        if off == 0 {
            self.emit(&[rex_w, 0x8B, 0x04 | ((reg & 7) << 3), 0x24]);
        } else if off < 128 {
            self.emit(&[rex_w, 0x8B, 0x44 | ((reg & 7) << 3), 0x24, off as u8]);
        } else {
            self.emit(&[rex_w, 0x8B, 0x84 | ((reg & 7) << 3), 0x24]);
            self.emit(&(off as i32).to_le_bytes());
        }
    }

    // ========================================================================
    // R13-relative addressing (frame pointer alternative)
    // ========================================================================

    /// Load effective address: lea reg, [r13 + offset]
    pub fn emit_lea_r13(&mut self, reg: u8, off: i64) {
        // r13 = 13, needs REX.B
        let rex = if reg >= 8 { 0x4D } else { 0x49 };
        if off >= -128 && off <= 127 {
            self.emit(&[
                rex,
                0x8D,
                0x44 | ((reg & 7) << 3),
                0x24 | (13 & 7),
                off as u8,
            ]);
        } else {
            self.emit(&[rex, 0x8D, 0x84 | ((reg & 7) << 3), 0x24 | (13 & 7)]);
            self.emit(&(off as i32).to_le_bytes());
        }
    }

    /// Load from [r13 + offset] to register (mov reg, [r13+off]).
    pub fn emit_mov_mem_r13(&mut self, reg: u8, off: i64) {
        let rex = if reg >= 8 { 0x4D } else { 0x49 };
        if off >= -128 && off <= 127 {
            self.emit(&[
                rex,
                0x8B,
                0x44 | ((reg & 7) << 3),
                0x24 | (13 & 7),
                off as u8,
            ]);
        } else {
            self.emit(&[rex, 0x8B, 0x84 | ((reg & 7) << 3), 0x24 | (13 & 7)]);
            self.emit(&(off as i32).to_le_bytes());
        }
    }

    /// Emit generic instruction with r13+disp addressing via ModRM+SIB.
    /// opcode_bytes includes the REX prefix if needed.
    pub fn emit_r13_modrm_disp(&mut self, reg_field: u8, off: i64) {
        // ModRM: mod = disp_size, reg = reg_field, rm = SIB
        // SIB: scale=0, index=4(SP), base=13(r13) -> base=5 means r13
        // Actually for [r13 + disp] we need SIB since r13 & 7 == 5 (rbp)
        // r13 & 7 = 5, so rm field = 100 for SIB, SIB base = 5
        let base = 13u8;
        let need_sib = (base & 7) == 5; // r13's low 3 bits = 5 = RBP encoding
        let rex_b = if base >= 8 { 0x01 } else { 0x00 };
        let rex_r = if reg_field >= 8 { 0x04 } else { 0x00 };
        let rex = 0x40 | rex_r | rex_b;
        self.emit(&[rex]);

        if off >= -128 && off <= 127 {
            if need_sib {
                self.emit(&[0x40 | (reg_field & 7) << 3 | 0x04]); // mod=01, rm=100(SIB)
                self.emit(&[0x24 | (base & 7)]); // SIB: scale=00, index=100(SP), base=base&7
                self.emit(&[off as u8]);
            } else {
                self.emit(&[0x40 | (reg_field & 7) << 3 | (base & 7)]);
                self.emit(&[off as u8]);
            }
        } else {
            if need_sib {
                self.emit(&[0x80 | (reg_field & 7) << 3 | 0x04]); // mod=10, rm=100
                self.emit(&[0x24 | (base & 7)]);
                self.emit(&(off as i32).to_le_bytes());
            } else {
                self.emit(&[0x80 | (reg_field & 7) << 3 | (base & 7)]);
                self.emit(&(off as i32).to_le_bytes());
            }
        }
    }

    /// Emit register-indirect ModRM: [base_reg]
    pub fn emit_reg_indirect_modrm(&mut self, reg_field: u8, base: u8) {
        let rex_b = if base >= 8 { 0x01 } else { 0x00 };
        let rex_r = if reg_field >= 8 { 0x04 } else { 0x00 };
        let rex = 0x40 | rex_r | rex_b;
        self.emit(&[rex]);
        let need_sib = (base & 7) == 5 || (base & 7) == 4;
        if need_sib {
            self.emit(&[0x00 | (reg_field & 7) << 3 | 0x04]); // mod=00, rm=100(SIB)
            self.emit(&[0x24 | (base & 7)]); // SIB: scale=00, index=SP, base=base
        } else {
            self.emit(&[0x00 | (reg_field & 7) << 3 | (base & 7)]);
        }
    }

    // ========================================================================
    // Stack frame variable management
    // ========================================================================

    /// Get the r13-relative offset of a local variable.
    pub fn var_offset(&mut self, name: &str) -> i64 {
        self.var_map.get(name).map(|(off, _)| *off).unwrap_or(0)
    }

    /// Load variable from stack to register (qword).
    pub fn load_stack(&mut self, reg: u8, off: i64) {
        let rex = if reg >= 8 { 0x4D } else { 0x49 };
        if off >= -128 && off <= 127 {
            self.emit(&[
                rex,
                0x8B,
                0x44 | ((reg & 7) << 3),
                0x24 | (13 & 7),
                off as u8,
            ]);
        } else {
            self.emit(&[rex, 0x8B, 0x84 | ((reg & 7) << 3), 0x24 | (13 & 7)]);
            self.emit(&(off as i32).to_le_bytes());
        }
    }

    /// Store register to stack variable (qword).
    pub fn store_stack(&mut self, reg: u8, off: i64) {
        let rex = if reg >= 8 { 0x4D } else { 0x49 };
        if off >= -128 && off <= 127 {
            self.emit(&[
                rex,
                0x89,
                0x44 | ((reg & 7) << 3),
                0x24 | (13 & 7),
                off as u8,
            ]);
        } else {
            self.emit(&[rex, 0x89, 0x84 | ((reg & 7) << 3), 0x24 | (13 & 7)]);
            self.emit(&(off as i32).to_le_bytes());
        }
    }

    /// Load from stack with specific size and zero/sign extend to 64-bit.
    pub fn load_stack_sized(&mut self, reg: u8, off: i64, sz: usize) {
        match sz {
            1 => {
                // movzx reg, byte [r13+off]
                let rex = if reg >= 8 { 0x4D } else { 0x49 };
                if off >= -128 && off <= 127 {
                    self.emit(&[
                        rex,
                        0x0F,
                        0xB6,
                        0x44 | ((reg & 7) << 3),
                        0x24 | (13 & 7),
                        off as u8,
                    ]);
                } else {
                    self.emit(&[rex, 0x0F, 0xB6, 0x84 | ((reg & 7) << 3), 0x24 | (13 & 7)]);
                    self.emit(&(off as i32).to_le_bytes());
                }
            },
            2 => {
                let rex = if reg >= 8 { 0x4D } else { 0x49 };
                if off >= -128 && off <= 127 {
                    self.emit(&[
                        rex,
                        0x0F,
                        0xB7,
                        0x44 | ((reg & 7) << 3),
                        0x24 | (13 & 7),
                        off as u8,
                    ]);
                } else {
                    self.emit(&[rex, 0x0F, 0xB7, 0x84 | ((reg & 7) << 3), 0x24 | (13 & 7)]);
                    self.emit(&(off as i32).to_le_bytes());
                }
            },
            4 => {
                // mov r32, [r13+off] (zero-extends to 64-bit)
                let rex = if reg >= 8 { 0x45 } else { 0x41 };
                if off >= -128 && off <= 127 {
                    self.emit(&[
                        rex,
                        0x8B,
                        0x44 | ((reg & 7) << 3),
                        0x24 | (13 & 7),
                        off as u8,
                    ]);
                } else {
                    self.emit(&[rex, 0x8B, 0x84 | ((reg & 7) << 3), 0x24 | (13 & 7)]);
                    self.emit(&(off as i32).to_le_bytes());
                }
            },
            8 => self.load_stack(reg, off),
            _ => self.load_stack(reg, off),
        }
    }

    /// Store register to stack with specific size.
    pub fn emit_store(&mut self, reg: u8, off: i64, sz: usize) {
        match sz {
            1 => {
                let rex = if reg >= 8 { 0x41 } else { 0x00 };
                let rex = rex | 0x40;
                if off >= -128 && off <= 127 {
                    self.emit(&[
                        rex,
                        0x88,
                        0x44 | ((reg & 7) << 3),
                        0x24 | (13 & 7),
                        off as u8,
                    ]);
                } else {
                    self.emit(&[rex, 0x88, 0x84 | ((reg & 7) << 3), 0x24 | (13 & 7)]);
                    self.emit(&(off as i32).to_le_bytes());
                }
            },
            2 => {
                let rex = if reg >= 8 { 0x41 } else { 0x00 };
                let rex = rex | 0x40 | 0x08; // REX.W for 16-bit? No, 66h prefix
                if off >= -128 && off <= 127 {
                    self.emit(&[
                        rex,
                        0x66,
                        0x89,
                        0x44 | ((reg & 7) << 3),
                        0x24 | (13 & 7),
                        off as u8,
                    ]);
                } else {
                    self.emit(&[rex, 0x66, 0x89, 0x84 | ((reg & 7) << 3), 0x24 | (13 & 7)]);
                    self.emit(&(off as i32).to_le_bytes());
                }
            },
            4 => {
                let rex = if reg >= 8 { 0x41 } else { 0x00 };
                if off >= -128 && off <= 127 {
                    self.emit(&[
                        rex,
                        0x89,
                        0x44 | ((reg & 7) << 3),
                        0x24 | (13 & 7),
                        off as u8,
                    ]);
                } else {
                    self.emit(&[rex, 0x89, 0x84 | ((reg & 7) << 3), 0x24 | (13 & 7)]);
                    self.emit(&(off as i32).to_le_bytes());
                }
            },
            8 => self.store_stack(reg, off),
            _ => self.store_stack(reg, off),
        }
    }

    /// Load a value from memory at [r13+off] with proper extension based on type.
    pub fn emit_load_mem_typed(&mut self, sz: usize, ty: &Type) {
        // result in rax (0)
        let signed = Self::is_signed_int(ty);
        match (sz, signed) {
            (1, true) => {
                // movsx rax, byte [r13+off] -> but off is variable
                // Since we don't know the off here, this is a stub that should be refactored
                self.emit(&[0x49, 0x0F, 0xBE, 0x04, 0x24]); // movsx rax, byte [r13]
            },
            (1, false) => {
                self.emit(&[0x49, 0x0F, 0xB6, 0x04, 0x24]); // movzx rax, byte [r13]
            },
            (2, true) => {
                self.emit(&[0x49, 0x0F, 0xBF, 0x04, 0x24]); // movsx rax, word [r13]
            },
            (2, false) => {
                self.emit(&[0x49, 0x0F, 0xB7, 0x04, 0x24]); // movzx rax, word [r13]
            },
            (4, true) => {
                self.emit(&[0x49, 0x63, 0x04, 0x24]); // movsxd rax, dword [r13]
            },
            (4, false) => {
                self.emit(&[0x45, 0x8B, 0x04, 0x24]); // mov eax, [r13] (zero extends)
            },
            _ => {
                self.emit(&[0x49, 0x8B, 0x04, 0x24]); // mov rax, [r13]
            },
        }
    }

    // ========================================================================
    // RSP manipulation
    // ========================================================================

    /// Subtract from RSP (allocate stack space).
    pub fn emit_sub_rsp(&mut self, bytes: i64) {
        if bytes <= 0 {
            return;
        }
        if bytes <= 127 {
            self.emit(&[0x48, 0x83, 0xEC, bytes as u8]);
        } else if bytes <= i32::MAX as i64 {
            self.emit(&[0x48, 0x81, 0xEC]);
            self.emit(&(bytes as i32).to_le_bytes());
        } else {
            self.mov_imm(11, bytes as i128);
            self.emit(&[0x4C, 0x29, 0xDC]); // sub rsp, r11
        }
    }

    /// Add to RSP (deallocate stack space).
    pub fn emit_add_rsp(&mut self, bytes: i64) {
        if bytes <= 0 {
            return;
        }
        if bytes <= 127 {
            self.emit(&[0x48, 0x83, 0xC4, bytes as u8]);
        } else if bytes <= i32::MAX as i64 {
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

    /// imul reg, reg (signed multiply, low 64 bits)
    pub fn imul_reg(&mut self, dst: u8, src: u8) {
        let rex = encode_rex_full(true, dst, 0, src);
        if let Some(r) = rex {
            self.emit(&[r, 0x0F, 0xAF, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
        } else {
            self.emit(&[0x48, 0x0F, 0xAF, 0xC0 | ((dst & 7) << 3) | (src & 7)]);
        }
    }

    /// imul reg, imm (signed multiply by immediate)
    pub fn imul_imm(&mut self, reg: u8, imm: i128) {
        let imm64 = imm as i64;
        let rex = if reg >= 8 { 0x49 } else { 0x48 };
        if imm64 >= i8::MIN as i64 && imm64 <= i8::MAX as i64 {
            self.emit(&[rex, 0x6B, 0xC0 | (reg & 7), imm64 as i8 as u8]);
        } else {
            self.emit(&[rex, 0x69, 0xC0 | (reg & 7)]);
            self.emit(&(imm64 as i32).to_le_bytes());
        }
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

    /// Call a label (relative, with pending fixup).
    pub fn call_label(&mut self, target: &str) {
        let at = self.code.len() as u64;
        self.emit(&[0xE8]);
        let _disp_offset = self.code.len() as u64;
        self.emit(&[0x00, 0x00, 0x00, 0x00]);
        let _next_ip = self.code.len() as u64;
        self.pending_func_rels.push((at, target.to_string()));
        // Also track as pending jump-like for resolution
        // We'll use pending_func_rels for function call relocations
    }

    /// Call an external function (helper that wraps call_label).
    pub fn emit_call_extern(&mut self, name: &str) {
        self.call_label(name);
    }

    // ========================================================================
    // Global variable support
    // ========================================================================

    /// Check if a variable name refers to a global (not local).
    pub fn is_global_not_local(&self, name: &str) -> bool {
        !self.var_map.contains_key(name)
    }

    /// Check if variable is a global (same as is_global_not_local for now).
    pub fn is_global_var(&self, name: &str) -> bool {
        self.globals_raw.contains_key(name)
    }

    /// Emit LEA of global variable address into register (RIP-relative).
    pub fn emit_global_addr(&mut self, reg: u8, name: &str) {
        let rex = if reg >= 8 { 0x4C } else { 0x48 };
        // lea reg, [rip + disp32]
        self.emit(&[rex, 0x8D, 0x05 | ((reg & 7) << 3)]);
        let disp_pos = self.code.len() as u64;
        self.emit(&[0x00, 0x00, 0x00, 0x00]); // placeholder
        self.pending_data_rels.push((disp_pos, name.to_string()));
    }

    /// Emit mov reg, [rip+disp32] for global load.
    pub fn emit_global_load(&mut self, reg: u8, name: &str) {
        let rex = if reg >= 8 { 0x4C } else { 0x48 };
        self.emit(&[rex, 0x8B, 0x05 | ((reg & 7) << 3)]);
        let disp_pos = self.code.len() as u64;
        self.emit(&[0x00, 0x00, 0x00, 0x00]);
        self.pending_data_rels.push((disp_pos, name.to_string()));
    }

    /// Emit mov [rip+disp32], reg for global store.
    pub fn emit_global_store(&mut self, reg: u8, name: &str) {
        let rex = if reg >= 8 { 0x4C } else { 0x48 };
        self.emit(&[rex, 0x89, 0x05 | ((reg & 7) << 3)]);
        let disp_pos = self.code.len() as u64;
        self.emit(&[0x00, 0x00, 0x00, 0x00]);
        self.pending_data_rels.push((disp_pos, name.to_string()));
    }

    // ========================================================================
    // Local variable registration / slot allocation
    // ========================================================================

    /// Register a local variable and allocate stack slot.
    /// Returns the r13-relative offset of the new variable.
    pub fn register_var(&mut self, name: &str, ty: &crate::ast::Type) -> i64 {
        let sz = ty.size().max(1);
        let slots = ((sz + 7) / 8) as i64; // round up to 8-byte slots
        let offset = -(self.var_map_total_slots + slots) * 8;
        self.var_map_total_slots += slots;
        self.var_map.insert(name.to_string(), (offset, ty.clone()));
        // Update frame size
        if -offset > self.frame_size {
            self.frame_size = -offset;
        }
        offset
    }

    /// Allocate a temporary slot (for sret, temporaries, etc.).
    /// Returns the r13-relative offset.
    pub fn alloc_sret_slot(&mut self, sz: usize) -> i64 {
        let sz = sz.max(1);
        let slots = ((sz + 7) / 8) as i64;
        let offset = -(self.var_map_total_slots + slots) * 8;
        self.var_map_total_slots += slots;
        if -offset > self.frame_size {
            self.frame_size = -offset;
        }
        offset
    }

    /// Allocate a local variable slot (generic name).
    pub fn alloc_local(&mut self, sz: usize) -> i64 {
        self.alloc_sret_slot(sz)
    }

    // ========================================================================
    // XMM / SSE helpers
    // ========================================================================

    /// Emit xmm0 load from r13+disp with given prefix bytes.
    /// prefix contains the REX+opcode prefix (e.g., &[0xF3, 0x49, 0x0F, 0x10]).
    pub fn emit_xmm0_r13_disp(&mut self, prefix: &[u8], off: i64) {
        self.emit(prefix);
        // xmm0 = reg 0, r13 = 13
        if off >= -128 && off <= 127 {
            // ModRM: mod=01, reg=000(xmm0), rm=100(SIB)
            self.emit(&[0x44, 0x24 | (13 & 7), off as u8]);
        } else {
            self.emit(&[0x84, 0x24 | (13 & 7)]);
            self.emit(&(off as i32).to_le_bytes());
        }
    }

    /// Store XMM register to RSP-relative slot.
    pub fn emit_xmm_store_rsp(&mut self, pfx: u8, reg: u8, off: usize) {
        // pfx: 0xF3 (movss), 0xF2 (movsd), 0x66 (movdqa/movups) etc.
        let rex_x = if reg >= 8 { 0x41 } else { 0x00 };
        let rex = 0x40 | rex_x;
        self.emit(&[rex, pfx, 0x0F, 0x11]); // MOVSS/MOVSD/MOVDQA ...
        if off < 128 {
            self.emit(&[0x44 | ((reg & 7) << 3), 0x24, off as u8]);
        } else {
            self.emit(&[0x84 | ((reg & 7) << 3), 0x24]);
            self.emit(&(off as i32).to_le_bytes());
        }
    }

    /// Load XMM register from RSP-relative slot.
    pub fn emit_xmm_load_rsp(&mut self, pfx: u8, reg: u8, off: usize) {
        let rex_x = if reg >= 8 { 0x41 } else { 0x00 };
        let rex = 0x40 | rex_x;
        self.emit(&[rex, pfx, 0x0F, 0x10]);
        if off < 128 {
            self.emit(&[0x44 | ((reg & 7) << 3), 0x24, off as u8]);
        } else {
            self.emit(&[0x84 | ((reg & 7) << 3), 0x24]);
            self.emit(&(off as i32).to_le_bytes());
        }
    }

    /// Load XMM register from stack (r13-relative) at given offset.
    pub fn load_stack_xmm(&mut self, reg: u8, off: i64, sz: usize, _aligned: bool) {
        let pfx = match sz {
            4 => 0xF3,  // movss
            8 => 0xF2,  // movsd
            16 => 0x66, // movdqa
            _ => 0xF3,
        };
        let rex = if reg >= 8 { 0x45 } else { 0x41 };
        self.emit(&[rex, pfx, 0x0F, 0x10]);
        if off >= -128 && off <= 127 {
            self.emit(&[0x44 | ((reg & 7) << 3), 0x24 | (13 & 7), off as u8]);
        } else {
            self.emit(&[0x84 | ((reg & 7) << 3), 0x24 | (13 & 7)]);
            self.emit(&(off as i32).to_le_bytes());
        }
    }

    /// Store 128-bit XMM to stack.
    pub fn store_xmm128_stack(&mut self, reg: u8, off: i64) {
        let rex = if reg >= 8 { 0x45 } else { 0x41 };
        self.emit(&[rex, 0xF3, 0x0F, 0x7F]); // movups xmm, [mem] for 128-bit store
        if off >= -128 && off <= 127 {
            self.emit(&[0x44 | ((reg & 7) << 3), 0x24 | (13 & 7), off as u8]);
        } else {
            self.emit(&[0x84 | ((reg & 7) << 3), 0x24 | (13 & 7)]);
            self.emit(&(off as i32).to_le_bytes());
        }
    }

    /// Store 64-bit XMM to stack.
    pub fn store_xmm64_stack(&mut self, reg: u8, off: i64) {
        let rex = if reg >= 8 { 0x45 } else { 0x41 };
        self.emit(&[rex, 0xF2, 0x0F, 0x11]); // movsd xmm, [mem]
        if off >= -128 && off <= 127 {
            self.emit(&[0x44 | ((reg & 7) << 3), 0x24 | (13 & 7), off as u8]);
        } else {
            self.emit(&[0x84 | ((reg & 7) << 3), 0x24 | (13 & 7)]);
            self.emit(&(off as i32).to_le_bytes());
        }
    }

    /// movq xmm, gp_reg (move 64-bit from GP to XMM)
    pub fn emit_movq_gp_to_xmm(&mut self, xmm_reg: u8, gp_reg: u8) {
        let rex_r = if xmm_reg >= 8 { 0x04 } else { 0x00 };
        let rex_b = if gp_reg >= 8 { 0x01 } else { 0x00 };
        let rex = 0x40 | 0x08 | rex_r | rex_b; // REX.W
        self.emit(&[rex, 0x66, 0x0F, 0x6E]);
        self.emit(&[0xC0 | ((xmm_reg & 7) << 3) | (gp_reg & 7)]);
    }

    /// Flip the sign bit of XMM float (single or double precision).
    pub fn flip_sign_xmm(&mut self, _reg: u8, ty: &crate::ast::Type) {
        // XOR with a constant in memory (sign bit mask)
        // For float: 0x80000000 (32-bit), for double: 0x8000000000000000 (64-bit)
        // We'll use a simple approach: load mask from rodata
        let sz = ty.size();
        if sz == 4 {
            self.emit(&[0xF3, 0x0F, 0x55, 0xC0]); // xorps xmm0, xmm0 (placeholder)
        } else {
            self.emit(&[0xF2, 0x0F, 0x55, 0xC0]); // xorpd xmm0, xmm0 (placeholder)
        }
    }

    /// If scalar type, load it into xmm0.
    pub fn emit_scalar_to_xmm0_if_needed(&mut self, _e: &crate::ast::Expr, _ty: &crate::ast::Type) {
        // Stub - actual implementation is in expression codegen
        // This is called after the value is already in rax; move to xmm0 if float
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

    /// Maybe align a jump target.
    pub fn maybe_align_jump(&mut self) {
        // Stub: align to 16 bytes if desired
    }

    /// Maybe align a loop top.
    pub fn maybe_align_loop(&mut self) {
        // Stub: align to 16 bytes if desired
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

    /// Resolve global key from variable name.
    pub fn resolve_global_key(&self, name: &str) -> Option<String> {
        if self.globals_raw.contains_key(name) {
            Some(name.to_string())
        } else {
            None
        }
    }

    /// Resolve GNU asm label: if the symbol carries an `__asm__("name")`
    /// alias, return the alias name; otherwise None.
    pub fn resolve_asm_label<'a>(&'a self, c_name: &'a str) -> &'a str {
        self.asm_label_map
            .get(c_name)
            .map(|s| s.as_str())
            .unwrap_or(c_name)
    }

    /// RSP (current stack pointer value - stored as field or computed)
    pub fn rsp(&self) -> u64 {
        // For codegen, this returns the current RSP value at this point
        // This is used for debug info and such
        // Return 0 as placeholder
        0
    }

    /// Get vector temp base offset for the given slot.
    /// `slot` selects the per-expression temporary area and `size` is the
    /// vector byte width; each slot occupies `size` bytes below/above the
    /// vector temp region base (r13-relative).
    pub fn vector_temp_base(&self, slot: i64, size: usize) -> i64 {
        self.vec_temp_off + slot * size as i64
    }

    /// Allocate callee-saved register.
    pub fn alloc_callee_saved(&mut self) -> Option<u8> {
        if self.callee_saved_used < self.callee_saved_pool.len() as u8 {
            let reg = self.callee_saved_pool[self.callee_saved_used as usize];
            self.callee_saved_used += 1;
            Some(reg)
        } else {
            None
        }
    }

    /// Free callee-saved register.
    pub fn free_callee_saved(&mut self, _reg: u8) {
        // Stub
    }
}
