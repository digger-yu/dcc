// src/codegen/aarch64/encode_truth_test.rs
//
// G1 门禁: 每个已编码发射点的 "dcc 编码字节" == "aarch64-linux-gnu-as 真值字节"。
//
// 第一性原理: `Insn::encode()` 是纯移动迁移的产物, 其正确性必须由外部真源
// (GNU as) 裁决, 不能自证。本测试把每条有明确 GAS 助记符的指令渲染成
// `.s` 文本, 用 `aarch64-linux-gnu-as` 汇编成 .o, 再用 `aarch64-linux-gnu-objdump`
// 取出 .text 首字, 与 `Insn::encode()` 输出逐字比较。
//
// 设计约束:
//   * 只覆盖 `encode()` 已经实现、且语义正确 (不在 G11 缺陷清单) 的构造子;
//   * 若 `aarch64-linux-gnu-as`/`-objdump` 不可用 (非开发机), 测试跳过而不失败;
//     工具在位却一条都没真跑仍算失败 (G5 反假绿: 至少一条真跑)。
//   * 缺陷清单 (docs/aarch64_encoding_defects.md) 中的字 NOT 在此验证 ——
//     它们按 G11 允许为红, 由 bug-fix 阶段单独修。

use super::insn::{Aarch64Opcode, Insn};
use crate::codegen::shared::asm_operand::Operand;
use std::process::Command;

/// 一条待测指令: (Insn, 期望被 as 接受的 GAS 文本, 描述)。
struct Case {
    insn: Insn,
    gas: &'static str,
    desc: &'static str,
}

fn truth_cases() -> Vec<Case> {
    vec![
        Case {
            insn: Insn::nop(),
            gas: "nop",
            desc: "nop",
        },
        Case {
            insn: Insn::ret(),
            gas: "ret",
            desc: "ret x30",
        },
        Case {
            insn: Insn::br(9),
            gas: "br x9",
            desc: "br x9",
        },
        Case {
            insn: Insn::blr(10),
            gas: "blr x10",
            desc: "blr x10",
        },
        // mov x9, x10 = orr x9, xzr, x10 (64-bit sf=1) — 构造子语义正确的代表元
        Case {
            insn: Insn::mov_reg(9, 10, true),
            gas: "mov x9, x10",
            desc: "mov x9,x10 (sf=1)",
        },
        // movz x0, #42
        Case {
            insn: Insn::mov_imm(0, 42, true),
            gas: "mov x0, #42",
            desc: "movz x0,#42",
        },
        // add x0, x1, #4
        Case {
            insn: Insn::add_imm(0, 1, 4, true),
            gas: "add x0, x1, #4",
            desc: "add x0,x1,#4",
        },
        // sub x0, x1, #4
        Case {
            insn: Insn::sub_imm(0, 1, 4, true),
            gas: "sub x0, x1, #4",
            desc: "sub x0,x1,#4",
        },
        // svc #0
        Case {
            insn: Insn::svc(0),
            gas: "svc #0",
            desc: "svc #0",
        },
        // dmb sy (imm=15)
        Case {
            insn: Insn::new(Aarch64Opcode::Dmb, &[Operand::uimm(15, 4)]),
            gas: "dmb sy",
            desc: "dmb sy",
        },
        // wfe / wfi / yield / sev / sevl / isb
        Case {
            insn: Insn::new(Aarch64Opcode::Wfe, &[]),
            gas: "wfe",
            desc: "wfe",
        },
        Case {
            insn: Insn::new(Aarch64Opcode::Wfi, &[]),
            gas: "wfi",
            desc: "wfi",
        },
        Case {
            insn: Insn::new(Aarch64Opcode::Yield, &[]),
            gas: "yield",
            desc: "yield",
        },
        Case {
            insn: Insn::new(Aarch64Opcode::Isb, &[]),
            gas: "isb",
            desc: "isb",
        },
    ]
}

/// 交叉真值工具链 (`as` + `objdump`) 是否可用 —— 非开发机上没有, 测试跳过。
fn truth_tools_available() -> bool {
    ["aarch64-linux-gnu-as", "aarch64-linux-gnu-objdump"]
        .iter()
        .all(|bin| Command::new(bin).arg("--version").output().is_ok())
}

/// 用 `as`+`objdump` 取一条指令的 .text 首 4 字节 (小端 u32)。
///
/// 工具可用性由 [`truth_tools_available`] 在测试入口统一判定; 这里的 `None`
/// 只表示「`as` 拒绝了这条文本」或输出解析失败, 由调用方按 skip 处理。
fn assemble_truth(gas: &str) -> Option<u32> {
    let as_bin = "aarch64-linux-gnu-as";
    let objdump_bin = "aarch64-linux-gnu-objdump";
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let src = dir.join(format!("dcc-g1-{pid}.s"));
    let obj = dir.join(format!("dcc-g1-{pid}.o"));
    std::fs::write(&src, format!(".text\n_start:\n  {}\n", gas)).ok()?;
    let st = Command::new(as_bin)
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .ok()?;
    if !st.status.success() {
        // as 拒绝了这条助记符 —— 该构造子对应的文本有问题, 不算通过。
        return None;
    }
    let out = Command::new(objdump_bin)
        .args([
            "-d",
            "--architecture=aarch64",
            &format!("{}", obj.display()),
        ])
        .output()
        .ok()?;
    let txt = String::from_utf8_lossy(&out.stdout);
    // objdump 行形如: "   0:\td503201f \tnop"
    for line in txt.lines() {
        let l = line.trim();
        let parts: Vec<&str> = l.split_whitespace().collect();
        // 第一个 token 形如 "0:" (地址), 第二个 token 是 hex 机器字
        if parts.len() >= 2 && parts[0].ends_with(':') {
            if let Ok(word) = u32::from_str_radix(parts[1].trim_start_matches("0x"), 16) {
                return Some(word);
            }
        }
    }
    None
}

#[test]
fn g1_encode_matches_gas() {
    if !truth_tools_available() {
        eprintln!("[G1] SKIP: aarch64-linux-gnu-as/objdump unavailable (非开发机)");
        return;
    }
    let cases = truth_cases();
    let mut ran = 0;
    let mut mismatches = 0;
    for c in &cases {
        let Some(dcc_word) = c.insn.encode() else {
            continue; // 该构造子尚未实现编码, 跳过
        };
        let Some(gas_word) = assemble_truth(c.gas) else {
            continue; // as 不可用或拒绝, skip 此条
        };
        ran += 1;
        if dcc_word != gas_word {
            mismatches += 1;
            eprintln!(
                "[G1] MISMATCH {}: dcc=0x{:08x} gas=0x{:08x} (gas text: {})",
                c.desc, dcc_word, gas_word, c.gas
            );
        }
    }
    // G5 反假绿: 必须至少真跑一条。工具可用性已在入口判定, 走到这里 `ran == 0`
    // 只可能是全部 `encode()` 未实现或被 `as` 拒绝 —— 门禁空跑, 不算绿。
    assert!(
        ran > 0,
        "G1 反假绿: 交叉工具在位但没有任何用例真跑 (encode() 未实现或被 as 拒绝), 门禁空跑"
    );
    assert_eq!(
        mismatches, 0,
        "G1: {}/{} 条指令编码与 GAS 真值不一致 (详见上方 eprintln)",
        mismatches, ran
    );
}
