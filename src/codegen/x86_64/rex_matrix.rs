// src/codegen/x86_64/rex_matrix.rs - REX 位序穷举门禁（#[cfg(test)]）
//
// 为什么必须有这一条：REX 的 R/B/X 三位各自绑定**不同的字段**（R→ModRM.reg、
// B→ModRM.r/m 与 opcode 低 3 位、X→SIB.index），而低 8 号里的 %spl/%bpl/%sil/%dil
// 只要「存在任意 REX」就合法。把这两件事混为一谈的编码点，会把字段搬到 +8 的高 8 个
// 寄存器（`mov %sil,(%rbx)` → `mov %sil,(%r11)`，直接写飞），或者干脆不发 REX
// （`mov $1,%sil` → `mov $1,%dh`，值静默落进 %rsi 的 8..15 位）。
//
// 这一类缺陷以前只能靠人撞：一个段错误换一个点位，P12 把热变量灌进 callee-saved 之后
// `dst>=8` 分支从死变活，修的速度追不上被点亮的速度。本文件把它变成机器判据：
// 对每条位序敏感的编码函数跑 16×16（或 16×宽度）全组合，把字节交给**自家解码器**读回来，
// 断言读回的寄存器名恰好是意图的那几个。
//
// 判据为什么是「名字」而不是「字节」：字节判据要再实现一遍编码器（同义反复，抓不到东西）；
// 解码器是独立写的位解析器，它唯一的盲区是「看不见有没有 REX」，而那一块已由
// `render_one::Rex.present` 与 `REG8_LEGACY` 补齐 —— 所以解码器现在两类都能抓：
//   * 搬错字段 → 意图名缺席（`%sil` 读成 `%r14b`）；
//   * 漏发 REX → 意图名读成 `%ah/%ch/%dh/%bh`，由下面的 legacy 反查直接判红。

use super::asm::operand::AsmOperand;
use super::codegen::{Codegen, GlobalVar};
use crate::ast::Type;
use crate::cli::{Abi, Arch, Os, TargetTriple};
use crate::codegen::x86_64::render_one::disassemble_one_at;
use crate::diagnostics::Severity;
use crate::target::Target;
use std::collections::{BTreeMap, HashMap};

/// 意图名表。后端在解析阶段就把 %ah/%ch/%dh/%bh 折叠到 %al/%cl/%dl/%bl
/// （见 asm::constraint 的寄存器名表），所以 8 位上下文里字段 4..7 **只可能**是
/// %spl/%bpl/%sil/%dil —— 任何解码结果里出现的 %ah..%bh 都等于「该发 REX 却没发」。
const R8: [&str; 16] = [
    "%al", "%cl", "%dl", "%bl", "%spl", "%bpl", "%sil", "%dil", "%r8b", "%r9b", "%r10b", "%r11b",
    "%r12b", "%r13b", "%r14b", "%r15b",
];
const R16: [&str; 16] = [
    "%ax", "%cx", "%dx", "%bx", "%sp", "%bp", "%si", "%di", "%r8w", "%r9w", "%r10w", "%r11w",
    "%r12w", "%r13w", "%r14w", "%r15w",
];
const R32: [&str; 16] = [
    "%eax", "%ecx", "%edx", "%ebx", "%esp", "%ebp", "%esi", "%edi", "%r8d", "%r9d", "%r10d",
    "%r11d", "%r12d", "%r13d", "%r14d", "%r15d",
];
const R64: [&str; 16] = [
    "%rax", "%rcx", "%rdx", "%rbx", "%rsp", "%rbp", "%rsi", "%rdi", "%r8", "%r9", "%r10", "%r11",
    "%r12", "%r13", "%r14", "%r15",
];

fn name(idx: u8, bits: usize) -> &'static str {
    let i = (idx & 15) as usize;
    match bits {
        8 => R8[i],
        16 => R16[i],
        32 => R32[i],
        _ => R64[i],
    }
}

fn cg() -> Codegen {
    Codegen::new(Target::resolve(&TargetTriple {
        arch: Arch::X86_64,
        vendor: "unknown".into(),
        os: Os::Linux,
        abi: Abi::Gnu,
    }))
}

fn decode(code: &[u8]) -> String {
    let mut out = String::new();
    let mut pos = 0;
    let labels: BTreeMap<u64, Vec<&str>> = BTreeMap::new();
    let calls: HashMap<u64, String> = HashMap::new();
    let data: HashMap<u64, String> = HashMap::new();
    while pos < code.len() {
        match disassemble_one_at(code, pos, 0, &calls, &data, &labels) {
            Some((len, text)) if len > 0 => {
                out.push_str(&text);
                out.push('\n');
                pos += len;
            },
            _ => break,
        }
    }
    out
}

/// 断言一段字节的解码结果恰好含有意图操作数、且不含任何被搬错/漏发 REX 的痕迹。
fn check(cg: &Codegen, case: &str, expected: &[&str]) {
    let line = first_line(cg, case);
    for e in expected {
        assert!(
            line.contains(e),
            "{case}: 期望读回 {e}，实际是 `{line}`（字节 {:?}）—— 字段被搬走了",
            cg.code
        );
    }
    assert_no_legacy8(&line, &cg.code, case, expected);
}

/// 同上，但还要按**位置**对：AT&T 是 `助记符 源, 目的`，只看「名字在不在」
/// 永远拓不了方向颠倒（`mov %rcx,%r9` 与 `mov %r9,%rcx` 含有一样的两个名字，
/// 但前者把 %r9 砸了而 cl 仍未设置）。这是实测踩过的一条：偏移位序错到
/// 足以交换两个操作数。
fn check_ordered(cg: &Codegen, case: &str, expected: &[&str]) {
    let line = first_line(cg, case);
    let operands = line
        .split_once(char::is_whitespace)
        .map(|(_, rest)| rest)
        .unwrap_or("");
    let got: Vec<&str> = operands.split(',').map(|t| t.trim()).collect();
    assert_eq!(
        got.len(),
        expected.len(),
        "{case}: 操作数个数不对，实际 `{line}`，字节 {:?}",
        cg.code
    );
    for (i, e) in expected.iter().enumerate() {
        assert!(
            got[i].contains(e),
            "{case}: 第 {} 个操作数应为 {e}，实际 `{line}`（字节 {:?}）",
            i + 1,
            cg.code
        );
    }
    assert_no_legacy8(&line, &cg.code, case, expected);
}

fn first_line(cg: &Codegen, case: &str) -> String {
    assert!(
        !cg.diagnostics.iter().any(|d| d.sev == Severity::Error),
        "{case}: 编码阶段就报错了: {:?}",
        cg.diagnostics
            .iter()
            .filter(|d| d.sev == Severity::Error)
            .map(|d| &d.msg)
            .collect::<Vec<_>>()
    );
    let out = decode(&cg.code);
    let line = out.lines().next().unwrap_or("").trim().to_string();
    // 解码器读不出来有两种兜底：`.byte ...` 与 `# UNKNOWN ...`。两者都等于这条用例
    // **没跑**，必须当场判红，否则门禁会在「新编码形态解码器不认识」时悄悄变成空跑。
    assert!(
        !line.contains(".byte") && !line.contains("UNKNOWN"),
        "{case}: 解码器看不懂这段字节，门禁未覆盖。字节 = {:?}, 解码 = {out}",
        cg.code
    );
    line
}

/// 同 `check`，但用于「意图那条不在首行」的多指令臂（`idiv`/`div` 前面必有 cqto 或
/// `xor rdx,rdx`）。判据仍是名字：除数寄存器被搬错字段时，`{mn} ... {reg}` 这一行根本
/// 不会出现，而不是出现一条看起来正常的假指令。
fn check_divisor_line(cg: &Codegen, case: &str, mn: &str, reg: &str) {
    let out = decode(&cg.code);
    assert!(
        out.lines()
            .any(|l| l.trim().starts_with(mn) && l.contains(reg)),
        "{case}: 没读到 `{mn} ... {reg}` 这一行（除数字段的 REX 位丢了就会读成别的号）。\
         字节 = {:?}, 解码 = {out}",
        cg.code
    );
    for legacy in ["%ah", "%ch", "%dh", "%bh"] {
        assert!(
            !out.contains(legacy),
            "{case}: 解码里出现 {legacy}，说明 8 位字段该发 REX 而漏发；字节 {:?}",
            cg.code
        );
    }
}

/// 同 `check`，但只断言「意图名在首行里」—— 用于操作数次序由解码侧决定、
/// 而本轮不想连带迁就的场合（见 `binop_matrix` 的 Mul 注释）。
fn check_at(cg: &Codegen, case: &str, expected: &str) {
    let line = first_line(cg, case);
    assert!(
        line.contains(expected),
        "{case}: 期望读回 {expected}，实际 `{line}`（字节 {:?}）—— 字段被搬走了",
        cg.code
    );
}

/// 逐操作数核对：首行里出现的**每一个** `%名` 都必须在 `allowed` 里。
///
/// 为什么需要这一条：自反形（同一个号既在 ModRM.reg 又在 r/m）的两个意图名是
/// 同一个，「名字在不在」拓不出其中一侧被搬回低 8 号 —— 实测
/// `movsbq (%r8), %eax` 这种读数里 `%r8` 明明在行上，而 dest 字段已经错了。
/// 只看首行：一个编码点发了多条时逐条跑本函数（或由调用方只传首条的意图）。
fn check_only_regs(cg: &Codegen, case: &str, allowed: &[impl AsRef<str>]) {
    let line = first_line(cg, case);
    let body = line.splitn(2, char::is_whitespace).nth(1).unwrap_or("");
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0usize;
    let mut seen = 0usize;
    while i < chars.len() {
        if chars[i] != '%' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_ascii_alphanumeric() {
            j += 1;
        }
        let got = &body[i..j];
        seen += 1;
        let want = allowed
            .iter()
            .map(|a| a.as_ref())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            allowed.iter().any(|a| a.as_ref() == got),
            "{case}: 读数是 `{line}`，其中的 {got} 不在意图寄存器集 [{want}] 里 —— \
             某个 ModRM 字段没被正确的 REX 位扩到（字节 {:?}）",
            cg.code
        );
        i = j;
    }
    assert!(seen > 0, "{case}: 首行 `{line}` 里一个寄存器都没读到，门禁未覆盖");
}

/// 某个号的全部宽度名（一个自反形的两个字段都只能出现这些名字）。
fn own_names(dst: u8) -> Vec<String> {
    [8usize, 16, 32, 64]
        .iter()
        .map(|&b| name(dst, b).to_string())
        .collect()
}

/// `emit_load_self_typed`：地址与目的地同一个号的按类型装载。
/// 这一族以前抄在 10 个站点上（既不发 REX 也不发 SIB），现在收敛到一个入口，
/// 而入口必须有自己的机器判据：16 号 × 4 宽度 × 2 符号性，按**逐操作数**核对。
#[test]
fn load_self_typed_matrix() {
    for dst in 0..16u8 {
        for size in [1usize, 2, 4, 8] {
            for signed in [false, true] {
                let mut c = cg();
                c.emit_load_self_typed(size, signed, dst);
                // 允许的名字：dst 自己的各宽度名（基址以 64 位名出现，目标按指令宽度出现）。
                let allowed = own_names(dst);
                check_only_regs(
                    &c,
                    &format!("load-self r{dst} size={size} signed={signed}"),
                    &allowed,
                );
            }
        }
    }
}

/// `gen_int_narrow_cast` / `gen_int_promote`：寄存器内部的截断与提升，全部是自反形。
/// 这两个入口以前各自手抄字节，同一个错在同一函数里修了一半还剩一半。
#[test]
fn narrow_cast_promote_matrix() {
    // (类型, narrow_cast 该不该发字节)：逐条写死，为的是把「按设计不发」和
    // 「漏发」分开。有符号 int/long 的截断与提升都不需要动作（值已在 64 位
    // 寄存器里），而空字节若不当成断言，本组就等于没跑。
    let cases = [
        (Type::Bool, true),
        (Type::Char, true),
        (Type::SChar, true),
        (Type::UChar, true),
        (Type::Short, true),
        (Type::UShort, true),
        (Type::Int, false),
        (Type::UInt, true),
    ];
    for dst in 0..16u8 {
        for (ty, emits) in cases.iter() {
            let mut c = cg();
            c.gen_int_narrow_cast(dst, ty);
            assert_eq!(
                !c.code.is_empty(),
                *emits,
                "narrow_cast r{dst} {ty:?}: 预期发字节={emits}，实际字节={:?}",
                c.code
            );
            if *emits {
                check_only_regs(&c, &format!("narrow_cast r{dst} {ty:?}"), &own_names(dst));
            }

            // 提升：int/UInt 以上不需要动作，窄类型必须发一条扩展指令。
            let mut p = cg();
            p.gen_int_promote(dst, ty);
            let want_promote = !matches!(ty, Type::Int | Type::UInt);
            assert_eq!(
                !p.code.is_empty(),
                want_promote,
                "promote r{dst} {ty:?}: 预期发字节={want_promote}，实际字节={:?}",
                p.code
            );
            if want_promote {
                check_only_regs(&p, &format!("promote r{dst} {ty:?}"), &own_names(dst));
            }
        }
    }
}

/// `emit_float_to_int_saturate`：正溢出夹 INT_MAX。旧实现按 `dst==1` 分两支，
/// else 支把 r8/r9/r10 当空闲暂存 —— P16 暂存号参数化后 dst 可以是任意号（含高 8
/// 号），dst 自己被当暂存毁掉（实测 method_overloads.cpp 2.5→int 得 0x80000004）。
/// 新实现零额外 GPR，故对 16 个 dst 全扫一遍：解码出的**整数**指令只允许碰 dst 自己。
#[test]
fn float_to_int_saturate_touches_only_dst() {
    for float_ty in &[Type::Float, Type::Double, Type::LongDouble] {
        for is_64bit in [false, true] {
            for dst in 0..16u8 {
                let mut c = cg();
                c.emit_float_to_int_saturate(dst, float_ty, is_64bit);
                let case = format!("saturate {float_ty:?} wide={is_64bit} dst=r{dst}");
                let out = decode(&c.code);
                // 整数宽度名：test 与 mov 立即数都按目标宽度出现。
                let bits = if is_64bit { 64 } else { 32 };
                let dst_name = name(dst, bits);
                // 逐行扫描所有整数指令（ucomis/xorps 行允许出现 xmm0/xmm1）。
                for line in out.lines().map(|l| l.trim()) {
                    if line.starts_with("ucomis") || line.starts_with("xorps") {
                        continue;
                    }
                    // jb/jns/jmp 的相对跳转与标号不含寄存器；test/mov 行必须只碰 dst。
                    if line.starts_with('j') {
                        continue;
                    }
                    if line.is_empty() || line.ends_with(':') {
                        continue;
                    }
                    let mut regs = Vec::new();
                    let chars: Vec<char> = line.chars().collect();
                    let mut i = 0;
                    while i < chars.len() {
                        if chars[i] != '%' {
                            i += 1;
                            continue;
                        }
                        let mut j = i + 1;
                        while j < chars.len() && chars[j].is_ascii_alphanumeric() {
                            j += 1;
                        }
                        regs.push(line[i..j].to_string());
                        i = j;
                    }
                    for r in &regs {
                        assert_eq!(
                            r, dst_name,
                            "{case}: 整数指令 `{line}` 碰了 {r}，饱和序列不得使用 dst 之外的 GPR \
                             （旧实现硬编码 r8/r9/r10 在 dst 命中时毁值）。全解码:\n{out}"
                        );
                    }
                    assert!(
                        regs.is_empty() || regs.iter().all(|r| r == dst_name),
                        "{case}: 整数寄存器只能是 {dst_name}，实际 {regs:?}；行 `{line}`"
                    );
                }
            }
        }
    }
}

fn assert_no_legacy8(line: &str, code: &[u8], case: &str, expected: &[&str]) {
    for legacy in ["%ah", "%ch", "%dh", "%bh"] {
        assert!(
            !line.contains(legacy),
            "{case}: 读回 {legacy} 说明 8 位字段没带 REX 前缀（该发却漏发），\
             意图名应为 {}；实际 `{line}`，字节 {:?}",
            expected.join(" / "),
            code
        );
    }
}

// ---- 被审的编码函数 ---------------------------------------------------------

/// `movzx/movsx` 的窄源：reg 字段是**目标**（宽），r/m 字段是**源**（窄）。
/// 已知踩雷：为源在 4..7 而置 REX.B，会把目标搬到 +8。
#[test]
fn mov_reg_truncate_matrix() {
    for sz in [8u8, 16] {
        for dst in 0..16u8 {
            for src in 0..16u8 {
                let mut c = cg();
                c.mov_reg_truncate(dst, src, (sz / 8) as usize);
                check_ordered(
                    &c,
                    &format!("movzx sz={sz} dst={dst} src={src}"),
                    &[name(src, sz as usize), name(dst, 32)],
                );
            }
        }
    }
}

/// `mov r/m, r` 存到裸指针：ModRM.reg 是**值**寄存器，r/m 是**基址**。
/// 已知踩雷：值寄存器落在 4..7 时置 REX.B，把基址从 (%rbx) 搬成 (%r11)。
#[test]
fn store_reg_to_ptr_matrix() {
    for sz in [1usize, 2, 4, 8] {
        let bits = sz * 8;
        for src in 0..16u8 {
            for base in 0..16u8 {
                let mut c = cg();
                c.store_reg_to_ptr(src, base, sz);
                check_ordered(
                    &c,
                    &format!("store sz={sz} src={src} base={base}"),
                    &[name(src, bits), name(base, 64)],
                );
            }
        }
    }
}

/// `binop`：表达式求值主干的 16 个臂，以前**不在矩阵里**。代价已经兑现：比较臂的一条
/// REX.B 漏发（`cmp %r8d,%r12d` 编成 `cmp %r8d,%esp`，mod=3 rm=100 是非法编码）能在
/// 92 个夹具上静默误编译而 cargo 用例全绿 —— 因为 P16 把右操作数的暂存号从写死的
/// `%rcx` 换成候选表首号（`%r8`）之后，「两侧同属高 8 号」从死分支变成常走分支。
/// 本矩阵就是把「点亮死分支」这类改动变成机器判据的那一步。
#[test]
fn binop_matrix() {
    use crate::ast::BinOp;

    // Add/Sub/And/Or/Xor：dst 在 r/m（REX.B），src 在 reg（REX.R）。32 位一支只有在
    // 「无符号且两侧 ≤4 字节」时才成立，两支的前缀基数不同（0 vs 0x48），必须都跑。
    for (ty, bits) in [(Type::UInt, 32usize), (Type::ULong, 64)] {
        for op in [BinOp::Add, BinOp::Sub, BinOp::And, BinOp::Or, BinOp::Xor] {
            for dst in 0..16u8 {
                for src in 0..16u8 {
                    let mut c = cg();
                    c.binop(op, dst, src, false, &ty, &ty);
                    check_ordered(
                        &c,
                        &format!("{op:?} r{dst},r{src} bits={bits}"),
                        &[name(src, bits), name(dst, bits)],
                    );
                }
            }
        }
        // 比较臂：首条就是 cmp（窄无符号的零扩展在 ULong 上提前返回，不会插到前面）。
        for op in [
            BinOp::Lt,
            BinOp::Le,
            BinOp::Gt,
            BinOp::Ge,
            BinOp::EqEq,
            BinOp::Ne,
        ] {
            for dst in 0..16u8 {
                for src in 0..16u8 {
                    for signed in [false, true] {
                        let mut c = cg();
                        c.binop(op, dst, src, signed, &ty, &ty);
                        check_ordered(
                            &c,
                            &format!("{op:?} r{dst},r{src} bits={bits} signed={signed}"),
                            &[name(src, bits), name(dst, bits)],
                        );
                    }
                }
            }
        }
    }

    // Mul：`imul r/m, r` 里 dst 在 reg 字段、src 在 r/m —— 与加减分支反着来，
    // 所以必须单独跑一遍（同一份 rex 变量在两支里贡献的是不同的位）。
    // 这里不用 `check_ordered` 而用 `check`：自家反汇编把 `0F AF` 的操作数**次序**
    // 念反了（`48 0f af c1` 自家读成 `imul %rax, %rcx`，objdump 是 `imul %rcx,%rax`），
    // 那是解码侧的一条独立缺陷（见设计文档 §11.51），不应让编码侧的矩阵去迁就它；
    // 而「两个意图名字必须都在」已经足以拓出字段被搬错/REX 位漏发。
    for dst in 0..16u8 {
        for src in 0..16u8 {
            let mut c = cg();
            c.binop(BinOp::Mul, dst, src, true, &Type::Long, &Type::Long);
            let case = format!("imul r{dst},r{src}");
            check(&c, &case, &[name(src, 64)]);
            check_at(&c, &case, name(dst, 64));
        }
    }

    // Shl/Shr 的 %cl 形：src 已经是 cl 时首条就是移位，r/m 是 dst（只可能贡献 REX.B）。
    for op in [BinOp::Shl, BinOp::Shr] {
        for dst in 0..16u8 {
            for signed in [false, true] {
                let mut c = cg();
                c.binop(op, dst, 1, signed, &Type::Long, &Type::Long);
                check_ordered(
                    &c,
                    &format!("{op:?} cl -> r{dst} signed={signed}"),
                    &["%cl", name(dst, 64)],
                );
            }
        }
    }

    // Div/Mod：`idiv/div r/m` 的除数在 r/m 字段，而它不在首条（前面有 cqto/xor rdx,rdx
    // 与把被除数搬 rax 的 mov），所以按「行」定位而不是取首行。src==2 一支会把除数
    // 经 push/pop 挪到 %rcx（刻意的），把它当 `%rdx` 期望会假红，故跳过。
    for op in [BinOp::Div, BinOp::Mod] {
        for dst in 0..16u8 {
            for src in 0..16u8 {
                if src == 2 {
                    continue;
                }
                for signed in [false, true] {
                    let mut c = cg();
                    c.binop(op, dst, src, signed, &Type::Long, &Type::Long);
                    let mn = if signed { "idiv" } else { "div" };
                    check_divisor_line(
                        &c,
                        &format!("{op:?} r{dst}/r{src} signed={signed}"),
                        mn,
                        name(src, 64),
                    );
                }
            }
        }
    }
}

/// asm 侧 `mov`，寄存器→寄存器：reg=源、r/m=目的，两者各自独立扩展。
#[test]
fn emit_asm_mov_reg_reg_matrix() {
    for bits in [8u8, 16, 32, 64] {
        for s in 0..16u8 {
            for d in 0..16u8 {
                let mut c = cg();
                c.emit_asm_mov(&AsmOperand::Reg(s, bits), &AsmOperand::Reg(d, bits), bits);
                check_ordered(
                    &c,
                    &format!("asm mov r{s}->r{d} bits={bits}"),
                    &[name(s, bits as usize), name(d, bits as usize)],
                );
            }
        }
    }
}

/// asm 侧 `mov $imm, r`：寄存器编号在 **opcode 低 3 位**，扩展它的是 REX.B。
/// 已知踩雷：8 位一支完全不发 REX（`%dh`），且 d>=8 时搬到 %r8 之外。
#[test]
fn emit_asm_mov_imm_to_reg_matrix() {
    for bits in [8u8, 16, 32, 64] {
        for d in 0..16u8 {
            let mut c = cg();
            c.emit_asm_mov(&AsmOperand::Imm(0x41), &AsmOperand::Reg(d, bits), bits);
            check(&c, &format!("asm mov $imm->r{d} bits={bits}"), &[name(d, bits as usize)]);
        }
    }
}

/// asm 侧 8 位 ALU 立即数（`80 /digit ib`）：`/digit` 是 opcode 扩展，**绝不能**被
/// REX.R 扩展（/0 会变成 /8 = 未定义操作码，真机 #UD），目的寄存器在 r/m 上需要 REX.B。
#[test]
fn emit_asm_alu_imm8_to_reg_matrix() {
    for op in 0..8u8 {
        for d in 0..16u8 {
            let mut c = cg();
            c.emit_asm_alu(&AsmOperand::Imm(16), &AsmOperand::Reg(d, 8), op, 8);
            check(&c, &format!("asm alu8 op={op} -> r{d}"), &[name(d, 8)]);
        }
    }
}

/// `mov $imm, [mem]`（`C6 /0 ib` 与 `C7 /0 id`）：目的在 **r/m**，base/index 只要有一个
/// >=8 就必须发 REX。旧代码把 need_rex 硬编码成 false：实测 `movb $77, -0x40(%r13)`
/// 被编成 `-0x40(%rbp)` —— 写进了别人的栈槽，长度完全一样，既不崩溃也无诊断。
#[test]
fn emit_asm_mov_imm_to_mem_matrix() {
    for bits in [8u8, 16, 32, 64] {
        for base in [3u8, 5, 12, 13, 14] {
            let mut c = cg();
            c.emit_asm_mov(
                &AsmOperand::Imm(0x41),
                &AsmOperand::Mem {
                    disp: -8,
                    base,
                    index: None,
                    scale: 0,
                },
                bits,
            );
            check(
                &c,
                &format!("asm mov $imm -> -8({}) bits={bits}", name(base, 64)),
                &[name(base, 64)],
            );
        }
    }
}

/// `setcc r/m8`：目的寄存器在 r/m 字段。已知踩雷：把 4..7 当成 %ah/%ch/%dh/%bh 而不发 REX。
#[test]
fn emit_asm_setcc_matrix() {
    for r in 0..16u8 {
        let mut c = cg();
        c.emit_asm_setcc(&AsmOperand::Reg(r, 8), 0x94); // sete
        check(&c, &format!("sete r{r}"), &[name(r, 8)]);
    }
}

/// 移位计数：x86 只认 %cl，所以任意寄存器先要 `mov c, cl`。那条 mov 里计数寄存器在
/// **ModRM.reg**（rcx 在 r/m），所以扩展它的是 REX.R；写成 REX.B 会把两个字段读成
/// `mov %rcx,%r9` —— 方向颠倒，既砸了 %r9 又没设 cl（实测踩过）。用按位序的
/// check_ordered：只看「名字在不在」对这类缺陷永远拓不到。
#[test]
fn emit_asm_shift_reg_count_matrix() {
    for bits in [32u8, 64] {
        for c in 0..16u8 {
            if c == 1 {
                continue; // 已经是 %cl，不发 mov
            }
            for d in 0..16u8 {
                let mut cgx = cg();
                cgx.emit_asm_shift(&AsmOperand::Reg(c, bits), &AsmOperand::Reg(d, bits), 7, bits);
                check_ordered(
                    &cgx,
                    &format!("sar 由 r{c} 计数 -> r{d} bits={bits}"),
                    &[name(c, 64), "%rcx"],
                );
            }
        }
    }
}

/// 立即数移位：8 位是 `D0 /digit` 与 `C0 /digit ib`，16/32/64 才是 `D1/C1`。
/// 拿 D1/C1 做 8 位会把整个 32 位移过去（解码回来是 %eax）。
#[test]
fn emit_asm_shift_imm_matrix() {
    for bits in [8u8, 16, 32, 64] {
        for d in 0..16u8 {
            for v in [1i64, 2] {
                let mut cgx = cg();
                cgx.emit_asm_shift(&AsmOperand::Imm(v), &AsmOperand::Reg(d, bits), 7, bits);
                // 第一行可能是前置的 mov（bits 不等于 8 时不会），移位总在同一行：
                // 目的寄存器的宽度就是这条指令的宽度。
                check(&cgx, &format!("sar ${v} r{d} bits={bits}"), &[name(d, bits as usize)]);
            }
        }
    }
}

/// asm 侧 `mov reg, [base]` 存：8 位必须走 `88 /r`。写成 `89 /r` 会把 1 字节存成 4 字节，
/// 越界盖掉相邻变量 —— 比错寄存器更坏。基址特意取 rsp/rbp/r12/r13 这几个「要 SIB / 要
/// disp8」的，因为它们才是 SIB/REX 判据最容易混的地方。
#[test]
fn emit_asm_mov_reg_to_mem_matrix() {
    for bits in [8u8, 16, 32, 64] {
        for src in 0..16u8 {
            for base in [3u8, 4, 5, 12, 13] {
                let mut c = cg();
                c.emit_asm_mov(
                    &AsmOperand::Reg(src, bits),
                    &AsmOperand::Mem {
                        disp: 0,
                        base,
                        index: None,
                        scale: 0,
                    },
                    bits,
                );
                check_ordered(
                    &c,
                    &format!("asm mov r{src} -> ({}) bits={bits}", name(base, 64)),
                    &[name(src, bits as usize), name(base, 64)],
                );
            }
        }
    }
}

/// `inc/dec r/m8`（`FE /0`）与 16/32/64 位（`FF /0`）：同样不允许把 /digit 当 reg。
#[test]
fn increment_matrix() {
    for sz in [1usize, 2, 4, 8] {
        let bits = sz * 8;
        for r in 0..16u8 {
            let mut c = cg();
            c.increment(r, sz);
            check(&c, &format!("inc r{r} sz={sz}"), &[name(r, bits)]);
        }
    }
}

/// 帧访问助手：整个后端流量最大的发射点，此前零覆盖。
///
/// 它们把寄存器放在 ModRM.reg（目的/源），把基址固定为 %r13 或 %r11，所以位序应该是
/// 「REX.R 给 reg + REX.B 给 base」。这一族的 modrm 不在紧跟的 emit 里（在
/// `emit_r13_modrm_disp` / `emit_reg_indirect_modrm` 里），所以批量批改的脚本只能把它们
/// 列为「判不出」——那不等于没病，而是判据没伸到这里。接进门禁就不再靠人读。
#[test]
fn frame_access_helpers_matrix() {
    for reg in 0..16u8 {
        // disp32 内与超出 disp32（后者走 lea %r11 + 间接寻址两条路）
        for off in [-40i64, -4096, 8i64, 4096i64] {
            let small = off.abs() < 2048;
            let mut c = cg();
            c.load_stack(reg, off);
            check(&c, &format!("load_stack r{reg} off={off}"), &[name(reg, 64)]);

            let mut c = cg();
            c.store_stack(reg, off);
            check(&c, &format!("store_stack r{reg} off={off}"), &[name(reg, 64)]);

            for size in [1usize, 2, 4, 8] {
                let mut c = cg();
                c.load_stack_sized(reg, off, size);
                // 窄尺寸走 movzx，目的寄存器读回来是 32 位。
                let w = if size < 4 { 32 } else { size * 8 };
                check(
                    &c,
                    &format!("load_stack_sized r{reg} size={size} off={off}"),
                    &[name(reg, w)],
                );

                let mut c = cg();
                c.emit_store(reg, off, size);
                check(
                    &c,
                    &format!("emit_store r{reg} size={size} off={off}"),
                    &[name(reg, size * 8)],
                );
            }

            if small {
                // %rsp 相对的两个助手：reg 字段是数据寄存器，base=rsp 需要 SIB。
                for off2 in [8usize, 4096] {
                    let mut c = cg();
                    c.store_rsp_qword(reg, off2);
                    check(&c, &format!("store_rsp_qword r{reg} off={off2}"), &[name(reg, 64)]);
                    let mut c = cg();
                    c.load_rsp_qword(reg, off2);
                    check(&c, &format!("load_rsp_qword r{reg} off={off2}"), &[name(reg, 64)]);
                }
                let mut c = cg();
                c.emit_lea_r13(reg, off);
                check(&c, &format!("emit_lea_r13 r{reg} off={off}"), &[name(reg, 64), "%r13"]);
            }
        }
    }
}

/// SSE 与 GP 之间的 `movd`，以及 `%r13` 相对的两条 `movdqu`。前缀次序必须是
/// `F3/F2 → REX → opcode`，而 xmm 寄存器号在 ModRM.reg 上（扩展位是 REX.R）。
#[test]
fn xmm_transfer_matrix() {
    for x in 0..16u8 {
        for off in [-40i64, 4096i64] {
            let mut c = cg();
            c.emit_movdqu_r13_to_xmm(x, off);
            check(&c, &format!("movdqu (%r13{off}) -> xmm{x}"), &[format!("%xmm{x}").as_str()]);

            let mut c = cg();
            c.emit_movdqu_xmm_to_r13(x, off);
            check(&c, &format!("movdqu xmm{x} -> (%r13{off})"), &[format!("%xmm{x}").as_str()]);
        }
    }
    for x in 0..16u8 {
        for gp in 0..16u8 {
            let mut c = cg();
            c.emit_movd_gp_to_xmm(x, gp);
            check(&c, &format!("movd r{gp} -> xmm{x}"), &[name(gp, 32)]);
        }
    }
}

/// SSE 与帧/栈之间的存与取。xmm 号一律在 ModRM.reg 上 ⇒ >=8 需要 REX.R；而
/// `movss/movsd` 的宽度由 F3/F2 定，**不能**带 REX.W（带了就会被 objdump 标成
/// `rex.WB movsd …` —— 那是多余位的唯一读数）。`emit_xmm_store_rsp` 旧代码一个前缀都不发，
/// 实测 `movsd %xmm9, (%rsp)` 实际写的是 %xmm0，两个浮点变量掉进同一个槽。
#[test]
fn xmm_frame_transfer_matrix() {
    for x in 0..16u8 {
        let xmm_name = format!("%xmm{x}");
        let xmm_name = xmm_name.as_str();
        for off in [-40i64, 4096i64] {
            let mut c = cg();
            c.store_xmm32_stack(x, off);
            check(&c, &format!("movss xmm{x} -> (%r13{off})"), &[xmm_name]);
            let mut c = cg();
            c.store_xmm64_stack(x, off);
            check(&c, &format!("movsd xmm{x} -> (%r13{off})"), &[xmm_name]);
            for elem in [4usize, 8] {
                let mut c = cg();
                c.load_stack_xmm(x, off, elem, elem == 4);
                check(&c, &format!("load_stack_xmm xmm{x} elem={elem} off={off}"), &[xmm_name]);
            }
            for off2 in [8usize, 4096] {
                for (p, mn) in [(0xF3u8, "movss"), (0xF2u8, "movsd")] {
                    let mut c = cg();
                    c.emit_xmm_store_rsp(p, x, off2);
                    check(&c, &format!("{mn} xmm{x} -> (%rsp+{off2})"), &[xmm_name]);
                    let mut c = cg();
                    c.emit_xmm_load_rsp(p, x, off2);
                    check(&c, &format!("{mn} (%rsp+{off2}) -> xmm{x}"), &[xmm_name]);
                }
            }
        }
        for base in [3u8, 4, 5, 12, 13] {
            for bits in [32u8, 64, 128] {
                let mut c = cg();
                c.store_xmm_to_ptr(x, base, bits);
                check(
                    &c,
                    &format!("store_xmm_to_ptr xmm{x} -> ({}) bits={bits}", name(base, 64)),
                    &[xmm_name, name(base, 64)],
                );
            }
        }
    }
}

/// 全局变量装载/存回（RIP 相对）：目标在 reg 字段，需要的是 REX.R；
/// 已知踩雷：注释写着 REX.R 而字节给了 0x41（REX.B），既丢 R 又把 r/m 搬成 (%r13+disp)。
#[test]
fn global_load_store_matrix() {
    // emit_global_load/store 的宽度来自 globals_raw，所以先把四种声明宽度登记进去。
    // 装载侧的期望宽度不是变量宽度：窄变量走 movzx/movsx，目标寄存器总是 32 位；
    // 64 位与 32 位有符号走 mov/movsxd，目标分别是 64/64 位。
    let cases = [
        ("g8", 1usize, Type::UChar, 32),
        ("g16", 2, Type::UShort, 32),
        ("g32", 4, Type::UInt, 32),
        ("g64", 8, Type::ULong, 64),
    ];
    let var = |sz: usize, ty: &Type| GlobalVar {
        offset: 0,
        size: sz,
        ty: ty.clone(),
        init: None,
        section: 6,
        is_tls: false,
        in_data_section: false,
    };
    for (key, sz, ty, load_w) in cases {
        for r in 0..16u8 {
            let mut load = cg();
            load.globals_raw.insert(key.to_string(), var(sz, &ty));
            load.emit_global_load(r, key);
            check(&load, &format!("load {key} -> r{r} sz={sz}"), &[name(r, load_w)]);

            let mut store = cg();
            store.globals_raw.insert(key.to_string(), var(sz, &ty));
            store.emit_global_store(r, key);
            // 存回侧的目标就在 ModRM.reg 上，宽度即变量宽度。
            check(&store, &format!("store r{r} -> {key} sz={sz}"), &[name(r, sz * 8)]);
        }
    }
}
