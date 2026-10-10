// src/codegen/x86_64/insn.rs
//
// x86_64 后端 - 指令记录模型 (统一发射入口).
//
// 第一性原理: x86_64 指令**变长** (1~15 字节, 含 REX / ModRM / SIB / disp / imm / 前缀),
// 不能像定长 ISA (aarch64/mips/arm/riscv32) 那样用 `u32` 字面值装. 因此本模型用
// `Raw` 承接迁移期所有 `self.emit(&[..])` 调用点; P9 步骤 1 起, 常用族 (AluRm /
// MovRm / UnaryRm / Shift / Lea / Call / Jmp / Jcc / Ret / Stack / SimdArith /
// SimdMov / X87) 逐步抽为类型化构造子, 其 encode/render 真源集中在 `opcode.rs`.
//
// 迁移纪律 (G11): 纯移动, 构造子编码 == 现有字节. 不变更任何位型.

use crate::codegen::shared::asm_operand::{InsnRec, Operand};
use crate::codegen::x86_64::opcode::X86_64Opcode;

#[derive(Clone, Debug)]
pub struct Insn {
    pub op: X86_64Opcode,
    pub ops: [Operand; 4],
    /// 兼容字段: 迁移期内 Raw 变体的字节序.
    /// 类型化变体的字节由 op.encode() 现场算, 不存这里.
    pub bytes: Vec<u8>,
    /// P9 方案 C: 发射点携带的 -S GAS 渲染文本.
    ///
    /// 真源语义: 一旦某条指令在 emit_insn 时带了 gas_text, -S 渲染器就**直接**
    /// 拿这一行输出 (不含前导 tab / 换行), 不再跑 disasm 反推. 没有这个字段,
    /// 渲染器必须退回字节反推. 这就是最终删除 disasm.rs 的前置条件.
    ///
    /// 注意: 这个字段**只影响 -S 文本**, 不参与 .text 段字节生成. 所以 G10 快照
    /// (校验 .text sha256) 对它零敏感.
    pub gas_text: Option<String>,
}

impl Insn {
    pub fn new(op: X86_64Opcode, ops: &[Operand]) -> Self {
        let mut arr = [Operand::None, Operand::None, Operand::None, Operand::None];
        let n = ops.len().min(4);
        arr[..n].clone_from_slice(&ops[..n]);
        Insn {
            op,
            ops: arr,
            bytes: Vec::new(),
            gas_text: None,
        }
    }

    /// 直接用字节切片构造 (迁移期兜底, 逐步替换为类型化构造子).
    /// x86_64 变长, 因此直接保留字节切片, 不做 u32/u16 拼装.
    ///
    /// P9 方案 C: 这个名字是历史遗留别名 (raw_bytes), 现存 4000+ 处调用点.
    /// 新代码**不应**用这个构造子; 用 `raw_with_gas(bytes, "gas text")` 直接携带
    /// 发射点已知的 GAS 文本, 或用类型化构造子 (`Insn::new(X86_64Opcode::...)`).
    ///
    /// 注: P9 收敛完成后仓库里已经没有 `Insn::raw_bytes(..)` 字面调用点.
    /// 保留这个名字仅为兼容可能出现的外部 / 测试直接引用.
    #[doc(hidden)]
    pub fn raw_bytes(b: &[u8]) -> Self {
        Insn::raw(b)
    }

    /// P9 方案 C 收敛后使用的主构造子 (替代 raw_bytes). 语义与 raw_bytes 一致,
    /// 但名字反映「这是字节切片兜底路径」而非「这是标准入口」. 现存调用点逐步迁移,
    /// 完成后 raw_bytes 这个名字将从仓库中消除.
    pub fn raw(b: &[u8]) -> Self {
        Insn::new(
            X86_64Opcode::Raw {
                bytes: b.to_vec(),
                mnemonic: None,
            },
            &[],
        )
    }

    pub fn nop() -> Self {
        Insn::new(
            X86_64Opcode::Raw {
                bytes: vec![0x90],
                mnemonic: Some("nop".into()),
            },
            &[],
        )
    }

    /// P9 步骤 5: 内联汇编发射完一段字节后, 用 (bytes, mnemonic) 就地包装成 Raw。
    /// 与 raw_bytes 的差别只是 mnemonic 字段填 Some, 供 render() 出 GAS 文本。
    /// 不改变任何发射字节 —— 只影响 -S 文本渲染 (步骤 6 起读 asm_insn_log)。
    pub fn raw_with_mnemonic(bytes: &[u8], mnemonic: &str) -> Self {
        let ops = [Operand::None, Operand::None, Operand::None, Operand::None];
        Insn {
            op: X86_64Opcode::Raw {
                bytes: bytes.to_vec(),
                mnemonic: Some(mnemonic.to_string()),
            },
            ops,
            bytes: Vec::new(),
            gas_text: Some(mnemonic.to_string()),
        }
    }

    /// P9 方案 C: 发射点已经知道这条指令最终在 -S 里要渲染成哪一行 GAS 文本,
    /// 直接携带. 渲染器不再需要反推字节.
    pub fn raw_with_gas(bytes: &[u8], gas_line: &str) -> Self {
        let ops = [Operand::None, Operand::None, Operand::None, Operand::None];
        Insn {
            op: X86_64Opcode::Raw {
                bytes: bytes.to_vec(),
                mnemonic: None,
            },
            ops,
            bytes: Vec::new(),
            gas_text: Some(gas_line.to_string()),
        }
    }

    pub fn encode_bytes(&self) -> Option<Vec<u8>> {
        // 类型化变体: 让 opcode.rs 现场算字节. 尚未实现的变体返回 None →
        // 发射方必须切回 raw_bytes 兜底; 当前没有任何调用点依赖 None 分支.
        self.op.encode()
    }

    pub fn to_rec(&self, pc: u64) -> InsnRec {
        // 迁移期: opcode_id 统一记 0 (架构中性 InsnRec 只在跨后端通道用,
        // 生产路径尚未切到). 步骤 6 完成后再按变体映射到 u16 空间.
        let mut rec = InsnRec::new(0, &self.ops[..]);
        rec.pc = pc;
        rec.bytes_len = self.bytes.len() as u8;
        rec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_bytes_passthrough() {
        // x86_64 本机小端, 字节原样落盘
        assert_eq!(
            Insn::raw(&[0x48, 0x8B, 0x05, 0x34, 0x12, 0x00, 0x00]).encode_bytes(),
            Some(vec![0x48, 0x8B, 0x05, 0x34, 0x12, 0x00, 0x00]),
        );
    }

    #[test]
    fn nop_encode() {
        assert_eq!(Insn::nop().encode_bytes(), Some(vec![0x90]));
    }

    #[test]
    fn raw_bytes_empty() {
        assert_eq!(Insn::raw(&[]).encode_bytes(), Some(Vec::new()));
    }
}
