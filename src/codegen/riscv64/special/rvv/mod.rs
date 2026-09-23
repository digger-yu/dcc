// src/codegen/riscv64/special/rvv/mod.rs (Placeholder)
// RISC-V Vector Extension Support

#[derive(Clone, Copy, Debug)]
pub struct RvvConfig {
    pub vlen: u16,      // 向量寄存器位宽 (128/256/512/1024)
    pub elen: u16,      // 最大元素位宽
    pub vtype_val: u32, // vtype 编码值
}

impl Default for RvvConfig {
    fn default() -> Self {
        Self {
            vlen: 128,
            elen: 64,
            vtype_val: 0,
        }
    }
}
