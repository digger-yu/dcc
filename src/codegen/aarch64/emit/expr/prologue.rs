// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::aarch64::codegen::{Aarch64Codegen, BranchKind};
use crate::codegen::aarch64::insn::Insn;
use crate::codegen::shared::elf::{
    self, ElfRela, ElfSym, R_AARCH64_CALL26, STB_GLOBAL, STT_FUNC, STT_OBJECT,
};

impl Aarch64Codegen {
    pub(crate) fn sub_sp_imm(&mut self, bytes: u32) {
        if bytes == 0 {
            return;
        }
        if bytes <= 0xfff {
            let insn = 0xd100_0000u32 | ((bytes & 0xfff) << 10) | (31 << 5) | 31;
            self.emit_insn(&Insn::raw(insn));
        }
    }

    /// -fstack-clash-protection: emit page-sized probes when allocating a
    /// large stack frame.  Each probe stores a zero byte at [sp] to trigger
    /// a fault on the guard page before the kernel maps it.
    pub(crate) fn emit_stack_clash_probes(&mut self, frame_size: u32) {
        let page: u32 = 4096;
        let remainder = frame_size % page;
        let pages = frame_size / page;
        // First: sub sp, sp, #remainder (if any)
        if remainder > 0 {
            self.sub_sp_imm(remainder);
        }
        if pages > 0 {
            // Probe loop using x16 (ip0, ABI scratch) as counter
            // movz x16, #pages
            let movz = 0x52800010u32 | ((pages & 0xffff) << 5);
            self.emit_insn(&Insn::raw(movz));
            // .probe_loop:
            let loop_start = self.pos();
            // sub sp, sp, #4096  (1 << 12, using shift encoding)
            let sub_insn = 0xd1000000u32 | (1u32 << 22) | (1u32 << 10) | (31u32 << 5) | 31;
            self.emit_insn(&Insn::raw(sub_insn));
            // strb wzr, [sp]  — probe the page
            let strb = 0x3900001fu32;
            self.emit_insn(&Insn::raw(strb));
            // sub x16, x16, #1
            let sub_cnt = 0xd1000010u32 | (1u32 << 10);
            self.emit_insn(&Insn::raw(sub_cnt));
            // cbnz x16, .probe_loop
            let loop_end = self.pos();
            let offset = (loop_start as i64 - loop_end as i64) as i32;
            let cbnz = 0xb5000010u32 | (((offset as u32 >> 2) & 0x7ffff) << 5);
            self.emit_insn(&Insn::raw(cbnz));
        }
    }

    pub(crate) fn add_sp_imm(&mut self, bytes: u32) {
        if bytes == 0 {
            return;
        }
        if bytes <= 0xfff {
            let insn = 0x9100_0000u32 | ((bytes & 0xfff) << 10) | (31 << 5) | 31;
            self.emit_insn(&Insn::raw(insn));
        }
    }

    pub(crate) fn neg_w0(&mut self) {
        self.emit_insn(&Insn::raw(0x4b000000));
    }

    pub(crate) fn mvn_w0(&mut self) {
        self.emit_insn(&Insn::raw(0x2a200320));
    }

    pub(crate) fn cmp_w0_zero(&mut self) {
        self.emit_insn(&Insn::raw(0x7100001f));
        self.emit_insn(&Insn::raw(0x1a9f07e0));
    }

    pub(crate) fn cmp_w0_w1(&mut self) {
        self.emit_insn(&Insn::raw(0x6b01001f));
    }

    /// cmp wn, #imm12 — 32 位立即数比较 (subs wzr, wn, #imm)
    pub(crate) fn emit_cmp_imm_w(&mut self, rn: u8, imm: u32) {
        let insn = 0x7100001fu32 | (imm << 10) | ((rn as u32 & 0x1f) << 5);
        self.emit_insn(&Insn::raw(insn));
    }

    /// add wd, wn, #imm12 — 32 位立即加法
    pub(crate) fn emit_add_imm_w(&mut self, dst: u8, src: u8, imm: u32) {
        let insn = 0x11000000u32 | (imm << 10) | ((src as u32 & 0x1f) << 5) | (dst as u32 & 0x1f);
        self.emit_insn(&Insn::raw(insn));
    }

    pub(crate) fn cbz_w0(&mut self, label: &str) {
        let pos = self.pos();
        self.emit_insn(&Insn::raw(0x34000000));
        if let Some(&target) = self.labels.get(label) {
            self.patch_cbz(pos, target);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::Cbz));
        }
    }

    pub(crate) fn b(&mut self, label: &str) {
        let pos = self.pos();
        self.emit_insn(&Insn::raw(0x14000000));
        if let Some(&target) = self.labels.get(label) {
            self.patch_b26(pos, target);
        } else {
            self.pending_branches
                .push((pos, label.to_string(), BranchKind::B));
        }
    }

    pub(crate) fn patch_b26(&mut self, at: u64, target: u64) {
        let pc = at + 4;
        let imm26 = ((target as i64 - pc as i64) >> 2) as u32 & 0x03ff_ffff;
        let insn = 0x1400_0000u32 | imm26;
        let off = at as usize;
        if off + 4 <= self.code.len() {
            self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    pub(crate) fn patch_cbz(&mut self, at: u64, target: u64) {
        let pc = at + 4;
        let imm19 = ((target as i64 - pc as i64) >> 2) as u32 & 0x7ffff;
        let insn = 0x3400_0000u32 | imm19;
        let off = at as usize;
        if off + 4 <= self.code.len() {
            self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    pub(crate) fn ret(&mut self) {
        // Stack protector epilogue: verify canary BEFORE frame teardown
        if self._target.stack_protector && self.canary_slot_off > 0 {
            self.emit_stack_chk_epilogue(self.canary_slot_off);
        }
        if self.mte_active() {
            // MTE 收尾: 恢复入参 x19, 清除 sp 的 tag, 再释放帧。
            // 返回时 sp 为 untagged, 调用方 (MTE 函数) 会通过其 x19 恢复自身 tag,
            // 非 MTE 调用方 (_start / libc) 本就期望 untagged sp。
            self.ldr_xt_sp(19, self.mte_save_off); // ldr x19, [sp, #mte_save_off]
            if self.lr_off >= 0 {
                self.ldr_xt_sp(30, self.lr_off); // ldr x30, [sp, #lr_off] 恢复 lr
            }
            self.emit_insn(&Insn::raw(0x9280dfff)); // and sp, sp, #0x00ffffffffffffff
            self.add_sp_imm(self.frame_bytes as u32);
        } else if self.frame_bytes > 0 {
            if self.lr_off >= 0 {
                self.ldr_xt_sp(30, self.lr_off); // ldr x30, [sp, #lr_off] 恢复 lr
            }
            self.add_sp_imm(self.frame_bytes as u32);
        }
        self.emit_insn(&Insn::raw(0xd65f03c0));
    }

    pub(crate) fn svc_zero(&mut self) {
        self.emit_insn(&Insn::raw(0xd4000001));
    }

    pub(crate) fn emit_bl_placeholder(&mut self) {
        self.emit_insn(&Insn::raw(0x94000000));
    }

    /// 校验 BL 偏移是否在 ±128MiB 范围内 (imm26 × 4)
    pub(crate) fn bl_offset_in_range(diff: i64) -> bool {
        (-(1 << 27)..(1 << 27)).contains(&diff)
    }

    pub(crate) fn emit_bl_to(&mut self, name: &str) {
        // MTE 调用点: 调用前清除 sp 的 tag (外部非 MTE 代码按 untagged sp 运行),
        // 返回后通过 x19 恢复本帧 tag, 保证后续 [sp,#off] 局部访问通过硬件检查。
        let untag = self.mte_active();
        if untag {
            self.mte_untag_sp_for_call();
        }
        if let Some(&target) = self.labels.get(name) {
            let pos = self.pos();
            self.emit_bl_placeholder();
            self.patch_bl(pos, target);
        } else if self.relocatable {
            let pos = self.pos();
            self.emit_bl_placeholder();
            let idx = self.ensure_sym(name);
            self.relocations.push(ElfRela {
                offset: pos,
                sym_index: idx,
                rel_type: R_AARCH64_CALL26,
                addend: 0,
            });
        } else {
            let pos = self.pos();
            self.emit_bl_placeholder();
            self.pending_bl_calls.push((pos, name.to_string()));
        }
        if untag {
            self.mte_retag_sp_after_call();
        }
    }

    pub(crate) fn ensure_sym(&mut self, name: &str) -> u32 {
        if let Some(&idx) = self.sym_index.get(name) {
            return idx;
        }
        self.symbols.push(ElfSym {
            name: name.to_string(),
            section: 0,
            value: 0,
            size: 0,
            bind: STB_GLOBAL,
            ty: STT_FUNC,
            visibility: elf::STV_DEFAULT,
        });
        let idx = self.symbols.len() as u32;
        self.sym_index.insert(name.to_string(), idx);
        idx
    }

    /// Like ensure_sym but creates an STT_OBJECT symbol (for data references).
    pub(crate) fn ensure_sym_object(&mut self, name: &str) -> u32 {
        if let Some(&idx) = self.sym_index.get(name) {
            return idx;
        }
        self.symbols.push(ElfSym {
            name: name.to_string(),
            section: 0,
            value: 0,
            size: 0,
            bind: STB_GLOBAL,
            ty: STT_OBJECT,
            visibility: elf::STV_DEFAULT,
        });
        let idx = self.symbols.len() as u32;
        self.sym_index.insert(name.to_string(), idx);
        idx
    }

    /// Emit ADRP Xd, __stack_chk_guard with proper relocation.
    /// Used by stack protector prologue/epilogue.
    pub(crate) fn emit_adrp_stack_chk_guard(&mut self, rd: u8) {
        let pos = self.pos();
        // ADRP Xd, #0 (placeholder, patched by linker via relocation)
        // Encoding: 1 immlo 10000 immhi Rd
        // immlo=00, immhi=0000000000000000000
        let adrp = 0x90000000u32 | (rd as u32 & 0x1f);
        self.emit_insn(&Insn::raw(adrp));
        if self.relocatable {
            let idx = self.ensure_sym("__stack_chk_guard");
            self.relocations.push(ElfRela {
                offset: pos,
                sym_index: idx,
                rel_type: elf::R_AARCH64_ADR_PREL_PG_HI21,
                addend: 0,
            });
        }
    }

    /// Emit LDR Xt, [Xd, :lo12:__stack_chk_guard] with proper relocation.
    /// Loads the 64-bit canary value from the global.
    pub(crate) fn emit_ldr_stack_chk_guard(&mut self, rt: u8, rn: u8) {
        let pos = self.pos();
        // LDR Xt, [Xn, #imm] (unsigned offset, 64-bit)
        // imm12 = 0 (placeholder, patched by linker)
        let ldr = 0xf9400000u32 | (rt as u32 & 0x1f) | ((rn as u32 & 0x1f) << 5);
        self.emit_insn(&Insn::raw(ldr));
        if self.relocatable {
            let idx = self.ensure_sym("__stack_chk_guard");
            self.relocations.push(ElfRela {
                offset: pos,
                sym_index: idx,
                rel_type: elf::R_AARCH64_LDST64_ABS_LO12_NC,
                addend: 0,
            });
        }
    }

    /// Emit CMP Xn, Xm (alias of SUBS XZR, Xn, Xm)
    pub(crate) fn emit_cmp_x_reg(&mut self, rn: u8, rm: u8) {
        // SUBS XZR, Xn, Xm => 0xEB000000 | Rm<<16 | Rn<<5 | 31
        let subs = 0xeb000000u32 | ((rm as u32 & 0x1f) << 16) | ((rn as u32 & 0x1f) << 5) | 31;
        self.emit_insn(&Insn::raw(subs));
    }

    /// Emit B.NE label (branch if not equal, condition code 0x1)
    pub(crate) fn emit_bne(&mut self, label: &str) {
        let pos = self.pos();
        // B.cond: 0x54000000 | imm19<<5 | cond
        // cond=0001 (NE)
        let bne = 0x54000001u32;
        self.emit_insn(&Insn::raw(bne));
        if let Some(&target) = self.labels.get(label) {
            self.patch_bcond(pos, target, 0x1);
        } else {
            self.pending_bcond.push((pos, label.to_string(), 0x1));
        }
    }

    /// Stack protector prologue: load canary from __stack_chk_guard
    /// and store at [sp, #canary_off]. Uses x16 as scratch.
    pub(crate) fn emit_stack_chk_prologue(&mut self, canary_off: i32) {
        self.emit_adrp_stack_chk_guard(16); // adrp x16, __stack_chk_guard
        self.emit_ldr_stack_chk_guard(16, 16); // ldr x16, [x16, :lo12:]
        self.str_xt_sp(16, canary_off); // str x16, [sp, #canary_off]
    }

    /// Stack protector epilogue: verify canary at [sp, #canary_off]
    /// against __stack_chk_guard. Calls __stack_chk_fail on mismatch.
    /// Uses x16, x17 as scratch. Must be called BEFORE frame teardown.
    pub(crate) fn emit_stack_chk_epilogue(&mut self, canary_off: i32) {
        let fail_label = self.fresh_label(".Lstack_chk_fail_");
        // Load stored canary
        self.ldr_xt_sp(16, canary_off); // ldr x16, [sp, #canary_off]
                                        // Load expected canary
        self.emit_adrp_stack_chk_guard(17); // adrp x17, __stack_chk_guard
        self.emit_ldr_stack_chk_guard(17, 17); // ldr x17, [x17, :lo12:]
                                               // Compare
        self.emit_cmp_x_reg(16, 17); // cmp x16, x17
                                     // Branch to fail if mismatch
        self.emit_bne(&fail_label);
        // Normal path continues (caller will do frame teardown + ret)
        // Fail path: call __stack_chk_fail (never returns)
        self.label(&fail_label);
        self.emit_bl_to("__stack_chk_fail");
    }
}
