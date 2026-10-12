// src/codegen/shared/asm_operand.rs
//
// 架构中性的指令记录模型 (InsnRec / Operand)。
//
// 第一性原理: 所有后端的发射最终都应走"先构造一份指令记录, 再由共享引擎
// 装填字节 / 渲染 GAS 文本 / 解析用户 inline asm"这一条单一通路。本模块
// 只放类型本身与最小往返辅助; 具体 ISA 编码表与 dialect 由各后端提供。
//
// 历史缺陷: x86_64/asm/operand.rs 定义了一份 `AsmOperand`, 四个交叉后端
// 又各自在 mini_asm 里按字符串现场解析。现在统一为一份 `Operand`,
// 寄存器编号以 ISA-中性的 `u8` 表示, 宽度以 `Width` 枚举区分。

/// 操作数宽度 (由 dialect 决定寄存器名后缀)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    /// 8 bit
    B8,
    /// 16 bit
    B16,
    /// 32 bit
    B32,
    /// 64 bit
    B64,
    /// 128 bit (vector / Neon)
    B128,
    /// 256 bit (AVX)
    B256,
    /// 512 bit (AVX-512)
    B512,
    /// 宽度由 ISA / 上下文决定 (用于不区分宽度的指令, 如 aarch64 `ret`)
    Bare,
}

impl Width {
    pub fn bits(self) -> u32 {
        match self {
            Width::B8 => 8,
            Width::B16 => 16,
            Width::B32 => 32,
            Width::B64 => 64,
            Width::B128 => 128,
            Width::B256 => 256,
            Width::B512 => 512,
            Width::Bare => 0,
        }
    }
}

/// 内存寻址的向量排列 (aarch64 Neon / SVE)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrangement {
    B8,
    B16,
    B32,
    B64,
    B128,
    Stride,
    Multiple,
}

/// 指令操作数 (架构中性)。
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Operand {
    /// 无操作数占位。
    #[default]
    None,
    /// 寄存器: (reg_idx, width)。reg_idx 语义由 ISA 定义 (aarch64 0..31,
    /// riscv 0..31, arm 0..15, mips 0..31, x86 0..15)。
    Reg { idx: u8, width: Width },
    /// 立即数: (值, 位宽, 是否有符号)。
    Imm { value: i64, bits: u8, signed: bool },
    /// 内存操作数: (base, index, scale, disp, writeback)。
    /// 所有字段可缺省 (None = 不使用)。
    Mem {
        base: Option<u8>,
        index: Option<u8>,
        scale: u8,
        disp: i64,
        writeback: bool,
    },
    /// 外部符号引用: (符号名, addend)。用于 ADRP/ADR/GOT/PC-rel 等。
    Sym { name: String, addend: i64 },
    /// 分支/跳转目标: (标签名, 是否 PC 相对)。
    Addr { label: String, pc_relative: bool },
    /// 向量寄存器: (reg_idx, 排列)。
    Vector { idx: u8, arr: Arrangement },
    /// 原始字节垫 (对齐/padding/directive 字节流)。
    Pad { bytes: Vec<u8> },
    /// 字面量池条目: (原始值字节, 对齐)。
    PoolData { bytes: Vec<u8>, alignment: u32 },
}

impl Operand {
    /// 寄存器简写 (默认 64-bit, 用于 aarch64 xN / x86 r64)。
    pub fn reg64(idx: u8) -> Self {
        Operand::Reg {
            idx,
            width: Width::B64,
        }
    }
    /// 32-bit 寄存器 (wN / eN / r32)。
    pub fn reg32(idx: u8) -> Self {
        Operand::Reg {
            idx,
            width: Width::B32,
        }
    }
    /// 立即数简写 (无符号)。
    pub fn uimm(value: i64, bits: u8) -> Self {
        Operand::Imm {
            value,
            bits,
            signed: false,
        }
    }
    /// 立即数简写 (有符号)。
    pub fn simm(value: i64, bits: u8) -> Self {
        Operand::Imm {
            value,
            bits,
            signed: true,
        }
    }
}

/// 一份指令记录: 发射时交给共享引擎, 编译完成后保留以用于 `-S` 渲染。
///
/// `opcode` 是 ISA 专属的枚举标签 (由 build.rs 从 TOML 生成, 各后端不同);
/// 这里用 `u16` 存储以保持架构中性 (后端把它映射到自己的枚举)。
#[derive(Clone, Debug, Default)]
pub struct InsnRec {
    /// ISA 专属 opcode 枚举值 (各后端定义自己的枚举, 存为 u16 以跨模块传输)。
    pub opcode: u16,
    /// 操作数列表 (最多 4 个, 与 GAS operand arity 兼容)。
    pub ops: [Operand; 4],
    /// 有效操作数个数。
    pub nops: u8,
    /// 指令在 .text 内的虚拟地址 (由 LayoutMap 回填)。
    pub pc: u64,
    /// 装填后实际长度 (变长 ISA 如 riscv c.* 可能是 2 或 4)。
    pub bytes_len: u8,
}

impl InsnRec {
    pub fn new(opcode: u16, ops: &[Operand]) -> Self {
        let mut arr = [Operand::None, Operand::None, Operand::None, Operand::None];
        let n = ops.len().min(4);
        arr[..n].clone_from_slice(&ops[..n]);
        InsnRec {
            opcode,
            ops: arr,
            nops: n as u8,
            pc: 0,
            bytes_len: 0,
        }
    }
    /// 取第 i 个操作数 (越界返回 None)。
    pub fn op(&self, i: usize) -> &Operand {
        if i < self.nops as usize {
            &self.ops[i]
        } else {
            &Operand::None
        }
    }
}

/// 布局真源: 编译完成后由共享引擎回填。所有 label/func_size 查询都应走它,
/// 不再直接读后端的 `labels` HashMap。
#[derive(Clone, Debug, Default)]
pub struct LayoutMap {
    /// 符号/标签名 -> 虚拟地址。
    pub addresses: std::collections::HashMap<String, u64>,
    /// 函数名 -> 大小 (字节)。
    pub func_sizes: std::collections::HashMap<String, u64>,
    /// 当前 .text 末尾位置。
    pub text_end: u64,
}

impl LayoutMap {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get(&self, name: &str) -> Option<u64> {
        self.addresses.get(name).copied()
    }
    pub fn func_size(&self, name: &str) -> Option<u64> {
        self.func_sizes.get(name).copied()
    }
    pub fn set(&mut self, name: impl Into<String>, addr: u64) {
        self.addresses.insert(name.into(), addr);
    }
    /// 从后端的 `labels` + `func_sizes` 两份 HashMap 一次性快照出只读 `LayoutMap`。
    ///
    /// 迁移期内: 后端内部状态仍是 `HashMap` 直写 (写入者继续用), 但任何
    /// "想读标签/函数尺寸" 的消费点都应改为吃 `&LayoutMap` 只读快照,
    /// 不再直接读 `pub labels: HashMap` 字段。
    pub fn snapshot(
        labels: &std::collections::HashMap<String, u64>,
        func_sizes: &std::collections::HashMap<String, u64>,
        text_end: u64,
    ) -> Self {
        LayoutMap {
            addresses: labels.clone(),
            func_sizes: func_sizes.clone(),
            text_end,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insn_rec_ctors() {
        let r = InsnRec::new(42, &[Operand::reg64(0), Operand::reg32(1), Operand::uimm(4, 12)]);
        assert_eq!(r.nops, 3);
        assert_eq!(
            r.op(0),
            &Operand::Reg {
                idx: 0,
                width: Width::B64
            }
        );
        assert_eq!(r.op(3), &Operand::None);
    }

    #[test]
    fn layout_map_roundtrip() {
        let mut lm = LayoutMap::new();
        lm.set("foo", 0x400100);
        lm.func_sizes.insert("foo".to_string(), 16);
        assert_eq!(lm.get("foo"), Some(0x400100));
        assert_eq!(lm.func_size("foo"), Some(16));
    }
}
