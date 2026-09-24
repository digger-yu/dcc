// RISC-V 64 backend - mechanically split from the monolithic
// implementation. Pure move: semantics preserved; only visibility and
// import paths were adjusted for the modular layout.
use crate::codegen::riscv64::codegen::Riscv64Codegen;

impl Riscv64Codegen {
    /// CSRRW rd, csr, rs1: 原子读写 CSR
    pub(crate) fn emit_csrrw(&mut self, rd: u8, csr: u16, rs1: u8) {
        let insn = (csr as u32) << 20 | (rs1 as u32) << 15 | 0b001 << 12 | (rd as u32) << 7 | 0x73;
        self.emit(&insn.to_le_bytes());
    }

    /// CSRRS rd, csr, rs1: 原子读并置位 CSR
    pub(crate) fn emit_csrrs(&mut self, rd: u8, csr: u16, rs1: u8) {
        let insn = (csr as u32) << 20 | (rs1 as u32) << 15 | 0b010 << 12 | (rd as u32) << 7 | 0x73;
        self.emit(&insn.to_le_bytes());
    }

    /// CSRRC rd, csr, rs1: 原子读并清位 CSR
    pub(crate) fn emit_csrrc(&mut self, rd: u8, csr: u16, rs1: u8) {
        let insn = (csr as u32) << 20 | (rs1 as u32) << 15 | 0b011 << 12 | (rd as u32) << 7 | 0x73;
        self.emit(&insn.to_le_bytes());
    }

    /// CSRRWI rd, csr, uimm: 立即数原子读写 CSR
    pub(crate) fn emit_csrrwi(&mut self, rd: u8, csr: u16, uimm: u8) {
        let insn = (csr as u32) << 20
            | ((uimm as u32) & 0x1f) << 15
            | 0b101 << 12
            | (rd as u32) << 7
            | 0x73;
        self.emit(&insn.to_le_bytes());
    }

    /// CSRRSI rd, csr, uimm: 立即数原子读并置位 CSR
    pub(crate) fn emit_csrrsi(&mut self, rd: u8, csr: u16, uimm: u8) {
        let insn = (csr as u32) << 20
            | ((uimm as u32) & 0x1f) << 15
            | 0b110 << 12
            | (rd as u32) << 7
            | 0x73;
        self.emit(&insn.to_le_bytes());
    }

    /// CSRRCI rd, csr, uimm: 立即数原子读并清位 CSR
    pub(crate) fn emit_csrrci(&mut self, rd: u8, csr: u16, uimm: u8) {
        let insn = (csr as u32) << 20
            | ((uimm as u32) & 0x1f) << 15
            | 0b111 << 12
            | (rd as u32) << 7
            | 0x73;
        self.emit(&insn.to_le_bytes());
    }

    /// MRET: 机器模式返回
    pub(crate) fn emit_mret(&mut self) {
        // 0x30200073: funct7=0011000, rs2=00010, rs1=00000, funct3=000, rd=00000
        let insn = 0x30200073u32;
        self.emit(&insn.to_le_bytes());
    }

    /// SRET: 监管模式返回
    pub(crate) fn emit_sret(&mut self) {
        let insn = 0x10200073u32;
        self.emit(&insn.to_le_bytes());
    }

    /// WFI: 等待中断
    pub(crate) fn emit_wfi(&mut self) {
        let insn = 0x10500073u32;
        self.emit(&insn.to_le_bytes());
    }

    /// SFENCE.VMA rs1, rs2: 虚拟内存屏障
    pub(crate) fn emit_sfence_vma(&mut self, rs1: u8, rs2: u8) {
        let insn = ((rs2 as u32) << 20 | (rs1 as u32) << 15) | 0x73;
        self.emit(&insn.to_le_bytes());
    }

    /// FENCE: 内存屏障 (全序)
    pub(crate) fn emit_fence(&mut self) {
        // FENCE pred=RW, succ=RW, fm=0
        let insn = 0x0ff0000fu32;
        self.emit(&insn.to_le_bytes());
    }

    /// FENCE.I: 指令获取屏障
    pub(crate) fn emit_fence_i(&mut self) {
        let insn = 0x0000100fu32;
        self.emit(&insn.to_le_bytes());
    }

    // ===== M8 新增: sret 辅助 =====

    /// memcpy_sp_to_a0: 从栈偏移 tmp 拷贝 size 字节到 [a0] 指向的内存
    pub(crate) fn memcpy_sp_to_a0(&mut self, tmp_off: i32, size: usize) {
        // a0 已指向目标 (caller 传入的 sret slot)
        // t0 = sp + tmp_off (源地址)
        self.mov_a_reg_sp_off(5, tmp_off); // t0 = &tmp
        let mut copied = 0usize;
        while copied + 8 <= size {
            // ld t1, 0(t0); sd t1, 0(a0)
            self.emit_ld(6, 5, copied as i32); // t1 = [t0+off]
            self.emit_sd(6, 10, copied as i32); // [a0+off] = t1
            copied += 8;
        }
        while copied + 4 <= size {
            self.emit_lw(6, 5, copied as i32);
            self.emit_sw(6, 10, copied as i32);
            copied += 4;
        }
        while copied < size {
            self.emit_lbu(6, 5, copied as i32);
            self.emit_sb(6, 10, copied as i32);
            copied += 1;
        }
    }

    /// LD rd, offset(rs1): 64-bit 整数加载
    pub(crate) fn emit_ld(&mut self, rd: u8, rs1: u8, off: i32) {
        // 压缩指令自动选择: C.LD (RV64)
        if (8..=15).contains(&rd)
            && (8..=15).contains(&rs1)
            && (0..=248).contains(&off)
            && off % 8 == 0
        {
            self.emit_c_ld(rd - 8, rs1 - 8, off as u8);
            return;
        }
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b011 << 12 | (rd as u32) << 7 | 0x03;
        self.emit(&insn.to_le_bytes());
    }

    /// -ftrivial-auto-var-init=zero: 零初始化栈帧
    /// 使用 sd zero, offset(sp) 每次清零 8 字节 (frame 已 16 字节对齐)
    pub(crate) fn emit_zero_init_frame(&mut self, frame_size: usize) {
        if frame_size == 0 {
            return;
        }
        // 从帧底向帧顶写入, offset 相对于 sp
        let words = frame_size / 8;
        for i in 0..words {
            let off = (i as i32) * 8 - (frame_size as i32);
            self.emit_sd(0, 2, off); // sd zero, off(sp)
        }
        // 处理剩余 4 字节
        let rem = frame_size % 8;
        if rem >= 4 {
            let off = (words * 8) as i32 - (frame_size as i32) + (rem - 4) as i32;
            self.emit_sw(0, 2, off); // sw zero, off(sp)
        }
    }

    /// SD rs2, offset(rs1): 64-bit 整数存储
    pub(crate) fn emit_sd(&mut self, rs2: u8, rs1: u8, off: i32) {
        // 压缩指令自动选择: C.SD (RV64)
        if (8..=15).contains(&rs2)
            && (8..=15).contains(&rs1)
            && (0..=248).contains(&off)
            && off % 8 == 0
        {
            self.emit_c_sd(rs2 - 8, rs1 - 8, off as u8);
            return;
        }
        let imm = off as u32;
        let insn = ((imm >> 5) & 0x7f) << 25
            | (rs2 as u32) << 20
            | (rs1 as u32) << 15
            | 0b011 << 12
            | (imm & 0x1f) << 7
            | 0x23;
        self.emit(&insn.to_le_bytes());
    }

    /// LBU rd, offset(rs1): 无符号字节加载
    pub(crate) fn emit_lbu(&mut self, rd: u8, rs1: u8, off: i32) {
        let imm = off as u32 & 0xfff;
        let insn = imm << 20 | (rs1 as u32) << 15 | 0b100 << 12 | (rd as u32) << 7 | 0x03;
        self.emit(&insn.to_le_bytes());
    }

    /// SB rs2, offset(rs1): 字节存储
    pub(crate) fn emit_sb(&mut self, rs2: u8, rs1: u8, off: i32) {
        let imm = off as u32;
        let insn = (((imm >> 5) & 0x7f) << 25 | (rs2 as u32) << 20 | (rs1 as u32) << 15)
            | (imm & 0x1f) << 7
            | 0x23;
        self.emit(&insn.to_le_bytes());
    }
}
