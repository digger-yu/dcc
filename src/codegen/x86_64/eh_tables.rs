//! src/codegen/x86_64/eh_tables.rs
//!
//! Itanium C++ 零成本异常 (zero-cost EH) 的 **LSDA 编码器**。
//!
//! 职责边界: 本模块只做**与指令生成无关的字节编码** (uleb128/sleb128 +
//! `.gcc_except_table` 的三段布局), 不生成指令、不接触 ELF 符号表。把「表内容」
//! 与「表放在哪个节、需要哪些重定位」分开, 是因为同一份编码既要被 x86_64 后端
//! 消费, 也要能被单元测试逐字节断言 (对照 g++ 产物)。
//!
//! LSDA (Language Specific Data Area) 的读者是 libstdc++ 的
//! `__gxx_personality_v0`, 因此字节布局必须与 GCC 完全一致。下面的格式来自对
//! g++ 产物的逐字节核对 (`readelf --debug-dump=frames` / `-x .gcc_except_table`
//! / `-r`):
//!
//! ```text
//! .LLSDA:
//!   byte 0xff              LPStart encoding = DW_EH_PE_omit (基址取 FDE 的 pc_begin)
//!   byte <ttype encoding>  TType encoding (本后端用 0x9b = pcrel|indirect|sdata4)
//!   uleb <len>             从本字段之后到 LSDA 结尾的字节数 (含下列全部三段)
//! .LLSDATTD:
//!   byte 0x01              call-site 表条目编码 = DW_EH_PE_uleb
//!   uleb <len>             call-site 表字节数
//!   [uleb start, uleb range, uleb landing_pad, uleb action] * N
//! .LLSDACSE:
//!   action 表: 每条记录 = (sleb filter, sleb next)
//!   align 4
//!   类型表: N 个 4 字节项, **索引 1 = 最后一项** (自表尾向前计数)
//! .LLSDATT
//! ```
//!
//! 四条最容易写错的规则 (前三条均已用 g++ 产物验证, 见 `tests` 里的对照断言):
//!
//! 1. call-site 里的 `action` 是链首记录相对 action 表的 **1-based** 字节偏移,
//!    0 表示「无 action」(该区间既不 cleanup 也不捕获, unwinder 直接掠过)。
//! 2. **`landing_pad = 0` 且 `action = 0` 的条目必须登记, 不能省略**。libstdc++ 的
//!    `__gxx_personality_v0` 在清理阶段遇到「PC 落在本函数 FDE 内却没有任何
//!    call-site 区间覆盖」时按「清理代码里二次抛出」处理并 `std::terminate`;
//!    于是 `_Unwind_Resume` / `__cxa_rethrow` 这类**本帧不接管、只把异常继续向外
//!    递**的调用反而必须有区间。g++ 产物里 `.LEHBn/.LEHEn` 恰好只包住
//!    `call _Unwind_Resume` 就是这条规则 (实测缺失时中间帧的析构一执行完就
//!    terminate, 外层 handler 永远进不去)。
//! 3. action 记录里的 `next` 是**自相对**偏移: 从 `next` 字段自身所在位置起算,
//!    指向链上下一条记录的**起始**位置; 0 表示链尾。因此把一条链的多条记录连续
//!    排布、且 `next` 恒占 1 字节时, 每条记录的 `next` 就是 `+1`。
//! 4. 头部 ttype 长度是**最小宽度**的 uleb128, 不能补零拉长: 读者在第一个
//!    continuation 位为 0 的字节处结束取值, 多余的 `0x00` 会被当成 call-site
//!    编码, 于是整张表错位 (见 `large_lsda_keeps_header_length_self_consistent`)。
//!    另外每个 LSDA 的总长度都是 4 的倍数 (类型表先对齐再排 4 字节项), 所以同节
//!    里后续函数的 LSDA 也从 4 字节对齐处开始 —— 类型表「相对本 LSDA 对齐」因此
//!    等价于「在节内绝对对齐」, 发射端可以直接把各段首尾相接。

use crate::ast::EhAction;
use std::collections::HashMap;

/// 一个受保护调用点 (由 `Stmt::Invoke` 在指令生成时登记)。
#[derive(Debug, Clone)]
pub struct EhSite {
    /// 调用指令起点 (`.text` 内绝对偏移)
    pub start: u64,
    /// 调用指令终点 (不含)
    pub end: u64,
    /// 落地垫的汇编标签名 (在 `Codegen::labels` 里查最终偏移)。
    /// `None` = 发 `landing_pad = 0` 的区间 (本帧不接管, 见模块头的规则 2)。
    pub landing_pad: Option<String>,
    /// 本调用点的 action 链 (处理顺序: 先 cleanup 后候选 handler)
    pub actions: Vec<EhAction>,
}

/// 一个已完成函数的 EH 归档 (函数结束时从 `Codegen::eh_sites` 搬来)。
#[derive(Debug, Clone)]
pub struct EhFuncInfo {
    /// 函数符号名 (同时是 `Codegen::labels` 里的标签名)
    pub name: String,
    /// **本次发射**在 `.text` 里的起始偏移。存下来而不是事后按名字查 `labels`:
    /// 同一个名字被发射多次时 (前端把重载合并成同一个链接名就会如此),
    /// `labels[name]` 只保留最后一份; 按名字查会让早先副本的调用点区间被编进
    /// 指向另一份代码的 LSDA, PC 查表整体错位 (实测减法直接下溢 panic)。
    pub text_start: u64,
    /// 按代码生成顺序登记的受保护调用点
    pub sites: Vec<EhSite>,
}

/// 一个函数的 **CFI 事实**: FDE 要据实描述序言, 而不是猜一个常量形状。
///
/// 本后端的序言固定为 `push %r13; mov %rsp,%r13` (+ `-frame-pointer` 时的
/// `push %rbp; mov %rbp,%rsp`), 之后整个函数体都以 r13 为帧基址, rsp 可以自由
/// 起伏 (call 参数区 / VLA)。因此 CFA 规则写成 `CFA = r13 + 16 (+8)` 一条就能
/// 覆盖全函数 —— 这是异常展开能走通本帧的关键 (旧实现对每个函数发同一段
/// `CFA = rsp + 16` 常量 ops, 只要 `sub $N,%rsp` 的 N != 0 就是错的)。
#[derive(Debug, Clone, Copy)]
pub struct FuncCfi {
    /// 序言字节数 (`DW_CFA_advance_loc` 到此偏移后才切换 CFA 规则)
    pub prologue_end: u64,
    /// 实际使用的 callee-saved 掩码: bit0=rbx, bit1=r12, bit2=r14, bit3=r15
    pub callee_saved_used: u8,
    /// 是否额外维护 rbp 帧指针 (影响 CFA 偏移量与 rbp 的保存位置)
    pub frame_pointer: bool,
    /// 本函数把跳表基址常驻在 `%rbp`，并在帧顶那一格存了调用者的值（阶段 6 批 4）。
    ///
    /// 必须进 CFI：那一格是 `CFA-24`（`CFA = %r13+16`，存槽在 `%r13-8`），与帧指针档里
    /// rbp 那条规则同位置同因子。漏了它，展开穿过本帧时 rbp 不会被恢复 —— 调用者的值
    /// 就丢了（症状是异常抛过 computed-goto 函数后上层函数的帧指针是跳表地址）。
    pub rbp_saved: bool,
}

/// LSDA 类型表编码: `DW_EH_PE_pcrel | DW_EH_PE_indirect | DW_EH_PE_sdata4`。
///
/// `indirect` 是 GCC 的默认选择: 表项存的是 `DW.ref._ZTIxxx` 槽的地址, 由该槽
/// 再指向真正的 typeinfo。这样即使 typeinfo 定义在其他动态库 (如
/// `std::bad_alloc`) 链接期也能解析; 直接 PC 相对引用外部数据符号在非 PIE 下
/// 同样无法由链接器完成。
pub const TTYPE_ENCODING: u8 = 0x9b;

/// call-site 表条目的编码 (`DW_EH_PE_uleb`)。
pub const CALL_SITE_ENCODING: u8 = 0x01;

/// `.gcc_except_table` 里一个函数 LSDA 的产物。
#[derive(Debug, Clone, Default)]
pub struct LsdaImage {
    /// LSDA 字节 (整段, 从 LPStart encoding 开始)
    pub bytes: Vec<u8>,
    /// 类型表每个 4 字节项需要的重定位: `(项在 bytes 中的偏移, 符号名)`。
    /// `catch (...)` 的空表项是常量 0, 不出现在这里。
    pub type_relocs: Vec<(u64, String)>,
    /// 登记了落地垫却查不到标签偏移的调用点 (标签名)。非空即为后端内部不变式
    /// 被破坏: 该区间本来受保护, 静默丢掉条目会让异常直接穿过本帧。发射端必须
    /// 把它当错误报出来, 不能发错表。
    pub unresolved_sites: Vec<String>,
}

/// 编码 uleb128。
pub fn write_uleb128(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            break;
        }
    }
}

/// 编码 sleb128。
pub fn write_sleb128(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        // 算术右移后剩余内容必须能被下一字节的 7 位继续表示, 否则还要再发一字节。
        let done = (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0);
        out.push(if done { b } else { b | 0x80 });
        if done {
            break;
        }
    }
}

/// 一条 action 链的**规范化 key** 类型: `(是否 cleanup, 类型索引)` 序列。
///
/// 命名它的理由除了可读, 也让 [`build_action_table`] 的返回类型不再是一串
/// 嵌套泛型 (clippy::type_complexity)。
type ChainKey = Vec<(bool, u32)>;

/// 一条 action 链的**规范化 key** (用于跨调用点去重共享同一条链)。
///
/// 用 `(是否 cleanup, 类型索引)` 序列而不是原始字节: 类型索引是 lowering 侧
/// 分配的单一真源, 同索引必然指向同一类型表项。
fn chain_key(actions: &[EhAction]) -> ChainKey {
    actions
        .iter()
        .map(|a| match a {
            EhAction::Cleanup => (true, 0),
            EhAction::Catch { type_index, .. } => (false, *type_index),
        })
        .collect()
}

/// action 记录里的 filter 值。
fn filter_of(action: &EhAction) -> i64 {
    match action {
        EhAction::Cleanup => 0,
        EhAction::Catch { type_index, .. } => *type_index as i64,
    }
}

/// 构建 action 表: 去重后的链 + `链 key -> 1-based 偏移`。
///
/// 每条链的记录**连续**排布。`next` 字段是自相对偏移, 而记录长度 =
/// `filter 字节数 + next 字节数`; 取「`next` 永远写 1 字节」这个定点:
/// 则第 i 条记录起点 = pos_i, `next` 字段位于 pos_i + len(filter_i),
/// 下一条记录起点 = pos_i + len(filter_i) + 1 —— 于是 **next 恒为 +1** (链尾为 0)。
/// 该定点自洽 (sleb(1) 就是 1 字节), 因此无需迭代求解。
fn build_action_table(chains: &[Vec<EhAction>]) -> (Vec<u8>, HashMap<ChainKey, u64>) {
    let mut table = Vec::new();
    let mut offsets: HashMap<ChainKey, u64> = HashMap::new();
    for chain in chains {
        let key = chain_key(chain);
        if offsets.contains_key(&key) {
            continue;
        }
        let base = table.len();
        for (i, action) in chain.iter().enumerate() {
            write_sleb128(&mut table, filter_of(action));
            let next = if i + 1 == chain.len() { 0 } else { 1 };
            write_sleb128(&mut table, next);
        }
        // call-site 里的 action 是 **1-based** 偏移 (0 保留给「无 action」)
        offsets.insert(key, base as u64 + 1);
    }
    (table, offsets)
}

/// uleb128 编码一个值需要的**最小字节数**。
fn uleb_width(value: u64) -> usize {
    let mut width = 1usize;
    let mut v = value;
    while v >= 128 {
        v >>= 7;
        width += 1;
    }
    width
}

/// 在 `pos` 处写入恰好占 `width` 字节的 uleb128。
///
/// `width` 必须是该值的**最小**宽度 (由 [`uleb_width`] 得到)。这里不做「补零拉长」:
/// uleb128 的字节流里高位补 `0x00` 不是合法的宽度填充 —— 读者在第一个
/// continuation 位为 0 的字节处就结束取值, 多出来的 `0x00` 会被当成**下一个字段**
/// (实测: 长度 0x7c 写成 `7c 00` 后, `0x00` 被读作 call-site encoding, 整张 LSDA
/// 错位, unwinder 找不到任何受保护区间 → 直接 `std::terminate`)。
fn write_uleb128_at(buf: &mut [u8], pos: usize, width: usize, value: u64) {
    let mut tmp = Vec::new();
    write_uleb128(&mut tmp, value);
    assert_eq!(
        tmp.len(),
        width,
        "LSDA ttype length 的 uleb 宽度不是最小编码: value={value} 实际 {} 字节, 约定 {} 字节",
        tmp.len(),
        width
    );
    buf[pos..pos + width].copy_from_slice(&tmp);
}

/// 编码一个函数的 LSDA。
///
/// - `func_start`: 函数在 `.text` 里的起始偏移 (LPStart = omit 时的隐含基址)。
/// - `sites`: 按代码顺序登记的受保护调用点。
/// - `label_offsets`: 汇编标签 -> `.text` 偏移 (解析 landing pad 地址)。
///
/// 类型表索引由 lowering 侧分配 (1-based, 每函数一段连续区间); 这里按索引组装
/// 类型表, 未被任何链引用的索引留空表项 (值 0), 以保持
/// 「索引 = 自表尾向前的第 index 项」这一 libstdc++ 依赖的不变式。
pub fn build_lsda(
    func_start: u64,
    sites: &[EhSite],
    label_offsets: &HashMap<String, u64>,
) -> LsdaImage {
    // 1. 类型表内容: index -> typeinfo 符号名 (None = 空表项, 即 catch (...))
    let mut max_index = 0u32;
    let mut types: HashMap<u32, Option<String>> = HashMap::new();
    for site in sites {
        for act in &site.actions {
            if let EhAction::Catch {
                type_index,
                typeinfo,
            } = act
            {
                max_index = max_index.max(*type_index);
                // 同一索引必须对应同一类型; 首次出现即定表项, 后续不覆盖。
                types.entry(*type_index).or_insert(typeinfo.clone());
            }
        }
    }

    // 2. 收集 call-site 条目与 action 链。
    //    `landing_pad = None` 是一条**有意的** lp=0/action=0 条目 (见模块头规则 2),
    //    与「有落地垫但标签解析不出来」是两件不同的事: 后者记入 `unresolved_sites`
    //    交给发射端报错, 绝不静默丢条目。
    let mut entries: Vec<(u64, u64, u64, Vec<EhAction>)> = Vec::new();
    let mut chains: Vec<Vec<EhAction>> = Vec::new();
    let mut unresolved_sites = Vec::new();
    for site in sites {
        // 区间必须落在本函数内 (`start/end` 与本函数的发射段同源)。不成立就是后端
        // 归档错了归属: 伪造一条偏移会让 unwinder 按错误的 PC 区间清理,
        // 比编译失败严重得多; 而 panic 会让整个编译器崩掉 —— 两者都不接受,
        // 因此记入 `unresolved_sites` 让发射端报结构化错误。
        let lp_off = match &site.landing_pad {
            None => 0,
            Some(label) => match label_offsets.get(label).copied() {
                Some(lp) => match lp.checked_sub(func_start) {
                    Some(off) => off,
                    None => {
                        unresolved_sites.push(label.clone());
                        continue;
                    },
                },
                None => {
                    unresolved_sites.push(label.clone());
                    continue;
                },
            },
        };
        let Some(start) = site.start.checked_sub(func_start) else {
            unresolved_sites.push(format!("<call-site {}..{}>", site.start, site.end));
            continue;
        };
        let range = site.end - site.start;
        // lp=0 的区间按 ABI 必须同时是 action=0 (本帧既不清理也不捕获)。
        let actions = if lp_off == 0 { Vec::new() } else { site.actions.clone() };
        if !actions.is_empty() {
            chains.push(actions.clone());
        }
        entries.push((start, range, lp_off, actions));
    }

    // 3. action 表 (跨调用点去重) + 每条链的 1-based 偏移
    let (action_table, action_offsets) = build_action_table(&chains);

    let mut cs_table = Vec::new();
    for (start, range, lp_off, actions) in &entries {
        write_uleb128(&mut cs_table, *start);
        write_uleb128(&mut cs_table, *range);
        write_uleb128(&mut cs_table, *lp_off);
        let action = if actions.is_empty() {
            0
        } else {
            action_offsets
                .get(&chain_key(actions))
                .copied()
                .unwrap_or(0)
        };
        write_uleb128(&mut cs_table, action);
    }

    // 4. 类型表: 索引 N..1 依次排布, 于是索引 1 落在表尾 (libstdc++ 的计数方向)
    let mut type_table = Vec::new();
    let mut type_relocs = Vec::new();
    for idx in (1..=max_index).rev() {
        let entry_pos = type_table.len() as u64;
        if let Some(Some(sym)) = types.get(&idx) {
            // `.long DW.ref.<sym> - .` -> R_X86_64_PC32, addend 0
            type_relocs.push((entry_pos, format!("DW.ref.{}", sym)));
        }
        type_table.extend_from_slice(&0u32.to_le_bytes());
    }

    // 5. 组装: 头部 + call-site 表 + action 表 + 4 字节对齐 + 类型表。
    //
    //    ttype length 字段的 uleb **宽度取决于长度本身**, 而长度又反过来受宽度影响
    //    (类型表的 4 字节对齐填充量随头部大小变化), 所以按宽度做定点迭代:
    //    先用上界猜宽度, 排一遍得到长度, 长度算出的最小宽度若不同就换宽度重排。
    //    长度对宽度单调不减, 因此迭代必然收敛 (两轮内)。
    let upper = cs_table.len() + action_table.len() + type_table.len() + 8;
    let mut width = uleb_width(upper as u64);
    let (bytes, type_table_start) = loop {
        let mut candidate: Vec<u8> = Vec::new();
        candidate.push(0xff); // LPStart encoding = DW_EH_PE_omit
        candidate.push(TTYPE_ENCODING);
        let len_field_pos = candidate.len();
        candidate.resize(candidate.len() + width, 0);
        let lsda_data_start = candidate.len(); // == .LLSDATTD
        candidate.push(CALL_SITE_ENCODING);
        write_uleb128(&mut candidate, cs_table.len() as u64);
        candidate.extend_from_slice(&cs_table);
        candidate.extend_from_slice(&action_table);
        while !candidate.len().is_multiple_of(4) {
            candidate.push(0);
        }
        let type_table_start = candidate.len();
        candidate.extend_from_slice(&type_table);
        let lsda_len = candidate.len() - lsda_data_start;
        if uleb_width(lsda_len as u64) == width {
            write_uleb128_at(&mut candidate, len_field_pos, width, lsda_len as u64);
            break (candidate, type_table_start);
        }
        width = uleb_width(lsda_len as u64);
    };

    LsdaImage {
        bytes,
        type_relocs: type_relocs
            .into_iter()
            .map(|(pos, sym)| (type_table_start as u64 + pos, sym))
            .collect(),
        unresolved_sites,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::EhAction;

    fn site(start: u64, end: u64, lp: &str, actions: Vec<EhAction>) -> EhSite {
        EhSite {
            start,
            end,
            landing_pad: Some(lp.to_string()),
            actions,
        }
    }

    /// `landing_pad = 0` 的区间 (受保护但本帧不接管)。
    fn bare_site(start: u64, end: u64) -> EhSite {
        EhSite {
            start,
            end,
            landing_pad: None,
            actions: Vec::new(),
        }
    }

    #[test]
    fn uleb_and_sleb_match_dwarf_encoding() {
        let mut out = Vec::new();
        write_uleb128(&mut out, 0);
        write_uleb128(&mut out, 1);
        write_uleb128(&mut out, 127);
        write_uleb128(&mut out, 128);
        assert_eq!(out, vec![0x00, 0x01, 0x7f, 0x80, 0x01]);

        let mut out = Vec::new();
        write_sleb128(&mut out, 0);
        write_sleb128(&mut out, 1);
        write_sleb128(&mut out, 63);
        write_sleb128(&mut out, 64);
        write_sleb128(&mut out, -1);
        write_sleb128(&mut out, -65);
        // -65 = [0xbf, 0x7f] (两字节), -1 = 0x7f (一字节)
        assert_eq!(out, vec![0x00, 0x01, 0x3f, 0xc0, 0x00, 0x7f, 0xbf, 0x7f]);
    }

    #[test]
    fn single_catch_lsda_layout_matches_gas_form() {
        // 对应 g++ 的 `try { g(); } catch (A a) {...}`:
        //   头 3 字节 (0xff, 0x9b, uleb len) + 0x01 + uleb cs_len + 一条 4 uleb 条目
        //   + action (0x01, 0x00) + 4 字节类型表
        let mut lbl = HashMap::new();
        lbl.insert(".Ll_lp0".to_string(), 40u64);
        let img = build_lsda(
            10,
            &[site(
                20,
                25,
                ".Ll_lp0",
                vec![EhAction::Catch {
                    type_index: 1,
                    typeinfo: Some("_ZTI1A".into()),
                }],
            )],
            &lbl,
        );
        assert_eq!(img.bytes[0], 0xff);
        assert_eq!(img.bytes[1], TTYPE_ENCODING);
        // 头部之后: call-site encoding, cs 长度, 4 个 uleb, action, 对齐, 类型表
        let pos_cs_enc = 3; // 0xff, 0x9b, uleb(len) —— len 在此场景占 1 字节
        assert_eq!(img.bytes[pos_cs_enc], 0x01);
        let cs_len = img.bytes[pos_cs_enc + 1] as usize;
        assert_eq!(cs_len, 4); // 4 条单字节 uleb: start=10, range=5, lp=30, action=1
        assert_eq!(&img.bytes[pos_cs_enc + 2..pos_cs_enc + 2 + cs_len], &[10, 5, 30, 1]);
        // action 表: (filter=1, next=0)
        let act = pos_cs_enc + 2 + cs_len;
        assert_eq!(&img.bytes[act..act + 2], &[0x01, 0x00]);
        // 类型表从 4 字节对齐处开始, 且只有一个表项 = 索引 1
        let tt = (act + 2).next_multiple_of(4);
        assert_eq!(img.type_relocs.len(), 1);
        assert_eq!(img.type_relocs[0].1, "DW.ref._ZTI1A");
        assert_eq!(img.type_relocs[0].0, tt as u64);
        // ttype length = 从 LLSDATTD (头部之后) 到结尾
        assert_eq!(img.bytes[2] as usize, img.bytes.len() - pos_cs_enc);
    }

    #[test]
    fn cleanup_then_handlers_chain_is_deduped_and_self_relative() {
        // 两个调用点共用同一条 [cleanup, catch(1), catch(2)] 链:
        // action 表里只应出现一次, 且 next 字段自相对 (连续排布时 = +1)。
        let mut lbl = HashMap::new();
        lbl.insert(".Ll_lp".to_string(), 100u64);
        let chain = vec![
            EhAction::Cleanup,
            EhAction::Catch {
                type_index: 1,
                typeinfo: Some("_ZTI1A".into()),
            },
            EhAction::Catch {
                type_index: 2,
                typeinfo: None,
            },
        ];
        let sites = vec![
            site(20, 25, ".Ll_lp", chain.clone()),
            site(30, 35, ".Ll_lp", chain.clone()),
        ];
        let img = build_lsda(10, &sites, &lbl);
        let pos_cs_enc = 3;
        assert_eq!(img.bytes[pos_cs_enc], 0x01);
        let cs_len = img.bytes[pos_cs_enc + 1] as usize;
        assert_eq!(cs_len, 8, "两个调用点共 8 个 uleb");
        let act = pos_cs_enc + 2 + cs_len;
        // (0,+1) (1,+1) (2,0) —— 每条记录 2 字节, next 指向下一条记录起点
        assert_eq!(&img.bytes[act..act + 6], &[0x00, 0x01, 0x01, 0x01, 0x02, 0x00]);
        // 两个调用点的 action 偏移相同 (链去重), 且是 1-based; 起始偏移各自不同
        assert_eq!(&img.bytes[pos_cs_enc + 2..pos_cs_enc + 2 + 4], &[10, 5, 90, 1]);
        assert_eq!(&img.bytes[pos_cs_enc + 2 + 4..pos_cs_enc + 2 + 8], &[20, 5, 90, 1]);
        // 类型表: 索引 2 在前, 索引 1 (= _ZTI1A) 在表尾
        let tt = (act + 6).next_multiple_of(4);
        assert_eq!(img.type_relocs.len(), 1);
        assert_eq!(img.type_relocs[0].0, (tt + 4) as u64);
        assert_eq!(img.type_relocs[0].1, "DW.ref._ZTI1A");
    }

    #[test]
    fn unresolvable_landing_pad_is_reported_not_dropped() {
        // 落地垫标签缺失时既不能伪造一条 lp=0 的条目 (会把异常交给错误的帧),
        // 也不能静默丢掉条目: 必须回报给发射端。
        let mut lbl = HashMap::new();
        lbl.insert("other".to_string(), 1u64);
        let img = build_lsda(0, &[site(4, 9, "missing_lp", vec![EhAction::Cleanup])], &lbl);
        assert_eq!(img.bytes[4], 0, "call-site 表应为空");
        assert_eq!(img.unresolved_sites, vec![String::from("missing_lp")]);
    }

    #[test]
    fn bare_site_lands_as_zero_lp_zero_action() {
        // `_Unwind_Resume` 一类: 条目必须存在, 且 lp 与 action 都是 0 ——
        // 缺了它, 展开到本帧时 libstdc++ 会 terminate 而不是继续向外递。
        let mut lbl = HashMap::new();
        lbl.insert(".Ll_lp".to_string(), 100u64);
        let img = build_lsda(
            10,
            &[
                bare_site(20, 25),
                site(30, 35, ".Ll_lp", vec![EhAction::Cleanup]),
            ],
            &lbl,
        );
        let pos_cs_enc = 3;
        assert_eq!(img.bytes[pos_cs_enc], 0x01);
        let cs_len = img.bytes[pos_cs_enc + 1] as usize;
        let cs = &img.bytes[pos_cs_enc + 2..pos_cs_enc + 2 + cs_len];
        assert_eq!(&cs[..4], &[10, 5, 0, 0], "无落地垫区间也要登记");
        assert!(img.unresolved_sites.is_empty());
    }

    /// 按最小宽度读回一个 uleb128, 返回 `(值, 值之后的下标)`。
    fn read_uleb(bytes: &[u8], mut pos: usize) -> (u64, usize) {
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            let b = bytes[pos];
            pos += 1;
            value |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return (value, pos);
            }
            shift += 7;
        }
    }

    /// 一份 LSDA 必须能被「从头顺序读下去」自洽解析。
    ///
    /// 这里不比对具体字节 (调用点偏移随代码生成变化), 而是把 ABI  reader 真正
    /// 依赖的三个不变式抽出来: 头部两个编码字节、长度字段占最小宽度、长度正好
    /// 覆盖到本 LSDA 结尾, 且整段长度是 4 的倍数 (保证同节下一段不错位)。
    fn assert_lsda_self_consistent(img: &LsdaImage) {
        assert_eq!(img.bytes[0], 0xff, "LPStart encoding");
        assert_eq!(img.bytes[1], TTYPE_ENCODING, "TType encoding");
        let (len, after_len) = read_uleb(&img.bytes, 2);
        assert_eq!(
            img.bytes[after_len],
            CALL_SITE_ENCODING,
            "ttype length 的 uleb 宽度不是最小编码, 后面的字段全部被顶偏: {:02x?}",
            &img.bytes[2..(2 + 6).min(img.bytes.len())]
        );
        assert_eq!(
            usize::try_from(len).expect("LSDA 长度超 u64"),
            img.bytes.len() - after_len,
            "ttype length 应正好覆盖到 LSDA 结尾"
        );
        assert_eq!(
            img.bytes.len() % 4,
            0,
            "LSDA 长度不是 4 的倍数会让同一节里后续函数的类型表错位"
        );
    }

    #[test]
    fn large_lsda_keeps_header_length_self_consistent() {
        // 回归: ttype 长度字段曾按「上界宽度预留 + 尾部补 0」回填。当实际长度 < 128
        // 而上界 >= 128 时会发出 `7c 00` 这种字段, uleb 读者读到 0x7c 就结束取值,
        // 多出的 0x00 被当成 call-site 编码 -> 整张表错位 -> unwinder 在本帧找不到
        // 任何受保护区间 -> std::terminate (实测: 同函数内多组 try/catch 的夹具)。
        // 扫一段调用点个数, 让总长度正好跨过 128 边界。
        let mut lbl = HashMap::new();
        lbl.insert(".Ll_lp".to_string(), 400u64);
        for n in 1usize..=40 {
            let sites: Vec<EhSite> = (0..n)
                .map(|i| {
                    // 每 6 个调用点换一条链, 让 action 表也随之成定长增长
                    let t = (i % 6) + 1;
                    site(
                        20 + (i as u64) * 10,
                        25 + (i as u64) * 10,
                        ".Ll_lp",
                        vec![
                            EhAction::Cleanup,
                            EhAction::Catch {
                                type_index: t as u32,
                                typeinfo: Some(format!("_ZTI{}C", t)),
                            },
                        ],
                    )
                })
                .collect();
            let img = build_lsda(10, &sites, &lbl);
            assert_lsda_self_consistent(&img);
            assert!(img.unresolved_sites.is_empty());
        }
    }

    #[test]
    fn lsda_without_type_table_still_pads_to_four() {
        // 只有 cleanup (无 handler) 的函数: 类型表为空, 但整段仍必须是 4 的倍数,
        // 否则同节里下一个函数的 LSDA 起点就丢掉对齐。
        let mut lbl = HashMap::new();
        lbl.insert(".Ll_lp".to_string(), 60u64);
        let img = build_lsda(
            10,
            &[
                site(20, 25, ".Ll_lp", vec![EhAction::Cleanup]),
                bare_site(30, 35),
            ],
            &lbl,
        );
        assert!(img.type_relocs.is_empty(), "cleanup-only 不应产生类型表");
        assert_lsda_self_consistent(&img);
    }
}
