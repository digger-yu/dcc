// src/codegen/x86_64/instructions.rs
// Low-level x86_64 machine code emission methods for Codegen
// Core instruction encoding: mov, arithmetic, logic, stack, jumps, labels

use crate::ast::{AddrSpace, Expr, Type};
use crate::codegen::x86_64::codegen::JumpKind;
use crate::codegen::x86_64::codegen::PendingJump;
use crate::codegen::x86_64::Codegen;
use crate::codegen::shared::asm_operand::Width;
use crate::codegen::x86_64::insn::Insn;
use crate::codegen::x86_64::opcode::{
    AluOp, MemOperand, Operand as OpcodeOperand, X86_64Opcode,
};
use crate::codegen::x86_64::emit::gas::{
    gp as gas_gp, mem_disp as gas_mem_disp, dec_imm as gas_dec_imm,
    REG64 as GAS_REG64, REG32 as GAS_REG32, REG16 as GAS_REG16, REG8 as GAS_REG8,
    XMM as GAS_XMM, modrm_disp_bytes as gas_modrm_disp_bytes,
};
use crate::semantics::ObjectResolver;

/// 生成期新增家超出帧额度的诊断码（与 `report_internal_bug` 同一条上报通道）。
/// 它是后端不变量破坏，不是用户代码错误；详见 `check_frame_budget`。
const DIAG_FRAME_BUDGET: &str = "E1128";

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

    /// 指令字节的**唯一追写出口**。函数体内不得再出现 `self.code.push/extend`：
    /// 求值落点的持有记录（`eval_holding`）以「又发了一条指令」为失效点，
    /// 多一个出口就等于那条事实会静默变陈旧。判据：
    /// `.work_tmp/p12_base/check_eval_base_single_source.sh`。
    pub fn emit(&mut self, b: &[u8]) {
        self.code.extend_from_slice(b);
        self.eval_holding = None;
    }

    /// 指令记录统一发射入口 (x86_64 变长, 字节原样直写).
    #[track_caller]
    pub(crate) fn emit_insn(&mut self, insn: &crate::codegen::x86_64::insn::Insn) -> bool {
        // 临时统计: DCC_GAS_STATS=1 时按 caller 统计发射频率.
        if std::env::var_os("DCC_GAS_STATS").is_some() {
            let loc = std::panic::Location::caller();
            let key = format!("{}:{}", loc.file(), loc.line());
            self.emit_insn_stats.entry(key).and_modify(|c| *c += 1).or_insert(1);
            let has_gas = insn.gas_text.is_some();
            self.emit_insn_gas_hits.entry(has_gas).and_modify(|c| *c += 1).or_insert(1);
        }
        match insn.encode_bytes() {
            Some(bytes) => {
                // P9 方案 C: 在发射前记下 pc, 发射后把 (pc, insn) 追加进 insn_records.
                // 这张表是 -S 渲染器 (build_asm_output_v2) 的唯一真源, 不再走 disasm.
                // 不改变发射字节 —— 只是多写一份 (pc, insn) 到 Vec, 对 .text 段零影响.
                let pc = self.code.len() as u64;
                self.emit(&bytes);
                self.insn_records.push((pc, insn.clone()));
                true
            }
            None => false,
        }
    }

    /// 「刚求出来的值在哪个号里」的读接口（前置一·下）。`None` 就是「没有这个事实」，
    /// 而不是「猜一个」—— 调用方宁可自己再求值一次，也不得拿陈旧值往下算。
    pub fn eval_held(&self) -> Option<crate::location::RegId> {
        self.eval_holding
    }

    // ========================================================================
    // Label management
    // ========================================================================

    pub fn fresh_label(&mut self) -> String {
        let n = self.next_label;
        self.next_label += 1;
        format!(".L{}", n)
    }

    /// `.L` 内部标号命名空间的唯一谓词。
    ///
    /// 与 [`Self::fresh_label`] 同处 L1 发射层, 因为两者描述同一个事实:
    /// 哪些标号名是**后端自己造的**。GAS/ELF 约定 `.L` 开头符号不进 symtab,
    /// 所以链接期根本不可能解析它们: 一条指向 `.L*` 的跳转如果没进 `labels`
    /// 表, 只能是发射器自己发漏了, 绝不能像合法外部符号那样降级成重定位。
    /// 区分这两类的判断只允许存在这一处。
    pub fn is_internal_label(name: &str) -> bool {
        name.starts_with(".L")
    }

    pub fn label(&mut self, name: &str) {
        self.labels.insert(name.to_string(), self.pos());
    }

    /// 一个函数 (含其嵌套函数) 发射结束时的 pending 闭合性断言。
    ///
    /// `jmp_rel32` / `jcc_rel32` 写出的 `disp32` 占位是 `0,0,0,0` —— 不回填就是
    /// 「跳到下一条」(原地自环)。那会产出一个能编过、能链上、跑必挂的目标文件,
    /// 比编译失败恶劣得多。因此本函数自己造的内部标号必须在函数发射结束时全部
    /// 可解析; 只有合法外部符号 (跳 TU / 内联汇编里声明的全局标签) 才能留给重定位。
    pub(in crate::codegen::x86_64) fn check_pending_labels_closed(&mut self, func_name: &str) {
        // 先取不变异地拷出名单, 再调 `&mut self` 的上报通道。
        let unresolved: Vec<String> = {
            let labels = &self.labels;
            self.pending
                .iter()
                .filter(|p| Self::is_internal_label(&p.target) && !labels.contains_key(&p.target))
                .map(|p| p.target.clone())
                .collect()
        };
        if unresolved.is_empty() {
            return;
        }
        // 去重但保持发射顺序, 使诊断本身可复现。
        let mut seen = std::collections::HashSet::new();
        let uniq: Vec<&str> = unresolved
            .iter()
            .filter(|n| seen.insert((*n).clone()))
            .map(|n| n.as_str())
            .collect();
        self.report_internal_bug(
            "E1102",
            format!(
                "internal label(s) {} referenced by a jump in function '{}' are never defined; \
                 their rel32 displacement would stay 0 (jump-to-next-instruction)",
                uniq
                    .iter()
                    .map(|n| format!("'{}'", n))
                    .collect::<Vec<_>>()
                    .join(", "),
                func_name
            ),
            "a compiler-generated .L label must be emitted with `Codegen::label` before the \
             function finishes; do not downgrade it to an external relocation",
        );
    }

    // ========================================================================
    // Jump / branch instructions
    // ========================================================================

    pub fn jmp_rel32(&mut self, target: &str, kind: JumpKind) {
        let at = self.pos();
        let (bytes, mnem): (Vec<u8>, &str) = if kind == JumpKind::Jmp {
            (vec![0xE9, 0, 0, 0, 0], "jmp")
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
            // 与 asm/render_one.rs 内助记符表逐条对齐:
            //   0x84=je 0x85=jne 0x8C=jl 0x8E=jle 0x8F=jg 0x8D=jge
            //   0x82=jb 0x87=ja  0x89=jns 0x80=jo
            let mnem = match kind {
                JumpKind::Je => "je",
                JumpKind::Jne => "jne",
                JumpKind::Jl => "jl",
                JumpKind::Jle => "jle",
                JumpKind::Jg => "jg",
                JumpKind::Jge => "jge",
                JumpKind::Jb => "jb",
                JumpKind::Ja => "ja",
                JumpKind::Jns => "jns",
                JumpKind::Jo => "jo",
                _ => unreachable!(),
            };
            (vec![0x0F, cc, 0, 0, 0, 0], mnem)
        };
        let gas = format!("{mnem} {target}");
        self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
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
        let bytes = [rex, 0x8B, 0xC0 | ((dst & 7) << 3) | (src & 7)];
        // AT&T: `mov %src, %dst` (源在前, 目的在后).
        let gas = format!("mov {}, {}", GAS_REG64[(src & 15) as usize], GAS_REG64[(dst & 15) as usize]);
        self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
    }
    /// 寄存器 → 寄存器**按声明宽度截断**的搬运，用于写进某个 `register asm` 变量的家。
    /// 规范形态：低 `sz` 位是值，高位为 0（读出时再按符号性扩展，见
    /// `mov_reg_extend`）。为什么必须截断：绑定路径原来是裸 64 位 `mov`，于是
    /// `register unsigned int v asm("rbx"); v = 0x100000001UL;` 之后 `v` 读回
    /// 4294967297，而栈槽路径（32 位 store）与 gcc 都是 1 —— 同一个变量在两条通路
    /// 下给出两个值，就是「宽度口径有两套」的直接后果。
    pub fn mov_reg_truncate(&mut self, dst: u8, src: u8, sz: usize) {
        let modrm = 0xC0u8 | ((dst & 7) << 3) | (src & 7);
        match sz {
            1 | 2 => {
                // movzx dst, src8 / src16: 只取低位并清零高位。
                // 位序必须分开判：REX.R 看 ModRM.reg（这里是 dst），REX.B 看 r/m
                // （这里是 src）。而 spl/bpl/sil/dil 只要「有 REX 前缀」就合法，
                // **不能**为此去置 REX.B —— 那会把字段索引搬到 +8 的高 8 个寄存器上。
                // 实测踩过：置了 B 之后 `movzbl %sil,%ebx` 被编成 `movzbl %r14b,%ebx`。
                let need_rex = dst >= 8 || src >= 8 || (sz == 1 && src >= 4);
                let op = if sz == 1 { 0xB6 } else { 0xB7 };
                // P9 方案 C: 挂 gas_text. AT&T 格式: movzbl src8, %dst32 / movzwl src16, %dst32.
                let mnemonic = if sz == 1 { "movzbl" } else { "movzwl" };
                let src8_name = if sz == 1 {
                    GAS_REG8[(src & 15) as usize]
                } else {
                    GAS_REG16[(src & 15) as usize]
                };
                let gas = format!("{mnemonic} {src8_name}, {}", GAS_REG32[(dst & 15) as usize]);
                if need_rex {
                    let mut rex = 0x40u8;
                    if dst >= 8 {
                        rex |= 0x04;
                    }
                    if src >= 8 {
                        rex |= 0x01;
                    }
                    self.emit_insn(&Insn::raw_with_gas(&[rex, 0x0F, op, modrm], &gas));
                } else {
                    self.emit_insn(&Insn::raw_with_gas(&[0x0F, op, modrm], &gas));
                }
            },
            4 => {
                // mov dst32, src32: 32 位写自动清零高 32 位。dst == src 时也不能跳,
                // 那一次「自己搬自己」正是把高 32 位清掉的动作。
                let mut rex = 0x40u8;
                if dst >= 8 {
                    rex |= 0x04;
                }
                if src >= 8 {
                    rex |= 0x01;
                }
                // 方向必须是 8B (mov r, r/m: ModRM.reg 是目的, rm 是源) —— 与 `mov()`
                // 同一个形态, 只是不带 REX.W。89 /r 的方向正好相反 (reg 是源), 用它
                // 会把变量的旧值当输入搬进来 (实测读出的是栈/参数垃圾)。
                // AT&T: mov %src32, %dst32.
                let gas = format!(
                    "mov {}, {}",
                    GAS_REG32[(src & 15) as usize],
                    GAS_REG32[(dst & 15) as usize],
                );
                if rex == 0x40 {
                    self.emit_insn(&Insn::raw_with_gas(&[0x8B, modrm], &gas));
                } else {
                    self.emit_insn(&Insn::raw_with_gas(&[rex, 0x8B, modrm], &gas));
                }
            },
            _ => {
                if dst != src {
                    self.mov(dst, src);
                }
            },
        }
    }

    /// 从 `mov_reg_truncate` 立下的规范形态**按声明宽度扩展**读出。
    /// 无符号与 8 字节就是普通 64 位搬运（高位本来就是 0 或有效位）；有符号窄类型
    /// 补 movsx/movsxd，与栈槽路径的 `movsxd`/`load_stack_sized` 同口径。
    pub fn mov_reg_extend(&mut self, dst: u8, src: u8, sz: usize, signed: bool) {
        if sz >= 8 || !signed {
            if dst != src {
                self.mov(dst, src);
            }
            return;
        }
        let modrm = 0xC0u8 | ((dst & 7) << 3) | (src & 7);
        let mut rex = 0x48u8;
        if dst >= 8 {
            rex |= 0x04;
        }
        if src >= 8 {
            rex |= 0x01;
        }
        // P9 方案 C: 挂 gas_text.
        //   sz=1: movsbl src8, %dst64   (0F BE)
        //   sz=2: movswl src16, %dst64  (0F BF)
        //   sz=4: movsxd src32, %dst64 (63)
        let (bytes, gas): (Vec<u8>, String) = match sz {
            1 => (
                vec![rex, 0x0F, 0xBE, modrm],
                format!(
                    "movsbl {}, {}",
                    GAS_REG8[(src & 15) as usize],
                    GAS_REG64[(dst & 15) as usize],
                ),
            ),
            2 => (
                vec![rex, 0x0F, 0xBF, modrm],
                format!(
                    "movswl {}, {}",
                    GAS_REG16[(src & 15) as usize],
                    GAS_REG64[(dst & 15) as usize],
                ),
            ),
            _ => (
                vec![rex, 0x63, modrm],
                format!(
                    "movsxd {}, {}",
                    GAS_REG32[(src & 15) as usize],
                    GAS_REG64[(dst & 15) as usize],
                ),
            ),
        };
        self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
    }


    pub fn mov_imm(&mut self, reg: u8, v: i128) {
        let reg_name = GAS_REG64[(reg & 15) as usize];
        if v >= i32::MIN as i128 && v <= i32::MAX as i128 {
            // 使用 MOV r/m64, imm32 (REX.W + 0xC7 /0) 符号扩展 32 位立即数到 64 位
            // 避免 MOV r32, imm32 的零扩展导致负数比较错误
            let rex = 0x48 + if reg >= 8 { 0x01 } else { 0 };
            let mut bytes: Vec<u8> = Vec::with_capacity(8);
            bytes.extend_from_slice(&[rex, 0xC7, 0xC0 + (reg & 7)]);
            bytes.extend_from_slice(&(v as i32).to_le_bytes());
            // AT&T: `mov $<imm>, %reg` (与 render_one.rs 输出对拍).
            let gas = format!("mov {}, {}", gas_dec_imm(v as i64), reg_name);
            self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
        } else {
            let mut bytes: Vec<u8> = Vec::with_capacity(10);
            bytes.push(0x48 + if reg >= 8 { 0x01 } else { 0 });
            bytes.push(0xb8 + (reg & 7));
            bytes.extend_from_slice(&(v as i64).to_le_bytes());
            let gas = format!("mov {}, {}", gas_dec_imm(v as i64), reg_name);
            self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
        }
    }

    // ========================================================================
    // Stack instructions
    // ========================================================================

    pub fn push(&mut self, reg: u8) {
        // 步骤 3: 走类型化 Stack{Push, B64, Reg(reg)}. 原手写字节按 reg 号 if/else:
        //   reg<8:  0x50 + reg
        //   reg>=8: 0x41, 0x50 + (reg & 7)
        // encode() 已按同一规则合并. 兜底保留原手写 if/else 分支以防形态未命中.
        let gas = format!("push {}", GAS_REG64[(reg & 15) as usize]);
        let insn = Insn::new(
            X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Push,
                width: Width::B64,
                dst: OpcodeOperand::Reg(reg, Width::B64),
            },
            &[],
        );
        // 直接覆盖 gas_text: 类型化构造子默认 gas_text=None.
        let mut insn = insn;
        insn.gas_text = Some(gas);
        if self.emit_insn(&insn) {
            return;
        }
        // 兜底: 理论上 B64 reg 形态恒命中, 只在 encode 意外返回 None 时走.
        let (lo, hi) = match reg {
            0 => (0x50, None),
            1 => (0x51, None),
            2 => (0x52, None),
            3 => (0x53, None),
            7 => (0x57, None),
            6 => (0x56, None),
            8 => (0x50, Some(0x41)),
            9 => (0x51, Some(0x41)),
            10 => (0x52, Some(0x41)),
            11 => (0x53, Some(0x41)),
            12 => (0x54, Some(0x41)),
            13 => (0x55, Some(0x41)),
            14 => (0x56, Some(0x41)),
            15 => (0x57, Some(0x41)),
            _ => panic!("push: unsupported reg {}", reg),
        };
        let bytes: Vec<u8> = if let Some(pfx) = hi {
            vec![pfx, lo]
        } else {
            vec![lo]
        };
        let gas = format!("push {}", GAS_REG64[(reg & 15) as usize]);
        self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
    }

    pub fn pop(&mut self, reg: u8) {
        // 步骤 3: 走类型化 Stack{Pop, B64, Reg(reg)}.
        //   reg<8:  0x58 + reg
        //   reg>=8: 0x41, 0x58 + (reg & 7)
        let gas = format!("pop {}", GAS_REG64[(reg & 15) as usize]);
        let insn = Insn::new(
            X86_64Opcode::Stack {
                op: crate::codegen::x86_64::opcode::StackOp::Pop,
                width: Width::B64,
                dst: OpcodeOperand::Reg(reg, Width::B64),
            },
            &[],
        );
        let mut insn = insn;
        insn.gas_text = Some(gas);
        if self.emit_insn(&insn) {
            return;
        }
        // 兜底.
        let (lo, hi) = match reg {
            0 => (0x58, None),
            1 => (0x59, None),
            2 => (0x5a, None),
            3 => (0x5b, None),
            7 => (0x5f, None),
            6 => (0x5e, None),
            8 => (0x58, Some(0x41)),
            9 => (0x59, Some(0x41)),
            10 => (0x5a, Some(0x41)),
            11 => (0x5b, Some(0x41)),
            12 => (0x5c, Some(0x41)),
            13 => (0x5d, Some(0x41)),
            14 => (0x5e, Some(0x41)),
            15 => (0x5f, Some(0x41)),
            _ => panic!("pop: unsupported reg {}", reg),
        };
        let bytes: Vec<u8> = if let Some(pfx) = hi {
            vec![pfx, lo]
        } else {
            vec![lo]
        };
        let gas = format!("pop {}", GAS_REG64[(reg & 15) as usize]);
        self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
    }

    pub fn store_rsp_qword(&mut self, reg: u8, off: usize) {
        // 步骤 2a: 走类型化 MovRm{B64, [rsp+off], reg}. 原手写字节:
        //   rex=48|(reg>=8?04:0), 0x89, ModRM[44|(reg<<3)|24|off], disp8/disp32.
        // encode() 会自己处理 SIB (rm=4=rsp) / disp8 / disp32 / REX.R.
        // P9 方案 C: 挂 gas_text. AT&T: mov %reg, off(%rsp).
        let gas = format!("mov {}, {}", GAS_REG64[(reg & 15) as usize], gas_mem_disp(4, off as i64));
        let mut insn = Insn::new(
            X86_64Opcode::MovRm {
                width: Width::B64,
                dst: OpcodeOperand::Mem(MemOperand::bp_disp(4, off as i32)),
                src: OpcodeOperand::Reg(reg, Width::B64),
            },
            &[],
        );
        insn.gas_text = Some(gas);
        let ok = self.emit_insn(&insn);
        if !ok {
            // encode 未实现兜底: 保留原始手写路径 (理论上本形态恒命中).
            let rex = 0x48 | if reg >= 8 { 0x04 } else { 0 };
            let mut bytes: Vec<u8> = vec![rex, 0x89];
            if off < 128 {
                bytes.extend_from_slice(&[0x44 | ((reg & 7) << 3), 0x24, off as u8]);
            } else {
                bytes.push(0x84 | ((reg & 7) << 3));
                bytes.push(0x24);
                bytes.extend_from_slice(&(off as u32).to_le_bytes());
            }
            self.emit_insn(&Insn::raw_with_gas(&bytes, &format!("mov {}, {}", GAS_REG64[(reg & 15) as usize], gas_mem_disp(4, off as i64))));
        }
    }

    pub fn load_rsp_qword(&mut self, reg: u8, off: usize) {
        // P9 方案 C: 挂 gas_text. AT&T: mov off(%rsp), %reg.
        let gas = format!("mov {}, {}", gas_mem_disp(4, off as i64), GAS_REG64[(reg & 15) as usize]);
        let mut insn = Insn::new(
            X86_64Opcode::MovRm {
                width: Width::B64,
                dst: OpcodeOperand::Reg(reg, Width::B64),
                src: OpcodeOperand::Mem(MemOperand::bp_disp(4, off as i32)),
            },
            &[],
        );
        insn.gas_text = Some(gas);
        let ok = self.emit_insn(&insn);
        if !ok {
            let rex = 0x48 | if reg >= 8 { 0x04 } else { 0 };
            let mut bytes: Vec<u8> = vec![rex, 0x8B];
            if off < 128 {
                bytes.extend_from_slice(&[0x44 | ((reg & 7) << 3), 0x24, off as u8]);
            } else {
                bytes.push(0x84 | ((reg & 7) << 3));
                bytes.push(0x24);
                bytes.extend_from_slice(&(off as u32).to_le_bytes());
            }
            self.emit_insn(&Insn::raw_with_gas(&bytes, &format!("mov {}, {}", gas_mem_disp(4, off as i64), GAS_REG64[(reg & 15) as usize])));
        }
    }

    // ========================================================================
    // R13-relative addressing (frame pointer alternative)
    // ========================================================================

    pub fn emit_lea_r13(&mut self, reg: u8, off: i64) {
        if Self::fits_disp32(off) {
            // 步骤 2b: 收敛到 Lea 变体. 形态: lea reg64, [r13+disp].
            // 原手写字节: 49|rex_r, 0x8D, modrm(disp8|disp32).
            let mut insn = Insn::new(
                X86_64Opcode::Lea {
                    dst: OpcodeOperand::Reg(reg, Width::B64),
                    mem: MemOperand::bp_disp(13, off as i32),
                    rip_relative: false,
                },
                &[],
            );
            let gas = format!("lea {}, {}", gas_mem_disp(13, off), GAS_REG64[(reg & 15) as usize]);
            insn.gas_text = Some(gas);
            if self.emit_insn(&insn) {
                return;
            }
            // 兜底: 与原手写完全一致 (理论上不会走到).
            let rex_r = if reg >= 8 { 0x04 } else { 0 };
            let gas = format!("lea {}, {}", gas_mem_disp(13, off), GAS_REG64[(reg & 15) as usize]);
            self.emit_insn(&Insn::raw_with_gas(&[0x49 | rex_r, 0x8D], &gas));
            self.emit_r13_modrm_disp(reg, off);
        } else {
            self.mov_imm(11, off as i128);
            self.emit_insn(&Insn::raw(&[0x4D, 0x8D, 0x5C, 0x1D, 0x00])); // lea r11, [r13+r11]
            let rex = 0x49 | if reg >= 8 { 0x04 } else { 0 } | 0x01;
            self.emit_insn(&Insn::raw(&[rex, 0x8D]));
            self.emit_reg_indirect_modrm(reg, 11);
        }
    }

    pub fn emit_mov_mem_r13(&mut self, reg: u8, off: i64) {
        self.load_stack(reg, off);
    }

    pub fn emit_r13_modrm_disp(&mut self, reg_field: u8, off: i64) -> bool {
        if Self::fits_disp8(off) {
            self.emit_insn(&Insn::raw(&[0x45 | ((reg_field & 7) << 3), off as i8 as u8]));
            true
        } else if Self::fits_disp32(off) {
            self.emit_insn(&Insn::raw(&[0x85 | ((reg_field & 7) << 3)]));
            self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
            true
        } else {
            false
        }
    }

    pub fn emit_reg_indirect_modrm(&mut self, reg_field: u8, base: u8) {
        let rm = base & 7;
        if rm == 4 {
            self.emit_insn(&Insn::raw(&[0x04 | ((reg_field & 7) << 3), 0x24]));
        } else if rm == 5 {
            self.emit_insn(&Insn::raw(&[0x45 | ((reg_field & 7) << 3), 0x00]));
        } else {
            self.emit_insn(&Insn::raw(&[(reg_field & 7) << 3 | rm]));
        }
    }

    // ========================================================================
    // Stack frame variable management
    // ========================================================================

    pub fn var_offset(&mut self, name: &str) -> i64 {
        if let Some(&(off, _)) = self.var_map.get(name) {
            return off;
        }
        // 兜底路径必须是**异常**而不是常规：一个没在规划期登记的名字在这里只能按
        // `Type::Int` 猜一个槽（非 int 的对象因此被按错宽度存取），而且晚分配带的容量
        // 也因此无法预知（§11.23）。规划期现在已经把体内被引用的名字都登过
        // （`plan_late_slot_budget`），所以走到这里就是后端不变量破坏：点名，本次
        // 编译失败（Error 级诊断），仍分配一个槽只为让后续流程不崩。
        self.report_internal_bug(
            "E1134",
            format!(
                "frame slot for '{}' was never planned; the emission-time fallback guesses Type::Int and is a backend invariant violation",
                name
            ),
            "把这个名字加进规划期登记（`func_setup::plan_late_slot_budget` / \
                 `collect_frame_var_decls`），或在发射点显式 register_var 并给出真实类型",
        );
        if std::env::var_os("DCC_LATE_ALLOC").is_some() {
            eprintln!(
                "LATE func={} name={} slots=1 ty=Int",
                self.current_func_name, name
            );
        }
        // 懒分配路径: 委托给 register_var 以保证偏移计算一致
        self.register_var(name, &Type::Int)
    }

    pub fn load_stack(&mut self, reg: u8, off: i64) {
        self.load_stack_sized(reg, off, 8);
    }

    pub fn store_stack(&mut self, reg: u8, off: i64) {
        if Self::fits_disp32(off) {
            // 手写完整字节序列 (REX+0x89+ModRM+disp), 携带 gas_text.
            // 形态: mov %reg, off(%r13) —— reg 在 ModRM.reg, r/m=r13=13=5 (低 3 位).
            // REX:  0x49 = REX.W + REX.B (r13 是 r/m 高位, 需 B=1). reg>=8 加 REX.R.
            let mut rex: u8 = 0x49;
            if reg >= 8 {
                rex |= 0x04;
            }
            let reg_field = reg & 7;
            let (modrm, disp_bytes) = gas_modrm_disp_bytes(reg_field, 13, off);
            let mut bytes: Vec<u8> = Vec::with_capacity(2 + 1 + disp_bytes.len());
            bytes.push(rex);
            bytes.push(0x89); // mov r/m64, r64
            bytes.push(modrm);
            bytes.extend_from_slice(&disp_bytes);
            let gas = format!("mov {}, {}", GAS_REG64[(reg & 15) as usize], gas_mem_disp(13, off));
            self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
        } else {
            self.emit_lea_r13(11, off);
            let mut rex = 0x49u8;
            if reg >= 8 {
                rex |= 0x04;
            }
            self.emit_insn(&Insn::raw(&[rex, 0x89]));
            self.emit_reg_indirect_modrm(reg, 11);
        }
    }

    /// 把 `src` 的值按 `sz` 字节存到 `base` 指向的地址（`[base]` 无位移形式）。
    ///
    /// `store_stack` 只会写帧内 `[r13+disp]`；而内联汇编的输出操作数绑到
    /// `a[i]` / `*p` / `s.f` 这类**非 Ident 左值**时，目标地址是运行时算出来的指针，
    /// 必须走这条。ModRM/SIB 交给 `emit_reg_indirect_modrm`，因此 base 可以是任意
    /// GP（含需要 SIB 的 rsp/r12 与需要 disp8 的 rbp/r13）。
    pub fn store_reg_to_ptr(&mut self, src: u8, base: u8, sz: usize) {
        // `88 /r`（mov r/m8, r8）的位序：ModRM.reg 是**值**寄存器 src，r/m 是基址
        // base。所以 REX.R 只看 src>=8，REX.B 只看 base>=8。
        // src 落在 r4-r7（spl/bpl/sil/dil）时只需要「存在 REX 前缀」，绝不能去置
        // REX.B —— 那会把 base 搬到高 8 个寄存器（实测 `mov %sil,(%rbx)` 被编成
        // `mov %sil,(%r11)`，直接段错误）。need_rex 因此单独算。
        let mut rex = 0x40u8;
        if src >= 8 {
            rex |= 0x04;
        }
        if base >= 8 {
            rex |= 0x01;
        }
        let need_rex = rex != 0x40 || (sz == 1 && (4..8).contains(&(src & 7)));
        match sz {
            1 => {
                if need_rex {
                    self.emit_insn(&Insn::raw(&[rex]));
                }
                self.emit_insn(&Insn::raw(&[0x88]));
            },
            2 => {
                self.emit_insn(&Insn::raw(&[0x66]));
                if rex != 0x40 {
                    self.emit_insn(&Insn::raw(&[rex]));
                }
                self.emit_insn(&Insn::raw(&[0x89]));
            },
            4 => {
                if rex != 0x40 {
                    self.emit_insn(&Insn::raw(&[rex]));
                }
                self.emit_insn(&Insn::raw(&[0x89]));
            },
            _ => {
                self.emit_insn(&Insn::raw(&[rex | 0x08]));
                self.emit_insn(&Insn::raw(&[0x89]));
            },
        }
        self.emit_reg_indirect_modrm(src, base);
    }

    /// `[base] <- xmm`：`bits` = 32 (movss) / 64 (movsd) / 128 (movups)。
    /// 编码与 `store_xmm32_stack`/`store_xmm64_stack` 同族，只是寻址换成 `[base]`，
    /// 用于「SSE 输出绑到 `a[i]` / `*p` 这类非 Ident 左值」的写回
    /// （见 `writeback_asm_general_lvalue`）。
    pub fn store_xmm_to_ptr(&mut self, xmm: u8, base: u8, bits: u8) {
        let prefix: u8 = match bits {
            32 => 0xF3,
            64 => 0xF2,
            _ => 0,
        };
        let mut rex = 0x40u8;
        if xmm >= 8 {
            rex |= 0x04;
        }
        if base >= 8 {
            rex |= 0x01;
        }
        if prefix != 0 {
            self.emit_insn(&Insn::raw(&[prefix]));
        }
        if rex != 0x40 {
            self.emit_insn(&Insn::raw(&[rex]));
        }
        self.emit_insn(&Insn::raw(&[0x0F, 0x11]));
        self.emit_reg_indirect_modrm(xmm, base);
    }

    pub fn load_stack_sized(&mut self, reg: u8, off: i64, size: usize) {
        if Self::fits_disp32(off) {
            // 与原手写完全对齐:
            //   size=1:  0x41|rex_r, 0x0F, 0xB6, modrm(disp8/disp32) — movzbl
            //   size=2:  0x41|rex_r, 0x0F, 0xB7                       — movzwl
            //   size=4:  0x41|rex_r, 0x8B                             — movl
            //   size=8:  0x49|rex_r, 0x8B                             — movq
            let rex_r = if reg >= 8 { 0x04 } else { 0 };
            let (opc_prefix, opc): (u8, &[u8]) = match size {
                1 => (0x41 | rex_r, &[0x0F, 0xB6]),
                2 => (0x41 | rex_r, &[0x0F, 0xB7]),
                4 => (0x41 | rex_r, &[0x8B]),
                _ => (0x49 | rex_r, &[0x8B]),
            };
            let (modrm, disp_bytes) = gas_modrm_disp_bytes(reg & 7, 13, off);
            let mut bytes: Vec<u8> = Vec::with_capacity(1 + opc.len() + 1 + disp_bytes.len());
            bytes.push(opc_prefix);
            bytes.extend_from_slice(opc);
            bytes.push(modrm);
            bytes.extend_from_slice(&disp_bytes);
            // GAS 助记符 (与 objdump/老 disassemble_range 输出对齐):
            //   size=1: movzbl off(%r13), %reg32  (0x0F 0xB6)
            //   size=2: movzwl off(%r13), %reg32  (0x0F 0xB7)
            //   size=4: mov    off(%r13), %reg32  (0x8B, REX.W=0)
            //   size=8: mov   off(%r13), %reg64  (0x8B, REX.W=1)  ← 老反推输出是 "mov", 不是 "movq"
            let (mnem, reg_name): (&str, &str) = match size {
                1 => ("movzbl", GAS_REG32[(reg & 15) as usize]),
                2 => ("movzwl", GAS_REG32[(reg & 15) as usize]),
                4 => ("mov", GAS_REG32[(reg & 15) as usize]),
                _ => ("mov", GAS_REG64[(reg & 15) as usize]),
            };
            let gas = format!("{} {}, {}", mnem, gas_mem_disp(13, off), reg_name);
            self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
        } else {
            self.emit_lea_r13(11, off);
            let rex = match size {
                1 | 2 | 4 => 0x41 | if reg >= 8 { 0x04 } else { 0 },
                _ => 0x49 | if reg >= 8 { 0x04 } else { 0 },
            };
            match size {
                1 => self.emit_insn(&Insn::raw(&[rex, 0x0F, 0xB6])),
                2 => self.emit_insn(&Insn::raw(&[rex, 0x0F, 0xB7])),
                4 => self.emit_insn(&Insn::raw(&[rex, 0x8B])),
                _ => self.emit_insn(&Insn::raw(&[rex, 0x8B])),
            };
            self.emit_reg_indirect_modrm(reg, 11);
        }
    }

    pub fn emit_store(&mut self, src_reg: u8, offset: i64, size: usize) {
        use crate::codegen::x86_64::emit::gas::{REG64 as GAS_REG64, REG8 as GAS_REG8, REG16 as GAS_REG16, REG32 as GAS_REG32};
        let emit_direct = |this: &mut Self, src_reg: u8, offset: i64, size: usize| {
            // 构造完整字节串 + 完整 gas_text, 一条 Insn 发射.
            let mut bytes: Vec<u8> = Vec::new();
            let (rex, op, width_prefix) = match size {
                1 => (0x41u8 | if src_reg >= 8 { 0x04 } else { 0 }, 0x88u8, None),
                2 => (0x41u8 | if src_reg >= 8 { 0x04 } else { 0 }, 0x89u8, Some(0x66u8)),
                4 => (0x41u8 | if src_reg >= 8 { 0x04 } else { 0 }, 0x89u8, None),
                8 => (0x49u8 | if src_reg >= 8 { 0x04 } else { 0 }, 0x89u8, None),
                _ => return,
            };
            if let Some(p) = width_prefix { bytes.push(p); }
            bytes.push(rex);
            bytes.push(op);
            // emit_r13_modrm_disp: fits_disp8 或 fits_disp32.
            let off_u8: u8 = (offset & 0xFF) as u8;
            let modrm_disp8: u8 = 0x45 | ((src_reg & 7) << 3);
            let modrm_disp32: u8 = 0x85 | ((src_reg & 7) << 3);
            let gas_reg_lo = match size {
                1 => GAS_REG8[(src_reg & 15) as usize].to_string(),
                2 => GAS_REG16[(src_reg & 15) as usize].to_string(),
                4 => GAS_REG32[(src_reg & 15) as usize].to_string(),
                _ => GAS_REG64[(src_reg & 15) as usize].to_string(),
            };
            if Self::fits_disp8(offset) {
                bytes.push(modrm_disp8);
                bytes.push(off_u8);
                let gas = format!("mov {}, {}(%r13)", gas_reg_lo, offset);
                this.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            } else if Self::fits_disp32(offset) {
                bytes.push(modrm_disp32);
                bytes.extend_from_slice(&(offset as i32).to_le_bytes());
                let gas = format!("mov {}, {}(%r13)", gas_reg_lo, offset);
                this.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
            }
        };

        if Self::fits_disp32(offset) {
            emit_direct(self, src_reg, offset, size);
            return;
        }

        // big disp: 走 lea r11, off(r13); mov reg, (r11) 路径
        self.emit_lea_r13(11, offset);
        match size {
            1 => {
                let rex = 0x41u8 | if src_reg >= 8 { 0x04 } else { 0 };
                self.emit_insn(&Insn::raw(&[rex, 0x88]));
                self.emit_reg_indirect_modrm(src_reg, 11);
            },
            2 => {
                let rex = 0x41u8 | if src_reg >= 8 { 0x04 } else { 0 };
                self.emit_insn(&Insn::raw(&[0x66, rex, 0x89]));
                self.emit_reg_indirect_modrm(src_reg, 11);
            },
            4 => {
                let rex = 0x41u8 | if src_reg >= 8 { 0x04 } else { 0 };
                self.emit_insn(&Insn::raw(&[rex, 0x89]));
                self.emit_reg_indirect_modrm(src_reg, 11);
            },
            8 => {
                let rex = 0x49u8 | if src_reg >= 8 { 0x04 } else { 0 };
                self.emit_insn(&Insn::raw(&[rex, 0x89]));
                self.emit_reg_indirect_modrm(src_reg, 11);
            },
            _ => {},
        }
    }

    /// 「地址与目的地是同一个号」的按类型装载（`mov (%dst) -> dst` 一族）。
    ///
    /// ModRM 的 reg 与 r/m 两个字段都是 dst ⇒ `dst>=8` 必须**同时**置 REX.R 与 REX.B
    /// （0x05）；而 rm==4（%rsp/%r12）在 mod=00 下必须带 SIB、rm==5（%rbp/%r13）在
    /// mod=00 下是 disp32 绝对寻址 —— 后两条由 [`Self::emit_reg_indirect_modrm`] 单源承担。
    ///
    /// 为什么要有这个入口：站点以前手写 `((dst&7)<<3)|(dst&7)`，既不发 REX 也不发 SIB，
    /// 于是 dst<8 时逐字正确、dst 落在 4/5/12/13 时读的是**别人的地址**。P12/P16 把家与
    /// 右操作数暂存号灌进高位寄存器之后，这条从死分支变成常走分支（实测
    /// `tests/fixtures/c/seg_object_body.c` 的 `gs.b += 4` 在 -O0 直接 SIGSEGV：地址算进
    /// %r8，装载却按 `(%rax)` 读，而 %rax 当时是右操作数 4）。dst<8 时发出的字节与旧
    /// 写法逐字相同（三档哈希是裁判）。
    pub fn emit_load_self_typed(&mut self, size: usize, is_signed: bool, dst: u8) {
        // 位序: 自反 ModRM, 同一个号既在 reg 又在 r/m ⇒ 两个扩展位都要置, 判据见 §7.21
        let rb = if dst >= 8 { 0x05 } else { 0u8 };
        let reg64 = crate::codegen::x86_64::emit::gas::REG64[(dst & 15) as usize];
        let reg32 = crate::codegen::x86_64::emit::gas::REG32[(dst & 15) as usize];
        // ModRM / SIB 字节构造: rm 字段 = dst & 7
        let rm = dst & 7;
        let (modrm_byte, extra): (u8, Option<u8>) = if rm == 4 {
            // SIB: mod=00, reg=dst&7, rm=100 → ModRM=reg<<3|0x04, SIB=0x24 (no base)
            (((dst & 7) << 3) | 0x04, Some(0x24))
        } else if rm == 5 {
            // mod=10 (disp32), reg=dst&7, rm=101 → ModRM=0x80|reg<<3|0x05, disp32=0
            // 注意: mod=00 时 rm=101 是 RIP-relative, 不是 [rbp+disp32]!
            (0x80 | ((dst & 7) << 3) | 0x05, None)
        } else {
            (((dst & 7) << 3) | rm, None)
        };
        let mem_disp = if rm == 5 { vec![0x00, 0x00, 0x00, 0x00] } else { vec![] };
        let gas_mem = match rm {
            4 => format!("({})", reg64),   // SIB 无 base: 一般不会出现 rsp/r12 自反, 保守写 (reg)
            5 => format!("({})", reg64),    // rbp/r13 自反, 走 disp32=0
            _ => format!("({})", reg64),
        };
        let (bytes, gas): (Vec<u8>, String) = match size {
            1 => {
                let mut b = vec![0x48 | rb, 0x0F, if is_signed { 0xBE } else { 0xB6 }, modrm_byte];
                if let Some(e) = extra { b.push(e); }
                b.extend_from_slice(&mem_disp);
                let op = if is_signed { "movsbq" } else { "movzbq" };
                (b, format!("{} {}, {}", op, gas_mem, reg64))
            },
            2 => {
                let mut b = vec![0x48 | rb, 0x0F, if is_signed { 0xBF } else { 0xB7 }, modrm_byte];
                if let Some(e) = extra { b.push(e); }
                b.extend_from_slice(&mem_disp);
                let op = if is_signed { "movswq" } else { "movzwq" };
                (b, format!("{} {}, {}", op, gas_mem, reg64))
            },
            4 => {
                if is_signed {
                    // movsxd r64, r/m32：REX.W 必留（32 位源要符号扩展到 64 位）
                    let mut b = vec![0x48 | rb, 0x63, modrm_byte];
                    if let Some(e) = extra { b.push(e); }
                    b.extend_from_slice(&mem_disp);
                    (b, format!("movsxd {}, {}", gas_mem, reg64))
                } else {
                    // mov r32, r/m32：32 位写自动零扩展高 32 位，不带 REX.W；低 8 号时
                    // 一个前缀都不发（裸 0x40 不扩任何字段，只多占一字节）。
                    let mut b = Vec::new();
                    if rb != 0 { b.push(0x40 | rb); }
                    b.push(0x8B);
                    b.push(modrm_byte);
                    if let Some(e) = extra { b.push(e); }
                    b.extend_from_slice(&mem_disp);
                    (b, format!("mov {}, {}", gas_mem, reg32))
                }
            },
            _ => {
                let mut b = vec![0x48 | rb, 0x8B, modrm_byte];
                if let Some(e) = extra { b.push(e); }
                b.extend_from_slice(&mem_disp);
                (b, format!("mov {}, {}", gas_mem, reg64))
            },
        };
        // 对 rm==4/5 的 SIB/disp32 情况已按 emit_reg_indirect_modrm 同规则手工拼好字节.
        self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
    }

    pub fn emit_load_mem_typed(&mut self, size: usize, ty: &Type) {
        if matches!(ty, Type::Float | Type::Double | Type::LongDouble) {
            // 单一真源：long double 必须 `fld tbyte [rax]`，
            // 不能再 `movsd` 读低 8 字节（真 80-bit 落槽后低 8 字节只是尾数）。
            self.emit_load_scalar_float_at(ty, 0);
            return;
        }
        match ty {
            Type::Float => { self.emit_insn(&Insn::raw_with_gas(&[0xF3, 0x0F, 0x10, 0x00], "movss (%rax), %xmm0")); } // movss xmm0, [rax]
            Type::Double => { self.emit_insn(&Insn::raw_with_gas(&[0xF2, 0x0F, 0x10, 0x00], "movsd (%rax), %xmm0")); } // movsd xmm0, [rax]
            _ => {
                if size < 8 && Self::is_signed_int(ty) {
                    match size {
                        1 => { self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x0F, 0xBE, 0x00], "movsbq (%rax), %rax")); } // movsbq rax, [rax]
                        2 => { self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x0F, 0xBF, 0x00], "movswq (%rax), %rax")); } // movswq rax, [rax]
                        _ => { self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x63, 0x00], "movsxd (%rax), %rax")); }       // movsxd rax, [rax]
                    };
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
            // 步骤 1 打样: 类型化 AluRm{Sub, B64, Rsp=Reg(4), Imm(imm32)}.
            // encode() 必须输出 `48 81 EC <imm32 LE>` (与原手写逐字节一致).
            let gas = format!("sub {}, %rsp", gas_dec_imm(bytes));
            let insn = Insn::new(
                X86_64Opcode::AluRm {
                    op: AluOp::Sub,
                    width: Width::B64,
                    dst: OpcodeOperand::Reg(4, Width::B64),
                    src: OpcodeOperand::Imm(bytes),
                },
                &[],
            );
            let mut insn = insn;
            insn.gas_text = Some(gas);
            self.emit_insn(&insn);
        } else {
            self.mov_imm(11, bytes as i128);
            let b = [0x4C, 0x29, 0xDC]; // sub rsp, r11
            let gas = format!("sub {}, {}", GAS_REG64[11], GAS_REG64[4]);
            self.emit_insn(&Insn::raw_with_gas(&b, &gas));
        }
    }

    pub fn emit_add_rsp(&mut self, bytes: i64) {
        if bytes <= 0 {
            return;
        }
        if bytes <= i32::MAX as i64 {
            let gas = format!("add {}, %rsp", gas_dec_imm(bytes));
            let insn = Insn::new(
                X86_64Opcode::AluRm {
                    op: AluOp::Add,
                    width: Width::B64,
                    dst: OpcodeOperand::Reg(4, Width::B64),
                    src: OpcodeOperand::Imm(bytes),
                },
                &[],
            );
            let mut insn = insn;
            insn.gas_text = Some(gas);
            self.emit_insn(&insn);
        } else {
            self.mov_imm(11, bytes as i128);
            let b = [0x4C, 0x01, 0xDC]; // add rsp, r11
            let gas = format!("add {}, {}", GAS_REG64[11], GAS_REG64[4]);
            self.emit_insn(&Insn::raw_with_gas(&b, &gas));
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
        self.emit_insn(&Insn::raw(&[rex, 0x0F, 0xAF, 0xC0 | ((dst & 7) << 3) | (src & 7)]));
    }

    pub fn imul_imm(&mut self, reg: u8, imm: i128) {
        let rex = if reg >= 8 { 0x4D } else { 0x48 };
        let modrm = 0xC0 | ((reg & 7) << 3) | (reg & 7);
        self.emit_insn(&Insn::raw(&[rex, 0x69, modrm]));
        self.emit_insn(&Insn::raw(&(imm as i32).to_le_bytes()));
    }

    /// Increment register by 1.
    pub fn increment(&mut self, reg: u8, sz: usize) {
        // 步骤 2c: 收敛到 UnaryRm{Inc}. 原手写按宽度分发, 逐位对照:
        //   sz=1:  40|(reg>=8?01:00), FE, C0|reg     (FE /0, REX.B 仅当 reg>=8)
        //   sz=2:  66, [41 if reg>=8], FF, C0|reg
        //   sz=4:  [41 if reg>=8], FF, C0|reg
        //   sz=8:  48|(reg>=8?01:00), FF, C0|reg
        let width = match sz {
            1 => Width::B8,
            2 => Width::B16,
            4 => Width::B32,
            _ => Width::B64,
        };
        let insn = Insn::new(
            X86_64Opcode::UnaryRm {
                op: crate::codegen::x86_64::opcode::UnaryOp::Inc,
                width,
                dst: OpcodeOperand::Reg(reg, width),
            },
            &[],
        );
        if self.emit_insn(&insn) {
            return;
        }
        // 兜底 (理论上本形态恒命中, 仅当 encode 返回 None 时走).
        if sz == 1 {
            let rex = 0x40u8 | if reg >= 8 { 0x01 } else { 0 };
            self.emit_insn(&Insn::raw(&[rex, 0xFE, 0xC0 | (reg & 7)]));
        } else if sz == 2 {
            if reg >= 8 {
                self.emit_insn(&Insn::raw(&[0x66, 0x41, 0xFF, 0xC0 | (reg & 7)]));
            } else {
                self.emit_insn(&Insn::raw(&[0x66, 0xFF, 0xC0 | (reg & 7)]));
            }
        } else if sz == 4 {
            if reg >= 8 {
                self.emit_insn(&Insn::raw(&[0x41, 0xFF, 0xC0 | (reg & 7)]));
            } else {
                self.emit_insn(&Insn::raw(&[0xFF, 0xC0 | (reg & 7)]));
            }
        } else {
            let rex = if reg >= 8 { 0x49 } else { 0x48 };
            self.emit_insn(&Insn::raw(&[rex, 0xFF, 0xC0 | (reg & 7)]));
        }
    }

    // ========================================================================
    // Call / Ret
    // ========================================================================

    pub fn call_label(&mut self, l: &str) {
        // 前置三的覆盖缺口检查：call 序列的存/取窗口开在 `abi/call.rs::emit_sysv_call` 两端，
        // 而 `call_label` 是本后端** primitive**，在 `abi/call.rs` 之外还有二十多个调用点
        // （内置函数降级成 `memcmp`/`memchr`/`libc_name`、`__fentry__`/`mcount`、
        // `__stack_chk_fail`、ifunc 的 CPU_INIT 那一族）。这些路径自己摆参数再直接 call，
        // 不经窗口 ⇒ 住在 caller-saved 号里的家会被毁。
        //
        // 实测证据：开池后 `tests/fixtures/c/strcasecmp.c` -O2 第 5 项失败
        // （`__builtin_strcasecmp` 降级成直接 call，而 `b2→%rsi`、`b3→%rdi` 正握着家），
        // 同一夹具不开池返回 0。
        //
        // 本检查把那个**静默错值**变成可见、可归因的失败（同一函数里已有窗口开着时不会误报）。
        // 延伸池默认关着 ⇒ `ext_home_saves` 为空 ⇒ 不进这条分支，今天的产物一字不变。
        if !self.ext_home_saves.is_empty() && self.home_overrides.is_empty() {
            self.report_internal_bug(
                "E1136",
                format!(
                    "call 到 '{}' 不经过 caller-saved 家的保存窗口：本函数有 {} 个延伸池家，\
                     它们会被这次调用毁掉",
                    l,
                    self.ext_home_saves.len()
                ),
                "把该调用点改用 `emit_sysv_call`，或在参数摆渡之前 `persist_ext_homes()`、\
                 结果消费之后 `restore_ext_homes(n)`；不得让家静默被毁",
            );
        }
        let at = self.pos();
        let bytes = [0xe8, 0, 0, 0, 0];
        let gas = format!("call {l}");
        self.emit_insn(&Insn::raw_with_gas(&bytes, &gas));
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
        let bytes = [0xe8, 0, 0, 0, 0];
        let gas = format!("call {name}");
        self.emit_insn(&Insn::raw_with_gas(&bytes, &gas)); // call rel32 placeholder
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

    /// 查「这个全局符号属于哪个段」→ x86 legacy 段前缀字节（`%fs`=0x64 / `%gs`=0x65）。
    ///
    /// 前置：`resolved` 必须是已过 `resolve_global_key` 的符号键 —— 函数体里的
    /// `static __seg_fs int x;` 在那里被换成 mangled 名，表也是按 mangled 名登记的。
    ///
    /// 返回 None ⇒ 默认段，**一个字节都不多发**：`global_seg` 只在
    /// `AddrSpace::is_segment()` 时才插条目，所以不带段限定的全局永远走无前缀分支；
    /// 段限定的符号本体与 `AddrSpace::Generic`（cs/ds/es/ss）也一律不发，与 pointee
    /// 那一侧（`abi::tyclass::ptr_seg_prefix`）口径一致。
    pub fn global_seg_prefix(&self, resolved: &str) -> Option<u8> {
        self.global_seg
            .get(resolved)
            .and_then(|a| AddrSpace::prefix_byte(*a))
    }

    /// 符号本体带段限定时，在指令最前贴段前缀字节。
    ///
    /// 前缀必须落在 legacy-prefix 区（LOCK/段/操作数尺寸/地址尺寸/REX/opcode 的硬次序）：
    /// 所以它先于 `0x66`（sz=2 的存）与 `0xF3/0xF2`（SSE mandatory prefix）发出去。
    /// 本函数**必须在任何 `self.pos()` 参与 disp32 计算之前**调用：pending 重定位要指
    /// 到 disp 字段，前缀把整条指令右移一字节，先取 pos 就全错位。
    #[inline]
    pub(crate) fn emit_global_seg(&mut self, resolved: &str) {
        if let Some(seg) = self.global_seg_prefix(resolved) {
            self.emit_insn(&Insn::raw(&[seg]));
        }
    }

    pub fn emit_global_addr(&mut self, reg: u8, name: &str) {
        let resolved = self.resolve_global_key(name).to_string();
        // 取址（LEA）**不发**段前缀，即使符号本体带 `__seg_gs`/`__seg_fs` —— 这不是保守，
        // 是硬件事实：LEA 不做内存访问，段重叠前缀对它的结果是空操作（gas 直接警告
        // “segment override on `lea' is ineffectual”；实测 `lea %fs:g(%rip)` 与不带前缀的
        // `lea g(%rip)` 得到同一个值）。所以在这里贴前缀只会给出“看起来修了”的假象：
        // 前缀字节占位、地址仍是不含段基址的裸地址，后续所有以它为基的访问仍然越段。
        //
        // 因此“先取址、后访问”那一族（`gv++` / `arr[i]` / `gs.b` / 聚合拷贝）的前缀必须
        // 落在**访问**那一笔上，单一真源是 `abi::tyclass::lvalue_seg_prefix`（它现在既认
        // pointee 的段，也认符号本体的段）；本函数只负责把裸地址算出来，与 gcc 同形
        // （gcc: `leaq arr(%rip),%rax` + `movl %gs:(%rax,%rdi,4),%eax`）。
        let disp32_pos = self.pos() + 3;
        let rex = if reg >= 8 { 0x4C } else { 0x48 };
        let gas = format!("lea {}(%rip), {}", resolved, GAS_REG64[(reg & 15) as usize]);
        self.emit_insn(&Insn::raw_with_gas(&[rex, 0x8D, 0x05 | ((reg & 7) << 3), 0, 0, 0, 0], &gas));
        self.pending_data_rels.push((disp32_pos, resolved));
    }

    pub fn emit_global_load(&mut self, reg: u8, name: &str) {
        let resolved = self.resolve_global_key(name).to_string();
        // 段限定与 TLS 是两条独立的轴：下面 `is_tls` 决定的是**重定位种类**
        // (pending_tls_rels vs pending_data_rels)，本行决定的是**指令字节**；
        // 一个符号可以同时带其中之一，也可能只有段（非 TLS 的 per-cpu 对象）。
        self.emit_global_seg(&resolved);
        let (sz, is_tls, _off, is_signed) = self
            .globals_raw
            .get(&resolved)
            .map(|g| (g.size, g.is_tls, g.offset, Self::is_signed_int(&g.ty)))
            .unwrap_or((8, false, 0, false));
        let tls_exec = is_tls && !self.relocatable;
        // 目标寄存器在 ModRM.reg 上，扩展它的是 REX.R(0x04) 而不是 REX.B(0x01)。
        // 写成 0x41 会同时干两件事：reg 字段留在低 8 个（值被写进 %eax），而 r/m 的
        // `mod=00 rm=101`（RIP 相对）被搬成 `(%r13+disp32)` —— 后者止于错寄存器，
        // 前者是静默错值。
        let pref = if reg >= 8 { 0x44 } else { 0x00 }; // REX.R for high reg
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
                self.emit_insn(&Insn::raw(&b));
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
                self.emit_insn(&Insn::raw(&b));
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
                    let dst_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(reg, 64)
                        .unwrap_or_else(|| "%rax".to_string());
                    let gas = format!("movsxd {}(%rip), {}", name, dst_name);
                    self.emit_insn(&Insn::raw_with_gas(&[rex, 0x63, modrm, 0, 0, 0, 0], &gas));
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
                    let dst_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(reg, 32)
                        .unwrap_or_else(|| "%eax".to_string());
                    let gas = format!("mov {}(%rip), {}", name, dst_name);
                    self.emit_insn(&Insn::raw_with_gas(&b, &gas));
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
                let dst_name = crate::codegen::x86_64::abi::regnames::att_name_of_num(reg, 64)
                    .unwrap_or_else(|| "%rax".to_string());
                let gas = format!("mov {}(%rip), {}", name, dst_name);
                self.emit_insn(&Insn::raw_with_gas(&[rex, 0x8B, modrm, 0, 0, 0, 0], &gas));
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
        // 同 load：段前缀只改指令字节，`is_tls` 只改重定位种类，两条轴不合并。
        self.emit_global_seg(&resolved);
        let (sz, is_tls, _off) = self
            .globals_raw
            .get(&resolved)
            .map(|g| (g.size, g.is_tls, g.offset))
            .unwrap_or((8, false, 0));
        let tls_exec = is_tls && !self.relocatable;
        let modrm = 0x05 | ((reg & 7) << 3);
        // 存回方向的**值**寄存器在 ModRM.reg 上，扩展它的是 REX.R(0x04)：
        // 旧代码 sz==1 对 reg>=8 只发全 0 的 0x40（存了 %al 而不是 %r8b），
        // sz==2/4 干脆不发前缀。8 位那支还要兼顾 %spl..%dil（4..7）只需「存在 REX」。
        // 前缀长度参与 disp 偏移计算（pending 重定位要指到 disp 字段），所以逐分支显式算。
        let rex_8 = if reg >= 8 { 0x44 } else if reg >= 4 { 0x40 } else { 0x00 };
        let rex_hi = if reg >= 8 { 0x44 } else { 0x00 };
        match sz {
            1 => {
                let pos = self.pos() + 2 + if rex_8 != 0 { 1 } else { 0 };
                if rex_8 != 0 {
                    self.emit_insn(&Insn::raw(&[rex_8, 0x88, modrm, 0, 0, 0, 0]));
                } else {
                    self.emit_insn(&Insn::raw(&[0x88, modrm, 0, 0, 0, 0]));
                }
                if tls_exec {
                    self.pending_tls_rels.push((pos, resolved.clone()));
                } else {
                    self.pending_data_rels.push((pos, resolved.clone()));
                }
            },
            2 => {
                let pos = self.pos() + 3 + if rex_hi != 0 { 1 } else { 0 };
                if rex_hi != 0 {
                    // 0x66 在 REX 之前（legacy prefix → REX → opcode 是硬性次序）
                    self.emit_insn(&Insn::raw(&[0x66, rex_hi, 0x89, modrm, 0, 0, 0, 0]));
                } else {
                    self.emit_insn(&Insn::raw(&[0x66, 0x89, modrm, 0, 0, 0, 0]));
                }
                if tls_exec {
                    self.pending_tls_rels.push((pos, resolved.clone()));
                } else {
                    self.pending_data_rels.push((pos, resolved.clone()));
                }
            },
            4 => {
                let pos = self.pos() + 2 + if rex_hi != 0 { 1 } else { 0 };
                if rex_hi != 0 {
                    self.emit_insn(&Insn::raw(&[rex_hi, 0x89, modrm, 0, 0, 0, 0]));
                } else {
                    self.emit_insn(&Insn::raw(&[0x89, modrm, 0, 0, 0, 0]));
                }
                if tls_exec {
                    self.pending_tls_rels.push((pos, resolved.clone()));
                } else {
                    self.pending_data_rels.push((pos, resolved.clone()));
                }
            },
            _ => {
                let pos = self.pos() + 3;
                let rex = if reg >= 8 { 0x4C } else { 0x48 };
                self.emit_insn(&Insn::raw(&[rex, 0x89, modrm, 0, 0, 0, 0]));
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

    /// 帧头字节数: 16 (保存的 r13 + 返回地址) + 32 (callee-saved) + temp_slots*8。
    /// 变量区与 sret/物化槽区都从帧头之下向下生长, 两条边界必须同口径。
    pub fn frame_hdr(&self) -> i64 {
        crate::codegen::x86_64::abi::layout::LayoutMap::hdr_of(self.temp_slots as i64)
    }

    /// **变量区**当前已占用的最低字节地址: 其它帧区只允许排在它之下。
    ///
    /// 变量区由 `pre_register_vars` (规划期) 与 `register_var` (运行期, 见其注释)
    /// 共同向下生长, 长度就是 `var_map_total_slots * 8`。
    pub fn var_arena_floor(&self) -> i64 {
        -(self.var_map_total_slots * 8) - self.frame_hdr() + 8
    }

    /// 下一个晚分配站点的序号（给 `__compound_lit_*` / `__vmsize_*` 这类**只在发射期才出现**
    /// 的对象做名字后缀）。
    ///
    /// 为什么不用 `self.pos()`（已发射字节数）：那个值把「名字」与「与本分配无关的机器码
    /// 长度」绑在一起 —— 任何改变前缀长度的东西（nop 填充、分支松弛、sanitizer 插桩、
    /// `-ftrivial-auto-var-init`）都会改名字；而名字只要求「同函数内每个到达点唯一」，
    /// 发射序就是那个唯一性本身的序。实测槽位**偏移**并不随 pos 变（偏移由水位算），
    /// 所以这不是一个已观测到的错值，而是一条无收益的耦合；真正决定偏移的是**到达顺序**
    /// （设计 §11.18 与本轮只读审计的那份「顺序差异风险表」），那一条仍在 §11.15 的
    /// 「late band 要搬进解算阶段」里，它必然改产物，不能靠改名完成。
    pub fn next_late_site(&mut self) -> u32 {
        self.late_site_seq = self.late_site_seq.saturating_add(1);
        self.late_site_seq
    }

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
        // 家**不**改变「要不要预留一个栈槽」这件事，所以这里**不得**因为有家就返 0。
        // 原来那句 `if self.homes.contains_key(name) { return 0; }`（P1-5）在
        // `pre_register_vars` 覆盖不到的三类子树里是真能走到的：`for` 的 init、语句表达式
        // `({...})` 里的声明、带标签的块（它们的声明只靠发射期懒注册）。返 0 就是把这个
        // 变量放到 `%r13+0` —— 而那里是保存的 r13 自身。实测（本轮只读审计带回）：
        //   `for (register int i asm("rbx") = 0; i < 3; i++) g(i);` ⇒ 内存侧读写变成
        //   `mov (%r13),%eax` / `mov %rax,(%r13)`，而读侧走 %ebx ⇒ 槽与家分裂，
        //   可执行版本直接死循环（timeout 后 rc=124）。另外 `asm/opmap.rs` 的
        //   xmm→GP 中转 scratch 拿 `var_offset(name)` 当内存用，同样要求「有家的变量也有槽」。
        // 为什么不是「省下这 8 字节」：槽本来就算在变量区里（`pre_register_vars` 不看 homes），
        // 返 0 省不下帧，只造成账目与实际不一致。
        // 分配口径与 pre_register_vars 共用单一真源 var_alloc_slots
        let slots = Self::var_alloc_slots(ty);
        // §11.23 步 3：帧规划之后的新名字登记就是**发射期晚分配**，它只能住在本函数
        // 自己的那条晚分配带里（带的尺寸是布局解算的输入，来自规划期那份可枚举预算）。
        // 以前它挤在变量区水位底下，于是需要两段只在生成期推帧坐标的代码：`min_used`
        // 夹逼（不让晚分配压穿 sret）与「`sret_region_base` 单调下移」——两者都在拿一个
        // 已经发射出去的 `sub rsp` 的帧做现场让步，就是 H1 要消除的那条静默长帧路径。
        // 带把它们代替了：上限就是带底，超出带就是预算少算（`E1133`），而不是静默重叠加。
        if self.frame_planned {
            let layout = self.frame_layout;
            let off = layout.late_band_top
                - self.late_var_cursor
                - slots.saturating_mul(8);
            self.late_var_cursor = self.late_var_cursor.saturating_add(slots.saturating_mul(8));
            self.var_map.insert(name.to_string(), (off, ty.clone()));
            self.late_slot_used = self.late_slot_used.saturating_add(slots);
            if std::env::var_os("DCC_LATE_ALLOC").is_some() {
                eprintln!(
                    "LATE func={} name={} slots={} off={} used={}/budget={}",
                    self.current_func_name,
                    name,
                    slots,
                    off,
                    self.late_slot_used,
                    self.late_slot_budget
                );
            }
            // 账面超出：静态扫描少算了这一族（只能由后端自己负责，不是用户代码错）。
            if self.late_slot_used > self.late_slot_budget {
                self.report_internal_bug(
                    "E1133",
                    format!(
                        "late frame allocation in `{}` exceeded its planned budget: {} slots used, {} reserved (last '{}')",
                        self.current_func_name, self.late_slot_used, self.late_slot_budget, name
                    ),
                    "把这一族发射期对象计进 `func_setup::late_need_*` 的静态扫描（而不是另开一条帧内分配），\
                     扫描口径必须覆盖它的所有到达站点",
                );
            }
            // 到位判据：对象的最低字节必须在带内。带为空时本判据不启用（见
            // `LayoutMap::fits_in_late_band`），所以不会在「一个晚分配也没有」上报假错。
            if !layout.fits_in_late_band(off) {
                self.report_internal_bug(
                    "E1133",
                    format!(
                        "late frame allocation in `{}` fell out of its band: '{}' at {off} is below the band bottom {} (band {}B)",
                        self.current_func_name,
                        name,
                        layout.late_band_bottom(),
                        layout.late_band_bytes
                    ),
                    "晚分配带是解算输入：带不够就是预算算少了（或是一个新的晚分配站点没被计入），\
                     把它的尺寸补进 `late_need_*` 而不是把手伸向别人的带",
                );
            }
            return off;
        }
        // 规划期路径：变量区水位。此时 sret/物化区与晚分配带都还没定坐标（帧尚未解算），
        // 新登记的变量就是普通局部，往下压变量区水位，后面由 `LayoutMap::resolve` 统一安排。
        let hdr = self.frame_hdr();
        let used = self.var_map_total_slots.saturating_mul(8);
        let off = -(used + hdr) - (slots.saturating_sub(1) * 8);
        self.var_map.insert(name.to_string(), (off, ty.clone()));
        self.var_map_total_slots = (used + slots * 8) / 8;
        // **Phase 2.2（H1 的硬边界）**：生成期的晚增长不得越过帧底。规划期这条在帧未解算
        // 时是空转（`frame_size == 0`），留着是为了「即使有人把登记推后到解算之后」也不会
        // 静默长帧。（判据口径只住在 `LayoutMap::fits_above_bottom`，本函数不另算上限。）
        self.check_frame_budget(self.var_arena_floor(), name);
        off
    }

    pub fn alloc_sret_slot(&mut self, size: usize) -> i64 {
        // 统一按 16 字节对齐向下步进: 任何尺寸序列都互不重叠
        // (旧 bump 以"调用次数 * 当前 padded"定址, 8B→32B→8B 会撞槽)。
        let padded = (size as i64 + 15) & !15;
        let cur = self.sret_watermark;
        self.sret_watermark = cur + padded;
        let off = self.sret_region_base - cur - padded;
        // 同一条判据的反方向：sret/物化带的最低字节也不得越过帧底。这里的 `off`
        // 本身就是本槽的底（槽向下生长），所以不需要再加变量区的用量。
        self.check_frame_budget(off, "<sret>");
        off
    }

    /// 生成期新增家之前问一次**解算表**：新家的最低字节是否还在帧里。越过帧底就是
    /// 后端不变量破坏，必须报错而不是继续长帧（`docs/p12-physreg-spill-design.md` §3 的 H1）。
    ///
    /// 口径是**单一标量** `low_off` 与 `LayoutMap::frame_base()` 的比较，不是「变量区字节 +
    /// sret 区字节」：两者各住自己的带，相加就是把同一段字节数两次，判据会比它声称
    /// 保护的那条不变量更紧，会把合法代码报成溢出（独立复核拓出）。真正的不变量只有一条：
    /// 帧已在 `sub rsp` 时发射出去，谁都越不过帧底。
    ///
    /// 也不拿更紧的 `arena_room` 当上限：它是诊断读数，不是可用额度（§11.4.5 那个双重
    /// 计费坑踩过一次）。发射期晚分配的到位判据不在此，而在 `register_var` 晚路径里的
    /// `LayoutMap::fits_in_late_band`（§11.23 步 3：晚分配已有自己的带，不再挤变量区）；
    /// 本函数仍负责 sret/物化区那一侧，包括 `allows_late_allocation` 那条「非零 spill band
    /// 不得被生成期增长压穿」（band=0 时它退化成今天这条帧底比较）。
    ///
    /// 只在帧已解算（`frame_size > 0`）时检查：`unresolved()` 占位表的帧大小是 0，
    /// 拿它当上限会把任何分配都误判成越界。
    fn check_frame_budget(&mut self, low_off: i64, who: &str) {
        let room = self.frame_layout;
        if room.frame_size <= 0 {
            return;
        }
        if room.allows_late_allocation(low_off) {
            return;
        }
        self.report_internal_bug(
            DIAG_FRAME_BUDGET,
            format!(
                "frame budget exhausted in `{}` while allocating '{who}': lowest byte offset \
                 {low_off} passes the frame bottom {} (frame_size {}B)",
                self.current_func_name,
                room.frame_base(),
                room.frame_size
            ),
            "帧大小在函数体之前就已发射（sub rsp），生成期只能从已预留区里取家；需要更多 \
             空间就应在 abi/layout.rs 的解算阶段加大预留，而不是让变量区压穿帧底",
        );
    }

    /// Allocate a local variable slot (generic name).
    pub fn alloc_local(&mut self, sz: usize) -> i64 {
        self.alloc_sret_slot(sz)
    }

    /// 复数 rvalue 内存对的槽偏移（帧底带内，带宽与「为什么需要这条带」见
    /// `abi::layout::CX_FRAME_BAND`）。
    ///
    /// 该地址对**所有**函数成立：帧规划把带从寄存器保存区/向量临时区里划了
    /// 出来，所以既不会被 `va_start` 的游标踩到，也不会被嵌套调用的临时槽覆盖。
    ///
    /// Phase 2 起它是解算表的读数而不是现场推算：以前有 42 处直接写
    /// `-self.frame_size + 8`，带的位置于是有 42 个理解；现在只有一个出处。
    /// （旧的同名常量 `Codegen::CX_FRAME_BAND` 与镜像字段 `Codegen::frame_size` 已删，
    /// 帧的坐标只剩 `LayoutMap` 一个出处。）
    pub fn complex_result_off(&self) -> i64 {
        self.frame_layout.rvalue_off
    }

    /// 保存当前 sret 槽水位 (用于嵌套求值作用域)。配合
    /// `restore_sret_watermark` 在其求值结束后回收槽位。所有 sret/物化槽
    /// 都在外层 block-item 内死亡, 但表达式嵌套时子表达式槽若不回收,
    /// 同一 item 内长链会推高不必要水位; save/restore 让兄弟表达式复用槽。
    pub fn save_sret_watermark(&self) -> i64 {
        self.sret_watermark
    }

    pub fn restore_sret_watermark(&mut self, mark: i64) {
        self.sret_watermark = mark;
    }

    // ========================================================================
    // XMM / SSE helpers
    // ========================================================================

    pub fn emit_xmm0_r13_disp(&mut self, opcode: &[u8], off: i64) {
        if Self::fits_disp32(off) {
            self.emit_insn(&Insn::raw(opcode));
            if Self::fits_disp8(off) {
                self.emit_insn(&Insn::raw(&[0x45, off as i8 as u8]));
            } else {
                self.emit_insn(&Insn::raw(&[0x85]));
                self.emit_insn(&Insn::raw(&(off as i32).to_le_bytes()));
            }
        } else {
            self.emit_lea_r13(11, off);
            let mut rewritten = opcode.to_vec();
            if let Some(rex) = rewritten.iter_mut().rev().find(|b| (**b & 0xF0) == 0x40) {
                *rex = (*rex & !0x01) | 0x01;
            }
            self.emit_insn(&Insn::raw(&rewritten));
            self.emit_insn(&Insn::raw(&[0x03]));
        }
    }

    /// SSE 标量存回 `%rsp` 相对槽。xmm 号在 ModRM.reg 上，所以 xmm>=8 **必须**发
    /// REX.R(0x04)；旧代码一个前缀都不发，于是 `movsd %xmm9, (%rsp)` 实际写的是
    /// `%xmm0` —— 两个不同的浮点变量会掉到同一个槽里。base=rsp 靠 SIB 表达，不需要 REX.B。
    ///
    /// 步骤 4: 改为构造 SimdMov 变体, 由 encode() 逐字节算出. 回退手写兜底以保 base 兼容.
    pub fn emit_xmm_store_rsp(&mut self, pfx: u8, xmm: u8, off: usize) {
        // 映射 pfx 到 SimdWidth; pfx=0 表示无 legacy 前缀 (movups/movaps).
        let width = match pfx {
            0xF3 => crate::codegen::x86_64::opcode::SimdWidth::SS,
            0xF2 => crate::codegen::x86_64::opcode::SimdWidth::SD,
            0x00 => crate::codegen::x86_64::opcode::SimdWidth::O,
            _ => {
                // 未知前缀: 走原手写.
                Self::emit_xmm_store_rsp_fallback(self, pfx, xmm, off);
                return;
            }
        };
        use crate::codegen::shared::asm_operand::Width as W;
        let mem = crate::codegen::x86_64::opcode::MemOperand {
            disp: off as i32,
            base: Some(4), // rsp
            index: None,
            seg: None,
        };
        let width_name = match pfx {
            0xF3 => "ss",
            0xF2 => "sd",
            _ => "",
        };
        let xmm_name = format!("%xmm{}", xmm & 15);
        let gas = format!("mov{} {}, {}(%rsp)", width_name, xmm_name, off);
        let mut insn = crate::codegen::x86_64::insn::Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::SimdMov {
                width,
                opcode10: false, // 0F 11 = store
                dst: crate::codegen::x86_64::opcode::Operand::Mem(mem),
                src: crate::codegen::x86_64::opcode::Operand::Reg(xmm, W::B128),
            },
            &[],
        );
        insn.gas_text = Some(gas);
        if self.emit_insn(&insn) {
            return;
        }
        Self::emit_xmm_store_rsp_fallback(self, pfx, xmm, off);
    }

    /// 原手写兜底 (encode None 时回退).
    fn emit_xmm_store_rsp_fallback(&mut self, pfx: u8, xmm: u8, off: usize) {
        let rex = if xmm >= 8 { 0x44 } else { 0 };
        if pfx != 0 {
            if rex != 0 {
                self.emit_insn(&Insn::raw(&[pfx, rex, 0x0F, 0x11]));
            } else {
                self.emit_insn(&Insn::raw(&[pfx, 0x0F, 0x11]));
            }
        } else {
            self.emit_insn(&Insn::raw(&[rex, 0x0F, 0x11]));
        }
        if off < 128 {
            self.emit_insn(&Insn::raw(&[0x44 | ((xmm & 7) << 3), 0x24, off as u8]));
        } else {
            self.emit_insn(&Insn::raw(&[0x84 | ((xmm & 7) << 3), 0x24]));
            self.emit_insn(&Insn::raw(&(off as u32).to_le_bytes()));
        }
    }

    pub fn emit_xmm_load_rsp(&mut self, pfx: u8, xmm: u8, off: usize) {
        // 与存回侧同一条位序规则（0F 10 /r，xmm 在 reg 字段）。
        let width = match pfx {
            0xF3 => crate::codegen::x86_64::opcode::SimdWidth::SS,
            0xF2 => crate::codegen::x86_64::opcode::SimdWidth::SD,
            0x00 => crate::codegen::x86_64::opcode::SimdWidth::O,
            _ => {
                Self::emit_xmm_load_rsp_fallback(self, pfx, xmm, off);
                return;
            }
        };
        use crate::codegen::shared::asm_operand::Width as W;
        let mem = crate::codegen::x86_64::opcode::MemOperand {
            disp: off as i32,
            base: Some(4),
            index: None,
            seg: None,
        };
        let width_name = match pfx {
            0xF3 => "ss",
            0xF2 => "sd",
            _ => "",
        };
        let xmm_name = format!("%xmm{}", xmm & 15);
        let gas = format!("mov{} {}(%rsp), {}", width_name, off, xmm_name);
        let mut insn = crate::codegen::x86_64::insn::Insn::new(
            crate::codegen::x86_64::opcode::X86_64Opcode::SimdMov {
                width,
                opcode10: true, // 0F 10 = load
                dst: crate::codegen::x86_64::opcode::Operand::Reg(xmm, W::B128),
                src: crate::codegen::x86_64::opcode::Operand::Mem(mem),
            },
            &[],
        );
        insn.gas_text = Some(gas);
        if self.emit_insn(&insn) {
            return;
        }
        Self::emit_xmm_load_rsp_fallback(self, pfx, xmm, off);
    }

    fn emit_xmm_load_rsp_fallback(&mut self, pfx: u8, xmm: u8, off: usize) {
        let rex = if xmm >= 8 { 0x44 } else { 0 };
        if pfx != 0 {
            if rex != 0 {
                self.emit_insn(&Insn::raw(&[pfx, rex, 0x0F, 0x10]));
            } else {
                self.emit_insn(&Insn::raw(&[pfx, 0x0F, 0x10]));
            }
        } else {
            self.emit_insn(&Insn::raw(&[rex, 0x0F, 0x10]));
        }
        if off < 128 {
            self.emit_insn(&Insn::raw(&[0x44 | ((xmm & 7) << 3), 0x24, off as u8]));
        } else {
            self.emit_insn(&Insn::raw(&[0x84 | ((xmm & 7) << 3), 0x24]));
            self.emit_insn(&Insn::raw(&(off as u32).to_le_bytes()));
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

    /// [`Self::store_xmm128_stack`] 的反向动作: `movups xmm, [r13 + off]`,
    /// 一次读回帧槽里的完整 128 位。16 字节 `[Sse, SseUp]` 聚合的返回端必须
    /// 用它, 而不是只读低 8 字节的 `movsd` —— `SseUp` 是**同一** xmm 寄存器高
    /// 64 位的延续, 不是第二个寄存器, 也不是第二个内存操作数。
    pub fn load_xmm128_stack(&mut self, xmm: u8, off: i64) {
        self.emit_xmm_r13_disp(0x00, 0x10, xmm, off);
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
        self.emit_insn(&Insn::raw(&[0x66, rex, 0x0F, 0x6E, 0xC0 | ((xmm & 7) << 3) | (gp & 7)]));
    }

    pub fn flip_sign_xmm(&mut self, xmm: u8, ty: &Type) {
        let mask: u64 = if matches!(ty, Type::Float) {
            0x8000_0000
        } else {
            0x8000_0000_0000_0000
        };
        self.emit_insn(&Insn::raw(&[0x48, 0xB8]));
        self.emit_insn(&Insn::raw(&mask.to_le_bytes())); // movabs rax, mask
        self.emit_insn(&Insn::raw(&[0x66, 0x48, 0x0F, 0x6E, 0xC8])); // movq xmm1, rax
        if xmm == 1 {
            // xorps/xorpd xmm0, xmm1 → 目标为 xmm1 时借用 xmm2
            self.emit_insn(&Insn::raw(&[0x66, 0x48, 0x0F, 0x6E, 0xD0])); // movq xmm2, rax
            if matches!(ty, Type::Float) {
                self.emit_insn(&Insn::raw(&[0x0F, 0x57, 0xCA])); // xorps xmm1, xmm2
            } else {
                self.emit_insn(&Insn::raw(&[0x66, 0x0F, 0x57, 0xCA])); // xorpd xmm1, xmm2
            }
        } else {
            if matches!(ty, Type::Float) {
                self.emit_insn(&Insn::raw(&[0x0F, 0x57, 0xC1])); // xorps xmm0, xmm1
            } else {
                self.emit_insn(&Insn::raw(&[0x66, 0x0F, 0x57, 0xC1])); // xorpd xmm0, xmm1
            }
        }
    }

    pub fn emit_scalar_to_xmm0_if_needed(&mut self, expr: &Expr, elem_ty: &Type) {
        let src = expr.ty();
        self.emit_scalar_to_xmm0_if_needed_ty(&src, elem_ty);
    }

    /// `emit_scalar_to_xmm0_if_needed` 的类型版（单一真源）：只依赖源类型，
    /// 供 `emit_store_scalar_float_at` 一类已经知道 `src_ty` 而不是 `&Expr` 的出口复用。
    pub fn emit_scalar_to_xmm0_if_needed_ty(&mut self, src_ty: &Type, elem_ty: &Type) {
        let expr_is_float = matches!(
            src_ty,
            Type::Float | Type::Double | Type::LongDouble | Type::Complex(_) | Type::Imaginary(_)
        );
        if !matches!(elem_ty, Type::Float | Type::Double | Type::LongDouble) {
            return;
        }
        if expr_is_float {
            // 浮点表达式结果已在 xmm0
            // 但如果表达式类型与目标类型不匹配，需要转换
            // 例如: double 表达式赋值给 float 变量，需要 cvtsd2ss
            if matches!(elem_ty, Type::Float) && matches!(src_ty, Type::Double) {
                // double -> float: cvtsd2ss xmm0, xmm0
                self.emit_insn(&Insn::raw_with_gas(&[0xF2, 0x0F, 0x5A, 0xC0], "cvtsd2ss %xmm0, %xmm0"));
            } else if matches!(elem_ty, Type::Double) && matches!(src_ty, Type::Float) {
                // float -> double: cvtss2sd xmm0, xmm0
                self.emit_insn(&Insn::raw_with_gas(&[0xF3, 0x0F, 0x5A, 0xC0], "cvtss2sd %xmm0, %xmm0"));
            }
            return;
        }
        // 整数→浮点转换: 根据整数类型宽度选择 32 位或 64 位转换
        let expr_ty = src_ty.clone();
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
            let dst = 0u8; // 整数结果约定在 rax
            // 自反 `mov %eax,%eax`：dst 恒为 0 ⇒ 没有任何字段需要扩展，裸 0x40 只会
            // 多占一字节并被 objdump 标成 `rex mov %eax,%eax`。
            debug_assert!(dst < 8);
            self.emit_insn(&Insn::raw(&[0x89, 0xC0 | ((dst & 7) << 3) | (dst & 7)]));
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
                self.emit_insn(&Insn::raw_with_gas(&[0xF3, 0x48, 0x0F, 0x2A, 0xC0], "cvtsi2ss %rax, %xmm0"));
            } else {
                self.emit_insn(&Insn::raw_with_gas(&[0xF3, 0x0F, 0x2A, 0xC0], "cvtsi2ss %eax, %xmm0"));
            }
        } else {
            if is_64bit || is_unsigned_small {
                // cvtsi2sdq xmm0, rax (REX.W + F2 0F 2A C0)
                self.emit_insn(&Insn::raw_with_gas(&[0xF2, 0x48, 0x0F, 0x2A, 0xC0], "cvtsi2sd %rax, %xmm0"));
            } else {
                self.emit_insn(&Insn::raw_with_gas(&[0xF2, 0x0F, 0x2A, 0xC0], "cvtsi2sd %eax, %xmm0"));
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
                self.emit_insn(&Insn::raw_with_gas(&[0x90], "nop"));
            }
        }
    }

    /// Emit a single NOP.
    pub fn nop(&mut self) {
        self.emit_insn(&Insn::raw_with_gas(&[0x90], "nop"));
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
                    self.emit_insn(&Insn::raw(&[r, 0x0F, 0xBE, 0xC0 | ((dst & 7) << 3) | (src & 7)]));
                } else {
                    self.emit_insn(&Insn::raw(&[0x48, 0x0F, 0xBE, 0xC0 | ((dst & 7) << 3) | (src & 7)]));
                }
            },
            16 => {
                let rex = encode_rex_full(true, dst, 0, src);
                if let Some(r) = rex {
                    self.emit_insn(&Insn::raw(&[r, 0x0F, 0xBF, 0xC0 | ((dst & 7) << 3) | (src & 7)]));
                } else {
                    self.emit_insn(&Insn::raw(&[0x48, 0x0F, 0xBF, 0xC0 | ((dst & 7) << 3) | (src & 7)]));
                }
            },
            32 => {
                let rex = encode_rex_full(true, dst, 0, src);
                use crate::codegen::x86_64::emit::gas::REG32 as GAS_REG32;
                use crate::codegen::x86_64::emit::gas::REG64 as GAS_REG64;
                let gas = format!("movsxd {}, {}", GAS_REG32[(src & 15) as usize], GAS_REG64[(dst & 15) as usize]);
                if let Some(r) = rex {
                    self.emit_insn(&Insn::raw_with_gas(&[r, 0x63, 0xC0 | ((dst & 7) << 3) | (src & 7)], &gas));
                } else {
                    self.emit_insn(&Insn::raw_with_gas(&[0x48, 0x63, 0xC0 | ((dst & 7) << 3) | (src & 7)], &gas));
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
        self.frame_layout.vec_temp(slot as i64, vec_size)
    }

    /// 绑定表是本函数物理寄存器所有权的真源之一。一个名字绑到 callee-saved 池里
    /// 的寄存器之后, 那个寄存器必须同时登记两件事, 否则就是「同一个寄存器被两个人
    /// 拥有」, 而两边都会毫无提示地覆盖对方:
    ///   * `used_mask` 对应位 —— 序言的四条保存指令先无条件发射, 函数体生成
    ///     完之后由 `patch_callee_saves` 把「本函数没用到」的那些改成 nop。不置位就
    ///     等于本函数可以毁掉调用者存放在该寄存器里的值: 实测调用方把二元式左操作数
    ///     溅在 %rbx 上并跨一次调用保活, 被调方绑了 %rbx 却没保存, 结果 1500 而 gcc
    ///     是 500。
    ///   * `allocated_mask` 对应位 —— 临时值池 (`alloc_callee_saved`) 不能再把这个
    ///     寄存器发出去, 否则表达式溅出直接盖掉变量的家: 实测 `register long v
    ///     asm("rbx") = 7` 在一次 `(q + r) * g(s)` 之后读出 25。
    ///
    /// 绑到 caller-saved 或固定用途寄存器 (rax/rcx/rdx/rsi/rdi/r8/r9/rsp/rbp/r13)
    /// 不在本函数的保护能力之内: 表达式求值每时每刻都在用它们, GCC 为此建的是完整的
    /// 干涉模型而不是一个位图。这条边界如实记在 docs/ssa-pass-upgrade-plan.md。
    ///
    /// Phase 1 起本方法只是 `RegBank::claim` 的薄封装：占用真相只有一份，
    /// 写路径也都只从 RegBank 走（见 `abi/regbank.rs` 头注释的 H4）。
    pub fn claim_bound_register(&mut self, reg: u8) {
        self.reg_bank.claim(reg);
    }

    pub fn alloc_callee_saved(&mut self) -> Option<u8> {
        self.reg_bank.alloc_temp()
    }

    pub fn free_callee_saved(&mut self, reg: u8) {
        // 变量的家不是临时值: claim 之后 `alloc_callee_saved` 就不会再发这个寄存器,
        // 所以正常路径到不了这里。留这道门是让「把绑定中的寄存器标回空闲」在未来
        // 任何新增的释放点上都直接失效, 而不是又退回本文件刚修掉的缺陷。判据现在
        // 住在 `RegBank::release` 里（被钉住的号不还），因此新增释放点绕不过去。
        self.reg_bank.release(reg);
    }

    // ---- 以下为自 monolith（28a9063）恢复的底层指令编码原语 ----

    pub(crate) fn emit_add_r10(&mut self, inner_sz: i64) {
        // add rax/eax, r10: reg=r10 → REX.R+W (0x4C)
        let rex = if inner_sz == 8 { 0x4C } else { 0x44 };
        self.emit_insn(&Insn::raw(&[rex, 0x01, 0xD0]));
    }
    pub(crate) fn emit_add_r11(&mut self, inner_sz: i64) {
        let rex = if inner_sz == 8 { 0x4C } else { 0x44 };
        self.emit_insn(&Insn::raw(&[rex, 0x01, 0xD8]));
    }
    pub(crate) fn emit_add_rm(&mut self, inner_sz: i64, off: i64) {
        let rex = if inner_sz == 8 { 0x48 } else { 0x00 };
        if rex != 0 {
            self.emit_insn(&Insn::raw(&[rex]));
        }
        self.emit_insn(&Insn::raw(&[0x03, 0x44, 0x24, off as u8])); // add rax/eax, [rsp+off]
    }
    pub(crate) fn emit_align_padding(&mut self, alignment: u32) {
        if alignment <= 1 {
            return;
        }
        let pos = self.code.len() as u64;
        let aligned = (pos + alignment as u64 - 1) & !(alignment as u64 - 1);
        let pad = (aligned - pos) as usize;
        for _ in 0..pad {
            self.emit_insn(&Insn::raw(&[0x90])); // NOP
        }
    }
    pub(crate) fn emit_and_rdx_rsi(&mut self, sz: usize) {
        match sz {
            1 => self.emit_insn(&Insn::raw(&[0x40, 0x20, 0xF2])), // and dl, sil
            2 => self.emit_insn(&Insn::raw(&[0x66, 0x21, 0xF2])), // and dx, si
            4 => self.emit_insn(&Insn::raw(&[0x21, 0xF2])),       // and edx, esi
            _ => self.emit_insn(&Insn::raw(&[0x48, 0x21, 0xF2])), // and rdx, rsi
        };
    }
    pub(crate) fn emit_cas_loop<F: Fn(&mut Self, usize)>(&mut self, sz: usize, compute_new: F) {
        match sz {
            1 => {
                // 8-bit: al/al, sil/dl/ah 8 位子寄存器参与
                self.emit_insn(&Insn::raw(&[0x0F, 0xB6, 0x07])); // movzx eax, byte [rdi]
                let l_retry = self.fresh_label();
                self.label(&l_retry);
                self.emit_insn(&Insn::raw(&[0x89, 0xC1])); // mov ecx, eax
                self.emit_insn(&Insn::raw(&[0x89, 0xC2])); // mov edx, eax
                compute_new(self, sz);
                self.emit_lock_cmpxchg_rdi_rdx(sz);
                let l_done = self.fresh_label();
                self.jmp_rel32(&l_done, JumpKind::Je);
                self.jmp_rel32(&l_retry, JumpKind::Jmp);
                self.label(&l_done);
            },
            2 => {
                // 16-bit: ax/si/dx 子寄存器
                self.emit_insn(&Insn::raw(&[0x0F, 0xB7, 0x07])); // movzx eax, word [rdi]
                let l_retry = self.fresh_label();
                self.label(&l_retry);
                self.emit_insn(&Insn::raw(&[0x89, 0xC1])); // mov ecx, eax
                self.emit_insn(&Insn::raw(&[0x89, 0xC2])); // mov edx, eax
                compute_new(self, sz);
                self.emit_lock_cmpxchg_rdi_rdx(sz);
                let l_done = self.fresh_label();
                self.jmp_rel32(&l_done, JumpKind::Je);
                self.jmp_rel32(&l_retry, JumpKind::Jmp);
                self.label(&l_done);
            },
            4 => {
                // 32-bit (zero-extends to 64-bit)
                self.emit_insn(&Insn::raw(&[0x8B, 0x07])); // mov eax, [rdi]
                let l_retry = self.fresh_label();
                self.label(&l_retry);
                self.emit_insn(&Insn::raw(&[0x89, 0xC1])); // mov ecx, eax
                self.emit_insn(&Insn::raw(&[0x89, 0xC2])); // mov edx, eax
                compute_new(self, sz);
                self.emit_lock_cmpxchg_rdi_rdx(sz);
                let l_done = self.fresh_label();
                self.jmp_rel32(&l_done, JumpKind::Je);
                self.jmp_rel32(&l_retry, JumpKind::Jmp);
                self.label(&l_done);
            },
            _ => {
                // 64-bit
                self.emit_insn(&Insn::raw(&[0x48, 0x8B, 0x07])); // mov rax, [rdi]
                let l_retry = self.fresh_label();
                self.label(&l_retry);
                self.emit_insn(&Insn::raw(&[0x48, 0x89, 0xC1])); // mov rcx, rax
                self.emit_insn(&Insn::raw(&[0x48, 0x89, 0xC2])); // mov rdx, rax
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
            self.emit_insn(&Insn::raw(&[0x8B, 0x44, 0x24, off1 as u8])); // mov eax, [rsp+off1]
            self.emit_insn(&Insn::raw(&[0x8B, 0x4C, 0x24, off2 as u8])); // mov ecx, [rsp+off2]
        } else {
            let ld = match (inner_sz, signed) {
                (2, true) => 0xBF,
                (2, false) => 0xB7,
                (_, true) => 0xBE,
                (_, false) => 0xB6,
            };
            self.emit_insn(&Insn::raw(&[0x0F, ld, 0x44, 0x24, off1 as u8])); // movsx/movzx eax
            self.emit_insn(&Insn::raw(&[0x0F, ld, 0x4C, 0x24, off2 as u8])); // movsx/movzx ecx
        }
    }
    pub(crate) fn emit_cqo(&mut self, inner_sz: i64) {
        if inner_sz == 8 {
            self.emit_insn(&Insn::raw(&[0x48]));
        }
        self.emit_insn(&Insn::raw(&[0x99]));
    }
    pub(crate) fn emit_div_rm(&mut self, inner_sz: i64, is_signed: bool, rm32: u8) {
        let op = if is_signed { rm32 + 8 } else { rm32 };
        if inner_sz == 8 {
            self.emit_insn(&Insn::raw(&[0x48, 0xF7, op]));
        } else {
            self.emit_insn(&Insn::raw(&[0xF7, op]));
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
        self.emit_insn(&Insn::raw(&[
            0x62,
            (r_inv << 7) | (1u8 << 6) | (b_inv << 5) | (r2 << 4) | (map & 0x0F),
            (w << 7) | (v_inv << 3) | (1u8 << 2) | (pp & 0x03),
            (l << 5) | (v4 << 3) | (mask & 0x07),
        ]));
    }
    pub(crate) fn emit_imul_r10(&mut self, inner_sz: i64) {
        // imul rax/eax, r10: rm=r10 → REX.B (0x41/0x49)
        let rex = if inner_sz == 8 { 0x49 } else { 0x41 };
        self.emit_insn(&Insn::raw(&[rex, 0x0F, 0xAF, 0xC2]));
    }
    pub(crate) fn emit_imul_r11(&mut self, inner_sz: i64) {
        let rex = if inner_sz == 8 { 0x49 } else { 0x41 };
        self.emit_insn(&Insn::raw(&[rex, 0x0F, 0xAF, 0xC3]));
    }
    pub(crate) fn emit_imul_rm(&mut self, inner_sz: i64, off: i64) {
        let rex = if inner_sz == 8 { 0x48 } else { 0x00 };
        if rex != 0 {
            self.emit_insn(&Insn::raw(&[rex]));
        }
        self.emit_insn(&Insn::raw(&[0x0F, 0xAF, 0x44, 0x24, off as u8])); // imul rax/eax, [rsp+off]
    }
    pub(crate) fn emit_load_mem(&mut self, size: usize) {
        match size {
            1 => self.emit_insn(&Insn::raw(&[0x48, 0x0f, 0xb6, 0x00])), // movzbl (%rax), %rax
            2 => self.emit_insn(&Insn::raw(&[0x48, 0x0f, 0xb7, 0x00])), // movzwl (%rax), %rax
            4 => self.emit_insn(&Insn::raw(&[0x8b, 0x00])), // mov (%rax), %eax (零扩展到64位)
            _ => self.emit_insn(&Insn::raw(&[0x48, 0x8b, 0x00])),       // mov (%rax), %rax
        };
    }
    fn emit_lock_cmpxchg_rdi_rdx(&mut self, sz: usize) {
        match sz {
            1 => self.emit_insn(&Insn::raw(&[0xF0, 0x40, 0x0F, 0xB0, 0x17])), // lock cmpxchg [rdi], dl
            2 => self.emit_insn(&Insn::raw(&[0x66, 0xF0, 0x0F, 0xB1, 0x17])), // lock cmpxchg [rdi], dx
            4 => self.emit_insn(&Insn::raw(&[0xF0, 0x0F, 0xB1, 0x17])),       // lock cmpxchg [rdi], edx
            _ => self.emit_insn(&Insn::raw(&[0xF0, 0x48, 0x0F, 0xB1, 0x17])), // lock cmpxchg [rdi], rdx
        };
    }
    pub(crate) fn emit_lock_cmpxchg_rdi_rsi(&mut self, sz: usize) {
        match sz {
            1 => self.emit_insn(&Insn::raw(&[0xF0, 0x40, 0x0F, 0xB0, 0x37])), // lock cmpxchg [rdi], sil
            2 => self.emit_insn(&Insn::raw(&[0x66, 0xF0, 0x0F, 0xB1, 0x37])), // lock cmpxchg [rdi], si
            4 => self.emit_insn(&Insn::raw(&[0xF0, 0x0F, 0xB1, 0x37])),       // lock cmpxchg [rdi], esi
            _ => self.emit_insn(&Insn::raw(&[0xF0, 0x48, 0x0F, 0xB1, 0x37])), // lock cmpxchg [rdi], rsi
        };
    }
    pub(crate) fn emit_lock_xchg_rdi_rsi(&mut self, sz: usize) {
        match sz {
            1 => self.emit_insn(&Insn::raw(&[0xF0, 0x40, 0x86, 0x37])), // lock xchg [rdi], sil
            2 => self.emit_insn(&Insn::raw(&[0x66, 0xF0, 0x87, 0x37])), // lock xchg [rdi], si
            4 => self.emit_insn(&Insn::raw(&[0xF0, 0x87, 0x37])),       // lock xchg [rdi], esi
            _ => self.emit_insn(&Insn::raw(&[0xF0, 0x48, 0x87, 0x37])), // lock xchg [rdi], rsi
        };
        match sz {
            1 => { self.emit_insn(&Insn::raw(&[0x40, 0x0F, 0xB6, 0xF6])); } // movzbl sil, esi
            2 => { self.emit_insn(&Insn::raw(&[0x0F, 0xB7, 0xF6])); }       // movzwl si, esi
            _ => {},
        };
    }
    pub(crate) fn emit_movd_gp_to_xmm(&mut self, xmm: u8, gp: u8) {
        let mut rex = 0x40;
        if xmm >= 8 {
            rex |= 0x04;
        }
        if gp >= 8 {
            rex |= 0x01;
        }
        self.emit_insn(&Insn::raw(&[0x66]));
        if rex != 0x40 {
            self.emit_insn(&Insn::raw(&[rex]));
        }
        self.emit_insn(&Insn::raw(&[0x0F, 0x6E, 0xC0 | ((xmm & 7) << 3) | (gp & 7)]));
    }
    pub(crate) fn emit_movdqu_r13_to_xmm(&mut self, xmm_reg: u8, off: i64) {
        if Self::fits_disp32(off) {
            let rex = 0x41 | if xmm_reg >= 8 { 0x04 } else { 0 };
            self.emit_insn(&Insn::raw(&[0xF3, rex, 0x0F, 0x6F]));
            self.emit_r13_modrm_disp(xmm_reg, off);
        } else {
            self.emit_lea_r13(11, off);
            let rex = 0x41 | if xmm_reg >= 8 { 0x04 } else { 0 } | 0x01;
            self.emit_insn(&Insn::raw(&[0xF3, rex, 0x0F, 0x6F]));
            self.emit_reg_indirect_modrm(xmm_reg, 11);
        }
    }
    pub(crate) fn emit_movdqu_xmm_to_r13(&mut self, xmm_reg: u8, off: i64) {
        if Self::fits_disp32(off) {
            let rex = 0x41 | if xmm_reg >= 8 { 0x04 } else { 0 };
            self.emit_insn(&Insn::raw(&[0xF3, rex, 0x0F, 0x7F]));
            self.emit_r13_modrm_disp(xmm_reg, off);
        } else {
            self.emit_lea_r13(11, off);
            let rex = 0x41 | if xmm_reg >= 8 { 0x04 } else { 0 } | 0x01;
            self.emit_insn(&Insn::raw(&[0xF3, rex, 0x0F, 0x7F]));
            self.emit_reg_indirect_modrm(xmm_reg, 11);
        }
    }
    pub(crate) fn emit_mov_from_mem(&mut self, sz: usize, dst: u8) {
        match sz {
            1 => self.emit_insn(&Insn::raw(&[0x0F, 0xB6, 0x07])), // movzx eax, byte [rdi]
            2 => self.emit_insn(&Insn::raw(&[0x0F, 0xB7, 0x07])), // movzx eax, word [rdi]
            4 => self.emit_insn(&Insn::raw(&[0x8B, 0x07])), // mov eax, [rdi] (zero-extends)
            _ => self.emit_insn(&Insn::raw(&[0x48, 0x8B, 0x07])), // mov rax, [rdi]
        };
        if dst != 0 {
            self.mov(dst, 0);
        }
    }
    pub(crate) fn emit_mov_rcx_r10(&mut self, inner_sz: i64) {
        let rex = if inner_sz == 8 { 0x4C } else { 0x44 };
        self.emit_insn(&Insn::raw(&[rex, 0x89, 0xD1]));
    }
    pub(crate) fn emit_mov_rcx_r11(&mut self, inner_sz: i64) {
        let rex = if inner_sz == 8 { 0x4C } else { 0x44 };
        self.emit_insn(&Insn::raw(&[rex, 0x89, 0xD9]));
    }
    pub(crate) fn emit_mov_to_mem(&mut self, sz: usize) {
        match sz {
            1 => self.emit_insn(&Insn::raw(&[0x40, 0x88, 0x37])), // mov [rdi], sil
            2 => self.emit_insn(&Insn::raw(&[0x66, 0x89, 0x37])), // mov [rdi], si
            4 => self.emit_insn(&Insn::raw(&[0x89, 0x37])),       // mov [rdi], esi
            _ => self.emit_insn(&Insn::raw(&[0x48, 0x89, 0x37])), // mov [rdi], rsi
        };
    }
    pub(crate) fn emit_neg(&mut self, sz: usize) {
        // 步骤 2c: 收敛到 UnaryRm{Neg}. 原手写针对 reg=6 (rsi):
        //   1: 40 F6 DE   2: 66 F7 DE   4: F7 DE   8: 48 F7 DE.
        let width = match sz {
            1 => Width::B8,
            2 => Width::B16,
            4 => Width::B32,
            _ => Width::B64,
        };
        let insn = Insn::new(
            X86_64Opcode::UnaryRm {
                op: crate::codegen::x86_64::opcode::UnaryOp::Neg,
                width,
                dst: OpcodeOperand::Reg(6, width),
            },
            &[],
        );
        if self.emit_insn(&insn) {
            return;
        }
        match sz {
            1 => self.emit_insn(&Insn::raw(&[0x40, 0xF6, 0xDE])), // neg sil
            2 => self.emit_insn(&Insn::raw(&[0x66, 0xF7, 0xDE])), // neg si
            4 => self.emit_insn(&Insn::raw(&[0xF7, 0xDE])),       // neg esi
            _ => self.emit_insn(&Insn::raw(&[0x48, 0xF7, 0xDE])), // neg rsi
        };
    }
    pub(crate) fn emit_or_rdx_rsi(&mut self, sz: usize) {
        match sz {
            1 => self.emit_insn(&Insn::raw(&[0x40, 0x08, 0xF2])), // or dl, sil
            2 => self.emit_insn(&Insn::raw(&[0x66, 0x09, 0xF2])), // or dx, si
            4 => self.emit_insn(&Insn::raw(&[0x09, 0xF2])),       // or edx, esi
            _ => self.emit_insn(&Insn::raw(&[0x48, 0x09, 0xF2])), // or rdx, rsi
        };
    }
    pub(crate) fn emit_store_imm32_mem(&mut self, base: u8, disp: i32, imm: i32) {
        let mut rex: u8 = 0x40;
        if base >= 8 {
            rex |= 0x01; // REX.B
        }
        let rm = base & 7;
        if disp.abs() < 128 {
            self.emit_insn(&Insn::raw(&[rex, 0xC7, 0x40 | rm]));
            self.emit_insn(&Insn::raw(&(disp as i8).to_le_bytes()));
        } else {
            self.emit_insn(&Insn::raw(&[rex, 0xC7, 0x80 | rm]));
            self.emit_insn(&Insn::raw(&disp.to_le_bytes()));
        }
        self.emit_insn(&Insn::raw(&imm.to_le_bytes()));
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
            self.emit_insn(&Insn::raw(&[rex, 0x89, ((src & 7) << 3) | rm]));
        } else if disp.abs() < 128 {
            self.emit_insn(&Insn::raw(&[rex, 0x89, 0x40 | ((src & 7) << 3) | rm]));
            self.emit_insn(&Insn::raw(&(disp as i8).to_le_bytes()));
        } else {
            self.emit_insn(&Insn::raw(&[rex, 0x89, 0x80 | ((src & 7) << 3) | rm]));
            self.emit_insn(&Insn::raw(&disp.to_le_bytes()));
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
            self.emit_insn(&Insn::raw(&[rex]));
        }
        self.emit_insn(&Insn::raw(&[0x29, 0xC8])); // sub rax/eax, rcx
    }
    pub(crate) fn emit_sub_rcx_rax(&mut self, inner_sz: i64) {
        let rex = if inner_sz == 8 { 0x48 } else { 0x00 };
        if rex != 0 {
            self.emit_insn(&Insn::raw(&[rex]));
        }
        self.emit_insn(&Insn::raw(&[0x29, 0xC1])); // sub rcx/eax? rcx, rax
    }
    pub(crate) fn emit_vex3_prefix(&mut self, r: u8, v: u8, l: u8, pp: u8) {
        let r_inv = if r >= 8 { 0u8 } else { 1u8 };
        let v_inv = (!v) & 0x0F;
        self.emit_insn(&Insn::raw(&[0xC5, (r_inv << 7) | (v_inv << 3) | (l << 2) | pp]));
    }
    pub(crate) fn emit_vex4_prefix(&mut self, r: u8, b: u8, map: u8, w: u8, v: u8, l: u8, pp: u8) {
        let r_inv = if r >= 8 { 0u8 } else { 1u8 };
        let b_inv = if b >= 8 { 0u8 } else { 1u8 };
        let v_inv = (!v) & 0x0F;
        self.emit_insn(&Insn::raw(&[
            0xC4,
            (r_inv << 7) | (1u8 << 6) | (b_inv << 5) | (map & 0x1F),
            (w << 7) | (v_inv << 3) | (l << 2) | (pp & 0x03),
        ]));
    }
    pub(crate) fn emit_xor_edx(&mut self, inner_sz: i64) {
        if inner_sz == 8 {
            self.emit_insn(&Insn::raw(&[0x48, 0x31, 0xD2]));
        } else {
            self.emit_insn(&Insn::raw(&[0x31, 0xD2]));
        }
    }
    pub(crate) fn emit_xor_rdx_rsi(&mut self, sz: usize) {
        match sz {
            1 => self.emit_insn(&Insn::raw(&[0x40, 0x30, 0xF2])), // xor dl, sil
            2 => self.emit_insn(&Insn::raw(&[0x66, 0x31, 0xF2])), // xor dx, si
            4 => self.emit_insn(&Insn::raw(&[0x31, 0xF2])),       // xor edx, esi
            _ => self.emit_insn(&Insn::raw(&[0x48, 0x31, 0xF2])), // xor rdx, rsi
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Abi, Arch, Os, TargetTriple};
    use crate::diagnostics::Severity;
    use crate::target::Target;

    fn x86() -> Codegen {
        Codegen::new(Target::resolve(&TargetTriple {
            arch: Arch::X86_64,
            vendor: "unknown".into(),
            os: Os::Linux,
            abi: Abi::Gnu,
        }))
    }

    fn jump_to(target: &str) -> PendingJump {
        PendingJump {
            at: 0,
            target: target.to_string(),
            kind: JumpKind::Jmp,
            disp_offset: 1,
            next_ip: 6,
            is_push_imm: false,
        }
    }

    fn error_codes(cg: &Codegen) -> Vec<&'static str> {
        cg.diagnostics
            .iter()
            .filter(|d| d.sev == Severity::Error)
            .map(|d| d.code)
            .collect()
    }

    /// `.L` 谓词只能盖住后端自造的名字。把 `printf` / `.text` 误判成内部
    /// 标号会让 `-c` 下每一条外部跳转都变成假报的后端 bug, 比静默误编译
    /// 更糟糕 (直接卡住所有用户)。
    #[test]
    fn internal_label_predicate_covers_only_the_dot_l_namespace() {
        assert!(Codegen::is_internal_label(".L0"));
        assert!(Codegen::is_internal_label(".L42"));
        assert!(Codegen::is_internal_label(".Ll_main_retry"));
        assert!(Codegen::is_internal_label(".Lasm_local_3_1_0"));
        assert!(!Codegen::is_internal_label("printf"));
        assert!(!Codegen::is_internal_label(".text"));
        assert!(!Codegen::is_internal_label(".rodata"));
        assert!(!Codegen::is_internal_label("L0"));
    }

    /// `fresh_label` 仍是 `.L<n>` 的唯一号源: 并行分区只改 `next_label` 的起点,
    /// 不改命名格式 (否则 `.L` 谓词与 symtab 规则会两边漂移)。
    #[test]
    fn fresh_label_is_the_single_number_source() {
        let mut cg = x86();
        assert_eq!(cg.next_label, 0);
        assert_eq!(cg.fresh_label(), ".L0");
        assert_eq!(cg.fresh_label(), ".L1");
        assert_eq!(cg.next_label, 2);
    }

    /// 缺陷 B 要求 3: 指向未定义内部标号的 pending 必须当场报错。
    /// `jmp_rel32` 留下的 `0,0,0,0` 占位不回填就是「跳到下一条」。
    #[test]
    fn undefined_internal_label_in_pending_is_a_hard_error() {
        let mut cg = x86();
        cg.pending.push(jump_to(".L7"));
        cg.pending.push(jump_to(".L7")); // 同一标号两处引用
        cg.check_pending_labels_closed("f");
        assert_eq!(error_codes(&cg), vec!["E1102"], "诊断必须只出一条且不得去重失败");
        let d = &cg.diagnostics[0];
        assert!(d.msg.contains(".L7") && d.msg.contains("'f'"), "msg = {}", d.msg);
    }

    /// 已定义的内部标号与合法外部符号都是正常局面, 不得报任何错。
    /// 这一条是「不得用误报把 fail-fast 变成 fail-everything」的反向守卫。
    #[test]
    fn defined_and_external_targets_stay_silent() {
        let mut cg = x86();
        cg.label(".L7");
        cg.pending.push(jump_to(".L7"));
        cg.pending.push(jump_to("other_func"));
        cg.pending.push(jump_to("asm_defined_tag"));
        cg.check_pending_labels_closed("f");
        assert!(
            cg.diagnostics.is_empty(),
            "unexpected: {:?}",
            error_codes(&cg)
        );
    }
}
