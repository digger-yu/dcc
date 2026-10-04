// src/codegen/x86_64/abi/regnames.rs —— x86_64 寄存器名 → (号, 位宽) 的**唯一**名表。
//
// 为什么要有它（`docs/p12-physreg-spill-design.md` §11.17 登记、§7 Phase 6.1 要求的
// 「三份名字表合一」）：同一件事（「`rbx` 是 3 号、64 位」）此前有三份实现：
//   1. `emit/func_setup.rs::parse_reg_name`        —— `register asm` 与 clobber 用；
//   2. `emit/expr/utils.rs::parse_reg_name_bits`   —— 表达式里的显式寄存器引用；
//   3. `asm/expansion.rs::parse_reg_name_bits`     —— asm 模板展开（与 2 内容逐条相同）。
// 2 与 3 是同一张表的两份拷贝（本轮逐条对拍过：68 个条目的名字、号、位宽、规范名全部相同），
// 而 1 是它们的**超集**：多认识 APX 的 r16-r31，并且额外接受 `%` 前缀与大写。三份表各自
// 演化就会漂移，而漂移的方向就是「同一个写法在一处认得、另一处不认得」—— 那已经是
// `%r13`/`%rsp` 这类保留号今天的行为（`register asm("r13")` 在 -O0 照做、在 -O2 静默降级）。
//
// 「可接受形式」因此成为表数据的一个**维度**（`percent_ok` / `extended`），而不是散在三个
// 调用点里的 if：调用方只声明自己接受哪种写法（`NAME_FORMS_STRICT` / `NAME_FORMS_USER`），
// 名字→号的事实仍然只有一份。这也让「新增一个寄存器（将来 EVEX/APX 再扩）」是一行表改动。

/// 一个可被写出来的 GP 寄存器名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RegNameSpec {
    /// 规范名（小写、无 `%`），也是返回给调用方的 AT&T 去前缀形态。
    pub name: &'static str,
    /// REX 三字段编码出来的那个 0..31。
    pub num: u8,
    /// 该写法对应的位宽（`eax` 是 32，`al` 是 8）。
    pub bits: u8,
    /// 这个写法是否接受 `%` 前缀形态（AT&T 里 `%rax` 合法，但 8 位半寄存器 `%al`/`%sil`
    /// 在原表里就不存在，接受它等于凭空扩语言）。
    pub percent_ok: bool,
    /// 是否 APX 扩展号（r16-r31）：只有 `register asm`/clobber 那条路认识它们，
    /// asm 模板里的显式寄存器引用（strict）今天不认， widening 会改产物。
    pub extended: bool,
}

/// 一个调用方允许的名字写法集合。
#[derive(Debug, Clone, Copy)]
pub(crate) struct NameForms {
    pub percent: bool,
    pub uppercase: bool,
    pub extended: bool,
}

/// asm 模板 / 表达式里的显式寄存器引用：只认精确的小写、无 `%`、无 APX 写法。
pub(crate) const NAME_FORMS_STRICT: NameForms = NameForms {
    percent: false,
    uppercase: false,
    extended: false,
};

/// `register asm("...")` 与 clobber 名单：允许 `%` 前缀、大写与 APX 扩展号。
pub(crate) const NAME_FORMS_USER: NameForms = NameForms {
    percent: true,
    uppercase: true,
    extended: true,
};

/// 唯一的名表。顺序沿用被合并掉的那三份（rax/eax/ax/al/ah, rbx…, r8…，然后 APX），
/// 便于逐条与 git 历史对拍。
#[rustfmt::skip]
pub(crate) static GP_REG_NAMES: &[RegNameSpec] = &[
    // rax
    RegNameSpec { name: "rax", num: 0, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "eax", num: 0, bits: 32, percent_ok: true, extended: false },
    RegNameSpec { name: "ax", num: 0, bits: 16, percent_ok: true, extended: false },
    RegNameSpec { name: "al", num: 0, bits: 8, percent_ok: false, extended: false },
    RegNameSpec { name: "ah", num: 0, bits: 8, percent_ok: false, extended: false },
    // rbx
    RegNameSpec { name: "rbx", num: 3, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "ebx", num: 3, bits: 32, percent_ok: true, extended: false },
    RegNameSpec { name: "bx", num: 3, bits: 16, percent_ok: true, extended: false },
    RegNameSpec { name: "bl", num: 3, bits: 8, percent_ok: false, extended: false },
    RegNameSpec { name: "bh", num: 3, bits: 8, percent_ok: false, extended: false },
    // rcx
    RegNameSpec { name: "rcx", num: 1, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "ecx", num: 1, bits: 32, percent_ok: true, extended: false },
    RegNameSpec { name: "cx", num: 1, bits: 16, percent_ok: true, extended: false },
    RegNameSpec { name: "cl", num: 1, bits: 8, percent_ok: false, extended: false },
    RegNameSpec { name: "ch", num: 1, bits: 8, percent_ok: false, extended: false },
    // rdx
    RegNameSpec { name: "rdx", num: 2, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "edx", num: 2, bits: 32, percent_ok: true, extended: false },
    RegNameSpec { name: "dx", num: 2, bits: 16, percent_ok: true, extended: false },
    RegNameSpec { name: "dl", num: 2, bits: 8, percent_ok: false, extended: false },
    RegNameSpec { name: "dh", num: 2, bits: 8, percent_ok: false, extended: false },
    // rsi / rdi / rsp / rbp：8 位形态是 sil/dil/spl/bpl（没有 ah/ch 那种高半字节）
    RegNameSpec { name: "rsi", num: 6, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "esi", num: 6, bits: 32, percent_ok: true, extended: false },
    RegNameSpec { name: "si", num: 6, bits: 16, percent_ok: true, extended: false },
    RegNameSpec { name: "sil", num: 6, bits: 8, percent_ok: false, extended: false },
    RegNameSpec { name: "rdi", num: 7, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "edi", num: 7, bits: 32, percent_ok: true, extended: false },
    RegNameSpec { name: "di", num: 7, bits: 16, percent_ok: true, extended: false },
    RegNameSpec { name: "dil", num: 7, bits: 8, percent_ok: false, extended: false },
    RegNameSpec { name: "rsp", num: 4, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "esp", num: 4, bits: 32, percent_ok: true, extended: false },
    RegNameSpec { name: "sp", num: 4, bits: 16, percent_ok: true, extended: false },
    RegNameSpec { name: "spl", num: 4, bits: 8, percent_ok: false, extended: false },
    RegNameSpec { name: "rbp", num: 5, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "ebp", num: 5, bits: 32, percent_ok: true, extended: false },
    RegNameSpec { name: "bp", num: 5, bits: 16, percent_ok: true, extended: false },
    RegNameSpec { name: "bpl", num: 5, bits: 8, percent_ok: false, extended: false },
    // r8..r15：只有 64 位形态有 `%` 写法（原表里 "%r8" 有、"r8d" 没有对应百分号项）
    RegNameSpec { name: "r8", num: 8, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "r8d", num: 8, bits: 32, percent_ok: false, extended: false },
    RegNameSpec { name: "r8w", num: 8, bits: 16, percent_ok: false, extended: false },
    RegNameSpec { name: "r8b", num: 8, bits: 8, percent_ok: false, extended: false },
    RegNameSpec { name: "r9", num: 9, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "r9d", num: 9, bits: 32, percent_ok: false, extended: false },
    RegNameSpec { name: "r9w", num: 9, bits: 16, percent_ok: false, extended: false },
    RegNameSpec { name: "r9b", num: 9, bits: 8, percent_ok: false, extended: false },
    RegNameSpec { name: "r10", num: 10, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "r10d", num: 10, bits: 32, percent_ok: false, extended: false },
    RegNameSpec { name: "r10w", num: 10, bits: 16, percent_ok: false, extended: false },
    RegNameSpec { name: "r10b", num: 10, bits: 8, percent_ok: false, extended: false },
    RegNameSpec { name: "r11", num: 11, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "r11d", num: 11, bits: 32, percent_ok: false, extended: false },
    RegNameSpec { name: "r11w", num: 11, bits: 16, percent_ok: false, extended: false },
    RegNameSpec { name: "r11b", num: 11, bits: 8, percent_ok: false, extended: false },
    RegNameSpec { name: "r12", num: 12, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "r12d", num: 12, bits: 32, percent_ok: false, extended: false },
    RegNameSpec { name: "r12w", num: 12, bits: 16, percent_ok: false, extended: false },
    RegNameSpec { name: "r12b", num: 12, bits: 8, percent_ok: false, extended: false },
    RegNameSpec { name: "r13", num: 13, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "r13d", num: 13, bits: 32, percent_ok: false, extended: false },
    RegNameSpec { name: "r13w", num: 13, bits: 16, percent_ok: false, extended: false },
    RegNameSpec { name: "r13b", num: 13, bits: 8, percent_ok: false, extended: false },
    RegNameSpec { name: "r14", num: 14, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "r14d", num: 14, bits: 32, percent_ok: false, extended: false },
    RegNameSpec { name: "r14w", num: 14, bits: 16, percent_ok: false, extended: false },
    RegNameSpec { name: "r14b", num: 14, bits: 8, percent_ok: false, extended: false },
    RegNameSpec { name: "r15", num: 15, bits: 64, percent_ok: true, extended: false },
    RegNameSpec { name: "r15d", num: 15, bits: 32, percent_ok: false, extended: false },
    RegNameSpec { name: "r15w", num: 15, bits: 16, percent_ok: false, extended: false },
    RegNameSpec { name: "r15b", num: 15, bits: 8, percent_ok: false, extended: false },
    // APX EGPR (Extended General Purpose Registers) r16-r31：只有 USER 那条路认识
    RegNameSpec { name: "r16", num: 16, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r16d", num: 16, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r16w", num: 16, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r16b", num: 16, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r17", num: 17, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r17d", num: 17, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r17w", num: 17, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r17b", num: 17, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r18", num: 18, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r18d", num: 18, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r18w", num: 18, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r18b", num: 18, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r19", num: 19, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r19d", num: 19, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r19w", num: 19, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r19b", num: 19, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r20", num: 20, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r20d", num: 20, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r20w", num: 20, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r20b", num: 20, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r21", num: 21, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r21d", num: 21, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r21w", num: 21, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r21b", num: 21, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r22", num: 22, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r22d", num: 22, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r22w", num: 22, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r22b", num: 22, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r23", num: 23, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r23d", num: 23, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r23w", num: 23, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r23b", num: 23, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r24", num: 24, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r24d", num: 24, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r24w", num: 24, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r24b", num: 24, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r25", num: 25, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r25d", num: 25, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r25w", num: 25, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r25b", num: 25, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r26", num: 26, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r26d", num: 26, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r26w", num: 26, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r26b", num: 26, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r27", num: 27, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r27d", num: 27, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r27w", num: 27, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r27b", num: 27, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r28", num: 28, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r28d", num: 28, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r28w", num: 28, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r28b", num: 28, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r29", num: 29, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r29d", num: 29, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r29w", num: 29, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r29b", num: 29, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r30", num: 30, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r30d", num: 30, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r30w", num: 30, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r30b", num: 30, bits: 8, percent_ok: false, extended: true },
    RegNameSpec { name: "r31", num: 31, bits: 64, percent_ok: true, extended: true },
    RegNameSpec { name: "r31d", num: 31, bits: 32, percent_ok: false, extended: true },
    RegNameSpec { name: "r31w", num: 31, bits: 16, percent_ok: false, extended: true },
    RegNameSpec { name: "r31b", num: 31, bits: 8, percent_ok: false, extended: true },
];

/// 名字 → 表项。`forms` 决定这个调用方接受哪些写法，**不接受的写法必须返回 `None`**
/// （而不是"顺手认一下"）：strict 那条路今天不认 APX，认了就改变 asm 展开的字节。
pub(crate) fn lookup_reg_name(raw: &str, forms: &NameForms) -> Option<&'static RegNameSpec> {
    let lowered;
    let mut text = raw;
    if forms.uppercase {
        lowered = raw.to_ascii_lowercase();
        text = &lowered;
    }
    if let Some(stripped) = text.strip_prefix('%') {
        if !forms.percent {
            return None;
        }
        text = stripped;
    }
    let had_percent = raw.starts_with('%');
    GP_REG_NAMES.iter().find(|e| {
        e.name == text && (forms.extended || !e.extended) && (!had_percent || e.percent_ok)
    })
}

/// 号 → 规范名（64 位形态）。表里同一号有多宽，所以按 `bits == 64` 取；APX 也算。
pub(crate) fn gp_name_of_num(num: u8) -> Option<&'static str> {
    GP_REG_NAMES
        .iter()
        .find(|e| e.num == num && e.bits == 64)
        .map(|e| e.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strict(name: &str) -> Option<u8> {
        lookup_reg_name(name, &NAME_FORMS_STRICT).map(|e| e.num)
    }
    fn user(name: &str) -> Option<u8> {
        lookup_reg_name(name, &NAME_FORMS_USER).map(|e| e.num)
    }
    fn bits_of(name: &str) -> Option<(u8, u8, &'static str)> {
        lookup_reg_name(name, &NAME_FORMS_STRICT).map(|e| (e.num, e.bits, e.name))
    }

    /// 表规模就是「合并没有丢条目」的证据：68 条 legacy + 64 条 APX。
    #[test]
    fn table_covers_every_merged_entry() {
        assert_eq!(GP_REG_NAMES.len(), 68 + 64);
        assert_eq!(
            GP_REG_NAMES.iter().filter(|e| !e.extended).count(),
            68,
            "legacy 条目数必须与被合并掉的两份表（各 68 条）相同"
        );
        // 号与位宽的组合唯一：同一 (name) 只能出现一次
        let mut seen: Vec<&str> = GP_REG_NAMES.iter().map(|e| e.name).collect();
        let before = seen.len();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), before, "同一个名字在表里出现两次（谁赢取决于顺序）");
    }

    /// strict 口径的**接受集必须一字不多**：`%`、大写、APX 全部不认。这三条正是
    /// 「三份表不一致」的具体所在，所以逐条钉住而不是笼统说「行为不变」。
    #[test]
    fn strict_forms_reject_percent_uppercase_and_apx() {
        assert_eq!(strict("rbx"), Some(3));
        assert_eq!(strict("%rbx"), None, "strict 认了 % 就会改变 asm 模板展开的字节");
        assert_eq!(strict("RBX"), None, "strict 不认大写");
        assert_eq!(strict("r16"), None, "strict 今天不认 APX");
        assert_eq!(strict("r15"), Some(15));
        assert_eq!(strict(""), None);
        assert_eq!(strict("nosuchreg"), None);
    }

    /// USER 口径（`register asm` 与 clobber）：%、大写、APX 都认，但 **8 位半寄存器与
    /// rNd/rNw/rNb 没有 `%` 形态**（原表就没有，凭空接受等于扩大语言）。
    #[test]
    fn user_forms_accept_percent_and_apx_but_not_invented_spellings() {
        assert_eq!(user("%rbx"), Some(3));
        assert_eq!(user("%RBX"), Some(3));
        assert_eq!(user("rbx"), Some(3));
        assert_eq!(user("R13"), Some(13));
        assert_eq!(user("%r13"), Some(13), "帧基址能被解析出来才谈得上拒它当家");
        assert_eq!(user("%r16"), Some(16));
        assert_eq!(user("r16d"), Some(16));
        assert_eq!(user("%sil"), None, "原表没有 %sil");
        assert_eq!(user("sil"), Some(6));
        assert_eq!(user("%al"), None, "原表没有 %al/%ah");
        assert_eq!(user("%r8d"), None, "r8..r31 只有 64 位形态有 % 写法");
        assert_eq!(user("%r8"), Some(8));
        assert_eq!(user("esp"), Some(4));
        assert_eq!(user("bpl"), Some(5));
    }

    /// 位宽/规范名读数必须与两份被合并的表一致（号、位宽、规范名三元组）。
    #[test]
    fn bits_and_canonical_name_match_the_merged_tables() {
        assert_eq!(bits_of("eax"), Some((0, 32, "eax")));
        assert_eq!(bits_of("ax"), Some((0, 16, "ax")));
        assert_eq!(bits_of("ah"), Some((0, 8, "ah")));
        assert_eq!(bits_of("dil"), Some((7, 8, "dil")));
        assert_eq!(bits_of("r13w"), Some((13, 16, "r13w")));
        assert_eq!(bits_of("r15b"), Some((15, 8, "r15b")));
    }

    /// 号 → 名必须与名 → 号互逆（诊断信息靠它，出错时说错号比不说更坏）。
    #[test]
    fn number_to_name_round_trips() {
        for num in 0..32u8 {
            let name = gp_name_of_num(num).unwrap_or_else(|| panic!("{num} 号没有 64 位规范名"));
            assert_eq!(user(name), Some(num), "号 {num} 的规范名 {name} 反查不一致");
            assert_eq!(name, name.to_ascii_lowercase());
        }
        assert_eq!(gp_name_of_num(3), Some("rbx"));
        assert_eq!(gp_name_of_num(13), Some("r13"));
        assert_eq!(gp_name_of_num(99), None);
    }
}
