// AArch64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::aarch64::codegen::Aarch64Codegen;
use crate::codegen::aarch64::insn::Insn;

impl Aarch64Codegen {
    pub(crate) fn patch_bl(&mut self, at: u64, target: u64) {
        // AArch64 B/BL 的立即数相对分支指令自身地址 (PC == at), 不是 at+4
        // (那是 ARM32 的 PC+8 流水线偏移)。旧代码误用 at+4 使同 TU 直补丁的
        // 调用目标总是少 4 字节 (实测 bl 落到 callee 前一条指令)。
        let pc = at;
        let diff = target as i64 - pc as i64;
        debug_assert!(Self::bl_offset_in_range(diff), "BL offset out of range: diff={diff}");
        let imm26 = ((diff >> 2) as u32) & 0x03ff_ffff;
        let insn = 0x9400_0000u32 | imm26;
        let off = at as usize;
        self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
    }

    pub(crate) fn emit_adr_placeholder(&mut self, rd: u8) {
        let insn = 0x1000_0000u32 | (rd as u32);
        self.emit_insn(&Insn::raw(insn));
    }

    /// 校验 ADR 偏移是否在 ±1MiB 范围内 (immhi[20:0] : immlo[1:0])
    pub(crate) fn adr_offset_in_range(diff: i64) -> bool {
        (-(1 << 20)..(1 << 20)).contains(&diff)
    }

    pub(crate) fn emit_ldr_s0_u32_placeholder(&mut self) {
        // ldr s0, [pc, #0]; b +8 (后续 patch_ldr_u32 修正偏移)
        self.emit_insn(&Insn::raw(0xbd000000));
        self.emit_insn(&Insn::raw(0x14000002));
    }

    /// 校验 LDR literal 偏移是否在 ±1MiB 范围内 (imm19 × 4)
    pub(crate) fn ldr_literal_offset_in_range(diff: i64) -> bool {
        (-(1 << 20)..(1 << 20)).contains(&diff)
    }

    pub(crate) fn patch_ldr_u32(&mut self, at: u64, addr: u64) {
        let pc = at + 8; // ldr literal PC-relative 从指令后 8 字节算起 (跳过 branch)
        let diff = addr as i64 - pc as i64;
        debug_assert!(
            Self::ldr_literal_offset_in_range(diff) && diff % 4 == 0,
            "LDR literal offset out of range or unaligned: diff={diff}"
        );
        let off = ((diff / 4) as u32) & 0x7ffff;
        let insn = 0xbd000000u32 | off;
        let off_usize = at as usize;
        if off_usize + 4 <= self.code.len() {
            self.code[off_usize..off_usize + 4].copy_from_slice(&insn.to_le_bytes());
        }
    }

    pub(crate) fn patch_adr(&mut self, at: u64, rd: u8, target: u64) {
        // 入队侧 (ops.rs::load_global_addr_w0) 统一通过 emit_adr_large 预留 8 字节:
        //   [at+0]  ADRP 占位 (0x90000000 | rd)
        //   [at+4]  ADD  rd, rd, #0 占位 (0x91000000 | rd<<5 | rd)
        // drain 时按 diff 是否越 ±1MiB 分两种填法。
        let pc = at;
        let diff = target as i64 - pc as i64;
        let off = at as usize;

        if !Self::adr_offset_in_range(diff) {
            // 越界: ADRP 页基址 + ADD 页内偏移。
            let page_off = (target as i64 & !0xfff) - (pc as i64 & !0xfff);
            let lo_off = (target as i64 & 0xfff) as u32;
            let immlo = ((page_off >> 12) & 0x3) as u32;
            let immhi = ((page_off >> 14) & 0x7ffff) as u32;
            // adrp rd, #page_off
            let insn_adrp = 0x9000_0000u32 | (immlo << 29) | (immhi << 5) | (rd as u32 & 0x1f);
            // add rd, rd, #lo_off  (sf=1 64-bit, op=ADD imm, shift=0, Rn=rd, Rd=rd)
            let insn_add =
                0x9100_0000u32 | (lo_off << 10) | ((rd as u32 & 0x1f) << 5) | (rd as u32 & 0x1f);
            if off + 8 <= self.code.len() {
                self.code[off..off + 4].copy_from_slice(&insn_adrp.to_le_bytes());
                self.code[off + 4..off + 8].copy_from_slice(&insn_add.to_le_bytes());
            }
            return;
        }
        // 未越界: 直接 ADR, 同时把 at+4 的 add 占位清成 NOP (避免多一条死指令)。
        let immlo = (diff & 0x3) as u32;
        let immhi = ((diff >> 2) & 0x7ffff) as u32;
        let insn = 0x1000_0000u32 | (immlo << 29) | (immhi << 5) | (rd as u32 & 0x1f);
        if off + 8 <= self.code.len() {
            self.code[off..off + 4].copy_from_slice(&insn.to_le_bytes());
            // NOP: 0xd503201f
            self.code[off + 4..off + 8].copy_from_slice(&0xd503_201f_u32.to_le_bytes());
        }
    }

    /// 发射 ADR + ADD 双指令序列 (用于超出 ±1MiB 的全范围地址)
    /// 调用方需预留 8 字节空间
    pub(crate) fn emit_adr_large(&mut self, rd: u8, target: u64) {
        // 先用 adrp 占位, 后续 patch
        let insn_adrp = 0x9000_0000u32 | (rd as u32);
        self.emit_insn(&Insn::raw(insn_adrp));
        // add rd, rd, #0 占位
        let insn_add = 0x9100_0000u32 | (rd as u32) << 5 | (rd as u32);
        self.emit_insn(&Insn::raw(insn_add));
        let pos = self.pos() - 8;
        self.pending_adr_exec.push((pos, rd, target));
    }

    // ===== 谓词/浮点比较 =====
}
