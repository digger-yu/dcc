// src/ssa/dump.rs - SSA IR 的读数通道
//
// 放什么: `dump_function` —— 把 SSA `Function` 的 BB / phi / inst / term 汇成一个
//   字符串, 供三条诊断通道共用: `DCC_SSA_IR_DUMP`(opt.rs)、`DCC_DUMP_CPP_AST`(cpp/native.rs)、
//   `DCC_PIPE_DUMP`(ssa/passes.rs)。它们只读不写, 不进产物字节。
// 不放什么: IR 数据结构 (在 `ir.rs`)、任何 pass 的改写与重建逻辑。
// 诊断文案在此唯一: 搬迁与后续改动都不得修改这里的打印格式与措辞。

use crate::ssa::ir::Function;

pub fn dump_function(func: &Function) -> String {
    let mut s = String::new();
    s.push_str(&format!("fn {}: {{\n", func.name));
    for b in &func.block_order {
        if let Some(bb) = func.blocks.get(b) {
            s.push_str(&format!("  {}: preds={:?} succs={:?}\n", bb.name, bb.preds, bb.succs));
            for phi in &bb.phis {
                s.push_str(&format!("    phi {} <- {:?}\n", phi.dest.name(), phi.incoming));
            }
            for inst in &bb.insts {
                let dst = inst
                    .dest
                    .as_ref()
                    .map(|d| d.name())
                    .unwrap_or_else(|| "_".to_string());
                s.push_str(&format!("    {} = {:?}\n", dst, inst.kind));
            }
            s.push_str(&format!("    term: {:?}\n", bb.term));
        }
    }
    s.push_str("}\n");
    s
}
